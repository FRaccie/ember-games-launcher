// Ember Games Launcher page. Talks to the Rust side through invoke(); every
// string from outside (paths, release titles) goes through esc().
"use strict";

const T = window.__TAURI__;
const invoke = (cmd, args) => T.core.invoke(cmd, args);
const $ = (s) => document.querySelector(s);
const esc = (v) => String(v ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));

let ov = null;          // overview from Rust
let sel = null;         // selected game id
const latest = {};      // id -> { release } | { error }
let installing = null;  // id being installed

function toast(text) {
  const t = $("#toast");
  t.textContent = text;
  t.classList.add("on");
  clearTimeout(toast.timer);
  toast.timer = setTimeout(() => t.classList.remove("on"), 5000);
}

function errText(e) {
  return typeof e === "string" ? e : (e && e.message) || String(e);
}

// ---- status of one game ----

function gameState(g) {
  if (!g.game) return { tone: "bad", chip: "Game not found", primary: "Find game folder", act: "find" };
  if (ov.platform === "linux" && !g.prefixReady)
    return { tone: "wait", chip: "Start the game once from Steam", primary: "Find game folder", act: "prefix" };
  if (!g.installed && g.existing.length)
    return { tone: "wait", chip: "Found a copy installed outside the launcher", primary: "Use this copy", act: "adopt" };
  if (!g.installed) return { tone: "", chip: "Game found · mod not installed", primary: "Install mod", act: "install" };
  if (g.changedOutside)
    return { tone: "wait", chip: "Changed outside the launcher", primary: "Check files", act: "check" };
  const l = latest[g.id];
  if (l && l.release && l.release.tag !== g.installed.tag)
    return { tone: "wait", chip: "Update ready · " + l.release.tag, primary: "Update and play", act: "update" };
  if (ov.platform === "linux" && !g.launchReady)
    return { tone: "wait", chip: "Needs Steam setup", primary: "Finish Steam setup", act: "setup" };
  return { tone: "ok", chip: "Ready · " + g.installed.tag, primary: "Play", act: "play" };
}

const SOURCES = {
  setup: "Installed by its own setup",
  steamEntry: "Started by a Steam entry",
  linkHandler: "Opens Ember links",
  usualFolder: "In the usual folder",
};
function copyVersion(c) {
  return c.version || c.setupVersion || "version unknown";
}

function channelLabel(g) {
  const c = g.channels.find((c) => c.id === g.channel);
  return c ? c.label : g.channel;
}

// ---- rendering ----

function renderList() {
  $("#game-list").innerHTML = ov.games.map((g) => {
    const s = gameState(g);
    return `<button class="game ${g.id === sel ? "on" : ""}" data-game="${esc(g.id)}">
      <span class="tile"><img class="art" src="brand/${esc(g.id)}/${esc(g.id)}-ember-icon.png" alt="" data-fallback="${esc(g.short)}"></span>
      <span class="txt"><span class="nm">${esc(g.name)}</span><span class="chip ${s.tone}">${esc(s.chip)}</span></span>
    </button>`;
  }).join("");
}

function row(k, v, action, data) {
  const btn = action ? `<button class="link" ${data}>${esc(action)}</button>` : "";
  return `<div class="row"><span class="k">${esc(k)}</span><span class="v" title="${esc(v)}">${esc(v)}</span>${btn}</div>`;
}

function renderDetail() {
  const g = ov.games.find((g) => g.id === sel);
  if (!g) { $("#detail").innerHTML = ""; return; }
  const s = gameState(g);
  const l = latest[g.id];
  const rows = [];
  rows.push(row("Game folder", g.game ? g.game.dir : "Not found in any Steam library", g.game ? "Change" : "Pick folder", 'data-do="find"'));
  if (g.versionLabel) rows.push(row("Version found", g.versionLabel + " (app " + g.game.appid + ")"));
  const have = g.installed ? g.installed.tag : "Not installed";
  const avail = l ? (l.release ? l.release.tag : "Could not check: " + l.error) : "Checking…";
  rows.push(row("Mod version", g.installed ? `${have} installed · newest ${avail}` : `${have} · newest ${avail}`, l && l.release ? "What changed" : "", 'data-do="notes"'));
  rows.push(row("Channel", channelLabel(g), g.channels.length > 1 ? "‹ " + channelLabel(g) + " ›" : "", 'data-do="channel"'));
  if (g.installed) rows.push(row("Mod folder", g.installed.dir + (g.installed.adopted ? " (taken over)" : ""), "Open", 'data-folder="mod"'));
  if (g.installed && g.existing.length) rows.push(row("Other copies", g.existing.map((c) => c.dir).join(" · ")));
  if (g.installed) rows.push(row("Steam entry", g.steamEntry ? `Added as "${g.modName}"` : "Not added", g.steamEntry ? "Remove" : "Add", g.steamEntry ? 'data-do="unsteam"' : 'data-do="steam"'));
  rows.push(row("Runs with", ov.platform === "linux" ? "Proton, in the game's own prefix" : "Windows, direct"));

  let note = "";
  if (s.act === "prefix") note = `<div class="note warn">Proton has not made this game's folder yet. Start ${esc(g.name)} once from Steam, quit it, then come back.</div>`;
  if (s.act === "adopt") note = `<div class="note stacked">
      <p>${esc(g.modName)} is already on this PC. The launcher can take that copy over: it updates and checks it in place, keeps your settings, and adds no second copy.</p>
      <div class="rows">${g.existing.map((c, i) => `<div class="row"><span class="k">${esc(SOURCES[c.source] || c.source)}</span>
        <span class="v" title="${esc(c.dir)}">${esc(copyVersion(c))} · ${esc(c.dir)}</span>
        <button class="link" data-adopt="${i}">Use this one</button></div>`).join("")}</div>
      <div class="buttons"><button class="btn ghost" data-do="install">Install a separate copy</button></div></div>`;
  if (s.act === "check") note = `<div class="note warn">The mod's files changed since the launcher last saw them, maybe by the mod's own updater. Check files finds out which release this copy is and whether any file is damaged.</div>`;
  if (s.act === "setup") note = `<div class="note warn">Steam needs one change so it can start the mod: the game's launch option gets Ember's wrapper. The game's own Play button still starts the plain game. Steam closes for a moment and opens again.</div>`;

  $("#detail").innerHTML = `
    <div class="hero"><img class="hero-art art" src="brand/${esc(g.id)}/${esc(g.id)}-ember-hero.png" alt=""><div>
      <span class="chip ${s.tone}">${esc(s.chip)}</span>
      <h1>${esc(g.name)}</h1><div class="rule"></div>
      <p>${esc(g.modName)}${g.blurb ? " · " + esc(g.blurb) : ""}</p>
    </div></div>
    <div class="actions">
      <button class="btn primary" data-do="${s.act}" ${installing ? "disabled" : ""}><span class="glyph">A</span>${esc(s.primary)}</button>
      ${g.installed && !g.steamEntry ? `<button class="btn" data-do="steam">Add to Steam</button>` : ""}
      ${g.installed && s.act !== "check" ? `<button class="btn" data-do="check">Check files</button>` : ""}
      ${g.installed ? `<button class="btn" data-do="install">Reinstall</button>` : ""}
      <button class="btn ghost" data-do="options"><span class="glyph">Y</span>Game options</button>
    </div>
    ${note}
    <div class="rows">${rows.join("")}</div>`;
}

function renderHeader() {
  const dot = $("#steam-dot");
  dot.className = "dot " + (ov.steam ? "ok" : "bad");
  $("#steam-text").textContent = (ov.steam ? "Steam found" : "Steam not found") + " · " + (ov.platform === "linux" ? "Linux" : "Windows");
}

async function refresh() {
  ov = await invoke("overview");
  if (!sel || !ov.games.some((g) => g.id === sel)) sel = (ov.games.find((g) => g.installed) || ov.games.find((g) => g.game) || ov.games[0] || {}).id;
  renderHeader();
  renderList();
  renderDetail();
  renderSettings();
}

async function checkLatest(id) {
  try { latest[id] = { release: await invoke("latest", { id }) }; }
  catch (e) { latest[id] = { error: errText(e) }; }
  if (ov) { renderList(); renderDetail(); }
}

function checkAll() {
  ov.games.forEach((g) => checkLatest(g.id));
}

// A game the launcher has no artwork for keeps a text tile (and a plain hero).
document.addEventListener("error", (ev) => {
  const img = ev.target;
  if (!(img instanceof HTMLImageElement) || !img.classList.contains("art")) return;
  if (img.dataset.fallback) img.replaceWith(document.createTextNode(img.dataset.fallback));
  else img.remove();
}, true);

// ---- dialogs ----

let dialogBack = null;
function openDialog(html, onBack) {
  $("#dialog").innerHTML = html;
  $("#scrim").classList.add("on");
  dialogBack = onBack || closeDialog;
  const first = $("#dialog [data-primary]") || $("#dialog button");
  if (first) first.focus();
}
function closeDialog() {
  $("#scrim").classList.remove("on");
  $("#dialog").innerHTML = "";
  dialogBack = null;
  focusSelected();
}

const STEPS = ["Read the catalog and pick the release", "Download the package", "Check the file against its checksum", "Unpack it into its own folder", "Done"];

async function install(id, thenPlay) {
  const g = ov.games.find((g) => g.id === id);
  installing = id;
  renderDetail();
  openDialog(`
    <span class="eyebrow">${esc(g.name)}</span>
    <h1>Installing ${esc(g.modName)}</h1><div class="rule"></div>
    <div><div class="muted" id="dl-text">Starting…</div></div>
    <div class="bar" role="progressbar" aria-label="Download progress"><div id="dl-bar"></div></div>
    <ol class="steps">${STEPS.map((s, i) => `<li data-step="${i}"><span class="d"></span>${esc(s)}</li>`).join("")}</ol>
    <p class="muted">${g.installed && g.installed.adopted
      ? "This updates the copy the launcher took over, in its own folder."
      : "The mod goes in a folder of its own, never into the game's folder."} Updating keeps the files the mod wrote there (settings, logs).</p>
    <div class="buttons"><button class="btn" data-close>Run in background</button></div>`);
  try {
    await invoke("install_mod", { id });
    closeDialog();
    toast(g.modName + " is installed.");
    await refresh();
    const now = ov.games.find((x) => x.id === id);
    if (thenPlay && now) await play(id);
    else if (ov.platform === "linux" && now && !now.launchReady) steamSetup(id);
  } catch (e) {
    $("#dialog").innerHTML = `<span class="eyebrow">${esc(g.name)}</span><h1>Install did not finish</h1><div class="rule"></div>
      <div class="note err">${esc(errText(e))}</div><p class="muted">Nothing in the game's folder was changed, and a mod that was already installed is still there.</p>
      <div class="buttons"><button class="btn" data-close>Close</button><button class="btn primary" data-primary data-retry="${esc(id)}">Try again</button></div>`;
  } finally {
    installing = null;
    renderDetail();
  }
}

function onProgress(p) {
  if (!$("#dl-bar")) return;
  document.querySelectorAll("ol.steps li").forEach((li) => {
    const i = +li.dataset.step;
    li.className = i < p.step ? "done" : i === p.step ? "now" : "";
    li.querySelector(".d").textContent = i < p.step ? "✓" : i === p.step ? "•" : "";
  });
  if (p.step === 1 && p.total) {
    const pct = Math.floor((p.done / p.total) * 100);
    $("#dl-bar").style.width = pct + "%";
    $("#dl-text").textContent = `Downloading · ${pct}% · ${(p.done / 1048576).toFixed(1)} of ${(p.total / 1048576).toFixed(1)} MB`;
  } else if (p.step > 1) {
    $("#dl-bar").style.width = "100%";
    $("#dl-text").textContent = STEPS[p.step];
  }
}

async function play(id) {
  try {
    await invoke("play", { id });
    toast("Starting " + ov.games.find((g) => g.id === id).modName + "…");
  } catch (e) {
    if (errText(e) === "steam-setup") steamSetup(id);
    else toast(errText(e));
  }
}

function steamWarn() {
  return ov.steamRunning
    ? `<div class="note warn">Steam is running. It must close for a moment to save this, then it opens again by itself.</div>`
    : "";
}

function steamSetup(id) {
  const g = ov.games.find((g) => g.id === id);
  openDialog(`<span class="eyebrow">${esc(g.name)}</span><h1>Finish Steam setup</h1><div class="rule"></div>
    <p class="muted">On Linux the mod runs in the game's own Proton session. Ember's wrapper goes in front of ${esc(g.name)}'s launch option; it starts the mod only when the launcher asks, so the game's own Play button still starts the plain game.</p>
    ${steamWarn()}
    <div class="buttons"><button class="btn" data-close>Not now</button>
    <button class="btn primary" data-primary data-setup="${esc(id)}">${ov.steamRunning ? "Restart Steam and set up" : "Set up"}</button></div>`);
}

async function doSetup(id) {
  $("#dialog .buttons").innerHTML = `<span class="muted">Working… Steam may close and open again.</span>`;
  try {
    await invoke("steam_setup", { id, restart: true });
    closeDialog();
    toast("Steam is set up for " + ov.games.find((g) => g.id === id).modName + ".");
    await refresh();
  } catch (e) {
    closeDialog();
    toast(errText(e));
  }
}

async function addToSteam(id) {
  const g = ov.games.find((g) => g.id === id);
  let entry;
  try { entry = await invoke("steam_entry_preview", { id }); } catch (e) { toast(errText(e)); return; }
  openDialog(`<span class="eyebrow">${esc(g.name)}</span><h1>Add ${esc(g.modName)} to Steam</h1><div class="rule"></div>
    <p class="muted">Adds its own entry to your Steam library, so it starts from Big Picture, Steam Deck Game Mode and Steam Input layouts. The normal game entry stays as it is.</p>
    <div class="rows">
      ${row("Name in Steam", entry.name)}
      ${row("Starts", entry.exe.replace(/"/g, "") + (entry.launchOptions ? " " + entry.launchOptions : ""))}
      ${row("Collection", "Ember")}
      ${row("Pictures", entry.icon ? "From the mod's own artwork" : "Steam's default (the mod has no artwork yet)")}
    </div>
    ${steamWarn()}
    <div class="buttons"><button class="btn" data-close>Not now</button>
    <button class="btn primary" data-primary data-addsteam="${esc(id)}">${ov.steamRunning ? "Restart Steam and add" : "Add"}</button></div>`);
}

async function doAddSteam(id, remove) {
  $("#dialog .buttons") && ($("#dialog .buttons").innerHTML = `<span class="muted">Working… Steam may close and open again.</span>`);
  try {
    await invoke(remove ? "remove_from_steam" : "add_to_steam", { id, restart: true });
    closeDialog();
    toast(remove ? "Removed from Steam." : "Added to Steam.");
    await refresh();
  } catch (e) {
    closeDialog();
    toast(errText(e));
  }
}

async function findFolder(id) {
  const dir = await T.dialog.open({ directory: true, title: "Pick the game's folder" });
  if (!dir) return;
  try { await invoke("set_folder", { id, folder: dir }); await refresh(); toast("Game folder saved."); }
  catch (e) { toast(errText(e)); }
}

async function cycleChannel(id) {
  const g = ov.games.find((g) => g.id === id);
  const i = g.channels.findIndex((c) => c.id === g.channel);
  const next = g.channels[(i + 1) % g.channels.length];
  await invoke("set_channel", { id, channel: next.id });
  delete latest[id];
  await refresh();
  checkLatest(id);
  if (g.installed) toast(`Channel is ${next.label}. Update to switch the installed build.`);
}

function options(id) {
  const g = ov.games.find((g) => g.id === id);
  openDialog(`<span class="eyebrow">${esc(g.name)}</span><h1>Game options</h1><div class="rule"></div>
    <div class="grid">
      ${g.installed ? `<button class="btn" data-folder="mod">Open mod folder</button>` : ""}
      ${g.game ? `<button class="btn" data-folder="game">Open game folder</button>` : ""}
      <button class="btn" data-do-dialog="find">Change game folder</button>
      ${g.installed ? `<button class="btn" data-bundle="${esc(id)}">Zip new logs</button>` : ""}
      ${g.installed ? `<button class="btn ghost" data-uninstall="${esc(id)}">${g.installed.adopted ? "Stop managing" : "Uninstall mod"}</button>` : ""}
    </div>
    <div class="buttons"><button class="btn" data-close>Back</button></div>`);
}

function notes(id) {
  const l = latest[id];
  if (!l || !l.release) return;
  const r = l.release;
  openDialog(`<span class="eyebrow">${esc(r.tag)}</span><h1>${esc(r.title)}</h1><div class="rule"></div>
    <div class="note notes-box">${esc(r.notes || "No notes on this release.")}</div>
    <div class="buttons"><button class="btn ghost" data-page="${esc(r.page)}">Open release page</button><button class="btn" data-close>Back</button></div>`);
}

async function uninstall(id) {
  const g = ov.games.find((g) => g.id === id);
  if (g.installed && g.installed.adopted) return openDialog(`<span class="eyebrow">${esc(g.name)}</span><h1>Stop managing ${esc(g.modName)}?</h1><div class="rule"></div>
    <p class="muted">The launcher forgets this copy and stops updating it. The folder stays as it is; remove it with its own uninstaller if you want it gone.</p>
    <div class="buttons"><button class="btn" data-close data-primary>Keep managing</button><button class="btn ghost" data-uninstall-yes="${esc(id)}">Stop managing</button></div>`);
  openDialog(`<span class="eyebrow">${esc(g.name)}</span><h1>Uninstall ${esc(g.modName)}?</h1><div class="rule"></div>
    <p class="muted">Its folder is removed. The game itself and the mod's settings in your user folder stay.</p>
    <div class="buttons"><button class="btn" data-close data-primary>Keep it</button><button class="btn ghost" data-uninstall-yes="${esc(id)}">Uninstall</button></div>`);
}

async function adopt(id, index, quiet) {
  const g = ov.games.find((g) => g.id === id);
  const c = g.existing[index];
  if (!c) return;
  try {
    const rec = await invoke("adopt_mod", { id, dir: c.dir });
    if (!quiet) toast(`${g.modName} ${rec.tag === "unknown" ? "" : rec.tag + " "}is now managed by the launcher.`);
  } catch (e) { toast(errText(e)); }
  await refresh();
  checkLatest(id);
}

async function checkFiles(id) {
  const g = ov.games.find((g) => g.id === id);
  openDialog(`<span class="eyebrow">${esc(g.name)}</span><h1>Checking ${esc(g.modName)}</h1><div class="rule"></div>
    <div class="muted">Finding which release this copy is, then comparing every file with it…</div>
    <div class="buttons"><button class="btn" data-close>Close</button></div>`);
  let r;
  try { r = await invoke("check_mod", { id }); }
  catch (e) {
    openDialog(`<span class="eyebrow">${esc(g.name)}</span><h1>Could not check</h1><div class="rule"></div>
      <div class="note err">${esc(errText(e))}</div><div class="buttons"><button class="btn" data-close data-primary>Close</button></div>`);
    return;
  }
  await refresh();
  const repair = `<button class="btn primary" data-primary data-repair="${esc(id)}">Repair</button>`;
  if (!r.check) {
    openDialog(`<span class="eyebrow">${esc(g.name)}</span><h1>No release matches this copy</h1><div class="rule"></div>
      <p class="muted">It may be a test build or changed by hand. Repair installs the newest ${esc(channelLabel(g))} release over it and keeps your settings and logs.</p>
      <div class="buttons"><button class="btn" data-close>Leave it</button>${repair}</div>`);
    return;
  }
  const c = r.check;
  if (!c.missing.length && !c.changed.length) {
    openDialog(`<span class="eyebrow">${esc(g.name)}</span><h1>All files are fine</h1><div class="rule"></div>
      <p class="muted">All ${c.files} files are the ones in ${esc(r.tag)}.</p>
      <div class="buttons"><button class="btn primary" data-primary data-close>Done</button></div>`);
    return;
  }
  const list = [...c.missing.map((f) => "Missing · " + f), ...c.changed.map((f) => "Changed · " + f)];
  openDialog(`<span class="eyebrow">${esc(g.name)}</span><h1>${list.length} of ${c.files} files differ from ${esc(r.tag)}</h1><div class="rule"></div>
    <div class="note notes-box">${list.slice(0, 12).map(esc).join("<br>")}${list.length > 12 ? "<br>… and " + (list.length - 12) + " more" : ""}</div>
    <p class="muted">Repair installs the newest ${esc(channelLabel(g))} release over this copy. Your settings and logs stay.</p>
    <div class="buttons"><button class="btn" data-close>Leave it</button>${repair}</div>`);
}

async function firstRun() {
  const found = ov.games.filter((g) => g.game);
  const picks = new Set(found.filter((g) => !g.installed && (g.existing.length || ov.platform !== "linux" || g.prefixReady)).map((g) => g.id));
  const label = () => {
    const take = [...picks].filter((id) => ov.games.find((g) => g.id === id).existing.length).length;
    const add = picks.size - take;
    const parts = [];
    if (add) parts.push(`Install ${add} mod${add === 1 ? "" : "s"}`);
    if (take) parts.push(`${add ? "take over" : "Take over"} ${take}`);
    return parts.join(", ") || "Install";
  };
  const draw = () => openDialog(`
    <span class="eyebrow">Welcome</span>
    <h1>We found ${found.length} of your ${ov.games.length} games</h1><div class="rule"></div>
    <p class="muted">Pick which Ember mods to set up. A mod that is already on this PC is taken over: the launcher updates that copy instead of installing a second one. You can change this later in the Library.</p>
    <div class="stack">
    ${ov.games.map((g) => {
      const can = g.game && !g.installed;
      const chip = !g.game ? `<span class="chip bad">Not found</span>` : g.installed ? `<span class="chip wait">Already set up</span>`
        : g.existing.length ? `<span class="chip wait">Installed · ${esc(copyVersion(g.existing[0]))}</span>`
        : `<span class="chip ok">Found${g.versionLabel ? " · " + esc(g.versionLabel) : ""}</span>`;
      const where = g.existing.length && !g.installed ? "Mod in " + g.existing[0].dir : g.game ? g.game.dir : "Not in any Steam library";
      return `<button class="pick ${picks.has(g.id) ? "on" : ""}" data-pick="${esc(g.id)}" ${can ? "" : "disabled"}>
        <span class="box" aria-hidden="true">${picks.has(g.id) ? "✓" : ""}</span>
        <span class="txt"><span>${esc(g.name)}</span><span class="where">${esc(where)}</span></span>${chip}</button>`;
    }).join("")}</div>
    <div class="buttons"><button class="btn" data-close>Skip for now</button>
    <button class="btn primary" data-primary data-installpicks ${picks.size ? "" : "disabled"}>${esc(label())}</button></div>`);
  firstRun.picks = picks;
  firstRun.draw = draw;
  draw();
}

// ---- logs page ----

let logsView = {};

async function renderLogs() {
  const v = await invoke("logs_view");
  logsView = v;
  const counts = Object.fromEntries(v.found);
  $("#logs-games").innerHTML = ov.games.map((g) => `<div class="card"><span class="nm">${esc(g.name)}</span>
    <span class="muted">${counts[g.id] ? counts[g.id] + " log file" + (counts[g.id] === 1 ? "" : "s") : "No logs yet"}</span>
    <button class="btn small" data-bundle="${esc(g.id)}" ${counts[g.id] ? "" : "disabled"}>Zip new logs</button></div>`).join("");
  const line = (b) => `<div class="row"><span class="k">${esc(b.games.join(", "))}</span>
    <span class="v">${esc(b.file)} · ${b.parts} file${b.parts === 1 ? "" : "s"} · ${(b.size / 1024).toFixed(0)} KB</span>
    ${b.sent ? (b.code ? `<span class="code">${esc(b.code)}</span>` : "") : `<button class="link" data-send="${esc(b.file)}" ${ov.reportsConfigured ? "" : "disabled"}>Send</button>`}</div>`;
  $("#outbox").innerHTML = v.outbox.length ? v.outbox.map(line).join("") : `<div class="row"><span class="v muted">Nothing waiting.</span></div>`;
  $("#sent").innerHTML = v.sent.length ? v.sent.slice(0, 20).map(line).join("") : `<div class="row"><span class="v muted">Nothing sent yet.</span></div>`;
  $("#logs-note").innerHTML = ov.reportsConfigured ? "" :
    `<div class="note warn">Sending is not set up yet. Bundles are saved in the logs folder; attach one to a bug report by hand for now.</div>`;
}

async function bundle(ids) {
  try {
    const b = await invoke("make_log_bundle", { ids });
    toast(b ? `Zipped ${b.parts} new log file${b.parts === 1 ? "" : "s"}: ${b.file}` : "No new logs since the last bundle you sent.");
  } catch (e) { toast(errText(e)); }
  if ($("#view-logs").classList.contains("on")) renderLogs();
}

// Send asks first: what goes, an optional note, and who reads it. Then it shows the report code.
function send(file) {
  const b = (logsView.outbox || []).find((x) => x.file === file);
  if (!b) return;
  openDialog(`
    <span class="eyebrow">Send logs</span>
    <h1>Send to the Ember developers</h1><div class="rule"></div>
    <div class="rows"><div class="row"><span class="k">Games</span><span class="v">${esc(b.games.join(", "))}</span></div>
    <div class="row"><span class="k">Files</span><span class="v">${b.parts} file${b.parts === 1 ? "" : "s"} · ${(b.size / 1024).toFixed(0)} KB</span></div></div>
    <label class="field"><span>What went wrong? (optional)</span>
    <textarea id="send-note" maxlength="1000" rows="4" placeholder="For example: the game froze on stage select after a rematch."></textarea></label>
    <div class="muted">Your account name is taken out of file paths. Logs are kept 90 days and read only by the Ember developers.</div>
    <div class="buttons"><button class="btn" data-close>Cancel</button>
    <button class="btn primary" data-primary data-sendyes="${esc(file)}">Send</button></div>`);
}

async function sendNow(file) {
  const note = ($("#send-note") || {}).value || "";
  const btn = $("#dialog [data-sendyes]");
  if (btn) { btn.disabled = true; btn.textContent = "Sending…"; }
  try {
    const code = await invoke("send_log_bundle", { file, note });
    openDialog(`
      <span class="eyebrow">Sent</span>
      <h1>Your report code</h1><div class="rule"></div>
      <div class="report-code">${esc(code || "sent")}</div>
      <div class="muted">Paste this code in the Ember Discord's #support channel so the developers can find your logs.</div>
      <div class="buttons"><button class="btn" data-copy="${esc(code)}">Copy code</button>
      <button class="btn primary" data-primary data-close>Done</button></div>`);
  } catch (e) {
    closeDialog();
    toast(errText(e));
  }
  renderLogs();
}

// ---- settings page ----

let appVersion = "";
if (T.app) T.app.getVersion().then((v) => { appVersion = v; }, () => {});

function renderSettings() {
  $("#settings-rows").innerHTML = [
    row("Steam", ov.steam || "Not found"),
    row("Catalog", ov.catalogSource === "online" ? "Newest, from the launcher's repo" : ov.catalogSource === "saved" ? "Last one fetched (offline)" : ov.catalogSource === "file" ? "A local file (EMBER_GAMES_CATALOG)" : "The copy built into this launcher"),
    row("Launcher data", "Mods, downloads and logs", "Open", 'data-folder-global="data"'),
    row("Version", "Ember Games Launcher " + (appVersion || "")),
  ].join("");
}

// ---- tabs and input ----

function showTab(name) {
  document.querySelectorAll("nav button").forEach((b) => b.setAttribute("aria-selected", String(b.dataset.tab === name)));
  document.querySelectorAll(".view").forEach((v) => v.classList.toggle("on", v.id === "view-" + name));
  if (name === "logs") renderLogs();
  const first = document.querySelector(".view.on button:not(:disabled)");
  if (first) first.focus();
}

function focusSelected() {
  const b = document.querySelector(`.game[data-game="${CSS.escape(sel || "")}"]`);
  if (b && $("#view-library").classList.contains("on")) b.focus();
}

document.addEventListener("click", async (ev) => {
  const t = ev.target.closest("button");
  if (!t) return;
  const d = t.dataset;
  if (d.tab) return showTab(d.tab);
  if (d.game) { sel = d.game; renderList(); renderDetail(); focusSelected(); return; }
  if (d.close !== undefined) return closeDialog();
  if (d.retry) return install(d.retry);
  if (d.setup) return doSetup(d.setup);
  if (d.addsteam) return doAddSteam(d.addsteam, false);
  if (d.pick) { const p = firstRun.picks; p.has(d.pick) ? p.delete(d.pick) : p.add(d.pick); return firstRun.draw(); }
  if (d.installpicks !== undefined) {
    closeDialog();
    for (const id of firstRun.picks) {
      const g = ov.games.find((g) => g.id === id);
      if (g.existing.length) await adopt(id, 0, true); else await install(id);
    }
    return;
  }
  if (d.adopt !== undefined) return adopt(sel, +d.adopt);
  if (d.repair) { closeDialog(); return install(d.repair); }
  if (d.folder) return invoke("open_folder", { kind: d.folder, id: sel }).catch((e) => toast(errText(e)));
  if (d.folderGlobal) return invoke("open_folder", { kind: d.folderGlobal }).catch((e) => toast(errText(e)));
  if (d.page) return invoke("open_release_page", { url: d.page }).catch((e) => toast(errText(e)));
  if (d.bundle) { closeDialog(); return bundle([d.bundle]); }
  if (d.send) return send(d.send);
  if (d.sendyes) return sendNow(d.sendyes);
  if (d.copy) return navigator.clipboard.writeText(d.copy).then(() => toast("Copied."), () => toast("Copy failed; select the code instead."));
  if (d.uninstall) return uninstall(d.uninstall);
  if (d.uninstallYes) { closeDialog(); const was = ov.games.find((g) => g.id === d.uninstallYes); try { await invoke("uninstall_mod", { id: d.uninstallYes }); toast(was && was.installed && was.installed.adopted ? "The launcher no longer manages it." : "Uninstalled."); } catch (e) { toast(errText(e)); } return refresh(); }
  if (d.doDialog === "find") { closeDialog(); return findFolder(sel); }
  if (t.id === "rescan") { await refresh(); checkAll(); return toast("Scanned again."); }
  if (t.id === "bundle-all") return bundle([]);
  if (t.id === "open-logs") return invoke("open_folder", { kind: "logs" });
  switch (d.do) {
    case "find": case "prefix": return findFolder(sel);
    case "install": return install(sel);
    case "adopt": return adopt(sel, 0);
    case "check": return checkFiles(sel);
    case "update": return install(sel, true);
    case "play": return play(sel);
    case "setup": return steamSetup(sel);
    case "steam": return addToSteam(sel);
    case "unsteam": return doAddSteam(sel, true);
    case "channel": return cycleChannel(sel);
    case "notes": return notes(sel);
    case "options": return options(sel);
  }
});

// Spatial focus for D-pad, stick and arrow keys: the nearest focusable
// control in that direction, inside the open dialog when there is one.
function focusables() {
  const root = $("#scrim").classList.contains("on") ? $("#dialog") : document;
  return [...root.querySelectorAll("button:not(:disabled)")].filter((b) => b.offsetParent !== null);
}
function move(dir) {
  const items = focusables();
  const cur = document.activeElement && items.includes(document.activeElement) ? document.activeElement : null;
  if (!cur) { items[0] && items[0].focus(); return; }
  const a = cur.getBoundingClientRect();
  const ax = a.left + a.width / 2, ay = a.top + a.height / 2;
  let best = null, bestScore = Infinity;
  for (const b of items) {
    if (b === cur) continue;
    const r = b.getBoundingClientRect();
    const bx = r.left + r.width / 2, by = r.top + r.height / 2;
    const dx = bx - ax, dy = by - ay;
    const main = dir === "left" ? -dx : dir === "right" ? dx : dir === "up" ? -dy : dy;
    if (main <= 1) continue;
    const side = dir === "left" || dir === "right" ? Math.abs(dy) : Math.abs(dx);
    const score = main + side * 2;
    if (score < bestScore) { bestScore = score; best = b; }
  }
  if (best) { best.focus(); best.scrollIntoView({ block: "nearest" }); }
}
function back() {
  if (dialogBack) dialogBack();
}
function cycleTab(step) {
  const tabs = [...document.querySelectorAll("nav button")];
  const i = tabs.findIndex((b) => b.getAttribute("aria-selected") === "true");
  showTab(tabs[(i + step + tabs.length) % tabs.length].dataset.tab);
}

document.addEventListener("keydown", (e) => {
  // Typing in a text field keeps its keys; only Escape still goes back.
  if (e.target instanceof HTMLTextAreaElement || e.target instanceof HTMLInputElement) {
    if (e.key === "Escape") { e.preventDefault(); back(); }
    return;
  }
  const map = { ArrowLeft: "left", ArrowRight: "right", ArrowUp: "up", ArrowDown: "down" };
  if (map[e.key]) { e.preventDefault(); move(map[e.key]); }
  else if (e.key === "Escape" || e.key === "Backspace") { e.preventDefault(); back(); }
  else if (e.key === "PageUp" || e.key === "q") cycleTab(-1);
  else if (e.key === "PageDown" || e.key === "e") cycleTab(1);
  else if (e.key === "y" && sel && !dialogBack) options(sel);
});

// Gamepad: A=0 B=1 Y=3 LB=4 RB=5 D-pad 12-15, left stick with repeat.
const pad = { held: {}, next: 0 };
function pollPads(t) {
  for (const gp of navigator.getGamepads ? navigator.getGamepads() : []) {
    if (!gp) continue;
    const press = (i) => gp.buttons[i] && gp.buttons[i].pressed;
    const edge = (i, fn) => { const p = press(i); if (p && !pad.held[i]) fn(); pad.held[i] = p; };
    edge(0, () => document.activeElement && document.activeElement.click());
    edge(1, back);
    edge(3, () => sel && !dialogBack && options(sel));
    edge(4, () => cycleTab(-1));
    edge(5, () => cycleTab(1));
    const x = gp.axes[0] || 0, y = gp.axes[1] || 0;
    const dir = press(12) || y < -0.6 ? "up" : press(13) || y > 0.6 ? "down" : press(14) || x < -0.6 ? "left" : press(15) || x > 0.6 ? "right" : null;
    if (dir && (dir !== pad.dir || t >= pad.next)) { move(dir); pad.next = t + (dir === pad.dir ? 120 : 380); }
    pad.dir = dir;
  }
  requestAnimationFrame(pollPads);
}
requestAnimationFrame(pollPads);

// ---- start ----

(async () => {
  T.event.listen("install-progress", (e) => onProgress(e.payload));
  T.event.listen("catalog-updated", async () => { await refresh(); checkAll(); });
  await refresh();
  checkAll();
  let seen = null;
  try { seen = localStorage.getItem("welcomed"); } catch (_) { /* private storage */ }
  if (!seen && !ov.games.some((g) => g.installed)) {
    firstRun();
    try { localStorage.setItem("welcomed", "1"); } catch (_) { /* fine */ }
  } else {
    focusSelected();
  }
})();
