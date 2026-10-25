use ens_calc::{audit, Engine};
fn main() {
    let b = ens_data::Bundle::load("data/bundle.json").unwrap();
    let e = Engine::new(&b);
    let arch = audit::archetypes(&b);
    let (_, bow) = &arch[0];
    let idx: Vec<usize> = b.skills.iter().enumerate().filter(|(_, s)| s.name == "Marksman").map(|(i, _)| i).collect();
    println!("nodes {:?} effects {:?}", idx, idx.iter().map(|&i| b.skills[i].effects.len()).collect::<Vec<_>>());
    let mut x = bow.clone();
    for &i in &idx { x.ranks[i] = b.skills[i].max.max(1) as u8; }
    let t = audit::audit_targets();
    let a = audit::outputs(&e, bow, &t[0]);
    let c = audit::outputs(&e, &x, &t[0]);
    println!("diff {:?}", a.iter().zip(&c).enumerate().filter(|(_, (p, q))| (*p - *q).abs() > 1e-9).collect::<Vec<_>>());
    let w = bow.weapon.clone().unwrap();
    let h1 = e.hit(bow, &w, bow.ammo.as_deref(), 1.0, &t[0]);
    let h2 = e.hit(&x, &w, x.ammo.as_deref(), 1.0, &t[0]);
    println!("h1 {:?} {:?} exp {}", h1.pool_terms, h1.per_type, h1.expected);
    println!("h2 {:?} exp {}", h2.pool_terms, h2.expected);
    let st = e.attributes(&x);
    println!("bow mod {} sources {:?}", st.get("WeaponCategoryDamageMod_Bow"), st.sources("WeaponCategoryDamageMod_Bow"));
    println!("eff {:?}", b.skills[37].effects);
    println!("bow weapon {:?} total {} {}", bow.weapon, e.total(bow, &t[0]).total, e.total(&x, &t[0]).total);
}
