// Live damage meter (docs/plans/live-meter.md): polls `meter_state` every 250 ms (the recorder's flush cadence) and
// redraws only when the numbers changed. Layout after Details! Damage Meter: bars per source, a hits table with
// normal and crit apart, recent fights, and damage taken.
const { invoke } = window.__TAURI__.core;
const $ = (s) => document.querySelector(s);
const esc = (s) => String(s ?? "").replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);
const COLORS = ["#E8B04A", "#4FC3C8", "#B48CE0", "#89E191", "#E5534B", "#D9A06B", "#7FA7E8"];
// ?overlay: the off-screen copy the planner captures for the in-game meter (docs/plans/overlay-parity.md); it keeps
// its own tab and fade, and takes hotkeys and clicks from the game through overlayCmd and overlayClick
const OVERLAY = location.search.includes("overlay");
const KEY = OVERLAY ? "meter-overlay" : "meter";
if (OVERLAY) document.documentElement.classList.add("overlay");
const ui = { tab: "damage", fade: 0 };
try { Object.assign(ui, JSON.parse(localStorage.getItem(KEY) || "{}")); } catch (_) {}
const save = () => { try { localStorage.setItem(KEY, JSON.stringify(ui)); } catch (_) {} };

function short(n) {
  if (n == null || !isFinite(n)) return "–";
  const a = Math.abs(n);
  return a >= 1e6 ? (n / 1e6).toFixed(2) + "M" : a >= 1e4 ? (n / 1e3).toFixed(1) + "k" : a >= 100 ? n.toFixed(0) : n.toFixed(1);
}
const clock = (s) => `${Math.floor(s / 60)}:${String(Math.floor(s % 60)).padStart(2, "0")}`;

function bars(rows) {
  if (!rows.length) return '<div class="note">No damage in this fight yet.</div>';
  const top = Math.max(...rows.map((r) => r.damage), 1);
  return rows.map((r, k) => `<div class="bar" title="${esc(r.name)}: ${r.hits} hits, ${r.crit_pct.toFixed(0)}% crits">
    <div class="fill" style="width:${(r.damage / top) * 100}%;background:${COLORS[k % COLORS.length]}"></div>
    <div class="txt"><span class="name">${esc(r.name)}${r.summon ? ' <span class="muted">summon</span>' : ""}</span>
    <span class="val num">${short(r.damage)}</span><span class="val num">(${short(r.dps)}/s)</span><span class="pct num">${(r.share * 100).toFixed(0)}%</span></div></div>`).join("");
}

// Details-style: one bar per player; hovering a bar opens that player's breakdown beside the pointer (tip.js)
function players(rows) {
  if (!rows.length) return '<div class="note">No damage in this fight yet.</div>';
  const top = Math.max(...rows.map((r) => r.damage), 1);
  return rows.map((r, k) => `<div class="bar player${r.me ? " me" : ""}" data-player="${r.id}">
    <div class="fill" style="width:${(r.damage / top) * 100}%;background:${COLORS[k % COLORS.length]}"></div>
    <div class="txt"><span class="name">${esc(r.name)}</span>
    <span class="val num">${short(r.damage)}</span><span class="val num">(${short(r.dps)}/s)</span><span class="pct num">${(r.share * 100).toFixed(0)}%</span></div></div>`).join("");
}

// the breakdown tooltip: the planner shows it beside the pointer (a window here, an image in the game)
let hoverId = null;
let moved = 0;
function hover(id, x, y) {
  if (id === hoverId) {
    // follow the pointer (the window copy), at most every 30 ms
    if (id != null && !OVERLAY && Date.now() - moved > 30) { moved = Date.now(); invoke("tip_move", { x, y }).catch(() => {}); }
    return;
  }
  hoverId = id;
  if (OVERLAY) invoke("tip_game", { player: id }).catch(() => {});
  else if (id == null) invoke("tip_hide").catch(() => {});
  else invoke("tip_show", { player: id, x, y }).catch(() => {});
}

function hits(rows) {
  if (!rows.length) return '<div class="note">No hits of your own in this fight yet.</div>';
  return `<table><thead><tr><th>Attack</th><th class="r">Hits</th><th class="r">Normal</th><th class="r">Crit</th><th class="r">Crit %</th></tr></thead><tbody>
    ${rows.map((p) => `<tr title="${esc(p.weapon)}, ${esc(p.name)}: lowest ${short(p.min)}, highest ${short(p.max)}${p.backstabs ? `, ${p.backstabs} backstabs` : ""}">
      <td>${esc(p.weapon)} <span class="muted">${esc(p.name)}</span></td><td class="r num">${p.hits}</td>
      <td class="r num">${p.avg_normal ? short(p.avg_normal) : "–"}</td><td class="r num">${p.avg_crit ? short(p.avg_crit) : "–"}</td>
      <td class="r num">${p.crit_pct.toFixed(0)}</td></tr>`).join("")}</tbody></table>`;
}

function fights(v) {
  const s = v.session;
  const head = `<div class="note">Session: ${s.fights} fights, ${clock(s.seconds)} in combat, ${short(s.damage)} damage, <b>${short(s.dps)} DPS</b>.</div>`;
  if (!v.history.length) return head;
  return head + `<table><thead><tr><th>Fight</th><th class="r">Time</th><th class="r">DPS</th></tr></thead><tbody>
    ${v.history.map((f) => `<tr class="${f.short ? "muted" : ""}" title="${f.short ? "Under 5 s" : ""}"><td>${esc(f.label)}</td>
      <td class="r num">${clock(f.seconds)}</td><td class="r num">${short(f.dps)}</td></tr>`).join("")}</tbody></table>`;
}

function taken(v) {
  const f = v.current;
  const head = `<div class="note">This fight: ${short(f ? f.taken : 0)} taken${f && f.friendly ? ` · ${short(f.friendly)} friendly fire dealt` : ""}. Session: ${short(v.session.taken)}.</div>`;
  if (!v.recap.length) return head;
  return head + `<table><thead><tr><th>Latest hits on you</th><th class="r">Damage</th></tr></thead><tbody>
    ${v.recap.map((r) => `<tr><td>${r.seconds_ago.toFixed(1)} s ago</td><td class="r num">${short(r.damage)}</td></tr>`).join("")}</tbody></table>`;
}

let last = "";
let lastView = null;
function render(v) {
  lastView = v;
  const key = JSON.stringify(v) + ui.tab;
  if (key === last) return;
  last = key;
  for (const b of document.querySelectorAll("nav button")) {
    b.classList.toggle("on", b.dataset.tab === ui.tab);
    b.setAttribute("aria-selected", b.dataset.tab === ui.tab);
  }
  $("#dot").classList.toggle("on", !!v.in_combat);
  if (v.error) {
    $("#fight").textContent = "Damage meter";
    $("#foot").innerHTML = `<span class="err">${esc(v.error)}</span>`;
    return;
  }
  const f = v.current;
  $("#fight").textContent = f ? f.label : v.log ? "Waiting for your first hit…" : "Waiting for the combat log…";
  $("#timer").textContent = f ? clock(f.seconds) : "";
  $("#dps").textContent = f ? short(f.dps) : "–";
  $("#now").textContent = f && f.current_dps != null ? `now ${short(f.current_dps)}/s` : v.in_combat ? "" : f ? "fight over" : "";
  $("#line").textContent = f ? `${short(f.damage)} dmg · ${f.crit_pct.toFixed(0)}% crit · top ${short(f.biggest)}` : "";
  const body = { damage: () => (f && f.players && f.players.length ? players(f.players) : bars(f ? f.sources : [])), hits: () => hits(f ? f.programs : []), fights: () => fights(v), taken: () => taken(v) }[ui.tab];
  // in someone else's world hits come from their server (the logger's client hook); an older logger without it sees none
  const joined = v.joined && !v.rows ? `<div class="note warn">You are in someone else's world and no hits have arrived yet. If none come once you fight, the combat logger running in the game is an older one: restart the game with the planner open.</div>` : "";
  $("#body").innerHTML = joined + body();
  if (ui.tab !== "damage") hover(null);
  const plan = v.planner_dps;
  const cmp = plan && f && f.dps ? ` · this fight ran at ${((f.dps / plan) * 100).toFixed(0)}% of it` : "";
  $("#foot").innerHTML = `${plan ? `Planner: ${short(plan)} DPS for the open build${cmp}` : "Open a build in the planner to compare."}
    <div class="muted">${OVERLAY ? "Drag the title to move · the corner to resize · F6 tab · F9 hide" : "Drag the title to move"}</div>`;
}

async function tick() {
  try { render(await invoke("meter_state")); } catch (e) { render({ error: String(e), rows: 0, history: [], recap: [], session: {} }); }
}

function setTab(tab) {
  ui.tab = tab;
  save();
  last = "";
  tick();
}
// the breakdown: hovering a player's bar shows it beside the pointer, leaving the bar hides it
$("#body").addEventListener("mousemove", (e) => {
  const b = e.target.closest("[data-player]");
  hover(b ? Number(b.dataset.player) : null, e.screenX, e.screenY);
});
$("#body").addEventListener("mouseleave", () => hover(null));
window.addEventListener("blur", () => hover(null));
$("#tabs").addEventListener("click", (e) => {
  const b = e.target.closest("[data-tab]");
  if (b) setTab(b.dataset.tab);
});
const FADES = [0.80, 0.55, 0.30, 0.95];
function applyFade() { document.documentElement.style.setProperty("--bg", `rgba(13, 11, 9, ${FADES[ui.fade % FADES.length]})`); }
function nextFade() { ui.fade = (ui.fade + 1) % FADES.length; save(); applyFade(); }
$("#fade").addEventListener("click", nextFade);
const TABS = ["damage", "hits", "fights", "taken"];
// from the planner's hotkeys: "tab" (next tab), "fade" (next see-through level)
window.overlayCmd = (c) => {
  if (c === "tab") setTab(TABS[(TABS.indexOf(ui.tab) + 1) % TABS.length]);
  if (c === "fade") nextFade();
};
// a click from the game at a point of the meter, in CSS pixels
window.overlayClick = (x, y) => {
  const el = document.elementFromPoint(x, y);
  if (el) el.click();
};
// the pointer from the game (in CSS pixels; -1, -1 when it left the meter): opens and closes the breakdown like a hover
window.overlayHover = (x, y) => {
  const el = x < 0 ? null : document.elementFromPoint(x, y);
  const bar = el && el.closest("[data-player]");
  hover(bar ? Number(bar.dataset.player) : null);
};
// the wheel from the game: scroll the list under the point
window.overlayWheel = (x, y, dy) => {
  const el = document.elementFromPoint(x, y);
  const box = el && el.closest("main");
  if (box) box.scrollTop += dy;
};
$("#close").addEventListener("click", () => window.__TAURI__.window.getCurrentWindow().close());
applyFade();
tick();
setInterval(tick, 250);
