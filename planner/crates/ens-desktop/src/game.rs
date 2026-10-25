//! A character save turned into the planner's build: gear, shield and weapons. What each save field means is in
//! `docs/plans/save-gear.md`; level and tree come from `Save::ranks`.

pub use ens_calc::from_save::apply_items;
use ens_calc::from_save::{equip, FIRST_GEAR_BIT, SHIELD_BIT};
use ens_calc::{Build, Carried};
use ens_data::save::{item_id, Items};
use ens_data::Bundle;
use std::collections::HashMap;

/// Everything the character has that the planner can use: worn armour, rings and shield, and from the inventory
/// any armour, ring, shield, weapon, arrow, spell, gem or food. Each keeps its own rarity, level and unlocked perks.
pub fn owned_items(b: &Bundle, items: &Items) -> Vec<crate::store::OwnedItem> {
    let guids: HashMap<u32, &str> = b.items.keys().filter_map(|g| Some((item_id(g)?, g.as_str()))).collect();
    let usable = |g: &str| {
        b.items.get(g).is_some_and(|i| {
            matches!(i.category.as_deref(), Some("Equipment") | Some("Weapons") | Some("Ammunition") | Some("WeaponGem"))
                || i.buff.as_ref().and_then(|x| x.slot.as_deref()) == Some("Food")
        })
    };
    let worn = items.equipped.iter().filter(|(bit, _)| (FIRST_GEAR_BIT..=SHIELD_BIT).contains(bit)).map(|(_, it)| (it, true));
    worn.chain(items.inventory.iter().map(|it| (it, false)))
        .filter_map(|(it, equipped)| {
            let g = *guids.get(&it.id)?;
            usable(g).then(|| crate::store::OwnedItem { item: equip(g, it), count: it.count, equipped })
        })
        .collect()
}

/// Indices into `owned` of the items that fit `slot` in this build. Weapons of every kind fit the weapon slot
/// (the character can hold any of them); arrows fit only with a bow in hand, spells only otherwise.
pub fn owned_for_slot(b: &Bundle, build: &Build, owned: &[crate::store::OwnedItem], slot: crate::model::Slot, summons: &[String]) -> Vec<usize> {
    use crate::model::Slot;
    let bow = build.weapon.as_ref().and_then(|w| b.items.get(&w.item)).and_then(|i| i.ammo.as_deref()) == Some("Arrow");
    (0..owned.len())
        .filter(|&k| {
            let g = &owned[k].item.item;
            let Some(i) = b.items.get(g) else { return false };
            match slot {
                Slot::Weapon => i.category.as_deref() == Some("Weapons") && i.damage.is_some(),
                Slot::Ammo => {
                    let arrow = i.slot.as_deref() == Some("Arrow");
                    let spell = i.slot.as_deref() == Some("Spell") && !summons.contains(g);
                    (arrow && bow) || (spell && !bow)
                }
                Slot::Gem => i.category.as_deref() == Some("WeaponGem"),
                Slot::Armor(k) => i.slot.as_deref() == Some(ens_calc::build::GEAR_SLOTS[k]),
                Slot::Food => i.buff.as_ref().and_then(|x| x.slot.as_deref()) == Some("Food") && !build.foods.contains(g),
                Slot::Summon => summons.contains(g) && !build.summons.contains(g),
            }
        })
        .collect()
}

/// The build with owned item `o` in `slot`, as the character has it (its own rarity, level and perks).
pub fn with_owned(b: &Bundle, build: &Build, slot: crate::model::Slot, o: &crate::store::OwnedItem) -> Build {
    use crate::model::Slot;
    let mut nb = build.clone();
    match slot {
        Slot::Weapon => nb.weapon = Some(o.item.clone()),
        Slot::Gem => nb.gem = Some(o.item.clone()),
        Slot::Armor(k) => nb.gear[k] = Some(o.item.clone()),
        Slot::Ammo => {
            nb.ammo = Some(o.item.item.clone());
            crate::model::keep_owned_upgrade(b, &mut nb, std::slice::from_ref(o), &o.item.item);
        }
        Slot::Food => {
            if !nb.foods.contains(&o.item.item) && nb.foods.len() < 4 {
                nb.foods.push(o.item.item.clone());
            }
        }
        Slot::Summon => {
            if !nb.summons.contains(&o.item.item) && nb.summons.len() < 3 {
                nb.summons.push(o.item.item.clone());
                crate::model::keep_owned_upgrade(b, &mut nb, std::slice::from_ref(o), &o.item.item);
            }
        }
    }
    nb
}

/// The build wearing the best of what the character owns: each armour slot gets the owned piece that rates highest
/// (`score` = damage, then hits survived), rings never the same piece twice, a slot nothing owned fits is emptied;
/// the weapon in hand is the owned weapon and arrow / spell pair that rates highest, the other owned weapons are
/// carried. Food, gems and summons are left as they are, except a gem stays only with the same weapon.
pub fn best_owned(b: &Bundle, build: &Build, owned: &[crate::store::OwnedItem], summons: &[String], score: impl Fn(&Build) -> (f64, f64)) -> Build {
    use crate::model::Slot;
    let better = |x: (f64, f64), y: (f64, f64)| x.0 > y.0 + 1e-9 || ((x.0 - y.0).abs() <= 1e-9 && x.1 > y.1 + 1e-9);
    let mut nb = build.clone();
    let mut used: Vec<usize> = vec![];
    for k in 0..nb.gear.len() {
        let mut best: Option<(usize, (f64, f64))> = None;
        for o in owned_for_slot(b, &nb, owned, Slot::Armor(k), summons).into_iter().filter(|o| !used.contains(o)) {
            let s = score(&with_owned(b, &nb, Slot::Armor(k), &owned[o]));
            if best.is_none_or(|(_, bs)| better(s, bs)) {
                best = Some((o, s));
            }
        }
        nb.gear[k] = best.map(|(o, _)| owned[o].item.clone());
        used.extend(best.map(|(o, _)| o));
    }

    let weapons = owned_for_slot(b, &nb, owned, Slot::Weapon, summons);
    let ammo_of = |kind: &str| -> Vec<Option<String>> {
        let mut v: Vec<Option<String>> = owned
            .iter()
            .filter(|o| b.items.get(&o.item.item).is_some_and(|i| i.slot.as_deref() == Some(kind) && !summons.contains(&o.item.item)))
            .map(|o| Some(o.item.item.clone()))
            .collect();
        v.dedup();
        if v.is_empty() {
            v.push(None);
        }
        v
    };
    let mut best: Option<(usize, Option<String>, (f64, f64))> = None;
    for &w in &weapons {
        let kind = b.items.get(&owned[w].item.item).and_then(|i| i.ammo.clone());
        let choices = match kind.as_deref() {
            Some("Arrow") => ammo_of("Arrow"),
            Some("Spell") => ammo_of("Spell"),
            _ => vec![None],
        };
        for a in choices {
            let mut t = nb.clone();
            (t.weapon, t.ammo, t.gem) = (Some(owned[w].item.clone()), a.clone(), None);
            if let Some(g) = &a {
                crate::model::keep_owned_upgrade(b, &mut t, owned, g);
            }
            let s = score(&t);
            if best.as_ref().is_none_or(|(_, _, bs)| better(s, *bs)) {
                best = Some((w, a, s));
            }
        }
    }
    if let Some((w, a, _)) = best {
        let same = nb.weapon.as_ref().map(|x| &x.item) == Some(&owned[w].item.item);
        (nb.weapon, nb.ammo) = (Some(owned[w].item.clone()), a.clone());
        if let Some(g) = &a {
            crate::model::keep_owned_upgrade(b, &mut nb, owned, g);
        }
        if !same {
            nb.gem = None;
        }
        let mut carried: Vec<Carried> = vec![];
        for &o in weapons.iter().filter(|&&o| o != w) {
            let kind = b.items.get(&owned[o].item.item).and_then(|i| i.ammo.clone());
            let ammo = match kind.as_deref() {
                Some("Arrow") | Some("Spell") => ammo_of(kind.as_deref().unwrap()).into_iter().next().flatten(),
                _ => None,
            };
            let c = Carried { weapon: owned[o].item.clone(), gem: None, ammo };
            if !carried.contains(&c) && c.weapon != owned[w].item {
                carried.push(c);
            }
        }
        nb.carried = carried;
    }
    nb
}
