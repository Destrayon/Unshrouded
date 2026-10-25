//! The JSON each part of the page draws: the catalog (fixed game data), the planner state, the rail summary with a
//! hover preview, the Overview, Calculations and the character sheet. Numbers come straight from the engine.

use crate::labels::{attr_label, attr_value, contribution, group, trim_num};
use crate::model::*;
use crate::{Pick, Planner};
use ens_calc::Build;
use ens_opt::{points_available, GOALS};
use serde_json::{json, Value};
use std::collections::HashSet;

fn kind_label(k: &str) -> &str {
    match k {
        "PassiveSkill" => "Passive",
        "ActiveSkill" => "Active",
        x => x,
    }
}

/// The coverage audit (`ens coverage` writes data/coverage.json): what the numbers do with each skill.
static COVERAGE_JSON: &str = include_str!("../../../data/coverage.json");

/// The coverage audit's class for an entry key ("skill:<name>", "gem:<guid>"), when it has one.
pub fn coverage_class(key: &str) -> Option<String> {
    let c: Value = serde_json::from_str(COVERAGE_JSON).ok()?;
    c.get("entries")?.get(key)?.get("class")?.as_str().map(str::to_string)
}

/// A skill's coverage class and note in player words, from the coverage audit; None when the audit has no entry.
pub fn skill_coverage(name: &str) -> Option<(String, String)> {
    static COV: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
    let c = COV.get_or_init(|| serde_json::from_str(COVERAGE_JSON).unwrap_or_default());
    let e = c.get("entries")?.get(format!("skill:{name}"))?;
    let class = e.get("class")?.as_str()?;
    let note = e.get("note").and_then(|x| x.as_str()).unwrap_or("");
    let text = match class {
        "counted" => "Counted in the numbers".to_string(),
        "unlocks an action" => "Unlocks an action; the numbers count the action where the planner prices it (weapon specials)".to_string(),
        "depends on how you play" => "Not in the numbers: it depends on how you play".to_string(),
        "partly counted" => "Partly counted in the numbers".to_string(),
        "not counted" => "Not in the numbers".to_string(),
        _ => "Not in the numbers: it acts outside combat".to_string(),
    };
    let note = if class == "unlocks an action" { String::new() } else { note.to_string() };
    Some((text, note))
}

/// Fixed game data: skills (with positions and links), item lists per slot, enemies, goals.
pub fn catalog(p: &Planner) -> Value {
    let skills: Vec<Value> = p.b.skills.iter().map(|s| {
        let coverage = skill_coverage(&s.name).map(|(t, n)| json!({"text": t, "note": n}));
        let effects: Vec<Value> = s.effects.iter().map(|e| {
            let attr = e.attribute.clone().unwrap_or_default();
            let values: Vec<String> = (1..=s.max.max(1)).map(|l| {
                let v = e.value.as_ref().map(|v| v.eval(&|a| if a == "Level" { l as f64 } else { 0.0 })).unwrap_or(0.0);
                contribution(&attr, &format!("{:?}", e.operation), v)
            }).collect();
            json!({"label": attr_label(&attr), "attr": attr, "values": values, "conditional": !e.when.is_empty()})
        }).collect();
        json!({"id": s.id, "name": s.name, "class": s.class, "kind": s.kind, "kindLabel": kind_label(&s.kind), "cost": s.cost, "max": s.max.max(1),
               "x": s.x, "y": s.y, "desc": s.desc.first().map(|d| d.replace("**", "")).unwrap_or_default(), "effects": effects, "coverage": coverage})
    }).collect();
    let mut edges = vec![];
    for (i, nbrs) in p.opt.adj.iter().enumerate() {
        for &j in nbrs {
            if j > i {
                edges.push([i, j]);
            }
        }
    }
    let l = &p.lists;
    json!({
        "skills": skills, "edges": edges,
        "lists": {"weapon": l.weapons, "ammo": l.ammo, "gear": l.gear, "food": l.foods, "gem": l.gems, "summon": l.summons},
        "enemies": p.enemies.iter().map(|e| e.label.clone()).collect::<Vec<_>>(),
        // the 1.0 caps every level field is held to (from the bundle and ens_calc::build)
        "caps": {"level": ens_calc::build::level_cap(p.b), "item": ens_calc::build::item_level_cap(p.b), "flame": ens_calc::build::FLAME_LEVEL_CAP},
        "armorSlots": ARMOR_SLOTS, "goals": GOALS.iter().map(|g| g.label()).collect::<Vec<_>>(),
        "game": format!("{} · {} skill nodes · {} perks · {} items", game_data(p.b).replace(", ", " · "), p.b.skills.len(), p.b.perks.len(), p.b.items.len()),
    })
}

fn slot_info(p: &Planner, b: &Build, slot: Slot) -> Value {
    let id = match slot {
        Slot::Weapon => "weapon".to_string(),
        Slot::Ammo => "ammo".into(),
        Slot::Gem => "gem".into(),
        Slot::Armor(k) => format!("armor{k}"),
        Slot::Food => "food".into(),
        Slot::Summon => "summon".into(),
    };
    let mut v = json!({"id": id, "name": slot.name()});
    match slot {
        Slot::Food | Slot::Summon => {
            let list = if slot == Slot::Food { &b.foods } else { &b.summons };
            v["items"] = json!(list.iter().map(|g| {
                let mut x = json!({"guid": g, "name": p.item_name(g)});
                // an upgradable (Eternal) summon spell: the level it is priced at and the range its upgrades reach
                if let Some(it) = p.b.items.get(g).filter(|i| slot == Slot::Summon && ens_calc::upgradable(i)) {
                    let rar = it.rarity.clone().unwrap_or_else(|| "Legendary".into());
                    let r = it.levels.as_ref().map(|r| (r.min, r.max)).unwrap_or((1, 1));
                    x["level"] = json!(p.engine.ammo_level(b, g, it));
                    x["levelMin"] = json!(r.0);
                    x["levelMax"] = json!((r.1 + ens_calc::unlocked_perks(it, &rar, None)).min(ens_calc::build::item_level_cap(p.b)));
                }
                x
            }).collect::<Vec<_>>());
            v["cap"] = json!(if slot == Slot::Food { 4 } else { 3 });
        }
        _ => {
            let Some(g) = equipped(b, slot) else { return v };
            let it = p.b.items.get(&g);
            // an upgradable (Eternal) arrow or spell shows the upgrade state it is priced at
            let up = (slot == Slot::Ammo && it.is_some_and(ens_calc::upgradable)).then(|| b.upgrade_of(&g));
            let e = match slot {
                Slot::Weapon => b.weapon.as_ref(),
                Slot::Gem => b.gem.as_ref(),
                Slot::Armor(k) => b.gear[k].as_ref(),
                _ => up.as_ref(),
            };
            let rar = e.and_then(|e| e.rarity.clone()).or(it.and_then(|i| i.rarity.clone())).unwrap_or_default();
            v["item"] = json!({"guid": g, "name": p.item_name(&g), "rarity": rar});
            if let (Some(e), Some(it)) = (e, it) {
                let lvl = if slot == Slot::Ammo { p.engine.ammo_level(b, &g, it) } else { e.level.unwrap_or(b.level) };
                v["item"]["level"] = json!(lvl);
                if slot != Slot::Gem {
                    let rr = if rar.is_empty() { "Legendary".to_string() } else { rar.clone() };
                    let max = ens_calc::unlocked_perks(it, &rr, None);
                    let n = ens_calc::perk_count(e, &rr);
                    v["item"]["perksMax"] = json!(max);
                    v["item"]["perks"] = json!(e.perks.unwrap_or(max).min(max));
                    let perks: Vec<Value> = it.perks.iter().filter(|x| !x.is_empty()).enumerate()
                        .map(|(k, pid)| json!({"name": p.b.perks.get(pid).map(|x| x.name.clone()).unwrap_or_else(|| "Unknown perk".into()), "on": k < n})).collect();
                    v["item"]["perkList"] = json!(perks);
                }
                if slot == Slot::Weapon {
                    v["item"]["damage"] = json!(ens_calc::item_damage(p.b, it, lvl));
                }
            }
        }
    }
    v
}

/// Everything the page needs to draw the controls: build, loadouts, fight settings, equipped items.
pub fn state(p: &mut Planner) -> Value {
    let b = p.build.clone();
    let spent = b.points(p.b);
    let regions: Vec<Value> = ["Ranger", "Warrior", "Mage"].iter().map(|c| {
        json!({"name": c, "points": p.b.skills.iter().enumerate().filter(|(_, s)| s.class == *c).map(|(i, s)| b.ranks[i] as u32 * s.cost).sum::<u32>()})
    }).collect();
    let mut slots = vec![];
    for s in [Slot::Weapon, Slot::Ammo, Slot::Gem] {
        slots.push(slot_info(p, &b, s));
    }
    for k in 0..7 {
        slots.push(slot_info(p, &b, Slot::Armor(k)));
    }
    slots.push(slot_info(p, &b, Slot::Food));
    slots.push(slot_info(p, &b, Slot::Summon));
    let mut seen = HashSet::new();
    let mut allocated = vec![];
    for (i, s) in p.b.skills.iter().enumerate() {
        if b.ranks[i] > 0 && s.kind != "Attribute" && seen.insert(s.name.clone()) {
            allocated.push(json!({"name": s.name, "class": s.class, "ranks": p.total_ranks(&s.name)}));
        }
    }
    let c = p.conditions();
    let carried: Vec<Value> = b.carried.iter().enumerate().map(|(k, w)| {
        let mut nb = b.clone();
        nb.use_carried(k);
        let it = p.b.items.get(&w.weapon.item);
        json!({
            "name": p.item_name(&w.weapon.item),
            "kind": p.weapon_kind(&w.weapon.item).unwrap_or_default(),
            "rarity": w.weapon.rarity.clone().or(it.and_then(|i| i.rarity.clone())).unwrap_or_default(),
            "level": w.weapon.level.unwrap_or(b.level),
            "gem": w.gem.as_ref().map(|g| p.item_name(&g.item)),
            "dps": summarize(&p.engine, &nb, &c).dps,
        })
    }).collect();
    let gem = b.gem.as_ref().map(|g| p.item_name(&g.item)).unwrap_or_default();
    let companions = !b.summons.is_empty() || gem.contains("Necromancy") || gem.contains("Skulls") || p.total_ranks("Necromancer") > 0;
    json!({
        "buildName": p.build_name, "loadoutName": b.name, "current": p.current,
        "loadouts": p.loadouts.iter().enumerate().map(|(k, l)| if k == p.current { json!({"name": b.name, "level": b.level}) } else { json!({"name": l.name, "level": l.level}) }).collect::<Vec<_>>(),
        "level": b.level, "flame": b.flame_level, "rested": b.rested, "heavy": b.heavy, "spent": spent, "have": points_available(b.level, b.found_points), "foundPoints": b.found_points,
        "ranks": b.ranks, "regions": regions, "allocated": allocated,
        // everything the numbers depend on (the whole build and the fight): views that cache computed numbers key on it
        "calcKey": key_of(&b, &p.fight),
        "canUndo": !p.undo.is_empty(), "canRedo": !p.redo.is_empty(), "notice": p.notice.take(),
        "hitKindProblem": p.engine.hit_kind_problem(&b, &p.fight.hit_kind),
        "weaponKind": b.weapon.as_ref().and_then(|w| p.weapon_kind(&w.item)),
        "fight": p.fight, "fightChanges": p.fight.changed(), "slots": slots, "notes": p.notes,
        "ranking": p.ranking.iter().map(|g| g.label()).collect::<Vec<_>>(),
        "gem": gem, "companions": companions, "carried": carried,
        "owned": p.owned.len(),
        "ownedFrom": p.owned_from.as_ref().map(|o| json!({"character": o.character, "loaded": o.loaded})),
        "removable": (0..b.ranks.len()).map(|i| can_remove(p.opt, &b, i)).collect::<Vec<_>>(),
        "enemyLabel": p.enemies[p.fight.enemy.min(p.enemies.len() - 1)].label,
        "opt": p.opt_result.as_ref().map(|_| true),
    })
}

/// The rail numbers, plus the numbers with a previewed change: {"node": i} (allocate its path, or remove a rank of
/// a full node) or {"slot": id, "guid": g}.
pub fn summary(p: &mut Planner, preview: Option<&Value>) -> Value {
    let now = p.summary();
    let mut out = json!({"now": now});
    let Some(pv) = preview else { return out };
    let mut path: Vec<usize> = vec![];
    let built: Option<(Build, String)> = if let Some(i) = pv.get("node").and_then(|v| v.as_u64()).map(|v| v as usize).filter(|i| *i < p.b.skills.len()) {
        let s = &p.b.skills[i];
        if let Some(pt) = path_to(p.opt, &p.build.ranks, i) {
            let pts: u32 = pt.iter().map(|&j| p.b.skills[j].cost).sum();
            let what = if pt.len() > 1 { format!("Allocate {} ({} nodes, {} points)", s.name, pt.len(), pts) } else { format!("Add a rank of {} ({} points)", s.name, pts) };
            path = pt.clone();
            Some((with_path(&p.build, &pt), what))
        } else if p.build.ranks[i] > 0 && can_remove(p.opt, &p.build, i) {
            let mut nb = p.build.clone();
            nb.ranks[i] -= 1;
            Some((nb, format!("Remove a rank of {}", s.name)))
        } else {
            None
        }
    } else if let (Some(slot), Some(g)) = (pv.get("slot").and_then(|v| v.as_str()).and_then(Slot::parse), pv.get("guid").and_then(|v| v.as_str())) {
        let verb = if matches!(slot, Slot::Food | Slot::Summon) { "Add" } else { "Equip" };
        let owned = pv.get("owned").and_then(|v| v.as_u64()).and_then(|k| p.owned.get(k as usize));
        let nb = match owned {
            Some(o) => crate::game::with_owned(p.b, &p.build, slot, o),
            None => apply_slot(p.b, &p.build, slot, g),
        };
        Some((nb, format!("{verb} {}", p.item_name(g))))
    } else {
        None
    };
    if let Some((nb, what)) = built {
        let c = p.conditions();
        out["preview"] = json!(summarize(&p.engine, &nb, &c));
        out["label"] = json!(what);
        out["path"] = json!(path);
    }
    out
}

pub fn overview(p: &mut Planner) -> Value {
    let s = p.summary();
    let enemy = p.enemies[p.fight.enemy.min(p.enemies.len() - 1)].label.clone();
    let ins = insights(p.b, &p.build, &s, &enemy, p.opt.connected(&p.build.ranks), p.engine.hit_kind_problem(&p.build, &p.fight.hit_kind));
    let mut out = json!({"summary": s, "insights": ins, "incoming": p.fight.incoming_type});
    out["presets"] = ens_calc::build::PRESETS.iter().map(|(l, k)| json!({"label": l, "kind": k})).collect();
    out["hasWeapon"] = json!(p.build.weapon.is_some());
    // every loadout under this fight, side by side (the open one from its live build)
    let c = p.conditions();
    out["loadouts"] = json!(p.loadouts.iter().enumerate().map(|(k, l)| {
        let b = if k == p.current { &p.build } else { l };
        let s = summarize(&p.engine, b, &c);
        let weapon = b.weapon.as_ref().map(|w| p.item_name(&w.item)).unwrap_or_else(|| "no weapon".into());
        json!({"name": if k == p.current { p.build.name.clone() } else { l.name.clone() }, "current": k == p.current, "level": b.level,
               "weapon": weapon, "dps": s.dps, "hit": s.expected_hit, "crit": s.crit, "health": s.health, "survived": s.hits_survived})
    }).collect::<Vec<_>>());
    if let Some(w) = p.build.weapon.clone() {
        let t = p.conditions().target;
        let h = p.engine.hit(&p.build, &w, p.build.ammo.as_deref(), 1.0, &t);
        let (secs, how) = p.engine.attack_interval(&p.build, &w, p.build.ammo.as_deref());
        let d = p.engine.dps(&p.build, &w, p.build.ammo.as_deref(), &t, 1.0 / secs.max(0.05));
        let wname = p.item_name(&w.item);
        let mut chain = vec![];
        if h.ammo_damage > 0.0 {
            let aname = p.build.ammo.as_deref().map(|g| p.item_name(g)).unwrap_or_default();
            chain.push(json!({"label": "Weapon + ammo", "value": trim_num(h.item_damage + h.ammo_damage), "tip": format!("{wname} {} + {aname} {}", trim_num(h.item_damage), trim_num(h.ammo_damage))}));
        } else {
            chain.push(json!({"label": "Weapon", "value": trim_num(h.item_damage), "tip": wname}));
        }
        let mut pool = 1.0;
        let mut pool_tip = vec![];
        for (k, v) in &h.pool_terms {
            if let Some(name) = k.strip_prefix('×') {
                let name = name.trim();
                let label = if name.contains("scale") { name.replace(" scale", "").replace(['(', ')'], "") } else { attr_label(name) };
                chain.push(json!({"op": "×", "label": label, "value": format!("{v:.2}"), "tip": name}));
            } else if *v != 0.0 {
                pool += v;
                pool_tip.push(format!("{} {:+.0}%", attr_label(k.trim_start_matches('+').trim()), v * 100.0));
            }
        }
        if !pool_tip.is_empty() {
            chain.push(json!({"op": "×", "label": "Damage bonuses", "value": format!("{pool:.2}"), "tip": format!("These bonuses add together into one pool, which then multiplies the hit once (the × tiles before it multiply each other):\n{}", pool_tip.join("\n"))}));
        }
        let types: Vec<String> = h.per_type.iter().filter(|(_, v)| *v > 0.05).map(|(k, v)| format!("{v:.1} {k}")).collect();
        chain.push(json!({"op": "→", "label": "By damage type", "value": types.join(" + ")}));
        if h.crit_chance > 0.0 && h.non_crit > 0.0 {
            chain.push(json!({"op": "×", "label": format!("Crit {:.0}%", h.crit_chance * 100.0), "value": format!("{:.3}", h.expected / h.non_crit), "tip": format!("A critical hit adds {:.0}% to the damage bonus pool; this tile is the average over your crit chance", (h.crit_damage_mod - 1.0) * 100.0)}));
        }
        if h.expected > 0.0 && (h.after_armor / h.expected - 1.0).abs() > 1e-4 {
            chain.push(json!({"op": "×", "label": "Enemy armour", "value": format!("{:.3}", h.after_armor / h.expected)}));
        }
        chain.push(json!({"op": "=", "label": "Expected hit", "value": format!("{:.1}", h.after_armor), "strong": true}));
        out["chain"] = json!(chain);
        out["rate"] = json!({"projectiles": d.projectiles, "secs": secs, "how": how, "dps": d.total});
    }
    out
}

/// Collapsible sections of the Calculations tab: rows of [label, value] with optional source lines and emphasis.
pub fn calcs(p: &mut Planner) -> Value {
    let s = p.summary();
    let Some(w) = p.build.weapon.clone() else { return json!({"sections": []}) };
    let t = p.conditions().target;
    let h = p.engine.hit(&p.build, &w, p.build.ammo.as_deref(), 1.0, &t);
    let (secs, how) = p.engine.attack_interval(&p.build, &w, p.build.ammo.as_deref());
    let d = p.engine.dps(&p.build, &w, p.build.ammo.as_deref(), &t, 1.0 / secs.max(0.05));
    let tot = p.engine.total(&p.build, &t);
    let st = p.engine.attributes(&p.build);
    let row = |a: String, b: String| json!({"label": a, "value": b});
    let strong = |a: &str, b: String| json!({"label": a, "value": b, "strong": true});
    let wname = p.item_name(&w.item);
    let mut hit = vec![
        row(format!("Weapon base damage at item level {} (every weapon's starting damage at that level)", w.level.unwrap_or(p.build.level)), trim_num(h.base_damage)),
        row(format!("{wname} damage"), trim_num(h.item_damage)),
    ];
    if let Some(a) = p.build.ammo.as_deref() {
        hit.push(row(format!("+ {} damage", p.item_name(a)), trim_num(h.ammo_damage)));
    }
    for (k, v) in &h.pool_terms {
        if let Some(name) = k.strip_prefix('×') {
            let name = name.trim();
            let label = if name.contains("scale") { format!("× {} scaling", name.replace(" scale", "").replace(['(', ')'], "")) } else { format!("× {}", attr_label(name)) };
            hit.push(row(label, format!("× {v:.3}")));
        } else if *v != 0.0 {
            let key = k.trim_start_matches('+').trim();
            let subs: Vec<String> = st.sources(key).iter().map(|c| format!("{}  {}", contribution(key, &format!("{:?}", c.op), c.value), c.source)).collect();
            hit.push(json!({"label": format!("+ {} (adds to the damage bonus pool, which multiplies the hit once)", attr_label(key)), "value": format!("{:+.1}%", v * 100.0), "sub": subs}));
        }
    }
    for (k, v) in &h.per_type {
        hit.push(row(format!("{k} damage"), format!("{v:.1}")));
    }
    hit.push(row("Non-crit hit".into(), format!("{:.1}", h.non_crit)));
    hit.push(row(format!("Crit {:.1}%, each adding {:.0}% to the pool", h.crit_chance * 100.0, (h.crit_damage_mod - 1.0) * 100.0), format!("{:.1} on average", h.expected)));
    hit.push(strong("Expected hit after enemy armour", format!("{:.1}", h.after_armor)));

    let mut rate = vec![row("Attack cycle".into(), format!("{secs:.2} s")), row("How the cycle is made".into(), how.clone()), row("Projectiles per attack".into(), format!("{:.2}", d.projectiles))];
    if d.aoe_radius > 0.0 {
        rate.push(row("Area effect".into(), format!("{:.1} m blast × {:.1} → {:.2} enemies", d.aoe_radius, d.aoe_attack_mod, d.aoe_targets)));
    }
    // multipliers on the regular hits that are not a step above: a wand's skill-gated extra projectiles, a zone's
    // repeated hits on each enemy
    let wf = p.engine.wand_spawn_factor(&p.build, &w);
    if (wf - 1.0).abs() > 1e-9 {
        rate.push(row("Extra wand projectiles from skills (chance × attack mod from the attack's data; Arcane Proliferation's tooltip says 30% at half damage, its data 15% at 0.25 on the Lightning chain)".into(), format!("× {wf:.3}")));
    }
    // the heavy multiplier comes from the game's Heavy sequences; the official notes describe heavy attacks as double
    // damage, and no measurement separates the two yet (R8-PS-02)
    if p.build.heavy && p.engine.melee_combo(&p.build, &w).is_some_and(|c| c.names.iter().all(|x| x.contains("_Heavy_"))) {
        let am = p.engine.melee_combo(&p.build, &w).map(|c| c.mean_attack_mod()).unwrap_or(1.0);
        rate.push(row("Heavy attack damage (from the game data's heavy attacks)".into(), format!("× {am:.2}")));
    }
    if d.zone_hits > 0.0 {
        rate.push(row("Zone hits on each enemy per cast (every 0.5 s while it stays inside, capped by zones alive)".into(), format!("× {:.2}", d.zone_hits)));
    }
    rate.push(row("Regular DPS".into(), format!("{:.1}", d.regular_dps)));
    if d.special_projectiles > 0.0 {
        rate.push(row("Special: Focus per hit / cost (Focus fills as you hit and pays for the weapon's special attack)".into(), format!("{} / {}", d.focus_per_hit, d.focus_cost)));
        rate.push(row("Special: projectiles × attack mod".into(), format!("{:.2} × {:.1}", d.special_projectiles, d.special_attack_mod)));
        rate.push(row("Special uses per second".into(), format!("{:.3}", d.special_per_s)));
        rate.push(row("Special DPS".into(), format!("{:.1}", d.special_dps)));
    }
    rate.push(strong("DPS while attacking", format!("{:.1}", d.total)));

    let mut mana = vec![
        row("Mana pool".into(), format!("{:.0}", p.engine.sheet(&p.build).values.get("Mana").copied().unwrap_or(0.0))),
        row("Regeneration".into(), format!("{} ({:.2} /s)", trim_num(p.engine.sheet(&p.build).values.get("Mana_Recharge").copied().unwrap_or(0.0)), s.mana_regen)),
        row("The attack needs".into(), format!("{:.2} /s", tot.main_mana_per_s)),
    ];
    let summon_mana: f64 = tot.summons.iter().map(|x| x.mana_per_s * x.uptime).sum();
    if summon_mana > 0.0 {
        mana.push(row("Summons need (paid first)".into(), format!("{summon_mana:.2} /s")));
    }
    for pr in tot.procs.iter().filter(|p| p.resource == "Mana") {
        mana.push(row(format!("+ {}", pr.skill), format!("{:+.2} /s", pr.per_s)));
    }
    mana.push(row(format!("Uptime over a {:.0} s fight", p.fight.fight_s), format!("{:.0}%", tot.mana_uptime * 100.0)));
    if tot.casting_share > 0.0 {
        mana.push(row("Time spent casting summons".into(), format!("{:.0}% (your attack runs in the rest)", tot.casting_share * 100.0)));
    }
    if let Some((n, fd)) = &tot.fallback {
        mana.push(row(format!("Out of mana: {n}"), format!("{fd:.1} DPS")));
    }
    mana.push(strong("Total DPS", format!("{:.1}", tot.total)));

    let mut extra = vec![];
    for (n, dd, how) in &tot.skill_damage {
        extra.push(row(n.clone(), format!("+{dd:.1} DPS ({how})")));
    }
    for (n, u, with) in &tot.buffs {
        extra.push(row(format!("{n} (buff)"), format!("up {:.0}% · {with:.1} DPS while up", u * 100.0)));
    }
    for (n, why) in &tot.not_counted {
        extra.push(row(format!("{n} (not counted)"), format!("adds 0: {why}")));
    }
    // what the planner does not model at all, where the numbers are read (also in Calculation Proof.md)
    for (n, why) in ens_calc::proof::NOT_MODELED {
        extra.push(row(format!("{n} (not in the numbers)"), why.to_string()));
    }
    // a gem that adds nothing in this fight says what it needs (or that it is not modeled)
    if let Some(needs) = p.engine.gem_needs(&p.build, &t) {
        let name = p.build.gem.as_ref().map(|g| p.item_name(&g.item)).unwrap_or_default();
        let modeled = p.build.gem.as_ref().is_some_and(|g| coverage_class(&format!("gem:{}", g.item)).is_some_and(|c| c != "not counted"));
        let why = if !needs.is_empty() {
            format!("it counts with {} (Combat setup)", needs.join(", "))
        } else if modeled {
            "it acts on things this build does not have (a weapon special, another spell or weapon)".to_string()
        } else {
            "the planner does not model this gem".to_string()
        };
        extra.push(row(format!("{name} (not counted)"), format!("adds 0 in this fight: {why}")));
    }
    for pr in &tot.procs {
        let what = if pr.orbs > 0.0 {
            format!("{:.0}% per {} → {:.2} {} orbs of {:.0} → +{:.2}/s", pr.chance * 100.0, pr.event, pr.orbs, pr.resource, pr.amount, pr.per_s)
        } else {
            format!("+{:.2} {}/s ({:.0}% per {})", pr.per_s, pr.resource, pr.chance * 100.0, pr.event)
        };
        extra.push(row(pr.skill.clone(), what));
    }
    let summons: Vec<Value> = tot.summons.iter().map(|x| row(format!("{} ×{:.1}", x.name, x.count), format!("{:.0} per hit · {} → {:.1} DPS each, up {:.0}% → {:.1} DPS", x.hit, x.note, x.each_dps, x.uptime * 100.0, x.total_dps))).collect();
    let hp = ens_calc::enemy_health(p.b, p.fight.enemy_level, p.conditions().hp_factor);
    let fight = vec![
        row("Enemy health (game formula, solo)".into(), format!("{hp:.0}")),
        row("Time to kill".into(), format!("{:.1} s", hp / tot.total.max(0.01))),
        row("Enemy's biggest hit".into(), format!("{:.0}", s.hit_raw)),
        row("After your armour and resistances".into(), format!("{:.0}", s.hit_taken)),
        row("Your health".into(), format!("{:.0}", s.health)),
        strong("Hits survived", format!("{:.1}", s.hits_survived)),
    ];
    let nx = extra.len();
    let out = json!({"sections": [
        {"id": "hit", "name": "One hit", "summary": format!("{:.1} expected", h.after_armor), "open": true, "rows": hit},
        {"id": "rate", "name": "Attack rate", "summary": format!("{:.2} projectiles every {secs:.2} s", d.projectiles), "rows": rate},
        {"id": "mana", "name": "Mana budget", "summary": if tot.main_mana_per_s > 0.0 { format!("{:.0}% uptime", tot.main_uptime * 100.0) } else { "no mana cost".into() }, "warn": tot.main_mana_per_s > 0.0 && tot.main_uptime < 0.6, "rows": mana},
        {"id": "extra", "name": "Gems, skills, on-hit effects, buffs", "summary": if nx == 0 { "none".to_string() } else { format!("{nx} active") }, "rows": extra, "empty": "Nothing in this build deals its own damage or triggers on hits."},
        {"id": "summons", "name": "Summons", "summary": if summons.is_empty() { "none".to_string() } else { format!("{:.1} DPS", tot.summons.iter().map(|x| x.total_dps).sum::<f64>()) }, "rows": summons, "empty": "No summons, totems or companions in this build."},
        {"id": "fight", "name": "Fight and survival", "summary": format!("{:.1} s to kill · {:.1} hits survived", hp / tot.total.max(0.01), s.hits_survived), "rows": fight},
    ]});
    out
}

/// The game data every number is read from (bundle meta: version and build).
pub fn game_data(b: &ens_data::Bundle) -> String {
    let m = |k: &str| b.meta.get(k).map(|v| v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string())).unwrap_or_else(|| "?".into());
    format!("Enshrouded {}, build {}", m("version"), m("build"))
}

/// Character sheet rows grouped as the game would: label, value, raw key, and every source that sets it.
pub fn sheet(p: &Planner, query: &str, all: bool) -> Value {
    let st = p.engine.attributes(&p.build);
    let sheet = p.engine.sheet(&p.build);
    let mut keys: Vec<String> = sheet.values.keys().cloned().collect();
    for c in &st.contributions {
        if !keys.contains(&c.attribute) {
            keys.push(c.attribute.clone());
        }
    }
    let q = query.to_lowercase();
    let main = ["Health", "Mana", "Stamina", "Strength", "Dexterity", "Intelligence", "Constitution", "Endurance", "Spirit", "CritChance", "CritDamageMod", "Health_Recharge", "Mana_Recharge", "Stamina_Recharge", "ArmorPoints_Physical", "ArmorPoints_Magical"];
    let mut rows: Vec<(usize, String, Value)> = keys.into_iter().filter_map(|k| {
        let srcs = st.sources(&k);
        let shown_regen = matches!(k.as_str(), "Health_Recharge" | "Mana_Recharge" | "Stamina_Recharge");
        if !(all || !srcs.is_empty() || main.contains(&k.as_str())) || (k.ends_with("_Recharge") && !shown_regen && !all) {
            return None;
        }
        let label = attr_label(&k);
        if !q.is_empty() && !label.to_lowercase().contains(&q) && !k.to_lowercase().contains(&q) {
            return None;
        }
        let v = sheet.values.get(&k).copied().unwrap_or_else(|| st.get(&k));
        let g = group(&k);
        let order = ["Attributes", "Resources", "Offence", "Defence", "Other"].iter().position(|x| *x == g).unwrap_or(4);
        let sources: Vec<String> = srcs.iter().map(|c| format!("{}  {}", contribution(&k, &format!("{:?}", c.op), c.value), c.source)).collect();
        // the game shows Regeneration as the attribute; the pool refills 0.2 × that per second (regen_per_s)
        let value = match k.strip_suffix("_Recharge").filter(|_| shown_regen) {
            Some(pool) => format!("{} ({:.2} /s)", trim_num(v), p.engine.regen_per_s(&st, pool)),
            None => attr_value(&k, v, p.engine.attrs.base(&k)),
        };
        Some((order, label.clone(), json!({"group": g, "label": label, "value": value, "key": k, "sources": sources})))
    }).collect();
    rows.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    json!(rows.into_iter().map(|r| r.2).collect::<Vec<_>>())
}

/// Item guids that fit a slot for this build.
/// The candidates for `slot`. Weapons keep to the type in hand unless `all_types` (A-vs-B across types).
pub fn candidate_guids(p: &Planner, slot: Slot, all_types: bool) -> Vec<String> {
    let l = &p.lists;
    let picks: Vec<&Pick> = match slot {
        Slot::Weapon => {
            let kind = p.build.weapon.as_ref().filter(|_| !all_types).and_then(|w| p.weapon_kind(&w.item));
            l.weapons.iter().filter(|x| kind.is_none() || p.weapon_kind(&x.guid) == kind).collect()
        }
        Slot::Ammo => {
            let bow = p.build.weapon.as_ref().and_then(|w| p.weapon_kind(&w.item)).as_deref() == Some("RangedBow");
            let summons: HashSet<&String> = l.summons.iter().map(|x| &x.guid).collect();
            l.ammo.iter().filter(|x| {
                let it = &p.b.items[&x.guid];
                let arrow = it.slot.as_deref() == Some("Arrow") || it.ammo.as_deref() == Some("Arrow");
                arrow == bow && !summons.contains(&x.guid)
            }).collect()
        }
        Slot::Gem => l.gems.iter().collect(),
        Slot::Armor(k) => l.gear[k].iter().collect(),
        Slot::Food => l.foods.iter().filter(|x| !p.build.foods.contains(&x.guid)).collect(),
        Slot::Summon => l.summons.iter().filter(|x| !p.build.summons.contains(&x.guid)).collect(),
    };
    picks.into_iter().map(|x| x.guid.clone()).collect()
}

pub fn opt_result(p: &mut Planner) -> Value {
    let now = p.summary();
    let Some(o) = &p.opt_result else { return Value::Null };
    let used: u32 = p.b.skills.iter().enumerate().map(|(i, s)| o.ranks[i] as u32 * s.cost).sum();
    let mut goals: Vec<(&&str, &f64)> = o.goals.iter().collect();
    goals.sort_by(|a, b| a.0.cmp(b.0));
    let gear = o.gear.as_ref().map(|(w, g)| {
        let name = |e: &Option<ens_calc::Equip>| e.as_ref().map(|e| (p.item_name(&e.item), e.rarity.clone().or(p.b.items.get(&e.item).and_then(|i| i.rarity.clone())).unwrap_or_default()));
        let mut rows = vec![json!({"slot": "Weapon", "item": name(w), "changed": w.as_ref().map(|e| &e.item) != p.build.weapon.as_ref().map(|e| &e.item)})];
        for (k, s) in ARMOR_SLOTS.iter().enumerate() {
            rows.push(json!({"slot": s, "item": name(&g[k]), "changed": g[k].as_ref().map(|e| &e.item) != p.build.gear[k].as_ref().map(|e| &e.item)}));
        }
        rows
    });
    json!({
        "now": now, "proposed": o.summary, "used": used, "have": points_available(p.build.level, p.build.found_points),
        "goals": goals.into_iter().map(|(k, v)| json!({"name": k, "value": v})).collect::<Vec<_>>(),
        "gear": gear, "steps": o.order.iter().map(|&i| p.b.skills[i].name.clone()).collect::<Vec<_>>(),
    })
}
