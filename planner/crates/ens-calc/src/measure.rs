//! Combat log groups turned into measurement rows for `review/measurements/` (the columns of
//! `review/golden_vectors.csv` without `vector_id` and `source_file`, which the verifier fills in).
//!
//! A golden row is priced by `tests/golden.rs` from the character level, Flame level, weapon (+ ammo) with its level,
//! rarity and unlocked perks, the Eternal ammo's upgrade, the worn gear, the skills, the food and an enemy template,
//! at AttackMod 1. A row is only written when that build prices the hit exactly as the full logged character does
//! ([`golden_hit`] mirrors golden.rs); otherwise the group is skipped with the reason, so a row never fails for a
//! state the file cannot describe.

use crate::build::{find_item, Build};
use crate::combat_log::{predict, Comparison, Pricing};
use crate::{Engine, Equip, Target};
use ens_data::Bundle;
use std::collections::HashMap;

/// Columns of a measurement file, in order.
pub const COLUMNS: [&str; 21] = [
    "patch", "weapon", "weapon_level", "perks", "str", "dex", "int", "skills", "buffs", "target", "scenario",
    "measured_min", "measured_max", "n_hits", "video_link", "char_level", "flame_level", "rarity", "gear", "ammo_level",
    "ammo_perks",
];

/// One measurement row, with the planner's prediction for the logged character beside it (not written to the file).
#[derive(Debug, Clone)]
pub struct Row {
    pub fields: HashMap<&'static str, String>,
    /// The full logged character's hit for this scenario, rounded like the game's numbers.
    pub predicted: f64,
    /// Every distinct damage the hits showed, with how many hits had it.
    pub values: Vec<(f64, usize)>,
}

impl Row {
    pub fn csv(&self) -> String {
        COLUMNS.iter().map(|c| quote(self.fields.get(c).map(String::as_str).unwrap_or(""))).collect::<Vec<_>>().join(",")
    }
}

/// A hit group that gave no row, and why.
#[derive(Debug, Clone)]
pub struct Skipped {
    pub what: String,
    pub why: String,
}

/// What the log cannot say about the session.
#[derive(Debug, Clone, Default)]
pub struct Session {
    /// Food and elixirs eaten (the save does not record them), as item names.
    pub buffs: Vec<String>,
    /// Hits the log marks as plain are this kind instead (sneak or merciless sessions: the log has no flag for them).
    pub scenario: Option<String>,
    /// The player used only the first light swing of the melee combo. The log cannot tell combo steps, heavy attacks
    /// and weapon specials apart (they share the base-hit program, and the record's impactId changes with every
    /// attack), and only the first light swing is priced at AttackMod 1 the way a row is, so melee rows need this.
    pub first_swing: bool,
    /// Where the hits came from, for the `video_link` column.
    pub source: String,
}

fn quote(s: &str) -> String {
    if s.contains(',') || s.contains('"') { format!("\"{}\"", s.replace('"', "")) } else { s.to_string() }
}

/// The `skills` column for a tree: `Name:rank` per node; nodes that share a name and have one rank (the attribute
/// nodes) as `Name:count`, which golden.rs spreads over that many nodes of the name.
pub fn skills_column(b: &Bundle, ranks: &[u8]) -> String {
    let mut out: Vec<String> = vec![];
    let mut counted: Vec<&str> = vec![];
    for (i, s) in b.skills.iter().enumerate() {
        let r = ranks.get(i).copied().unwrap_or(0);
        if r == 0 {
            continue;
        }
        let same = b.skills.iter().filter(|x| x.name == s.name).count();
        if same > 1 && s.max <= 1 {
            if !counted.contains(&s.name.as_str()) {
                counted.push(&s.name);
                let n = b.skills.iter().enumerate().filter(|(k, x)| x.name == s.name && ranks.get(*k).copied().unwrap_or(0) > 0).count();
                out.push(format!("{}:{n}", s.name));
            }
        } else {
            out.push(format!("{}:{r}", s.name));
        }
    }
    out.join(";")
}

/// The hit golden.rs predicts for a row: the same steps as its `predict` (keep the two in step).
pub fn golden_hit(b: &Bundle, e: &Engine, row: &HashMap<&'static str, String>) -> Result<f64, String> {
    let get = |k: &str| row.get(k).map(|s| s.as_str()).unwrap_or("");
    let level: u32 = get("weapon_level").parse().map_err(|_| format!("weapon_level {:?} is not a number", get("weapon_level")))?;
    // optional whole-number cells: empty means the column's default
    let num = |k: &str| -> Result<Option<u32>, String> {
        match get(k) {
            "" => Ok(None),
            v => v.parse::<u32>().map(Some).map_err(|_| format!("{k} {v:?} is not a number")),
        }
    };
    // the character's own level (default: the weapon's level) and Flame level (default 1)
    let mut build = Build::new(b, num("char_level")?.unwrap_or(level).clamp(1, 50));
    if let Some(f) = num("flame_level")? {
        build.flame_level = f;
    }
    let mut parts = get("weapon").splitn(2, " + ");
    let wname = parts.next().unwrap_or("").trim();
    let wg = find_item(b, wname).ok_or_else(|| format!("no weapon named {wname:?}"))?;
    let perks = match get("perks") {
        "" => None,
        p => Some(p.parse::<u32>().map_err(|_| format!("perks {p:?} is not a number"))?),
    };
    // the weapon's rolled rarity (default: the rarity the game data lists for it)
    let rarity = Some(get("rarity").to_string()).filter(|r| !r.is_empty());
    build.weapon = Some(Equip { item: wg, rarity, level: Some(level), perks });
    if let Some(a) = parts.next() {
        build.ammo = Some(find_item(b, a.trim()).ok_or_else(|| format!("no ammo or spell named {:?}", a.trim()))?);
    }
    // an upgradable (Eternal) arrow or spell at a recorded level and unlocked perks (default: every perk, +1 level each)
    if num("ammo_level")?.is_some() || num("ammo_perks")?.is_some() {
        let a = build.ammo.clone().ok_or("ammo_level / ammo_perks set but the weapon fires no arrow or spell")?;
        build.upgrades.insert(a.clone(), Equip { item: a, rarity: None, level: num("ammo_level")?, perks: num("ammo_perks")? });
    }
    // worn armour, rings and shield: `Name|rarity|level|perks` per piece, `;` between pieces; empty fields take the
    // item's listed rarity, the planner's default level and every perk its rarity allows
    for piece in get("gear").split(';').map(str::trim).filter(|s| !s.is_empty()) {
        let f: Vec<&str> = piece.split('|').map(str::trim).collect();
        let item = find_item(b, f[0]).ok_or_else(|| format!("no gear named {:?}", f[0]))?;
        let field = |k: usize| f.get(k).copied().filter(|v| !v.is_empty());
        let n = |k: usize| field(k).map(|v| v.parse::<u32>().map_err(|_| format!("gear {piece:?}: {v:?} is not a number"))).transpose();
        build.wear(b, Equip { item, rarity: field(1).map(str::to_string), level: n(2)?, perks: n(3)? })?;
    }
    for entry in get("skills").split(';').map(str::trim).filter(|s| !s.is_empty()) {
        let (name, rank) = entry.rsplit_once(':').ok_or_else(|| format!("skill {entry:?} needs Name:rank"))?;
        let rank: u32 = rank.trim().parse().map_err(|_| format!("skill rank in {entry:?} is not a number"))?;
        let nodes: Vec<usize> = b.skills.iter().enumerate().filter(|(_, s)| s.name == name.trim()).map(|(i, _)| i).collect();
        if nodes.is_empty() {
            return Err(format!("no skill named {:?}", name.trim()));
        }
        if nodes.len() > 1 && b.skills[nodes[0]].max <= 1 {
            if rank as usize > nodes.len() {
                return Err(format!("{name}: {rank} taken but the tree has {}", nodes.len()));
            }
            for &i in nodes.iter().take(rank as usize) {
                build.ranks[i] = 1;
            }
        } else {
            build.ranks[nodes[0]] = rank.min(255) as u8;
        }
    }
    for f in get("buffs").split(';').map(str::trim).filter(|s| !s.is_empty()) {
        build.foods.push(find_item(b, f).ok_or_else(|| format!("no food or elixir named {f:?}"))?);
    }
    let sheet = e.sheet(&build);
    for (col, attr) in [("str", "Strength"), ("dex", "Dexterity"), ("int", "Intelligence")] {
        if let Ok(want) = get(col).parse::<f64>() {
            let have = sheet.values.get(attr).copied().unwrap_or(0.0);
            if (have - want).abs() > 0.5 {
                return Err(format!("state mismatch: {attr} is {have} from these skills, the row says {want}"));
            }
        }
    }
    let mut target = Target::default();
    match get("target") {
        "" | "dummy" => {}
        t => {
            let et = b.enemy_types.get(t).ok_or_else(|| format!("no enemy template named {t:?}"))?;
            target.susceptibility = et.susceptibility.clone();
            target.faction = Some(et.faction.clone());
            target.enemy_class = Some(et.class.clone());
            target.flying = et.flying;
            target.enemy_type = Some(t.to_string());
        }
    }
    let scenario = get("scenario");
    target.hit_kind = match scenario {
        "normal" | "crit" => None,
        "backstab" => Some("Backstab".into()),
        "sneak" => Some("Sneak".into()),
        "merciless" => Some("Merciless".into()),
        other => return Err(format!("scenario {other:?} is not modelled by the calculator")),
    };
    let w = build.weapon.clone().unwrap();
    let h = e.hit(&build, &w, build.ammo.as_deref(), 1.0, &target);
    let armor = if h.expected > 0.0 { h.after_armor / h.expected } else { 1.0 };
    let one = if scenario == "crit" { h.non_crit * h.crit_multiplier } else { h.non_crit };
    Ok((one * armor).round())
}

/// Why the golden build can differ from the logged character: what a row cannot carry.
fn differences(b: &Bundle, build: &Build) -> Vec<String> {
    let mut out = vec![];
    if let Some(g) = &build.gem {
        out.push(format!("a weapon gem ({}; a row has no gem column)", b.items.get(&g.item).map(|i| i.name.as_str()).unwrap_or("?")));
    }
    if !build.rested {
        out.push("not Rested (a row assumes Rested)".into());
    }
    out
}

/// The `gear` column: worn armour, rings and shield as `Name|rarity|level|perks`, `;` between pieces.
pub fn gear_column(b: &Bundle, build: &Build) -> String {
    build.gear.iter().flatten().chain(build.offhand.iter()).map(|e| {
        let name = b.items.get(&e.item).map(|i| i.name.clone()).unwrap_or_default();
        format!("{name}|{}|{}|{}", e.rarity.clone().unwrap_or_default(), e.level.map(|l| l.to_string()).unwrap_or_default(),
            e.perks.map(|p| p.to_string()).unwrap_or_default())
    }).collect::<Vec<_>>().join(";")
}

/// The rows a set of compared hit groups gives, and the groups that gave none.
pub fn rows(b: &Bundle, e: &Engine, build: &Build, compared: &[Comparison], fixed: Option<&str>, session: &Session) -> (Vec<Row>, Vec<Skipped>) {
    let name = |g: &str| b.items.get(g).map(|i| i.name.clone()).unwrap_or_else(|| g.to_string());
    let patch = format!("{} (game build {})", b.meta.get("version").and_then(|v| v.as_str()).unwrap_or("?"),
        b.meta.get("build").map(|v| v.to_string()).unwrap_or_default());
    let (mut out, mut skipped) = (vec![], vec![]);
    for c in compared {
        let g = &c.group;
        let what = format!("{} hits ({})", g.hits.len(),
            c.best.as_ref().map(|p| name(&p.weapon.item)).unwrap_or_else(|| format!("weapon category {:#x}", g.weapon_category)));
        let Some(best) = &c.best else {
            skipped.push(Skipped { what, why: "no weapon in the save explains these hits".into() });
            continue;
        };
        // melee hits: combo steps (AttackMod 1.5 on the Acid Cleaver's third), heavy attacks and specials (the Baroness
        // Daggers' Spin at 0.5 and 3.0) land beside the first light swing with the same program, so they cannot be told
        // apart; a row prices the first swing
        let melee = b.items.get(&best.weapon.item).and_then(|i| i.weapon_category.as_ref()).and_then(|c| b.weapon_categories.get(c))
            .and_then(|c| c.kind.as_deref()).is_some_and(|k| !k.starts_with("Ranged"));
        if melee && !session.first_swing {
            skipped.push(Skipped { what, why: "melee hits mix combo steps, heavy attacks and weapon specials, which the log cannot tell apart; record a session using only the first light swing and pass --first-swing".into() });
            continue;
        }
        if best.range.is_some() || (best.attack_mod - 1.0).abs() > 1e-9 {
            skipped.push(Skipped { what, why: format!("priced at AttackMod {:.2} (an explosion, echo or weapon special); a row prices the plain hit", best.attack_mod) });
            continue;
        }
        let key = fixed.map(str::to_string).or_else(|| c.identified.as_ref().map(|i| i.key.clone()));
        let Some(key) = key else {
            skipped.push(Skipped { what, why: "the target is only known by faction; pass --target <enemy> when you hit one kind of enemy".into() });
            continue;
        };
        // a part of an enemy (a weak spot or core with its own health) is not an enemy template a row can name
        if key.contains(" > ") {
            skipped.push(Skipped { what, why: "hits on a part of the enemy (a weak spot or core); a row names a whole enemy".into() });
            continue;
        }
        let mut full = build.clone();
        (full.weapon, full.ammo) = (Some(best.weapon.clone()), best.ammo.clone());
        let sheet = e.sheet(&full);
        let attr = |k: &str| sheet.values.get(k).copied().unwrap_or(0.0);
        let weapon_col = format!("{}{}", name(&best.weapon.item), best.ammo.as_ref().map(|a| format!(" + {}", name(a))).unwrap_or_default());
        let level = best.weapon.level.unwrap_or(full.level);
        // the save's upgrade record for an Eternal arrow or spell, when it has one
        let ammo_up = best.ammo.as_ref().and_then(|a| full.upgrades.get(a)).cloned();
        let mut kinds: Vec<&str> = g.hits.iter().map(|h| h.kind()).collect();
        kinds.sort();
        kinds.dedup();
        for kind in kinds {
            let scenario = match (kind, session.scenario.as_deref()) {
                ("normal", Some(s)) => s.to_string(),
                ("normal" | "crit" | "backstab", _) => kind.to_string(),
                _ => {
                    skipped.push(Skipped { what: format!("{what}, {kind}"), why: format!("a row has no scenario for {kind} hits") });
                    continue;
                }
            };
            let mut dmg: Vec<f64> = g.hits.iter().filter(|h| h.kind() == kind).map(|h| h.damage()).collect();
            dmg.sort_by(|a, b| a.total_cmp(b));
            let mut values: Vec<(f64, usize)> = vec![];
            for d in &dmg {
                match values.last_mut() {
                    Some((v, n)) if v == d => *n += 1,
                    _ => values.push((*d, 1)),
                }
            }
            let mut fields: HashMap<&'static str, String> = HashMap::new();
            for (k, v) in [
                ("patch", patch.clone()), ("weapon", weapon_col.clone()), ("weapon_level", level.to_string()),
                ("perks", best.weapon.perks.map(|p| p.to_string()).unwrap_or_default()),
                ("str", format!("{:.0}", attr("Strength"))), ("dex", format!("{:.0}", attr("Dexterity"))), ("int", format!("{:.0}", attr("Intelligence"))),
                ("skills", skills_column(b, &full.ranks)), ("buffs", session.buffs.join(";")), ("target", key.clone()),
                ("scenario", scenario.clone()), ("measured_min", format!("{}", dmg[0])), ("measured_max", format!("{}", dmg[dmg.len() - 1])),
                ("n_hits", dmg.len().to_string()), ("video_link", session.source.clone()),
                ("char_level", full.level.to_string()), ("flame_level", full.flame_level.to_string()),
                ("rarity", best.weapon.rarity.clone().unwrap_or_default()), ("gear", gear_column(b, &full)),
                ("ammo_level", ammo_up.as_ref().and_then(|u| u.level).map(|l| l.to_string()).unwrap_or_default()),
                ("ammo_perks", ammo_up.as_ref().and_then(|u| u.perks).map(|p| p.to_string()).unwrap_or_default()),
            ] {
                fields.insert(k, v);
            }
            // the full character's hit for this scenario, the way golden.rs reads one hit
            let mut t = c.target.clone();
            t.enemy_type = Some(key.clone());
            if let Some(et) = b.enemy_types.get(&key) {
                (t.susceptibility, t.faction, t.enemy_class, t.flying) = (et.susceptibility.clone(), Some(et.faction.clone()), Some(et.class.clone()), et.flying);
            }
            t.hit_kind = match scenario.as_str() { "backstab" => Some("Backstab".into()), "sneak" => Some("Sneak".into()), "merciless" => Some("Merciless".into()), _ => None };
            let p = predict(e, &full, &best.weapon, best.ammo.as_deref(), &t, Pricing::AttackMod(1.0));
            let predicted = (if scenario == "crit" { p.normal * p.crit_multiplier } else { p.normal }).round();
            match golden_hit(b, e, &fields) {
                Ok(gh) if (gh - predicted).abs() < 0.5 => out.push(Row { fields, predicted, values }),
                Ok(gh) => {
                    let why = differences(b, &full);
                    skipped.push(Skipped { what: format!("{what}, {scenario}"), why: format!("the file would price this hit at {gh}, the logged character at {predicted}: {}",
                        if why.is_empty() { "the reduced build differs in a way the exporter cannot name".to_string() } else { why.join("; ") }) });
                }
                Err(why) => {
                    let more = differences(b, &full);
                    skipped.push(Skipped { what: format!("{what}, {scenario}"), why: if more.is_empty() { why } else { format!("{why} ({})", more.join("; ")) } });
                }
            }
        }
    }
    (out, skipped)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundle() -> Bundle {
        Bundle::load(concat!(env!("CARGO_MANIFEST_DIR"), "/../../data/bundle.json")).unwrap()
    }

    /// A character golden.rs can describe (no gear, Flame 1, weapon at the character level): the replica of its
    /// reader prices the hit exactly as the planner prices the character, so the skills column round-trips.
    #[test]
    fn golden_replica_prices_a_describable_character_like_the_planner() {
        let b = bundle();
        let e = Engine::new(&b);
        let mut bd = Build::sample_bow(&b);
        (bd.gear, bd.offhand, bd.foods, bd.flame_level) = (vec![None; bd.gear.len()], None, vec![], 1);
        let mut w = bd.weapon.clone().unwrap();
        w.rarity = None;
        w.level = Some(bd.level);
        bd.weapon = Some(w.clone());
        let sheet = e.sheet(&bd);
        let mut row: HashMap<&'static str, String> = HashMap::new();
        for (k, v) in [("weapon", format!("{} + {}", b.items[&w.item].name, b.items[bd.ammo.as_ref().unwrap()].name)),
                       ("weapon_level", bd.level.to_string()), ("skills", skills_column(&b, &bd.ranks)),
                       ("str", format!("{:.0}", sheet.values["Strength"])), ("dex", format!("{:.0}", sheet.values["Dexterity"])),
                       ("int", format!("{:.0}", sheet.values["Intelligence"])), ("target", "dummy".into())] {
            row.insert(k, v);
        }
        for scenario in ["normal", "crit", "backstab"] {
            row.insert("scenario", scenario.into());
            let t = Target { hit_kind: (scenario == "backstab").then(|| "Backstab".into()), ..Default::default() };
            let p = predict(&e, &bd, &w, bd.ammo.as_deref(), &t, Pricing::AttackMod(1.0));
            let want = (if scenario == "crit" { p.normal * p.crit_multiplier } else { p.normal }).round();
            assert_eq!(golden_hit(&b, &e, &row), Ok(want), "{scenario}");
        }
    }

    /// What a row cannot carry is named: a weapon gem.
    #[test]
    fn differences_name_a_gem() {
        let b = bundle();
        let mut bd = Build::sample_bow(&b);
        let gem = b.items.iter().find(|(g, _)| b.gems.contains_key(*g)).map(|(g, _)| g.clone()).unwrap();
        bd.gem = Some(Equip { item: gem, rarity: None, level: Some(10), perks: None });
        assert!(differences(&b, &bd).join(" ").contains("weapon gem"));
        assert!(differences(&b, &Build::sample_bow(&b)).is_empty());
    }

    /// A session gives rows: one per hit kind, with the measured range, the hit count, the --target enemy and the
    /// session's source; a character with Flame 3, a ring, a rolled rarity and a weapon below its level gives rows too,
    /// with those in the new columns, and golden.rs's reading of each row prices it like the character.
    #[test]
    fn sessions_give_rows_with_level_flame_rarity_and_gear() {
        use crate::combat_log::{Group, LogHit};
        let b = bundle();
        let e = Engine::new(&b);
        let mut bd = Build::sample_bow(&b);
        (bd.gear, bd.offhand, bd.foods, bd.flame_level) = (vec![None; bd.gear.len()], None, vec![], 1);
        let mut w = bd.weapon.clone().unwrap();
        (w.rarity, w.level) = (None, Some(bd.level));
        bd.weapon = Some(w.clone());
        let key = "Enemy_Fogger_Depleted".to_string();
        let et = &b.enemy_types[&key];
        let t = Target { susceptibility: et.susceptibility.clone(), faction: Some(et.faction.clone()), enemy_class: Some(et.class.clone()),
            flying: et.flying, enemy_type: Some(key.clone()), ..Default::default() };
        let best = predict(&e, &bd, &w, bd.ammo.as_deref(), &t, Pricing::AttackMod(1.0));
        let hit = |d: i64, crit: bool| LogHit { health: -d, crit, ..Default::default() };
        let n = best.normal.round() as i64;
        let c = (best.normal * best.crit_multiplier).round() as i64;
        let group = Group { weapon_category: 0, pattern: vec![], program: 0, target_faction: 2, target_type: None, blast_collisions: None,
            hits: vec![hit(n, false), hit(n, false), hit(n + 1, false), hit(c, true)] };
        let cmp = |bd: &Build| vec![Comparison { group: group.clone(), target: t.clone(), identified: None, faction: None, kind: "normal",
            measured: None, best: Some(predict(&e, bd, &w, bd.ammo.as_deref(), &t, Pricing::AttackMod(1.0))) }];
        let session = Session { buffs: vec![], scenario: None, source: "combat log test.csv".into(), first_swing: false };
        let (rows, skipped) = super::rows(&b, &e, &bd, &cmp(&bd), Some(&key), &session);
        assert!(skipped.is_empty(), "{skipped:?}");
        assert_eq!(rows.len(), 2);
        let normal = rows.iter().find(|r| r.fields["scenario"] == "normal").unwrap();
        assert_eq!((normal.fields["measured_min"].as_str(), normal.fields["measured_max"].as_str(), normal.fields["n_hits"].as_str()),
            (n.to_string().as_str(), (n + 1).to_string().as_str(), "3"));
        assert_eq!(normal.fields["target"], key);
        assert_eq!(normal.predicted, n as f64);
        assert_eq!(normal.csv().split(',').count(), COLUMNS.len());
        // a lived-in character: Flame 3, a ring, an Uncommon roll of the weapon below the character's level
        let mut lived = bd.clone();
        lived.flame_level = 3;
        lived.gear[5] = Some(Equip { item: find_item(&b, "Hunter's Ring").unwrap(), rarity: Some("Rare".into()), level: Some(20), perks: None });
        let mut lw = w.clone();
        (lw.rarity, lw.level) = (Some("Uncommon".into()), Some(bd.level - 3));
        lived.weapon = Some(lw.clone());
        let cmp2 = vec![Comparison { group: group.clone(), target: t.clone(), identified: None, faction: None, kind: "normal",
            measured: None, best: Some(predict(&e, &lived, &lw, lived.ammo.as_deref(), &t, Pricing::AttackMod(1.0))) }];
        let (rows, skipped) = super::rows(&b, &e, &lived, &cmp2, Some(&key), &session);
        assert!(skipped.is_empty() && rows.len() == 2, "{skipped:?}");
        let r = &rows[0];
        assert_eq!((r.fields["flame_level"].as_str(), r.fields["rarity"].as_str(), r.fields["gear"].as_str()), ("3", "Uncommon", "Hunter's Ring|Rare|20|"));
        assert_eq!(r.fields["char_level"], bd.level.to_string());
        for r in &rows {
            assert_eq!(golden_hit(&b, &e, &r.fields), Ok(r.predicted));
        }
    }

    /// Melee hits give rows only when the session used the first light swing alone (--first-swing): the log cannot
    /// tell combo steps, heavy attacks and specials apart.
    #[test]
    fn melee_rows_need_first_swing() {
        use crate::combat_log::{Group, LogHit};
        let b = bundle();
        let e = Engine::new(&b);
        let mut bd = Build::new(&b, 15);
        let w = Equip { item: find_item(&b, "Barbarian Greatsword").unwrap(), rarity: None, level: Some(15), perks: None };
        bd.weapon = Some(w.clone());
        let t = Target::default();
        let best = predict(&e, &bd, &w, None, &t, Pricing::AttackMod(1.0));
        let group = Group { weapon_category: 0, pattern: vec![], program: 0, target_faction: 0, target_type: None, blast_collisions: None,
            hits: vec![LogHit { health: -(best.normal.round() as i64), ..Default::default() }] };
        let cmp = vec![Comparison { group, target: t, identified: None, faction: None, kind: "normal", measured: None, best: Some(best) }];
        let mut session = Session { buffs: vec![], scenario: None, source: "x".into(), first_swing: false };
        let (rows, skipped) = super::rows(&b, &e, &bd, &cmp, Some("dummy"), &session);
        assert!(rows.is_empty() && skipped[0].why.contains("--first-swing"), "{skipped:?}");
        session.first_swing = true;
        assert_eq!(super::rows(&b, &e, &bd, &cmp, Some("dummy"), &session).0.len(), 1);
    }
}
