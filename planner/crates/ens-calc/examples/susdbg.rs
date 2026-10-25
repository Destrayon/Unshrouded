use ens_calc::attr::AttrSystem;
use ens_data::Op;
fn main() {
    let b = ens_data::Bundle::load("data/bundle.json").unwrap();
    let sys = AttrSystem::new(&b);
    let names = ["DamageSusceptibility", "DamageSusceptibility_Physical", "DamageSusceptibility_Cut", "DamageSusceptibility_Pierce", "DamageSusceptibility_Magical", "DamageSusceptibility_Magical_Base", "DamageSusceptibility_Magical_Adder", "DamageSusceptibility_Magical_Multiplier", "DamageSusceptibility_Fire", "DamageSusceptibility_Lightning", "DamageSusceptibility_Poison", "DamageSusceptibility_SkillShot"];
    let st = sys.state();
    println!("base: {:?}", names.iter().map(|n| (n, st.get(n))).collect::<Vec<_>>());
    for (attr, v) in [("DamageSusceptibility_Physical", 0.5), ("DamageSusceptibility_Magical_Base", 0.5), ("DamageSusceptibility_Magical_Adder", 0.3), ("DamageSusceptibility_Magical_Multiplier", 1.0)] {
        let mut s2 = sys.state();
        s2.apply(attr, Op::Add, v, "t");
        println!("+{v} {attr}: {:?}", names.iter().filter_map(|n| { let x = s2.get(n); if (x - st.get(n)).abs() > 1e-9 { Some((n, x)) } else { None } }).collect::<Vec<_>>());
    }

}
