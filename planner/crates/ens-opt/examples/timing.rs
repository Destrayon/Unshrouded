use ens_calc::{Build, Target};
use ens_opt::{Goal, Optimizer, Plan};
fn main() {
    let b = ens_data::Bundle::load("data/bundle.json").unwrap();
    let o = Optimizer::new(&b);
    let s = Build::sample_bow(&b);
    let plan = Plan { level: 50, flame_level: 10, weapon: s.weapon.clone(), ammo: s.ammo.clone(), gear: s.gear.clone(), foods: s.foods.clone(), summons: vec![], found_points: None, weapons: vec![], required: vec![], avoided: vec![], rarity: None, summon_choice: Default::default(),
        ranking: vec![Goal::Damage, Goal::Survival, Goal::Movement, Goal::Sustain, Goal::Utility], attacks_per_s: 1.0, target: Target::default() };
    let t = std::time::Instant::now();
    for _ in 0..20 { let _ = o.goals(&plan, &s.ranks); }
    println!("goals: {:.2} ms", t.elapsed().as_secs_f64() * 50.0);
    let t = std::time::Instant::now();
    for _ in 0..20 { let _ = o.engine.total(&s, &Target::default()); }
    println!("total: {:.2} ms", t.elapsed().as_secs_f64() * 50.0);
    let t = std::time::Instant::now();
    for _ in 0..20 { let _ = o.engine.sheet(&s); }
    println!("sheet: {:.2} ms", t.elapsed().as_secs_f64() * 50.0);
    let t = std::time::Instant::now();
    for _ in 0..20 { let _ = o.engine.attributes(&s); }
    println!("attributes: {:.2} ms", t.elapsed().as_secs_f64() * 50.0);
    println!("candidates {:?}", o.gear_candidates(&plan).iter().map(|v| v.len()).collect::<Vec<_>>());
}
