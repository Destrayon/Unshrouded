//! Live damage meter over a combat log the recorder is still writing (docs/plans/live-meter.md). The design follows
//! Details! Damage Meter where it fits our data (docs/research/details-meter.md, "What an Enshrouded meter should
//! adopt"): fights split by an idle gap and ended at their last hit, damage credited to its owner through
//! `permission` with a row per summon, a 5 s rolling DPS from the hits' own timestamps, a per-program table with
//! normal and crit hits apart, friendly fire booked apart, damage taken and a recent-hits recap, and a session total
//! over fight time only. Every player is followed, not only you (docs/plans/all-players-meter.md): a fight is any
//! player's combat, and each player gets a bar with a breakdown: each weapon, its hits split by attack (the hit's
//! program: base hit, Strong Shot, a weapon special, …) and damage element (the arrow's element stands in for the
//! ammo: the game records no arrow id). Hits that did no damage are left out of it.

use crate::combat_log::{self, LogHit};
use ens_data::Bundle;
use serde::Serialize;
use std::collections::{BTreeMap, VecDeque};

/// No hit by or on you (or your summons) for this long ends a fight.
pub const IDLE_MS: u64 = 8_000;
/// Window of the rolling "current DPS".
pub const WINDOW_MS: u64 = 5_000;
/// A fight shorter than this is kept but marked short.
pub const SHORT_MS: u64 = 5_000;
/// Finished fights kept.
pub const HISTORY: usize = 25;
/// Hits taken kept for the recap.
pub const RECAP: usize = 32;

/// `keen::FactionType` values of the player's side (combat_log::FACTIONS): Player 7, AutomatedPlayer 8,
/// PlayerAlly 11. A hit on these is friendly fire, not damage done.
const FRIENDLY: [u8; 3] = [7, 8, 11];

/// The lines of a growing CSV: keeps the header and any line not finished yet, and parses only complete rows.
#[derive(Default)]
pub struct Tail {
    header: Option<String>,
    partial: String,
}

impl Tail {
    pub fn feed(&mut self, chunk: &str) -> Vec<LogHit> {
        self.partial.push_str(chunk);
        let Some(cut) = self.partial.rfind('\n') else { return vec![] };
        let done: String = self.partial.drain(..=cut).collect();
        let mut lines = done.lines();
        if self.header.is_none() {
            match lines.next() {
                Some(h) => self.header = Some(h.to_string()),
                None => return vec![],
            }
        }
        let body: Vec<&str> = lines.filter(|l| !l.trim().is_empty()).collect();
        if body.is_empty() {
            return vec![];
        }
        let text = format!("{}\n{}\n", self.header.as_deref().unwrap_or(""), body.join("\n"));
        let mut hits = combat_log::parse(&text).unwrap_or_default();
        // rows of one flush can arrive out of order
        hits.sort_by_key(|h| h.ms);
        hits
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct HitStats {
    pub count: u32,
    pub sum: f64,
    pub min: f64,
    pub max: f64,
}

impl HitStats {
    fn add(&mut self, v: f64) {
        if self.count == 0 || v < self.min {
            self.min = v;
        }
        self.max = self.max.max(v);
        self.count += 1;
        self.sum += v;
    }
    pub fn avg(&self) -> f64 {
        if self.count > 0 { self.sum / self.count as f64 } else { 0.0 }
    }
}

/// One damage program (our "spell"): normal and crit hits apart, as the calculator prices them.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Program {
    pub normal: HitStats,
    pub crit: HitStats,
    pub backstabs: u32,
    pub weak_spots: u32,
    pub weapon_category: u32,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Source {
    pub damage: f64,
    pub hits: u32,
    pub crits: u32,
    pub summon: bool,
}

/// A share of one player's damage: one weapon, one attack or one projectile.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Part {
    pub damage: f64,
    pub hits: u32,
    pub crits: u32,
    pub biggest: f64,
}

impl Part {
    fn merge(&mut self, o: &Part) {
        self.damage += o.damage;
        self.hits += o.hits;
        self.crits += o.crits;
        self.biggest = self.biggest.max(o.biggest);
    }

    fn add(&mut self, amount: f64, crit: bool) {
        self.damage += amount;
        self.hits += 1;
        self.crits += crit as u32;
        self.biggest = self.biggest.max(amount);
    }
}

/// One player's damage in a fight, with its breakdown.
#[derive(Debug, Clone, Default)]
pub struct PlayerFight {
    pub total: Part,
    /// By weapon category ("w:<id>"), summons together ("summons").
    pub weapons: BTreeMap<String, Part>,
    /// By weapon, attack (program), element mask and ammunition item (0: unknown).
    pub detail: BTreeMap<(String, u32, u16, u32), Part>,
}

/// One fight: any player's hits (and hits on them) separated from the next by more than IDLE_MS.
#[derive(Debug, Clone, Default)]
pub struct Fight {
    pub start_ms: u64,
    pub end_ms: u64,
    pub start_unix: u64,
    pub end_unix: u64,
    /// Sum of the per-type amounts on enemies: the damage the calculator predicts.
    pub damage: f64,
    /// Health actually removed from enemies (health_change).
    pub effective: f64,
    pub hits: u32,
    pub crits: u32,
    pub biggest: f64,
    /// Your weapon hits by weapon category ("w:<id>"), your summons by entity ("s:<id>").
    pub sources: BTreeMap<String, Source>,
    pub programs: BTreeMap<u32, Program>,
    /// The fight's number in the session, from 1.
    pub number: u32,
    pub taken: f64,
    pub friendly: f64,
    recent: VecDeque<(u64, f64)>,
    /// Every player's damage on enemies, by player entity.
    pub players: BTreeMap<u32, PlayerFight>,
}

impl Fight {
    fn duration_ms(&self) -> u64 {
        self.end_ms.saturating_sub(self.start_ms)
    }
}

/// The meter: feed it the log as it grows, read a view whenever the window redraws.
#[derive(Default)]
pub struct Meter {
    tail: Tail,
    me: Option<u32>,
    weapon_hits: BTreeMap<u32, u32>,
    open: Option<Fight>,
    fights: VecDeque<Fight>,
    /// Session totals over every fight (fight time only).
    total_damage: f64,
    total_ms: u64,
    total_taken: f64,
    total_fights: u32,
    last_ms: u64,
    last_unix: u64,
    recap: VecDeque<(u64, u32, f64)>,
    rows: u64,
    /// Players seen so far (attackers that hit with a weapon of their own), in the order they first appeared.
    players: Vec<u32>,
    /// The names the recorder read from the game (names column), by entity id.
    names: BTreeMap<u32, String>,
    /// The local player as the game says (names column), over the most-weapon-hits guess.
    local: Option<u32>,
}

impl Meter {
    /// Feed newly written log text; returns how many rows it held.
    pub fn feed(&mut self, chunk: &str) -> usize {
        let hits = self.tail.feed(chunk);
        let n = hits.len();
        for h in hits {
            self.add(h);
        }
        n
    }

    /// The player: the attacker with the most weapon hits so far (enemies' hits carry no weapon category).
    fn note_attacker(&mut self, h: &LogHit) {
        if h.weapon_category != 0 && h.permission == h.attacker {
            *self.weapon_hits.entry(h.attacker).or_default() += 1;
            self.me = self.local.or_else(|| self.weapon_hits.iter().max_by_key(|(_, n)| **n).map(|(a, _)| *a));
            if !self.players.contains(&h.attacker) {
                self.players.push(h.attacker);
            }
        }
    }

    /// The player a hit is credited to: its attacker, or the owner of the summon or projectile that made it.
    fn player_of(&self, h: &LogHit) -> Option<u32> {
        [h.attacker, h.permission].into_iter().find(|e| *e != 0 && self.players.contains(e))
    }

    fn add(&mut self, h: LogHit) {
        self.rows += 1;
        if let Some(cell) = h.extra.get("names").filter(|c| !c.is_empty()) {
            for (id, name, me) in combat_log::player_names(cell) {
                if me {
                    (self.local, self.me) = (Some(id), Some(id));
                }
                self.names.insert(id, name);
            }
        }
        self.note_attacker(&h);
        let Some(me) = self.me else { return };
        let mine = h.attacker == me || h.permission == me;
        let on_me = h.root_target == me || h.target == me;
        let player = self.player_of(&h);
        let on_player = self.players.contains(&h.root_target) || self.players.contains(&h.target);
        if !mine && !on_me && player.is_none() && !on_player {
            return;
        }
        if self.open.as_ref().is_some_and(|f| h.ms > f.end_ms + IDLE_MS) {
            self.close();
        }
        let unix = h.unix_ms.unwrap_or(0);
        let number = self.total_fights + 1;
        let f = self.open.get_or_insert_with(|| Fight { start_ms: h.ms, end_ms: h.ms, start_unix: unix, end_unix: unix, number, ..Default::default() });
        f.end_ms = f.end_ms.max(h.ms);
        f.end_unix = f.end_unix.max(unix);
        self.last_ms = self.last_ms.max(h.ms);
        self.last_unix = self.last_unix.max(unix);
        // a client-hook row has no per-type amounts: its health change is the damage
        let amount: f64 = if h.from_client() { h.damage() } else { h.types.iter().sum() };
        // every player's damage on enemies, with its breakdowns
        if let Some(pl) = player.filter(|_| !on_player && !FRIENDLY.contains(&h.target_faction) && amount > 0.0) {
            let pf = f.players.entry(pl).or_default();
            pf.total.add(amount, h.crit);
            let weapon = if h.attacker != pl { "summons".to_string() } else { format!("w:{}", h.weapon_category) };
            pf.weapons.entry(weapon.clone()).or_default().add(amount, h.crit);
            pf.detail.entry((weapon, h.program, h.elements(), h.ammo_item())).or_default().add(amount, h.crit);
        }
        if !mine && !on_me {
            return;
        }
        if mine && h.root_target != me {
            if FRIENDLY.contains(&h.target_faction) {
                f.friendly += amount;
                return;
            }
            f.damage += amount;
            f.effective += h.damage();
            f.hits += 1;
            f.crits += h.crit as u32;
            f.biggest = f.biggest.max(amount);
            f.recent.push_back((h.ms, amount));
            while f.recent.front().is_some_and(|(t, _)| *t + WINDOW_MS <= h.ms) {
                f.recent.pop_front();
            }
            let summon = h.attacker != me;
            let key = if summon { format!("s:{}", h.source) } else { format!("w:{}", h.weapon_category) };
            let s = f.sources.entry(key).or_default();
            s.damage += amount;
            s.hits += 1;
            s.crits += h.crit as u32;
            s.summon = summon;
            if !summon {
                let p = f.programs.entry(h.program).or_default();
                p.weapon_category = h.weapon_category;
                if h.crit { p.crit.add(amount) } else { p.normal.add(amount) }
                p.backstabs += h.backstab as u32;
                p.weak_spots += h.skill_shot as u32;
            }
        } else if on_me {
            let taken = h.damage();
            f.taken += taken;
            self.recap.push_back((h.ms, h.attacker, taken));
            if self.recap.len() > RECAP {
                self.recap.pop_front();
            }
        }
    }

    fn close(&mut self) {
        if let Some(f) = self.open.take() {
            self.total_damage += f.damage;
            self.total_ms += f.duration_ms();
            self.total_taken += f.taken;
            self.total_fights += 1;
            self.fights.push_front(f);
            self.fights.truncate(HISTORY);
        }
    }

    /// What the window shows at wall-clock time `now_unix_ms`. Names come from the game data.
    pub fn view(&self, b: &Bundle, now_unix_ms: u64) -> View {
        let idle = self.last_unix > 0 && now_unix_ms.saturating_sub(self.last_unix) > IDLE_MS;
        // the log's own clock now: its last row plus the wall-clock time since that row
        let now_ms = self.last_ms + now_unix_ms.saturating_sub(self.last_unix).min(60_000);
        let current = self.open.as_ref().map(|f| fight_view(b, f, Some(now_ms), self.me, &self.players, &self.names))
            .or_else(|| self.fights.front().map(|f| fight_view(b, f, None, self.me, &self.players, &self.names)));
        let open_dmg = self.open.as_ref().map(|f| f.damage).unwrap_or(0.0);
        let open_ms = self.open.as_ref().map(|f| f.duration_ms()).unwrap_or(0);
        let session_ms = self.total_ms + open_ms;
        View {
            in_combat: self.open.is_some() && !idle,
            me: self.me,
            rows: self.rows,
            current,
            history: self.fights.iter().map(|f| summary(b, f)).collect(),
            session: Session {
                fights: self.total_fights + self.open.is_some() as u32,
                seconds: session_ms as f64 / 1000.0,
                damage: self.total_damage + open_dmg,
                dps: (self.total_damage + open_dmg) / (session_ms.max(1000) as f64 / 1000.0),
                taken: self.total_taken + self.open.as_ref().map(|f| f.taken).unwrap_or(0.0),
            },
            recap: self.recap.iter().rev().take(8).map(|(ms, a, d)| Taken { seconds_ago: now_ms.saturating_sub(*ms) as f64 / 1000.0, attacker: *a, damage: *d }).collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct View {
    pub in_combat: bool,
    pub me: Option<u32>,
    pub rows: u64,
    /// The open fight, else the last one.
    pub current: Option<FightView>,
    pub history: Vec<FightSummary>,
    pub session: Session,
    /// Your latest hits taken, newest first.
    pub recap: Vec<Taken>,
}

/// One line of a player's breakdown.
#[derive(Debug, Clone, Serialize)]
pub struct PartRow {
    pub name: String,
    /// The attack (program id) and the damage elements by name, for the tooltip's icon and tags; None and empty for
    /// rows that are not one attack.
    pub program: Option<u32>,
    pub elements: Vec<String>,
    /// The ammunition item (arrow or spell) when the log has it, for its name (planner/data/ammo.json).
    pub ammo: Option<u32>,
    pub damage: f64,
    pub dps: f64,
    pub share: f64,
    pub hits: u32,
    pub crit_pct: f64,
    pub avg: f64,
    pub biggest: f64,
}

/// One weapon of a player's breakdown, with its hits by attack and element.
#[derive(Debug, Clone, Serialize)]
pub struct WeaponGroup {
    pub name: String,
    /// The weapon category id (0 for summons), for the tooltip's icon.
    pub weapon: u32,
    pub damage: f64,
    pub dps: f64,
    pub share: f64,
    pub hits: u32,
    pub crit_pct: f64,
    pub rows: Vec<PartRow>,
}

/// One player's bar and its breakdown.
#[derive(Debug, Clone, Serialize)]
pub struct PlayerRow {
    pub id: u32,
    /// "You", or "Player 2", "Player 3", … in the order they first hit something.
    pub name: String,
    pub me: bool,
    pub damage: f64,
    pub dps: f64,
    pub share: f64,
    pub hits: u32,
    pub crit_pct: f64,
    pub biggest: f64,
    /// Each weapon (summons together), most damage first, its hits by attack and element.
    pub weapons: Vec<WeaponGroup>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Session {
    pub fights: u32,
    pub seconds: f64,
    pub damage: f64,
    pub dps: f64,
    pub taken: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Taken {
    pub seconds_ago: f64,
    pub attacker: u32,
    pub damage: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceRow {
    pub name: String,
    pub summon: bool,
    pub damage: f64,
    pub dps: f64,
    pub share: f64,
    pub hits: u32,
    pub crit_pct: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProgramRow {
    pub name: String,
    pub weapon: String,
    pub hits: u32,
    pub avg_normal: f64,
    pub avg_crit: f64,
    pub min: f64,
    pub max: f64,
    pub crit_pct: f64,
    pub backstabs: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct FightView {
    pub label: String,
    pub seconds: f64,
    pub short: bool,
    pub damage: f64,
    pub effective: f64,
    pub dps: f64,
    /// Damage of the last WINDOW_MS ÷ its length (only while the fight is open).
    pub current_dps: Option<f64>,
    pub hits: u32,
    pub crit_pct: f64,
    pub biggest: f64,
    pub taken: f64,
    pub friendly: f64,
    pub sources: Vec<SourceRow>,
    pub programs: Vec<ProgramRow>,
    /// Every player, most damage first.
    pub players: Vec<PlayerRow>,
    /// All players' damage per second together.
    pub group_dps: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct FightSummary {
    pub label: String,
    pub seconds: f64,
    pub damage: f64,
    pub dps: f64,
    pub short: bool,
}

/// Whether a weapon category takes ammo: any of its items names an ammo type (game data: Bow → Arrow, Staff → Spell).
fn takes_ammo(b: &Bundle, category: u32) -> bool {
    b.items.values().any(|it| {
        it.ammo.as_deref().is_some_and(|a| a != "Invalid")
            && it.weapon_category.as_deref().and_then(ens_data::save::item_id) == Some(category)
    })
}

fn weapon_name(b: &Bundle, id: u32) -> String {
    b.weapon_categories.iter().find(|(g, _)| ens_data::save::item_id(g) == Some(id)).map(|(_, c)| c.name.clone())
        .unwrap_or_else(|| if id == 0 { "Other".into() } else { format!("Weapon {id:#x}") })
}

/// A fight's name: its number in the session (the log has no reliable enemy name, level or health).
fn label(f: &Fight) -> String {
    format!("Fight {}", f.number)
}

fn secs(f: &Fight) -> f64 {
    f.duration_ms().max(1_000) as f64 / 1000.0
}

fn summary(_b: &Bundle, f: &Fight) -> FightSummary {
    FightSummary { label: label(f), seconds: f.duration_ms() as f64 / 1000.0, damage: f.damage, dps: f.damage / secs(f), short: f.duration_ms() < SHORT_MS }
}

fn program_name(id: u32) -> String {
    combat_log::program_label(id).map(str::to_string).unwrap_or_else(|| format!("program {id:#x}"))
}

fn part_rows(parts: impl Iterator<Item = (String, Part)>, total: f64, s: f64) -> Vec<PartRow> {
    let mut rows: Vec<PartRow> = parts.map(|(name, p)| PartRow {
        name, program: None, elements: vec![], ammo: None, damage: p.damage, dps: p.damage / s, share: if total > 0.0 { p.damage / total } else { 0.0 }, hits: p.hits,
        crit_pct: if p.hits > 0 { p.crits as f64 / p.hits as f64 * 100.0 } else { 0.0 },
        avg: if p.hits > 0 { p.damage / p.hits as f64 } else { 0.0 }, biggest: p.biggest,
    }).collect();
    rows.sort_by(|a, b| b.damage.total_cmp(&a.damage));
    rows
}

fn player_rows(b: &Bundle, f: &Fight, me: Option<u32>, order: &[u32], names: &BTreeMap<u32, String>) -> Vec<PlayerRow> {
    let s = secs(f);
    let group: f64 = f.players.values().map(|p| p.total.damage).sum();
    let mut rows: Vec<PlayerRow> = f.players.iter().map(|(id, p)| {
        let t = p.total.damage;
        let n = order.iter().filter(|e| Some(**e) != me).position(|e| e == id).map_or(0, |k| k + 2);
        PlayerRow {
            id: *id, name: names.get(id).cloned().unwrap_or_else(|| if Some(*id) == me { "You".into() } else { format!("Player {n}") }), me: Some(*id) == me,
            damage: t, dps: t / s, share: if group > 0.0 { t / group } else { 0.0 }, hits: p.total.hits,
            crit_pct: if p.total.hits > 0 { p.total.crits as f64 / p.total.hits as f64 * 100.0 } else { 0.0 }, biggest: p.total.biggest,
            weapons: {
                let mut groups: Vec<WeaponGroup> = p.weapons.iter().map(|(k, w)| WeaponGroup {
                    name: match k.split_once(':') {
                        Some(("w", id)) => weapon_name(b, id.parse().unwrap_or(0)),
                        _ => "Summons".into(),
                    },
                    weapon: k.strip_prefix("w:").and_then(|id| id.parse().ok()).unwrap_or(0),
                    damage: w.damage, dps: w.damage / s, share: if t > 0.0 { w.damage / t } else { 0.0 }, hits: w.hits,
                    crit_pct: if w.hits > 0 { w.crits as f64 / w.hits as f64 * 100.0 } else { 0.0 },
                    rows: {
                        // one row per attack; for weapons that take ammo (bows: Arrow, staffs: Spell) the ammo apart by
                        // element (the hit's element is the ammo's). Other weapons' elements are their own, so their rows
                        // are not split by element.
                        let bow = k.strip_prefix("w:").and_then(|id| id.parse().ok()).is_some_and(|id| takes_ammo(b, id));
                        // the ammo item when the log names it, else its element stands in
                        let mut merged: BTreeMap<(u32, u16, u32), Part> = BTreeMap::new();
                        for ((_, prog, el, ammo), v) in p.detail.iter().filter(|((wk, _, _, _), _)| wk == k) {
                            let key = match (bow, *ammo) { (false, _) => (*prog, 0, 0), (true, 0) => (*prog, *el, 0), (true, a) => (*prog, 0, a) };
                            merged.entry(key).or_default().merge(v);
                        }
                        let label = |prog: u32, el: u16, ammo: u32| {
                            let e = combat_log::element_name(el);
                            match (ammo, e.is_empty()) {
                                (0, true) => program_name(prog),
                                (0, false) => format!("{} · {e}", program_name(prog)),
                                (a, _) => format!("{} · ammo {a}", program_name(prog)),
                            }
                        };
                        let mut rows = part_rows(merged.iter().map(|((prog, el, a), v)| (label(*prog, *el, *a), v.clone())), t, s);
                        // part_rows sorts by damage: give each row its attack, ammo and (for unnamed ammo) elements back
                        for r in rows.iter_mut() {
                            if let Some(((prog, el, a), _)) = merged.iter().find(|((prog, el, a), _)| r.name == label(*prog, *el, *a)) {
                                r.program = Some(*prog);
                                r.ammo = (*a != 0).then_some(*a);
                                r.elements = combat_log::element_name(*el).split(" + ").filter(|x| !x.is_empty()).map(str::to_string).collect();
                            }
                        }
                        rows
                    },
                }).collect();
                groups.sort_by(|a, b| b.damage.total_cmp(&a.damage));
                groups
            },
        }
    }).collect();
    rows.sort_by(|a, b| b.damage.total_cmp(&a.damage));
    rows
}

fn fight_view(b: &Bundle, f: &Fight, now_ms: Option<u64>, me: Option<u32>, order: &[u32], names: &BTreeMap<u32, String>) -> FightView {
    let s = secs(f);
    let mut sources: Vec<SourceRow> = f.sources.iter().map(|(k, x)| {
        let name = match k.split_once(':') {
            Some(("w", id)) => weapon_name(b, id.parse().unwrap_or(0)),
            Some(("s", id)) => format!("Summon #{id}"),
            _ => k.clone(),
        };
        SourceRow { name, summon: x.summon, damage: x.damage, dps: x.damage / s, share: if f.damage > 0.0 { x.damage / f.damage } else { 0.0 },
            hits: x.hits, crit_pct: if x.hits > 0 { x.crits as f64 / x.hits as f64 * 100.0 } else { 0.0 } }
    }).collect();
    sources.sort_by(|a, b| b.damage.total_cmp(&a.damage));
    let mut programs: Vec<ProgramRow> = f.programs.iter().map(|(id, p)| {
        let hits = p.normal.count + p.crit.count;
        ProgramRow {
            name: program_name(*id),
            weapon: weapon_name(b, p.weapon_category), hits, avg_normal: p.normal.avg(), avg_crit: p.crit.avg(),
            min: if p.normal.count > 0 { p.normal.min } else { p.crit.min }, max: p.normal.max.max(p.crit.max),
            crit_pct: if hits > 0 { p.crit.count as f64 / hits as f64 * 100.0 } else { 0.0 }, backstabs: p.backstabs,
        }
    }).collect();
    programs.sort_by(|a, b| (b.avg_normal * b.hits as f64).total_cmp(&(a.avg_normal * a.hits as f64)));
    let current_dps = now_ms.map(|now| f.recent.iter().filter(|(t, _)| *t + WINDOW_MS > now).map(|(_, d)| d).sum::<f64>() / (WINDOW_MS as f64 / 1000.0));
    FightView {
        label: label(f), seconds: f.duration_ms() as f64 / 1000.0, short: f.duration_ms() < SHORT_MS, damage: f.damage, effective: f.effective,
        dps: f.damage / s, current_dps, hits: f.hits, crit_pct: if f.hits > 0 { f.crits as f64 / f.hits as f64 * 100.0 } else { 0.0 },
        biggest: f.biggest, taken: f.taken, friendly: f.friendly, sources, programs,
        players: player_rows(b, f, me, order, names), group_dps: f.players.values().map(|p| p.total.damage).sum::<f64>() / s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundle() -> Bundle {
        Bundle::load(concat!(env!("CARGO_MANIFEST_DIR"), "/../../data/bundle.json")).unwrap()
    }

    const HEAD: &str = "ms,entity_a,entity_b,health_change,stamina_change,flags,crit,backstab,weak_spot,type0,type1,type2,type3,type4,type5,\
type6,type7,type8,type9,type10,type11,record,blocked,no_damage,loss_x10,unix_ms,attacker_level,target_level,target_health_max,root_source,\
target_id,root_target,source,permission,weapon_category,weapon_pide,program,material,attack_category,target_faction";

    /// A compact row: attacker, target, permission, weapon category, cut damage, crit, ms, target faction.
    fn row(ms: u64, attacker: u32, target: u32, permission: u32, weapon: u32, dmg: f64, crit: bool, faction: u8) -> String {
        format!("{ms},{attacker},{target},-{},0,0x0,{crit},false,false,{dmg},0,0,0,0,0,0,0,0,0,0,0,,false,false,0,{},0x13,0xa,0x64,{attacker},{target},{target},{attacker},\
{permission},{weapon},0,102728174,0,1,{faction}\n", dmg.round(), 1_792_700_000_000 + ms)
    }

    #[test]
    fn fights_split_by_idle_gap_and_end_at_their_last_hit() {
        let b = bundle();
        let mut m = Meter::default();
        let mut text = format!("{HEAD}\n");
        // fight 1: 4 hits of 50 over 3 s; fight 2 starts 10 s after the last hit
        for k in 0..4 {
            text += &row(1_000 + k * 1_000, 1, 77, 1, 0x1573ffb0, 50.0, k == 3, 2);
        }
        text += &row(14_000, 1, 78, 1, 0x1573ffb0, 30.0, false, 2);
        // arrives in two chunks, the first cut mid-line
        let cut = text.len() - 20;
        assert_eq!(m.feed(&text[..cut]), 4);
        assert_eq!(m.feed(&text[cut..]), 1);
        let v = m.view(&b, 1_792_700_000_000 + 14_000);
        assert_eq!(v.history.len(), 1);
        let f1 = &v.history[0];
        assert_eq!((f1.seconds, f1.damage), (3.0, 200.0));
        assert!((f1.dps - 200.0 / 3.0).abs() < 1e-9 && f1.short);
        let cur = v.current.unwrap();
        assert_eq!((cur.damage, cur.hits), (30.0, 1));
        assert!(v.in_combat);
        assert_eq!(v.session.fights, 2);
        assert_eq!((f1.label.as_str(), cur.label.as_str()), ("Fight 1", "Fight 2"), "fights are named by number");
        assert!((v.session.damage - 230.0).abs() < 1e-9 && (v.session.seconds - 3.0).abs() < 1e-9, "fight time only");
    }

    #[test]
    fn summons_credit_their_owner_and_friendly_fire_is_apart() {
        let b = bundle();
        let mut m = Meter::default();
        let mut text = format!("{HEAD}\n");
        text += &row(1_000, 1, 77, 1, 0x1573ffb0, 40.0, false, 2);
        text += &row(1_500, 9914, 77, 1, 0, 25.0, false, 2); // my summon, permission names me
        text += &row(2_000, 1, 5, 1, 0x1573ffb0, 10.0, false, 7); // my explosion on a player
        text += &row(2_500, 77, 1, 77, 0, 12.0, false, 7); // the enemy hits me
        m.feed(&text);
        let f = m.view(&b, 1_792_700_000_000 + 2_600).current.unwrap();
        assert_eq!((f.damage, f.friendly, f.taken), (65.0, 10.0, 12.0));
        let summon = f.sources.iter().find(|s| s.summon).unwrap();
        assert_eq!((summon.name.as_str(), summon.damage), ("Summon #9914", 25.0));
        assert!(f.sources.iter().any(|s| !s.summon && s.damage == 40.0));
        assert_eq!(f.programs.iter().map(|p| p.hits).sum::<u32>(), 1, "only your own hits on enemies are in your program table");
    }

    /// Two players and a summon: one bar each, the summon on its owner's bar, breakdowns by weapon, attack and
    /// projectile; a fight with only the other player's hits still counts.
    #[test]
    fn every_player_gets_a_bar_with_breakdowns() {
        let b = bundle();
        let bow = 4022992680u32; // a weapon category id in the real log
        let mut text = format!("{HEAD}\n");
        // you (1) hit four times, player 2 (50) twice, player 2's summon (77, permission 50) once
        for k in 0..4 {
            text += &row(1000 + k * 300, 1, 900, 1, bow, 40.0, k == 0, 2);
        }
        text += &row(1100, 50, 900, 50, bow, 30.0, false, 2);
        text += &row(1400, 50, 901, 50, bow, 30.0, true, 2);
        text += &row(1500, 77, 901, 50, 0, 25.0, false, 2);
        // a later fight where only player 2 fights
        text += &row(30_000, 50, 905, 50, bow, 60.0, false, 2);
        let mut m = Meter::default();
        m.feed(&text);
        let open = m.view(&b, 1_792_700_000_000 + 30_500).current.unwrap();
        assert_eq!(open.players.len(), 1, "player 2's fight");
        assert_eq!((open.players[0].name.as_str(), open.players[0].damage), ("Player 2", 60.0));
        let earlier = m.fights.front().unwrap();
        let rows = player_rows(&b, earlier, m.me, &m.players, &m.names);
        assert_eq!(rows.iter().map(|r| (r.name.as_str(), r.damage)).collect::<Vec<_>>(), [("You", 160.0), ("Player 2", 85.0)]);
        let p2 = &rows[1];
        assert_eq!(p2.weapons.len(), 2, "the bow and Summons");
        assert!(p2.weapons.iter().any(|w| w.name == "Summons" && w.damage == 25.0));
        assert_eq!(p2.hits, 3);
        assert!((rows[0].share - 160.0 / 245.0).abs() < 1e-9);
        // the test rows carry their damage in slot 0 (Cut) with the projectile program (0x061f81ee)
        let you = &rows[0].weapons[0];
        // not a bow in the game data: one row per attack, no element split
        assert_eq!((you.rows.len(), you.rows[0].name.as_str(), you.rows[0].hits), (1, "projectile hit", 4));
        assert_eq!((you.weapon, you.rows[0].program), (bow, Some(0x061f_81ee)));
    }

    /// Names from the recorder's names column replace "You" and "Player 2".
    #[test]
    fn players_are_named_from_the_names_column() {
        let b = bundle();
        let bow = 4022992680u32;
        let head = format!("{HEAD},origin,damage_types,names");
        let mut text = format!("{head}\n");
        text += &row(1000, 1, 900, 1, bow, 40.0, false, 2).replace('\n', ",apply,0,*1=Alice;50=Bob%2C the Builder\n");
        text += &row(1200, 50, 900, 50, bow, 30.0, false, 2).replace('\n', ",apply,0,\n");
        let mut m = Meter::default();
        m.feed(&text);
        let v = m.view(&b, 1_792_700_000_000 + 1500).current.unwrap();
        let names: Vec<&str> = v.players.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["Alice", "Bob, the Builder"]);
        assert!(v.players[0].me);
    }

    /// A bow's hits split by element (the arrow, its ammo), a staff's likewise (its Spell ammo), a wand's not, and a
    /// hit that did no damage left out.
    #[test]
    fn a_bow_splits_by_arrow_element_and_zero_damage_hits_are_left_out() {
        let b = bundle();
        // the Bow weapon category of the game data
        let bow = ens_data::save::item_id("4ceed18e62b73b4db4d4adf12013dd6a").unwrap();
        let mut text = format!("{HEAD}\n");
        text += &row(1000, 1, 900, 1, bow, 40.0, false, 2);
        // the same bow, an ice arrow: slot 4 instead of slot 0
        text += &row(1300, 1, 900, 1, bow, 30.0, false, 2).replacen(",30,0,0,0,0,", ",0,0,0,0,30,", 1);
        text += &row(1600, 1, 900, 1, 0, 0.0, false, 2);
        let mut m = Meter::default();
        m.feed(&text);
        let v = m.view(&b, 1_792_700_000_000 + 2000).current.unwrap();
        let you = &v.players[0];
        assert_eq!((you.hits, you.weapons.len()), (2, 1), "the zero-damage hit is not a hit and not a weapon");
        let names: Vec<&str> = you.weapons[0].rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["projectile hit · Cut", "projectile hit · Ice"]);
        // the same two hits with a wand: one row, the attack
        let wand = ens_data::save::item_id("b7bbeb13619ad84e865dd1954a52a19c").unwrap();
        let mut m = Meter::default();
        m.feed(&text.replace(&bow.to_string(), &wand.to_string()));
        let v = m.view(&b, 1_792_700_000_000 + 2000).current.unwrap();
        let rows: Vec<(&str, u32)> = v.players[0].weapons[0].rows.iter().map(|r| (r.name.as_str(), r.hits)).collect();
        assert_eq!(rows, [("projectile hit", 2)]);
        // with the recorder's ammo_item column the ammo itself splits the rows, whatever the element
        let with_ammo = format!("{HEAD},origin,damage_types,names,ammo_item,used_item\n")
            + &text.lines().skip(1).enumerate().map(|(k, l)| format!("{l},apply,0,,{},0\n", if k == 0 { 397_674_071 } else { 2_956_941_513u32 })).collect::<String>();
        let mut m = Meter::default();
        m.feed(&with_ammo);
        let v = m.view(&b, 1_792_700_000_000 + 2000).current.unwrap();
        let ammo: Vec<Option<u32>> = v.players[0].weapons[0].rows.iter().map(|r| r.ammo).collect();
        assert_eq!(ammo, [Some(397_674_071), Some(2_956_941_513)], "Eternal Frost Arrow I, then the Wooden Arrows");
        let staff = ens_data::save::item_id("5f54ab153da8594ab3d5a65b0fba0614").unwrap();
        let mut m = Meter::default();
        m.feed(&text.replace(&bow.to_string(), &staff.to_string()));
        let v = m.view(&b, 1_792_700_000_000 + 2000).current.unwrap();
        assert_eq!(v.players[0].weapons[0].rows.len(), 2, "a staff's Spell ammo by element");
    }

    #[test]
    fn current_dps_is_the_last_five_seconds() {
        let b = bundle();
        let mut m = Meter::default();
        let mut text = format!("{HEAD}\n");
        for k in 0..10 {
            text += &row(1_000 + k * 1_000, 1, 77, 1, 0x1573ffb0, if k < 5 { 10.0 } else { 100.0 }, false, 2);
        }
        m.feed(&text);
        let f = m.view(&b, 1_792_700_000_000 + 10_000).current.unwrap();
        assert!((f.current_dps.unwrap() - 500.0 / 5.0).abs() < 1e-9, "{:?}", f.current_dps);
        assert!((f.dps - 550.0 / 9.0).abs() < 1e-9);
        let p = &f.programs[0];
        assert_eq!((p.hits, p.avg_normal, p.min, p.max), (10, 55.0, 10.0, 100.0));
    }

    /// A real recorded session replayed through the meter in flush-sized chunks gives the same totals as one feed.
    #[test]
    #[ignore = "reads Combat Logger/combat-1792718413.csv"]
    fn real_log_replays_the_same_in_chunks() {
        let b = bundle();
        let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../Combat Logger/combat-1792718413.csv")).unwrap();
        let (mut whole, mut parts) = (Meter::default(), Meter::default());
        whole.feed(&text);
        for c in text.as_bytes().chunks(4096) {
            parts.feed(std::str::from_utf8(c).unwrap_or(""));
        }
        let (a, z) = (whole.view(&b, u64::MAX / 2), parts.view(&b, u64::MAX / 2));
        assert_eq!((a.session.fights, a.rows), (z.session.fights, z.rows));
        assert!((a.session.damage - z.session.damage).abs() < 1e-6);
        assert!(a.session.fights > 1 && a.session.damage > 0.0);
    }
}
