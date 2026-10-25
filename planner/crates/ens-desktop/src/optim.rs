//! Optimizer settings, plans built from them, milestone results as loadouts, and their views. See
//! `docs/plans/optimizer-v2.md`.

use crate::model::{summarize, ARMOR_SLOTS};
use crate::Planner;
use ens_calc::{Build, Carried};
use ens_opt::{attack_rate, Milestone, MilestoneResult, Plan, SummonChoice, WeaponPlan, DEFAULT_MILESTONES};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// What the player asks the optimizer for. Saved with the build.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OptSettings {
    /// Weapons to build for; empty means the weapon in hand.
    #[serde(default)]
    pub weapons: Vec<WeaponPlan>,
    /// Skill node ids the tree must have / must not have.
    #[serde(default)]
    pub required: Vec<u32>,
    #[serde(default)]
    pub avoided: Vec<u32>,
    /// "keep" (the gear on now), "best" (the best in the game the level allows) or "owned" (the best you own).
    #[serde(default = "best_gear")]
    pub gear: String,
    /// Rarity assumed for picked items that roll their rarity.
    #[serde(default = "legendary")]
    pub rarity: String,
    /// More local-search rounds: slower, sometimes a better tree.
    #[serde(default)]
    pub thorough: bool,
    /// Milestones rebuild the tree from scratch instead of growing the previous one.
    #[serde(default)]
    pub respec: bool,
    #[serde(default = "default_milestones")]
    pub milestones: Vec<Milestone>,
    /// Which summons: keep the loadout's, chosen families, or the best (docs/plans/optimizer-summons.md).
    #[serde(default)]
    pub summons: SummonChoice,
}

fn best_gear() -> String {
    "best".into()
}

fn legendary() -> String {
    "Legendary".into()
}

fn default_milestones() -> Vec<Milestone> {
    DEFAULT_MILESTONES.to_vec()
}

impl Default for OptSettings {
    fn default() -> Self {
        OptSettings {
            weapons: vec![], required: vec![], avoided: vec![], gear: best_gear(), rarity: legendary(), thorough: false, respec: false,
            milestones: default_milestones(), summons: SummonChoice::Keep,
        }
    }
}

impl OptSettings {
    pub fn iters(&self) -> usize {
        if self.thorough { 40 } else { 8 }
    }
}

/// Progress of the running optimizer job, for the page to poll.
pub static DONE: AtomicUsize = AtomicUsize::new(0);
pub static TOTAL: AtomicUsize = AtomicUsize::new(0);
pub static RUNNING: AtomicBool = AtomicBool::new(false);

pub fn progress() -> Value {
    json!({ "done": DONE.load(Ordering::Relaxed), "total": TOTAL.load(Ordering::Relaxed), "running": RUNNING.load(Ordering::Relaxed) })
}

/// The optimizer plan for the open loadout and the settings: current level, Flame, gear, food, fight; the settings'
/// weapons (or the weapon in hand), must-have / avoided nodes and rarity.
pub fn plan_from(p: &Planner) -> Plan {
    let s = &p.opt_settings;
    let index = |ids: &[u32]| -> Vec<usize> { ids.iter().filter_map(|id| p.b.skills.iter().position(|n| n.id == *id)).collect() };
    let b = &p.build;
    Plan {
        level: b.level,
        flame_level: b.flame_level,
        weapon: b.weapon.clone(),
        ammo: b.ammo.clone(),
        gear: b.gear.clone(),
        foods: b.foods.clone(),
        summons: b.summons.clone(),
        ranking: p.ranking.clone(),
        attacks_per_s: b.weapon.as_ref().map(|w| attack_rate(&p.engine, b, w, b.ammo.as_deref())).unwrap_or(1.0),
        target: p.conditions().target,
        found_points: b.found_points,
        weapons: s.weapons.clone(),
        required: index(&s.required),
        avoided: index(&s.avoided),
        rarity: Some(s.rarity.clone()),
        summon_choice: s.summons.clone(),
    }
}

/// A loadout from the open one with a plan's tree, gear and weapons: the main weapon in hand with its ammo, the
/// other weapons carried with theirs. Food and the gem stay as they are (the gem only for the same weapon); summons
/// too, unless the optimizer chose them (plan.summon_choice not Keep).
pub fn build_from(base: &Build, plan: &Plan, ranks: &[u8], order: &[usize], name: String) -> Build {
    let mut nb = base.clone();
    nb.name = name;
    if plan.summon_choice != SummonChoice::Keep {
        nb.summons = plan.summons.clone();
    }
    (nb.level, nb.flame_level, nb.ranks, nb.order) = (plan.level, plan.flame_level, ranks.to_vec(), order.to_vec());
    nb.gear = plan.gear.clone();
    let same = nb.weapon.as_ref().map(|w| &w.item) == plan.weapon.as_ref().map(|w| &w.item);
    (nb.weapon, nb.ammo) = (plan.weapon.clone(), plan.ammo.clone());
    if !same {
        nb.gem = None;
    }
    nb.carried = plan
        .weapons
        .iter()
        .filter(|w| w.weapon != plan.weapon)
        .filter_map(|w| Some(Carried { weapon: w.weapon.clone()?, gem: None, ammo: w.ammo.clone() }))
        .collect();
    nb
}

/// Name of the loadout made from milestone `m`.
pub fn milestone_name(m: &MilestoneResult) -> String {
    format!("Level {}", m.level)
}

/// One gear or weapon row for the page: slot, item, rarity, active perk names.
fn item_row(p: &Planner, slot: &str, e: Option<&ens_calc::Equip>, extra: Value) -> Value {
    let Some(e) = e else { return json!({ "slot": slot, "item": null }) };
    let it = p.b.items.get(&e.item);
    let rarity = e.rarity.clone().or(it.and_then(|i| i.rarity.clone())).unwrap_or_default();
    let n = it.map(|_| ens_calc::perk_count(e, &rarity)).unwrap_or(0);
    let perks: Vec<String> = it
        .map(|i| i.perks.iter().filter(|x| !x.is_empty()).take(n).filter_map(|pid| p.b.perks.get(pid).map(|x| x.name.clone())).collect())
        .unwrap_or_default();
    json!({ "slot": slot, "item": p.item_name(&e.item), "rarity": rarity, "perks": perks, "extra": extra })
}

/// Skills in purchase order, one entry per skill name with the ranks bought (attribute points are separate nodes
/// sharing a name, so they add up under one entry), in order of first purchase.
fn picks(p: &Planner, order: &[usize]) -> Vec<Value> {
    let mut out: Vec<(usize, u32)> = vec![];
    for &i in order {
        match out.iter_mut().find(|(j, _)| p.b.skills[*j].name == p.b.skills[i].name) {
            Some(x) => x.1 += 1,
            None => out.push((i, 1)),
        }
    }
    out.into_iter()
        .map(|(i, n)| {
            let s = &p.b.skills[i];
            json!({ "node": i, "name": s.name, "kind": s.kind, "class": s.class, "ranks": n })
        })
        .collect()
}

pub fn milestones_view(p: &Planner) -> Value {
    let Some(list) = &p.ms_result else { return Value::Null };
    let c = p.conditions();
    let rows: Vec<Value> = list
        .iter()
        .enumerate()
        .map(|(k, m)| {
            let b = build_from(&p.build, &m.plan, &m.ranks, &m.order, milestone_name(m));
            let s = summarize(&p.engine, &b, &c);
            let mut gear = vec![];
            for (w, wp) in m.plan.weapons.iter().enumerate() {
                let ammo = wp.ammo.as_ref().map(|a| p.item_name(a));
                let share = wp.share / m.plan.weapons.iter().map(|x| x.share.max(0.0)).sum::<f64>().max(1e-9);
                gear.push(item_row(p, if w == 0 { "Weapon" } else { "Also" }, wp.weapon.as_ref(), json!({ "ammo": ammo, "share": share, "category": wp.category })));
            }
            for (k, slot) in ARMOR_SLOTS.iter().enumerate() {
                gear.push(item_row(p, slot, m.plan.gear[k].as_ref(), Value::Null));
            }
            // the summons it chose (or kept), as the loadout will have them
            for g in &b.summons {
                gear.push(item_row(p, "Summon", Some(&ens_calc::Equip { item: g.clone(), rarity: None, level: None, perks: None }), Value::Null));
            }
            json!({
                "index": k, "level": m.level, "flame": m.flame_level, "budget": m.budget, "spent": m.spent,
                "added": picks(p, &m.added), "removed": m.removed.iter().map(|&i| p.b.skills[i].name.clone()).collect::<Vec<_>>(),
                "dps": s.dps, "survived": s.hits_survived, "kills": m.goals.get("Damage"),
                "gear": gear, "loadout": p.loadouts.iter().any(|l| l.name == milestone_name(m)) || p.build.name == milestone_name(m),
            })
        })
        .collect();
    json!({ "milestones": rows })
}

/// Make milestone `k` (or every milestone) into loadouts named "Level N", replacing loadouts of that name.
/// One milestone also switches to its loadout.
pub fn apply_milestones(p: &mut Planner, k: Option<usize>) -> Result<(), String> {
    let list = p.ms_result.clone().ok_or("Run the milestone plan first.")?;
    let picks: Vec<&MilestoneResult> = match k {
        Some(k) => vec![list.get(k).ok_or("no such milestone")?],
        None => list.iter().collect(),
    };
    p.loadouts[p.current] = p.build.clone();
    let mut last = None;
    for m in picks {
        let name = milestone_name(m);
        let nb = build_from(&p.build, &m.plan, &m.ranks, &m.order, name.clone());
        match p.loadouts.iter().position(|l| l.name == name) {
            Some(j) => {
                p.loadouts[j] = nb;
                last = Some(j);
            }
            None => {
                p.loadouts.push(nb);
                last = Some(p.loadouts.len() - 1);
            }
        }
    }
    if k.is_some() {
        if let Some(j) = last {
            p.current = j;
        }
    }
    p.build = p.loadouts[p.current].clone();
    (p.undo, p.redo) = (vec![], vec![]);
    Ok(())
}
