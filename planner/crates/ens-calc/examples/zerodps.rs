use ens_calc::{Build, Engine, Equip, Target};
fn main() {
    let b = ens_data::Bundle::load("data/bundle.json").unwrap();
    let e = Engine::new(&b);
    let mut seen = std::collections::BTreeSet::new();
    let mut zero = vec![];
    for (g, i) in &b.items {
        if i.name.is_empty() || i.category.as_deref() != Some("Weapons") || i.damage.is_none() || !seen.insert(i.name.clone()) { continue; }
        let k = i.weapon_category.as_ref().and_then(|c| b.weapon_categories.get(c)).and_then(|c| c.kind.clone()).unwrap_or_default();
        let mut bd = Build::new(&b, 50);
        bd.weapon = Some(Equip { item: g.clone(), rarity: Some("Legendary".into()), level: Some(50), perks: None });
        if k == "RangedBow" { bd.ammo = ens_calc::build::find_item(&b, "Shroud Arrow III"); }
        if k == "RangedStaff" { bd.ammo = ens_calc::build::find_item(&b, "Ice Bolt IV"); }
        let t = e.total(&bd, &Target::default());
        if t.total <= 0.0 { zero.push(format!("{} [{k}] dmg {:?}", i.name, i.damage.as_ref().map(|d| (d.speed_mod, d.dmg_mod)))); }
    }
    println!("{} zero-DPS weapons:\n{}", zero.len(), zero.join("\n"));
}
