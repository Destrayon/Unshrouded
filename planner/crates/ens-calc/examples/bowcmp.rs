use ens_calc::{build::find_item, Build, Engine, Equip, Target};
fn main() {
    let b = ens_data::Bundle::load("data/bundle.json").unwrap();
    let e = Engine::new(&b);
    for name in ["Tarred Bow", "Fell Sentinel Bow"] {
        let mut bd = Build::sample_bow(&b);
        let g = find_item(&b, name).unwrap();
        bd.weapon = Some(Equip { item: g, rarity: Some("Legendary".into()), level: Some(50), perks: None });
        let t = Target { pack_size: 1.0, pack_radius: 4.5, ..Default::default() };
        let tot = e.total(&bd, &t);
        let st = e.attributes(&bd);
        println!("{name}: total {:.1} main {:.1} uptime {:.2} fallback {:?} crit {:.3} critmod {:.3}", tot.total, tot.main.total, tot.main_uptime, tot.fallback, tot.main.hit.crit_chance, tot.main.hit.crit_multiplier);
        for a in ["CritChance_Adder", "CritDamageMod_Adder"] {
            for c in st.sources(a) { println!("   {a} {:?} {:.3} <- {}", c.op, c.value, c.source); }
        }
        let (secs, how) = e.attack_interval(&bd, bd.weapon.as_ref().unwrap(), bd.ammo.as_deref());
        println!("   cycle {secs:.2} {how}");
    }
}
