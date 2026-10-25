//! Every weapon with compatible ammunition or spells, at item levels 1, 25 and 50, must give finite, non-negative
//! numbers (hit, attack cycle, DPS, mana use) against a plain target and a resistant, armored Fell target.
use ens_calc::{Build, Engine, Equip, Target};
use std::collections::{BTreeMap, HashMap};

#[test]
fn every_weapon_and_ammo_gives_sane_numbers() {
    let b = ens_data::Bundle::load(concat!(env!("CARGO_MANIFEST_DIR"), "/../../data/bundle.json")).unwrap();
    let e = Engine::new(&b);
    let kind = |g: &str| b.items.get(g).and_then(|i| i.weapon_category.as_ref()).and_then(|c| b.weapon_categories.get(c)).and_then(|c| c.kind.clone()).unwrap_or_default();
    // one weapon per name
    let mut weapons: BTreeMap<String, String> = BTreeMap::new();
    for (g, i) in &b.items {
        if !i.name.is_empty() && i.category.as_deref() == Some("Weapons") && i.damage.is_some() {
            weapons.entry(i.name.clone()).or_insert_with(|| g.clone());
        }
    }
    let mut arrows: Vec<String> = b.items.iter().filter(|(_, i)| !i.name.is_empty() && i.ammo.as_deref() == Some("Arrow") && i.category.as_deref() == Some("Ammunition")).map(|(g, _)| g.clone()).collect();
    let mut spells: Vec<String> = b.items.iter().filter(|(_, i)| !i.name.is_empty() && i.slot.as_deref() == Some("Spell")).map(|(g, _)| g.clone()).collect();
    arrows.sort();
    spells.sort();
    let mut sus = HashMap::new();
    for (k, v) in [("Physical", 1.2), ("Pierce", 0.5), ("MagBase", 0.8), ("Fire", 1.5), ("SkillShot", 1.5)] {
        sus.insert(k.to_string(), v);
    }
    let targets = [
        Target::default(),
        Target { susceptibility: sus, armor_physical: 200.0, armor_magical: 200.0, faction: Some("Fogger".into()), enemy_class: Some("Mage".into()), pack_size: 4.0, pack_radius: 5.0, enemy_health: 3000.0, orb_pickup: 0.5, ..Default::default() },
    ];
    let mut bad = vec![];
    let mut checked = 0;
    for (name, g) in &weapons {
        let k = kind(g);
        let ammo: Vec<Option<String>> = match k.as_str() {
            "RangedBow" => arrows.iter().step_by(arrows.len().max(1) / 6 + 1).cloned().map(Some).collect(),
            "RangedStaff" => spells.iter().step_by(spells.len().max(1) / 8 + 1).cloned().map(Some).collect(),
            _ => vec![None],
        };
        for a in &ammo {
            for lvl in [1u32, 25, 50] {
                let mut bd = Build::new(&b, lvl);
                bd.weapon = Some(Equip { item: g.clone(), rarity: Some("Legendary".into()), level: Some(lvl), perks: None });
                bd.ammo = a.clone();
                for t in &targets {
                    let tot = e.total(&bd, t);
                    let w = bd.weapon.as_ref().unwrap();
                    let (secs, _) = e.attack_interval(&bd, w, bd.ammo.as_deref());
                    let vals = [tot.total, tot.main.total, tot.main.hit.expected, tot.main.hit.after_armor, tot.main_mana_per_s, tot.main_uptime, secs];
                    checked += 1;
                    if vals.iter().any(|v| !v.is_finite() || *v < 0.0) || secs <= 0.0 || tot.main_uptime > 1.0 + 1e-9 {
                        bad.push(format!("{name} [{k}] ammo {:?} L{lvl}: {vals:?}", a.as_ref().and_then(|x| b.items.get(x)).map(|i| i.name.clone())));
                    }
                }
            }
        }
    }
    assert!(checked > 1000, "only {checked} combinations");
    assert!(bad.is_empty(), "{} bad of {checked}:\n{}", bad.len(), bad.iter().take(30).cloned().collect::<Vec<_>>().join("\n"));
}
