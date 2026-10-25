use ens_calc::{audit, Engine};
fn main() {
    let b = ens_data::Bundle::load("data/bundle.json").unwrap();
    let e = Engine::new(&b);
    let t = audit::audit_target();
    let (_, bow) = audit::archetypes(&b).into_iter().next().unwrap();
    let mut x = bow.clone();
    x.set_rank_by_name(&b, "Blessed Arrows", 3);
    let a = e.total(&bow, &t);
    let c = e.total(&x, &t);
    println!("base procs {:?}\nwith procs {:?}", a.procs.iter().map(|p| (&p.skill, p.per_s)).collect::<Vec<_>>(), c.procs.iter().map(|p| (&p.skill, p.per_s)).collect::<Vec<_>>());
    println!("weapon {:?} ammo {:?} uptime {} {}", bow.weapon, bow.ammo, a.main_uptime, c.main_uptime);
    let idx: Vec<usize> = b.skills.iter().enumerate().filter(|(_, s)| s.name == "Blessed Arrows").map(|(i, _)| i).collect();
    println!("nodes {:?} max {:?}", idx, idx.iter().map(|&i| b.skills[i].max).collect::<Vec<_>>());
}
