//! Which spell items the planner treats as summon spells (ens_calc::is_summon_spell), and which other spells it lists as
//! ammo: `cargo run --release -p ens-calc --example summon_spells`.

fn main() {
    let b = ens_data::Bundle::load(ens_data::default_bundle_path()).expect("bundle");
    let mut summons: Vec<&str> = vec![];
    let mut other: Vec<&str> = vec![];
    for it in b.items.values().filter(|i| i.slot.as_deref() == Some("Spell")) {
        if ens_calc::is_summon_spell(&b, it) { summons.push(&it.name) } else { other.push(&it.name) }
    }
    summons.sort();
    other.sort();
    println!("summon spells ({}): {}", summons.len(), summons.join(", "));
    println!("other spells ({}): {}", other.len(), other.join(", "));
}
