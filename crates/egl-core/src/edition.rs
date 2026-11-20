//! SFxT game data: which version a player's copy is, what is missing or
//! wrong against the Ember target (the Community Fix v1.1 data, "E1"), and
//! patching it from a pack the player supplies, with rollback on Steam.
//!
//! The list of files comes from the SFxT mod's own `patch/manifest.json`
//! (paths, sizes, SHA-256, and what each known version has instead). The
//! launcher ships no game files: a pack is a folder or one zip the player
//! points at, and only files whose hash is the manifest's are taken from it.
//! Everything is staged and checked before the game folder is written, and
//! every write is journaled. Nothing under `XLiveLessNess` is ever touched.

use crate::install::{sha256_file, write_atomic};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum EditionError {
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("the patch manifest cannot be read: {0}")]
    Manifest(String),
    #[error("not a zip the launcher can read: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("the manifest names an unsafe path: {0}")]
    UnsafePath(String),
    #[error("the pack lacks {} file(s) the game needs, e.g. {}", .0.len(), .0.first().map(String::as_str).unwrap_or(""))]
    PackIncomplete(Vec<String>),
    #[error("the pack's copy of {path} is not the one the manifest lists (sha256 {got})")]
    PackFileWrong { path: String, got: String },
    #[error("the game folder is the pack's folder, or holds it")]
    PackInsideGame,
    #[error("nothing to roll back")]
    NoJournal,
    #[error("rollback is only for the Steam version")]
    NoRollback,
    #[error("cancelled")]
    Cancelled,
}

// ---------------------------------------------------------------- manifest

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct Manifest {
    pub version: String,
    pub title: String,
    #[serde(default)]
    pub target: String,
    #[serde(default)]
    pub sources: Vec<SourceInfo>,
    #[serde(default)]
    pub executables: Vec<Exe>,
    pub files: Vec<MFile>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct SourceInfo {
    pub id: String,
    pub title: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct Exe {
    pub sha256: String,
    pub size: u64,
    pub name: String,
    pub build: Option<String>,
    pub supported: bool,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Class {
    Gameplay,
    Content,
    Media,
}

/// What a known version has at a path: the target bytes, nothing, or other bytes.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(untagged)]
pub enum Has {
    Word(String),
    Other { size: u64, sha256: String },
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct CopyRecipe {
    pub source: String,
    pub path: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct MFile {
    /// Windows style (`patch\CMN\...`), relative to the game folder.
    pub path: String,
    pub size: u64,
    pub sha256: String,
    pub class: Class,
    #[serde(default)]
    pub states: BTreeMap<String, Has>,
    /// The same bytes at another path of a known version's own files.
    #[serde(default)]
    pub copy: Vec<CopyRecipe>,
}

/// The game folders the manifest may write into. Anything else is refused.
const DATA_ROOTS: [&str; 3] = ["patch", "resource", "stream"];

/// The manifest path as parts, or an error when it could leave the data folders.
pub fn parts(path: &str) -> Result<Vec<String>, EditionError> {
    let norm = path.replace('\\', "/");
    let mut out = Vec::new();
    for c in Path::new(&norm).components() {
        match c {
            Component::Normal(s) if !s.to_string_lossy().contains(':') => out.push(s.to_string_lossy().into_owned()),
            _ => return Err(EditionError::UnsafePath(path.into())),
        }
    }
    let first_ok = out.first().is_some_and(|f| DATA_ROOTS.iter().any(|r| f.eq_ignore_ascii_case(r)));
    let xlln = out.iter().any(|p| p.eq_ignore_ascii_case("XLiveLessNess"));
    if !first_ok || xlln || out.len() < 2 {
        return Err(EditionError::UnsafePath(path.into()));
    }
    Ok(out)
}

impl Manifest {
    pub fn parse(text: &str) -> Result<Manifest, EditionError> {
        let m: Manifest = serde_json::from_str(text).map_err(|e| EditionError::Manifest(e.to_string()))?;
        if m.files.is_empty() {
            return Err(EditionError::Manifest("no files".into()));
        }
        for f in &m.files {
            parts(&f.path)?;
            if f.sha256.len() != 64 {
                return Err(EditionError::Manifest(format!("bad hash for {}", f.path)));
            }
        }
        Ok(m)
    }
    pub fn load(path: &Path) -> Result<Manifest, EditionError> {
        Manifest::parse(&std::fs::read_to_string(path)?)
    }
}

// ------------------------------------------------------------------ paths

/// `rel` under `root`, matching each part without regard to case when the
/// exact name is not there (the game's paths are Windows paths; Linux and
/// the Deck have case-sensitive file systems). The exact path when nothing
/// matches, so a new file gets the manifest's spelling.
pub fn resolve(root: &Path, parts: &[String]) -> PathBuf {
    let mut at = root.to_path_buf();
    let mut found = true;
    for p in parts {
        let exact = at.join(p);
        if !found || exact.exists() {
            found = found && exact.exists();
            at = exact;
            continue;
        }
        let hit = std::fs::read_dir(&at)
            .ok()
            .and_then(|rd| rd.flatten().map(|e| e.file_name()).find(|n| n.to_string_lossy().eq_ignore_ascii_case(p)));
        match hit {
            Some(n) => at = at.join(n),
            None => {
                found = false;
                at = exact;
            }
        }
    }
    at
}

fn key(path: &str) -> String {
    path.replace('\\', "/").to_ascii_lowercase()
}

// ------------------------------------------------------------- validation

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum FileStatus {
    Ok,
    Missing,
    /// Present, with other bytes.
    Wrong,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FileReport {
    pub path: String,
    pub class: Class,
    pub status: FileStatus,
    pub size: u64,
    /// The size the game's file has, when it has one.
    pub found_size: Option<u64>,
    /// Which known version these bytes are (`steam`, `cp12`), when one matches.
    pub found_as: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct VersionMatch {
    pub id: String,
    pub title: String,
    /// Files whose state is the one this version has.
    pub matching: usize,
    pub total: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct XllnState {
    /// `XLiveLessNess\` exists in the game folder.
    pub folder: bool,
    /// `XLiveLessNess\dlc\<title>` holds the DLC packs.
    pub dlc: bool,
    /// The DLC packs were moved into a mod folder (old drop-in installs);
    /// the folder they are in now.
    pub dlc_moved_to: Option<PathBuf>,
    pub xlive_dll: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub game_dir: PathBuf,
    pub manifest: String,
    pub exe: Option<Exe>,
    /// sha256 of the game's exe when no listed one matches.
    pub exe_unknown: Option<String>,
    /// The version the data looks like, best first.
    pub versions: Vec<VersionMatch>,
    pub lined_up: usize,
    pub total: usize,
    pub gameplay_ok: bool,
    /// Missing or wrong files (media included; media are never required).
    pub problems: Vec<FileReport>,
    /// Bytes a patch has to write (media left out).
    pub needed_bytes: u64,
    /// Every file was hashed (else only gameplay files and size mismatches).
    pub full: bool,
    pub xlln: XllnState,
    /// Whether a patch of this copy can be rolled back (Steam only).
    pub rollback_allowed: bool,
}

impl Report {
    /// The build named by the exe ("Steam", "Community Fix v1.1").
    pub fn build(&self) -> Option<&str> {
        self.exe.as_ref().and_then(|e| e.build.as_deref())
    }
    pub fn needs_patch(&self) -> bool {
        self.problems.iter().any(|p| p.class != Class::Media)
    }
}

const DLC_TITLE: &str = "43430FA5";

/// Looks at XLiveLessNess in the game folder, and at the mod folders an old
/// drop-in install may have moved its DLC packs to.
pub fn xlln_state(game: &Path, mod_dirs: &[PathBuf]) -> XllnState {
    let folder = resolve(game, &["XLiveLessNess".into()]);
    let dlc = resolve(game, &["XLiveLessNess".into(), "dlc".into(), DLC_TITLE.into()]);
    let dlc_moved_to =
        if dlc.is_dir() { None } else { mod_dirs.iter().map(|d| d.join("dlc")).find(|d| d.join(DLC_TITLE).is_dir()) };
    XllnState {
        folder: folder.is_dir(),
        dlc: dlc.is_dir(),
        dlc_moved_to,
        xlive_dll: resolve(game, &["xlive.dll".into()]).is_file(),
    }
}

/// Moves DLC packs an old drop-in moved into a mod folder back to
/// `XLiveLessNess\dlc`, where XLiveLessNess and Ember's xlive read them.
/// Never removes anything; refuses when the game already has its own.
pub fn restore_xlln_dlc(game: &Path, moved: &Path) -> Result<PathBuf, EditionError> {
    let target = game.join("XLiveLessNess").join("dlc");
    if resolve(game, &["XLiveLessNess".into(), "dlc".into(), DLC_TITLE.into()]).exists() {
        return Err(EditionError::Io(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "the game already has XLiveLessNess\\dlc; both are kept",
        )));
    }
    std::fs::create_dir_all(&target)?;
    for e in std::fs::read_dir(moved)? {
        let e = e?;
        let to = target.join(e.file_name());
        if to.exists() {
            continue;
        }
        if std::fs::rename(e.path(), &to).is_err() {
            // Another drive: copy, then leave the original in place.
            copy_tree(&e.path(), &to)?;
        }
    }
    Ok(target)
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    if from.is_dir() {
        std::fs::create_dir_all(to)?;
        for e in std::fs::read_dir(from)? {
            let e = e?;
            copy_tree(&e.path(), &to.join(e.file_name()))?;
        }
        Ok(())
    } else {
        std::fs::copy(from, to).map(|_| ())
    }
}

/// The game's exe, as the manifest knows it.
pub fn identify_exe(game: &Path, exe_name: &str, m: &Manifest) -> (Option<Exe>, Option<String>) {
    let p = resolve(game, &[exe_name.to_string()]);
    let Ok(hash) = sha256_file(&p) else { return (None, None) };
    match m.executables.iter().find(|e| e.sha256 == hash) {
        Some(e) => (Some(e.clone()), None),
        None => (None, Some(hash)),
    }
}

/// Compares the game's data with the manifest. `full` hashes every file;
/// otherwise gameplay files are hashed and the rest checked by size.
/// `progress(done, total)` returns false to cancel.
pub fn validate(
    game: &Path,
    exe_name: &str,
    m: &Manifest,
    mod_dirs: &[PathBuf],
    full: bool,
    mut progress: impl FnMut(usize, usize) -> bool,
) -> Result<Report, EditionError> {
    let (exe, exe_unknown) = identify_exe(game, exe_name, m);
    let mut versions: Vec<VersionMatch> = m
        .sources
        .iter()
        .map(|s| VersionMatch { id: s.id.clone(), title: s.title.clone(), matching: 0, total: m.files.len() })
        .collect();
    let mut problems = Vec::new();
    let mut lined_up = 0;
    let mut gameplay_ok = true;
    let mut needed_bytes = 0;
    for (i, f) in m.files.iter().enumerate() {
        if !progress(i, m.files.len()) {
            return Err(EditionError::Cancelled);
        }
        let p = resolve(game, &parts(&f.path)?);
        let found_size = std::fs::metadata(&p).ok().filter(|md| md.is_file()).map(|md| md.len());
        let hash_it = |size: u64| full || f.class == Class::Gameplay || size != f.size;
        let hash = match found_size {
            Some(s) if hash_it(s) => Some(sha256_file(&p)?),
            _ => None,
        };
        let ok = match (found_size, &hash) {
            (Some(s), Some(h)) => s == f.size && *h == f.sha256,
            (Some(s), None) => s == f.size,
            (None, _) => false,
        };
        // Which known version these bytes are.
        let mut found_as = None;
        for v in versions.iter_mut() {
            let same = match f.states.get(&v.id) {
                None => ok, // not listed: the version has the target (fix11)
                Some(Has::Word(w)) if w == "target" => ok,
                Some(Has::Word(_)) => found_size.is_none(),
                Some(Has::Other { size, sha256 }) => {
                    found_size == Some(*size) && hash.as_deref().is_none_or(|h| h == sha256)
                }
            };
            if same {
                v.matching += 1;
                if !ok && found_size.is_some() && found_as.is_none() {
                    found_as = Some(v.id.clone());
                }
            }
        }
        if ok {
            lined_up += 1;
        } else {
            if f.class == Class::Gameplay {
                gameplay_ok = false;
            }
            if f.class != Class::Media {
                needed_bytes += f.size;
            }
            problems.push(FileReport {
                path: f.path.clone(),
                class: f.class,
                status: if found_size.is_some() { FileStatus::Wrong } else { FileStatus::Missing },
                size: f.size,
                found_size,
                found_as,
            });
        }
    }
    versions.sort_by_key(|v| std::cmp::Reverse(v.matching));
    let rollback_allowed = exe.as_ref().and_then(|e| e.build.as_deref()) == Some("Steam");
    Ok(Report {
        game_dir: game.to_path_buf(),
        manifest: m.version.clone(),
        exe,
        exe_unknown,
        versions,
        lined_up,
        total: m.files.len(),
        gameplay_ok,
        problems,
        needed_bytes,
        full,
        xlln: xlln_state(game, mod_dirs),
        rollback_allowed,
    })
}

// ------------------------------------------------------------------- packs

/// Where the target files come from: a folder (a Complete Edition copy, an
/// unpacked pack) or one zip. Only manifest paths are ever read from it.
pub enum Pack {
    Folder(PathBuf),
    Zip { archive: zip::ZipArchive<std::fs::File>, index: HashMap<String, usize> },
}

impl Pack {
    /// Opens a pack and finds where the game's data folders sit in it (a
    /// pack may hold `Street Fighter X Tekken\patch\...` or `patch\...`).
    pub fn open(path: &Path, m: &Manifest) -> Result<Pack, EditionError> {
        if path.is_dir() {
            // The folder that holds patch/resource/stream, here or one level down.
            let has_data = |d: &Path| DATA_ROOTS.iter().any(|r| resolve(d, &[r.to_string()]).is_dir());
            if has_data(path) {
                return Ok(Pack::Folder(path.to_path_buf()));
            }
            for e in std::fs::read_dir(path)?.flatten() {
                if e.path().is_dir() && has_data(&e.path()) {
                    return Ok(Pack::Folder(e.path()));
                }
            }
            return Ok(Pack::Folder(path.to_path_buf()));
        }
        let mut archive = zip::ZipArchive::new(std::fs::File::open(path)?)?;
        let names: Vec<String> =
            (0..archive.len()).map(|i| archive.by_index_raw(i).map(|f| key(f.name()))).collect::<Result<_, _>>()?;
        let wanted: std::collections::HashSet<String> = m.files.iter().map(|f| key(&f.path)).collect();
        // The prefix that makes the most entries manifest paths.
        let mut prefixes: HashMap<String, usize> = HashMap::new();
        for n in &names {
            for w in DATA_ROOTS {
                let needle = format!("{w}/");
                let at = if n.starts_with(&needle) { Some(0) } else { n.find(&format!("/{needle}")).map(|i| i + 1) };
                if let Some(at) = at {
                    if wanted.contains(&n[at..]) {
                        *prefixes.entry(n[..at].to_string()).or_default() += 1;
                    }
                }
            }
        }
        let prefix = prefixes.into_iter().max_by_key(|(_, c)| *c).map(|(p, _)| p).unwrap_or_default();
        let index = names
            .iter()
            .enumerate()
            .filter_map(|(i, n)| {
                n.strip_prefix(prefix.as_str()).filter(|r| wanted.contains(*r)).map(|r| (r.to_string(), i))
            })
            .collect();
        Ok(Pack::Zip { archive, index })
    }

    pub fn has(&self, f: &MFile) -> bool {
        match self {
            Pack::Folder(root) => parts(&f.path).map(|p| resolve(root, &p).is_file()).unwrap_or(false),
            Pack::Zip { index, .. } => index.contains_key(&key(&f.path)),
        }
    }

    /// Copies the pack's file to `dest`, returning its sha256.
    fn copy_to(&mut self, f: &MFile, dest: &Path) -> Result<String, EditionError> {
        match self {
            Pack::Folder(root) => {
                let src = resolve(root, &parts(&f.path)?);
                copy_hashed(&mut std::fs::File::open(src)?, dest)
            }
            Pack::Zip { archive, index } => {
                let i = *index.get(&key(&f.path)).ok_or_else(|| EditionError::PackIncomplete(vec![f.path.clone()]))?;
                let mut entry = archive.by_index(i)?;
                copy_hashed(&mut entry, dest)
            }
        }
    }
}

fn copy_hashed(from: &mut impl Read, dest: &Path) -> Result<String, EditionError> {
    use sha2::{Digest, Sha256};
    use std::io::Write;
    if let Some(d) = dest.parent() {
        std::fs::create_dir_all(d)?;
    }
    let mut out = std::fs::File::create(dest)?;
    let mut hash = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = from.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
        out.write_all(&buf[..n])?;
    }
    out.sync_all()?;
    Ok(hex::encode(hash.finalize()))
}

// ------------------------------------------------------------- patch, undo

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Before {
    pub size: u64,
    pub sha256: String,
    /// Kept in the journal's `backup` folder.
    pub backed_up: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Written {
    pub path: String,
    pub sha256: String,
    /// The file the game had there, if any.
    pub before: Option<Before>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Phase {
    Applying,
    Applied,
    RolledBack,
}

/// `<work>/journal.json`: what a patch wrote, so it can be undone.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Journal {
    pub game_dir: PathBuf,
    pub manifest: String,
    pub build: Option<String>,
    pub pack: PathBuf,
    pub at: u64,
    pub phase: Phase,
    pub rollback: bool,
    pub written: Vec<Written>,
    /// Folders the patch made (removed again on rollback when empty).
    #[serde(default)]
    pub made_dirs: Vec<PathBuf>,
}

impl Journal {
    pub fn path(work: &Path) -> PathBuf {
        work.join("journal.json")
    }
    pub fn load(work: &Path) -> Option<Journal> {
        std::fs::read(Journal::path(work)).ok().and_then(|b| serde_json::from_slice(&b).ok())
    }
    fn save(&self, work: &Path) -> std::io::Result<()> {
        write_atomic(&Journal::path(work), &serde_json::to_vec_pretty(self).unwrap())
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Applied {
    pub written: usize,
    pub from_pack: usize,
    pub from_own_files: usize,
    pub backed_up: usize,
    /// Media the pack lacked (never required).
    pub skipped_media: usize,
}

/// Where one game folder's patch keeps its journal and backups.
pub fn work_dir(data: &Path, game: &Path) -> PathBuf {
    use sha2::{Digest, Sha256};
    let id = hex::encode(&Sha256::digest(game.to_string_lossy().to_lowercase().as_bytes())[..8]);
    data.join("editions").join(id)
}

/// `<file name>.<suffix>` in the same folder.
fn beside(p: &Path, suffix: &str) -> PathBuf {
    let mut name = p.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{suffix}"));
    p.with_file_name(name)
}

fn is_inside(child: &Path, parent: &Path) -> bool {
    let c = child.canonicalize().unwrap_or_else(|_| child.to_path_buf());
    let p = parent.canonicalize().unwrap_or_else(|_| parent.to_path_buf());
    c.starts_with(&p)
}

/// Brings the game's data to the manifest's target from `pack`. Every needed
/// file is first staged and checked; the game folder is only written once
/// all of them are. On the Steam version each replaced file is backed up
/// first, so [`rollback`] restores the exact bytes.
pub fn apply(
    report: &Report,
    m: &Manifest,
    pack_path: &Path,
    work: &Path,
    mut progress: impl FnMut(&str, usize, usize) -> bool,
) -> Result<Applied, EditionError> {
    let game = &report.game_dir;
    if pack_path.is_dir() && (is_inside(pack_path, game) || is_inside(game, pack_path)) {
        return Err(EditionError::PackInsideGame);
    }
    let mut pack = Pack::open(pack_path, m)?;
    let by_path: HashMap<&str, &MFile> = m.files.iter().map(|f| (f.path.as_str(), f)).collect();

    // Plan: each needed file from the game's own bytes (a copy recipe) or the pack.
    enum From<'a> {
        Own(PathBuf),
        Pack(&'a MFile),
    }
    let mut plan: Vec<(&MFile, From)> = Vec::new();
    let mut lacking = Vec::new();
    let mut skipped_media = 0;
    for p in &report.problems {
        let f = by_path[p.path.as_str()];
        let own = f.copy.iter().map(|c| parts(&c.path).map(|ps| resolve(game, &ps))).find_map(|r| {
            r.ok().filter(|src| src.is_file() && sha256_file(src).ok().as_deref() == Some(f.sha256.as_str()))
        });
        match own {
            Some(src) => plan.push((f, From::Own(src))),
            None if pack.has(f) => plan.push((f, From::Pack(f))),
            None if f.class == Class::Media => skipped_media += 1,
            None => lacking.push(f.path.clone()),
        }
    }
    if !lacking.is_empty() {
        return Err(EditionError::PackIncomplete(lacking));
    }

    // Stage and check everything before the game folder is touched.
    let stage = work.join("stage");
    if stage.exists() {
        std::fs::remove_dir_all(&stage)?;
    }
    let total = plan.len();
    for (i, (f, from)) in plan.iter().enumerate() {
        if !progress("check", i, total) {
            let _ = std::fs::remove_dir_all(&stage);
            return Err(EditionError::Cancelled);
        }
        let dest = stage.join(parts(&f.path)?.join("/"));
        let got = match from {
            From::Own(src) => copy_hashed(&mut std::fs::File::open(src)?, &dest)?,
            From::Pack(f) => pack.copy_to(f, &dest)?,
        };
        if got != f.sha256 {
            let _ = std::fs::remove_dir_all(&stage);
            return Err(EditionError::PackFileWrong { path: f.path.clone(), got });
        }
    }

    // Write, journaling each file before it changes.
    let rollback = report.rollback_allowed;
    let mut journal = Journal {
        game_dir: game.clone(),
        manifest: m.version.clone(),
        build: report.build().map(str::to_string),
        pack: pack_path.to_path_buf(),
        at: crate::install::now(),
        phase: Phase::Applying,
        rollback,
        written: Vec::new(),
        made_dirs: Vec::new(),
    };
    // A second patch on top of an earlier (or an interrupted) one keeps the first one's originals.
    let earlier = Journal::load(work).filter(|j| j.phase != Phase::RolledBack);
    if let Some(e) = &earlier {
        journal.written = e.written.clone();
        journal.made_dirs = e.made_dirs.clone();
    }
    journal.save(work)?;
    let backup = work.join("backup");
    let mut backed_up = 0;
    for (i, (f, _)) in plan.iter().enumerate() {
        progress("write", i, total);
        let ps = parts(&f.path)?;
        let dest = resolve(game, &ps);
        let rel = ps.join("/");
        let known = journal.written.iter().position(|w| w.path == f.path);
        let before = match std::fs::metadata(&dest) {
            Ok(md) if md.is_file() => {
                let sha256 = sha256_file(&dest)?;
                let mut backed = false;
                if rollback && known.is_none() {
                    let b = backup.join(&rel);
                    if copy_hashed(&mut std::fs::File::open(&dest)?, &b)? != sha256 {
                        return Err(EditionError::Io(std::io::Error::other(format!("backup of {} failed", f.path))));
                    }
                    backed = true;
                    backed_up += 1;
                }
                Some(Before { size: md.len(), sha256, backed_up: backed })
            }
            _ => None,
        };
        if let Some(parent) = dest.parent() {
            let mut missing = Vec::new();
            let mut d = parent.to_path_buf();
            while !d.exists() && d.starts_with(game) && d != *game {
                missing.push(d.clone());
                d = d.parent().unwrap().to_path_buf();
            }
            std::fs::create_dir_all(parent)?;
            journal.made_dirs.extend(missing);
        }
        match known {
            Some(k) => journal.written[k].sha256 = f.sha256.clone(),
            None => journal.written.push(Written { path: f.path.clone(), sha256: f.sha256.clone(), before }),
        }
        journal.save(work)?;
        let tmp = beside(&dest, "ember-new");
        std::fs::copy(stage.join(&rel), &tmp)?;
        std::fs::rename(&tmp, &dest)?;
    }
    journal.phase = Phase::Applied;
    journal.save(work)?;
    let _ = std::fs::remove_dir_all(&stage);
    let from_own = plan.iter().filter(|(_, f)| matches!(f, From::Own(_))).count();
    Ok(Applied { written: total, from_pack: total - from_own, from_own_files: from_own, backed_up, skipped_media })
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RolledBack {
    pub restored: usize,
    pub removed: usize,
    /// Files changed since the patch: left as they are.
    pub left: Vec<String>,
}

/// Undoes a patch of the Steam version: removes the files it added and puts
/// every replaced file back from its backup, checking each one.
pub fn rollback(work: &Path) -> Result<RolledBack, EditionError> {
    let mut j = Journal::load(work).ok_or(EditionError::NoJournal)?;
    if j.phase == Phase::RolledBack {
        return Err(EditionError::NoJournal);
    }
    if !j.rollback {
        return Err(EditionError::NoRollback);
    }
    let game = j.game_dir.clone();
    let backup = work.join("backup");
    let mut out = RolledBack { restored: 0, removed: 0, left: Vec::new() };
    for w in j.written.iter().rev() {
        let ps = parts(&w.path)?;
        let dest = resolve(&game, &ps);
        let now = sha256_file(&dest).ok();
        match &w.before {
            None => match now.as_deref() {
                Some(h) if h == w.sha256 => {
                    std::fs::remove_file(&dest)?;
                    out.removed += 1;
                }
                None => {}
                Some(_) => out.left.push(w.path.clone()),
            },
            Some(b) if b.backed_up => {
                if now.as_deref() == Some(b.sha256.as_str()) {
                    continue;
                }
                let src = backup.join(ps.join("/"));
                let tmp = beside(&dest, "ember-old");
                if copy_hashed(&mut std::fs::File::open(&src)?, &tmp)? != b.sha256 {
                    let _ = std::fs::remove_file(&tmp);
                    return Err(EditionError::Io(std::io::Error::other(format!(
                        "the backup of {} is damaged",
                        w.path
                    ))));
                }
                std::fs::rename(&tmp, &dest)?;
                out.restored += 1;
            }
            Some(_) => out.left.push(w.path.clone()),
        }
    }
    let mut dirs = j.made_dirs.clone();
    dirs.sort_by_key(|d| std::cmp::Reverse(d.components().count()));
    for d in dirs {
        let _ = std::fs::remove_dir(d); // only when empty
    }
    j.phase = Phase::RolledBack;
    j.save(work)?;
    let _ = std::fs::remove_dir_all(&backup);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::io::Write;

    fn sha(b: &[u8]) -> String {
        hex::encode(Sha256::digest(b))
    }

    /// A made-up game: target data T, Steam has some of it, other bytes for one file.
    fn manifest(steam_exe: &[u8]) -> Manifest {
        let file = |path: &str, body: &[u8], class: Class, steam: Option<Has>| MFile {
            path: path.into(),
            size: body.len() as u64,
            sha256: sha(body),
            class,
            states: steam.map(|s| BTreeMap::from([("steam".to_string(), s)])).unwrap_or_default(),
            copy: Vec::new(),
        };
        let mut shared = file("patch\\CMN\\battle\\RYU.bac", b"ryu", Class::Gameplay, Some(Has::Word("target".into())));
        shared.copy.clear();
        let mut copyable =
            file("patch\\CMN\\subtitle\\JUN.sub", b"same", Class::Content, Some(Has::Word("absent".into())));
        copyable.copy = vec![CopyRecipe { source: "steam".into(), path: "resource\\CMN\\ASK.sub".into() }];
        Manifest {
            version: "E1".into(),
            title: "test".into(),
            target: "fix11".into(),
            sources: vec![
                SourceInfo { id: "steam".into(), title: "Steam".into() },
                SourceInfo { id: "fix11".into(), title: "Fix".into() },
            ],
            executables: vec![Exe {
                sha256: sha(steam_exe),
                size: steam_exe.len() as u64,
                name: "Steam SFTK.exe".into(),
                build: Some("Steam".into()),
                supported: true,
            }],
            files: vec![
                shared,
                file(
                    "patch\\CMN\\boot\\game\\const.lua",
                    b"new tables",
                    Class::Gameplay,
                    Some(Has::Other { size: 3, sha256: sha(b"old") }),
                ),
                file("resource\\CMN\\ASK.sub", b"same", Class::Content, Some(Has::Word("target".into()))),
                copyable,
                file("patch\\CMN\\battle\\ELN\\ELN.bac", b"elena", Class::Gameplay, Some(Has::Word("absent".into()))),
                file("stream\\movie\\op.usm", b"movie", Class::Media, Some(Has::Word("absent".into()))),
            ],
        }
    }

    fn put(root: &Path, rel: &str, body: &[u8]) {
        let p = root.join(rel.replace('\\', "/"));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    fn steam_game(root: &Path) {
        put(root, "SFTK.exe", b"steam exe");
        put(root, "patch/CMN/battle/RYU.bac", b"ryu");
        put(root, "patch/CMN/boot/game/const.lua", b"old");
        put(root, "resource/CMN/ASK.sub", b"same");
        put(root, "XLiveLessNess/dlc/43430FA5/Pack_01/content.xbx", b"dlc");
    }

    fn zip_pack(path: &Path, files: &[(&str, &[u8])]) {
        let mut z = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        for (n, b) in files {
            z.start_file(*n, zip::write::SimpleFileOptions::default()).unwrap();
            z.write_all(b).unwrap();
        }
        z.finish().unwrap();
    }

    fn run(game: &Path, m: &Manifest) -> Report {
        validate(game, "SFTK.exe", m, &[], true, |_, _| true).unwrap()
    }

    #[test]
    fn refuses_paths_outside_the_data_folders() {
        assert!(parts("patch\\CMN\\a.bac").is_ok());
        assert!(parts("..\\SFTK.exe").is_err());
        assert!(parts("SFTK.exe").is_err());
        assert!(parts("xlive.dll").is_err());
        assert!(parts("patch\\..\\..\\x").is_err());
        assert!(parts("patch\\XLiveLessNess\\dlc\\x").is_err());
        assert!(parts("C:\\patch\\x").is_err());
        let mut m = manifest(b"x");
        m.files[0].path = "XLiveLessNess\\dlc\\a".into();
        assert!(Manifest::parse(&serde_json::to_string(&m).unwrap()).is_err());
    }

    #[test]
    fn tells_steam_data_and_what_is_missing() {
        let t = tempfile::tempdir().unwrap();
        steam_game(t.path());
        let m = manifest(b"steam exe");
        let r = run(t.path(), &m);
        assert_eq!(r.build(), Some("Steam"));
        assert!(r.rollback_allowed);
        assert_eq!(r.versions[0].id, "steam");
        assert_eq!(r.versions[0].matching, r.total);
        assert!(!r.gameplay_ok);
        let wrong: Vec<_> = r.problems.iter().filter(|p| p.status == FileStatus::Wrong).collect();
        assert_eq!(wrong.len(), 1);
        assert_eq!(wrong[0].found_as.as_deref(), Some("steam"));
        assert_eq!(r.problems.iter().filter(|p| p.status == FileStatus::Missing).count(), 3);
        assert!(r.xlln.folder && r.xlln.dlc);
    }

    #[test]
    fn patch_from_a_zip_then_roll_back_to_the_exact_steam_files() {
        let t = tempfile::tempdir().unwrap();
        let game = t.path().join("game");
        steam_game(&game);
        let before: Vec<(String, Vec<u8>)> = walk(&game);
        let m = manifest(b"steam exe");
        let r = run(&game, &m);
        let pack = t.path().join("pack.zip");
        zip_pack(
            &pack,
            &[
                ("Street Fighter X Tekken/patch/CMN/boot/game/const.lua", b"new tables"),
                ("Street Fighter X Tekken/patch/CMN/battle/ELN/ELN.bac", b"elena"),
                ("Street Fighter X Tekken/SFTK.exe", b"not read"),
                ("Street Fighter X Tekken/XLiveLessNess/xlive.dll", b"not read"),
            ],
        );
        let work = t.path().join("work");
        let a = apply(&r, &m, &pack, &work, |_, _, _| true).unwrap();
        assert_eq!(a.written, 3);
        assert_eq!(a.from_own_files, 1); // JUN.sub copied from ASK.sub
        assert_eq!(a.backed_up, 1);
        assert_eq!(a.skipped_media, 1);
        let after = run(&game, &m);
        assert!(after.gameplay_ok && !after.needs_patch());
        assert_eq!(std::fs::read(game.join("SFTK.exe")).unwrap(), b"steam exe");
        assert!(!game.join("XLiveLessNess/xlive.dll").exists());
        assert!(game.join("XLiveLessNess/dlc/43430FA5/Pack_01/content.xbx").exists());

        let rb = rollback(&work).unwrap();
        assert_eq!((rb.restored, rb.removed), (1, 2));
        assert!(rb.left.is_empty());
        assert_eq!(walk(&game), before);
        assert!(!game.join("patch/CMN/battle/ELN").exists());
        assert!(matches!(rollback(&work), Err(EditionError::NoJournal)));
    }

    #[test]
    fn a_wrong_or_short_pack_writes_nothing() {
        let t = tempfile::tempdir().unwrap();
        let game = t.path().join("game");
        steam_game(&game);
        let before = walk(&game);
        let m = manifest(b"steam exe");
        let r = run(&game, &m);
        let work = t.path().join("work");

        let short = t.path().join("short.zip");
        zip_pack(&short, &[("patch/CMN/boot/game/const.lua", b"new tables")]);
        match apply(&r, &m, &short, &work, |_, _, _| true) {
            Err(EditionError::PackIncomplete(l)) => assert_eq!(l, vec!["patch\\CMN\\battle\\ELN\\ELN.bac"]),
            other => panic!("{other:?}"),
        }
        let bad = t.path().join("bad.zip");
        zip_pack(&bad, &[("patch/CMN/boot/game/const.lua", b"tampered!!"), ("patch/CMN/battle/ELN/ELN.bac", b"elena")]);
        assert!(matches!(apply(&r, &m, &bad, &work, |_, _, _| true), Err(EditionError::PackFileWrong { .. })));
        assert_eq!(walk(&game), before);
    }

    #[test]
    fn a_folder_pack_works_and_community_copies_get_no_rollback() {
        let t = tempfile::tempdir().unwrap();
        let game = t.path().join("game");
        steam_game(&game);
        std::fs::write(game.join("SFTK.exe"), b"community exe").unwrap();
        let mut m = manifest(b"steam exe");
        m.executables.push(Exe {
            sha256: sha(b"community exe"),
            size: 13,
            name: "Fix v1.1".into(),
            build: Some("Community Fix v1.1".into()),
            supported: true,
        });
        let r = run(&game, &m);
        assert!(!r.rollback_allowed);
        let pack = t.path().join("Fix v1.1");
        put(&pack, "Street Fighter X Tekken/Patch/cmn/boot/game/const.lua", b"new tables");
        put(&pack, "Street Fighter X Tekken/Patch/cmn/battle/ELN/ELN.bac", b"elena");
        let work = t.path().join("work");
        let a = apply(&r, &m, &pack, &work, |_, _, _| true).unwrap();
        assert_eq!(a.backed_up, 0);
        assert!(!run(&game, &m).needs_patch());
        assert!(matches!(rollback(&work), Err(EditionError::NoRollback)));
    }

    #[test]
    fn refuses_a_pack_inside_the_game() {
        let t = tempfile::tempdir().unwrap();
        steam_game(t.path());
        let m = manifest(b"steam exe");
        let r = run(t.path(), &m);
        let work = t.path().join("w");
        assert!(matches!(
            apply(&r, &m, &t.path().join("patch"), &work, |_, _, _| true),
            Err(EditionError::PackInsideGame)
        ));
    }

    #[test]
    fn moves_dlc_an_old_drop_in_took_back() {
        let t = tempfile::tempdir().unwrap();
        let game = t.path().join("game");
        put(&game, "SFTK.exe", b"x");
        let ember = game.join("SFxT Ember");
        put(&ember, "dlc/43430FA5/Pack_01/content.xbx", b"dlc");
        let s = xlln_state(&game, std::slice::from_ref(&ember));
        assert!(!s.folder && !s.dlc);
        assert_eq!(s.dlc_moved_to.as_deref(), Some(ember.join("dlc").as_path()));
        restore_xlln_dlc(&game, &ember.join("dlc")).unwrap();
        let s = xlln_state(&game, std::slice::from_ref(&ember));
        assert!(s.folder && s.dlc && s.dlc_moved_to.is_none());
        assert_eq!(std::fs::read(game.join("XLiveLessNess/dlc/43430FA5/Pack_01/content.xbx")).unwrap(), b"dlc");
    }

    #[test]
    fn reads_the_real_manifest_when_present() {
        let p = Path::new("/mnt/c/Users/FRaccie/source/repos/ember-build/sfxt/patch/manifest.json");
        if let Ok(m) = Manifest::load(p) {
            assert_eq!(m.files.len(), 3083);
            assert!(m.executables.iter().any(|e| e.build.as_deref() == Some("Steam")));
        }
    }

    fn walk(root: &Path) -> Vec<(String, Vec<u8>)> {
        let mut out = Vec::new();
        fn go(root: &Path, d: &Path, out: &mut Vec<(String, Vec<u8>)>) {
            for e in std::fs::read_dir(d).unwrap().flatten() {
                if e.path().is_dir() {
                    go(root, &e.path(), out);
                } else {
                    out.push((
                        e.path().strip_prefix(root).unwrap().to_string_lossy().into_owned(),
                        std::fs::read(e.path()).unwrap(),
                    ));
                }
            }
        }
        go(root, root, &mut out);
        out.sort();
        out
    }
}
