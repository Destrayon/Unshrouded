//! A build: character level, Flame level, skill ranks per tree node, equipment, food.

use ens_data::Bundle;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Equip {
    /// Item GUID (bundle.items key).
    pub item: String,
    /// Rolled rarity (None = the item's own rarity).
    pub rarity: Option<String>,
    /// Item level (None = clamp of character level into the item's range).
    pub level: Option<u32>,
    /// Perks unlocked on the item (None = all its rarity allows).
    #[serde(default)]
    pub perks: Option<u32>,
}

/// A weapon the character holds but is not using: on the action bar, in the backpack, or a bow in the ranged slot
/// while something else is in hand. Its gem goes with it, and so does the arrow or spell it fires (the game has one
/// Arrow and one Spell slot; this keeps the one this weapon uses). See `docs/plans/carried-weapons.md`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Carried {
    pub weapon: Equip,
    #[serde(default)]
    pub gem: Option<Equip>,
    #[serde(default)]
    pub ammo: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Build {
    pub name: String,
    pub level: u32,
    pub flame_level: u32,
    /// Rank per skill node index (bundle.skills order).
    pub ranks: Vec<u8>,
    /// Armour and rings: Head, Chest (UpperBody), Hands (Arms), Legs (LowerBody), Feet, Ring1, Ring2.
    pub gear: Vec<Option<Equip>>,
    pub weapon: Option<Equip>,
    pub offhand: Option<Equip>,
    /// Ammunition or spell used with the weapon.
    pub ammo: Option<String>,
    pub foods: Vec<String>,
    /// Summon spells kept up (Hollow Wretch, totems); each counts up to its cap.
    #[serde(default)]
    pub summons: Vec<String>,
    /// The weapon's gem (item GUID and gem level; None = the weapon's item level).
    #[serde(default)]
    pub gem: Option<Equip>,
    /// Other weapons the character holds; they add nothing to the numbers until one is taken in hand. Left out of
    /// the build code when empty, so codes made before this field read and print the same.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub carried: Vec<Carried>,
    /// Allow gear skill ranks beyond a skill's max (unverified in game; default false).
    #[serde(default)]
    pub gear_ranks_over_cap: bool,
    /// The Rested buff (from resting by a comfortable bed or fire) is up. On by default and left out of the build
    /// code while on, so codes made before this field read the same.
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub rested: bool,
    /// Skill points from the Shroud Roots and Elixir Wells this character has found, when known (Load from game);
    /// None assumes the even spread of `ens_opt::level_points`. Left out of the build code when None.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub found_points: Option<u32>,
    /// Melee: heavy attacks back to back instead of the light combo (`Engine::melee_combo`). Left out of the build
    /// code while off, so codes made before this field read the same.
    #[serde(default, skip_serializing_if = "is_false")]
    pub heavy: bool,
    /// The order the tree's points were bought in (node indices, one entry per rank): the player's clicks, the
    /// optimizer's leveling order, or worked out once for a tree loaded without one. Empty: not known yet.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub order: Vec<usize>,
    /// Rarity, level and unlocked perks of upgradable ammo and summon spells (1.0: Eternal spells and arrows upgrade
    /// like weapons), by item GUID, as the save records them. An upgradable item missing here counts at the planner's
    /// default (every perk its rarity allows, +1 item level each). Left out of the build code while empty.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub upgrades: std::collections::BTreeMap<String, Equip>,
    /// Test-only attribute boosts (attribute, amount added) used by the coverage audit; never saved in build codes.
    #[serde(skip)]
    pub extra: Vec<(String, f64)>,
    /// Test-only damage share added to every damage type (so the audit sees per-element bonuses); never saved.
    #[serde(skip)]
    pub extra_dist: f64,
    /// Test-only attribute adds that are part of an audit archetype itself (e.g. armor points); never saved.
    #[serde(skip)]
    pub base_extra: Vec<(String, f64)>,
    /// Internal: skip triggered buffs (set while evaluating a build with a triggered buff already applied).
    #[serde(skip)]
    pub no_triggers: bool,
}

fn yes() -> bool {
    true
}

fn is_true(b: &bool) -> bool {
    *b
}

fn is_false(b: &bool) -> bool {
    !*b
}

pub const GEAR_SLOTS: [&str; 7] = ["Armour_Head", "Armour_UpperBody", "Armour_Arms", "Armour_LowerBody", "Armour_Feet", "Armour_Ring", "Armour_Ring"];

impl Build {
    pub fn new(bundle: &Bundle, level: u32) -> Self {
        Build {
            name: "New build".into(),
            level,
            flame_level: 1,
            ranks: vec![0; bundle.skills.len()],
            gear: vec![None; GEAR_SLOTS.len()],
            weapon: None,
            offhand: None,
            ammo: None,
            foods: vec![],
            summons: vec![],
            gem: None,
            carried: vec![],
            gear_ranks_over_cap: false,
            rested: true,
            found_points: None,
            heavy: false,
            order: vec![],
            upgrades: Default::default(),
            extra: vec![],
            extra_dist: 0.0,
            base_extra: vec![],
            no_triggers: false,
        }
    }

    pub fn all_equipment(&self) -> impl Iterator<Item = &Equip> {
        self.gear.iter().flatten().chain(self.weapon.iter()).chain(self.offhand.iter())
    }

    /// Put a piece of gear in its slot: a shield as the offhand, armour in its slot, a ring in the first free ring slot.
    pub fn wear(&mut self, bundle: &Bundle, e: Equip) -> Result<(), String> {
        let it = bundle.items.get(&e.item).ok_or_else(|| format!("no item {}", e.item))?;
        let slot = it.slot.as_deref().unwrap_or("");
        if slot == "Shield" {
            self.offhand = Some(e);
            return Ok(());
        }
        let free = GEAR_SLOTS.iter().enumerate().find(|(k, s)| **s == slot && self.gear[*k].is_none()).map(|(k, _)| k);
        match free {
            Some(k) => {
                self.gear[k] = Some(e);
                Ok(())
            }
            None if GEAR_SLOTS.contains(&slot) => Err(format!("{} has no free {slot} slot", it.name)),
            None => Err(format!("{} is not armour, a ring or a shield", it.name)),
        }
    }

    /// The upgrade state of an ammo or summon item: what `upgrades` records, else the planner's default. A record
    /// naming another item than its key is not used.
    pub fn upgrade_of(&self, guid: &str) -> Equip {
        self.upgrades.get(guid).filter(|e| e.item == guid).cloned().unwrap_or(Equip { item: guid.to_string(), rarity: None, level: None, perks: None })
    }

    /// Every item whose perks count: the equipment, plus the arrow or spell in the one ammo slot when it carries perks
    /// of its own (1.0 Eternal spells and arrows, which upgrade like weapons). Summon spells listed beside it are not
    /// in that slot while the attack fires, so their perks do not count.
    pub fn perk_items(&self, bundle: &Bundle) -> Vec<Equip> {
        let mut out: Vec<Equip> = self.all_equipment().cloned().collect();
        for g in self.ammo.iter() {
            if bundle.items.get(g).is_some_and(crate::upgradable) {
                out.push(self.upgrade_of(g));
            }
        }
        out
    }

    /// Take carried weapon `k` in hand. The weapon in hand (with its gem and ammo) goes into the carried list in its place;
    /// with nothing in hand the entry just leaves the list. Out-of-range `k` changes nothing.
    pub fn use_carried(&mut self, k: usize) {
        if k >= self.carried.len() {
            return;
        }
        match self.weapon.take() {
            Some(w) => {
                let held = Carried { weapon: w, gem: self.gem.take(), ammo: self.ammo.take() };
                let c = std::mem::replace(&mut self.carried[k], held);
                (self.weapon, self.gem, self.ammo) = (Some(c.weapon), c.gem, c.ammo);
            }
            None => {
                let c = self.carried.remove(k);
                (self.weapon, self.gem, self.ammo) = (Some(c.weapon), c.gem, c.ammo);
            }
        }
    }

    /// Whether `order` lists exactly the tree's ranks.
    pub fn order_matches(&self) -> bool {
        let mut n = vec![0u32; self.ranks.len()];
        for &i in &self.order {
            match n.get_mut(i) {
                Some(x) => *x += 1,
                None => return false,
            }
        }
        n.iter().zip(&self.ranks).all(|(a, &b)| *a == b as u32)
    }

    /// Set the rank of the first tree node with this name (for tests and scripted builds).
    pub fn set_rank_by_name(&mut self, bundle: &Bundle, name: &str, rank: u8) {
        if let Some(i) = bundle.skills.iter().position(|s| s.name == name) {
            self.ranks[i] = rank;
        }
    }

    /// Skill points spent (node cost × rank).
    pub fn points(&self, bundle: &Bundle) -> u32 {
        self.ranks.iter().enumerate().map(|(i, r)| *r as u32 * bundle.skills[i].cost).sum()
    }

    /// Compact shareable code: base64url of JSON.
    pub fn to_code(&self) -> String {
        let json = serde_json::to_vec(self).unwrap_or_default();
        b64(&json)
    }

    pub fn from_code(code: &str) -> Option<Self> {
        let bytes = code_json(code)?;
        serde_json::from_slice(&bytes).ok()
    }

    /// A code stamped with the game data it was made against (`data_tag`), so another data version can refuse it.
    pub fn to_code_for(&self, bundle: &Bundle) -> String {
        self.to_code_with(bundle, serde_json::Map::new())
    }

    /// A build code carrying extra top-level fields (the planner's fight setup), which `from_code_for` ignores and
    /// `code_field` reads back.
    pub fn to_code_with(&self, bundle: &Bundle, extra: serde_json::Map<String, serde_json::Value>) -> String {
        let mut v = serde_json::to_value(self).unwrap_or_default();
        if let Some(o) = v.as_object_mut() {
            o.extend(extra);
            o.insert("data".into(), data_tag(bundle).into());
            o.insert("patch".into(), patch_label(bundle).into());
            o.insert("engine".into(), ENGINE_VERSION.into());
            // the tree by skill id too ([id, rank] per bought node, and the buying order as ids), so a code survives a
            // change to the tree's node order
            let ids: Vec<serde_json::Value> = self.ranks.iter().enumerate().filter(|(_, r)| **r > 0)
                .filter_map(|(i, r)| bundle.skills.get(i).map(|s| serde_json::json!([s.id, r]))).collect();
            o.insert("rankIds".into(), ids.into());
            // the buying order as positions in rankIds (short numbers; codes before stored the skill ids as orderIds)
            let bought: Vec<usize> = self.ranks.iter().enumerate().filter(|(_, r)| **r > 0).map(|(i, _)| i).collect();
            o.insert("orderIdx".into(), self.order.iter().filter_map(|i| bought.iter().position(|b| b == i)).collect::<Vec<_>>().into());
        }
        let json = serde_json::to_vec(&v).unwrap_or_default();
        // deflated, so a full level-50 build fits one chat message (Discord: 2,000 characters)
        let mut z = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::best());
        let packed = std::io::Write::write_all(&mut z, &json).ok().and_then(|_| z.finish().ok());
        match packed {
            Some(p) => format!("{PACKED}{}", b64(&p)),
            None => b64(&json),
        }
    }

    /// One extra top-level field of a build code (see `to_code_with`); None when the code has none or is no code.
    pub fn code_field(code: &str, key: &str) -> Option<serde_json::Value> {
        let bytes = code_json(code)?;
        serde_json::from_slice::<serde_json::Value>(&bytes).ok()?.get(key).cloned()
    }

    /// Read a code against this game data, with a warning when it was made against other numbers. Refused: a code
    /// whose tree layout differs (ranks are stored by node position, so they cannot be read safely), and a code whose
    /// shape or items this data cannot hold. A code made before codes were stamped loads when its shape fits.
    pub fn from_code_for(code: &str, bundle: &Bundle) -> Result<(Self, Option<String>), String> {
        let bytes = code_json(code).ok_or("That is not a build code (or it is far longer than any build).")?;
        let v: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| "That is not a build code.")?;
        let here = data_tag(bundle);
        let layout = layout_tag(bundle);
        let mut warning = None;
        let mut by_id = false;
        // the patch a reader can recognise: the code's own label, or the game build at the front of its stamp
        let mine = patch_label(bundle);
        let theirs = v.get("patch").and_then(|x| x.as_str()).map(String::from)
            .or_else(|| v.get("data").and_then(|x| x.as_str()).and_then(|t| t.split('-').next()).map(|b| format!("game build {b}")))
            .unwrap_or_default();
        if v.get("data").is_none() {
            warning = Some(format!("That code carries no game-data stamp (made before codes were stamped, or by an older app). Its skills are read by position, so they may be wrong if the skill tree changed since; this planner has game data {here}."));
        }
        if let Some(tag) = v.get("data").and_then(|x| x.as_str()) {
            let their_layout: String = tag.splitn(3, '-').take(2).collect::<Vec<_>>().join("-");
            if their_layout.split('-').nth(1) != layout.split('-').nth(1) {
                if v.get("rankIds").and_then(|x| x.as_array()).is_none() {
                    return Err(format!("That code was made for {theirs} (data {tag}); this planner has {mine} (data {here}). Skill ranks are stored by position, so it cannot be read safely."));
                }
                by_id = true;
                warning = Some(format!("That code was made for another skill tree ({theirs}, data {tag}; this planner has {mine}, data {here}). Its skills were matched by skill, and skills the tree no longer has were dropped."));
            } else if tag != here {
                warning = Some(format!("That code was made with other game data ({theirs}, data {tag}; this planner has {mine}, data {here}). It loads, but its numbers may differ from what its author saw."));
            }
            // same data, older math: the planner's own rules changed since the code was made
            let engine = v.get("engine").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
            if warning.is_none() && engine < ENGINE_VERSION {
                warning = Some(format!("That code was made by an earlier version of the planner, whose damage rules have changed since ({}). It loads as it was, but its totals may differ from what its author saw.", ENGINE_CHANGES.iter().filter(|(v, _)| *v > engine).map(|(_, c)| *c).collect::<Vec<_>>().join("; ")));
            }
        }
        let mut v = v;
        if by_id {
            // ranks by skill id onto this tree's node order
            let at: std::collections::HashMap<u64, usize> = bundle.skills.iter().enumerate().map(|(i, s)| (s.id as u64, i)).collect();
            let mut ranks = vec![0u8; bundle.skills.len()];
            for e in v["rankIds"].as_array().into_iter().flatten() {
                if let (Some(id), Some(r)) = (e.get(0).and_then(|x| x.as_u64()), e.get(1).and_then(|x| x.as_u64())) {
                    if let Some(&i) = at.get(&id) {
                        ranks[i] = r as u8;
                    }
                }
            }
            let ids: Vec<u64> = match v.get("orderIds").and_then(|x| x.as_array()) {
                Some(a) => a.iter().filter_map(|x| x.as_u64()).collect(),
                None => v["orderIdx"].as_array().into_iter().flatten().filter_map(|x| x.as_u64())
                    .filter_map(|k| v["rankIds"].get(k as usize).and_then(|e| e.get(0)).and_then(|x| x.as_u64())).collect(),
            };
            let order: Vec<usize> = ids.into_iter().filter_map(|id| at.get(&id).copied()).collect();
            v["ranks"] = serde_json::json!(ranks);
            v["order"] = serde_json::json!(order);
        }
        let b: Build = serde_json::from_value(v).map_err(|_| "That is not a build code.")?;
        if b.name.chars().count() > MAX_NAME_CHARS {
            return Err(format!("That code's build name is {} characters long; names go up to {MAX_NAME_CHARS}.", b.name.chars().count()));
        }
        if b.ranks.len() != bundle.skills.len() {
            return Err(format!("That code has {} skill nodes; this game data has {}.", b.ranks.len(), bundle.skills.len()));
        }
        if b.gear.len() != GEAR_SLOTS.len() {
            return Err(format!("That code has {} gear slots; a character has {}.", b.gear.len(), GEAR_SLOTS.len()));
        }
        let guids = b.all_equipment().map(|e| &e.item).chain(b.gem.iter().map(|e| &e.item)).chain(b.ammo.iter()).chain(&b.foods).chain(&b.summons)
            .chain(b.carried.iter().flat_map(|c| std::iter::once(&c.weapon.item).chain(c.gem.iter().map(|e| &e.item)).chain(c.ammo.iter())));
        for g in guids {
            if !bundle.items.contains_key(g) {
                return Err(format!("That code names an item this game data does not have ({g})."));
            }
        }
        Ok((b, warning))
    }

    /// What makes this build impossible in the game, one line each; empty for a build a character can have.
    pub fn problems(&self, bundle: &Bundle) -> Vec<String> {
        let mut out = vec![];
        let name = |g: &str| bundle.items.get(g).map(|i| i.name.clone()).unwrap_or_else(|| g.to_string());
        let (lcap, icap) = (level_cap(bundle), item_level_cap(bundle));
        if !(1..=lcap).contains(&self.level) {
            out.push(format!("Character level {} is outside 1 to {lcap}.", self.level));
        }
        if !(1..=FLAME_LEVEL_CAP).contains(&self.flame_level) {
            out.push(format!("Flame level {} is outside 1 to {FLAME_LEVEL_CAP}.", self.flame_level));
        }
        for (i, s) in bundle.skills.iter().enumerate() {
            if self.ranks.get(i).is_some_and(|&r| r as u32 > s.max.max(1)) {
                out.push(format!("{} has {} ranks; it has at most {}.", s.name, self.ranks[i], s.max.max(1)));
            }
        }
        // upgrade records of arrows, spells and summon spells: checked like equipment
        for (g, e) in &self.upgrades {
            let Some(it) = bundle.items.get(g) else {
                out.push(format!("An upgrade record names {g}, which this game data does not have."));
                continue;
            };
            if &e.item != g {
                out.push(format!("The upgrade record for {} names another item ({}); it is not used.", it.name, name(&e.item)));
                continue;
            }
            if !crate::upgradable(it) {
                out.push(format!("{} does not upgrade; its upgrade record is not used for its perks.", it.name));
            }
            if let Some(r) = e.rarity.as_deref().filter(|r| !RARITIES.contains(r)) {
                out.push(format!("{} has rarity {r}, which the game does not have.", it.name));
            }
            let perks = crate::unlocked_perks(it, e.rarity.as_deref().or(it.rarity.as_deref()).unwrap_or("Common"), e.perks);
            // 0x140404650 keeps Level − unlockedPerks within [minLevel, maxLevel]
            if let (Some(l), Some(r)) = (e.level, it.levels.as_ref()) {
                if l > icap || l > r.max + perks || l < r.min + perks {
                    out.push(format!("{} is at item level {l}; it goes from {} to {} (plus {perks} unlocked perks, at most {icap}).", it.name, r.min, (r.max + perks).min(icap)));
                }
            }
        }
        for e in self.all_equipment().chain(self.gem.iter()) {
            if e.level.is_some_and(|l| l > icap) {
                out.push(format!("{} is at item level {}; items go up to {icap}.", name(&e.item), e.level.unwrap()));
            } else if let (Some(l), Some(it)) = (e.level, bundle.items.get(&e.item)) {
                // 0x140404650 keeps Level - unlockedPerks within [minLevel, maxLevel]: an item cannot pass its own max
                // by more than its unlocked perks
                if let Some(r) = it.levels.as_ref().filter(|r| r.max > 0) {
                    let perks = crate::unlocked_perks(it, e.rarity.as_deref().or(it.rarity.as_deref()).unwrap_or("Common"), e.perks);
                    if l > r.max + perks {
                        out.push(format!("{} is at item level {l}; it goes up to {} (level {} plus {perks} unlocked perks).", name(&e.item), r.max + perks, r.max));
                    }
                }
            }
            if let Some(r) = &e.rarity {
                if !RARITIES.contains(&r.as_str()) {
                    out.push(format!("{} has rarity '{r}', which the game does not have.", name(&e.item)));
                }
            }
        }
        for (k, e) in self.gear.iter().enumerate() {
            if let (Some(e), Some(want)) = (e, GEAR_SLOTS.get(k)) {
                if bundle.items.get(&e.item).and_then(|i| i.slot.as_deref()) != Some(*want) {
                    out.push(format!("{} cannot be worn in the {} slot.", name(&e.item), want.trim_start_matches("Armour_")));
                }
            }
        }
        if let Some(e) = &self.offhand {
            if bundle.items.get(&e.item).and_then(|i| i.slot.as_deref()) != Some("Shield") {
                out.push(format!("{} cannot be held in the off hand; only shields can.", name(&e.item)));
            }
        }
        if let Some(w) = &self.weapon {
            let wi = bundle.items.get(&w.item);
            if wi.and_then(|i| i.category.as_deref()) != Some("Weapons") {
                out.push(format!("{} is not a weapon.", name(&w.item)));
            }
            let kind = wi.and_then(|i| i.weapon_category.as_ref()).and_then(|c| bundle.weapon_categories.get(c)).and_then(|c| c.kind.as_deref()).unwrap_or("");
            if let Some(a) = &self.ammo {
                let ai = bundle.items.get(a);
                let arrow = ai.is_some_and(|i| i.ammo.as_deref() == Some("Arrow") || i.slot.as_deref() == Some("Arrow"));
                let spell = ai.is_some_and(|i| i.slot.as_deref() == Some("Spell"));
                let ok = match kind {
                    "RangedBow" => arrow,
                    "RangedStaff" | "RangedWand" => spell,
                    _ => false,
                };
                if !ok {
                    out.push(format!("{} cannot fire {}.", name(&w.item), name(a)));
                }
            }
        }
        let stomach = bundle.skills.iter().enumerate().any(|(i, s)| s.name == "Dessert Stomach" && self.ranks.get(i).is_some_and(|&r| r > 0));
        let food_slots = if stomach { 4 } else { 3 };
        let mut foods = self.foods.clone();
        foods.sort();
        foods.dedup();
        if foods.len() < self.foods.len() {
            out.push("The same food is eaten twice; each food buff counts once.".into());
        }
        if self.foods.len() > food_slots {
            out.push(format!("{} foods are eaten; there are {food_slots} food slots{}.", self.foods.len(), if stomach { "" } else { " (4 with Dessert Stomach)" }));
        }
        let mut summons = self.summons.clone();
        summons.sort();
        summons.dedup();
        if summons.len() < self.summons.len() {
            out.push("The same summon spell is listed twice.".into());
        }
        out
    }
}

/// Starting points for a new player: (label, weapon kind). See `preset`.
pub const PRESETS: [(&str, &str); 7] = [("Bow", "RangedBow"), ("Staff", "RangedStaff"), ("Wand", "RangedWand"), ("Greatsword", "GreatSword"),
    ("One-handed sword", "OneHandSword"), ("Two-handed hammer", "TwoHandHammer"), ("Daggers", "Dagger")];

/// A build of `kind` at character `level`: the highest-level weapon of that kind a character of that level can hold
/// (ties by name), at the character's level (an upgraded weapon) and its own rarity, with the highest-level arrow (bow) or attack spell (staff) of that level.
/// Tree, gear and food stay as in `base`.
pub fn preset(bundle: &Bundle, base: &Build, kind: &str, level: u32) -> Option<Build> {
    let kind_of = |i: &ens_data::Item| i.weapon_category.as_ref().and_then(|c| bundle.weapon_categories.get(c)).and_then(|c| c.kind.clone());
    let lv = |i: &ens_data::Item| i.levels.as_ref().map(|l| l.min).unwrap_or(0);
    let best = |f: &dyn Fn(&ens_data::Item) -> bool| -> Option<String> {
        let mut c: Vec<(&String, &ens_data::Item)> = bundle.items.iter().filter(|(_, i)| !i.name.is_empty() && lv(i) <= level && f(i)).collect();
        c.sort_by(|a, b| lv(b.1).cmp(&lv(a.1)).then(a.1.name.cmp(&b.1.name)).then(a.0.cmp(b.0)));
        c.first().map(|(g, _)| (*g).clone())
    };
    let weapon = best(&|i| i.category.as_deref() == Some("Weapons") && i.damage.is_some() && kind_of(i).as_deref() == Some(kind))?;
    let ammo = match kind {
        "RangedBow" => best(&|i| i.ammo.as_deref() == Some("Arrow") && i.category.as_deref() == Some("Ammunition") && i.damage.is_some() && !i.configs.contains_key("2938334926")),
        "RangedStaff" => best(&|i| i.slot.as_deref() == Some("Spell") && crate::deals_damage(i) && !crate::is_summon_spell(bundle, i) && !crate::is_zone_spell(i)),
        _ => None,
    };
    let it = &bundle.items[&weapon];
    let mut b = base.clone();
    b.level = level;
    // the highest level the game allows this weapon at this character level: the character level clamped into the
    // item's range, plus one per unlocked perk (0x140404650 keeps Level - unlockedPerks within [minLevel, maxLevel])
    let wl = crate::default_item_level(it, it.rarity.as_deref().unwrap_or("Common"), level, None).min(item_level_cap(bundle));
    b.weapon = Some(Equip { item: weapon.clone(), rarity: it.rarity.clone(), level: Some(wl), perks: None });
    b.ammo = ammo;
    b.gem = None;
    b.carried.clear();
    Some(b)
}

/// Rarities an item can have (bundle items' rarity values).
pub const RARITIES: [&str; 5] = ["Common", "Uncommon", "Rare", "Epic", "Legendary"];

/// Flame level cap in 1.0: 11 (1.0 notes, https://www.keengamer.com/articles/guides/enshrouded-1-0-patch-notes-all-new-features-and-changes/).
/// The bundle's balancing flameLevelMax is 20, but its per-Flame-level tables stop growing after entry 11
/// (altarsPerFlameLevel: [1, 2, 4, 6, 8, 10, 11, 12, 13, 14, 15, 16, 16, ...]).
pub const FLAME_LEVEL_CAP: u32 = 11;

/// Character level cap: bundle balancing playerLevelCap (50; playerLevelMax 100 is the size of the level table).
pub fn level_cap(bundle: &Bundle) -> u32 {
    bundle.balance("playerLevelCap").unwrap_or(50.0) as u32
}

/// Item level cap: bundle balancing itemLevelCap (55).
pub fn item_level_cap(bundle: &Bundle) -> u32 {
    bundle.balance("itemLevelCap").unwrap_or(55.0) as u32
}

/// The game data a code is made against: the game build and a hash of the skill tree's node ids in order (ranks are
/// stored by node position, so any change to that order makes old codes unreadable).
/// Then a hash of the whole bundle, so a build made against other numbers is noticed even when the tree is the same.
pub fn data_tag(bundle: &Bundle) -> String {
    let c = (bundle.content_hash ^ (bundle.content_hash >> 32)) as u32;
    format!("{}-{c:08x}", layout_tag(bundle))
}

/// The game data as a player names it: "Enshrouded 1.0, game build 1273043, game data built 2026-10-20" (the date
/// the game's own data file was built, bundle meta.built).
pub fn patch_label(bundle: &Bundle) -> String {
    let m = |k: &str| bundle.meta.get(k).map(|v| v.as_str().map(String::from).unwrap_or_else(|| v.to_string())).unwrap_or_default();
    let built = m("built");
    format!("Enshrouded {}, game build {}, game data built {}", m("version"), m("build"), built.get(..10).unwrap_or(&built))
}

/// Version of the planner's damage rules, written into build codes. Raise it, with a line in ENGINE_CHANGES, when a
/// fix changes what the same build and data total, so a code made before says its numbers may differ.
pub const ENGINE_VERSION: u32 = 2;

/// What changed in each engine version, for the warning on older codes (version 1: codes stamped before R7).
pub const ENGINE_CHANGES: &[(u32, &str)] = &[
    (1, "weapon specials, sneak and merciless pacing, the free-arrow fallback, weak spots and Eternal ammo upgrades (round 6)"),
    (2, "Eternal summon perks, base damage at item level 36 (round 7)"),
];

/// The part of `data_tag` that decides whether ranks stored by position can be read: game build and node order.
pub fn layout_tag(bundle: &Bundle) -> String {
    let mut h: u32 = 0x811c9dc5;
    for s in &bundle.skills {
        for byte in s.id.to_le_bytes() {
            h = (h ^ byte as u32).wrapping_mul(0x01000193);
        }
    }
    let build = bundle.meta.get("build").map(|v| v.to_string()).unwrap_or_default();
    format!("{build}-{h:08x}")
}

/// Ranks and buying order saved against tree `from` (skill ids in node order), moved onto this bundle's node order by
/// skill id; nodes the tree no longer has are dropped. Gear-granted ranks are not stored, so nothing else moves.
pub fn remap_ranks(b: &mut Build, from: &[u32], bundle: &Bundle) {
    let at: std::collections::HashMap<u32, usize> = bundle.skills.iter().enumerate().map(|(i, s)| (s.id, i)).collect();
    let mut ranks = vec![0u8; bundle.skills.len()];
    for (k, r) in b.ranks.iter().enumerate() {
        if let Some(&i) = from.get(k).and_then(|id| at.get(id)) {
            ranks[i] = *r;
        }
    }
    b.order = b.order.iter().filter_map(|k| from.get(*k).and_then(|id| at.get(id)).copied()).collect();
    b.ranks = ranks;
}

/// GUID of an item by display name (prefers the level-scaling version when several exist).
pub fn find_item(bundle: &Bundle, name: &str) -> Option<String> {
    let mut c: Vec<(&String, &ens_data::Item)> = bundle.items.iter().filter(|(_, i)| i.name == name).collect();
    c.sort_by_key(|(g, i)| (i.levels.as_ref().map(|l| (l.max - l.min, l.min)).unwrap_or((0, 0)), (*g).clone()));
    c.last().map(|(g, _)| (*g).clone())
}

/// Lowercase alphanumeric words of a template key or a query ("Enemy_Scavenger_Melee" -> enemy, scavenger, melee).
fn words(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_lowercase).collect()
}

/// True when every word of `query` is a word of the template `key` ("scavenger weakling" matches
/// Enemy_Scavenger_Melee_Weakling). A query with no words matches nothing.
pub fn enemy_key_matches(key: &str, query: &str) -> bool {
    let q = words(query);
    let k = words(key);
    !q.is_empty() && q.iter().all(|w| k.contains(w))
}

/// Enemy template key (bundle.enemy_types) from the key itself (case-insensitive, `Enemy_` optional) or from words
/// of it ("scavenger weakling"). None or several matches is an error naming the closest keys or every candidate.
pub fn find_enemy(bundle: &Bundle, name: &str) -> Result<String, String> {
    let q = name.trim();
    let prefixed = format!("Enemy_{q}");
    if let Some(k) = bundle.enemy_types.keys().find(|k| k.eq_ignore_ascii_case(q) || k.eq_ignore_ascii_case(&prefixed)) {
        return Ok(k.clone());
    }
    let qw = words(q);
    if qw.is_empty() {
        return Err(format!("{name:?} is not an enemy name; list them with `ens data enemies`"));
    }
    // key words the query did not name
    let leftover = |k: &str| words(k).iter().filter(|w| !qw.contains(w)).count();
    let mut hits: Vec<(&String, &ens_data::EnemyType)> = bundle.enemy_types.iter().filter(|(k, _)| enemy_key_matches(k, q)).collect();
    hits.sort_by_key(|(k, _)| (leftover(k), (*k).clone()));
    match hits.as_slice() {
        [(k, _)] => Ok((*k).clone()),
        [] => {
            // closest: most query words found inside a key word, then fewest leftover words
            let found = |k: &str| {
                let kw = words(k);
                qw.iter().filter(|w| kw.iter().any(|x| x.contains(w.as_str()))).count()
            };
            let mut near: Vec<(usize, usize, &String)> = bundle.enemy_types.keys().map(|k| (found(k), leftover(k), k)).filter(|c| c.0 > 0).collect();
            near.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(b.2)));
            if near.is_empty() {
                return Err(format!("no enemy template matches {name:?}; list them with `ens data enemies`"));
            }
            let list: Vec<String> = near.iter().take(5).map(|c| format!("  {}", c.2)).collect();
            Err(format!("no enemy template matches {name:?}; closest:\n{}", list.join("\n")))
        }
        _ => {
            const SHOWN: usize = 12;
            let list: Vec<String> = hits.iter().take(SHOWN).map(|(k, t)| format!("  {k}  ({} {})", t.faction, t.class)).collect();
            let more = if hits.len() > SHOWN {
                format!("\n  ...and {} more; narrow the query, or list them with `ens data enemies`", hits.len() - SHOWN)
            } else {
                String::new()
            };
            Err(format!("{name:?} matches {} enemy templates; use one of:\n{}{}", hits.len(), list.join("\n"), more))
        }
    }
}

impl Build {
    /// A level-50 bow build used for checks and screenshots.
    pub fn sample_bow(bundle: &Bundle) -> Build {
        let mut b = Build::new(bundle, 50);
        b.name = "Sample bow build".into();
        b.flame_level = 10;
        for (i, s) in bundle.skills.iter().enumerate() {
            if s.name == "Dexterity" {
                b.ranks[i] = 1;
            }
        }
        for (n, r) in [("Marksman", 3), ("Sniper", 3), ("Fatal Precision", 3), ("Skill Shot", 3), ("Graceful Stride", 1), ("Ranger", 3), ("Multi Shot", 1)] {
            b.set_rank_by_name(bundle, n, r);
        }
        b.weapon = find_item(bundle, "Tarred Bow").map(|g| Equip { item: g, rarity: Some("Legendary".into()), level: Some(50), perks: None });
        b.ammo = find_item(bundle, "Eternal Frost Arrow III");
        b.foods = ["Pumpkin Soup", "Chicken Soup"].iter().filter_map(|n| find_item(bundle, n)).collect();
        b
    }
}

const ALPHA: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// Start of a deflated build code (codes from before it are plain base64url of JSON and start with "eyJ").
pub const PACKED: &str = "ENS2.";

/// The JSON inside a build code: a deflated one (`PACKED`) or a plain one; None when it is no code.
pub fn code_json(code: &str) -> Option<Vec<u8>> {
    let code: String = code.split_whitespace().collect();
    // no real build comes near these: a level-50 character with every slot, carried weapons and a fight setup is
    // under 2,000 characters packed and 10 KB of JSON
    if code.len() > MAX_CODE_CHARS {
        return None;
    }
    let out = match code.strip_prefix(PACKED) {
        Some(rest) => {
            let mut out = Vec::new();
            let packed = unb64(rest)?;
            let z = flate2::read::DeflateDecoder::new(&packed[..]);
            std::io::Read::read_to_end(&mut std::io::Read::take(z, MAX_CODE_JSON as u64 + 1), &mut out).ok()?;
            out
        }
        None => unb64(&code)?,
    };
    (out.len() <= MAX_CODE_JSON).then_some(out)
}

/// Longest build code read (characters) and longest JSON one may unpack to (bytes).
pub const MAX_CODE_CHARS: usize = 64 * 1024;
pub const MAX_CODE_JSON: usize = 256 * 1024;
/// Longest build or loadout name a code may carry.
pub const MAX_NAME_CHARS: usize = 200;

fn b64(data: &[u8]) -> String {
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let n = (chunk[0] as u32) << 16 | (*chunk.get(1).unwrap_or(&0) as u32) << 8 | *chunk.get(2).unwrap_or(&0) as u32;
        for i in 0..=chunk.len() {
            out.push(ALPHA[((n >> (18 - 6 * i)) & 63) as usize] as char);
        }
    }
    out
}

pub(crate) fn unb64(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let vals: Vec<u32> = s.bytes().map(|c| ALPHA.iter().position(|a| *a == c).map(|p| p as u32)).collect::<Option<_>>()?;
    for chunk in vals.chunks(4) {
        let mut n = 0u32;
        for (i, v) in chunk.iter().enumerate() {
            n |= v << (18 - 6 * i);
        }
        for i in 0..chunk.len().saturating_sub(1) {
            out.push((n >> (16 - 8 * i)) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn code_roundtrip() {
        let data = b"hello world, build!";
        assert_eq!(unb64(&b64(data)).unwrap(), data);
    }

    fn bundle() -> Bundle {
        Bundle::load(ens_data::default_bundle_path()).unwrap()
    }

    #[test]
    fn carried_weapons_swap_and_old_codes_stay_the_same() {
        let b = bundle();
        let mut build = Build::sample_bow(&b);
        let code = build.to_code();
        assert_eq!(Build::from_code(&code).unwrap().to_code(), code, "no carried weapons: the code is unchanged");
        assert!(!String::from_utf8(unb64(&code).unwrap()).unwrap().contains("carried"));

        let bow = build.weapon.clone().unwrap();
        let sword = Equip { item: find_item(&b, "Sword").unwrap_or_else(|| b.items.keys().next().unwrap().clone()), rarity: None, level: Some(20), perks: None };
        let arrows = build.ammo.clone();
        build.gem = None;
        build.carried.push(Carried { weapon: sword.clone(), gem: None, ammo: None });
        build.use_carried(0);
        assert_eq!((build.weapon.as_ref(), build.ammo.as_ref()), (Some(&sword), None));
        assert_eq!(build.carried, vec![Carried { weapon: bow.clone(), gem: None, ammo: arrows.clone() }]);
        let back = Build::from_code(&build.to_code()).unwrap();
        assert_eq!(back, build);
        build.use_carried(5);
        assert_eq!(back, build, "out of range: no change");

        build.weapon = None;
        build.use_carried(0);
        assert_eq!((build.weapon.as_ref(), build.ammo.clone(), build.carried.len()), (Some(&bow), arrows, 0));
    }

    #[test]
    fn find_enemy_exact_key() {
        let b = bundle();
        assert_eq!(find_enemy(&b, "Enemy_Scavenger_Melee_Weakling").unwrap(), "Enemy_Scavenger_Melee_Weakling");
        assert_eq!(find_enemy(&b, "enemy_scavenger_melee_weakling").unwrap(), "Enemy_Scavenger_Melee_Weakling");
    }

    #[test]
    fn find_enemy_key_without_prefix() {
        let b = bundle();
        assert_eq!(find_enemy(&b, "Scavenger_Melee_Weakling").unwrap(), "Enemy_Scavenger_Melee_Weakling");
    }

    #[test]
    fn find_enemy_loose_unique() {
        let b = bundle();
        assert_eq!(find_enemy(&b, "skink patrol").unwrap(), "Enemy_LizardpeopleDrak00_Weakling_Melee_Skink_Patrol");
    }

    #[test]
    fn find_enemy_ambiguous_lists_candidates() {
        let b = bundle();
        let e = find_enemy(&b, "scavenger weakling").unwrap_err();
        let listed: Vec<&str> = e.lines().skip(1).filter_map(|l| l.split_whitespace().next()).collect();
        assert_eq!(listed, ["Enemy_Scavenger_Melee_Weakling", "Enemy_Scavenger_Melee_Weakling_Shielded"], "{e}");
    }

    #[test]
    fn find_enemy_unknown_suggests() {
        let b = bundle();
        let e = find_enemy(&b, "scavenger zzzdragon").unwrap_err();
        assert!(e.contains("closest:") && e.contains("Enemy_Scavenger"), "{e}");
    }
}
