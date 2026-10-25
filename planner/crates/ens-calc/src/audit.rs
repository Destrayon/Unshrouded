//! Coverage audit: does every skill, perk and food actually move the planner's numbers?
//!
//! Archetype builds (one per weapon type, plus a summoner) give the outputs a chance to react. For every attribute
//! that any effect touches, the audit adds a test boost (`Build::extra`) and records in which archetypes any output
//! moved: an attribute that moves nothing is not read by the engine. Each skill is also set to full rank on its own
//! (behaviour such as extra projectiles or procs shows up here). Entities are then classed as counted, partly
//! counted, not counted, or outside combat numbers (gathering, building, survival comforts…).

use crate::{build::find_item, Build, Engine, Equip, Target};
use ens_data::Bundle;
use std::collections::{BTreeMap, BTreeSet};

/// Attribute name fragments that belong to systems outside combat numbers.
const NON_COMBAT: &[&str] = &[
    "Gather", "Mining", "Woodcutting", "Lumber", "Craft", "Build", "FallDamage", "Swim", "Glid", "Oxygen", "Comfort", "Hunger",
    "Inventory", "Loot", "Fish", "Durability", "Xp", "Experience", "Jump", "Climb", "BodyHeat", "Cold", "Freez", "Rest", "Carry",
    "Weight", "Noise", "Knowledge", "Torch", "Light", "Harvest", "Plant", "Cook", "Tool", "Grappl", "Hook", "Vukah", "Fog_Resistance",
    "FogResistance", "ShroudTime", "Shroud_", "Revive", "Respawn", "Aggro", "Visibility",
];

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Class {
    Counted,
    Partly,
    NotCounted,
    OutsideCombat,
    /// No effects and no program: the skill unlocks an action (Blink, Sneak Attack…) or gates an attack variant.
    Unlock,
    /// Works only in situations the planner does not simulate (parrying, overpowering, being hit, healing others,
    /// stuns, aggro, sneaking, switching weapons); the note says which.
    Scenario,
}

impl Class {
    pub fn label(&self) -> &'static str {
        match self {
            Class::Counted => "counted",
            Class::Partly => "partly counted",
            Class::NotCounted => "not counted",
            Class::OutsideCombat => "outside combat numbers",
            Class::Unlock => "unlocks an action",
            Class::Scenario => "depends on how you play",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub kind: &'static str,
    pub key: String,
    pub name: String,
    pub class: Class,
    /// Attributes this entity changes that the engine never reads (combat-relevant ones).
    pub unread: Vec<String>,
    pub note: String,
}

#[derive(Debug, Default)]
pub struct Report {
    pub archetypes: Vec<String>,
    /// attribute -> archetypes where a boost moved some output (empty = never read)
    pub attributes: BTreeMap<String, Vec<String>>,
    pub entries: Vec<Entry>,
}

/// Movement speed and sprint stamina (no damage, sustain or survival number uses them), and changes of the current
/// Health / Mana / Stamina value (instant restores; on-hit and on-kill gains are counted by the proc model instead).
const NON_COMBAT_EXACT: &[&str] = &["Movement_Mod", "StaminaDepletionMod_Sprint", "Stamina", "Health", "Mana"];

fn non_combat(attr: &str) -> bool {
    NON_COMBAT.iter().any(|k| attr.contains(k)) || NON_COMBAT_EXACT.contains(&attr)
}

fn weapon_of_kind(b: &Bundle, kind: &str) -> Option<String> {
    let mut best: Option<(u32, String, String)> = None;
    for (g, i) in &b.items {
        if i.name.is_empty() || i.category.as_deref() != Some("Weapons") || i.damage.is_none() {
            continue;
        }
        let k = i.weapon_category.as_ref().and_then(|c| b.weapon_categories.get(c)).and_then(|c| c.kind.clone());
        if k.as_deref() != Some(kind) {
            continue;
        }
        let lv = i.levels.as_ref().map(|l| l.max).unwrap_or(0);
        let cand = (lv, i.name.clone(), g.clone());
        if best.as_ref().map(|x| (cand.0, &cand.1) > (x.0, &x.1)).unwrap_or(true) {
            best = Some(cand);
        }
    }
    best.map(|x| x.2)
}

/// One build per weapon type (level 50, Flame 10, Legendary weapon at item level 50, empty tree).
pub fn archetypes(b: &Bundle) -> Vec<(String, Build)> {
    let mut out = vec![];
    let base = |b: &Bundle| {
        let mut x = Build::new(b, 50);
        x.flame_level = 10;
        x
    };
    let mut bow = Build::sample_bow(b);
    bow.ranks = vec![0; b.skills.len()];
    bow.foods.clear();
    out.push(("bow".to_string(), bow));
    for (name, kind, ammo) in [
        ("staff", "RangedStaff", Some("Eternal Ice Bolt IV")),
        ("wand", "RangedWand", None),
        ("one-hand sword", "OneHandSword", None),
        ("greatsword", "GreatSword", None),
        ("two-hand hammer", "TwoHandHammer", None),
        ("dagger", "Dagger", None),
        ("throwable", "Throwable", None),
        ("unarmed", "UnarmedFists", None),
    ] {
        let Some(w) = weapon_of_kind(b, kind) else { continue };
        let mut x = base(b);
        x.weapon = Some(Equip { item: w, rarity: Some("Legendary".into()), level: Some(50), perks: None });
        x.ammo = ammo.and_then(|a| find_item(b, a));
        out.push((name.to_string(), x));
    }
    if let Some(w) = weapon_of_kind(b, "RangedStaff") {
        let mut x = base(b);
        x.weapon = Some(Equip { item: w, rarity: Some("Legendary".into()), level: Some(50), perks: None });
        x.ammo = find_item(b, "Eternal Ice Bolt IV");
        x.summons = ["Eternal Hollow Wretch II", "Eternal Ice Totem II"].iter().filter_map(|n| find_item(b, n)).collect();
        out.push(("summoner".to_string(), x));
    }
    // a small share of every damage type, so per-element bonuses have something to act on
    // and some armor points (added directly, not from gear, whose perks could grant skill ranks), so armor
    // multipliers have points to act on
    for (_, x) in out.iter_mut() {
        // sneak and merciless attacks need their active skills (Engine::hit_kind_problem), so the situational targets
        // can move numbers for the weapons that can make them
        x.set_rank_by_name(b, "Sneak Attack", 1);
        x.set_rank_by_name(b, "Merciless Attack", 1);
        x.extra_dist = 0.05;
        x.base_extra = ["Physical", "Magical"].iter().map(|k| (format!("ArmorPoints_{k}_Adder"), 120.0))
            .chain(["Cut", "Pierce", "Blunt", "Fire", "Ice", "Fog"].iter().map(|k| (format!("ArmorPoints_{k}_Adder"), 10.0)))
            // and 10 points in every attribute, so effects that scale with an attribute (Athlete: 9 Health per Strength) show
            .chain(["Strength", "Dexterity", "Intelligence", "Constitution", "Endurance", "Spirit"].iter().map(|k| (format!("{k}_Base"), 10.0))).collect();
    }
    out
}

/// Whether some attack sequence's follow-up, impact or spawn needs this skill (requiredSkillId).
fn gates_attacks(bundle: &Bundle, id: u32) -> bool {
    bundle.sequences.values().any(|q| ["followUps", "impacts", "spawns"].iter().any(|k| {
        q.get(*k).and_then(|x| x.as_array()).is_some_and(|a| a.iter().any(|f| f.get("requiredSkillId").and_then(|x| x.as_u64()) == Some(id as u64)))
    }))
}

/// True when an effect can change something: not a zero add/subtract or a ×1 / ÷1 at any rank (food and flask
/// templates carry dozens of such placeholder effects).
fn effect_matters(e: &ens_data::Effect, max_rank: u32) -> bool {
    let Some(v) = &e.value else { return false };
    (1..=max_rank.max(1)).any(|r| {
        let x = v.eval(&|a| if a == "Level" { r as f64 } else { 10.0 });
        match e.operation {
            ens_data::Op::Add | ens_data::Op::Sub => x.abs() > 1e-12,
            ens_data::Op::Mul | ens_data::Op::Div => (x - 1.0).abs() > 1e-12,
        }
    })
}

pub fn audit_target() -> Target {
    Target { armor_physical: 60.0, armor_magical: 60.0, pack_size: 3.0, pack_radius: 4.5, enemy_health: 5000.0, orb_pickup: 1.0, ..Default::default() }
}

/// One target per faction that has a "damage against" mod, cycling the enemy classes, plus a flying one, so every
/// against-mod has a target it applies to.
pub fn audit_targets() -> Vec<Target> {
    let classes = ["Melee", "Ranger", "Mage"];
    let mut v: Vec<Target> = ["Fogger", "Vukah", "Scavenger", "Wildlife", "Skeleton", "Drak", "Ancients"].iter().enumerate().map(|(k, f)| Target {
        faction: Some(f.to_string()),
        enemy_class: Some(classes[k % 3].to_string()),
        incoming_type: Some(["Pierce", "Fire", "Ice", "Fog", "Lightning", "Poison", "Cut"][k].to_string()),
        ..audit_target()
    }).collect();
    v.push(Target { flying: true, enemy_class: Some("Melee".into()), incoming_type: Some("Blunt".into()), ..audit_target() });
    // situational hits: sneak, merciless, and weak-spot hits on a soaked target with a weak spot
    v.push(Target { hit_kind: Some("Sneak".into()), ..audit_target() });
    v.push(Target { hit_kind: Some("Merciless".into()), ..audit_target() });
    v.push(Target { hit_kind: Some("Backstab".into()), ..audit_target() });
    let mut weak = audit_target();
    weak.weak_spot = true;
    weak.soaked = true;
    weak.susceptibility.insert("SkillShot".into(), 1.5);
    v.push(weak);
    v
}

/// Every number the planner shows for a build.
pub fn outputs(e: &Engine, b: &Build, t: &Target) -> Vec<f64> {
    let sheet = e.sheet(b);
    let mut keys: Vec<_> = sheet.values.iter().collect();
    keys.sort_by(|a, b| a.0.cmp(b.0));
    let mut v: Vec<f64> = keys.into_iter().map(|(_, x)| *x).collect();
    let tot = e.total(b, t);
    v.extend([tot.total, tot.main.total, tot.main.hit.expected, tot.main.hit.crit_chance, tot.main.hit.after_armor, tot.main_uptime,
              tot.main_mana_per_s, tot.main.special_dps, tot.main.aoe_targets, tot.main.projectiles]);
    v.push(tot.procs.iter().map(|p| p.per_s).sum());
    v.push(tot.summons.iter().map(|s| s.total_dps).sum());
    // survival: one enemy hit of the target's damage type after the build's resistances and armor
    v.push(e.incoming_target(b, 50, t).1);
    if let Some(w) = &b.weapon {
        v.push(e.attack_interval(b, w, b.ammo.as_deref()).0);
    }
    v
}

fn moved(a: &[f64], b: &[f64]) -> bool {
    a.len() != b.len() || a.iter().zip(b).any(|(x, y)| (x - y).abs() > 1e-9 * (1.0 + x.abs()) && !(x.is_nan() && y.is_nan()))
}

/// Why a skill only matters in situations the planner does not simulate, from its program and attributes:
/// (true = outside combat numbers, reason).
fn scenario_reason(s: &ens_data::SkillNode, attrs: &BTreeSet<String>) -> Option<(bool, String)> {
    let nodes: BTreeSet<&str> = s.program.iter().map(|c| c.node.as_str()).collect();
    let hit_flag = |f: &str| s.program.iter().any(|c| c.node.starts_with("ForEachHitEvent") && c.inputs.keys().any(|k| k.ends_with(f)));
    let desc = s.desc.last().cloned().unwrap_or_default().to_lowercase();
    if s.closure.iter().any(|(k, _)| k.contains("FogResistanceGain")) {
        return Some((true, "extends your time in the Shroud".into()));
    }
    if nodes.contains("PacifyFaction") || nodes.contains("CallFactionForHelp") {
        return Some((true, "changes how animals or Vukah behave toward you".into()));
    }
    if nodes.contains("ForEachParryEvent") || attrs.contains("ParryProficiency_Multiplier") {
        return Some((false, "triggers on parrying".into()));
    }
    if hit_flag("wasBlockBroken") {
        return Some((false, "triggers on overpowering (breaking a block)".into()));
    }
    if desc.contains("receiv") || desc.contains("when you are killed") || desc.contains("when you are targeted") {
        return Some((false, "triggers when you are hit or killed".into()));
    }
    if desc.contains("heal") || nodes.contains("ForEachPlayerInRange") || attrs.contains("HealingMod_Adder") {
        return Some((false, "heals or buffs you and allies".into()));
    }
    if desc.contains("stun") {
        return Some((false, "stuns or staggers enemies (no damage number)".into()));
    }
    if attrs.contains("EnemyTargetSelectionBonus") {
        return Some((false, "changes enemy aggro".into()));
    }
    if attrs.contains("Movement_Sneak") || attrs.contains("StaminaDepletionMod_Sprint") {
        return Some((false, "movement while sneaking or sprinting".into()));
    }
    if hit_flag("wasSkillShot") || hit_flag("wasVisceral") {
        return Some((false, "triggers on skill-shot, sneak or merciless kills".into()));
    }
    if desc.contains("next bow attack") || desc.contains("next staff attack") {
        return Some((false, "needs switching between two weapon types".into()));
    }
    None
}

/// Why a triggered perk changes no damage, sustain or survival number, from its program (and the buffs it applies):
/// (class, reason).
fn perk_reason(p: &ens_data::Perk, bundle: &Bundle) -> Option<(Class, String)> {
    let has = |n: &str| p.program.iter().flatten().any(|c| c.node == n);
    let input_has = |f: &str| p.program.iter().flatten().any(|c| c.inputs.keys().any(|k| k.contains(f)));
    let attr_ref = |a: &str| p.program.iter().flatten().any(|c| c.inputs.values().any(|v| v.get("kind").and_then(|k| k.as_str()) == Some("attribute") && v.get("value").and_then(|x| x.as_str()) == Some(a)));
    if has("SetAttribute") && attr_ref("Durability") {
        return Some((Class::OutsideCombat, "repairs your weapon's durability on hits".into()));
    }
    if input_has("ForEachReceivedHit") {
        return Some((Class::Scenario, "triggers when you are hit".into()));
    }
    if has("IsInState") {
        return Some((Class::Scenario, "only while airborne (IsInState Grounded keeps its buff off on the ground)".into()));
    }
    // buffs it applies (BuffType input → fnv1a of the GUID = buffTypeId)
    let fnv = |hex: &str| -> Option<u32> {
        let b: Vec<u8> = (0..hex.len()).step_by(2).filter_map(|i| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok()).collect();
        (b.len() == 16).then(|| b.iter().fold(0x811c9dc5u32, |h, c| (h ^ *c as u32).wrapping_mul(0x01000193)))
    };
    let buffs: Vec<&ens_data::Buff> = p.program.iter().flatten().filter(|c| c.node == "ApplyBuff").flat_map(|c| c.inputs.values())
        .filter_map(|v| v.get("value").and_then(|x| x.as_str()))
        .filter_map(|x| fnv(x.trim_start_matches("guid:"))).filter_map(|id| bundle.buffs.get(&id.to_string())).collect();
    if !buffs.is_empty() {
        // the Poisoned debuff's program (969e3bf1…) only reaches its ApplyDamage when the target has faction Player
        // (HasFaction mask 0x0080), so applying it to enemies deals no damage
        if buffs.iter().all(|b| b.name == "Poisoned") {
            return Some((Class::OutsideCombat, "applies Poisoned, whose damage only hits players (its program checks HasFaction Player), so it adds no damage to enemies".into()));
        }
        let attrs: Vec<&str> = buffs.iter().flat_map(|b| b.effects.iter()).filter_map(|e| e.attribute.as_deref()).collect();
        if !attrs.is_empty() && attrs.iter().all(|a| a.starts_with("Stamina")) {
            return Some((Class::OutsideCombat, "raises stamina regeneration for a while (DPS and survival here do not use stamina)".into()));
        }
    }
    None
}

pub fn run(bundle: &Bundle) -> Report {
    let e = Engine::new(bundle);
    let targets = audit_targets();
    let arch = archetypes(bundle);
    // outputs across every target, concatenated per archetype
    let all = |b: &Build| -> Vec<f64> { targets.iter().flat_map(|t| outputs(&e, b, t)).collect() };
    let base: Vec<Vec<f64>> = arch.iter().map(|(_, b)| all(b)).collect();
    let mut rep = Report { archetypes: arch.iter().map(|(n, _)| n.clone()).collect(), ..Default::default() };

    // every attribute an effect touches
    let mut attrs: BTreeSet<String> = BTreeSet::new();
    for s in &bundle.skills {
        attrs.extend(s.effects.iter().filter(|x| effect_matters(x, s.max)).filter_map(|x| x.attribute.clone()));
    }
    for p in bundle.perks.values() {
        attrs.extend(p.effects.iter().filter(|x| effect_matters(x, 1)).filter_map(|x| x.attribute.clone()));
    }
    for i in bundle.items.values() {
        if let Some(bf) = &i.buff {
            attrs.extend(bf.effects.iter().filter(|x| effect_matters(x, 1)).filter_map(|x| x.attribute.clone()));
        }
    }
    for a in &attrs {
        let mut hit = vec![];
        for (k, (name, b)) in arch.iter().enumerate() {
            let mut x = b.clone();
            x.extra = vec![(a.clone(), 1.0)];
            if moved(&base[k], &all(&x)) {
                hit.push(name.clone());
            }
        }
        rep.attributes.insert(a.clone(), hit);
    }
    let read = |a: &str| rep.attributes.get(a).map(|v| !v.is_empty()).unwrap_or(false);
    let classify = |effect_attrs: &BTreeSet<String>, moved_any: Option<bool>, behaviour: bool| -> (Class, Vec<String>) {
        let unread: Vec<String> = effect_attrs.iter().filter(|a| !read(a) && !non_combat(a)).cloned().collect();
        let any_read = effect_attrs.iter().any(|a| read(a));
        let all_noncombat = !effect_attrs.is_empty() && effect_attrs.iter().all(|a| non_combat(a) || read(a)) && !any_read;
        let moves = moved_any.unwrap_or(any_read);
        let class = if moves && unread.is_empty() && !(behaviour && moved_any == Some(false)) {
            Class::Counted
        } else if moves {
            Class::Partly
        } else if all_noncombat && !behaviour {
            Class::OutsideCombat
        } else if effect_attrs.is_empty() && !behaviour {
            Class::OutsideCombat
        } else {
            Class::NotCounted
        };
        (class, unread)
    };

    // skills: one entry per name, set every node of that name to full rank in each archetype
    let mut done = BTreeSet::new();
    for s in &bundle.skills {
        if s.kind == "Root" || !done.insert(s.name.clone()) {
            continue;
        }
        let idx: Vec<usize> = bundle.skills.iter().enumerate().filter(|(_, x)| x.name == s.name).map(|(i, _)| i).collect();
        let mut moved_in = vec![];
        for (k, (name, b)) in arch.iter().enumerate() {
            let mut x = b.clone();
            for &i in &idx {
                x.ranks[i] = bundle.skills[i].max.max(1) as u8;
            }
            if moved(&base[k], &all(&x)) {
                moved_in.push(name.clone());
            }
        }
        let ea: BTreeSet<String> = s.effects.iter().filter(|x| effect_matters(x, s.max)).filter_map(|x| x.attribute.clone()).collect();
        let behaviour = s.triggered || !s.program.is_empty() || s.unparsed.is_some();
        // effects on the current Mana/Health/Stamina pools are resource procs; when the skill moves the numbers its
        // proc is what moved them
        let ea: BTreeSet<String> = if moved_in.is_empty() { ea } else { ea.into_iter().filter(|a| !matches!(a.as_str(), "Mana" | "Health" | "Stamina")).collect() };
        let (mut class, unread) = classify(&ea, Some(!moved_in.is_empty()), behaviour && ea.is_empty());
        // a skill with no effects or program unlocks an action (Blink, Double Jump), unless it gates attack steps
        // (Heavy Hitter's faster follow-ups, Snap's merciless finishers) or its text is about damage (Venom Mastery)
        // without naming a key to press: those move no number and stay not counted
        let last = s.desc.last().map(|d| d.replace("**", "").replace('\n', " ")).unwrap_or_default();
        let gates = gates_attacks(bundle, s.id);
        let keyed = last.contains("[key]") || last.starts_with("Unlocks");
        if class == Class::NotCounted && s.effects.is_empty() && s.program.is_empty() && (keyed || !gates && !last.to_lowercase().contains("damage")) {
            class = Class::Unlock;
        }
        let scenario = scenario_reason(s, &ea);
        if class == Class::NotCounted {
            if let Some((outside, why)) = &scenario {
                class = if *outside { Class::OutsideCombat } else { Class::Scenario };
                rep.entries.push(Entry { kind: "skill", key: s.name.clone(), name: s.name.clone(), class, unread: vec![], note: why.clone() });
                continue;
            }
        }
        let mut note = if moved_in.is_empty() { String::new() } else { format!("moves numbers for: {}", moved_in.join(", ")) };
        if class == Class::Unlock {
            note = format!("unlocks an action: {}", s.desc.last().map(|d| d.replace("**", "").replace('\n', " ")).unwrap_or_default());
        } else if class == Class::NotCounted && gates {
            note = format!("gates attack steps that are never in the best combo the planner picks: {last}");
        } else if class == Class::NotCounted && behaviour {
            note = format!("special behaviour not calculated: {last}");
        }
        rep.entries.push(Entry { kind: "skill", key: s.name.clone(), name: s.name.clone(), class, unread, note });
    }

    // perks: effects, skill levels, damage shares, armor shares, triggered behaviour
    for (g, p) in &bundle.perks {
        let ea: BTreeSet<String> = p.effects.iter().filter(|x| effect_matters(x, 1)).filter_map(|x| x.attribute.clone()).collect();
        let behaviour = p.calls.iter().any(|c| matches!(c.as_str(), "ForEachHitEvent" | "ForEachHitEventInStream" | "OnEvent" | "ApplyBuff" | "ApplyDamage" | "CreateDamageSetup" | "ChangeAttribute" | "SetAttribute"));
        let skill_level = p.calls.iter().any(|c| c == "AddSkillLevel");
        let (mut class, unread) = classify(&ea, None, behaviour);
        let mut note = String::new();
        if p.damage.is_some() {
            class = Class::Counted;
            note = "damage-type share".into();
        } else if p.armor.is_some() {
            class = Class::Counted;
            note = "armor share".into();
        } else if skill_level && ea.is_empty() && !behaviour {
            class = Class::Counted;
            note = "adds a skill rank (see that skill's entry)".into();
        } else if let Some(sp) = crate::perk_proc_spec(p) {
            if matches!(sp.event.as_str(), "hit" | "kill") {
                class = Class::Counted;
                note = format!("{} gain per {}", sp.resource, sp.event);
            } else {
                class = Class::Scenario;
                note = format!("{} gain per {} (only when that happens)", sp.resource, sp.event);
            }
        } else if let (true, Some((c, why))) = (class != Class::Counted, perk_reason(p, bundle)) {
            class = c;
            note = why;
        } else if behaviour && class != Class::Counted {
            class = if ea.iter().any(|a| read(a)) { Class::Partly } else { Class::NotCounted };
            note = "triggered behaviour (on hit, on event or buff) not calculated".into();
        }
        rep.entries.push(Entry { kind: "perk", key: g.clone(), name: if p.name.is_empty() { p.debug.clone().unwrap_or_default() } else { p.name.clone() }, class, unread, note });
    }

    // foods and other buff items
    for (g, i) in &bundle.items {
        let Some(bf) = &i.buff else { continue };
        if i.name.is_empty() {
            continue;
        }
        let ea: BTreeSet<String> = bf.effects.iter().filter(|x| effect_matters(x, 1)).filter_map(|x| x.attribute.clone()).collect();
        let (class, unread) = classify(&ea, None, false);
        rep.entries.push(Entry { kind: if bf.slot.as_deref() == Some("Food") { "food" } else { "buff item" }, key: g.clone(), name: i.name.clone(), class, unread, note: String::new() });
    }
    // weapon gems: counted when one archetype's numbers move with the gem in the audit fight; otherwise what they need
    // (Engine::gem_needs) or not modeled
    let e = Engine::new(bundle);
    let t = audit_target();
    // gems also need builds the skill audit has no use for: a Fireball staff, and a bow whose special is unlocked
    let mut gem_arch = arch.clone();
    if let Some((_, staff)) = arch.iter().find(|(n, _)| n == "staff") {
        let mut fire = staff.clone();
        fire.ammo = find_item(bundle, "Fireball IV");
        gem_arch.push(("staff + Fireball".into(), fire));
    }
    let mut bow = Build::sample_bow(bundle);
    if let Some(req) = bow.weapon.as_ref().and_then(|w| bundle.items.get(&w.item)).and_then(|wi| wi.sequences.iter().find(|q| q.trigger == "SpecialAbility")).and_then(|q| q.required_skill.clone()) {
        if let Some(i) = bundle.skills.iter().position(|sk| sk.id == crate::fnv1a_hex(&req)) {
            bow.ranks[i] = bundle.skills[i].max.max(1) as u8;
        }
    }
    gem_arch.push(("bow with its special".into(), bow));
    for (g, gem) in &bundle.gems {
        let name = bundle.items.get(g).map(|i| i.name.clone()).unwrap_or_else(|| gem.debug.clone());
        let mut needs: BTreeSet<&str> = BTreeSet::new();
        let mut counted = vec![];
        for (an, b0) in &gem_arch {
            let mut x = b0.clone();
            x.gem = Some(Equip { item: g.clone(), rarity: None, level: Some(50), perks: None });
            match e.gem_needs(&x, &t) {
                None => counted.push(an.clone()),
                Some(n) => needs.extend(n),
            }
        }
        let (class, note) = if !counted.is_empty() {
            (Class::Counted, format!("moves numbers for: {}", counted.join(", ")))
        } else if !needs.is_empty() {
            (Class::Scenario, format!("counts with {}", needs.into_iter().collect::<Vec<_>>().join(", ")))
        } else {
            (Class::NotCounted, "not modeled by the planner".into())
        };
        rep.entries.push(Entry { kind: "gem", key: g.clone(), name, class, unread: vec![], note });
    }
    // consumables that deal damage (summon scrolls such as Fire Wisp Summon): the bundle has their damage but no
    // companion template, so the planner cannot price them
    for (g, i) in &bundle.items {
        if i.name.is_empty() || i.buff.is_some() || i.category.as_deref() != Some("Consumables") || !i.damage.as_ref().is_some_and(|d| d.dmg_mod > 0.0) {
            continue;
        }
        rep.entries.push(Entry { kind: "damage item", key: g.clone(), name: i.name.clone(), class: Class::NotCounted, unread: vec![], note: "deals damage, but the game data has no template for what it summons, so it is not modeled".into() });
    }
    rep
}

impl Report {
    pub fn counts(&self, kind: &str) -> BTreeMap<String, usize> {
        let mut m = BTreeMap::new();
        for e in self.entries.iter().filter(|e| e.kind == kind) {
            *m.entry(e.class.label().to_string()).or_insert(0) += 1;
        }
        m
    }

    /// Unread combat attributes ranked by how many entities change them.
    pub fn gaps(&self) -> Vec<(String, usize, Vec<String>)> {
        let mut m: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for e in &self.entries {
            for a in &e.unread {
                m.entry(a.clone()).or_default().insert(format!("{} {}", e.kind, e.name));
            }
        }
        let mut v: Vec<(String, usize, Vec<String>)> = m.into_iter().map(|(a, s)| (a, s.len(), s.into_iter().collect())).collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        v
    }

    pub fn markdown(&self) -> String {
        let mut s = String::from("# Coverage\n\nWhether each skill, perk and food changes the planner's numbers. Generated by `ens audit`: every attribute any effect touches is boosted in one test build per weapon type (");
        s += &self.archetypes.join(", ");
        s += "), and every skill is set to full rank, to see which numbers move.\n\n";
        for kind in ["skill", "perk", "food", "buff item", "damage item", "gem"] {
            let c = self.counts(kind);
            if c.is_empty() {
                continue;
            }
            s += &format!("- **{}s**: {}\n", kind, c.iter().map(|(k, n)| format!("{n} {k}")).collect::<Vec<_>>().join(", "));
        }
        s += "\n## Attributes changed by effects but never read by the calculation\n\nRanked by how many skills, perks and foods change them.\n\n| Attribute | Used by | Examples |\n|---|---|---|\n";
        for (a, n, who) in self.gaps().iter().take(60) {
            s += &format!("| {a} | {n} | {} |\n", who.iter().take(4).cloned().collect::<Vec<_>>().join("; "));
        }
        for (kind, title) in [("skill", "Skills"), ("perk", "Perks"), ("food", "Foods"), ("buff item", "Other buff items"), ("damage item", "Items that deal damage"), ("gem", "Weapon gems")] {
            let mut list: Vec<&Entry> = self.entries.iter().filter(|e| e.kind == kind && matches!(e.class, Class::NotCounted | Class::Partly)).collect();
            if list.is_empty() {
                continue;
            }
            list.sort_by(|a, b| a.class.cmp(&b.class).then(a.name.cmp(&b.name)));
            list.dedup_by(|a, b| a.name == b.name && a.class == b.class && a.unread == b.unread);
            s += &format!("\n## {title} not fully counted\n\n| Name | Status | Missing |\n|---|---|---|\n");
            for e in list {
                let miss = if e.unread.is_empty() { e.note.clone() } else { format!("{}{}", e.unread.join(", "), if e.note.is_empty() { String::new() } else { format!(" ({})", e.note) }) };
                s += &format!("| {} | {} | {} |\n", e.name.replace('|', "/"), e.class.label(), miss.replace('|', "/"));
            }
        }
        s
    }

    pub fn json(&self) -> serde_json::Value {
        let mut ents = serde_json::Map::new();
        for e in &self.entries {
            ents.insert(format!("{}:{}", e.kind, e.key), serde_json::json!({"name": e.name, "class": e.class.label(), "unread": e.unread, "note": e.note}));
        }
        serde_json::json!({"archetypes": self.archetypes, "attributes": self.attributes, "entries": ents})
    }
}
