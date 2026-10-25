//! Combat logs from the recorder (`ens-hook`): parse the CSV, decode the copied hit event, pick out the player's hits,
//! group them by weapon type, damage types and damage program, and compare each group with the planner's hit for
//! the character in the save. See `docs/plans/combat-log-v2.md`.

use crate::{Build, Engine, Equip, Target, DAMAGE_TYPES};
use ens_data::save::{item_id, Save};
use ens_data::Bundle;
use std::collections::BTreeMap;

/// Offsets in the copied record, which is a `keen::ecs::HitEvent` (`kfc/types.json`: `GameEvent.timeStamp` 0x0,
/// `BaseHitEvent.programId` 0x58, `materialA` 0x64, `rootSourceId` 0x90, `targetId` 0x94; `HitEvent.rootTargetId`
/// 0xa0, `sourceId` 0xa4, `weaponPideId` 0xac, `healthChange` 0xbc, `weaponCategory` 0xd8, `damageTypes` 0xdc,
/// `attackCategory` 0xdf, `targetFaction` 0xf0).
pub mod record {
    pub const PROGRAM: usize = 0x58;
    pub const MATERIAL: usize = 0x64;
    pub const ROOT_SOURCE: usize = 0x90;
    pub const TARGET: usize = 0x94;
    pub const WEAPON_PIDE: usize = 0xac;
    pub const WEAPON_CATEGORY: usize = 0xd8;
    pub const ATTACK_CATEGORY: usize = 0xdf;
    pub const TARGET_FACTION: usize = 0xf0;
    /// `HitEvent.rootTargetId`: the entity a hit part belongs to (equal to targetId for a hit on the entity itself).
    pub const ROOT_TARGET: usize = 0xa0;
    /// `HitEvent.sourceId`: the entity that dealt the hit.
    pub const SOURCE: usize = 0xa4;
    /// `HitEvent.permissionSourceId`: whose hit it counts as. A summon's hits name the summon as rootSourceId and
    /// sourceId and you here (combat-1792717052.csv: two Hollow Wretches, 48 hits, permission 1).
    pub const PERMISSION: usize = 0xa8;
}

/// `keen::FactionType` (kfc/types.json), by value: the names the bundle's enemy types use.
pub const FACTIONS: [&str; 16] = ["Environment", "HitAll", "Fogger", "Vukah", "Scavenger", "Wildlife", "WildBeast", "Player",
    "AutomatedPlayer", "HitOthers", "Skeleton", "PlayerAlly", "FarmAnimal", "Cyclops", "Drak", "Ancients"];

pub fn faction_name(f: u8) -> String {
    FACTIONS.get(f as usize).map(|s| s.to_string()).unwrap_or_else(|| format!("faction {f}"))
}

/// An attribute cell written by the recorder (`attacker_level`, `target_level`, `target_health_max`): the raw 32
/// bits in hex. Attribute storage may be float or integer, so bits that make a normal float are read as one and
/// anything else (small integers are denormal floats) as an integer. None for an empty cell.
pub fn attr_value(cell: &str) -> Option<f64> {
    let bits = u32::from_str_radix(cell.trim().strip_prefix("0x")?, 16).ok()?;
    let f = f32::from_bits(bits);
    Some(if f.is_normal() && f.abs() < 1e9 { f as f64 } else { bits as f64 })
}

/// What a hit's damage program (`programId` = FNV-1a of an `ImpactProgram` GUID) belongs to, from the templates
/// that use it in the game data (kfc `keen::ecs::TemplateResource`, scanned 2026-10-21): the projectile hit shared by
/// arrows and other projectiles (123 templates, e.g. `Projectile_Player_T7_Arrow_Magic_Ice_Eternal`), the Explosive
/// Arrow blast (`Explosion_Player_Arrow_Explosive`), the generic explosion (97 templates), Strong Shot
/// (`Projectile_Player_Arrow_SpecialAbility_Strong_Shot`), and base hits and mining (`Base_Impact`,
/// `Mine_Resource_WoodAxe`, `Mine_Resource_PickAxe`).
pub fn program_label(id: u32) -> Option<&'static str> {
    Some(match id {
        0x061f_81ee => "projectile hit",
        0x0a4e_14a1 => "Explosive Arrow blast",
        0x7a12_0ee8 => "explosion",
        0x6984_940f => "Strong Shot",
        0x0a73_ea32 => "base hit",
        // weapon specials' spawned hits (bundle specialTemplates, kfc/export_specials.py)
        0x0629_0ca2 => "wand special projectile",
        0x44ee_a5bc => "wand special implosion",
        0x88f2_4735 => "sword special projectile",
        _ => return None,
    })
}

/// The recorder's names column (`id=name;id=name`, the local player's id marked `*`, separators in names written as
/// %XX): each player's entity id, name, and whether it is the local player.
pub fn player_names(cell: &str) -> Vec<(u32, String, bool)> {
    let unesc = |t: &str| {
        let mut out = String::new();
        let mut it = t.chars();
        while let Some(c) = it.next() {
            if c == '%' {
                let hex: String = it.by_ref().take(2).collect();
                out.push(u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32).unwrap_or('?'));
            } else {
                out.push(c);
            }
        }
        out
    };
    cell.split(';').filter_map(|p| p.split_once('=')).filter_map(|(id, n)| {
        let id = id.trim();
        let me = id.starts_with('*');
        Some((id.trim_start_matches('*').parse().ok()?, unesc(n), me))
    }).collect()
}

/// A hit's elements as words: "Fire + Lightning". Empty for none.
pub fn element_name(mask: u16) -> String {
    crate::DAMAGE_TYPES.iter().enumerate().filter(|(k, _)| mask & 1 << k != 0).map(|(_, t)| t.2).collect::<Vec<_>>().join(" + ")
}

/// True when a hit only damaged material (wood, stone, metal): a tree, ore or a building, not a creature.
pub fn on_material(h: &LogHit) -> bool {
    let p = h.pattern();
    !p.is_empty() && p.iter().all(|&k| k >= 9)
}

/// One logged hit.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LogHit {
    pub ms: u64,
    /// Wall-clock time (unix ms) when the recorder wrote it; logs before v2 have none.
    pub unix_ms: Option<u64>,
    pub attacker: u32,
    pub target: u32,
    pub weapon_category: u32,
    pub weapon_pide: u32,
    pub program: u32,
    pub material: u32,
    pub attack_category: u8,
    /// `keen::FactionType` of the target (index into [`FACTIONS`]); in every log, as it is part of the record.
    pub target_faction: u8,
    /// From the recorder's attribute columns (logs from 2026-10-22 on); None when absent or unreadable.
    pub attacker_level: Option<f64>,
    pub target_level: Option<f64>,
    pub target_health_max: Option<f64>,
    /// The enemy type [`identify`] names for the target (set by [`compare`]); None when not identified.
    pub target_type: Option<String>,
    /// `rootTargetId`, and for a hit on an attached part (root_target != target) the root's level and max health
    /// from other rows on the root (set by [`compare`]).
    pub root_target: u32,
    /// `sourceId`: who dealt the hit.
    pub source: u32,
    /// `permissionSourceId`: whose hit it counts as (you, for your summons' hits).
    pub permission: u32,
    pub root_level: Option<f64>,
    pub root_health_max: Option<f64>,
    /// For an Explosive Arrow blast: how many things that blast hit, this one included (set by [`count_blasts`]).
    pub blast_collisions: Option<u32>,
    pub health: i64,
    /// Damage per `keen::impact::DamageTypeNew` slot (0 Cut … 6 Lightning, 7 Healing, 8 Poison, 9 Wood, 10 Stone, 11 Metal).
    pub types: [f64; 12],
    pub crit: bool,
    pub backstab: bool,
    /// Flags 0x40 "effective": a damage type of the hit had a susceptibility factor above 1.19 (FUN_1403d84b0
    /// `if (fVar4 <= 1.19) {...} else { *param_2 = 1; }`, param_2 = local_7b5). A skill shot sets it too.
    pub effective: bool,
    /// Flags 0x100 "resisted": a damage type's factor was above 0 and below 0.81 (FUN_1403d84b0
    /// `if ((fVar4 < 0.81) && (0.0 < fVar4)) { *param_3 = 1; }`, param_3 = local_79e).
    pub resisted: bool,
    /// ...and was a skill shot on a target whose SkillShot susceptibility is above 1 (flags 0x80: ApplyDamage sets it
    /// with 0x40 only when `*(float *)(lVar22 + 8)`, attribute 0x29096433, is above 1.0; pc_ApplyDamage.c
    /// `local_79d = '';`). That is the bonus the planner models; a 0x40-only hit gets another, unmodelled one.
    pub skill_shot: bool,
    pub blocked: bool,
    /// Columns this parser does not know, by name.
    pub extra: BTreeMap<String, String>,
}

impl LogHit {
    /// Damage-type slots with damage, in slot order.
    pub fn pattern(&self) -> Vec<usize> {
        (0..12).filter(|&k| self.types[k].abs() > 1e-6).collect()
    }
    /// A hit the game's client was sent (the recorder's client hook, origin column 'client'): no per-type amounts,
    /// so it counts by its health change and is left out of measurements.
    pub fn from_client(&self) -> bool {
        self.extra.get("origin").is_some_and(|o| o == "client")
    }

    /// The damage types the hit carried, one bit per type slot (0 Cut, 1 Pierce, 2 Blunt, 3 Fire, 4 Ice, 5 Fog,
    /// 6 Lightning, 7 Poison: crate::DAMAGE_TYPES order): from the per-type amounts when the row has them, else from the
    /// recorder's damage_types column (the event's mask; assumed to use the same bit order, not yet checked against a
    /// row that has both).
    pub fn elements(&self) -> u16 {
        let from_types = (0..8).filter(|&k| self.types[k].abs() > 1e-6).fold(0u16, |m, k| m | 1 << k);
        if from_types != 0 {
            return from_types;
        }
        self.extra.get("damage_types").and_then(|v| v.parse::<u16>().ok()).unwrap_or(0) & 0xff
    }

    /// The ammunition item (arrow or spell, `ItemInfo.itemId`; planner/data/ammo.json) the recorder read for the hit;
    /// 0 when unknown (older logs, client rows, melee).
    pub fn ammo_item(&self) -> u32 {
        self.extra.get("ammo_item").and_then(|v| v.parse().ok()).unwrap_or(0)
    }

    pub fn damage(&self) -> f64 {
        (-self.health).max(0) as f64
    }
    /// "normal", "crit", "skill shot", "skill shot+crit", "backstab", "backstab+crit". An effective or resisted hit
    /// is a normal one: the susceptibility behind those marks is part of every prediction.
    pub fn kind(&self) -> &'static str {
        match (self.backstab, self.skill_shot, self.crit) {
            (true, _, false) => "backstab",
            (true, _, true) => "backstab+crit",
            (false, true, false) => "skill shot",
            (false, true, true) => "skill shot+crit",
            (false, false, true) => "crit",
            (false, false, false) => "normal",
        }
    }
}

fn u32_at(b: &[u8], o: usize) -> u32 {
    b.get(o..o + 4).map(|x| u32::from_le_bytes(x.try_into().unwrap())).unwrap_or(0)
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len() / 2).filter_map(|i| u8::from_str_radix(s.get(2 * i..2 * i + 2)?, 16).ok()).collect()
}

/// Parse a recorder CSV (any version: columns are found by name; the hit event fields come from `record`).
pub fn parse(text: &str) -> Result<Vec<LogHit>, String> {
    let mut lines = text.lines();
    let header: Vec<&str> = lines.next().unwrap_or("").split(',').collect();
    let col = |n: &str| header.iter().position(|h| *h == n);
    let (Some(hc), Some(rc)) = (col("health_change"), col("record")) else {
        return Err("not a combat log from ens_hook.dll (no health_change / record columns)".into());
    };
    let known = ["ms", "entity_a", "entity_b", "health_change", "stamina_change", "flags", "crit", "backstab", "weak_spot", "record",
                 "blocked", "no_damage", "loss_x10", "unix_ms",
                 "attacker_level", "target_level", "target_health_max",
                 "root_source", "target_id", "root_target", "source", "permission", "weapon_category", "weapon_pide", "program",
                 "material", "attack_category", "target_faction"];
    let mut out = vec![];
    for l in lines {
        let c: Vec<&str> = l.split(',').collect();
        if c.len() < header.len() {
            continue; // a row cut off mid-write
        }
        let get = |n: &str| col(n).and_then(|i| c.get(i)).copied().unwrap_or("");
        let rec = unhex(c[rc]);
        // compact rows (recorder after 2026-10-23) leave the record out and carry its fields as decimal columns
        let field = |n: &str, at: usize| get(n).parse::<u32>().ok().unwrap_or_else(|| u32_at(&rec, at));
        let byte = |n: &str, at: usize| get(n).parse::<u32>().ok().map(|v| v as u8).unwrap_or_else(|| rec.get(at).copied().unwrap_or(0));
        let flags = u32::from_str_radix(get("flags").trim_start_matches("0x"), 16).unwrap_or(0);
        let mut h = LogHit {
            ms: get("ms").parse().unwrap_or(0),
            unix_ms: get("unix_ms").parse().ok(),
            attacker: field("root_source", record::ROOT_SOURCE),
            target: field("target_id", record::TARGET),
            root_target: field("root_target", record::ROOT_TARGET),
            source: field("source", record::SOURCE),
            permission: field("permission", record::PERMISSION),
            weapon_category: field("weapon_category", record::WEAPON_CATEGORY),
            weapon_pide: field("weapon_pide", record::WEAPON_PIDE),
            program: field("program", record::PROGRAM),
            material: field("material", record::MATERIAL),
            attack_category: byte("attack_category", record::ATTACK_CATEGORY),
            target_faction: byte("target_faction", record::TARGET_FACTION),
            attacker_level: attr_value(get("attacker_level")),
            target_level: attr_value(get("target_level")),
            target_health_max: attr_value(get("target_health_max")),
            health: c[hc].parse().unwrap_or(0),
            crit: get("crit") == "true",
            backstab: get("backstab") == "true",
            effective: flags & 0x40 != 0,
            resisted: flags & 0x100 != 0,
            skill_shot: flags & 0x80 != 0,
            blocked: get("blocked") == "true",
            ..Default::default()
        };
        for k in 0..12 {
            h.types[k] = get(&format!("type{k}")).parse::<f64>().unwrap_or(0.0).abs();
        }
        for (i, name) in header.iter().enumerate() {
            if !known.contains(name) && !name.starts_with("type") {
                h.extra.insert(name.to_string(), c.get(i).copied().unwrap_or("").to_string());
            }
        }
        out.push(h);
    }
    Ok(out)
}

/// The player: the attacker with the most hits that carry a weapon category (enemies' hits carry none).
pub fn player(hits: &[LogHit]) -> Option<u32> {
    let mut n: BTreeMap<u32, usize> = BTreeMap::new();
    for h in hits.iter().filter(|h| h.weapon_category != 0) {
        *n.entry(h.attacker).or_default() += 1;
    }
    n.into_iter().max_by_key(|&(id, k)| (k, std::cmp::Reverse(id))).map(|(id, _)| id)
}

/// A set of the player's damaging hits that came from one source against one kind of target: same weapon category,
/// damage types, program, target faction and identified target type.
#[derive(Debug, Clone)]
pub struct Group {
    pub weapon_category: u32,
    pub pattern: Vec<usize>,
    pub program: u32,
    pub target_faction: u8,
    pub target_type: Option<String>,
    pub blast_collisions: Option<u32>,
    pub hits: Vec<LogHit>,
}

impl Group {
    /// Lowest and highest target level among the hits that record one.
    pub fn target_levels(&self) -> Option<(f64, f64)> {
        let v: Vec<f64> = self.hits.iter().filter_map(|h| h.target_level).collect();
        let lo = v.iter().copied().fold(f64::INFINITY, f64::min);
        (!v.is_empty()).then(|| (lo, v.iter().copied().fold(f64::NEG_INFINITY, f64::max)))
    }
}

pub fn groups(hits: &[LogHit], player: u32) -> Vec<Group> {
    type Key = (u32, Vec<usize>, u32, u8, Option<String>, Option<u32>);
    let mut map: BTreeMap<Key, Vec<LogHit>> = BTreeMap::new();
    for h in hits.iter().filter(|h| h.attacker == player && !h.by_companion() && h.health < 0 && !h.blocked && !on_material(h)) {
        let key = (h.weapon_category, h.pattern(), h.program, h.target_faction, h.target_type.clone(), h.blast_collisions);
        map.entry(key).or_default().push(h.clone());
    }
    let mut out: Vec<Group> = map
        .into_iter()
        .map(|((weapon_category, pattern, program, target_faction, target_type, blast_collisions), hits)| Group {
            weapon_category,
            pattern,
            program,
            target_faction,
            target_type,
            blast_collisions,
            hits,
        })
        .collect();
    out.sort_by(|a, b| b.hits.len().cmp(&a.hits.len()));
    out
}

/// Set `blast_collisions` on every Explosive Arrow blast row: the rows of one attacker's blast program that follow each
/// other within 3 ms are one blast (the game applies a blast's hits in one or two consecutive frames), and every row
/// counts, buildings and trees too, as the blast program counts every collision (`GetStreamEventCount`).
pub fn count_blasts(b: &Bundle, hits: &mut [LogHit]) {
    let Some(x) = &b.explosive_arrow else { return };
    let mut idx: Vec<usize> = (0..hits.len()).filter(|&i| hits[i].program == x.blast_program).collect();
    idx.sort_by_key(|&i| (hits[i].attacker, hits[i].ms));
    let mut start = 0;
    for k in 1..=idx.len() {
        let split = k == idx.len() || {
            let (a, z) = (&hits[idx[k - 1]], &hits[idx[k]]);
            a.attacker != z.attacker || z.ms.saturating_sub(a.ms) > 3
        };
        if split {
            for &i in &idx[start..k] {
                hits[i].blast_collisions = Some((k - start) as u32);
            }
            start = k;
        }
    }
}

/// One of your summons (or totems) as the log saw it, by swing: the game records a summon's swing as one row per frame
/// for as long as its weapon overlaps the target (3–9 rows 16 ms apart in combat-1792717052.csv), but only the first
/// one counts (a 434-health enemy took nine 352 rows and was hit again 2.4 s later). So rows within 0.2 s of each other
/// are one swing, priced by its first row. `hits` counts swings; `span_s` runs from the first swing to the last.
#[derive(Debug, Clone)]
pub struct CompanionLog {
    pub source: u32,
    /// The summon's own level, from the attacker_level column (None in logs without it).
    pub level: Option<f64>,
    pub program: u32,
    pub hits: usize,
    pub span_s: f64,
    pub median_hit: f64,
}

impl CompanionLog {
    /// Hits per second between its first and last hit (None for a single hit).
    pub fn per_s(&self) -> Option<f64> {
        (self.hits > 1 && self.span_s > 0.0).then(|| (self.hits - 1) as f64 / self.span_s)
    }
}

/// Your summons' hits, one entry per summon entity, most hits first.
pub fn companions(hits: &[LogHit], player: u32) -> Vec<CompanionLog> {
    let mut by: BTreeMap<u32, Vec<&LogHit>> = BTreeMap::new();
    for h in hits.iter().filter(|h| h.health < 0 && !on_material(h)) {
        if let Some(s) = h.summon_of(player) {
            by.entry(s).or_default().push(h);
        }
    }
    let mut out: Vec<CompanionLog> = by
        .into_iter()
        .map(|(source, hs)| {
            let mut hs = hs;
            hs.sort_by_key(|h| h.ms);
            let mut swings: Vec<&LogHit> = vec![];
            for h in &hs {
                if swings.last().is_none_or(|s| h.ms.saturating_sub(s.ms) > 200 && hs.iter().filter(|x| x.ms < h.ms && h.ms - x.ms <= 200).count() == 0) {
                    swings.push(h);
                }
            }
            let (first, last) = (swings.first().map(|h| h.ms).unwrap_or(0), swings.last().map(|h| h.ms).unwrap_or(0));
            let mut programs: BTreeMap<u32, usize> = BTreeMap::new();
            for h in &hs {
                *programs.entry(h.program).or_default() += 1;
            }
            CompanionLog {
                source,
                level: hs.iter().find_map(|h| if h.attacker == source { h.attacker_level } else { None }),
                program: programs.into_iter().max_by_key(|(_, n)| *n).map(|(p, _)| p).unwrap_or(0),
                hits: swings.len(),
                span_s: (last - first) as f64 / 1000.0,
                median_hit: median(&swings.iter().map(|h| h.damage()).collect::<Vec<_>>()).unwrap_or(0.0),
            }
        })
        .collect();
    out.sort_by(|a, b| b.hits.cmp(&a.hits));
    out
}

pub fn median(v: &[f64]) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    let mut v = v.to_vec();
    v.sort_by(|a, b| a.total_cmp(b));
    Some(v[v.len() / 2])
}

/// The planner's hit for one weapon and ammo: damage per type slot after the target's susceptibility and armor,
/// plus the crit multiplier and the weak-spot hit.
#[derive(Debug, Clone)]
pub struct Predicted {
    pub weapon: Equip,
    pub ammo: Option<String>,
    pub per_slot: [f64; 12],
    pub normal: f64,
    pub crit_multiplier: f64,
    pub weak: f64,
    /// The AttackMod the hit was priced at (1, or the blast's / echo's from [`Engine::explosive_hits`], or Strong
    /// Shot's at the average roll).
    pub attack_mod: f64,
    /// A special with a random roll (Strong Shot): the normal hit at the lowest and highest roll (r = 0 and r = 1).
    pub range: Option<(f64, f64)>,
}

/// How a hit is priced: the weapon and ammo's item damage at an AttackMod, or a weapon special's template (bundle
/// `specialTemplates`, Strong Shot among them) at roll r.
#[derive(Debug, Clone)]
pub enum Pricing {
    AttackMod(f64),
    Template(String, f64),
}

pub fn predict(engine: &Engine, build: &Build, weapon: &Equip, ammo: Option<&str>, target: &Target, pricing: Pricing) -> Predicted {
    let mut b = build.clone();
    (b.weapon, b.ammo) = (Some(weapon.clone()), ammo.map(str::to_string));
    let at = |t: &Target, r: Option<f64>| match (&pricing, r) {
        (Pricing::Template(name, x), r) => engine.template_hit(&b, weapon, ammo, t, name, 1.0, r.unwrap_or(*x)),
        (Pricing::AttackMod(am), _) => engine.hit(&b, weapon, ammo, *am, t),
    };
    let h = at(target, None);
    let attack_mod = h.attack_mod;
    let armor = if h.expected > 0.0 { h.after_armor / h.expected } else { 1.0 };
    let mut per_slot = [0.0; 12];
    for (name, v) in &h.per_type {
        // the log's slots follow keen::impact::DamageTypeNew (poison is slot 8, after healing), the planner's list does not
        if let Some(k) = if name == "Poison" { Some(8) } else { DAMAGE_TYPES.iter().position(|t| t.2 == name) } {
            per_slot[k] += v * armor;
        }
    }
    // a weak spot is a sub-entity with the enemy's susceptibility except its own SkillShot (bundle weakSpot)
    let mut weak_target = Target { weak_spot: true, ..target.clone() };
    if let Some(ws) = target.enemy_type.as_ref().and_then(|k| engine.bundle.enemy_types.get(k)).and_then(|e| e.weak_spot) {
        weak_target.susceptibility.insert("SkillShot".into(), ws);
    }
    let weak = at(&weak_target, None);
    let rolls = matches!(&pricing, Pricing::Template(name, _) if engine.bundle.special_templates.get(name).and_then(|t| t.attack_variation).is_some_and(|v| v > 0.0));
    let range = rolls.then(|| {
        let (lo, hi) = (at(target, Some(0.0)), at(target, Some(1.0)));
        let arm = |x: &crate::HitBreakdown| if x.expected > 0.0 { x.after_armor / x.expected } else { 1.0 };
        (lo.non_crit * arm(&lo), hi.non_crit * arm(&hi))
    });
    Predicted {
        weapon: weapon.clone(),
        ammo: ammo.map(str::to_string),
        per_slot,
        normal: h.non_crit * armor,
        crit_multiplier: h.crit_multiplier,
        weak: weak.non_crit * armor,
        attack_mod,
        range,
    }
}

/// How far a prediction is from a group's normal hits: the median of |ln(measured / predicted)| over the group's
/// damage types, plus a large penalty for every type one has and the other does not.
pub fn distance(p: &Predicted, measured: &[f64; 12]) -> f64 {
    let mut d = 0.0;
    for k in 0..12 {
        match (measured[k] > 0.0, p.per_slot[k] > 0.0) {
            (true, true) => d += (measured[k] / p.per_slot[k]).ln().abs(),
            (false, false) => {}
            _ => d += 10.0,
        }
    }
    d
}

/// [`distance`] for hits of `kind`: crits are compared after taking the prediction's crit multiplier off.
pub fn fit_distance(p: &Predicted, measured: &[f64; 12], kind: &str) -> f64 {
    if kind == "crit" && p.crit_multiplier > 0.0 {
        distance(p, &measured.map(|v| v / p.crit_multiplier))
    } else {
        distance(p, measured)
    }
}

/// Median damage per slot of a group's hits of one kind.
pub fn measured_slots(g: &Group, kind: &str) -> Option<[f64; 12]> {
    let hits: Vec<&LogHit> = g.hits.iter().filter(|h| h.kind() == kind).collect();
    if hits.is_empty() {
        return None;
    }
    let mut out = [0.0; 12];
    for (k, slot) in out.iter_mut().enumerate() {
        *slot = median(&hits.iter().map(|h| h.types[k]).collect::<Vec<_>>()).unwrap_or(0.0);
    }
    Some(out)
}

/// The enemy types a target can be: those of its faction whose max health at the logged level
/// (`enemy_health(level, healthFactor)`) is the logged max health. Several types often fit (66 Foggers have factor 1.0);
/// they are grouped into `options` by resistances and weak spot, the one with the most ordinary spawned enemies first. `target` / `key` / `shared` are
/// the option in use: the most common one, or the one [`compare`] found to fit the damage best.
pub struct Identified {
    pub target: Target,
    pub key: String,
    pub names: Vec<String>,
    /// How many of `names` have exactly the resistances of `target`.
    pub shared: usize,
    pub options: Vec<TargetOption>,
    /// True when [`compare`] chose `target` because its prediction explains the damage.
    pub picked: bool,
}

/// Enemy types with the same resistances and weak spot, named after the first of them (a spawned enemy, not a boss or
/// a `Base_` parent template, when there is one).
#[derive(Clone)]
pub struct TargetOption {
    pub key: String,
    pub target: Target,
    pub count: usize,
    /// How many of them are spawned enemies (not bosses, not `Base_` parent templates).
    pub spawned: usize,
}

impl Identified {
    fn choose(&mut self, i: usize) {
        let o = &self.options[i];
        (self.key, self.target, self.shared, self.picked) = (o.key.clone(), o.target.clone(), o.count, true);
    }
}

pub fn identify(b: &Bundle, faction: u8, level: f64, max_health: f64) -> Option<Identified> {
    let name = faction_name(faction);
    let level = level.round().clamp(0.0, 1000.0) as u32;
    let mut fits: Vec<(&String, &ens_data::EnemyType)> = b
        .enemy_types
        .iter()
        .filter(|(k, e)| e.faction == name && !unused_template(k))
        .filter(|(_, e)| e.health_factor.is_some_and(|f| (crate::enemy_health(b, level, f) - max_health.round()).abs() < 0.5))
        .collect();
    fits.sort_by(|a, b| a.0.cmp(b.0));
    let mut by: BTreeMap<(Vec<(String, String)>, String), Vec<(&String, &ens_data::EnemyType)>> = BTreeMap::new();
    for (k, e) in &fits {
        by.entry((sus_key(e), format!("{:?}", e.weak_spot))).or_default().push((k, e));
    }
    let mut options: Vec<TargetOption> = by
        .into_values()
        .map(|v| {
            let (k, _) = v
                .iter()
                .find(|(k, e)| !e.boss && !k.starts_with("Base_"))
                .or_else(|| v.iter().find(|(_, e)| !e.boss))
                .unwrap_or(&v[0]);
            let types: Vec<&ens_data::EnemyType> = v.iter().map(|(_, e)| *e).collect();
            let mut target = enemy_target(b, k);
            target.enemy_class = most_shared(&types).and_then(|(_, s)| s.class);
            let spawned = v.iter().filter(|(k, e)| !e.boss && !k.starts_with("Base_")).count();
            TargetOption { key: (*k).clone(), target, count: v.len(), spawned }
        })
        .collect();
    // the enemy you meet is most likely an ordinary spawned one, so those count first
    options.sort_by(|a, b| b.spawned.cmp(&a.spawned).then(b.count.cmp(&a.count)).then(a.key.cmp(&b.key)));
    let first = options.first()?.clone();
    Some(Identified { target: first.target, key: first.key, names: fits.iter().map(|(k, _)| (*k).clone()).collect(), shared: first.count, options, picked: false })
}

impl Identified {
    /// Keep the options whose resistances give the hit's effective / resisted marks (the factor thresholds of
    /// FUN_1403d84b0 over the hit's damage types). Unchanged when none would.
    pub fn match_marks(&mut self, engine: &Engine, h: &LogHit) {
        let names: Vec<&str> = h.pattern().iter().filter_map(|&k| if k == 8 { Some("Poison") } else { DAMAGE_TYPES.get(k).map(|t| t.2) }).collect();
        let fits = |o: &TargetOption| {
            let f = engine.effective_susceptibility(&o.target.susceptibility);
            let factor = |n: &str| f.get(n).copied().unwrap_or(1.0);
            let effective = names.iter().any(|n| factor(n) > 1.19);
            let resisted = names.iter().any(|n| factor(n) > 0.0 && factor(n) < 0.81);
            (h.skill_shot || effective == h.effective) && resisted == h.resisted
        };
        let kept: Vec<TargetOption> = self.options.iter().filter(|o| fits(o)).cloned().collect();
        if !kept.is_empty() && kept.len() < self.options.len() {
            self.options = kept;
            let o = &self.options[0];
            (self.key, self.target, self.shared) = (o.key.clone(), o.target.clone(), o.count);
        }
    }
}

impl LogHit {
    /// A hit dealt by something of the attacker's (a summon, a totem), not by the attacker itself.
    pub fn by_companion(&self) -> bool {
        self.source != 0 && self.source != self.attacker
    }

    /// A hit by one of `player`'s summons: the summon is the attacker and `player` the permission source (or, in
    /// the other form, `player` the attacker and the summon the source).
    pub fn summon_of(&self, player: u32) -> Option<u32> {
        if self.attacker == player && self.by_companion() {
            Some(self.source)
        } else if self.attacker != player && self.permission == player {
            Some(self.attacker)
        } else {
            None
        }
    }

    /// A hit on an attached part of another entity (a core, a weak spot, a body collider).
    pub fn on_part(&self) -> bool {
        self.root_target != 0 && self.root_target != self.target
    }
}

/// [`identify`] for one hit, narrowed by its effective / resisted marks. A hit on an attached part is identified
/// from its root's level and max health; when the part has health of its own, the enemy's hitable part with that
/// health replaces the target's susceptibility (key "enemy > part"), else the part shares the enemy's.
pub fn identify_hit(b: &Bundle, engine: &Engine, h: &LogHit) -> Option<Identified> {
    let mut id = if h.on_part() {
        let mut id = identify(b, h.target_faction, h.root_level?, h.root_health_max?)?;
        if let Some(hp) = h.target_health_max {
            let parts: Vec<TargetOption> = id
                .options
                .iter()
                .filter_map(|o| {
                    let part = b.enemy_types.get(&o.key)?.hitables.iter().find(|p| (p.health - hp).abs() < 0.5)?;
                    let mut target = o.target.clone();
                    target.susceptibility = part.susceptibility.clone();
                    Some(TargetOption { key: format!("{} > {}", o.key, part.name), target, ..o.clone() })
                })
                .collect();
            if !parts.is_empty() {
                id.options = parts;
                let o = &id.options[0];
                (id.key, id.target, id.shared) = (o.key.clone(), o.target.clone(), o.count);
            }
        }
        id
    } else {
        identify(b, h.target_faction, h.target_level?, h.target_health_max?)?
    };
    id.match_marks(engine, h);
    Some(id)
}

/// Templates the game does not spawn in play: debug, test and deprecated ones.
fn unused_template(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    ["debug", "test", "deprecated", "donotuse", "notfinished"].iter().any(|w| k.contains(w))
}

/// A bundle enemy type as a target.
pub fn enemy_target(b: &Bundle, key: &str) -> Target {
    let et = &b.enemy_types[key];
    Target {
        susceptibility: et.susceptibility.clone(),
        faction: Some(et.faction.clone()),
        enemy_class: Some(et.class.clone()),
        flying: et.flying,
        enemy_type: Some(key.to_string()),
        ..Target::default()
    }
}

fn sus_key(e: &ens_data::EnemyType) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = e.susceptibility.iter().map(|(k, x)| (k.clone(), format!("{x:.4}"))).collect();
    v.sort();
    v
}

struct Shared {
    count: usize,
    class: Option<String>,
    flying: bool,
    susceptibility: std::collections::HashMap<String, f64>,
}

/// The resistances most of `types` share, with how many share them, their most common class and whether all fly.
fn most_shared(types: &[&ens_data::EnemyType]) -> Option<(Vec<(String, String)>, Shared)> {
    let mut by: BTreeMap<Vec<(String, String)>, Vec<&ens_data::EnemyType>> = BTreeMap::new();
    for e in types {
        by.entry(sus_key(e)).or_default().push(e);
    }
    let (key, same) = by.into_iter().max_by(|a, b| a.1.len().cmp(&b.1.len()).then(b.0.cmp(&a.0)))?;
    let mut classes: BTreeMap<&str, usize> = BTreeMap::new();
    for e in &same {
        *classes.entry(e.class.as_str()).or_default() += 1;
    }
    let class = classes.into_iter().max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(a.0))).map(|(c, _)| c.to_string());
    let shared = Shared { count: same.len(), class, flying: same.iter().all(|e| e.flying), susceptibility: same[0].susceptibility.clone() };
    Some((key, shared))
}

/// The target a faction stands for: the resistances most of the faction's enemy types share, with its most common
/// enemy class. `shared` of `of` types have exactly these resistances. None for factions with no enemy types
/// (Environment, HitOthers and the like: training dummies, props), which are compared against a plain target.
pub struct FactionTarget {
    pub target: Target,
    pub shared: usize,
    pub of: usize,
}

pub fn faction_target(b: &Bundle, faction: u8) -> Option<FactionTarget> {
    let name = faction_name(faction);
    let types: Vec<&ens_data::EnemyType> = b.enemy_types.values().filter(|e| e.faction == name).collect();
    let (_, same) = most_shared(&types)?;
    let target = Target {
        susceptibility: same.susceptibility,
        faction: Some(name),
        enemy_class: same.class,
        flying: same.flying,
        ..Target::default()
    };
    Some(FactionTarget { target, shared: same.count, of: types.len() })
}

/// The character in a save as a build: level, tree, found skill points, Flame level, gear and weapons.
pub fn character(b: &Bundle, engine: &Engine, s: &Save, flame: u32) -> Build {
    let mut build = Build::new(b, s.level.clamp(1, crate::build::level_cap(b)));
    build.ranks = s.ranks(b).ranks;
    build.flame_level = flame;
    build.found_points = s.found_skill_points();
    crate::from_save::apply_items(b, &mut build, &s.items, |t| engine.total(t, &Target::default()).total);
    build
}

/// What the save owns that can deal a hit: weapons (with the save's rarity and level) and arrows / spells.
pub fn owned(b: &Bundle, s: &Save) -> (Vec<Equip>, Vec<String>) {
    let guid_of: std::collections::HashMap<u32, &String> = b.items.keys().filter_map(|g| Some((item_id(g)?, g))).collect();
    let (mut weapons, mut ammo): (Vec<Equip>, Vec<String>) = (vec![], vec![]);
    for it in s.items.inventory.iter().chain(s.items.equipped.iter().map(|(_, it)| it)) {
        let Some(&g) = guid_of.get(&it.id) else { continue };
        let i = &b.items[g];
        if i.category.as_deref() == Some("Weapons") && i.damage.is_some() {
            let e = crate::from_save::equip(g, it);
            if !weapons.contains(&e) {
                weapons.push(e);
            }
        } else if matches!(i.slot.as_deref(), Some("Arrow") | Some("Spell")) && !ammo.contains(g) {
            ammo.push(g.clone());
        }
    }
    (weapons, ammo)
}

/// One group of logged hits next to the owned weapon and ammo that predict it best.
pub struct Comparison {
    pub group: Group,
    /// What the hits were compared against: the identified enemy, the faction's target, or the --target / plain target.
    pub target: Target,
    /// Set when the target was identified from level and max health.
    pub identified: Option<Identified>,
    /// (shared, of) from [`FactionTarget`] when the faction picked the target.
    pub faction: Option<(usize, usize)>,
    /// "normal", else "skill shot", else "crit" (the first kind the group has): the hits `measured` is the median of.
    pub kind: &'static str,
    pub measured: Option<[f64; 12]>,
    pub best: Option<Predicted>,
}

/// Compare the player's hit groups in `hits` with the owned weapons and ammo. `fixed` replaces the target the
/// log's faction picks (a `--target` enemy).
pub fn compare(b: &Bundle, engine: &Engine, build: &Build, weapons: &[Equip], ammo: &[String], hits: &[LogHit], player: u32,
               fixed: Option<&Target>) -> Vec<Comparison> {
    let cat_id = |g: &str| b.items.get(g).and_then(|i| i.weapon_category.as_ref()).and_then(|c| item_id(c));
    let mut named: Vec<LogHit> = hits.to_vec();
    count_blasts(b, &mut named);
    // level and max health of every entity hit on itself, for hits on its parts
    let mut roots: BTreeMap<u32, (f64, f64)> = BTreeMap::new();
    for h in named.iter().filter(|h| !h.on_part()) {
        if let (Some(l), Some(hp)) = (h.target_level, h.target_health_max) {
            roots.insert(h.target, (l, hp));
        }
    }
    for h in named.iter_mut().filter(|h| h.on_part()) {
        if let Some(&(l, hp)) = roots.get(&h.root_target) {
            (h.root_level, h.root_health_max) = (Some(l), Some(hp));
        }
    }
    type Seen = (u8, i64, i64, i64, i64, Vec<usize>, bool, bool, bool);
    let mut cache: BTreeMap<Seen, Option<String>> = BTreeMap::new();
    let r = |x: Option<f64>| x.map(|v| v.round() as i64).unwrap_or(-1);
    for h in named.iter_mut().filter(|h| h.attacker == player) {
        let seen = (h.target_faction, r(h.target_level), r(h.target_health_max), r(h.root_level), r(h.root_health_max), h.pattern(), h.effective, h.resisted, h.skill_shot);
        let key = cache.entry(seen).or_insert_with(|| identify_hit(b, engine, h).map(|i| i.key)).clone();
        h.target_type = key;
    }
    groups(&named, player)
        .into_iter()
        .map(|g| {
            let first = g.hits.iter().find(|h| if h.on_part() { h.root_level.is_some() } else { h.target_level.is_some() && h.target_health_max.is_some() });
            let mut identified = match (fixed, &g.target_type, first) {
                (None, Some(_), Some(h)) => identify_hit(b, engine, h),
                _ => None,
            };
            let ft = if fixed.is_none() && identified.is_none() { faction_target(b, g.target_faction) } else { None };
            let faction = ft.as_ref().map(|f| (f.shared, f.of));
            let target = fixed.cloned().or(identified.as_ref().map(|i| i.target.clone())).or(ft.map(|f| f.target)).unwrap_or_default();
            let mut pairs: Vec<(&Equip, Option<&str>)> = vec![];
            for w in weapons.iter().filter(|w| cat_id(&w.item) == Some(g.weapon_category)) {
                match b.items.get(&w.item).and_then(|i| i.ammo.clone()).as_deref() {
                    Some(k @ ("Arrow" | "Spell")) => {
                        pairs.extend(ammo.iter().filter(|a| b.items[*a].slot.as_deref() == Some(k)).map(|a| (w, Some(a.as_str()))))
                    }
                    _ => pairs.push((w, None)),
                }
            }
            let (kind, measured) = ["normal", "skill shot", "crit"]
                .into_iter()
                .find_map(|k| measured_slots(&g, k).map(|m| (k, Some(m))))
                .unwrap_or(("normal", None));
            // the Explosive Arrow's blast and echo explosion are their own hits: only explosive ammo fits them, priced at
            // their AttackMod (Shrapnel Blast per collision, the echo's 0.5)
            let x = b.explosive_arrow.as_ref();
            // a weapon special's projectile or explosion (Strong Shot, the sword projectile, the wand's) is priced as its
            // template's program prices it, at the average roll
            let special = b.special_templates.iter().filter(|(_, t)| t.program == Some(g.program) && t.attack_mod.is_some()).map(|(k, _)| k.clone()).min();
            let attack_mod = |w: &Equip, a: Option<&str>| -> Option<Pricing> {
                match x {
                    _ if special.is_some() => Some(Pricing::Template(special.clone().unwrap(), 0.5)),
                    Some(x) if g.program == x.blast_program => engine
                        .explosive_hits(build, w, a, &Target::default(), g.blast_collisions.unwrap_or(1))
                        .map(|e| Pricing::AttackMod(e.blast_attack_mod)),
                    Some(x) if g.program == x.echo_program => engine
                        .explosive_hits(build, w, a, &Target::default(), 1)
                        .and_then(|e| e.echo.map(|_| Pricing::AttackMod(x.echo_attack_mod))),
                    _ => Some(Pricing::AttackMod(1.0)),
                }
            };
            let best_for = |t: &Target| {
                measured.and_then(|m| {
                    pairs
                        .iter()
                        .filter_map(|(w, a)| attack_mod(w, *a).map(|pr| predict(engine, build, w, *a, t, pr)))
                        .min_by(|x, y| fit_distance(x, &m, kind).total_cmp(&fit_distance(y, &m, kind)))
                })
            };
            let mut target = target;
            let mut best = best_for(&target);
            // several enemy types fit the health: on normal hits, another type's resistances replace the most common
            // ones only when they explain the damage (distance under 0.05, about 5% across the damage types), so a hit
            // the planner does not model (Strong Shot, the explosion) does not drag the choice to a misfit
            if let (Some(id), Some(m), "normal") = (identified.as_mut(), measured, kind) {
                let fit = |p: &Option<Predicted>| p.as_ref().map(|p| distance(p, &m)).unwrap_or(f64::INFINITY);
                let mut pick = (0, fit(&best));
                for (i, o) in id.options.iter().enumerate().skip(1) {
                    let d = fit(&best_for(&o.target));
                    if d < pick.1 - 1e-9 {
                        pick = (i, d);
                    }
                }
                if pick.0 != 0 && pick.1 < 0.05 {
                    id.choose(pick.0);
                    target = id.target.clone();
                    best = best_for(&target);
                }
            }
            Comparison { group: g, target, identified, faction, kind, measured, best }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(attacker: u32, category: u32, program: u32, health: i32, types: &[(usize, f32)], crit: bool, skill_shot: bool) -> String {
        let mut rec = vec![0u8; 0x150];
        rec[record::ROOT_SOURCE..record::ROOT_SOURCE + 4].copy_from_slice(&attacker.to_le_bytes());
        rec[record::TARGET..record::TARGET + 4].copy_from_slice(&77u32.to_le_bytes());
        rec[record::WEAPON_CATEGORY..record::WEAPON_CATEGORY + 4].copy_from_slice(&category.to_le_bytes());
        rec[record::PROGRAM..record::PROGRAM + 4].copy_from_slice(&program.to_le_bytes());
        let hex: String = rec.iter().map(|b| format!("{b:02x}")).collect();
        let mut t = [0f32; 12];
        for &(k, v) in types {
            t[k] = v;
        }
        let ts: Vec<String> = t.iter().map(|v| v.to_string()).collect();
        let flags = if skill_shot { 0xc0 } else { 0 };
        format!("1,{attacker},0,{health},0,{flags:#x},{crit},false,{skill_shot},{},{hex},false,false,0,1792617060000", ts.join(","))
    }

    fn csv(rows: &[String]) -> String {
        let types: Vec<String> = (0..12).map(|i| format!("type{i}")).collect();
        let mut s = format!("ms,entity_a,entity_b,health_change,stamina_change,flags,crit,backstab,weak_spot,{},record,blocked,no_damage,loss_x10,unix_ms\n", types.join(","));
        for r in rows {
            s.push_str(r);
            s.push('\n');
        }
        s
    }

    /// The hit event fields come out of the record; the player is the attacker whose hits carry a weapon category;
    /// groups split by damage types and program.
    #[test]
    fn parse_player_and_groups() {
        let text = csv(&[
            row(1, 0xBEEF, 5, -40, &[(1, 40.0)], false, false),
            row(1, 0xBEEF, 5, -41, &[(1, 41.0)], false, false),
            row(1, 0xBEEF, 5, -72, &[(1, 72.0)], true, false),
            row(1, 0xBEEF, 6, -30, &[(3, 23.0), (6, 7.0)], false, true),
            row(9, 0, 8, -12, &[(0, 12.0)], false, false),
        ]);
        let hits = parse(&text).unwrap();
        assert_eq!(hits.len(), 5);
        assert_eq!((hits[0].attacker, hits[0].target, hits[0].weapon_category, hits[0].program), (1, 77, 0xBEEF, 5));
        assert_eq!(hits[0].unix_ms, Some(1792617060000));
        assert_eq!(hits[3].pattern(), vec![3, 6]);
        assert_eq!((hits[2].kind(), hits[3].kind()), ("crit", "skill shot"));
        assert_eq!(player(&hits), Some(1));
        let g = groups(&hits, 1);
        assert_eq!(g.len(), 2);
        assert_eq!((g[0].pattern.clone(), g[0].hits.len()), (vec![1], 3));
        assert_eq!(measured_slots(&g[0], "normal").unwrap()[1], 41.0);
        assert!(parse("a,b\n1,2").is_err());
    }

    /// The live log's level-3 Foggers with 103 max health are the Depleted ones (factor 0.74); an impossible health
    /// names nobody; a weak-spot skill shot uses the enemy's weak-spot SkillShot, and an enemy without one gets none.
    #[test]
    fn identify_from_level_and_health_and_weak_spots() {
        let b = Bundle::load(ens_data::default_bundle_path()).unwrap();
        let id = identify(&b, 2, 3.0, 103.0).unwrap();
        assert_eq!((id.key.as_str(), id.options.len(), id.picked), ("Enemy_Fogger_Depleted", 1, false));
        assert!(id.names.iter().all(|k| crate::enemy_health(&b, 3, b.enemy_types[k].health_factor.unwrap()) == 103.0));
        assert!(id.names.iter().all(|k| !k.to_ascii_lowercase().contains("debug")), "{:?}", id.names);
        assert!(identify(&b, 2, 3.0, 99_999.0).is_none());
        let soldiers = identify(&b, 2, 3.0, 140.0).unwrap();
        assert!(soldiers.options.len() > 1 && soldiers.options[0].spawned >= soldiers.options[1].spawned);

        let eng = Engine::new(&b);
        let build = Build::new(&b, 13);
        let bow = &crate::build::find_item(&b, "Talion's Bow").unwrap();
        let arrow = &crate::build::find_item(&b, "Wooden Arrow").unwrap();
        let w = Equip { item: bow.clone(), rarity: None, level: None, perks: None };
        assert_eq!(b.enemy_types["Enemy_Fogger_Depleted"].weak_spot, Some(2.0));
        let p = predict(&eng, &build, &w, Some(arrow), &enemy_target(&b, "Enemy_Fogger_Depleted"), Pricing::AttackMod(1.0));
        assert!(p.weak > 1.5 * p.normal, "weak {} normal {}", p.weak, p.normal);
        let none = b.enemy_types.iter().find(|(_, e)| e.weak_spot.is_none() && e.faction == "Fogger").unwrap().0; // BTreeMap: stable
        let p = predict(&eng, &build, &w, Some(arrow), &enemy_target(&b, none), Pricing::AttackMod(1.0));
        assert!((p.weak - p.normal).abs() < 1e-9);
    }

    /// Level-3 Foggers with 84 max health come in three resistance sets. A Fire + Lightning hit marked resisted
    /// (0x100) is one with Magical_Base 0.8 (fire 1.2 × 0.8, lightning 0.8 < 0.81); marked effective (0x40) it is one
    /// without that, whose fire factor is above 1.19.
    #[test]
    fn effective_and_resisted_marks_narrow_the_enemy() {
        let b = Bundle::load(ens_data::default_bundle_path()).unwrap();
        let eng = Engine::new(&b);
        let h = |effective, resisted| LogHit {
            target_faction: 2,
            target_level: Some(3.0),
            target_health_max: Some(84.0),
            types: [0.0, 0.0, 0.0, 76.0, 0.0, 0.0, 19.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            effective,
            resisted,
            ..Default::default()
        };
        assert!(identify(&b, 2, 3.0, 84.0).unwrap().options.len() > 1);
        let r = identify_hit(&b, &eng, &h(false, true)).unwrap();
        assert!(r.options.iter().all(|o| o.target.susceptibility.get("MagBase") == Some(&0.8)), "{:?}", r.names);
        let e = identify_hit(&b, &eng, &h(true, false)).unwrap();
        assert!(e.options.iter().all(|o| o.target.susceptibility.get("MagBase") != Some(&0.8)));
        assert!(eng.effective_susceptibility(&e.target.susceptibility)["Fire"] > 1.19);
    }

    /// A summon's hits (rootSourceId you, sourceId the summon) are counted per summon with their rate, and kept out of
    /// your weapon groups.
    #[test]
    fn summon_hits_by_source() {
        let mine = LogHit { attacker: 1, source: 1, weapon_category: 7, health: -30, types: [0.0, 30.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0], ..Default::default() };
        let summon = |ms, dmg: i64| LogHit { attacker: 1, source: 50, ms, health: -dmg, types: [dmg as f64, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0], ..Default::default() };
        let hits = vec![mine.clone(), summon(1000, 100), summon(3000, 120), summon(5000, 110)];
        let c = companions(&hits, 1);
        assert_eq!((c.len(), c[0].source, c[0].hits, c[0].median_hit), (1, 50, 3, 110.0));
        assert!((c[0].per_s().unwrap() - 0.5).abs() < 1e-9, "2 gaps in 4 s");
        assert_eq!(groups(&hits, 1).iter().map(|g| g.hits.len()).sum::<usize>(), 1, "only your own hit");
        // the form the game uses for Hollow Wretches: the summon attacks, you are the permission source
        let wretch = |ms| LogHit { attacker: 77, source: 77, permission: 1, attacker_level: Some(17.0), ms, health: -60, types: [60.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0], ..Default::default() };
        let enemy = LogHit { attacker: 9, source: 9, permission: 0, health: -20, ..Default::default() };
        let c = companions(&[mine, wretch(0), wretch(16), wretch(33), wretch(1500), wretch(1516), enemy], 1);
        assert_eq!((c.len(), c[0].source, c[0].hits, c[0].level), (1, 77, 2, Some(17.0)), "rows 16 ms apart are one swing");
    }

    /// A hit on a part (targetId != rootTargetId) is identified from the root's level and max health; a part with 5
    /// max health on a level-3 Depleted Fogger is its core, with the core's own resistances (Physical 1.5).
    #[test]
    fn hits_on_parts_use_the_part() {
        let b = Bundle::load(ens_data::default_bundle_path()).unwrap();
        let eng = Engine::new(&b);
        let part = LogHit {
            target_faction: 2,
            target: 11,
            root_target: 10,
            root_level: Some(3.0),
            root_health_max: Some(103.0),
            target_health_max: Some(5.0),
            types: [0.0, 857.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            effective: true,
            ..Default::default()
        };
        assert!(part.on_part());
        let id = identify_hit(&b, &eng, &part).unwrap();
        assert_eq!(id.key, "Enemy_Fogger_Depleted > Hitable_Enemy_Fogger_Depleted_Core_WeakSpot");
        assert_eq!(id.target.susceptibility.get("Physical"), Some(&1.5));
        // a part without health of its own (a body collider) shares the enemy's resistances
        let collider = LogHit { target_health_max: None, effective: false, ..part };
        assert_eq!(identify_hit(&b, &eng, &collider).unwrap().key, "Enemy_Fogger_Depleted");
    }

    /// A save that owns two bows and two arrow types, and a log of hits on Foggers that one bow and arrow deal
    /// exactly: the comparison picks the Fogger resistances, finds that pair, and reads the level columns.
    #[test]
    fn compare_a_log_against_a_save() {
        let b = Bundle::load(ens_data::default_bundle_path()).unwrap();
        let eng = Engine::new(&b);
        let fits = |g: &&String, k: &str| b.items[*g].ammo.as_deref() == Some(k) && b.items[*g].damage.is_some();
        let mut bows: Vec<&String> = b.items.keys().filter(|g| fits(g, "Arrow") && b.items[*g].category.as_deref() == Some("Weapons")).collect();
        let mut arrows: Vec<&String> = b.items.keys().filter(|g| b.items[*g].slot.as_deref() == Some("Arrow")).collect();
        bows.sort();
        arrows.sort();
        let item = |g: &str, level| ens_data::save::SaveItem { slot: 0, id: item_id(g).unwrap(), count: 1, level: Some(level), perks: Some((3, 0)) };
        let s = Save {
            name: "Test".into(),
            guid: [0; 16],
            level: 10,
            xp: 0,
            nodes: vec![],
            tail: vec![],
            items: ens_data::save::Items {
                inventory: vec![item(bows[0], 5), item(bows[1], 9), item(arrows[0], 1), item(arrows[1], 1)],
                ..Default::default()
            },
            items_error: None,
            knowledge: None,
        };
        let build = character(&b, &eng, &s, 2);
        let (weapons, ammo) = owned(&b, &s);
        assert_eq!((weapons.len(), ammo.len()), (2, 2));
        let fogger = faction_target(&b, 2).unwrap();
        assert_eq!(fogger.target.faction.as_deref(), Some("Fogger"));
        assert!(fogger.shared >= 1 && fogger.shared <= fogger.of);
        let truth = predict(&eng, &build, &weapons[1], Some(&ammo[1]), &fogger.target, Pricing::AttackMod(1.0));
        assert!(truth.normal > 0.0);

        let cat = item_id(b.items[bows[1]].weapon_category.as_ref().unwrap()).unwrap();
        let mut rec = vec![0u8; 0x150];
        rec[record::ROOT_SOURCE..record::ROOT_SOURCE + 4].copy_from_slice(&1u32.to_le_bytes());
        rec[record::TARGET..record::TARGET + 4].copy_from_slice(&77u32.to_le_bytes());
        rec[record::WEAPON_CATEGORY..record::WEAPON_CATEGORY + 4].copy_from_slice(&cat.to_le_bytes());
        rec[record::PROGRAM..record::PROGRAM + 4].copy_from_slice(&0x061f_81eeu32.to_le_bytes());
        rec[record::TARGET_FACTION] = 2;
        let hex: String = rec.iter().map(|b| format!("{b:02x}")).collect();
        let ts: Vec<String> = truth.per_slot.iter().map(|v| (*v as f32).to_string()).collect();
        let health = -(truth.per_slot.iter().sum::<f64>().round() as i64);
        let types: Vec<String> = (0..12).map(|i| format!("type{i}")).collect();
        let mut text = format!("ms,entity_a,entity_b,health_change,stamina_change,flags,crit,backstab,weak_spot,{},record,blocked,no_damage,\
                                loss_x10,unix_ms,attacker_level,target_level,target_health_max\n", types.join(","));
        for (ms, level) in [(1, "0x41c80000"), (2, "0x41f00000"), (3, "0x1b")] {
            text += &format!("{ms},1,0,{health},0,0x0,false,false,false,{},{hex},false,false,0,1792617060000,0xa,{level},\n", ts.join(","));
        }
        let hits = parse(&text).unwrap();
        assert_eq!((hits[0].target_faction, hits[0].attacker_level, hits[0].target_level), (2, Some(10.0), Some(25.0)));
        assert_eq!((hits[1].target_level, hits[2].target_level, hits[0].target_health_max), (Some(30.0), Some(27.0), None));

        let c = compare(&b, &eng, &build, &weapons, &ammo, &hits, 1, None);
        assert_eq!(c.len(), 1);
        assert_eq!((c[0].group.target_faction, c[0].faction.is_some(), c[0].kind), (2, true, "normal"));
        assert_eq!(c[0].group.target_levels(), Some((25.0, 30.0)));
        let best = c[0].best.as_ref().unwrap();
        assert_eq!((best.weapon.item.as_str(), best.ammo.as_deref()), (bows[1].as_str(), Some(ammo[1].as_str())));
        assert!((c[0].measured.unwrap().iter().sum::<f64>() - truth.normal).abs() < 0.01 * truth.normal + 0.01);
    }

    /// A compact row (decoded columns, empty record) parses to the same hit as the full row of the same record.
    #[test]
    fn compact_row_reads_like_the_full_row() {
        let mut rec = vec![0u8; 0x150];
        let put = |rec: &mut Vec<u8>, at: usize, v: u32| rec[at..at + 4].copy_from_slice(&v.to_le_bytes());
        for (at, v) in [(record::ROOT_SOURCE, 1), (record::TARGET, 77), (record::ROOT_TARGET, 77), (record::SOURCE, 1), (record::PERMISSION, 1),
                        (record::WEAPON_CATEGORY, 0x1573ffb0), (record::WEAPON_PIDE, 9), (record::PROGRAM, 0x061f_81ee), (record::MATERIAL, 3)] {
            put(&mut rec, at, v);
        }
        rec[record::ATTACK_CATEGORY] = 0x11;
        rec[record::TARGET_FACTION] = 2;
        let hex: String = rec.iter().map(|b| format!("{b:02x}")).collect();
        let types = "0,19,0,0,0,0,0,0,0,0,0,0";
        let head = format!("ms,entity_a,entity_b,health_change,stamina_change,flags,crit,backstab,weak_spot,{},record,blocked,no_damage,\
                            loss_x10,unix_ms,attacker_level,target_level,target_health_max,root_source,target_id,root_target,source,\
                            permission,weapon_category,weapon_pide,program,material,attack_category,target_faction",
                           (0..12).map(|i| format!("type{i}")).collect::<Vec<_>>().join(","));
        let decoded = "1,77,77,1,1,359923632,9,102728174,3,17,2";
        let full = format!("{head}\n5,1,77,-19,0,0x8,true,false,false,{types},{hex},false,false,190,1792718451000,0x13,0xa,0xed,{decoded}\n");
        let compact = format!("{head}\n5,1,77,-19,0,0x8,true,false,false,{types},,false,false,190,1792718451000,0x13,0xa,0xed,{decoded}\n");
        let (a, b) = (parse(&full).unwrap(), parse(&compact).unwrap());
        assert_eq!(a, b);
        assert_eq!((b[0].attacker, b[0].target, b[0].weapon_category, b[0].program, b[0].attack_category, b[0].target_faction),
                   (1, 77, 0x1573ffb0, 0x061f_81ee, 0x11, 2));
        assert!(b[0].extra.is_empty(), "decoded columns are not extra columns: {:?}", b[0].extra);
    }
}
