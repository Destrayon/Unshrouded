// Enshrouded Planner page. All planner state lives in the Rust backend: this file sends actions ("allocate this
// node", "equip this item") through Tauri's invoke and draws the JSON it gets back.
"use strict";

const { invoke } = window.__TAURI__.core;
const $ = (s, r = document) => r.querySelector(s);
const esc = (s) => String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
const fx = (x, d = 1) => (x == null || !isFinite(x) ? "—" : Number(x).toFixed(d));
const pct = (x, d = 0) => (x == null ? "—" : (x * 100).toFixed(d) + "%");
const debounce = (f, ms) => { let t; return (...a) => { clearTimeout(t); t = setTimeout(() => f(...a), ms); }; };

const CLASS_COLOR = { Ranger: "#6FAE4E", Warrior: "#C8463C", Mage: "#4A86D8" };
const SOURCE_COLORS = ["#E8B04A", "#4FC3C8", "#9CA6EA", "#89E191", "#D97A9A", "#B3A98A"];
const TABS = [["overview", "Overview"], ["tree", "Skill tree"], ["gear", "Gear"], ["setup", "Combat setup"], ["calcs", "Calculations"], ["optimizer", "Optimizer"], ["notes", "Notes"]];

let CAT = null; // fixed game data
let ICON = []; // per skill index: { on: Image, off: Image, src } for active skills that have a game icon
let ST = null; // planner state
let SUM = null; // rail numbers {now}
let PREV = null; // rail numbers with a previewed change {now, preview, label, path}
const ui = {
  tab: "overview", slot: "weapon", search: "", heat: false, heatData: null, heatKey: "", heatBusy: false,
  hover: null, selected: null, pan: { x: 0, y: 0 }, zoom: 0.11, cand: null, candKey: "", candSeq: 0,
  filter: "", sheetQ: "", sheetAll: false, open: { hit: true }, optGear: true, optRarity: "Legendary",
  optBusy: false, optRes: null, optStep: 0, code: "", previewKey: "", ownedOnly: false, allTypes: false,
};

// ------------------------------------------------------------------ plumbing

// A selector for the focused control, so focus can return to it after the page redraws (keyboard users keep their place).
function focusKey(el) {
  if (!el || el === document.body) return null;
  if (el.id) return "#" + CSS.escape(el.id);
  for (const a of ["data-f", "data-do", "data-hk", "data-node-act", "data-slot", "data-guid", "data-go"]) {
    if (el.hasAttribute(a)) return `[${a}="${CSS.escape(el.getAttribute(a))}"]`;
  }
  return null;
}
function refocus(key) {
  const el = key && document.querySelector(key);
  if (el && document.activeElement !== el) el.focus();
}

// a preset runs the optimizer for a few seconds: its buttons say so and wait
async function startPreset(kind) {
  const key = focusKey(document.activeElement);
  const buttons = [...document.querySelectorAll('[data-do^="preset:"]')];
  buttons.forEach((b) => { b.disabled = true; b.setAttribute("aria-busy", "true"); });
  flash("Building a starting point: weapon, skill tree, armour and food…");
  let ok = true;
  try { ST = await invoke("preset", { kind }); } catch (e) { ok = false; flash(String(e), true); }
  buttons.forEach((b) => { b.disabled = false; b.removeAttribute("aria-busy"); });
  await refresh();
  if (ok) flash("Starting point ready. Change anything, or run the optimizer for a deeper search.");
  refocus(key);
}

// returns true when the action went through with nothing to report, so a caller's success message never covers an
// error or a one-time notice
async function act(a) {
  const key = focusKey(document.activeElement);
  let clean = true;
  try {
    ST = await invoke("act", { action: a });
    if (ST.notice) { flash(ST.notice, true); clean = false; }
  } catch (e) {
    clean = false;
    flash(String(e), true);
    // the page's copy may hold the refused value: take the backend's state again, so every field shows the value in
    // use and the next change does not resend the refused one; blur so the focused field redraws too
    ST = await invoke("state");
    if (document.activeElement && document.activeElement.blur) document.activeElement.blur();
  }
  await refresh();
  refocus(key);
  return clean;
}

async function refresh() {
  SUM = await invoke("summary", { preview: null });
  PREV = null;
  ui.previewKey = "";
  renderTop();
  renderRail();
  await renderView();
}

let previewSeq = 0;
async function setPreview(p) {
  const key = p ? JSON.stringify(p) : "";
  if (key === ui.previewKey) return;
  ui.previewKey = key;
  const seq = ++previewSeq;
  const r = p ? await invoke("summary", { preview: p }) : null;
  if (seq !== previewSeq) return;
  PREV = r && r.preview ? r : null;
  renderRail();
  if (ui.tab === "tree") drawTree();
}

// A message in the on-screen toast (a live region, so screen readers read it). Errors are announced at once and stay
// until clicked or replaced; other messages fade after 6 s.
function flash(msg, bad) {
  const s = $("#status");
  s.textContent = msg;
  s.hidden = !msg;
  s.className = "toast " + (bad ? "bad" : "good");
  s.setAttribute("role", bad ? "alert" : "status");
  s.setAttribute("aria-live", bad ? "assertive" : "polite");
  s.title = bad ? "Click to dismiss" : "";
  clearTimeout(flash.t);
  if (!bad) flash.t = setTimeout(() => { s.textContent = ""; s.hidden = true; }, 6000);
}
$("#status").addEventListener("click", () => { const s = $("#status"); s.textContent = ""; s.hidden = true; });

function delta(now, then, higherBetter, d = 1) {
  if (now == null || then == null || !isFinite(now) || !isFinite(then)) return "";
  const x = then - now;
  if (Math.abs(x) < Math.pow(10, -d) / 2) return "";
  const good = x > 0 === higherBetter;
  return ` <span class="${good ? "d-good" : "d-bad"}">${x > 0 ? "+" : "−"}${Math.abs(x).toFixed(d)}</span>`;
}

// ------------------------------------------------------------------ top bar, tabs, rail

function renderTop() {
  const t = ST;
  // while typing in a top-bar field, only refresh what depends on it, so the field keeps its cursor
  if (document.activeElement && $("#top").contains(document.activeElement) && document.activeElement.tagName === "INPUT") {
    const pts = $("#top .field b");
    if (pts) { pts.textContent = t.spent; pts.style.color = t.spent > t.have ? "var(--bad)" : "var(--text)"; pts.nextSibling.textContent = ` / ${t.have}`; }
    $("#undo").disabled = !t.canUndo;
    $("#redo").disabled = !t.canRedo;
    return;
  }
  const chips = t.loadouts.map((l, k) => `<button class="chip ${k === t.current ? "on" : ""}" data-loadout="${k}" data-tip="Switch loadout (each keeps its own tree, gear and level)">${esc(l.name)} · L${l.level}</button>`).join("");
  $("#top").innerHTML = `
    <div class="brand" title="Unshrouded — build planner and DPS meter for Enshrouded">UNSHROUDED</div><div class="vsep"></div>
    <button class="btn" id="openbuilds" data-tip="Your saved builds: open, copy, delete or start a new one (Ctrl+O). Every change is saved as you go.">Builds</button>
    <label class="buildname"><small>BUILD</small><input id="bname" value="${esc(t.buildName)}" spellcheck="false"></label>
    <div class="chips">${chips}<button class="chip" id="newload" data-tip="New loadout (a copy of this one)">+</button>
      <input type="text" id="lname" value="${esc(t.loadoutName)}" style="width:96px" data-tip="Rename this loadout" spellcheck="false">
      ${t.loadouts.length > 1 ? `<button class="chip" id="delload" data-tip="Delete this loadout (click twice)">×</button>` : ""}</div>
    <div class="spacer"></div>
    <label class="field">Level <input type="number" id="level" min="1" max="${CAT.caps.level}" value="${t.level}"></label>
    <label class="field">Flame <input type="number" id="flame" min="1" max="${CAT.caps.flame}" value="${t.flame}"></label>
    <span class="field" data-tip="${t.foundPoints != null ? `2 per level after the first, plus ${t.foundPoints} from the Shroud Roots and Elixir Wells this character has found (from the save).` : "2 per level after the first, plus the Shroud Roots and Elixir Wells points spread evenly over the levels. Load from game, or enter the points from what your character has found, to use your real total."}">Points <b class="num" style="color:${t.spent > t.have ? "var(--bad)" : "var(--text)"}">${t.spent}</b> / ${t.foundPoints != null ? t.have : `~${t.have} <span class="small muted">(estimate)</span>`}</span>
    <label class="field" data-tip="Skill points from the Shroud Roots and Elixir Wells this character has found (up to 114). Leave empty to estimate them from your level.">Roots &amp; Wells <input type="number" id="found" min="0" max="114" placeholder="?" style="width:56px" value="${t.foundPoints ?? ""}"></label>
    <button class="btn" id="openext" data-tip="Extensions: the live DPS meter in the game, with the combat logger started for you">Extensions</button>
    <button class="btn" id="openmeter" data-tip="A small damage meter that stays on top of the game (play in borderless windowed mode) and reads the combat logger live">Live meter</button>
    <button class="btn" id="loadgame" data-tip="Fill level, skill tree, gear and weapons from your newest Enshrouded save (the game saves every few seconds while you play). Gems and food are not read. Ctrl+Z undoes it.">Load from game</button>
    <button class="btn icon" id="undo" ${t.canUndo ? "" : "disabled"} data-tip="Undo (Ctrl+Z)">↶</button>
    <button class="btn icon" id="redo" ${t.canRedo ? "" : "disabled"} data-tip="Redo (Ctrl+Y)">↷</button>
    <button class="btn primary" id="share" data-tip="Copy this loadout's build code">Share build</button>`;
}

// ---- saved builds: the backend writes the open build after every change; this dialog switches between them

const ago2 = (t) => { const s = Math.max(0, Math.round(Date.now() / 1000 - t)); return s < 90 ? "just now" : s < 5400 ? `${Math.round(s / 60)} min ago` : s < 129600 ? `${Math.round(s / 3600)} h ago` : `${Math.round(s / 86400)} days ago`; };

async function openBuilds() {
  const r = await invoke("builds");
  const rows = r.builds.map((x) => `<tr class="${x.id === r.current ? "cur" : ""}">
      <td><div class="nm">${esc(x.name)}${x.id === r.current ? ' <span class="small muted">(open)</span>' : ""}</div>
        <div class="meta">${esc([x.character ? "from " + x.character : "", "level " + x.level, x.loadouts + (x.loadouts === 1 ? " loadout" : " loadouts"), "edited " + ago2(x.edited)].filter(Boolean).join(" · "))}</div></td>
      <td class="acts">${x.id === r.current ? "" : `<button class="btn" data-bop="open" data-id="${esc(x.id)}">Open</button>`}
        <button class="btn" data-bop="duplicate" data-id="${esc(x.id)}" data-tip="Open a copy to try changes on">Copy</button>
        <button class="btn" data-bop="delete" data-id="${esc(x.id)}">Delete</button></td></tr>`).join("");
  const d = $("#dlg");
  openDlg();
  d.innerHTML = `<div class="dlg" role="dialog" aria-modal="true" aria-label="Saved builds"><div class="head"><h2>Builds</h2>
      <div style="display:flex;gap:8px"><button class="btn primary" data-bop="new">New build</button><button class="btn icon" id="dlgclose" aria-label="Close">×</button></div></div>
    ${r.saving ? "" : '<div class="muted" style="margin-bottom:8px">Builds are not being saved on this computer (no app data folder).</div>'}
    ${r.builds.length ? `<table class="blist"><tbody>${rows}</tbody></table>` : '<div class="muted">No saved builds yet.</div>'}</div>`;
  d.hidden = false;
  if (!d.contains(document.activeElement)) $("#dlgclose").focus();
}

// Extensions: the planner as the meter's loader (docs/plans/planner-loader.md). The panel polls the loader once a second
// while it is open.
let EXT_TIMER = null;
// loader::PLACES, in order
const EXT_PLACES = ["Left, below the health bars", "Top right, below the quest", "Bottom right", "Bottom left"];
function extRows(x) {
  const game = !x.game ? "Not running" : x.in_world ? "In a world" : x.state ? "Main menu" : "Starting";
  const logger = x.logger && x.joined ? `Recording${x.log ? " to " + x.log : ""} (someone else's world: their server's hits, without the damage-type split)`
    : x.logger ? `Recording${x.log ? " to " + x.log : ""}`
    : !x.meter ? "Off"
    : x.logger_error ? `Could not start: ${x.logger_error}`
    : x.game ? "Starting when the game reaches its menu" : "Starts with the game";
  const meter = !x.meter ? "Off"
    : x.hidden ? "Hidden: press F9 in the game (or untick Hidden) to show it"
    : x.showing && x.overlay_loaded ? "Showing in the game"
    : x.restart_needed ? "Restart Enshrouded once: the meter is loaded when the game starts"
    : x.game ? "Hidden until you enter a world" : "Shows when you enter a world";
  const picture = !x.showing ? null : x.picture_age == null ? "Not drawn yet: the meter page has not been captured"
    : x.picture_age < 3 ? "Live" : `Stale for ${x.picture_age.toFixed(0)} s: showing a simpler drawn meter until the page draws again`;
  return [["Game", game], ["Combat log", logger], ["In-game meter", meter], ...(picture ? [["In-game picture", picture]] : [])]
    .map(([k, v]) => `<tr><td class="meta">${k}</td><td class="nm" style="font-weight:600">${esc(v)}</td></tr>`).join("")
    + (x.error ? `<tr><td class="meta">Problem</td><td style="color:var(--bad)">${esc(x.error)}</td></tr>` : "");
}
async function openExtensions() {
  let x;
  try { x = await invoke("extensions_state"); } catch (e) { flash(String(e), true); return; }
  const d = $("#dlg");
  openDlg();
  d.innerHTML = `<div class="dlg" role="dialog" aria-modal="true" aria-labelledby="exth"><div class="head"><h2 id="exth">Extensions</h2>
      <button class="btn icon" id="dlgclose" aria-label="Close">×</button></div>
    <label class="field" style="gap:10px;color:var(--text);font-weight:700"><input type="checkbox" id="extmeter" role="switch" ${x.meter ? "checked" : ""} style="width:18px;height:18px">Live DPS meter</label>
    <div class="muted" style="margin:6px 0 12px 28px;max-width:600px">While the planner is open, it starts the combat logger in Enshrouded and draws the damage meter into the game's
      picture whenever you are in a world, so it shows on Discord and OBS game capture too. Nothing is drawn on the menus.</div>
    <div style="display:flex;gap:18px;flex-wrap:wrap;align-items:center;margin:0 0 12px 28px">
      <label class="field">Position <select id="extplace">${x.place === -1 ? '<option value="-1" selected>Where you dragged it</option>' : ""}${EXT_PLACES.map((n, i) => `<option value="${i}" ${i === x.place ? "selected" : ""}>${n}</option>`).join("")}</select></label>
      <label class="field"><input type="checkbox" id="exthidden" ${x.hidden ? "checked" : ""}> Hidden</label>
      ${x.size ? `<span class="field">Size ${Math.round(x.size[0])} × ${Math.round(x.size[1])} <button class="btn" id="extsize">Reset size</button></span>` : ""}
    </div>
    <div class="muted" style="margin:0 0 12px 28px;max-width:600px">In the game: <b>F6</b> next tab · <b>F7</b> more or less see-through · <b>F8</b> move ·
      <b>F9</b> hide or show. When the game shows its cursor (inventory, map): drag the meter's title bar to move it, drag its
      bottom-right corner to resize it, click its tabs, and scroll its lists with the wheel.</div>
    <table class="blist"><tbody id="extrows">${extRows(x)}</tbody></table></div>`;
  d.hidden = false;
  if (!d.contains(document.activeElement)) $("#extmeter").focus();
  clearInterval(EXT_TIMER);
  EXT_TIMER = setInterval(async () => {
    const rows = $("#extrows");
    if (!rows) { clearInterval(EXT_TIMER); return; }
    try { rows.innerHTML = extRows(await invoke("extensions_state")); } catch (_) {}
  }, 1000);
}
$("#dlg").addEventListener("click", async (e) => {
  if (e.target.id !== "extsize") return;
  try { await invoke("extensions_layout", { place: Number($("#extplace").value), hidden: $("#exthidden").checked, resetSize: true }); e.target.closest(".field").remove(); }
  catch (err) { flash(String(err), true); }
});
$("#dlg").addEventListener("change", async (e) => {
  if (e.target.id === "extplace" || e.target.id === "exthidden") {
    try { await invoke("extensions_layout", { place: Number($("#extplace").value), hidden: $("#exthidden").checked, resetSize: false }); }
    catch (err) { flash(String(err), true); }
    return;
  }
  if (e.target.id !== "extmeter") return;
  const on = e.target.checked;
  try { await invoke("extensions_set", { meter: on }); flash(on ? "Live DPS meter on." : "Live DPS meter off."); }
  catch (err) { e.target.checked = !on; flash(String(err), true); }
});

// a modal dialog: the page behind is inert, Tab stays inside, and closing returns focus to the control that opened it
let DLG_OPENER = null;
function openDlg() {
  if ($("#dlg").hidden) DLG_OPENER = document.activeElement && document.activeElement.id;
  $("#app").inert = true;
}
function closeBuilds() {
  $("#dlg").hidden = true; $("#dlg").innerHTML = ""; $("#app").inert = false;
  const o = DLG_OPENER && document.getElementById(DLG_OPENER);
  DLG_OPENER = null;
  if (o) o.focus();
}
$("#dlg").addEventListener("keydown", (e) => {
  if (e.key !== "Tab") return;
  const f = [...$("#dlg").querySelectorAll("button:not([disabled]), input, select, textarea, [tabindex]:not([tabindex='-1'])")].filter((x) => x.offsetParent !== null);
  if (!f.length) return;
  const first = f[0], last = f[f.length - 1];
  if (e.shiftKey && (document.activeElement === first || !$("#dlg").contains(document.activeElement))) { e.preventDefault(); last.focus(); }
  else if (!e.shiftKey && document.activeElement === last) { e.preventDefault(); first.focus(); }
});

$("#dlg").addEventListener("click", async (e) => {
  if (e.target.id === "dlg" || e.target.id === "dlgclose") return closeBuilds();
  const b = e.target.closest("[data-bop]");
  if (!b) return;
  const op = b.dataset.bop;
  // deleting asks for a second click instead of a browser confirm box
  if (op === "delete" && b.dataset.armed !== "1") {
    b.dataset.armed = "1"; b.textContent = "Delete for good?"; b.style.color = "var(--bad)";
    setTimeout(() => { if (b.isConnected) { b.dataset.armed = ""; b.textContent = "Delete"; b.style.color = ""; } }, 3000);
    return;
  }
  try { await invoke("build_file", { op, id: b.dataset.id || null }); } catch (err) { flash(String(err), true); return; }
  ST = await invoke("state");
  ui.cand = null; ui.candKey = ""; ui.heatData = null; ui.heatKey = ""; ui.optRes = null; ui.msRes = null; OPTCAT = null;
  await refresh();
  if (op === "delete") { await openBuilds(); flash("Build deleted."); }
  else { closeBuilds(); flash(ST.notice || (op === "new" ? "New build started." : op === "duplicate" ? "Opened a copy." : `Opened ${ST.buildName}.`), !!ST.notice); }
});

// ---- loading from the game: the backend reads the newest character save and fills level and tree

// the save folder the player chose, if the Steam folder had none
function savedDir() { try { return localStorage.getItem("saveDir") || ""; } catch (_) { return ""; } }

// asks for the folder that holds characterN.ksd, then loads from it
function askSaveDir(dirs) {
  const d = $("#dlg");
  openDlg();
  d.innerHTML = `<form class="dlg" id="sdform" role="dialog" aria-modal="true" aria-labelledby="sdh"><h2 id="sdh">Where are your Enshrouded saves?</h2>
    <p class="small" style="color:var(--text2)">No characterN.ksd was found in ${esc((dirs || []).join(", ") || "the Steam folder")}. Paste the folder that holds your character files (in Steam: userdata, your id, 1203620, remote).</p>
    <label class="field" style="display:flex;gap:8px"><span>Save folder</span><input type="text" id="savedir" style="flex:1" value="${esc(savedDir())}"></label>
    <div style="display:flex;gap:8px;margin-top:12px;justify-content:flex-end"><button type="button" class="btn outline" id="sdcancel">Cancel</button><button type="submit" class="btn" id="sdok">Load from this folder</button></div></form>`;
  d.hidden = false;
  $("#savedir").focus();
  $("#sdcancel").onclick = () => closeBuilds();
  // Enter in the field submits the form
  $("#sdform").onsubmit = (e) => { e.preventDefault(); try { localStorage.setItem("saveDir", $("#savedir").value.trim()); } catch (_) {} closeBuilds(); loadGame(); };
}

async function loadGame() {
  let g;
  try { g = await invoke("load_game", { dir: savedDir() || null }); } catch (e) { flash(String(e), true); return; }
  if (!g.found) { askSaveDir(g.dirs); return; }
  if (g.error) { flash(g.error, true); return; }
  ST = await invoke("state");
  const when = g.age_s < 90 ? `${g.age_s}s ago` : g.age_s < 5400 ? `${Math.round(g.age_s / 60)} min ago` : `${Math.round(g.age_s / 3600)} h ago`;
  const gear = g.itemsError ? ` Gear not read: ${g.itemsError}.` : ` ${g.gear} gear pieces, ${g.weapons} weapons${g.weapon ? `, ${g.weapon} in hand` : ""}.`;
  flash(`${g.updated ? "Loaded" : "Already matches"} ${g.name}: level ${g.level}, ${g.points} skill points${g.flame != null ? `, Flame ${g.flame} (${g.world})` : ""} (saved ${when}).${gear}`, !!g.itemsError);
  await refresh();
}

$("#top").addEventListener("click", async (e) => {
  const b = e.target.closest("button");
  if (!b) return;
  if (b.id === "loadgame") { await loadGame(); return; }
  if (b.id === "openext") { await openExtensions(); return; }
  if (b.id === "openmeter") { try { await invoke("meter_open"); } catch (e) { flash(String(e), true); } return; }
  if (b.id === "openbuilds") { await openBuilds(); return; }
  if (b.dataset.loadout != null) act({ type: "switch_loadout", index: +b.dataset.loadout });
  else if (b.id === "newload") act({ type: "new_loadout" });
  else if (b.id === "delload") {
    // deleting asks for a second click
    if (b.dataset.armed !== "1") {
      b.dataset.armed = "1"; b.textContent = "Delete?"; b.style.color = "var(--bad)";
      setTimeout(() => { if (b.isConnected) { b.dataset.armed = ""; b.textContent = "×"; b.style.color = ""; } }, 3000);
      return;
    }
    act({ type: "delete_loadout", index: ST.current }).then(() => flash("Loadout deleted."));
  }
  else if (b.id === "undo") act({ type: "undo" });
  else if (b.id === "redo") act({ type: "redo" });
  else if (b.id === "share") {
    const code = await invoke("code");
    await navigator.clipboard.writeText(code);
    flash("Build code copied to the clipboard.");
  }
});
const numberAct = debounce((type, v) => act({ type, value: v == null ? null : +v }), 250);
$("#top").addEventListener("input", (e) => {
  if (e.target.id === "level") numberAct("level", e.target.value);
  if (e.target.id === "flame") numberAct("flame", e.target.value);
  if (e.target.id === "found") numberAct("found_points", e.target.value === "" ? null : e.target.value);
});
$("#top").addEventListener("change", (e) => {
  if (e.target.id === "bname") act({ type: "build_name", value: e.target.value });
  if (e.target.id === "lname") act({ type: "loadout_name", value: e.target.value });
});

function renderTabs() {
  $("#tabs").innerHTML = TABS.map(([id, n]) => `<button data-tab="${id}" class="${ui.tab === id ? "on" : ""}">${n.toUpperCase()}</button>`).join("");
}
$("#tabs").addEventListener("click", (e) => {
  const b = e.target.closest("button[data-tab]");
  if (b) goTab(b.dataset.tab);
});
function goTab(t) {
  ui.tab = t;
  setPreview(null);
  renderTabs();
  renderRail();
  renderView();
}

// What each optimizer goal scores.
const GOAL_TIPS = {
  Damage: "Total DPS in the fight set up in Combat setup.",
  Survival: "How many of the enemy's biggest hits you survive.",
  Movement: "Sprint speed, stamina for sprinting and gliding, and other ways of getting around.",
  Sustain: "Health, mana and stamina you get back during a fight.",
  Utility: "Time saved outside combat (gathering, crafting, travel), estimated per hour of play.",
};

// What each number on the rail means, for a player meeting it for the first time.
const RAIL_TIPS = {
  "Total DPS": "Damage per second over one fight (its length is set in Combat setup): your attacks (as long as mana lasts), summons, and skills and gems that deal their own damage.",
  "Expected hit": "Average damage of one regular hit after the enemy's armour, with crits counted at their chance.",
  "Time to kill": "Seconds to kill one enemy of the type and level set in Combat setup at this DPS.",
  "Crit chance": "Share of your hits that are critical hits.",
  "Hits survived": "How many of the enemy's biggest hits your full health takes, after your armour and resistances. Below 1, one hit kills you.",
  "Mana uptime": "Share of the fight your mana-costing attack keeps going. Out of mana, a bow uses its best arrow that costs no mana.",
  "Health": "Your maximum health.",
};

function renderRail() {
  const n = SUM.now;
  const p = PREV && PREV.preview;
  const line = (label, value, cls, d) => `<div class="kv"><span class="k" data-tip="${esc(RAIL_TIPS[label] || "")}">${label}</span><span class="v num ${cls || ""}">${value}${d || ""}</span></div>`;
  const sc = n.hits_survived < 1 ? "c-bad" : n.hits_survived < 2 ? "c-warn" : "";
  let html = `<div class="eyebrow" data-tip="${esc(RAIL_TIPS["Total DPS"])}">Total DPS · ${fx(ST.fight.fight_s || 60, 0)} s fight</div>
    <div><span class="bignum num">${fx(n.dps)}</span>${p ? delta(n.dps, p.dps, true) : ""}</div>
    <div class="preview-note">${PREV ? "Previewing: " + esc(PREV.label) : '<span class="muted">Hover a node or item to preview it</span>'}</div>
    <div style="margin-top:10px">
    ${line("Expected hit", fx(n.expected_hit), "", p && delta(n.expected_hit, p.expected_hit, true))}
    ${line("Time to kill", n.ttk == null ? "—" : fx(n.ttk) + " s", "", p && n.ttk != null && p.ttk != null ? delta(n.ttk, p.ttk, false) : "")}
    ${line("Crit chance", pct(n.crit, 1), "", p && delta(n.crit * 100, p.crit * 100, true))}
    ${line("Hits survived", fx(n.hits_survived), sc, p && delta(n.hits_survived, p.hits_survived, true))}
    ${n.has_mana_cost ? line("Mana uptime", pct(n.mana_uptime), n.mana_uptime < 0.6 ? "c-warn" : "", p && delta(n.mana_uptime * 100, p.mana_uptime * 100, true, 0)) : ""}
    ${line("Health", fx(n.health, 0), "", p && delta(n.health, p.health, true, 0))}
    </div>`;
  if (ui.tab === "tree") {
    html += `<div style="margin-top:18px" class="eyebrow">Points by region</div>` + ST.regions.map((r) =>
      `<div class="kv" style="border:0;padding:4px 0"><span class="k" style="display:flex;align-items:center;gap:8px;color:${r.points ? "var(--text)" : "var(--text3)"}"><span style="width:10px;height:10px;border-radius:5px;background:${CLASS_COLOR[r.name]}"></span>${r.name}</span><span class="v num" style="color:${r.points ? "var(--text)" : "var(--text3)"}">${r.points}</span></div>`).join("");
  }
  if (ui.tab !== "setup") {
    const f = ST.fight;
    html += `<div class="card raised" style="margin-top:18px;padding:14px">
      <div class="eyebrow">Fighting</div>
      <div style="font-weight:700;margin:4px 0 2px">${esc(ST.enemyLabel)} · L${f.enemy_level}</div>
      <div class="small" style="color:var(--text2)">${f.pack_size > 1 ? `${fx(f.pack_size, 0)} enemies within ${fx(f.pack_radius)} m` : "Single target"}${ST.weaponKind && !ST.weaponKind.startsWith("Ranged") ? ` · ${ST.heavy ? "heavy attacks" : "light combo"}` : ""}${f.hit_kind !== "Normal" ? ` · ${esc(f.hit_kind.toLowerCase())} ${Math.round((f.hit_share ?? 1) * 100)}%` : ""}</div>
      ${(ST.fightChanges || []).length ? `<div class="small" style="color:var(--warn);margin-top:2px">Also: ${ST.fightChanges.map(esc).join(" · ")}</div>` : ""}
      <button class="link" data-go="setup" style="margin-top:6px">Change in Combat setup</button></div>`;
  }
  $("#rail").innerHTML = html;
  // the headline number is announced when it changes (not while previewing a hover)
  const said = `Total DPS ${fx(n.dps)}`;
  let live = $("#dpslive");
  if (!live) {
    live = Object.assign(document.createElement("div"), { id: "dpslive", className: "sr-only" });
    live.setAttribute("aria-live", "polite"); live.setAttribute("aria-atomic", "true");
    document.body.append(live);
  }
  if (live.dataset.said !== said) { live.dataset.said = said; live.textContent = said; }
}
$("#rail").addEventListener("click", (e) => {
  const b = e.target.closest("[data-go]");
  if (b) goTab(b.dataset.go);
});

// ------------------------------------------------------------------ views

async function renderView() {
  const v = $("#view");
  // typing in a setting or note: keep that field (its value is already what the backend has)
  const ae = document.activeElement;
  if (ae && v.contains(ae) && ae.matches("input[type=number], input[type=text], textarea") && (ui.tab === "setup" || ui.tab === "notes")) return;
  v.className = ui.tab === "tree" ? "flush" : "";
  // the tree view stays put (canvas, hover, pan) and only redraws after a change
  if (ui.tab === "tree" && $("#tree")) {
    renderNodePanel();
    loadHeat();
    loadSteps();
    drawTree();
    return;
  }
  switch (ui.tab) {
    case "overview": return renderOverview(v);
    case "tree": return renderTree(v);
    case "gear": return renderGear(v);
    case "setup": return renderSetup(v);
    case "calcs": return renderCalcs(v);
    case "optimizer": return renderOptimizer(v);
    case "notes": return renderNotes(v);
  }
}

// ---- overview

// Presets: open while no weapon is equipped, folded away under "Start over" once one is.
function presetCard(o) {
  if (!o.presets) return "";
  const body = `<div class="small" style="color:var(--text2);margin-bottom:8px">Builds a starting point for a level-${ST.level} character (set your level in the top bar first): the strongest weapon of that kind with its arrows or spell, a skill tree, armour (Rare where rarity rolls) and three foods. Takes a few seconds and replaces this loadout.</div>
      <div style="display:flex;flex-wrap:wrap;gap:6px;margin-bottom:16px">${o.presets.map((x) => `<button class="btn outline" data-do="preset:${esc(x.kind)}">${esc(x.label)}</button>`).join("")}</div>`;
  return o.hasWeapon ? `<details style="margin-bottom:12px"><summary class="small muted">Start over from a preset</summary>${body}</details>` : `<h2 style="margin-bottom:8px">Start from a weapon</h2>${body}`;
}

// every loadout side by side under the current fight, so two builds compare without switching back and forth
function loadoutTable(ls) {
  if (!ls || ls.length < 2) return "";
  const best = Math.max(...ls.map((l) => l.dps));
  return `<section class="card" style="margin-top:16px"><h2>Loadouts side by side</h2><table class="ctable" style="margin-top:8px"><thead><tr><th>Loadout</th><th>Weapon</th><th class="r">Level</th><th class="r">Total DPS</th><th class="r">vs best</th><th class="r">Expected hit</th><th class="r">Crit</th><th class="r">Health</th><th class="r">Hits survived</th></tr></thead><tbody>
    ${ls.map((l) => `<tr${l.current ? ' class="cur"' : ""}><td>${esc(l.name)}${l.current ? ' <span class="small muted">(open)</span>' : ""}</td><td>${esc(l.weapon)}</td><td class="r num">${l.level}</td><td class="r num"><b>${fx(l.dps)}</b></td><td class="r num">${best > 0 ? (l.dps >= best ? (ls.some((o) => o !== l && (best - o.dps) / best < 0.01) ? "≈ best" : "best") : (best - l.dps) / best < 0.01 ? "≈ best" : "−" + (((best - l.dps) / best) * 100).toFixed(0) + "%") : "—"}</td><td class="r num">${fx(l.hit)}</td><td class="r num">${pct(l.crit, 1)}</td><td class="r num">${fx(l.health, 0)}</td><td class="r num">${fx(l.survived)}</td></tr>`).join("")}</tbody></table></section>`;
}

async function renderOverview(v) {
  const o = await invoke("overview");
  if (ui.tab !== "overview") return;
  const s = o.summary;
  const total = s.sources.reduce((a, x) => a + Math.max(0, x.dps), 0);
  const bar = total > 0 ? s.sources.map((x, k) => `<div style="width:${(Math.max(0, x.dps) / total) * 100}%;background:${SOURCE_COLORS[Math.min(k, 5)]}"></div>`).join("") : "";
  const srcs = s.sources.length ? s.sources.map((x, k) => `<div class="src"><span class="dot" style="background:${SOURCE_COLORS[Math.min(k, 5)]}"></span>
      <div><div class="n">${esc(x.name)}</div><div class="note">${esc(x.note)}</div></div>
      <span class="num" style="text-align:right;font-weight:700;font-size:16px">${fx(x.dps)}</span>
      <span class="num" style="text-align:right;color:var(--text2)">${total > 0 ? ((x.dps / total) * 100).toFixed(0) + "%" : "—"}</span></div>`).join("")
    : `<div class="muted">No damage yet: equip a weapon or a summon.</div>`;
  const chain = o.chain
    ? `<div class="chain">${o.chain.map((b) => `${b.op ? `<span class="op">${b.op}</span>` : ""}<div class="tile ${b.strong ? "strong" : ""}" ${b.tip ? `data-tip="${esc(b.tip)}"` : ""}><div class="l">${esc(b.label)}</div><div class="v num">${esc(b.value)}</div></div>`).join("")}</div>
       <div style="margin-top:10px;color:var(--text2)">${fx(o.rate.projectiles, 2)} projectiles per attack · one attack every ${fx(o.rate.secs, 2)} s (${esc(o.rate.how)}) · <b style="color:var(--text)">${fx(o.rate.dps)} DPS while attacking</b></div>`
    : `<div class="muted">Pick a weapon on the Gear tab to see how a hit is built.</div>`;
  const sc = s.hits_survived < 1 ? "c-bad" : s.hits_survived < 2 ? "c-warn" : "c-good";
  const tile = (l, val, c) => `<div class="tile"><div class="l">${l}</div><div class="v num ${c || ""}">${val}</div></div>`;
  const ins = o.insights.length ? o.insights.map((i, k) => `<div class="insight ${i.warn ? "warn" : ""}"><span class="badge">${k + 1}</span><div>
      <div class="t">${esc(i.title)}</div><div class="b">${esc(i.body)}</div>
      ${i.action ? `<button class="btn outline" data-do="${esc(i.action[1])}">${esc(i.action[0])}</button>` : ""}</div></div>`).join("")
    : `<div class="muted">Nothing obvious. Try the skill-tree heatmap or the optimizer to squeeze out more.</div>`;
  v.innerHTML = `<div class="grid2">
    <div class="stack">
      <section class="card"><div class="card-head"><h2>Where your damage comes from</h2><span class="num" style="color:var(--text2)">${fx(s.dps)} DPS</span></div><div class="bar">${bar}</div>${srcs}</section>
      <section class="card"><div class="card-head"><h2>How one hit is built</h2><button class="link" data-go="calcs">Full breakdown</button></div>${chain}</section>
      <section class="card"><div class="card-head"><h2>Survival</h2><span style="color:var(--text2)">against ${esc(o.incoming)} hits</span></div>
        <div class="tiles3">${tile("Health", fx(s.health, 0))}${tile("Physical armour", fx(s.armor_phys, 0))}${tile("Magical armour", fx(s.armor_mag, 0))}${tile("Biggest hit taken", fx(s.hit_taken, 0))}${tile("Hits survived", fx(s.hits_survived), sc)}${tile("Health regen", fx(s.health_regen) + " /s")}</div>
        ${s.hit_raw > s.hit_taken + 0.5 ? `<div class="small" style="margin-top:8px;color:var(--text2)">Armour and resistances cut that hit from ${fx(s.hit_raw, 0)} to ${fx(s.hit_taken, 0)}.</div>` : ""}</section>
    </div>
    <section class="card">${presetCard(o)}<h2 style="margin-bottom:12px">What's holding you back</h2>${ins}</section>
  </div>${loadoutTable(o.loadouts)}`;
}

$("#view").addEventListener("click", (e) => {
  const go = e.target.closest("[data-go]");
  if (go) return goTab(go.dataset.go);
  const d = e.target.closest("[data-do]");
  if (!d) return;
  const [kind, arg] = d.dataset.do.split(/:(.*)/s);
  if (kind === "tab") goTab(arg);
  else if (kind === "slot") { ui.slot = arg; goTab("gear"); }
  else if (kind === "ammo") act({ type: "equip", slot: "ammo", guid: arg });
  else if (kind === "preset") startPreset(arg);
});

// ---- skill tree

let treeGeom = null;
function renderTree(v) {
  v.innerHTML = `<div class="tree-wrap">
    <div class="tree-stage" id="stage">
      <div class="tree-bar">
        <input type="search" id="tsearch" placeholder="Search skills (Ctrl+F)" value="${esc(ui.search)}">
        <span id="found" style="color:var(--teal)"></span>
        <div class="seg"><button data-heat="0" class="${ui.heat ? "" : "on"}" aria-pressed="${!ui.heat}">Region colours</button><button data-heat="1" class="${ui.heat ? "on" : ""}" aria-pressed="${!!ui.heat}">DPS per point</button></div>
        <span id="heatspin"></span>
        <div class="spacer"></div><button class="btn" id="resettree">Reset tree</button>
      </div>
      <canvas id="tree"></canvas>
      <div class="stepper" id="stepper" hidden></div>
      <div class="tree-hint">Click: add a rank (and the path to it) · Right-click: remove · Drag: pan · Scroll: zoom</div>
    </div>
    <div class="node-panel" id="npanel"></div></div>`;
  const c = $("#tree");
  const fit = () => {
    const r = c.getBoundingClientRect();
    const dpr = window.devicePixelRatio || 1;
    c.width = Math.max(1, r.width * dpr);
    c.height = Math.max(1, r.height * dpr);
    drawTree();
  };
  new ResizeObserver(fit).observe(c);
  let drag = null;
  c.addEventListener("mousedown", (e) => { if (e.button === 0) drag = { x: e.clientX, y: e.clientY, moved: false }; });
  window.addEventListener("mouseup", () => { if (drag) c.classList.remove("drag"); });
  c.addEventListener("mousemove", (e) => {
    if (drag && e.buttons & 1) {
      const dx = e.clientX - drag.x, dy = e.clientY - drag.y;
      if (Math.abs(dx) + Math.abs(dy) > 3 || drag.moved) {
        drag.moved = true;
        c.classList.add("drag");
        ui.pan.x += dx / ui.zoom;
        ui.pan.y += dy / ui.zoom;
        drag.x = e.clientX;
        drag.y = e.clientY;
        drawTree();
        return;
      }
    }
    const h = hitNode(e);
    if (h !== ui.hover) {
      ui.hover = h;
      renderNodePanel();
      setPreview(h == null ? null : { node: h });
      drawTree();
    }
  });
  c.addEventListener("mouseleave", () => { ui.hover = null; setPreview(null); renderNodePanel(); drawTree(); });
  c.addEventListener("click", () => {
    const moved = drag && drag.moved;
    drag = null;
    if (moved || ui.hover == null) return;
    ui.selected = ui.hover;
    act({ type: "allocate", node: ui.hover });
  });
  c.addEventListener("contextmenu", (e) => e.preventDefault());
  // right button removes a rank (on the press itself: WebView2 may swallow the context-menu event)
  c.addEventListener("mousedown", (e) => {
    if (e.button !== 2) return;
    const h = hitNode(e);
    if (h != null && ST.removable[h]) {
      ui.selected = h;
      act({ type: "remove", node: h });
    } else if (h != null) {
      flash(`${CAT.skills[h].name} can't lose a rank: other allocated skills connect through it.`, true);
    }
  });
  c.addEventListener("wheel", (e) => {
    e.preventDefault();
    const z = Math.min(0.8, Math.max(0.03, ui.zoom * (1 - e.deltaY * 0.0012)));
    ui.zoom = z;
    drawTree();
  }, { passive: false });
  $("#tsearch").addEventListener("input", (e) => { ui.search = e.target.value; drawTree(); });
  // keyboard: Enter selects the next skill that matches the search; the node panel then offers its rank buttons
  $("#tsearch").addEventListener("keydown", (e) => {
    if (e.key !== "Enter") return;
    e.preventDefault();
    const hits = CAT.skills.map((s, i) => (matches(s) ? i : -1)).filter((i) => i >= 0);
    if (!hits.length) return flash("No skill matches that search.", true);
    const at = hits.indexOf(ui.selected);
    ui.selected = hits[(at + 1) % hits.length];
    ui.hover = null;
    renderNodePanel();
    drawTree();
    flash(`${CAT.skills[ui.selected].name} (${hits.indexOf(ui.selected) + 1} of ${hits.length}). Enter for the next one.`);
  });
  v.querySelector(".seg").addEventListener("click", (e) => {
    const b = e.target.closest("button[data-heat]");
    if (!b) return;
    ui.heat = b.dataset.heat === "1";
    v.querySelectorAll(".seg button").forEach((x) => x.classList.toggle("on", x === b));
    loadHeat();
    drawTree();
  });
  $("#resettree").addEventListener("click", () => act({ type: "reset_tree" }));
  $("#stepper").addEventListener("click", (e) => {
    const b = e.target.closest("button[data-step]");
    if (b) stepTo(b.dataset.step);
  });
  $("#stepper").addEventListener("input", (e) => { if (e.target.id === "steprange") stepTo(+e.target.value); });
  loadSteps();
  if (ui.focusSearch) { $("#tsearch").focus(); ui.focusSearch = false; }
  renderNodePanel();
  loadHeat();
  fit();
}

// ---- stepping through the tree: always on. The steps are the order the tree was built in (clicks, the optimizer's
// leveling order), kept with the build; a tree loaded without one gets an order worked out once (best gain per
// point) and kept. The stepper sits at the last step, so the tree looks as built; stepping back shows the tree
// after k points without changing the build.

let stepSeq = 0;
async function loadSteps() {
  const key = JSON.stringify(ST.ranks);
  if (ui.step && ui.step.key === key) { drawStepper(); return; }
  if (!ST.ranks.some((r) => r > 0)) { ui.step = { order: [], levels: [], unordered: 0, k: 0, key }; drawStepper(); return; }
  const seq = ++stepSeq;
  // keep showing the last steps while asking: a stored order comes back at once, so the "working" line shows
  // only when the order has to be worked out (a tree loaded without one)
  setTimeout(() => { if (seq === stepSeq && (!ui.step || ui.step.key !== key)) { ui.step = { order: [], levels: [], unordered: 0, k: 0, key, loading: true }; drawStepper(); } }, 200);
  try {
    const r = await invoke("tree_order");
    if (seq !== stepSeq || JSON.stringify(ST.ranks) !== key) return;
    ui.step = { ...r, k: r.order.length, key };
  } catch (e) {
    if (seq === stepSeq) ui.step = null;
    flash(String(e), true);
  }
  drawStepper(); drawTree();
}

// the tree drawn: the whole build, or the build after k points while stepping back
function stepping() { return !!(ui.step && !ui.step.loading && ui.step.k < ui.step.order.length); }

function stepRanks() {
  const r = new Array(CAT.skills.length).fill(0);
  for (let j = 0; j < ui.step.k; j++) r[ui.step.order[j]]++;
  return r;
}

function stepTo(k) {
  const st = ui.step;
  if (!st || st.loading) return;
  const n = st.order.length;
  if (k === "first") k = 0; else if (k === "last") k = n; else if (k === "prev") k = st.k - 1; else if (k === "next") k = st.k + 1;
  else if (k === "apply") { const r = stepRanks(); act({ type: "set_ranks", ranks: r }); flash("Tree set to that step (Ctrl+Z undoes it)."); return; }
  st.k = Math.max(0, Math.min(n, +k));
  if (st.k > 0) {
    const i = st.order[st.k - 1], s = CAT.skills[i], g = geometry();
    ui.selected = i;
    ui.pan.x = g.cx - s.x; ui.pan.y = g.cy - s.y;
  } else ui.selected = null;
  drawStepper(); drawTree(); renderNodePanel();
}

function drawStepper() {
  const el = $("#stepper");
  if (!el) return;
  const st = ui.step;
  el.hidden = !st;
  if (!st) { el.innerHTML = ""; return; }
  if (st.loading) { el.innerHTML = '<span class="spinner"></span><span class="what">Working out the order to take these skills in…</span>'; return; }
  const n = st.order.length, k = st.k;
  // skill points the tree has at this step: each rank costs its skill's cost
  const spent = st.order.slice(0, k).reduce((t, i) => t + (CAT.skills[i].cost || 0), 0);
  if (!n) { el.innerHTML = '<span class="what">Add skills and this bar steps through them in the order to take them while leveling.</span>'; return; }
  let what = "Start: an empty tree. Press → or ▶ to take the first point.";
  if (k > 0) {
    const i = st.order[k - 1], s = CAT.skills[i];
    const rank = st.order.slice(0, k).filter((x) => x === i).length;
    const lvl = st.levels[k - 1];
    what = `${k === n ? "Last point: " : ""}<b>${esc(s.name)}</b>${s.max > 1 ? ` · rank ${rank}/${s.max}` : ""} · ${lvl != null ? `from level ${lvl}` : "more points than level 50 gives"}${k > n - st.unordered ? ' · <span class="c-warn">not connected to the tree before it</span>' : ""}`;
  }
  el.innerHTML = `<button class="btn icon" data-step="first" aria-label="Back to the start">⏮</button><button class="btn icon" data-step="prev" aria-label="Previous step">◀</button>
    <input type="range" id="steprange" min="0" max="${n}" value="${k}" aria-label="Step">
    <button class="btn icon" data-step="next" aria-label="Next step">▶</button><button class="btn icon" data-step="last" aria-label="The whole tree">⏭</button>
    <span class="num" style="white-space:nowrap">Step ${k} / ${n}</span><span class="num stepspent" style="white-space:nowrap"><b>${spent}</b> points spent</span><span class="what">${what}</span>
    ${k < n ? '<button class="btn" data-step="apply" data-tip="Make the loadout\'s tree this step\'s tree (one undo step)">Set tree to this step</button>' : ""}`;
}

async function loadHeat() {
  if (!ui.heat) return;
  const key = ST.calcKey;
  if (key === ui.heatKey && ui.heatData) return;
  ui.heatKey = key;
  const sp = $("#heatspin");
  if (sp) sp.innerHTML = '<span class="spinner"></span>';
  const data = await invoke("heatmap");
  if (key !== ui.heatKey) return;
  ui.heatData = data;
  if ($("#heatspin")) $("#heatspin").innerHTML = "";
  drawTree();
}

function matches(s) {
  if (ui.search.length < 2) return false;
  const q = ui.search.toLowerCase();
  return s.name.toLowerCase().includes(q) || s.desc.toLowerCase().includes(q) || s.effects.some((e) => e.label.toLowerCase().includes(q) || e.attr.toLowerCase().includes(q));
}

function nodeRadius(s) {
  return Math.max(3, ({ Root: 60, Attribute: 34, ActiveSkill: 70 }[s.kind] || 55) * ui.zoom);
}

function geometry() {
  if (!treeGeom) {
    let cx = 0, cy = 0;
    for (const s of CAT.skills) { cx += s.x; cy += s.y; }
    treeGeom = { cx: cx / CAT.skills.length, cy: cy / CAT.skills.length };
  }
  const c = $("#tree");
  const r = c.getBoundingClientRect();
  return { w: r.width, h: r.height, ...treeGeom };
}

function toScreen(g, s) {
  return [g.w / 2 + (s.x - g.cx + ui.pan.x) * ui.zoom, g.h / 2 + (s.y - g.cy + ui.pan.y) * ui.zoom];
}

function hitNode(e) {
  const c = $("#tree");
  const r = c.getBoundingClientRect();
  const mx = e.clientX - r.left, my = e.clientY - r.top;
  const g = geometry();
  let best = null, bd = 1e9;
  CAT.skills.forEach((s, i) => {
    const [x, y] = toScreen(g, s);
    const d = Math.hypot(mx - x, my - y);
    if (d <= Math.max(6, nodeRadius(s)) && d < bd) { bd = d; best = i; }
  });
  return best;
}

function mix(a, b, t) {
  const p = (h) => [1, 3, 5].map((k) => parseInt(h.slice(k, k + 2), 16));
  const [x, y] = [p(a), p(b)];
  return "rgb(" + x.map((v, k) => Math.round(v + (y[k] - v) * t)).join(",") + ")";
}

function drawTree() {
  const c = $("#tree");
  if (!c || ui.tab !== "tree") return;
  const ctx = c.getContext("2d");
  const dpr = window.devicePixelRatio || 1;
  const g = geometry();
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.fillStyle = "#120D09";
  ctx.fillRect(0, 0, g.w, g.h);
  const sk = CAT.skills, ranks = stepping() ? stepRanks() : ST.ranks;
  const pos = sk.map((s) => toScreen(g, s));
  // region names beyond each region's outermost node
  for (const cl of ["Ranger", "Warrior", "Mage"]) {
    const pts = sk.filter((s) => s.class === cl).map((s) => [s.x - g.cx, s.y - g.cy]);
    if (!pts.length) continue;
    let dx = 0, dy = 0;
    pts.forEach(([x, y]) => { dx += x; dy += y; });
    const len = Math.hypot(dx, dy) || 1;
    dx /= len; dy /= len;
    const reach = Math.max(...pts.map(([x, y]) => x * dx + y * dy));
    const ax = g.w / 2 + (dx * (reach + 260) + ui.pan.x) * ui.zoom, ay = g.h / 2 + (dy * (reach + 260) + ui.pan.y) * ui.zoom;
    ctx.font = `${Math.round(Math.min(30, Math.max(13, 180 * ui.zoom)))}px Cinzel`;
    ctx.fillStyle = CLASS_COLOR[cl];
    ctx.textAlign = "center"; ctx.textBaseline = "middle";
    ctx.globalAlpha = 0.85; ctx.fillText(cl.toUpperCase(), ax, ay); ctx.globalAlpha = 1;
  }
  const path = new Set(PREV && PREV.path ? PREV.path : []);
  const live = (i) => ranks[i] > 0 || sk[i].kind === "Root";
  for (const [i, j] of CAT.edges) {
    const lit = live(i) && live(j) && (ranks[i] > 0 || ranks[j] > 0);
    const prev = (path.has(i) || path.has(j)) && (path.has(i) || live(i)) && (path.has(j) || live(j));
    ctx.beginPath(); ctx.moveTo(...pos[i]); ctx.lineTo(...pos[j]);
    ctx.strokeStyle = lit ? "#E8B04A" : prev ? "#4FC3C8" : "#3A2A1E";
    ctx.lineWidth = lit || prev ? 3 : 1.2;
    ctx.stroke();
  }
  const heat = ui.heat ? ui.heatData : null;
  const hmax = heat ? Math.max(0, ...heat.filter((x) => x != null)) : 0;
  let found = 0;
  sk.forEach((s, i) => {
    const [x, y] = pos[i];
    const r = nodeRadius(s);
    const col = CLASS_COLOR[s.class] || "#D9A441";
    let fill = s.kind === "Root" ? "#563122" : ranks[i] > 0 ? col : "#17110C";
    let stroke = s.kind === "Root" ? "#E8B04A" : ranks[i] > 0 ? "#ECD799" : col;
    let lw = s.kind === "Root" ? 2.5 : 2;
    if (heat && ranks[i] === 0 && heat[i] != null && hmax > 0) fill = mix("#21180F", "#E8B04A", Math.sqrt(Math.max(0, Math.min(1, heat[i] / hmax))));
    if (path.has(i)) { if (ranks[i] === 0) fill = "rgba(79,195,200,.35)"; stroke = "#4FC3C8"; lw = 2.5; }
    // shape: attribute points are diamonds, everything else a circle
    ctx.beginPath();
    if (s.kind === "Attribute") { ctx.moveTo(x, y - r * 1.15); ctx.lineTo(x + r * 1.15, y); ctx.lineTo(x, y + r * 1.15); ctx.lineTo(x - r * 1.15, y); ctx.closePath(); }
    else ctx.arc(x, y, r, 0, Math.PI * 2);
    ctx.fillStyle = fill; ctx.fill();
    ctx.globalAlpha = ranks[i] > 0 || s.kind === "Root" || path.has(i) ? 1 : 0.8;
    ctx.strokeStyle = stroke; ctx.lineWidth = lw; ctx.stroke(); ctx.globalAlpha = 1;
    // picture: the game's icon for active skills (greyed until taken), a class-colour diamond for passives,
    // the stat's first letters for attribute points
    const ic = ICON[i];
    if (ic && r >= 7) {
      const im = ranks[i] > 0 ? ic.on : ic.off;
      if (im.complete && im.naturalWidth) {
        ctx.save(); ctx.beginPath(); ctx.arc(x, y, r - 2, 0, Math.PI * 2); ctx.clip();
        ctx.globalAlpha = ranks[i] > 0 || path.has(i) ? 1 : 0.55;
        ctx.drawImage(im, x - r + 2, y - r + 2, 2 * r - 4, 2 * r - 4);
        ctx.restore();
      }
    } else if (s.kind === "PassiveSkill" && r >= 7) {
      const d = r * 0.38;
      ctx.beginPath(); ctx.moveTo(x, y - d); ctx.lineTo(x + d * 0.85, y); ctx.lineTo(x, y + d); ctx.lineTo(x - d * 0.85, y); ctx.closePath();
      ctx.fillStyle = ranks[i] > 0 ? "#21180F" : col; ctx.globalAlpha = ranks[i] > 0 ? 0.7 : 0.85; ctx.fill(); ctx.globalAlpha = 1;
    } else if (s.kind === "Attribute" && r >= 9) {
      ctx.font = `bold ${Math.round(Math.max(8, r * 0.62))}px 'Alegreya Sans'`; ctx.fillStyle = ranks[i] > 0 ? "#0D0B09" : col;
      ctx.textAlign = "center"; ctx.textBaseline = "middle"; ctx.fillText(s.name.slice(0, 3).toUpperCase(), x, y + 1);
    }
    if (matches(s)) { found++; ctx.beginPath(); ctx.arc(x, y, r + 5, 0, Math.PI * 2); ctx.strokeStyle = "#4FC3C8"; ctx.lineWidth = 2; ctx.stroke(); }
    if (ui.selected === i) { ctx.beginPath(); ctx.arc(x, y, r + 9, 0, Math.PI * 2); ctx.strokeStyle = "#F5ECE8"; ctx.lineWidth = 1.5; ctx.stroke(); }
    if (ui.zoom > 0.08 && s.kind !== "Root" && ranks[i] > 0 && s.max > 1) {
      ctx.font = "bold 11.5px 'Alegreya Sans'"; ctx.textAlign = "center"; ctx.textBaseline = "middle";
      const by = s.kind === "PassiveSkill" || ic ? y - r - 9 : y;
      if (by !== y) { ctx.fillStyle = "#0D0B09"; ctx.fillRect(x - 13, by - 8, 26, 16); ctx.fillStyle = "#ECD799"; } else ctx.fillStyle = "#0D0B09";
      ctx.fillText(`${ranks[i]}/${s.max}`, x, by);
    }
    if (ui.zoom > 0.16 && s.kind !== "Attribute" && s.kind !== "Root") {
      ctx.font = "12.5px 'Alegreya Sans'"; ctx.fillStyle = ranks[i] > 0 ? "#ECD799" : "#B3A98A"; ctx.textAlign = "center"; ctx.textBaseline = "top";
      ctx.fillText(s.name, x, y + r + 8);
    }
  });
  const f = $("#found");
  if (f) f.textContent = ui.search.length >= 2 ? `${found} found` : "";
}

// A node that raises sneak, merciless or backstab damage adds nothing until the Combat setup asks for those hits.
// Shown in every view of the node panel, heatmap or not.
function scenarioNote(s) {
  const kind = ["Sneak", "Merciless", "Backstab"].find((k) => s.effects.some((e) => (e.attr || "").includes(k)));
  if (!kind) return "";
  if (ST.fight.hit_kind !== kind) return `<div class="small muted" style="margin-top:4px">Adds nothing while Your hits is ${esc(ST.fight.hit_kind)}: it raises ${kind.toLowerCase()} damage. Set ${kind} in Combat setup to price it.</div>`;
  if (ST.hitKindProblem) return `<div class="small muted" style="margin-top:4px">Adds nothing yet: ${esc(ST.hitKindProblem)} Your hits are counted as normal hits until then.</div>`;
  return "";
}

function renderNodePanel() {
  const el = $("#npanel");
  if (!el) return;
  const i = ui.hover ?? ui.selected;
  let html = "";
  if (i != null) {
    const s = CAT.skills[i];
    const rank = ST.ranks[i];
    const effects = s.effects.map((e) => `<div class="small" style="margin-top:8px;color:var(--text2)" data-tip="${esc(e.attr)}">${esc(e.label)}</div>
      <div class="rankpills">${e.values.map((v, k) => `<span class="${k + 1 === rank ? "on" : ""}">${esc(v)}</span>`).join("")}</div>
      ${e.conditional ? '<div class="small muted">Only under a condition (see the game\'s tooltip)</div>' : ""}`).join("");
    const heat = ui.heat && ui.heatData && ui.heatData[i] != null ? `<div style="margin-top:8px;color:var(--teal)">${ui.heatData[i] >= 0 ? "+" : ""}${fx(ui.heatData[i], 2)} DPS per point, path included</div>` : "";
    html = `<div class="card raised" style="border-color:var(--flame)">
      <div style="display:flex;gap:12px;align-items:center"><span class="nico">${skillMark(i, 52)}</span>
        <div style="flex:1;min-width:0"><div style="display:flex;justify-content:space-between;align-items:baseline"><h2 style="font-size:19px">${esc(s.name)}</h2><b style="color:${CLASS_COLOR[s.class] || "var(--core)"}">${rank} / ${s.max}</b></div>
      <div class="small" style="color:var(--text2)">${esc(s.class)} · ${esc(s.kindLabel)} · ${s.cost} point${s.cost === 1 ? "" : "s"} per rank</div></div></div>
      ${s.desc ? `<div style="margin-top:8px;line-height:1.4">${esc(s.desc)}</div>` : ""}${effects}
      ${s.coverage ? `<div class="small muted" style="margin-top:8px">${esc(s.coverage.text)}${s.coverage.note ? ` (${esc(s.coverage.note)})` : ""}.</div>` : ""}${scenarioNote(s)}${heat}
      <div style="display:flex;gap:8px;margin-top:10px"><button class="btn outline" data-node-act="allocate" data-node="${i}">Add a rank${rank === 0 ? " (path included)" : ""}</button>${ST.removable[i] ? `<button class="btn outline" data-node-act="remove" data-node="${i}">Remove a rank</button>` : ""}</div></div>`;
  } else {
    html = `<div class="card raised"><span style="color:var(--text2)">Hover a node to see what it does and how it would change your numbers in the left panel.</span></div>`;
  }
  html += `<div class="eyebrow" style="margin-top:16px;margin-bottom:6px">Allocated skills</div>` +
    (ST.allocated.length ? ST.allocated.map((a) => `<div class="alloc">${skillMark(CAT.skills.findIndex((s) => s.name === a.name))}<span style="flex:1">${esc(a.name)}</span><span class="muted">${a.ranks}</span></div>`).join("") : '<div class="muted">None yet.</div>');
  el.innerHTML = html;
}

// ---- gear

function slotSub(s) {
  if (s.items) return s.items.length ? `${s.items.length} / ${s.id === "food" ? 3 : 3}` : "";
  const it = s.item;
  if (!it) return "";
  const bits = [];
  if (it.rarity && s.id !== "gem") bits.push(it.rarity);
  if (it.level != null) bits.push("level " + it.level);
  if (it.perksMax) bits.push(`${it.perks} / ${it.perksMax} perks`);
  if (it.damage != null) bits.push(fx(it.damage, 0) + " damage");
  return bits.join(" · ");
}

function slotCard(s, wide) {
  const filled = s.items ? s.items.length > 0 : !!s.item;
  const name = s.items ? s.items.map((x) => x.name).join(", ") : s.item ? s.item.name : "";
  return `<button class="slot ${wide ? "wide" : ""} ${filled ? "" : "empty"} ${ui.slot === s.id ? "on" : ""}" data-slot="${s.id}">
    <div class="sn">${esc(s.name)}</div>
    ${filled ? `<div class="it ${s.item ? "r-" + esc(s.item.rarity) : ""}">${esc(name)}</div><div class="sub">${esc(slotSub(s))}</div>` : '<div class="muted">Empty</div>'}
    ${s.id === "weapon" && ST.carried.length ? `<div class="sub">+ ${ST.carried.length} more carried</div>` : ""}</button>`;
}

async function renderGear(v) {
  const by = Object.fromEntries(ST.slots.map((s) => [s.id, s]));
  const order = ["ammo", "gem", "armor0", "armor1", "armor2", "armor3", "armor4", "armor5", "armor6", "food"];
  const s = by[ui.slot] || by.weapon;
  let detail = "";
  if (s.items) {
    detail = `<div style="color:var(--text2);margin-bottom:8px">${s.id === "food" ? "One food per type; 3 slots, 4 with the Dessert Stomach skill." : "Summon spells you keep up; each counts up to its cap and costs mana."}</div>
      <div>${s.items.length ? s.items.map((x, k) => `<span class="pill">${esc(x.name)}${x.level != null ? ` <label class="small">L <input type="number" data-upgrade="${esc(x.guid)}" min="${x.levelMin}" max="${x.levelMax}" value="${x.level}" style="width:52px" aria-label="${esc(x.name)} item level (${x.levelMin} to ${x.levelMax})"></label>` : ""}<button data-unequip="${k}" aria-label="Remove ${esc(x.name)}">×</button></span>`).join("") : '<span class="muted">None yet</span>'}</div>`;
  } else if (s.item) {
    const it = s.item;
    const rar = ["Common", "Uncommon", "Rare", "Epic", "Legendary"];
    detail = `<div class="card raised">
      <div style="display:flex;justify-content:space-between;align-items:center"><b class="r-${esc(it.rarity)}" style="font-size:18px">${esc(it.name)}</b><button class="btn" data-unequip="1">Unequip</button></div>
      <div style="display:flex;flex-wrap:wrap;gap:14px;align-items:center;margin-top:10px">
        ${s.id !== "gem" && (s.id !== "ammo" || it.perksMax != null) ? `<label class="field">Rarity <select data-item="rarity">${rar.map((r) => `<option ${r === it.rarity ? "selected" : ""}>${r}</option>`).join("")}</select></label>` : ""}
        ${it.level != null ? `<label class="field">${s.id === "gem" ? "Gem level" : "Item level"} <input type="number" data-item="level" min="1" max="${CAT.caps.item}" value="${it.level}"></label>` : ""}
        ${it.perksMax ? `<label class="field">Perks unlocked <input type="number" data-item="perks" min="0" max="${it.perksMax}" value="${it.perks}"> / ${it.perksMax}</label>` : ""}
      </div>
      ${it.perkList && it.perkList.length ? `<div style="margin-top:10px">${it.perkList.map((p) => `<div class="perk ${p.on ? "" : "off"}"><span class="g">${p.on ? "◆" : "◇"}</span>${esc(p.name)}${p.on ? "" : ' <span class="small">locked</span>'}</div>`).join("")}</div>` : ""}</div>`;
  } else {
    detail = `<div style="color:var(--text2)">Nothing equipped here. Pick one below; the list is ranked by what it does for this build.</div>`;
  }
  if (s.id === "weapon") detail += carriedList();
  v.innerHTML = `<div class="gear-wrap">
    <div><div style="display:flex;justify-content:space-between;align-items:center;margin-bottom:10px"><h2>Equipped</h2>
      ${ST.owned ? `<button class="btn outline" id="bestowned" data-tip="Wear the best armour and rings from what ${esc(ST.ownedFrom ? ST.ownedFrom.character : "your character")} owns, and take the best owned weapon and arrow or spell in hand (one undo step)">Best of what I own</button>` : ""}</div><div class="slots">${slotCard(by.weapon, true)}${order.map((id) => slotCard(by[id])).join("")}${slotCard(by.summon, true)}</div></div>
    <section class="card"><h2>${esc(s.name)}</h2><div style="margin-top:10px">${detail}</div>
      <div class="card-head" style="margin-top:18px"><div><h2 style="font-size:16px;display:inline">${s.id === "food" ? "Food to add" : s.id === "summon" ? "Summons to add" : "Options for this slot"}</h2> <span style="color:var(--text2)">each tried in your build</span> <span id="cspin"></span></div>
        <div style="display:flex;gap:12px;align-items:center">${s.id === "weapon" && s.item ? `<label class="field" data-tip="Compare weapons of every type, not only the type you hold"><input type="checkbox" id="alltypes" ${ui.allTypes ? "checked" : ""}> All weapon types</label>` : ""}${ST.owned ? `<label class="field" data-tip="${esc(ST.ownedFrom ? `${ST.owned} items from ${ST.ownedFrom.character}'s save, read ${ago2(ST.ownedFrom.loaded)}. Load from game again to refresh.` : "")}"><input type="checkbox" id="ownedonly" ${ui.ownedOnly ? "checked" : ""}> Only what I own</label>` : ""}
        <input type="search" id="cfilter" placeholder="Search name or perk" aria-label="Search items by name or perk" value="${esc(ui.filter)}" style="width:200px"></div></div>
      <div id="cands"></div></section></div>`;
  loadCandidates();
}

// Other weapons the character holds (action bar, backpack, the ranged slot); only the one in hand counts.
function carriedList() {
  const rows = ST.carried.map((w, k) => `<tr><td class="name"><span class="r-${esc(w.rarity)}">${esc(w.name)}</span>
      <div class="small muted">${esc([w.kind, w.rarity, "level " + w.level, w.gem ? "gem: " + w.gem : ""].filter(Boolean).join(" · "))}</div></td>
      <td class="r num"><b>${fx(w.dps)}</b></td><td class="r"><button class="btn" data-use="${k}" data-tip="Take this weapon in hand; the current one moves to this list">Use</button>
      <button class="btn icon" data-drop="${k}" aria-label="Stop carrying ${esc(w.name)}" data-tip="Stop carrying it">×</button></td></tr>`).join("");
  return `<div style="margin-top:16px"><h2 style="font-size:16px">Also carried</h2>
    <div class="small muted" style="margin:2px 0 8px">One bow or staff fits the ranged slot; the rest sit on the action bar or in the backpack. Numbers are for the weapon in hand only. Its gem and arrow or spell go with it.</div>
    ${ST.carried.length ? `<table class="ctable"><thead><tr><th>Weapon</th><th class="r">Total DPS if used</th><th></th></tr></thead><tbody>${rows}</tbody></table>`
      : '<div class="muted">No other weapons. Use “Carry” on an option below to add one without swapping.</div>'}</div>`;
}

async function loadCandidates() {
  const own = ui.ownedOnly && ST.owned > 0;
  const key = JSON.stringify([ui.slot, ST.calcKey, own, ST.owned, ST.carried, ui.allTypes]);
  if (key !== ui.candKey) {
    ui.candKey = key;
    ui.cand = null;
    const seq = ++ui.candSeq;
    if ($("#cspin")) $("#cspin").innerHTML = '<span class="spinner"></span>';
    drawCandidates();
    const r = await invoke("candidates", { slot: ui.slot, owned: own, all: ui.allTypes });
    if (seq !== ui.candSeq) return;
    ui.cand = r;
    if ($("#cspin")) $("#cspin").innerHTML = "";
  }
  drawCandidates();
}

function drawCandidates() {
  const el = $("#cands");
  if (!el) return;
  if (!ui.cand) { el.innerHTML = '<div class="muted">Working out every option…</div>'; return; }
  const { rows, now } = ui.cand;
  const f = ui.filter.toLowerCase();
  const best = Math.max(...rows.map((r) => r.dps)), worst = Math.min(...rows.map((r) => r.dps));
  // the search matches item names and the perks each item carries at its shown rarity ("sneak" finds sneak rings)
  const perkHit = (r) => (r.perks || []).filter((p) => p.toLowerCase().includes(f));
  const shown = rows.filter((r) => !f || r.name.toLowerCase().includes(f) || perkHit(r).length).slice(0, 80);
  const carry = ui.slot === "weapon";
  el.innerHTML = `<table class="ctable"><thead><tr><th>Item</th><th>Rarity</th><th class="r">Total DPS</th><th class="r">vs now</th><th class="r">Hits survived</th>${carry ? "<th></th>" : ""}</tr></thead><tbody>
    ${shown.map((r) => {
      const d = r.dps - now.dps, ds = r.survived - now.survived;
      const t = best > worst ? (r.dps - worst) / (best - worst) : 1;
      const dcol = d > 0 ? mix("#CFC5A6", "#89E191", t) : "var(--bad)";
      return `<tr data-guid="${esc(r.guid)}" ${r.owned != null ? `data-owned="${r.owned}"` : ""} class="${r.equipped ? "eq" : ""}" ${r.equipped ? "" : `tabindex="0" role="button" aria-label="Equip ${esc(r.name)}, ${fx(r.dps)} total DPS"`}><td class="name" style="color:${r.equipped ? "var(--head)" : "var(--text)"}">${esc(r.name)}${r.level != null ? ` <span class="small muted">L${r.level}</span>` : ""}${r.equipped ? " (equipped)" : ""}</td>
        <td class="r-${esc(r.rarity)}">${esc(r.rarity)}</td><td class="r num">${r.tieAbove ? '<span class="muted" data-tip="Within 1% of the row above: the order of these two is a sidegrade, not a ranking">≈ </span>' : ""}<b>${fx(r.dps)}</b></td>
        <td class="r num">${r.equipped || Math.abs(d) < 0.05 ? '<span class="muted">—</span>' : `<b style="color:${dcol}">${d > 0 ? "+" : "−"}${Math.abs(d).toFixed(1)}</b>`}</td>
        <td class="r num">${Math.abs(ds) < 0.05 ? `<span style="color:var(--text2)">${fx(r.survived)}</span>` : `<span class="${ds > 0 ? "c-good" : "c-bad"}">${fx(r.survived)} (${ds > 0 ? "+" : "−"}${Math.abs(ds).toFixed(1)})</span>`}</td>
        ${carry ? `<td class="r"><button class="btn" data-carry="${esc(r.guid)}" data-tip="Add to the weapons you carry, keeping the one in hand">Carry</button></td>` : ""}</tr>`;
    }).join("")}</tbody></table>
    ${carry && !ui.allTypes && ST.slots.some((x) => x.id === "weapon" && x.item) ? '<div class="small muted" style="margin-top:8px">Showing weapons of the type you hold. Tick “All weapon types” to compare others.</div>' : ""}${!rows.length && ui.ownedOnly ? '<div class="muted">You own nothing that fits this slot.</div>' : ""}${rows.some((r) => r.tieAbove) ? '<div class="small muted" style="margin-top:8px">≈ marks a row within 1% of the row above it: the two are too close to call.</div>' : ""}${rows.length ? `<div class="small muted" style="margin-top:8px">Hover a row to preview it in the left panel. Click to equip${carry ? " in hand (replacing the current one), or Carry to keep both" : ""}.</div>` : ui.ownedOnly ? "" : '<div class="muted">Nothing fits this slot.</div>'}`;
}

// the node panel's rank buttons (the keyboard way to buy or sell a node)
document.addEventListener("click", (e) => {
  const b = e.target.closest && e.target.closest("[data-node-act]");
  if (!b) return;
  ui.selected = +b.dataset.node;
  act({ type: b.dataset.nodeAct, node: +b.dataset.node });
});

$("#view").addEventListener("mouseover", (e) => {
  if (ui.tab !== "gear") return;
  const tr = e.target.closest("tr[data-guid]");
  if (tr && !tr.classList.contains("eq")) setPreview(tr.dataset.owned != null ? { slot: ui.slot, guid: tr.dataset.guid, owned: +tr.dataset.owned } : { slot: ui.slot, guid: tr.dataset.guid });
});
// keyboard: a focused candidate row previews like a hover, and Enter or Space equips it like a click
$("#view").addEventListener("focusin", (e) => {
  if (ui.tab !== "gear") return;
  const tr = e.target.closest && e.target.closest("tr[data-guid]");
  if (tr && !tr.classList.contains("eq")) setPreview(tr.dataset.owned != null ? { slot: ui.slot, guid: tr.dataset.guid, owned: +tr.dataset.owned } : { slot: ui.slot, guid: tr.dataset.guid });
});
$("#view").addEventListener("keydown", (e) => {
  if (ui.tab !== "gear" || (e.key !== "Enter" && e.key !== " ")) return;
  const tr = e.target.closest && e.target.closest("tr[data-guid]");
  if (tr && e.target === tr) { e.preventDefault(); tr.click(); }
});
$("#view").addEventListener("mouseout", (e) => {
  if (ui.tab !== "gear") return;
  const tr = e.target.closest("tr[data-guid]");
  if (tr && !tr.contains(e.relatedTarget)) setPreview(null);
});
$("#view").addEventListener("click", (e) => {
  if (ui.tab !== "gear") return;
  const cw = e.target.closest("[data-carry]");
  if (cw) { setPreview(null); return act({ type: "carry_weapon", guid: cw.dataset.carry }); }
  const uw = e.target.closest("[data-use]");
  if (uw) return act({ type: "use_weapon", index: +uw.dataset.use });
  const dw = e.target.closest("[data-drop]");
  if (dw) return act({ type: "drop_weapon", index: +dw.dataset.drop });
  const sl = e.target.closest("[data-slot]");
  if (sl) { ui.slot = sl.dataset.slot; ui.filter = ""; setPreview(null); return renderGear($("#view")); }
  const tr = e.target.closest("tr[data-guid]");
  if (tr && !tr.classList.contains("eq")) {
    setPreview(null);
    const a = { type: "equip", slot: ui.slot, guid: tr.dataset.guid };
    if (tr.dataset.owned != null) a.owned = +tr.dataset.owned;
    return act(a);
  }
  if (e.target.id === "bestowned") { act({ type: "best_owned" }).then(() => flash("Wearing the best of what you own.")); return; }
  const un = e.target.closest("[data-unequip]");
  if (un) act({ type: "unequip", slot: ui.slot, index: +un.dataset.unequip });
});
const itemAct = debounce((field, value) => act({ type: "item", slot: ui.slot, [field]: field === "rarity" ? value : +value }), 300);
$("#view").addEventListener("input", (e) => {
  if (ui.tab === "gear" && e.target.id === "cfilter") { ui.filter = e.target.value; drawCandidates(); }
  if (ui.tab === "gear" && e.target.id === "ownedonly") { ui.ownedOnly = e.target.checked; loadCandidates(); }
  if (ui.tab === "gear" && e.target.id === "alltypes") { ui.allTypes = e.target.checked; loadCandidates(); }
});
// item level, rarity and perks commit on change (Enter, leaving the field, the arrows) so the redraw can't take
// the field away mid-typing
$("#view").addEventListener("change", (e) => {
  if (ui.tab === "gear" && e.target.dataset.item) itemAct(e.target.dataset.item, e.target.value);
  if (ui.tab === "gear" && e.target.dataset.upgrade) act({ type: "upgrade", guid: e.target.dataset.upgrade, level: +e.target.value });
});

// ---- combat setup

const PLAY = [
  ["orb_pickup", "Orbs picked up", "Health and mana orbs you walk over", "pct", () => true],
  ["summon_idle", "Companions idle", "Time summons spend not attacking", "pct", () => ST.companions],
  ["summon_attack_s", "Companion attacks every (s)", "Seconds between one summon's attacks: about 2.6 s when nothing is hitting it, 6–13 s when it is being hit or walks between targets", "sec", () => ST.companions],
  ["summon_life_s", "Gem companion lasts (s)", "How long a Necromancy gem skeleton lives before it dies or decays out of combat; the game data gives no timer", "sec", () => true],
  ["fight_s", "Fight length (s)", "Necromancy gem skeletons start each fight from none; the gem's DPS is the average over this long", "sec", () => true],
  ["sprint_start", "Sprint into fights", "Gem of Swiftness", "bool", () => ST.gem.includes("Swiftness")],
  ["evades_per_min", "Evades per minute", "Gem of Agility", "num", () => ST.gem.includes("Agility")],
  ["air_share", "Attacks in the air", "Gem of Aerobatics", "pct", () => ST.gem.includes("Aerobatics")],
  ["glides_per_min", "Glide landings per minute", "Gem of Meteoric Impact", "num", () => ST.gem.includes("Meteoric")],
  ["blocks_per_min", "Hits blocked per minute", "Gem of Retaliation", "num", () => ST.gem.includes("Retaliation")],
  ["night", "Night", "Gem of Lycanthropy", "bool", () => ST.gem.includes("Lycanthropy")],
  ["elixir", "Elixir active", "Gem of Battle Mania", "bool", () => ST.gem.includes("Battle Mania")],
];

function control(key, kind, val, extra = "") {
  if (kind === "bool") return `<input type="checkbox" data-f="${key}" data-k="bool" ${val ? "checked" : ""}>`;
  if (kind === "pct") return `<span class="field"><input type="number" data-f="${key}" data-k="pct" min="0" max="100" value="${Math.round(val * 100)}"> %</span>`;
  if (kind === "sec") return `<span class="field"><input type="number" data-f="${key}" data-k="num" min="0.5" step="0.1" value="${val}"> s</span>`;
  return `<input type="number" data-f="${key}" data-k="num" ${extra} value="${val}">`;
}

function setting(name, help, ctl, relevant = true) {
  // the control's accessible name is the setting's name (its help text describes it)
  const label = String(name).replace(/<[^>]*>/g, "").replace(/"/g, "&quot;");
  const named = ctl.replace(/<(input|select)(?![^>]*aria-label)/, `<$1 aria-label="${label}"`);
  return `<div class="setting ${relevant ? "" : "dim"}"><div><div class="nm">${name}</div><div class="hp">${help}</div></div>${named}</div>`;
}

function renderSetup(v) {
  const f = ST.fight;
  const kinds = ["Normal", "Sneak", "Merciless", "Backstab"];
  const types = ["Cut", "Pierce", "Blunt", "Fire", "Ice", "Fog", "Lightning", "Poison"];
  v.innerHTML = `<div class="grid3">
    <section class="card"><h2 style="margin-bottom:12px">The enemy</h2>
      <label class="stackf"><span>Enemy</span><input type="search" id="efilter" placeholder="Search by name or faction" value="${esc(ui.efilter || "")}">
        <select data-f="enemy" data-k="int" id="esel" size="1">${enemyOptions()}</select>
        <span class="small" style="color:var(--text2)">Sets resistances, faction bonuses and the hits it deals.</span></label>
      ${setting("Level", "Enemy health and damage scale with it", control("enemy_level", "num", f.enemy_level, `min="1" max="${CAT.caps.item}"`))}
      ${setting("Health ×", "Multiplies the picked enemy's own health from the game data (bosses have up to 35 times a normal enemy's). 1 = as the game has it on the Default world difficulty; raise it for a harder world.", control("health_mult", "num", f.health_mult ?? 1, 'min="0.1" max="3" step="0.05"'))}
      ${setting("Physical armour", "Reduces your cut, pierce and blunt hits", control("armor_physical", "num", f.armor_physical, 'min="0" max="500"'))}
      ${setting("Magical armour", "Reduces your fire, ice and fog hits", control("armor_magical", "num", f.armor_magical, 'min="0" max="500"'))}
      ${setting("Its hits deal", "Damage type used for Hits survived", `<select data-f="incoming_type" data-k="str">${types.map((t) => `<option ${t === f.incoming_type ? "selected" : ""}>${t}</option>`).join("")}</select>`)}
      ${setting("Target soaked", "Wet enemies: bonuses that need it apply", control("soaked", "bool", f.soaked))}
    </section>
    <div class="stack">
      <section class="card"><h2 style="margin-bottom:12px">Your hits</h2>
        <div class="seg" id="hitkind">${kinds.map((k) => `<button data-hk="${k}" class="${f.hit_kind === k ? "on" : ""}" aria-pressed="${f.hit_kind === k}">${k}</button>`).join("")}</div>
        <div class="small" style="color:var(--text2);margin:8px 0">Sneak: on an unaware enemy. Merciless: on an overpowered enemy (its stun bar is full). Backstab: from behind.</div>
        ${f.hit_kind !== "Normal" ? setting("Share of hits", f.hit_kind === "Backstab" ? "How many of your hits land from behind; the rest are normal." : `Share of your attacking time spent on ${esc(f.hit_kind.toLowerCase())} attacks, each its own key-press action at its own pace; the rest is your normal attacks. A merciless attack needs an overpowered enemy, a sneak attack an unaware one, so they are moments in a fight: 20% is a starting guess, not a game value.`, control("hit_share", "pct", f.hit_share ?? 1)) : ""}
        ${setting("Weak-spot hits", "Skill shots on enemies that have a weak spot", control("weak_spot", "bool", f.weak_spot))}
        ${setting("Heavy attacks", "Melee: heavy attacks back to back instead of the light combo. In 1.0 a heavy attack hits every enemy within its reach.", `<input type="checkbox" id="heavy" ${ST.heavy ? "checked" : ""}>`)}
        ${setting("Zones alive at once", "How many zones of one spell can burn at the same time. The game limits it, but the limit is not in its data; 1 means each new cast replaces the last.", control("zone_cap", "num", f.zone_cap ?? 1, 'min="1" max="10"'))}
        ${setting("Zone stay", "Share of a zone spell's lifetime (Pillar Of Flames, Blizzard, Ball Lightning) an enemy stays inside it; the zone hits it every 0.5 s while it does.", control("zone_stay", "pct", f.zone_stay ?? 0.5))}
        ${ST.heavy ? setting("Heavy attack reach (m)", "How far from you a heavy attack hits. The game data has no reach, so this is your estimate; enemies inside it all take the hit.", control("heavy_reach_m", "num", f.heavy_reach_m ?? 2.5, 'min="0.5" max="8" step="0.5"')) : ""}</section>
      <section class="card"><h2 style="margin-bottom:12px">The group</h2>
        ${setting("Enemies", "How many are fighting you at once", control("pack_size", "num", f.pack_size, 'min="1" max="8"'))}
        ${setting("Spread (m)", "How far apart they stand", control("pack_radius", "num", f.pack_radius, 'min="2" max="10" step="0.5"'))}
        <div class="small" style="color:var(--text2);margin-top:8px">Decides how many enemies explosions, chains and Storm's Eye reach, and whether group buffs such as Warden apply.</div></section>
    </div>
    <section class="card"><h2>How you play</h2><div class="small" style="color:var(--text2);margin:4px 0 8px">Dimmed rows only matter with gear this build doesn't have.</div>
      ${setting("Rested", "From resting by a bed or fire: more stamina and stamina regeneration, growing with your level", `<input type="checkbox" id="rested" ${ST.rested ? "checked" : ""}>`)}
      ${PLAY.map(([k, n, h, kind, rel]) => setting(n, h, control(k, kind, f[k]), rel())).join("")}
      <button class="btn" id="resetplay" style="margin-top:12px">Reset to defaults</button></section></div>`;
}

function enemyOptions() {
  const q = (ui.efilter || "").toLowerCase();
  return CAT.enemies.map((l, i) => (i === 0 || i === ST.fight.enemy || !q || l.toLowerCase().includes(q) ? `<option value="${i}" ${i === ST.fight.enemy ? "selected" : ""}>${esc(l)}</option>` : "")).join("");
}

const fightAct = debounce(() => act({ type: "fight", fight: ST.fight }), 250);
function readFight(el) {
  const k = el.dataset.f, kind = el.dataset.k;
  let v = el.value;
  if (kind === "bool") v = el.checked;
  else if (kind === "pct") v = Math.max(0, Math.min(100, +v || 0)) / 100;
  else if (kind === "num") v = +v || 0;
  else if (kind === "int") v = parseInt(v, 10) || 0;
  ST.fight[k] = v;
  fightAct();
}
$("#view").addEventListener("input", (e) => {
  if (ui.tab !== "setup") return;
  if (e.target.id === "efilter") { ui.efilter = e.target.value; $("#esel").innerHTML = enemyOptions(); return; }
  if (e.target.dataset.f) readFight(e.target);
});
$("#view").addEventListener("change", (e) => {
  if (ui.tab === "setup" && e.target.id === "rested") return act({ type: "rested", value: e.target.checked });
  if (ui.tab === "setup" && e.target.id === "heavy") return act({ type: "heavy", value: e.target.checked });
  if (ui.tab === "setup" && e.target.dataset.f && (e.target.type === "checkbox" || e.target.tagName === "SELECT")) readFight(e.target);
});
$("#view").addEventListener("click", (e) => {
  if (ui.tab !== "setup") return;
  const hk = e.target.closest("[data-hk]");
  if (hk && hk.dataset.hk !== ST.fight.hit_kind) {
    // each kind keeps the share the player gave it this session
    ui.hitShares = Object.assign(ui.hitShares || {}, { [ST.fight.hit_kind]: ST.fight.hit_share });
    ST.fight.hit_kind = hk.dataset.hk;
    // a sneak or merciless attack is a moment, not every attack: start at a fifth of the attacking time (a guess to
    // adjust; backstab is positioning and starts at every hit)
    ST.fight.hit_share = ui.hitShares[hk.dataset.hk] ?? (["Sneak", "Merciless"].includes(hk.dataset.hk) ? 0.2 : 1);
    act({ type: "fight", fight: ST.fight });
  }
  if (e.target.id === "resetplay") act({ type: "reset_play" });
});

// ---- calculations

async function renderCalcs(v) {
  const [c, sheet] = await Promise.all([invoke("calcs"), invoke("sheet", { query: ui.sheetQ, all: ui.sheetAll })]);
  if (ui.tab !== "calcs") return;
  const sections = c.sections.length ? c.sections.map((s) => `<details class="sec ${s.warn ? "warn" : ""}" data-sec="${s.id}" ${ui.open[s.id] ?? s.open ? "open" : ""}>
      <summary><h2>${esc(s.name)}</h2><span class="num ${s.warn ? "c-warn" : ""}" style="color:${s.warn ? "" : "var(--soft)"}">${esc(s.summary)}</span></summary>
      <div class="rows">${s.rows.length ? s.rows.map((r) => `<div class="row ${r.strong ? "strong" : ""}"><span class="k">${esc(r.label)}</span><span class="v num">${esc(r.value)}</span></div>${(r.sub || []).map((x) => `<div class="subrow">${esc(x)}</div>`).join("")}`).join("") : `<div class="muted" style="padding:6px 0">${esc(s.empty || "")}</div>`}</div></details>`).join("")
    : `<div class="card"><span style="color:var(--text2)">Pick a weapon on the Gear tab to see the damage calculations.</span></div>`;
  v.innerHTML = `<div class="grid2"><div>${sections}</div>
    <section class="card sheet"><div class="card-head"><h2>Character sheet</h2><label class="field"><input type="checkbox" id="sheetall" ${ui.sheetAll ? "checked" : ""}> Show unchanged</label></div>
      <input type="search" id="sheetq" placeholder="Find a stat (e.g. crit, bow, armour)" value="${esc(ui.sheetQ)}" style="width:100%">
      <div id="sheetrows">${sheetRows(sheet)}</div>
      <div class="small muted" style="margin-top:10px">Hover a stat to see every source that sets it.</div></section></div>`;
}

function sheetRows(rows) {
  let g = "", out = "";
  for (const r of rows) {
    if (r.group !== g) { g = r.group; out += `<div class="eyebrow grp">${esc(g)}</div>`; }
    const tip = [r.key, ...r.sources].join("\n");
    out += `<div class="row" data-tip="${esc(tip)}"><span class="k">${esc(r.label)}</span><span class="v num" style="font-weight:700;color:${r.sources.length ? "var(--text)" : "var(--text2)"}">${esc(r.value)}</span></div>`;
  }
  return out || '<div class="muted" style="margin-top:8px">No stat matches.</div>';
}

$("#view").addEventListener("toggle", (e) => { if (e.target.dataset && e.target.dataset.sec) ui.open[e.target.dataset.sec] = e.target.open; }, true);
const sheetReload = debounce(async () => { $("#sheetrows").innerHTML = sheetRows(await invoke("sheet", { query: ui.sheetQ, all: ui.sheetAll })); }, 150);
$("#view").addEventListener("input", (e) => {
  if (ui.tab !== "calcs") return;
  if (e.target.id === "sheetq") { ui.sheetQ = e.target.value; sheetReload(); }
  if (e.target.id === "sheetall") { ui.sheetAll = e.target.checked; sheetReload(); }
});

// ---- optimizer: "find my build" settings on the left (saved with the build), results on the right — the best
// build for this level, or a plan per milestone shown as level tabs

const PRESETS = {
  Balanced: ["Damage", "Survival", "Movement", "Sustain", "Utility"],
  "Glass cannon": ["Damage", "Sustain", "Movement", "Survival", "Utility"],
  Explorer: ["Movement", "Utility", "Survival", "Damage", "Sustain"],
  "Co-op support": ["Sustain", "Survival", "Utility", "Damage", "Movement"],
};
const AMMO_KIND = { Bow: "Arrow", Staff: "Spell" };
let OPTCAT = null;

async function optCatalog() {
  if (!OPTCAT) OPTCAT = await invoke("opt_catalog");
  return OPTCAT;
}
// settings are kept in the backend (saved with the build); the page edits a copy and sends it back on each change
function optSettings() { return OPTCAT.settings; }
const saveOpt = () => invoke("act", { action: { type: "opt_settings", settings: OPTCAT.settings } }).catch((e) => flash(String(e), true));

function skillById(id) { return CAT.skills.find((s) => s.id === id); }
function skillOptions() {
  const seen = new Set();
  return CAT.skills.filter((s) => s.kind !== "Root" && s.kind !== "Attribute" && !seen.has(s.name) && seen.add(s.name))
    .map((s) => `<option value="${esc(s.name)}">`).join("");
}

function weaponRows(s) {
  if (!s.weapons.length) {
    const held = ST.slots.find((x) => x.id === "weapon");
    return `<div class="muted" style="margin:4px 0 8px">Builds for the weapon in hand${held && held.item ? `: <b class="r-${esc(held.item.rarity)}">${esc(held.item.name)}</b>` : " (none yet)"}. Add weapons to plan for a weapon type, or for several.</div>`;
  }
  const total = s.weapons.reduce((a, w) => a + Math.max(0, w.share || 0), 0) || 1;
  return s.weapons.map((w, k) => {
    const kind = AMMO_KIND[w.category];
    const fams = kind === "Arrow" ? OPTCAT.arrows : kind === "Spell" ? OPTCAT.spells : [];
    return `<div class="wrow">
      <select data-w="${k}" data-wf="category" aria-label="Weapon type">${OPTCAT.categories.map((c) => `<option ${c === w.category ? "selected" : ""}>${esc(c)}</option>`).join("")}</select>
      ${kind ? `<select data-w="${k}" data-wf="ammo_family" aria-label="${kind === "Arrow" ? "Arrows" : "Spell"}"><option value="">${kind === "Arrow" ? "Best arrows" : "Best spell"} for the level</option>${fams.map((f) => `<option ${f === w.ammo_family ? "selected" : ""}>${esc(f)}</option>`).join("")}</select>` : '<span class="muted small">no ammo</span>'}
      <label class="field small" data-tip="Share of fight time with this weapon">${s.weapons.length > 1 ? `<input type="number" data-w="${k}" data-wf="share" min="0" max="100" value="${Math.round((w.share / total) * 100)}"> %` : ""}</label>
      <button class="btn icon" data-wdel="${k}" aria-label="Remove this weapon">×</button></div>`;
  }).join("");
}

// summons the optimizer works with (docs/plans/optimizer-summons.md): the loadout's, the best for each level, or chosen
// kinds at the tier each level allows (none chosen: no summons). Settings: { mode: "keep" | "best" | "fixed", families }.
function summonRows(s, seg) {
  const sc = s.summons || { mode: "keep" };
  const mode = sc.mode || "keep";
  const fams = (sc.mode === "fixed" && sc.families) || [];
  const pick = mode !== "fixed" ? "" : `<div style="display:flex;flex-wrap:wrap;gap:6px;align-items:center;margin-top:8px">
      ${fams.map((f) => `<span class="pill">${esc(f)}<button data-sumdel="${esc(f)}" aria-label="Remove ${esc(f)}">×</button></span>`).join("")}
      <select id="sumadd" aria-label="Add a summon"><option value="">${fams.length ? "Add another" : "Add a summon"}…</option>${(OPTCAT.summons || []).filter((f) => !fams.includes(f)).map((f) => `<option>${esc(f)}</option>`).join("")}</select>
      ${fams.length ? "" : '<span class="small muted">none chosen: no summons</span>'}</div>`;
  return seg("optsummon", [["keep", "Keep mine"], ["best", "Best for the level"], ["fixed", "Choose"]], mode)
    + pick + `<div class="small muted" style="margin-top:6px">Summon spells are cast from a staff (in hand or carried); each kind is taken at the highest tier the level allows.</div>`;
}

function skillChips(ids, kind) {
  const chips = ids.map((id) => { const sk = skillById(id); return sk ? `<span class="pill">${esc(sk.name)}<button data-sdel="${kind}" data-id="${id}" aria-label="Remove ${esc(sk.name)}">×</button></span>` : ""; }).join("");
  return `<div style="display:flex;flex-wrap:wrap;gap:6px;align-items:center">${chips}
    <input type="text" list="skilllist" data-sadd="${kind}" placeholder="${kind === "required" ? "Add a skill to always take" : "Add a skill to never take"}" style="width:210px"></div>`;
}

function milestoneEditor(s) {
  const ms = [...s.milestones].sort((a, b) => a.level - b.level);
  return `<div style="display:flex;flex-wrap:wrap;gap:6px">${ms.map((m) => `<span class="pill ms"><span class="small muted">L</span><input type="number" aria-label="Milestone character level" data-ml="${m.level}" data-mf="level" min="1" max="${CAT.caps.level}" value="${m.level}"><span class="small muted">Flame</span><input type="number" aria-label="Milestone Flame level" data-ml="${m.level}" data-mf="flame_level" min="1" max="${CAT.caps.flame}" value="${m.flame_level}"><button data-mdel="${m.level}" aria-label="Remove level ${m.level}">×</button></span>`).join("")}
    <button class="btn" id="msadd">+ Level</button><button class="btn" id="msreset" data-tip="One milestone at the end of each region's level band, with the Flame level players usually have there. Both come from a community pacing guide, not game data, and sources disagree on the bands.">Region bands</button></div>
    <label class="field" style="margin-top:8px"><input type="checkbox" id="msrespec" ${s.respec ? "checked" : ""}> Allow respec between milestones (otherwise each tree grows from the one before)</label>`;
}

async function renderOptimizer(v) {
  await optCatalog();
  if (ui.tab !== "optimizer") return;
  const s = optSettings();
  const r = ST.ranking;
  const preset = Object.keys(PRESETS).find((k) => PRESETS[k].join() === r.join());
  const seg = (id, opts, cur) => `<div class="seg" id="${id}">${opts.map(([k, l, dis]) => `<button data-v="${k}" class="${k === cur ? "on" : ""}" aria-pressed="${k === cur}" ${dis ? "disabled" : ""}>${l}</button>`).join("")}</div>`;
  const left = `<section class="card optset"><h2>Find my build</h2>
      <div class="eyebrow">Start from</div>${seg("optpreset", Object.keys(PRESETS).map((k) => [k, k]), preset)}
      <div class="eyebrow">Weapons</div>${weaponRows(s)}<button class="btn" id="wadd">+ Add weapon</button>
      <div class="eyebrow">Summons</div>${summonRows(s, seg)}
      <div class="eyebrow">Goals, most important first</div>
      ${r.map((g, i) => `<div class="goal"><span class="i">${i + 1}</span><span class="g" tabindex="0" data-tip="${esc(GOAL_TIPS[g] || "")}">${esc(g)}</span>
        <button class="btn icon" data-up="${i}" ${i === 0 ? "disabled" : ""} aria-label="Higher priority">▲</button><button class="btn icon" data-down="${i}" ${i === r.length - 1 ? "disabled" : ""} aria-label="Lower priority">▼</button></div>`).join("")}
      <div class="eyebrow">Always take</div>${skillChips(s.required, "required")}
      <div class="eyebrow">Never take</div>${skillChips(s.avoided, "avoided")}
      <datalist id="skilllist">${skillOptions()}</datalist>
      <div class="eyebrow">Gear</div>${seg("optgearmode", [["keep", "Keep mine"], ["best", "Best in the game"], ["owned", "Best I own", !ST.owned]], s.gear)}
      ${s.gear === "best" ? `<label class="field" style="margin-top:8px">Rarity for items that roll it <select id="optrar">${["Common", "Uncommon", "Rare", "Epic", "Legendary"].map((x) => `<option ${x === s.rarity ? "selected" : ""}>${x}</option>`).join("")}</select></label>` : ""}
      ${s.gear === "owned" ? `<div class="small muted" style="margin-top:6px">From ${ST.ownedFrom ? esc(ST.ownedFrom.character) + "'s save" : "your save"}; milestones still pick from the whole game.</div>` : ""}
      <div class="eyebrow">Effort</div>${seg("opteffort", [["quick", "Quick"], ["thorough", "Thorough"]], s.thorough ? "thorough" : "quick")}
      <div class="eyebrow">Milestones</div>${milestoneEditor(s)}
      <div style="display:flex;gap:8px;margin-top:16px;flex-wrap:wrap">
        <button class="btn primary" id="optrun" ${ui.optBusy ? "disabled" : ""}>Best build at level ${ST.level}</button>
        <button class="btn outline" id="msrun" ${ui.optBusy ? "disabled" : ""}>Plan every milestone</button></div>
      <div class="small muted" style="margin-top:6px">Uses this loadout's food${(s.summons || {}).mode && s.summons.mode !== "keep" ? "" : ", summons"} and the Combat setup.</div></section>`;
  const tabs = `<div class="seg" id="optview" style="margin-bottom:12px">${[["level", "This level"], ["milestones", "Milestones"]].map(([k, l]) => `<button data-v="${k}" class="${(ui.optView || "level") === k ? "on" : ""}" aria-pressed="${(ui.optView || "level") === k}">${l}</button>`).join("")}</div>`;
  const right = `<section class="card">${tabs}<div id="optresult">${ui.optBusy ? busyText() : (ui.optView === "milestones" ? milestonesHtml() : levelHtml())}</div></section>`;
  v.innerHTML = `<div class="grid2" style="grid-template-columns:minmax(0,460px) minmax(0,1fr)">${left}${right}</div>`;
}

function busyText() {
  const p = ui.optProgress;
  const what = ui.optJob === "milestones" ? (p && p.total ? `Planning milestone ${Math.min(p.done + 1, p.total)} of ${p.total}…` : "Planning milestones…") : "Searching for the best build…";
  return `<div style="color:var(--text2)"><span class="spinner"></span> ${what}</div>`;
}

function levelHtml() {
  const res = ui.optRes;
  if (!res) return `<div class="muted">Run "Best build at level ${ST.level}" to see the best tree${optSettings().gear !== "keep" ? " and gear" : ""} for your goals here, next to what you have now.</div>`;
  const n = res.now, p = res.proposed;
  const row = (l, a, b, hi, d = 1) => `<tr><td>${l}</td><td class="r num">${fx(a, d)}</td><td class="r num" style="color:var(--head);font-weight:700">${fx(b, d)}</td><td class="r num">${delta(a, b, hi, d) || '<span class="muted">—</span>'}</td></tr>`;
  return `<table class="cmp" style="width:100%;border-collapse:collapse"><tr><td></td><td class="r small" style="color:var(--text2)">Now</td><td class="r small" style="color:var(--text2)">Proposed</td><td class="r small" style="color:var(--text2)">Change</td></tr>
      ${row("Total DPS", n.dps, p.dps, true)}${row("Expected hit", n.expected_hit, p.expected_hit, true)}${row("Hits survived", n.hits_survived, p.hits_survived, true)}${row("Health", n.health, p.health, true, 0)}
      ${n.has_mana_cost || p.has_mana_cost ? row("Mana uptime %", n.mana_uptime * 100, p.mana_uptime * 100, true, 0) : ""}</table>
    <div style="color:var(--text2);margin:10px 0">${res.used} of ${res.have} points used</div>
    ${res.gear ? `<div class="eyebrow" style="margin-top:14px">Gear it chose</div>${res.gear.map((g) => `<div class="row"><span class="k">${esc(g.slot)}</span><span class="v ${g.item ? "r-" + esc(g.item[1]) : "muted"}">${g.item ? esc(g.item[0]) : "Empty"}${g.changed ? ' <span class="small" style="color:var(--teal)">changed</span>' : ""}</span></div>`).join("")}` : ""}
    <div style="display:flex;gap:10px;align-items:center;margin-top:14px;flex-wrap:wrap">
      <button class="btn primary" id="optapply">Apply to this loadout</button>
      <span style="color:var(--text2)">Leveling order</span>
      <button class="btn icon" id="optprev">◀</button><b class="num">${ui.optStep}/${res.steps.length}</b><button class="btn icon" id="optnext">▶</button>
      <button class="btn" id="optshow">Show the tree at this step</button></div>
    ${ui.optStep > 0 ? `<div class="soft" style="margin-top:8px">Step ${ui.optStep}: ${esc(res.steps[ui.optStep - 1])}</div>` : ""}`;
}

function milestonesHtml() {
  const ms = ui.msRes && ui.msRes.milestones;
  if (!ms || !ms.length) return `<div class="muted">Run "Plan every milestone" for a tree${optSettings().gear !== "keep" ? ", gear and weapons" : ""} at each level on the left. Each tab can become a loadout.</div>`;
  const k = Math.min(ui.msSel || 0, ms.length - 1), m = ms[k];
  const tabs = `<div class="brackets">${ms.map((x, j) => `<button data-ms="${j}" class="${j === k ? "on" : ""}">L${x.level} · F${x.flame}</button>`).join("")}</div>`;
  const chip = (a) => `<span class="pill" style="border-color:var(--${(a.class || "core").toLowerCase()})" data-tip="${esc(a.kind)}">${skillMark(a.node, 20)}${esc(a.name)}${a.ranks > 1 ? ` <b>×${a.ranks}</b>` : ""}</span>`;
  const gearRow = (g) => {
    if (!g.item) return `<div class="row"><span class="k">${esc(g.slot)}</span><span class="v muted">Empty</span></div>`;
    const x = g.extra || {};
    const info = [x.category && g.slot !== "Weapon" ? x.category : "", x.ammo ? "with " + x.ammo : "", x.share != null && x.share < 0.999 ? Math.round(x.share * 100) + "% of the fight" : ""].filter(Boolean).join(" · ");
    return `<div class="grow"><span class="k">${esc(g.slot === "Also" ? "Also carry" : g.slot)}</span><div><b class="r-${esc(g.rarity)}">${esc(g.item)}</b>${info ? ` <span class="small muted">${esc(info)}</span>` : ""}
      ${g.perks && g.perks.length ? `<div class="small muted">${esc(g.perks.join(" · "))}</div>` : ""}</div></div>`;
  };
  return `${tabs}
    <div class="chain" style="margin:10px 0">
      <div class="tile"><div class="l">Points</div><div class="v num">${m.spent} / ${m.budget}</div></div>
      <div class="tile"><div class="l">Total DPS</div><div class="v num">${fx(m.dps)}</div></div>
      <div class="tile"><div class="l">Hits survived</div><div class="v num">${fx(m.survived)}</div></div>
      ${m.kills != null ? `<div class="tile"><div class="l">Kills per hour</div><div class="v num">${fx(m.kills, 0)}</div></div>` : ""}</div>
    <div class="eyebrow">${k === 0 ? "Skills to take" : `New skills since level ${ms[k - 1].level}`}</div>
    <div style="display:flex;flex-wrap:wrap;gap:6px">${m.added.length ? m.added.map(chip).join("") : '<span class="muted">None</span>'}</div>
    ${m.removed.length ? `<div class="eyebrow">Respec away</div><div class="soft">${m.removed.map(esc).join(", ")}</div>` : ""}
    <div class="eyebrow">Gear and weapons</div>${m.gear.map(gearRow).join("")}
    <div style="display:flex;gap:8px;margin-top:14px;flex-wrap:wrap">
      <button class="btn primary" data-msapply="${k}">${m.loadout ? "Update" : "Make"} loadout “Level ${m.level}”</button>
      <button class="btn" id="msapplyall">Make loadouts for every milestone</button></div>
    <div class="small muted" style="margin-top:8px">Milestones assume the Shroud Roots and Elixir Wells points of an average player at each level.</div>`;
}

async function runOpt(job) {
  ui.optBusy = true; ui.optJob = job; ui.optProgress = null;
  if (job === "milestones") ui.optView = "milestones"; else ui.optView = "level";
  renderOptimizer($("#view"));
  const poll = setInterval(async () => {
    ui.optProgress = await invoke("opt_progress");
    if (ui.tab === "optimizer" && $("#optresult")) $("#optresult").innerHTML = busyText();
  }, 500);
  try {
    if (job === "milestones") { ui.msRes = await invoke("milestones"); ui.msSel = 0; flash("Milestone plan ready."); }
    else { ui.optRes = await invoke("optimize"); ui.optStep = ui.optRes.steps.length; flash("Optimizer finished."); }
  } catch (err) { flash(String(err), true); }
  clearInterval(poll);
  ui.optBusy = false;
  if (ui.tab === "optimizer") renderOptimizer($("#view"));
}

$("#view").addEventListener("click", async (e) => {
  if (ui.tab !== "optimizer") return;
  const t = e.target.closest("button");
  if (!t) return;
  const s = OPTCAT && OPTCAT.settings;
  const rerender = () => { saveOpt(); renderOptimizer($("#view")); };
  if (t.dataset.up || t.dataset.down) {
    const r = [...ST.ranking];
    const i = +(t.dataset.up ?? t.dataset.down), j = t.dataset.up ? i - 1 : i + 1;
    [r[i], r[j]] = [r[j], r[i]];
    return act({ type: "ranking", order: r });
  }
  const segv = t.closest(".seg") && t.dataset.v;
  if (segv && t.closest("#optpreset")) return act({ type: "ranking", order: PRESETS[segv] });
  if (segv && t.closest("#optgearmode")) { s.gear = segv; return rerender(); }
  if (segv && t.closest("#opteffort")) { s.thorough = segv === "thorough"; return rerender(); }
  if (segv && t.closest("#optsummon")) { s.summons = segv === "fixed" ? { mode: "fixed", families: (s.summons && s.summons.families) || [] } : { mode: segv }; return rerender(); }
  if (t.dataset.sumdel != null) { s.summons.families = s.summons.families.filter((f) => f !== t.dataset.sumdel); return rerender(); }
  if (segv && t.closest("#optview")) { ui.optView = segv; return renderOptimizer($("#view")); }
  if (t.id === "wadd") { s.weapons.push({ category: OPTCAT.categories.includes("Bow") ? "Bow" : OPTCAT.categories[0], share: 1 }); return rerender(); }
  if (t.dataset.wdel != null) { s.weapons.splice(+t.dataset.wdel, 1); return rerender(); }
  if (t.dataset.sdel) { s[t.dataset.sdel] = s[t.dataset.sdel].filter((x) => x !== +t.dataset.id); return rerender(); }
  if (t.dataset.mdel) { s.milestones = s.milestones.filter((m) => m.level !== +t.dataset.mdel); return rerender(); }
  if (t.id === "msadd") {
    const top = Math.max(0, ...s.milestones.map((m) => m.level));
    s.milestones.push({ level: Math.min(CAT.caps.level, top + 5), flame_level: Math.max(1, ...s.milestones.map((m) => m.flame_level)) });
    return rerender();
  }
  if (t.id === "msreset") { s.milestones = OPTCAT.milestones.map((m) => ({ ...m })); return rerender(); }
  if (t.dataset.ms) { ui.msSel = +t.dataset.ms; return renderOptimizer($("#view")); }
  if (t.dataset.msapply) { const ok = await act({ type: "apply_milestone", index: +t.dataset.msapply }); ui.msRes = await invoke("milestones_view"); if (ok) flash("Loadout made and opened."); return; }
  if (t.id === "msapplyall") { const ok = await act({ type: "apply_milestone" }); ui.msRes = await invoke("milestones_view"); if (ok) flash("A loadout for every milestone. Switch between them at the top."); return; }
  if (t.id === "optrun") return runOpt("level");
  if (t.id === "msrun") return runOpt("milestones");
  if (t.id === "optapply") act({ type: "apply_opt" });
  if (t.id === "optprev" && ui.optStep > 0) { ui.optStep--; renderOptimizer($("#view")); }
  if (t.id === "optnext" && ui.optRes && ui.optStep < ui.optRes.steps.length) { ui.optStep++; renderOptimizer($("#view")); }
  if (t.id === "optshow") act({ type: "opt_step", step: ui.optStep });
});
$("#view").addEventListener("change", (e) => {
  if (ui.tab !== "optimizer" || !OPTCAT) return;
  const s = OPTCAT.settings, el = e.target;
  const rerender = () => { saveOpt(); renderOptimizer($("#view")); };
  if (el.id === "optrar") { s.rarity = el.value; return saveOpt(); }
  if (el.id === "sumadd" && el.value) { s.summons = { mode: "fixed", families: [...((s.summons && s.summons.families) || []), el.value] }; return rerender(); }
  if (el.id === "msrespec") { s.respec = el.checked; return saveOpt(); }
  if (el.dataset.w != null) {
    const w = s.weapons[+el.dataset.w], f = el.dataset.wf;
    if (f === "category") { w.category = el.value; w.ammo_family = null; w.ammo = null; w.weapon = null; }
    else if (f === "ammo_family") { w.ammo_family = el.value || null; w.ammo = null; }
    else if (f === "share") { w.share = Math.max(0, +el.value || 0); }
    return rerender();
  }
  if (el.dataset.ml) {
    const m = s.milestones.find((x) => x.level === +el.dataset.ml);
    if (m) m[el.dataset.mf] = Math.max(1, Math.min(el.dataset.mf === "level" ? CAT.caps.level : CAT.caps.flame, Math.round(+el.value || 1)));
    return rerender();
  }
  if (el.dataset.sadd) {
    const sk = CAT.skills.find((x) => x.name.toLowerCase() === el.value.trim().toLowerCase());
    if (!sk) { flash("No skill by that name.", true); return; }
    const list = s[el.dataset.sadd], other = s[el.dataset.sadd === "required" ? "avoided" : "required"];
    if (!list.includes(sk.id)) list.push(sk.id);
    const j = other.indexOf(sk.id); if (j >= 0) other.splice(j, 1);
    return rerender();
  }
});

// ---- notes

async function renderNotes(v) {
  ui.code = await invoke("code");
  v.innerHTML = `<div style="max-width:820px" class="stack">
    <section class="card"><h2>Build code</h2><div style="color:var(--text2);margin:4px 0 8px">A build code holds this loadout's tree, gear, food and level. Paste one to load it.</div>
      <textarea id="codebox" rows="3" spellcheck="false">${esc(ui.code)}</textarea>
      <div style="display:flex;gap:8px;margin-top:8px"><button class="btn primary" id="loadcode">Load into this loadout</button><button class="btn" id="copycode">Copy this loadout's code</button><button class="btn" id="copytext" data-tip="Numbers, fight setup, gear, skills and the code as plain text, for anyone without the planner">Copy as text</button></div></section>
    <section class="card"><h2 style="margin-bottom:8px">Notes</h2><textarea id="notes" rows="16" placeholder="Rotation, gear to farm, reminders…">${esc(ST.notes)}</textarea></section></div>`;
}
const notesAct = debounce((t) => act({ type: "notes", value: t }), 600);
$("#view").addEventListener("input", (e) => { if (ui.tab === "notes" && e.target.id === "notes") notesAct(e.target.value); });
$("#view").addEventListener("click", async (e) => {
  if (ui.tab !== "notes") return;
  if (e.target.id === "loadcode") { if (await act({ type: "load_code", code: $("#codebox").value })) flash("Build loaded."); }
  if (e.target.id === "copycode") { await navigator.clipboard.writeText(await invoke("code")); flash("Build code copied to the clipboard."); }
  if (e.target.id === "copytext") { await navigator.clipboard.writeText(await invoke("text_summary")); flash("Build copied as text: paste it anywhere."); }
});

// ------------------------------------------------------------------ tooltips and shortcuts

document.addEventListener("mousemove", (e) => {
  const tip = $("#tip");
  const t = e.target.closest && e.target.closest("[data-tip]");
  if (!t) { tip.style.display = "none"; return; }
  tip.textContent = t.dataset.tip;
  tip.style.display = "block";
  const w = tip.offsetWidth, h = tip.offsetHeight;
  tip.style.left = Math.min(window.innerWidth - w - 8, e.clientX + 14) + "px";
  tip.style.top = Math.min(window.innerHeight - h - 8, e.clientY + 16) + "px";
});

// the same tips for keyboard focus: shown under the focused control and read as its description
document.addEventListener("focusin", (e) => {
  const tip = $("#tip");
  document.querySelectorAll('[aria-describedby="tip"]').forEach((x) => x.removeAttribute("aria-describedby"));
  const t = e.target.closest && e.target.closest("[data-tip]");
  if (!t || !t.dataset.tip) { tip.style.display = "none"; return; }
  tip.textContent = t.dataset.tip;
  tip.style.display = "block";
  if (!e.target.hasAttribute("aria-describedby")) e.target.setAttribute("aria-describedby", "tip");
  const r = e.target.getBoundingClientRect(), w = tip.offsetWidth, h = tip.offsetHeight;
  tip.style.left = Math.max(8, Math.min(window.innerWidth - w - 8, r.left)) + "px";
  tip.style.top = (r.bottom + 6 + h > window.innerHeight - 8 ? Math.max(8, r.top - h - 6) : r.bottom + 6) + "px";
});
document.addEventListener("focusout", () => { $("#tip").style.display = "none"; });

// page zoom for larger text: Ctrl+= / Ctrl+- / Ctrl+0, kept between sessions
function setZoom(z) {
  const v = Math.min(2, Math.max(0.7, Math.round(z * 10) / 10));
  document.documentElement.style.zoom = v;
  document.documentElement.classList.toggle("big", v >= 1.5);
  try { localStorage.setItem("zoom", String(v)); } catch (_) {}
  return v;
}
try { setZoom(+localStorage.getItem("zoom") || 1); } catch (_) {}

document.addEventListener("keydown", (e) => {
  const typing = ["INPUT", "TEXTAREA", "SELECT"].includes(document.activeElement && document.activeElement.tagName);
  if (e.ctrlKey && ["=", "+", "-", "0"].includes(e.key)) {
    e.preventDefault();
    const cur = +document.documentElement.style.zoom || 1;
    flash(`Text size ${Math.round(setZoom(e.key === "0" ? 1 : e.key === "-" ? cur - 0.1 : cur + 0.1) * 100)}%`);
    return;
  }
  if (e.ctrlKey && e.key.toLowerCase() === "z" && !e.shiftKey && !typing) { e.preventDefault(); act({ type: "undo" }); }
  if (e.ctrlKey && (e.key.toLowerCase() === "y" || (e.shiftKey && e.key.toLowerCase() === "z")) && !typing) { e.preventDefault(); act({ type: "redo" }); }
  if (e.key === "Escape" && !$("#dlg").hidden) { closeBuilds(); return; }
  if (e.ctrlKey && e.key.toLowerCase() === "o") { e.preventDefault(); openBuilds(); return; }
  if (ui.tab === "tree" && ui.step && !typing) {
    const k = { ArrowRight: "next", ArrowLeft: "prev", Home: "first", End: "last", Escape: "last" }[e.key];
    if (k) { e.preventDefault(); stepTo(k); return; }
  }
  if (e.ctrlKey && e.key.toLowerCase() === "f") { e.preventDefault(); ui.focusSearch = true; if (ui.tab === "tree") $("#tsearch").focus(); else goTab("tree"); }
});
document.addEventListener("contextmenu", (e) => { if (!e.target.closest("input, textarea")) e.preventDefault(); });

// ------------------------------------------------------------------ start

// skill icons: loaded once; the tree redraws as each image arrives
async function loadIcons() {
  let map = {};
  try { map = (await invoke("skill_icons")) || {}; } catch (e) { return; }
  ICON = CAT.skills.map((s) => {
    const m = map[String(s.id)];
    if (!m) return null;
    const mk = (src) => { const im = new Image(); im.onload = () => { if (ui.tab === "tree") drawTree(); }; im.src = src; return im; };
    return { on: mk(m.on), off: mk(m.off), src: m.on };
  });
}
// a small picture for a skill in lists: its icon, or a diamond in its class colour
function skillMark(i, size = 18) {
  const ic = ICON[i], s = CAT.skills[i];
  if (ic) return `<img class="sico" src="${ic.src}" width="${size}" height="${size}" alt="">`;
  const col = CLASS_COLOR[s && s.class] || "#D9A441";
  return `<svg class="sico" width="${size}" height="${size}" viewBox="-10 -10 20 20" aria-hidden="true"><rect x="-6" y="-6" width="12" height="12" transform="rotate(45)" fill="${col}" opacity=".85"/></svg>`;
}

(async function init() {
  CAT = await invoke("catalog");
  loadIcons();
  ST = await invoke("state");
  $("#game").textContent = CAT.game;
  const start = new URLSearchParams(location.search).get("tab");
  if (start) ui.tab = start;
  renderTabs();
  await refresh();
})();
