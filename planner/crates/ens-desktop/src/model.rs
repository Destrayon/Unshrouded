//! Planner logic shared by every view: the fight conditions, build summaries (what the rail and Overview show),
//! gear-slot swaps, tree paths, and the "What's holding you back" rules. Ported from the egui app (ens-app).

use ens_calc::{Build, Engine, Equip, Target};
use ens_data::Bundle;
use ens_opt::{points_available, Optimizer};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};

pub const ARMOR_SLOTS: [&str; 7] = ["Head", "Chest", "Hands", "Legs", "Feet", "Ring", "Ring 2"];

/// A gear slot on the Gear tab: "weapon", "ammo", "gem", "armor0".."armor6", "food", "summon".
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Slot {
    Weapon,
    Ammo,
    Gem,
    Armor(usize),
    Food,
    Summon,
}

impl Slot {
    pub fn parse(s: &str) -> Option<Slot> {
        Some(match s {
            "weapon" => Slot::Weapon,
            "ammo" => Slot::Ammo,
            "gem" => Slot::Gem,
            "food" => Slot::Food,
            "summon" => Slot::Summon,
            _ => Slot::Armor(s.strip_prefix("armor")?.parse().ok().filter(|k: &usize| *k < 7)?),
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Slot::Weapon => "Weapon in hand",
            Slot::Ammo => "Ammo / spell",
            Slot::Gem => "Weapon gem",
            Slot::Armor(k) => ARMOR_SLOTS[k],
            Slot::Food => "Food",
            Slot::Summon => "Summons kept up",
        }
    }
}

/// An enemy to calculate against: its template's susceptibility, FactionType, class and flying flag.
#[derive(Clone)]
pub struct EnemyPick {
    pub label: String,
    /// template name in the bundle's enemyTypes (None = training dummy)
    pub name: Option<String>,
    /// Variant templates of the same enemy, merged into this entry: a fight saved against one finds this.
    pub aliases: Vec<String>,
    pub sus: HashMap<String, f64>,
    pub faction: Option<String>,
    pub class: Option<String>,
    pub flying: bool,
}

/// Every enemy template with a readable label; the training dummy first.
/// The enemy list as planners before R4 built it (every template, labels from the raw name, sorted by label), as
/// template names by index: a fight saved then stores only its index into this list.
pub fn legacy_enemy_names(b: &Bundle) -> Vec<Option<String>> {
    let mut faction_label: HashMap<String, String> = HashMap::new();
    for p in b.perks.values() {
        if let (Some(rest), [e]) = (p.name.strip_prefix("Damage against "), p.effects.as_slice()) {
            if let Some(a) = &e.attribute {
                faction_label.entry(a.clone()).or_insert_with(|| rest.to_string());
            }
        }
    }
    let mut list: Vec<(String, String)> = b.enemy_types.iter().map(|(name, t)| {
        let fac = ens_calc::faction_against_attribute(&t.faction).and_then(|a| faction_label.get(a).cloned()).unwrap_or_else(|| t.faction.clone());
        let pretty = name.trim_start_matches("Base_").trim_start_matches("Enemy_").replace('_', " ");
        let class = match t.class.as_str() { "Ranger" => "ranged", "Mage" => "mage", _ => "melee" };
        (format!("{fac} · {pretty} ({class}{}{})", if t.flying { ", flying" } else { "" }, if t.boss { ", boss" } else { "" }), name.clone())
    }).collect();
    list.sort_by(|a, b| a.0.cmp(&b.0));
    std::iter::once(None).chain(list.into_iter().map(|(_, n)| Some(n))).collect()
}

/// The enemies a player can pick, by in-game name, one entry each; the training dummy first
/// (docs/plans/enemy-names.md). The template the game's spawn table names stands for its enemy and the variants that
/// took its name become aliases; a boss the spawn table does not name reads as its template name in words; any other
/// template without a name (sub-enemies, effects, bases, unnamed variants) is left out.
pub fn enemies(b: &Bundle) -> Vec<EnemyPick> {
    let mut out = vec![EnemyPick { label: "Training dummy (no resistances)".into(), name: None, aliases: vec![], sus: HashMap::new(), faction: None, class: None, flying: false }];
    // test, debug, work-in-progress and retired templates are not enemies a player meets
    let internal = |n: &str| { let l = n.to_lowercase(); ["deprecat", "test", "debug", "_old", "old_", "wip", "placement_helper", "donotuse", "notfinished"].iter().any(|w| l.contains(w)) };
    let pick = |name: &String, t: &ens_data::EnemyType, label: String| EnemyPick {
        label, name: Some(name.clone()), aliases: vec![], sus: t.susceptibility.clone(), faction: Some(t.faction.clone()), class: Some(t.class.clone()), flying: t.flying,
    };
    let mut by_label: std::collections::BTreeMap<String, EnemyPick> = std::collections::BTreeMap::new();
    for (name, t) in b.enemy_types.iter().filter(|(n, _)| !internal(n)) {
        let label = match &t.display_name {
            Some(d) => d.clone(),
            // a boss's helper (an add) and the abstract Base_ templates are not bosses a player fights
            None if t.boss && !name.starts_with("Base_") && !name.to_lowercase().contains("bossadd") => format!("{} (boss)", boss_label(name)),
            None => continue,
        };
        match by_label.get_mut(&label) {
            None => {
                by_label.insert(label.clone(), pick(name, t, label));
            }
            Some(e) if t.display_name_own && !e.name.as_ref().and_then(|n| b.enemy_types.get(n)).is_some_and(|x| x.display_name_own) => {
                let variant = std::mem::replace(e, pick(name, t, label));
                e.aliases = variant.aliases;
                e.aliases.extend(variant.name);
            }
            Some(e) => e.aliases.push(name.clone()),
        }
    }
    out.extend(by_label.into_values());
    out
}

/// A boss's template name in words: "Enemy_Boss_Vorgoth_Phase1_Humanoid" reads "Vorgoth Phase 1 Humanoid".
fn boss_label(name: &str) -> String {
    let mut words: Vec<String> = vec![];
    for w in name.split('_') {
        let l = w.to_lowercase();
        // everything after a lore tag names the lore (quest hints, crafting stations)
        if l.starts_with("lore") {
            break;
        }
        if ["enemy", "boss", "midboss", "base"].contains(&l.as_str()) || l.starts_with("hook") || l.contains("healthbar") || l.contains("healtbar")
            || l.starts_with("hint") || (l.starts_with("ag") && l.len() > 2 && l[2..].chars().all(|c| c.is_ascii_digit())) {
            continue;
        }
        // split CamelCase and a trailing number: "GiantLizard" → "Giant Lizard", "Phase1" → "Phase 1" (a tier tag such
        // as "T2" stays whole); a zero-led number ("QuickAttack02") is a variant's index and goes, so the variants merge
        let mut word = String::new();
        let chars: Vec<char> = w.chars().collect();
        for (i, &c) in chars.iter().enumerate() {
            let prev = if i > 0 { Some(chars[i - 1]) } else { None };
            let run = chars[..i].iter().rev().take_while(|p| p.is_ascii_alphabetic()).count();
            if i > 0 && ((c.is_ascii_uppercase() && prev.is_some_and(|p| p.is_ascii_lowercase())) || (c.is_ascii_digit() && run > 1)) {
                word.push(' ');
            }
            word.push(c);
        }
        words.extend(word.split(' ').filter(|x| !x.is_empty() && !(x.starts_with('0') || x.chars().all(|c| c.is_ascii_digit()) && w.chars().all(|c| c.is_ascii_digit()))).map(str::to_string));
    }
    words.join(" ")
}

/// The picker entry a saved enemy (template name) stands under: its own entry or the one it was merged into.
pub fn find_enemy(list: &[EnemyPick], key: &str) -> Option<usize> {
    list.iter().position(|e| e.name.as_deref() == Some(key) || e.aliases.iter().any(|a| a == key))
}

impl Fight {
    /// The settings that differ from a new fight's, in words, beyond the enemy, group and hit kind the Fighting card
    /// already shows: each can move Total DPS a lot, so the card lists them next to it.
    pub fn changed(&self) -> Vec<String> {
        let d = Fight::default();
        let mut out = vec![];
        let ne = |a: f64, b: f64| (a - b).abs() > 1e-9;
        if ne(self.armor_physical, d.armor_physical) || ne(self.armor_magical, d.armor_magical) {
            out.push(format!("enemy armour {:.0}/{:.0}", self.armor_physical, self.armor_magical));
        }
        if ne(self.health_mult, d.health_mult) { out.push(format!("health \u{d7}{}", self.health_mult)); }
        if self.weak_spot { out.push("weak-spot hits".into()); }
        if self.soaked { out.push("enemy soaked".into()); }
        if ne(self.orb_pickup, d.orb_pickup) { out.push(format!("orbs picked up {:.0}%", self.orb_pickup * 100.0)); }
        if self.sprint_start { out.push("sprint start".into()); }
        if self.night { out.push("night".into()); }
        if self.elixir { out.push("elixir".into()); }
        if ne(self.evades_per_min, d.evades_per_min) { out.push(format!("{} evades/min", self.evades_per_min)); }
        if ne(self.blocks_per_min, d.blocks_per_min) { out.push(format!("{} blocks/min", self.blocks_per_min)); }
        if ne(self.glides_per_min, d.glides_per_min) { out.push(format!("{} glides/min", self.glides_per_min)); }
        if ne(self.air_share, d.air_share) { out.push(format!("{:.0}% in the air", self.air_share * 100.0)); }
        if ne(self.summon_idle, d.summon_idle) { out.push(format!("companions idle {:.0}%", self.summon_idle * 100.0)); }
        if ne(self.summon_attack_s, d.summon_attack_s) { out.push(format!("companion attacks every {} s", self.summon_attack_s)); }
        if ne(self.summon_life_s, d.summon_life_s) { out.push(format!("gem companions last {} s", self.summon_life_s)); }
        if ne(self.fight_s, d.fight_s) { out.push(format!("{} s fights", self.fight_s)); }
        if ne(self.heavy_reach_m, d.heavy_reach_m) { out.push(format!("heavy reach {} m", self.heavy_reach_m)); }
        if ne(self.zone_stay, d.zone_stay) { out.push(format!("enemies stay in zones {:.0}%", self.zone_stay * 100.0)); }
        if ne(self.zone_cap, d.zone_cap) { out.push(format!("{} zones at once", self.zone_cap)); }
        out
    }

    /// Why these settings cannot be used, or Ok: each number must be finite and inside the range its Combat setup
    /// field allows, so a typed value never reaches the numbers unchecked.
    pub fn check(&self) -> Result<(), String> {
        // the text settings take only the values their buttons and lists offer (a share code is not trusted)
        if !["Normal", "Backstab", "Sneak", "Merciless"].contains(&self.hit_kind.as_str()) {
            return Err("That combat setting was not changed: Your hits must be Normal, Backstab, Sneak or Merciless.".into());
        }
        if !["Cut", "Pierce", "Blunt", "Fire", "Ice", "Fog", "Lightning", "Poison"].contains(&self.incoming_type.as_str()) {
            return Err("That combat setting was not changed: Its hits deal must be a damage type.".into());
        }
        if self.enemy_key.as_ref().is_some_and(|k| k.len() > 200) {
            return Err("That combat setting was not changed: the enemy name is too long.".into());
        }
        let ranges: [(&str, f64, f64, f64); 17] = [
            ("Zones alive at once", self.zone_cap, 1.0, 10.0),
            ("Zone stay", self.zone_stay, 0.05, 1.0),
            ("Enemies", self.pack_size, 1.0, 8.0),
            ("Spread", self.pack_radius, 2.0, 10.0),
            ("Health ×", self.health_mult, 0.1, 3.0),
            ("Physical armour", self.armor_physical, 0.0, 500.0),
            ("Magical armour", self.armor_magical, 0.0, 500.0),
            ("Share of hits", self.hit_share, 0.0, 1.0),
            ("Heavy attack reach", self.heavy_reach_m, 0.5, 8.0),
            ("Orb pickup", self.orb_pickup, 0.0, 1.0),
            ("Evades per minute", self.evades_per_min, 0.0, 600.0),
            ("Share of attacks in the air", self.air_share, 0.0, 1.0),
            ("Glide landings per minute", self.glides_per_min, 0.0, 600.0),
            ("Blocks per minute", self.blocks_per_min, 0.0, 600.0),
            ("Companions idle", self.summon_idle, 0.0, 1.0),
            ("Seconds between companion attacks", self.summon_attack_s, 0.0, 120.0),
            ("Fight length", self.fight_s, 0.0, 3600.0),
        ];
        for (name, v, lo, hi) in ranges {
            if !v.is_finite() || v < lo || v > hi {
                return Err(format!("That combat setting was not changed: {name} must be {lo} to {hi}."));
            }
        }
        Ok(())
    }
}

fn zone_cap_default() -> f64 {
    ens_calc::ZONE_CAP
}

fn zone_stay_default() -> f64 {
    ens_calc::ZONE_STAY
}

fn heavy_reach_default() -> f64 {
    ens_calc::HEAVY_REACH_M
}

fn one() -> f64 {
    1.0
}

/// The fight settings from the Combat setup tab.
#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub struct Fight {
    pub enemy: usize,
    /// The picked enemy's template name, so a saved fight finds it again when the list changes.
    #[serde(default)]
    pub enemy_key: Option<String>,
    pub enemy_level: u32,
    /// The player set the enemy level; until then it follows the character's level.
    #[serde(default)]
    pub enemy_level_set: bool,
    /// × the picked enemy's own health (its template's EnemyScaling.healthScaleFactor, bundle enemyTypes
    /// healthFactor; 1 for the training dummy). Builds saved before it carried a fixed 0.85 factor under another name.
    #[serde(default = "one")]
    pub health_mult: f64,
    pub armor_physical: f64,
    pub armor_magical: f64,
    pub pack_size: f64,
    pub pack_radius: f64,
    pub orb_pickup: f64,
    pub incoming_type: String,
    pub hit_kind: String,
    /// Share of your hits that are of `hit_kind` (the rest are normal).
    #[serde(default = "one")]
    pub hit_share: f64,
    pub weak_spot: bool,
    pub soaked: bool,
    pub sprint_start: bool,
    pub evades_per_min: f64,
    pub air_share: f64,
    pub glides_per_min: f64,
    pub blocks_per_min: f64,
    pub night: bool,
    pub elixir: bool,
    pub summon_idle: f64,
    /// Seconds between one companion's attacks (ens_calc::SUMMON_ATTACK_S when absent, as in builds saved before it).
    #[serde(default = "summon_attack_default")]
    pub summon_attack_s: f64,
    /// Seconds a gem companion lasts, and seconds of one fight (ens_calc::SUMMON_LIFE_S / FIGHT_S when absent).
    #[serde(default = "summon_life_default")]
    pub summon_life_s: f64,
    #[serde(default = "fight_default")]
    pub fight_s: f64,
    /// How far a heavy melee attack reaches (m; ens_calc::HEAVY_REACH_M when absent).
    #[serde(default = "heavy_reach_default")]
    pub heavy_reach_m: f64,
    /// Share of a zone spell's lifetime enemies stay inside it (ens_calc::ZONE_STAY when absent).
    #[serde(default = "zone_stay_default")]
    pub zone_stay: f64,
    /// Zones of one spell alive at once (ens_calc::ZONE_CAP when absent).
    #[serde(default = "zone_cap_default")]
    pub zone_cap: f64,
}

fn summon_attack_default() -> f64 {
    ens_calc::SUMMON_ATTACK_S
}

fn summon_life_default() -> f64 {
    ens_calc::SUMMON_LIFE_S
}

fn fight_default() -> f64 {
    ens_calc::FIGHT_S
}

impl Default for Fight {
    fn default() -> Self {
        Fight {
            enemy: 0,
            enemy_key: None,
            enemy_level: 50,
            enemy_level_set: false,
            health_mult: 1.0,
            armor_physical: 0.0,
            armor_magical: 0.0,
            pack_size: 1.0,
            pack_radius: 4.5,
            orb_pickup: 0.5,
            incoming_type: "Pierce".into(),
            hit_kind: "Normal".into(),
            hit_share: 1.0,
            weak_spot: false,
            soaked: false,
            sprint_start: false,
            evades_per_min: 0.0,
            air_share: 0.0,
            glides_per_min: 0.0,
            blocks_per_min: 0.0,
            night: false,
            elixir: false,
            summon_idle: 0.0,
            summon_attack_s: ens_calc::SUMMON_ATTACK_S,
            summon_life_s: ens_calc::SUMMON_LIFE_S,
            fight_s: ens_calc::FIGHT_S,
            heavy_reach_m: ens_calc::HEAVY_REACH_M,
            zone_stay: ens_calc::ZONE_STAY,
            zone_cap: ens_calc::ZONE_CAP,
        }
    }
}

#[derive(Clone)]
pub struct Conditions {
    pub target: Target,
    pub enemy_level: u32,
    pub hp_factor: f64,
}

impl Fight {
    pub fn conditions(&self, b: &Bundle, enemies: &[EnemyPick]) -> Conditions {
        let e = &enemies[self.enemy.min(enemies.len() - 1)];
        // the enemy's own health factor (a boss 15–35, a wisp 0.05), times the player's multiplier
        let template = e.name.as_ref().and_then(|n| b.enemy_types.get(n)).and_then(|t| t.health_factor).unwrap_or(1.0);
        let hp_factor = template * self.health_mult;
        let target = Target {
            susceptibility: e.sus.clone(),
            armor_physical: self.armor_physical,
            armor_magical: self.armor_magical,
            faction: e.faction.clone(),
            enemy_type: e.name.clone(),
            enemy_class: e.class.clone(),
            flying: e.flying,
            pack_size: self.pack_size,
            pack_radius: self.pack_radius,
            enemy_health: ens_calc::enemy_health(b, self.enemy_level, hp_factor),
            orb_pickup: self.orb_pickup,
            incoming_type: Some(self.incoming_type.clone()),
            hit_kind: if self.hit_kind == "Normal" { None } else { Some(self.hit_kind.clone()) },
            hit_share: (self.hit_kind != "Normal").then_some(self.hit_share),
            weak_spot: self.weak_spot,
            soaked: self.soaked,
            play: ens_calc::Play {
                sprint_start: self.sprint_start,
                evades_per_min: self.evades_per_min,
                air_share: self.air_share,
                glides_per_min: self.glides_per_min,
                blocks_per_min: self.blocks_per_min,
                night: self.night,
                elixir: self.elixir,
                summon_idle: self.summon_idle,
                summon_attack_s: self.summon_attack_s,
                summon_life_s: self.summon_life_s,
                fight_s: self.fight_s,
                heavy_reach_m: self.heavy_reach_m,
                zone_stay: self.zone_stay,
                zone_cap: self.zone_cap,
            },
        };
        Conditions { target, enemy_level: self.enemy_level, hp_factor }
    }
}

/// One line of "where your damage comes from".
#[derive(Clone, Serialize)]
pub struct Source {
    pub name: String,
    pub dps: f64,
    pub note: String,
}

/// The numbers the rail and the Overview show, for the current build or a preview of a change.
#[derive(Clone, Default, Serialize)]
pub struct Summary {
    pub dps: f64,
    pub attacking_dps: f64,
    pub expected_hit: f64,
    pub ttk: Option<f64>,
    pub mana_uptime: f64,
    pub mana_per_s: f64,
    pub has_mana_cost: bool,
    pub hits_survived: f64,
    pub crit: f64,
    pub health: f64,
    pub health_regen: f64,
    pub mana_regen: f64,
    pub armor_phys: f64,
    pub armor_mag: f64,
    pub hit_taken: f64,
    pub hit_raw: f64,
    pub sources: Vec<Source>,
    pub fallback: Option<(String, f64)>,
}

pub fn summarize(engine: &Engine, b: &Build, c: &Conditions) -> Summary {
    let sheet = engine.sheet(b);
    let g = |k: &str| sheet.values.get(k).copied().unwrap_or(0.0);
    let tot = engine.total(b, &c.target);
    let hp = ens_calc::enemy_health(engine.bundle, c.enemy_level, c.hp_factor);
    let (raw, taken) = engine.incoming_target(b, c.enemy_level, &c.target);
    let st = engine.attributes(b);
    let up = tot.mana_uptime;
    let mut sources = vec![];
    let main_name = b.ammo.as_deref().or(b.weapon.as_ref().map(|w| w.item.as_str())).and_then(|g| engine.bundle.items.get(g)).map(|i| i.name.clone()).unwrap_or_else(|| "Main attack".into());
    if tot.main.regular_dps > 0.0 {
        let note = if up < 0.999 { format!("Main attack while mana lasts · {:.0}% of the fight", up * 100.0) } else { "Main attack".into() };
        sources.push(Source { name: main_name, dps: tot.main.regular_dps * up, note });
    }
    if tot.main.special_dps > 0.0 {
        sources.push(Source { name: "Special ability".into(), dps: tot.main.special_dps * up, note: format!("{:.3} uses per second", tot.main.special_per_s) });
    }
    if let Some((n, fd)) = &tot.fallback {
        sources.push(Source { name: n.clone(), dps: fd * (1.0 - up), note: format!("Used when out of mana · {:.0}% of the fight", (1.0 - up) * 100.0) });
    }
    for s in &tot.summons {
        if s.total_dps > 0.0 {
            sources.push(Source { name: s.name.clone(), dps: s.total_dps, note: format!("{} · {:.1} alive", if s.kind == "totem" { "Totem" } else { "Companion" }, s.count) });
        }
    }
    for (n, d, how) in &tot.skill_damage {
        if *d > 0.0 {
            sources.push(Source { name: n.clone(), dps: *d, note: how.clone() });
        }
    }
    let listed: f64 = sources.iter().map(|s| s.dps).sum();
    let rest = tot.total - listed;
    if rest.abs() > 0.05 {
        let names: Vec<String> = tot.buffs.iter().map(|(n, u, _)| format!("{n} ({:.0}% up)", u * 100.0)).collect();
        sources.push(Source { name: "Triggered buffs".into(), dps: rest, note: if names.is_empty() { "Skills that raise damage while active".into() } else { names.join(", ") } });
    }
    sources.sort_by(|a, b| b.dps.partial_cmp(&a.dps).unwrap_or(std::cmp::Ordering::Equal));
    Summary {
        dps: tot.total,
        attacking_dps: tot.replaced.as_ref().map(|r| r.1).unwrap_or(tot.main.total),
        expected_hit: tot.main.hit.expected,
        ttk: if tot.total > 0.0 { Some(hp / tot.total) } else { None },
        mana_uptime: up,
        mana_per_s: tot.main_mana_per_s,
        has_mana_cost: tot.main_mana_per_s > 0.0,
        hits_survived: g("Health") / taken.max(1.0),
        crit: if b.weapon.is_some() { tot.main.hit.crit_chance } else { g("CritChance") },
        health: g("Health"),
        health_regen: engine.regen_per_s(&st, "Health"),
        mana_regen: g("Mana regen /s"),
        armor_phys: g("ArmorPoints_Physical"),
        armor_mag: g("ArmorPoints_Magical"),
        hit_taken: taken,
        hit_raw: raw,
        sources,
        fallback: tot.fallback.clone(),
    }
}

/// Shortest chain of unallocated nodes from the allocated tree (or a root) to `target`, target included.
pub fn path_to(opt: &Optimizer, ranks: &[u8], target: usize) -> Option<Vec<usize>> {
    let skills = &opt.bundle.skills;
    if skills[target].kind == "Root" {
        return None;
    }
    if ranks[target] > 0 {
        return if (ranks[target] as u32) < skills[target].max.max(1) { Some(vec![target]) } else { None };
    }
    let mut prev = vec![usize::MAX; ranks.len()];
    let mut seen = vec![false; ranks.len()];
    let mut q = VecDeque::new();
    for i in 0..ranks.len() {
        if ranks[i] > 0 || skills[i].kind == "Root" {
            seen[i] = true;
            q.push_back(i);
        }
    }
    while let Some(u) = q.pop_front() {
        if u == target {
            break;
        }
        for &v in &opt.adj[u] {
            if !seen[v] && skills[v].kind != "Root" {
                seen[v] = true;
                prev[v] = u;
                q.push_back(v);
            }
        }
    }
    if !seen[target] {
        return None;
    }
    let mut path = vec![];
    let mut u = target;
    while ranks[u] == 0 && skills[u].kind != "Root" {
        path.push(u);
        u = prev[u];
        if u == usize::MAX {
            return None;
        }
    }
    path.reverse();
    Some(path)
}

pub fn with_path(b: &Build, path: &[usize]) -> Build {
    let mut nb = b.clone();
    for &i in path {
        nb.ranks[i] += 1;
        nb.order.push(i);
    }
    nb
}

/// Whether one rank of node `i` can come off without disconnecting the tree.
pub fn can_remove(opt: &Optimizer, b: &Build, i: usize) -> bool {
    if b.ranks[i] == 0 {
        return false;
    }
    if b.ranks[i] > 1 {
        return true;
    }
    // a tree that is already split (a loaded build code, the sample build) can always lose a rank; otherwise the
    // removal must not split it
    if !opt.connected(&b.ranks) {
        return true;
    }
    let mut t = b.ranks.clone();
    t[i] = 0;
    opt.connected(&t)
}

pub fn rarity_of(b: &Bundle, guid: &str) -> String {
    b.items.get(guid).and_then(|i| i.rarity.clone()).unwrap_or_default()
}

/// The build with `guid` put into `slot`. A weapon comes at its own level (every weapon's level is fixed, one item
/// copy per drop level) and its own rarity, unless its rarity rolls on drop (generateRarity): then it keeps the
/// rarity you hold, so it compares like for like.
pub fn apply_slot(bundle: &Bundle, b: &Build, slot: Slot, guid: &str) -> Build {
    let mut nb = b.clone();
    match slot {
        Slot::Weapon => {
            let it = bundle.items.get(guid);
            let held = nb.weapon.as_ref().and_then(|w| w.level);
            let lvl = it.and_then(|i| i.levels.as_ref()).map(|l| held.unwrap_or(l.min).clamp(l.min, l.max)).or(held);
            let rar = if it.and_then(|i| i.generate_rarity) == Some(true) {
                nb.weapon.as_ref().and_then(|w| w.rarity.clone()).or_else(|| Some(rarity_of(bundle, guid)))
            } else {
                Some(rarity_of(bundle, guid))
            };
            nb.weapon = Some(Equip { item: guid.to_string(), rarity: rar, level: lvl, perks: None });
        }
        Slot::Ammo => nb.ammo = Some(guid.to_string()),
        Slot::Gem => {
            let lvl = nb.gem.as_ref().and_then(|g| g.level);
            nb.gem = Some(Equip { item: guid.to_string(), rarity: None, level: lvl, perks: None });
        }
        Slot::Armor(k) => nb.gear[k] = Some(Equip { item: guid.to_string(), rarity: Some(rarity_of(bundle, guid)), level: None, perks: None }),
        Slot::Food => {
            if !nb.foods.iter().any(|f| f == guid) && nb.foods.len() < 4 {
                nb.foods.push(guid.to_string());
            }
        }
        Slot::Summon => {
            if !nb.summons.iter().any(|f| f == guid) && nb.summons.len() < 3 {
                nb.summons.push(guid.to_string());
            }
        }
    }
    nb
}

pub fn equipped(b: &Build, slot: Slot) -> Option<String> {
    match slot {
        Slot::Weapon => b.weapon.as_ref().map(|e| e.item.clone()),
        Slot::Ammo => b.ammo.clone(),
        Slot::Gem => b.gem.as_ref().map(|e| e.item.clone()),
        Slot::Armor(k) => b.gear[k].as_ref().map(|e| e.item.clone()),
        Slot::Food => b.foods.first().cloned(),
        Slot::Summon => b.summons.first().cloned(),
    }
}

pub fn equip_mut<'a>(bundle: &Bundle, b: &'a mut Build, slot: Slot) -> Option<&'a mut Equip> {
    match slot {
        Slot::Weapon => b.weapon.as_mut(),
        Slot::Gem => b.gem.as_mut(),
        Slot::Armor(k) => b.gear[k].as_mut(),
        // an upgradable (Eternal) arrow or spell keeps its level and perks in the build's upgrade records
        Slot::Ammo => {
            let g = b.ammo.clone().filter(|g| bundle.items.get(g).is_some_and(ens_calc::upgradable))?;
            let e = b.upgrade_of(&g);
            Some(b.upgrades.entry(g).or_insert(e))
        }
        _ => None,
    }
}

/// Keep the upgrade state the save records for an owned arrow, spell or summon spell that upgrades.
pub fn keep_owned_upgrade(bundle: &Bundle, b: &mut Build, owned: &[crate::store::OwnedItem], guid: &str) {
    if bundle.items.get(guid).is_some_and(ens_calc::upgradable) {
        if let Some(o) = owned.iter().find(|o| o.item.item == guid) {
            b.upgrades.insert(guid.to_string(), o.item.clone());
        }
    }
}

/// Something the Overview suggests fixing, with an optional one-click action.
#[derive(Serialize)]
pub struct Insight {
    pub warn: bool,
    pub title: String,
    pub body: String,
    /// (button label, action): "tab:<tab>", "slot:<slot>", "ammo:<guid>"
    pub action: Option<(String, String)>,
}

pub fn insights(bundle: &Bundle, b: &Build, s: &Summary, enemy_label: &str, tree_connected: bool, hit_kind_problem: Option<String>) -> Vec<Insight> {
    let name = |g: &str| bundle.items.get(g).map(|i| i.name.clone()).unwrap_or_default();
    let mut out = vec![];
    // a build the game cannot have still gets numbers, but they describe no real character
    let mut problems = b.problems(bundle);
    if !tree_connected {
        problems.push("Some skill nodes do not connect to a starting node, so they cannot be bought.".into());
    }
    if !problems.is_empty() {
        out.push(Insight { warn: true, title: "This build cannot exist in the game".into(), body: problems.join(" "), action: None });
    }
    if let Some(why) = hit_kind_problem {
        out.push(Insight { warn: true, title: "Your hits are counted as normal hits".into(), body: format!("{why} The Combat setup asks for them, so the numbers count normal hits instead."), action: Some(("Open Combat setup".into(), "tab:setup".into())) });
    }
    let spent = b.points(bundle);
    let have = points_available(b.level, b.found_points);
    if b.weapon.is_none() {
        out.push(Insight { warn: true, title: "No weapon equipped".into(), body: "Pick a weapon to see how much damage this build does.".into(), action: Some(("Pick a weapon".into(), "slot:weapon".into())) });
    }
    if spent > have {
        out.push(Insight { warn: true, title: format!("{} points over budget", spent - have), body: format!("Level {} gives {have} skill points; this tree spends {spent}.", b.level), action: Some(("Open the skill tree".into(), "tab:tree".into())) });
    }
    if s.has_mana_cost && s.mana_uptime < 0.99 {
        let ammo = b.ammo.as_deref().map(name).unwrap_or_else(|| "Your attack".into());
        out.push(Insight {
            warn: true,
            title: format!("Mana runs out after {:.0} s", s.mana_uptime * 60.0),
            body: format!("{ammo} needs {:.1} mana/s and you regenerate {:.1}/s, so it is up {:.0}% of a fight.", s.mana_per_s, s.mana_regen, s.mana_uptime * 100.0),
            action: Some(("See the mana budget".into(), "tab:calcs".into())),
        });
    }
    if let Some((n, fd)) = &s.fallback {
        if *fd > s.attacking_dps + 0.05 {
            let guid = bundle.items.iter().filter(|(_, i)| &i.name == n && i.category.as_deref() == Some("Ammunition"))
                .filter(|(_, i)| i.levels.as_ref().map(|l| l.min <= b.level).unwrap_or(true)).map(|(g, _)| g.clone()).min();
            out.push(Insight {
                warn: true,
                title: format!("{n} alone would do more"),
                body: format!("{n}: {fd:.1} DPS with no mana. Your mana attack: {:.1} DPS while it lasts, so the totals count {n} for the whole fight.", s.attacking_dps),
                action: guid.map(|g| (format!("Switch to {n}"), format!("ammo:{g}"))),
            });
        }
    }
    if s.hits_survived < 2.0 && s.health > 0.0 {
        out.push(Insight {
            warn: s.hits_survived < 1.0,
            title: if s.hits_survived < 1.0 { "One hit can kill you".into() } else { "Two hits can kill you".into() },
            body: format!("The biggest hit from {enemy_label} deals {:.0} after your {:.0} physical and {:.0} magical armour; your {:.0} health survives {:.1} of them.", s.hit_taken, s.armor_phys, s.armor_mag, s.health, s.hits_survived),
            action: b.gear.iter().position(|g| g.is_none()).filter(|k| *k < 5).map(|k| ("Fill armour slots".into(), format!("slot:armor{k}"))),
        });
    }
    let empty: Vec<&str> = ARMOR_SLOTS.iter().enumerate().filter(|(k, _)| b.gear[*k].is_none()).map(|(_, n)| *n).collect();
    let gem_empty = b.gem.is_none() && b.weapon.is_some();
    if !empty.is_empty() || gem_empty {
        let n = empty.len() + gem_empty as usize;
        let mut names: Vec<String> = empty.iter().map(|s| s.to_lowercase()).collect();
        if gem_empty {
            names.push("weapon gem".into());
        }
        let first = match b.gear.iter().position(|g| g.is_none()) { Some(k) => format!("slot:armor{k}"), None => "slot:gem".into() };
        out.push(Insight { warn: false, title: format!("{n} gear slot{} empty", if n == 1 { " is" } else { "s are" }), body: format!("Empty: {}. The Gear tab ranks every item for a slot by what it adds.", names.join(", ")), action: Some(("Fill gear slots".into(), first)) });
    }
    if have > spent {
        out.push(Insight { warn: false, title: format!("{} skill point{} unspent", have - spent, if have - spent == 1 { "" } else { "s" }), body: "The optimizer can place them for your goals.".into(), action: Some(("Open the optimizer".into(), "tab:optimizer".into())) });
    }
    if b.foods.len() < 3 {
        out.push(Insight { warn: false, title: format!("{} food slot{} unused", 3 - b.foods.len(), if b.foods.len() == 2 { "" } else { "s" }), body: "Food adds stats for the whole fight.".into(), action: Some(("Add food".into(), "slot:food".into())) });
    }
    out.sort_by_key(|i| !i.warn);
    out
}

pub fn key_of(b: &Build, f: &Fight) -> String {
    format!("{}|{}", b.to_code(), serde_json::to_string(f).unwrap_or_default())
}
