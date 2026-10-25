//! A character save turned into a build: gear, shield and weapons, each with the rarity, level and unlocked perks
//! the save records. What each save field means is in `docs/plans/save-gear.md`; level and tree come from
//! `Save::ranks`. Shared by the desktop app (Load from game) and `ens calc log`.

use crate::{Build, Carried, Equip};
use ens_data::save::{item_id, Items, SaveItem};
use ens_data::Bundle;
use std::collections::HashMap;

/// `keen::ItemRarity` names in enum order (Common 0 … Legendary 4), as the planner spells rarities.
pub const RARITIES: [&str; 5] = ["Common", "Uncommon", "Rare", "Epic", "Legendary"];
/// Mask bit of the first armour slot (`keen::EquipmentSlot::Armour_Head` = 5); head, chest, hands, legs, feet and
/// the two rings follow in `GEAR_SLOTS` order, bits 5–11.
pub const FIRST_GEAR_BIT: u8 = 5;
/// Mask bit of the shield in the save's fixed slots.
pub const SHIELD_BIT: u8 = 12;

/// What a load put into the build, for the page to report.
#[derive(Debug, Default, PartialEq)]
pub struct Loaded {
    pub gear: usize,
    pub weapons: usize,
    /// Records whose id is no bundle item.
    pub unknown: usize,
}

pub fn equip(guid: &str, it: &SaveItem) -> Equip {
    Equip {
        item: guid.to_string(),
        rarity: it.rarity().map(|r| RARITIES[r as usize].to_string()),
        level: it.level.map(u32::from),
        perks: it.perks.map(|(_, unlocked)| unlocked as u32),
    }
}

/// Put the save's gear, shield and weapons into `build`. Every weapon in the inventory becomes a weapon the build
/// carries; the one in hand is the build's current weapon if the character still has it, else the one `dps` rates
/// highest. Each weapon gets the equipped arrow or spell that it fires. The gem stays only while the same weapon
/// stays in hand (gems are not read from the save).
pub fn apply_items(b: &Bundle, build: &mut Build, items: &Items, dps: impl Fn(&Build) -> f64) -> Loaded {
    let guids: HashMap<u32, &str> = b.items.keys().filter_map(|g| Some((item_id(g)?, g.as_str()))).collect();
    let mut out = Loaded::default();
    let mut guid_of = |it: &SaveItem| {
        let g = guids.get(&it.id).copied();
        out.unknown += g.is_none() as usize;
        g
    };
    let slot = |bit: u8| items.equipped.iter().find(|(k, _)| *k == bit).map(|(_, it)| it);

    for k in 0..build.gear.len() {
        build.gear[k] = slot(FIRST_GEAR_BIT + k as u8).and_then(|it| guid_of(it).map(|g| equip(g, it)));
    }
    build.offhand = slot(SHIELD_BIT).and_then(|it| guid_of(it).map(|g| equip(g, it)));

    let ammo_at = |i: Option<usize>| i.and_then(|i| guids.get(&items.inventory[i].id)).map(|g| g.to_string());
    let (arrow, spell) = (ammo_at(items.arrow), ammo_at(items.spell));
    // upgradable arrows and spells (1.0 Eternal ammo) keep the rarity, level and unlocked perks the save records
    build.upgrades.clear();
    for it in [items.arrow, items.spell].into_iter().flatten().map(|i| &items.inventory[i]) {
        if let Some(g) = guids.get(&it.id).filter(|g| b.items.get(**g).is_some_and(crate::upgradable)) {
            build.upgrades.insert(g.to_string(), equip(g, it));
        }
    }
    let mut weapons: Vec<Carried> = vec![];
    for it in &items.inventory {
        let Some(g) = guid_of(it) else { continue };
        let Some(item) = b.items.get(g) else { continue };
        if item.category.as_deref() != Some("Weapons") || item.damage.is_none() {
            continue;
        }
        let ammo = match item.ammo.as_deref() {
            Some("Arrow") => arrow.clone(),
            Some("Spell") => spell.clone(),
            _ => None,
        };
        let w = Carried { weapon: equip(g, it), gem: None, ammo };
        if !weapons.contains(&w) {
            weapons.push(w);
        }
    }
    out.gear = build.gear.iter().flatten().count() + build.offhand.is_some() as usize;
    out.weapons = weapons.len();

    let held = build.weapon.as_ref().map(|w| w.item.clone());
    let keep = held.as_ref().and_then(|h| weapons.iter().position(|w| &w.weapon.item == h));
    let pick = keep.or_else(|| {
        let score = |w: &Carried| {
            let mut t = build.clone();
            (t.weapon, t.ammo, t.gem) = (Some(w.weapon.clone()), w.ammo.clone(), None);
            dps(&t)
        };
        (0..weapons.len()).max_by(|&x, &y| score(&weapons[x]).total_cmp(&score(&weapons[y])))
    });
    match pick {
        Some(k) => {
            let w = weapons.remove(k);
            if keep.is_none() {
                build.gem = None;
            }
            (build.weapon, build.ammo) = (Some(w.weapon), w.ammo);
        }
        None => (build.weapon, build.gem, build.ammo) = (None, None, None),
    }
    build.carried = weapons;
    out
}

