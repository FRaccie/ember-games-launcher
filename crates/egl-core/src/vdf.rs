//! Steam's two VDF formats. Text VDF (libraryfolders.vdf, appmanifest_*.acf,
//! localconfig.vdf) keeps its pairs in order and keeps repeated keys, so a
//! rewrite changes only what was set. Binary VDF is shortcuts.vdf.

use std::fmt::Write as _;

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum VdfError {
    #[error("unterminated string")]
    Unterminated,
    #[error("unbalanced braces")]
    Braces,
    #[error("section without a name")]
    Nameless,
    #[error("conditional {0} is not supported")]
    Conditional(String),
    #[error("binary vdf: unknown type {0} at {1}")]
    BinaryType(u8, usize),
    #[error("binary vdf: truncated")]
    Truncated,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Str(String),
    Map(Map),
}

/// An ordered text VDF map. Lookups ignore letter case, as Steam's do.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Map(pub Vec<(String, Value)>);

impl Map {
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.0.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v)
    }
    pub fn str(&self, key: &str) -> Option<&str> {
        match self.get(key) {
            Some(Value::Str(s)) => Some(s),
            _ => None,
        }
    }
    pub fn map(&self, key: &str) -> Option<&Map> {
        match self.get(key) {
            Some(Value::Map(m)) => Some(m),
            _ => None,
        }
    }
    pub fn path(&self, keys: &[&str]) -> Option<&Map> {
        keys.iter().try_fold(self, |m, k| m.map(k))
    }
    /// The map under key, added at the end when missing. A string under that
    /// key is replaced by an empty map.
    pub fn child(&mut self, key: &str) -> &mut Map {
        let at = match self.0.iter().position(|(k, _)| k.eq_ignore_ascii_case(key)) {
            Some(i) => i,
            None => {
                self.0.push((key.to_string(), Value::Map(Map::default())));
                self.0.len() - 1
            }
        };
        if !matches!(self.0[at].1, Value::Map(_)) {
            self.0[at].1 = Value::Map(Map::default());
        }
        match &mut self.0[at].1 {
            Value::Map(m) => m,
            Value::Str(_) => unreachable!(),
        }
    }
    pub fn child_path(&mut self, keys: &[&str]) -> &mut Map {
        keys.iter().fold(self, |m, k| m.child(k))
    }
    pub fn set(&mut self, key: &str, value: &str) {
        match self.0.iter_mut().find(|(k, _)| k.eq_ignore_ascii_case(key)) {
            Some(pair) => pair.1 = Value::Str(value.to_string()),
            None => self.0.push((key.to_string(), Value::Str(value.to_string()))),
        }
    }
    pub fn remove(&mut self, key: &str) {
        self.0.retain(|(k, _)| !k.eq_ignore_ascii_case(key));
    }
}

enum Tok {
    Open,
    Close,
    Str(String),
}

fn tokens(text: &str) -> Result<Vec<Tok>, VdfError> {
    let chars: Vec<char> = text.trim_start_matches('\u{feff}').chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
        } else if c == '/' && chars.get(i + 1) == Some(&'/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
        } else if c == '{' {
            out.push(Tok::Open);
            i += 1;
        } else if c == '}' {
            out.push(Tok::Close);
            i += 1;
        } else if c == '[' {
            let end = chars[i..].iter().position(|&c| c == ']').map_or(chars.len(), |p| i + p + 1);
            return Err(VdfError::Conditional(chars[i..end].iter().collect()));
        } else if c == '"' {
            i += 1;
            let mut s = String::new();
            loop {
                match chars.get(i) {
                    None => return Err(VdfError::Unterminated),
                    Some('"') => break,
                    Some('\\') if matches!(chars.get(i + 1), Some('\\' | '"' | 'n' | 't')) => {
                        s.push(match chars[i + 1] {
                            'n' => '\n',
                            't' => '\t',
                            other => other,
                        });
                        i += 2;
                    }
                    Some(&ch) => {
                        s.push(ch);
                        i += 1;
                    }
                }
            }
            out.push(Tok::Str(s));
            i += 1;
        } else {
            let start = i;
            while i < chars.len() && !chars[i].is_whitespace() && !matches!(chars[i], '{' | '}' | '"') {
                i += 1;
            }
            out.push(Tok::Str(chars[start..i].iter().collect()));
        }
    }
    Ok(out)
}

pub fn parse_text(text: &str) -> Result<Map, VdfError> {
    let mut stack = vec![Map::default()];
    let mut key: Option<String> = None;
    for tok in tokens(text)? {
        match tok {
            Tok::Open => {
                let name = key.take().ok_or(VdfError::Nameless)?;
                stack.last_mut().unwrap().0.push((name, Value::Map(Map::default())));
                stack.push(Map::default());
            }
            Tok::Close => {
                if stack.len() == 1 || key.is_some() {
                    return Err(VdfError::Braces);
                }
                let done = stack.pop().unwrap();
                let parent = stack.last_mut().unwrap();
                parent.0.last_mut().unwrap().1 = Value::Map(done);
            }
            Tok::Str(s) => match key.take() {
                None => key = Some(s),
                Some(k) => stack.last_mut().unwrap().0.push((k, Value::Str(s))),
            },
        }
    }
    if stack.len() != 1 || key.is_some() {
        return Err(VdfError::Braces);
    }
    Ok(stack.pop().unwrap())
}

fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Steam's own layout: tab indents, key and value split by two tabs.
pub fn format_text(map: &Map) -> String {
    fn walk(map: &Map, depth: usize, out: &mut String) {
        let pad = "\t".repeat(depth);
        for (k, v) in &map.0 {
            match v {
                Value::Str(s) => {
                    let _ = writeln!(out, "{pad}{}\t\t{}", quote(k), quote(s));
                }
                Value::Map(m) => {
                    let _ = writeln!(out, "{pad}{}\n{pad}{{", quote(k));
                    walk(m, depth + 1, out);
                    let _ = writeln!(out, "{pad}}}");
                }
            }
        }
    }
    let mut out = String::new();
    walk(map, 0, &mut out);
    out
}

/// A binary VDF value (shortcuts.vdf uses maps, strings and int32s).
#[derive(Debug, Clone, PartialEq)]
pub enum Bin {
    Map(Vec<(String, Bin)>),
    Str(String),
    Int(i32),
}

impl Bin {
    pub fn get(&self, key: &str) -> Option<&Bin> {
        match self {
            Bin::Map(pairs) => pairs.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Bin::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_int(&self) -> Option<i32> {
        match self {
            Bin::Int(i) => Some(*i),
            _ => None,
        }
    }
}

const B_MAP: u8 = 0;
const B_STR: u8 = 1;
const B_INT: u8 = 2;
const B_END: u8 = 8;

fn cstr(data: &[u8], at: usize) -> Result<(String, usize), VdfError> {
    let end = data[at..].iter().position(|&b| b == 0).ok_or(VdfError::Truncated)? + at;
    Ok((String::from_utf8_lossy(&data[at..end]).into_owned(), end + 1))
}

fn read_map(data: &[u8], mut at: usize) -> Result<(Vec<(String, Bin)>, usize), VdfError> {
    let mut out = Vec::new();
    loop {
        let kind = *data.get(at).ok_or(VdfError::Truncated)?;
        if kind == B_END {
            return Ok((out, at + 1));
        }
        let (key, next) = cstr(data, at + 1)?;
        at = next;
        let value = match kind {
            B_MAP => {
                let (m, next) = read_map(data, at)?;
                at = next;
                Bin::Map(m)
            }
            B_STR => {
                let (s, next) = cstr(data, at)?;
                at = next;
                Bin::Str(s)
            }
            B_INT => {
                let bytes = data.get(at..at + 4).ok_or(VdfError::Truncated)?;
                at += 4;
                Bin::Int(i32::from_le_bytes(bytes.try_into().unwrap()))
            }
            other => return Err(VdfError::BinaryType(other, at)),
        };
        out.push((key, value));
    }
}

/// Reads a whole binary VDF file. An empty file is an empty map.
pub fn parse_binary(data: &[u8]) -> Result<Bin, VdfError> {
    if data.is_empty() {
        return Ok(Bin::Map(Vec::new()));
    }
    let (pairs, _) = read_map(data, 0)?;
    Ok(Bin::Map(pairs))
}

pub fn write_binary(value: &Bin) -> Vec<u8> {
    fn walk(pairs: &[(String, Bin)], out: &mut Vec<u8>) {
        for (k, v) in pairs {
            let tag = match v {
                Bin::Map(_) => B_MAP,
                Bin::Str(_) => B_STR,
                Bin::Int(_) => B_INT,
            };
            out.push(tag);
            out.extend_from_slice(k.as_bytes());
            out.push(0);
            match v {
                Bin::Map(m) => walk(m, out),
                Bin::Str(s) => {
                    out.extend_from_slice(s.as_bytes());
                    out.push(0);
                }
                Bin::Int(i) => out.extend_from_slice(&i.to_le_bytes()),
            }
        }
        out.push(B_END);
    }
    let mut out = Vec::new();
    if let Bin::Map(pairs) = value {
        walk(pairs, &mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIBS: &str = r#"
"libraryfolders"
{
	"0"
	{
		"path"		"C:\\Program Files (x86)\\Steam"
		"apps" { "45760" "123" }
	}
	// a comment
	"1"
	{
		"path"		"D:\\SteamLibrary"
	}
}
"#;

    #[test]
    fn parses_nested_text_and_ignores_case() {
        let m = parse_text(LIBS).unwrap();
        let lf = m.map("LibraryFolders").unwrap();
        assert_eq!(lf.map("0").unwrap().str("path"), Some(r"C:\Program Files (x86)\Steam"));
        assert_eq!(lf.map("1").unwrap().str("PATH"), Some(r"D:\SteamLibrary"));
        assert_eq!(lf.path(&["0", "apps"]).unwrap().str("45760"), Some("123"));
    }

    #[test]
    fn text_round_trip_keeps_order_and_repeats() {
        let src = "\"a\"\n{\n\t\"k\"\t\t\"1\"\n\t\"k\"\t\t\"2\"\n\t\"q\"\t\t\"say \\\"hi\\\"\"\n}\n";
        let m = parse_text(src).unwrap();
        assert_eq!(format_text(&m), src);
    }

    #[test]
    fn refuses_broken_text() {
        assert_eq!(parse_text("\"a\" { \"b\" \"c\""), Err(VdfError::Braces));
        assert_eq!(parse_text("\"a"), Err(VdfError::Unterminated));
        assert!(matches!(parse_text("\"a\" \"b\" [$WIN32]"), Err(VdfError::Conditional(_))));
    }

    #[test]
    fn set_and_child_path_edit_in_place() {
        let mut m = parse_text("\"S\" { \"Apps\" { \"1\" { \"x\" \"y\" } } }").unwrap();
        m.child_path(&["s", "apps", "1"]).set("LaunchOptions", "w %command%");
        m.child_path(&["S", "Apps", "2"]).set("LaunchOptions", "z");
        let apps = m.path(&["S", "Apps"]).unwrap();
        assert_eq!(apps.map("1").unwrap().str("x"), Some("y"));
        assert_eq!(apps.map("1").unwrap().str("launchoptions"), Some("w %command%"));
        assert_eq!(apps.map("2").unwrap().str("LaunchOptions"), Some("z"));
    }

    #[test]
    fn binary_round_trip() {
        let v = Bin::Map(vec![(
            "shortcuts".into(),
            Bin::Map(vec![(
                "0".into(),
                Bin::Map(vec![
                    ("appid".into(), Bin::Int(-1234)),
                    ("AppName".into(), Bin::Str("SF4 Ember".into())),
                    ("tags".into(), Bin::Map(vec![("0".into(), Bin::Str("Ember".into()))])),
                ]),
            )]),
        )]);
        let bytes = write_binary(&v);
        assert_eq!(parse_binary(&bytes).unwrap(), v);
        assert_eq!(*bytes.last().unwrap(), B_END);
    }

    #[test]
    fn binary_refuses_truncation() {
        let bytes = write_binary(&Bin::Map(vec![("a".into(), Bin::Int(1))]));
        assert_eq!(parse_binary(&bytes[..bytes.len() - 3]), Err(VdfError::Truncated));
        assert_eq!(parse_binary(&[]).unwrap(), Bin::Map(vec![]));
    }
}
