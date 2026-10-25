// The breakdown tooltip (docs/plans/all-players-meter.md): one player's damage by weapon, each weapon's hits by attack
// and element, Details-style. The planner shows it beside the pointer (its own window over the meter window, or a second
// image in the game) and calls showPlayer(id); after each change the page asks the planner to fit the window to it.
const { invoke } = window.__TAURI__.core;
const $ = (s) => document.querySelector(s);
const esc = (s) => String(s ?? "").replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);
const COLORS = ["#E8B04A", "#4FC3C8", "#B48CE0", "#89E191", "#E5534B", "#D9A06B", "#7FA7E8"];
function short(n) {
  if (n == null || !isFinite(n)) return "–";
  const a = Math.abs(n);
  return a >= 1e6 ? (n / 1e6).toFixed(2) + "M" : a >= 1e4 ? (n / 1e3).toFixed(1) + "k" : a >= 100 ? n.toFixed(0) : n.toFixed(1);
}

// friendly names and icons for attacks and weapons (data/attack_names.json), loaded once
let NAMES = { programs: {}, weapons: {} };
invoke("attack_names").then((n) => { if (n && n.programs) { NAMES = n; last = ""; tick(); } }).catch(() => {});
const icon = (src) => src ? `<img class="ico" src="${src}" alt="">` : '<span class="ico"></span>';
// the shared projectile hit (0x061f81ee) named for the weapon that fired it
const SHOT = { bow: "Arrow", wand: "Wand Bolt", staff: "Staff Spell" };
function attackName(r, weapon) {
  // the arrow or spell itself when the log names it ("Eternal Frost Arrow I")
  const ammo = r.ammo != null && NAMES.ammo && NAMES.ammo[String(r.ammo)];
  if (ammo && ammo.name) return ammo.name;
  if (r.program === 0x061f81ee) {
    const k = Object.keys(SHOT).find((w) => weapon.toLowerCase().includes(w));
    if (k) return SHOT[k];
  }
  const n = r.program != null && NAMES.programs[String(r.program)];
  const name = n && n.name ? n.name : r.name.split(" · ")[0];
  // short and without the element: "Wand Heavy / Evasion Attack (Fire)" is "Wand Heavy"
  return name.replace(/ \/ Evasion Attack/, "").replace(/ \((Blunt|Ice|Fire|Shroud|Lightning|Poison|Fog|Physical|Cut|Pierce)\)$/, "");
}

// rows that read the same once named (a heavy attack's elements) shown as one
function merged(rows, weapon) {
  const out = new Map();
  for (const r of rows) {
    const name = attackName(r, weapon);
    const m = out.get(name);
    if (!m) { out.set(name, { ...r, name, crits: r.crit_pct * r.hits / 100 }); continue; }
    m.damage += r.damage; m.hits += r.hits; m.crits += r.crit_pct * r.hits / 100; m.share += r.share;
    m.biggest = Math.max(m.biggest, r.biggest);
  }
  return [...out.values()].map((m) => ({ ...m, avg: m.hits ? m.damage / m.hits : 0, crit_pct: m.hits ? m.crits / m.hits * 100 : 0 }))
    .sort((a, b) => b.damage - a.damage);
}

let player = null;
let last = "";
let size = "";

function render(p) {
  const tip = $("#tip");
  const key = JSON.stringify(p);
  if (key === last) return;
  last = key;
  if (!p) { tip.hidden = true; tip.innerHTML = ""; return; }
  tip.hidden = false;
  tip.innerHTML = `<div class="thead"><b>${esc(p.name)}</b><span class="num">${short(p.damage)} · ${short(p.dps)}/s · ${p.crit_pct.toFixed(0)}% crit</span></div>`
    + p.weapons.map((w, k) => {
      const rows = merged(w.rows, w.name);
      const top = Math.max(...rows.map((r) => r.damage), 1);
      const wn = NAMES.weapons[String(w.weapon)] || {};
      return `<div class="weapon"><div class="wname">${icon(wn.icon)}<span class="wt">${esc(w.name)}</span><span class="num">${short(w.damage)} · ${(w.share * 100).toFixed(0)}%</span></div>
        ${rows.map((r) => {
          const pn = (r.program != null && NAMES.programs[String(r.program)]) || {};
          return `<div class="line">
          <div class="fill" style="width:${(r.damage / top) * 100}%;background:${COLORS[k % COLORS.length]}"></div>
          ${icon(pn.icon || wn.icon)}<span class="n">${esc(r.name)}</span><span class="v num">${short(r.damage)}</span>
          <span class="m num">${r.hits}×</span><span class="m num">${short(r.avg)}</span><span class="m num">${r.crit_pct.toFixed(0)}%</span></div>`;
        }).join("")}</div>`;
    }).join("")
    + `<div class="foot">damage · hits · average · crit</div>`;
  fit();
}

function fit() {
  const r = $("#tip").getBoundingClientRect();
  const s = `${Math.ceil(r.width)}x${Math.ceil(r.height)}`;
  if (s === size || !r.width) return;
  size = s;
  invoke("tip_fit", { w: Math.ceil(r.width), h: Math.ceil(r.height) }).catch(() => {});
}

async function tick() {
  if (player == null) return render(null);
  try {
    const v = await invoke("meter_state");
    render(v.current && v.current.players ? v.current.players.find((x) => x.id === player) : null);
  } catch (_) {}
}

// from the planner: whose breakdown to show (null: none)
window.showPlayer = (id) => {
  player = id;
  last = "";
  tick();
};
setInterval(tick, 250);
