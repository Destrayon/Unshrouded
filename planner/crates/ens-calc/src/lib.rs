//! Enshrouded 1.0 calculation engine.
//!
//! Formulas follow the game (see `docs/research/engine-formulas.md`):
//! * weapon base damage `trunc(10 × (1 + 0.15 × level))`, item damage `trunc(base × speedMod × dmgMod)`;
//! * one additive damage pool per damage type: `1 + 0.05 × scaling attribute + Σ(mod − 1)` over the weapon-category
//!   mod, `DamageMod`, the attack category mod (Ranged/Magic/Melee) and its per-element mod, and situational mods;
//! * crit chance `CritChance + CritChance_Adder_<category>`, crit multiplier `CritDamageMod`;
//! * flat armor with blowthrough.
//! Attribute values come from the game's attribute formulas (`attr`), start values from the player template.

pub mod attr;
pub mod audit;
pub mod build;
pub mod combat_log;
pub mod from_save;
pub mod item_stats;
pub mod measure;
pub mod meter;
pub mod proof;
pub mod validate;

pub use attr::{AttrState, AttrSystem, Contribution};
pub use build::{Build, Carried, Equip};

use ens_data::{Bundle, Effect, Item, Op, ValueSpec};
use std::collections::HashMap;

/// Active perk slots by rarity, top to bottom (confirmed in game).
pub fn active_perks(rarity: &str) -> usize {
    match rarity {
        "Common" => 1,
        "Uncommon" => 2,
        "Rare" => 3,
        "Epic" => 4,
        _ => 5,
    }
}

/// Perks you have unlocked on an item: your setting, at most the ones available (a new item gets availablePerks =
/// min(rarity + 1, 5) and unlockedPerks = 0, 0x14040ab40; runes unlock them one by one; unset = all available).
pub fn unlocked_perks(item: &Item, rarity: &str, chosen: Option<u32>) -> u32 {
    let n = active_perks(rarity).min(item.perks.iter().filter(|p| !p.is_empty()).count()) as u32;
    chosen.map_or(n, |c| c.min(n))
}

/// Perk slots that count on an equipped item: the first unlockedPerks of its list.
pub fn perk_count(e: &Equip, rarity: &str) -> usize {
    let n = active_perks(rarity);
    e.perks.map_or(n, |c| (c as usize).min(n))
}

/// An ammo or summon item that upgrades like a weapon (1.0 Eternal spells and arrows): ammunition with perks of its
/// own. Regular arrows and spells carry five all-zero perk GUIDs.
pub fn upgradable(item: &Item) -> bool {
    item.category.as_deref() == Some("Ammunition") && item.perks.iter().any(|p| p.chars().any(|c| c != '0'))
}

/// Default item level: the character level clamped into the item's range, plus one per unlocked perk (each unlock
/// raises the level; 0x140404650 keeps Level − unlockedPerks inside [minLevel, maxLevel]).
pub fn default_item_level(item: &Item, rarity: &str, char_level: u32, chosen: Option<u32>) -> u32 {
    item.levels.as_ref().map(|l| l.max.min(char_level.max(l.min))).unwrap_or(char_level) + unlocked_perks(item, rarity, chosen)
}

/// Mana per use of an item (spell, summon, arrow): its config 2938334926 × your ManaCostMod, truncated (the item
/// config reader 0x1404131f0 multiplies only this config id, 0xAF236ECE, by attribute ManaCostMod 0x8d19048a).
pub fn mana_cost(st: &AttrState, item: &Item) -> f64 {
    let base = item.configs.get("2938334926").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let m = st.get("ManaCostMod");
    (base * if m > 0.0 { m } else { 1.0 }).trunc()
}

/// A gem config at a gem level: scaled values by 0x140e97300 (WeaponPowerCreep = value + 0.15 × factor × level),
/// integer configs rounded half away from zero (0x1404131f0); plain values as stored (times in seconds).
pub fn gem_value(bundle: &Bundle, gem: &ens_data::Gem, id: &str, level: f64) -> Option<f64> {
    if let Some(c) = gem.scaled.get(id) {
        let v = c.value.as_f64().unwrap_or(0.0);
        let x = match c.function.as_str() {
            "None" => v,
            "Linear" => v + c.factor * level,
            "Percentage" => c.factor * level,
            _ => v + bundle.balance("powerCreepWeapon").unwrap_or(0.15) * c.factor * level,
        };
        return Some(if c.int { x.signum() * (x.abs() + 0.5).floor() } else { x });
    }
    gem.simple.get(id).and_then(|v| v.as_f64())
}

/// Weapon base damage at an item level (GetWeaponBaseDamage -> 0x140e723f0), in the exe's own 32-bit float order:
/// `cvtsi2ss xmm2, rcx; mulss xmm2, [rax + 0x28]; cvtsi2ss xmm0, level; mulss xmm2, xmm0; addss xmm2, xmm1;
/// cvttss2si` = trunc(seed × creep × level + seed) (analysis/fn_baseDamageByLevel.asm). The 64-bit seed × (1 + creep
/// × level) truncates one lower at level 36.
pub fn weapon_base_damage(bundle: &Bundle, level: u32) -> f64 {
    let seed = bundle.balance("genericWeaponSeed").unwrap_or(10.0) as f32;
    let creep = bundle.balance("powerCreepWeapon").unwrap_or(0.15) as f32;
    (seed * creep * level as f32 + seed).trunc() as f64
}

/// Item damage as shown in game (0x140e5dea0): base × speedMod × dmgMod, truncated.
pub fn item_damage(bundle: &Bundle, item: &Item, level: u32) -> f64 {
    let d = match &item.damage {
        Some(d) => d,
        None => return 0.0,
    };
    (weapon_base_damage(bundle, level) * d.speed_mod * d.dmg_mod).trunc()
}

/// FactionType -> its "damage against" attribute, from ApplyDamage's jump table at 0x1403d7658 (enum values 2–15;
/// Wildlife and WildBeast share one mod; Player, ally, Cyclops and farm animals have none).
pub fn faction_against_attribute(faction: &str) -> Option<&'static str> {
    match faction {
        "Fogger" => Some("DamageModAgainst_Faction_Fogger"),
        "Vukah" => Some("DamageModAgainst_Faction_Vukah"),
        "Scavenger" => Some("DamageModAgainst_Faction_Scavenger"),
        "Wildlife" | "WildBeast" => Some("DamageModAgainst_Faction_Wildlife_Wildbeast"),
        "Skeleton" => Some("DamageModAgainst_Faction_Skeleton"),
        "Drak" => Some("DamageModAgainst_Faction_Drak"),
        "Ancients" => Some("DamageModAgainst_Faction_Ancients"),
        _ => None,
    }
}

pub const DAMAGE_TYPES: [(&str, &str, &str); 8] = [
    ("physicalCutDamage", "Physical_Cut", "Cut"),
    ("physicalPierceDamage", "Physical_Pierce", "Pierce"),
    ("physicalBluntDamage", "Physical_Blunt", "Blunt"),
    ("magicalFireDamage", "Magical_Fire", "Fire"),
    ("magicalIceDamage", "Magical_Ice", "Ice"),
    ("magicalFogDamage", "Magical_Fog", "Fog"),
    ("magicalLightningDamage", "Magical_Lightning", "Lightning"),
    // poison: no <Category>DamageMod_ attribute exists for it (empty suffix = no element mod), its susceptibility is its
    // own alone (susceptibility_factor) and armor lets it through; the Acid Cleaver's 0.5 poison share was dropped
    // before (combat-1792718413.csv: 21.0 poison logged beside 21.6 cut)
    ("poisonDamage", "", "Poison"),
];

/// Attack category of a weapon type (ApplyDamage bit 1 melee, 2 ranged, 4 magic).
pub fn attack_category(weapon_type: &str) -> &'static str {
    match weapon_type {
        "RangedBow" => "Ranged",
        "RangedStaff" | "RangedWand" => "Magic",
        _ => "Melee",
    }
}

/// Attribute that scales a weapon type's damage (DamageScaleAttribute in the weapon programs).
pub fn scaling_attribute(weapon_type: &str) -> &'static str {
    match weapon_type {
        "RangedBow" | "Dagger" => "Dexterity",
        "RangedStaff" | "RangedWand" => "Intelligence",
        _ => "Strength",
    }
}

/// Character sheet: derived stats with sources.
#[derive(Debug, Clone, Default)]
pub struct Sheet {
    pub values: HashMap<String, f64>,
    pub skill_ranks: HashMap<String, u32>,
    pub warnings: Vec<String>,
}

/// The Explosive Arrow's blast on one target and, with Echoing Explosives, the echo explosion it may trigger there
/// ([`Engine::explosive_hits`]).
#[derive(Debug, Clone)]
pub struct ExplosiveHits {
    pub blast: HitBreakdown,
    pub blast_attack_mod: f64,
    pub echo: Option<HitBreakdown>,
    pub echo_chance: f64,
}

/// One hit's breakdown against a target.
#[derive(Debug, Clone, Default)]
pub struct HitBreakdown {
    pub base_damage: f64,
    pub item_damage: f64,
    /// the ammo or spell's own item damage, added to the weapon's
    pub ammo_damage: f64,
    pub attack_mod: f64,
    pub per_type: Vec<(String, f64)>,
    pub pool_terms: Vec<(String, f64)>,
    pub crit_chance: f64,
    /// One crit hit ÷ one normal hit, before armor. A crit adds CritDamageMod − 1 to each damage type's bonus pool
    /// (ApplyDamage), so this is below CritDamageMod whenever the pool is above 1: the test character's greatsword hits
    /// 61.8 and crits 91.8 (combat-1792718413.csv), × 1.485, not × 1.5.
    pub crit_multiplier: f64,
    /// CritDamageMod (+ CritDamageMod_Vs_Soaked on a soaked target): what a crit adds to the pool, + 1.
    pub crit_damage_mod: f64,
    pub non_crit: f64,
    pub expected: f64,
    pub after_armor: f64,
}

pub struct Engine<'a> {
    pub bundle: &'a Bundle,
    pub attrs: AttrSystem,
    skill_by_id: HashMap<u32, usize>,
    /// Fallback arrow per (weapon, ammo, level, target types): picking it scans every arrow, which would dominate
    /// optimizer runs; skills change its DPS but rarely which arrow is best.
    fallback_cache: std::sync::Mutex<HashMap<String, Option<String>>>,
}

impl<'a> Engine<'a> {
    pub fn new(bundle: &'a Bundle) -> Self {
        let skill_by_id = bundle.skills.iter().enumerate().map(|(i, s)| (s.id, i)).collect();
        Engine { bundle, attrs: AttrSystem::new(bundle), skill_by_id, fallback_cache: Default::default() }
    }

    /// Effective skill ranks: tree ranks plus gear `AddSkillLevel` perks (capped at max unless the build allows more).
    pub fn effective_ranks(&self, b: &Build) -> Vec<u32> {
        let tree: Vec<u32> = (0..self.bundle.skills.len()).map(|i| *b.ranks.get(i).unwrap_or(&0) as u32).collect();
        // gear perks (AddSkillLevel 0x1403d3320 → 0x140e867c0) add +1 per level to the low 5 bits of the skill's byte
        // (tree rank in the top 3 bits); ranks are per node and gear adds to the skill (first node with that id)
        let mut bonus = vec![0u32; tree.len()];
        for e in &b.perk_items(self.bundle) {
            let Some(item) = self.bundle.items.get(&e.item) else { continue };
            let n = perk_count(e, e.rarity.as_deref().or(item.rarity.as_deref()).unwrap_or("Legendary"));
            for pid in item.perks.iter().take(n) {
                let Some(p) = self.bundle.perks.get(pid) else { continue };
                if let Some(sl) = &p.skill_level {
                    let id = sl.get("skillId").and_then(|x| x.get("value")).and_then(|x| x.as_u64()).unwrap_or(0) as u32;
                    let lvl = sl.get("level").and_then(|x| x.as_u64()).unwrap_or(1) as u32;
                    if let Some(&i) = self.skill_by_id.get(&id) {
                        bonus[i] += lvl;
                    }
                }
            }
        }
        // the skill's program Level (0x140e7c0c0, used by 0x1402d1a00 to spawn and update the skill impact):
        // 0 while the tree rank is 0, else tree rank + gear bonus, NOT capped at the skill's max. A single-rank node
        // (max 0) counts as unlocked when it is in the list at all (0x140e6c910), so gear alone unlocks it.
        tree.iter().zip(bonus.iter()).enumerate()
            .map(|(i, (&r, &g))| if self.bundle.skills[i].max > 0 { if r == 0 { 0 } else { r + g } } else { u32::from(r > 0 || g > 0) })
            .collect()
    }

    /// Whether the skill with this bundle id is unlocked (tree or gear), as IsSkillUnlocked sees it.
    pub fn skill_unlocked(&self, b: &Build, id: u32) -> bool {
        self.skill_by_id.get(&id).is_some_and(|&i| self.effective_ranks(b).get(i).is_some_and(|&r| r > 0))
    }

    /// The Explosive Arrow's hits on one target (bundle `explosiveArrow`, from the blast's impact program): the blast
    /// is the arrow's hit at AttackMod shrapnel_base + shrapnel_per_target × `collisions` with Shrapnel Blast unlocked
    /// (everything the blast touches counts, buildings too), else 1; with Echoing Explosives, `echo_chance` of a
    /// second explosion at AttackMod echo_attack_mod. None when the ammo has no Explosive Arrow blast.
    pub fn explosive_hits(&self, b: &Build, weapon: &Equip, ammo: Option<&str>, target: &Target, collisions: u32) -> Option<ExplosiveHits> {
        let (blast_attack_mod, echo) = self.explosive_mods(b, ammo, collisions as f64)?;
        let x = self.bundle.explosive_arrow.as_ref()?;
        let echo = echo.map(|(_, am, _)| self.hit(b, weapon, ammo, am, target));
        Some(ExplosiveHits { blast: self.hit(b, weapon, ammo, blast_attack_mod, target), blast_attack_mod, echo, echo_chance: x.echo_chance })
    }

    /// The blast's AttackMod for `collisions` and, with Echoing Explosives, (chance, AttackMod, radius m) of the echo;
    /// None when the ammo's projectile does not spawn the Explosive Arrow blast.
    pub fn explosive_mods(&self, b: &Build, ammo: Option<&str>, collisions: f64) -> Option<(f64, Option<(f64, f64, f64)>)> {
        let x = self.bundle.explosive_arrow.as_ref()?;
        let p = ammo.and_then(|a| self.bundle.items.get(a)).and_then(|i| i.projectile.as_ref())?;
        let blasts = p.get("spawns").and_then(|s| s.as_object()).is_some_and(|s| {
            s.values().any(|c| c.get("name").and_then(|n| n.as_str()).is_some_and(|n| n.starts_with("Explosion_Player_Arrow_Explosive")))
        });
        if !blasts {
            return None;
        }
        let blast = if self.skill_unlocked(b, x.shrapnel_skill) { x.shrapnel_base + x.shrapnel_per_target * collisions } else { 1.0 };
        let echo = self.skill_unlocked(b, x.echo_skill).then_some((x.echo_chance, x.echo_attack_mod, x.echo_radius));
        Some((blast, echo))
    }

    /// Whether an effect's branch conditions hold for these ranks. Unknown condition kinds (situational checks such as
    /// IsInState or HasBuff) count as not met, so only always-on effects reach the character sheet.
    pub fn effect_active(&self, e: &Effect, ranks: &[u32]) -> bool {
        if e.when.is_empty() {
            return true;
        }
        e.when.iter().any(|alt| {
            alt.iter().all(|c| match c.node.as_str() {
                "IsSkillUnlocked" => {
                    let unlocked = c.skill.and_then(|id| self.skill_by_id.get(&id)).map(|&i| ranks[i] > 0).unwrap_or(false);
                    unlocked == c.is
                }
                _ => false,
            })
        })
    }

    /// Apply every effect of the build to a fresh attribute state.
    pub fn attributes(&self, b: &Build) -> AttrState<'_> {
        let ranks = self.effective_ranks(b);
        // two passes so effects reading other attributes (Life Essences x Intelligence) see the first-pass totals
        let mut first = self.attrs.state();
        self.apply_all(b, &ranks, &mut first, None);
        let snapshot: HashMap<String, f64> =
            ["Intelligence", "Dexterity", "Strength", "Constitution", "Endurance", "Spirit"].iter().map(|n| (n.to_string(), first.get(n))).collect();
        let mut st = self.attrs.state();
        self.apply_all(b, &ranks, &mut st, Some(&snapshot));
        st
    }

    fn apply_all(&self, b: &Build, ranks: &[u32], st: &mut AttrState, snap: Option<&HashMap<String, f64>>) {
        let creep = self.bundle.balance("powerCreepWeapon").unwrap_or(0.15);
        let look = |level: f64| {
            move |a: &str| -> f64 {
                if a == "Level" {
                    level
                } else if a == "$powerCreepWeapon" {
                    creep
                } else {
                    snap.and_then(|s| s.get(a)).copied().unwrap_or(0.0)
                }
            }
        };
        // skills: one application per node with its rank (Level = rank)
        for (i, s) in self.bundle.skills.iter().enumerate() {
            let r = ranks[i];
            if r == 0 {
                continue;
            }
            for e in s.effects.iter().filter(|e| self.effect_active(e, ranks)) {
                let (Some(attr), Some(v)) = (&e.attribute, &e.value) else { continue };
                let val = v.eval(&look(r as f64));
                st.apply(attr, e.operation, val, &format!("skill {} ({r})", s.name));
            }
        }
        // flame attribute skills: +1 per two Flame levels
        let half = (b.flame_level / 2) as f64;
        for (skill, attr) in [
            ("Graceful Stride", "Dexterity_Flame_Base"),
            ("Barbarian", "Strength_Flame_Base"),
            ("Exalted", "Intelligence_Flame_Base"),
            ("Thick Skin", "Constitution_Flame_Base"),
            ("Endurance of the Flame", "Endurance_Flame_Base"),
            ("Arcane Concentration", "Spirit_Flame_Base"),
        ] {
            if self.bundle.skills.iter().enumerate().any(|(i, s)| s.name == skill && ranks[i] > 0) {
                st.apply(attr, Op::Add, half, &format!("skill {skill} (Flame {})", b.flame_level));
            }
            // and every Flame level above 1 gives apPerFlameLevel points to all six attributes (0x1402ea69b:
            // X_Flame_Base = (skill ? flame >> 1 : 0) + FUN_140e723d0 = (flame − 1) × apPerFlameLevel)
            let ap = self.bundle.balance("apPerFlameLevel").unwrap_or(1.0);
            if b.flame_level > 1 {
                st.apply(attr, Op::Add, (b.flame_level - 1) as f64 * ap, &format!("Flame level {}", b.flame_level));
            }
        }
        // gear perks (active slots by rarity) and applied buffs (food); upgradable ammo and summon spells carry perks too
        for e in &b.perk_items(self.bundle) {
            let Some(item) = self.bundle.items.get(&e.item) else { continue };
            let rarity = e.rarity.clone().or(item.rarity.clone()).unwrap_or_else(|| "Legendary".into());
            for pid in item.perks.iter().take(perk_count(e, &rarity)) {
                let Some(p) = self.bundle.perks.get(pid) else { continue };
                for eff in p.effects.iter().filter(|e| self.effect_active(e, ranks)) {
                    let (Some(attr), Some(v)) = (&eff.attribute, &eff.value) else { continue };
                    st.apply(attr, eff.operation, v.eval(&look(1.0)), &format!("{} ({})", item.name, p.name));
                }
            }
        }
        // Rested buff (program 7c5e7191b646ae4d905809780481a9bb): FetchAttribute `Level` of the root entity, then
        // `Div::result` = Level / `2::value` (config 3160803022 = 2, integer), `add::result` = `5::value` (3840843052
        // = 5) + that, `Mul::result` = `Stamina_Max_Increase::value` (3714676942 = 8) × that → Stamina_Max_Adder;
        // `Mul1::result` = `Stamina_Recharge_Increase::value` (1565696159 = 4) × Level → Stamina_Recharge
        if b.rested {
            let level = b.level as f64;
            st.apply("Stamina_Max_Adder", Op::Add, 8.0 * (5.0 + (level / 2.0).floor()), "Rested");
            st.apply("Stamina_Recharge", Op::Add, 4.0 * level, "Rested");
        }
        // item stats: Base_Impact_Equipment (program a620334b) adds each equipped item's own stat configs, unscaled
        // (item_stats.rs); carried weapons are not equipped and add nothing
        for e in b.all_equipment() {
            let Some(item) = self.bundle.items.get(&e.item) else { continue };
            for &(cid, _, attr, op) in item_stats::ITEM_STATS {
                let Some(v) = item.configs.get(&cid.to_string()).and_then(|v| v.as_f64()) else { continue };
                if v != 0.0 {
                    st.apply(attr, op, v, &format!("{} (item stat)", item.name));
                }
            }
        }
        // gear armor: (armorSetup.offset + armorSetup.gradient x item level) x the item's share per armor type
        // (balancing getter 0x140e5de70 = offset + gradient x level; applying it per share is inferred, call site not found)
        let (off, grad) = self
            .bundle
            .balancing
            .get("armorSetup")
            .map(|a| (a.get("offset").and_then(|x| x.as_f64()).unwrap_or(23.0), a.get("gradient").and_then(|x| x.as_f64()).unwrap_or(15.0)))
            .unwrap_or((23.0, 15.0));
        for e in b.all_equipment() {
            let Some(item) = self.bundle.items.get(&e.item) else { continue };
            let level = e.level.unwrap_or_else(|| default_item_level(item, e.rarity.as_deref().or(item.rarity.as_deref()).unwrap_or("Legendary"), b.level, e.perks)) as f64;
            if let Some(dist) = item.armor.as_ref().and_then(|a| a.get("distribution")) {
                let base = off + grad * level;
                for (key, attr) in [("physical", "ArmorPoints_Physical_Adder"), ("magical", "ArmorPoints_Magical_Adder"), ("cut", "ArmorPoints_Cut_Adder"),
                                    ("pierce", "ArmorPoints_Pierce_Adder"), ("blunt", "ArmorPoints_Blunt_Adder"), ("fire", "ArmorPoints_Fire_Adder"),
                                    ("ice", "ArmorPoints_Ice_Adder"), ("fog", "ArmorPoints_Fog_Adder"), ("lightning", "ArmorPoints_Lightning_Adder")] {
                    let share = dist.get(key).and_then(|x| x.as_f64()).unwrap_or(0.0);
                    if share > 0.0 {
                        st.apply(attr, Op::Add, (base * share).round(), &format!("armor {} (L{level})", item.name));
                    }
                }
            }
            // armor perks (Perk_Armor_*: perkArmorSetup shares): the k-th of the n active perks counts at level
            // item level − n + 1 + k (0x14037d785 → 0x140e54500, same offset + gradient × level, rounded)
            let rarity = e.rarity.clone().or(item.rarity.clone()).unwrap_or_else(|| "Legendary".into());
            let n = perk_count(e, &rarity);
            for (k, pid) in item.perks.iter().take(n).enumerate() {
                let Some(dist) = self.bundle.perks.get(pid).and_then(|p| p.armor.as_ref()) else { continue };
                let pl = (level - n as f64 + 1.0 + k as f64).max(1.0);
                let pbase = off + grad * pl;
                for (key, attr) in [("physical", "ArmorPoints_Physical_Adder"), ("magical", "ArmorPoints_Magical_Adder"), ("cut", "ArmorPoints_Cut_Adder"),
                                    ("pierce", "ArmorPoints_Pierce_Adder"), ("blunt", "ArmorPoints_Blunt_Adder"), ("fire", "ArmorPoints_Fire_Adder"),
                                    ("ice", "ArmorPoints_Ice_Adder"), ("fog", "ArmorPoints_Fog_Adder"), ("lightning", "ArmorPoints_Lightning_Adder")] {
                    let share = dist.get(key).copied().unwrap_or(0.0);
                    if share > 0.0 {
                        st.apply(attr, Op::Add, (pbase * share).round(), &format!("armor perk {} (L{pl})", item.name));
                    }
                }
            }
        }
        for (attr, v) in b.base_extra.iter().chain(b.extra.iter()) {
            st.apply(attr, Op::Add, *v, "audit boost");
        }
        for f in &b.foods {
            let Some(item) = self.bundle.items.get(f) else { continue };
            if let Some(buff) = &item.buff {
                for eff in buff.effects.iter().filter(|e| self.effect_active(e, ranks)) {
                    let (Some(attr), Some(v)) = (&eff.attribute, &eff.value) else { continue };
                    let val = v.eval(&look(1.0));
                    if val != 0.0 || !matches!(v, ValueSpec::Const { .. }) {
                        st.apply(attr, eff.operation, val, &format!("food {}", item.name));
                    }
                }
            }
        }
    }

    /// Regeneration per second of Mana/Stamina/Health (0x140237700 -> 0x1402328c0): after the RechargeTimeout the
    /// rate ramps up over RechargeRampup (smootherstep) to `X_Recharge × X_RechargeMod × rechargeModifier (0.2)` per second.
    pub fn regen_per_s(&self, st: &AttrState, pool: &str) -> f64 {
        let modv = st.get(&format!("{pool}RechargeMod"));
        let modv = if modv > 0.0 { modv } else { 1.0 };
        st.get(&format!("{pool}_Recharge")) * modv * self.bundle.balance("rechargeModifier").unwrap_or(0.2)
    }

    /// Main derived stats for the character sheet.
    pub fn sheet(&self, b: &Build) -> Sheet {
        let st = self.attributes(b);
        let bal = |k: &str, d: f64| self.bundle.balance(k).unwrap_or(d);
        let mut v = HashMap::new();
        for a in ["Strength", "Dexterity", "Intelligence", "Constitution", "Endurance", "Spirit"] {
            v.insert(a.to_string(), st.get(a));
        }
        let con = st.get("Constitution");
        let spi = st.get("Spirit");
        let end = st.get("Endurance");
        v.insert("Health".into(), st.get("Health_Max_Base") + bal("playerHealthPerAP", 50.0) * con + st.get("Health_Max_Adder"));
        v.insert("Mana".into(), st.get("Mana_Max_Base") + bal("playerManaPerAP", 20.0) * spi + st.get("Mana_Max_Adder"));
        v.insert("Stamina".into(), st.get("Stamina_Max_Base") + bal("playerStaminaPerAP", 10.0) * end + st.get("Stamina_Max_Adder"));
        for a in ["Mana_Recharge", "Stamina_Recharge", "Health_Recharge", "CritChance", "CritDamageMod", "MaxTotemCount", "MaxSkeletonSpellCount", "Movement_Sprint", "ArmorPoints_Physical", "ArmorPoints_Magical"] {
            v.insert(a.to_string(), st.get(a));
        }
        v.insert("Mana regen /s".into(), self.regen_per_s(&st, "Mana"));
        v.insert("Stamina regen /s".into(), self.regen_per_s(&st, "Stamina"));
        let ranks = self.effective_ranks(b);
        let mut sr = HashMap::new();
        for (i, s) in self.bundle.skills.iter().enumerate() {
            if ranks[i] > 0 && s.kind != "Attribute" {
                *sr.entry(s.name.clone()).or_insert(0) += ranks[i];
            }
        }
        Sheet { values: v, skill_ranks: sr, warnings: vec![] }
    }

    /// Expected damage of one hit with a weapon (and ammo/spell) against a target with given armor and susceptibility.
    pub fn hit(&self, b: &Build, weapon: &Equip, ammo: Option<&str>, attack_mod: f64, target: &Target) -> HitBreakdown {
        self.hit_setup(b, weapon, ammo, attack_mod, target, None, None)
    }

    /// The hit of a template a weapon special spawns, as its impact program prices it (bundle `specialTemplates`):
    /// from the weapon base damage in the program's types or from the inherited item damage, at the template's
    /// AttackMod × (offset + variation × r), with the template's attack category. Unknown templates fall back to the
    /// item hit at `fallback_attack_mod`.
    pub fn template_hit(&self, b: &Build, weapon: &Equip, ammo: Option<&str>, target: &Target, name: &str, fallback_attack_mod: f64, r: f64) -> HitBreakdown {
        let Some(t) = self.bundle.special_templates.get(name) else { return self.hit(b, weapon, ammo, fallback_attack_mod, target) };
        let attack_mod = t.attack_mod.unwrap_or(fallback_attack_mod) * (t.offset.unwrap_or(1.0) + t.attack_variation.unwrap_or(0.0) * r);
        let cat = t.attack_category.as_ref().and_then(|cs| cs.iter().find(|c| matches!(c.as_str(), "Melee" | "Ranged" | "Magic")).cloned());
        let setup = t.base_damage.as_ref().map(|types| DAMAGE_TYPES.iter().map(|d| if types.iter().any(|x| x == d.2) { 1.0 } else { 0.0 }).collect());
        // a template whose attack category has AoE (the wand's implosion: [Magic, AoE]) never backstabs
        let aoe = t.attack_category.as_ref().is_some_and(|cs| cs.iter().any(|c| c == "AoE"));
        if aoe && target.hit_kind.as_deref() == Some("Backstab") {
            return self.hit_setup(b, weapon, ammo, attack_mod, &Target { hit_kind: None, ..target.clone() }, setup, cat);
        }
        self.hit_setup(b, weapon, ammo, attack_mod, target, setup, cat)
    }

    /// The bow's Strong Shot (bundle `strongShot`, from its impact program): raw damage is the generic weapon base at
    /// the bow's item level in the program's damage types (Pierce), AttackMod = attack_mod × (offset + variation × r)
    /// for r in [0, 1); everything after (attribute scale, damage mods, crit, susceptibility, armor) is ApplyDamage's as
    /// for any hit. None without the bundle section.
    pub fn strong_shot(&self, b: &Build, weapon: &Equip, ammo: Option<&str>, target: &Target, r: f64) -> Option<HitBreakdown> {
        let s = self.bundle.strong_shot.as_ref()?;
        let dist: Vec<f64> = DAMAGE_TYPES.iter().map(|t| if s.damage_types.iter().any(|d| d == t.2) { 1.0 } else { 0.0 }).collect();
        let attack_mod = s.attack_mod * (s.offset + s.attack_variation * r);
        Some(self.hit_setup(b, weapon, ammo, attack_mod, target, Some(dist), None))
    }

    /// [`Engine::hit`]; `base_setup` replaces the item damage with the weapon base damage in those shares
    /// (an impact program's CreateDamageSetup(GetWeaponBaseDamage)); `category` replaces the attack category the
    /// weapon and ammo configs give (a spawned template's own).
    fn hit_setup(&self, b: &Build, weapon: &Equip, ammo: Option<&str>, attack_mod: f64, target: &Target, base_setup: Option<Vec<f64>>,
                 category: Option<String>) -> HitBreakdown {
        let st = self.attributes(b);
        let mut out = HitBreakdown { attack_mod, ..Default::default() };
        let Some(w) = self.bundle.items.get(&weapon.item) else { return out };
        let level = weapon.level.unwrap_or_else(|| default_item_level(w, weapon.rarity.as_deref().or(w.rarity.as_deref()).unwrap_or("Legendary"), b.level, weapon.perks));
        out.base_damage = weapon_base_damage(self.bundle, level);
        out.item_damage = item_damage(self.bundle, w, level);
        let wtype = w.weapon_category.as_ref().and_then(|c| self.bundle.weapon_categories.get(c)).and_then(|c| c.kind.clone()).unwrap_or_default();
        let wcat_attr = w.weapon_category.as_ref().and_then(|c| self.bundle.weapon_categories.get(c)).and_then(|c| c.damage_attribute.clone());
        let ammo_item = ammo.and_then(|a| self.bundle.items.get(a));
        // the hit's Damage config (0x1403e9de0, written into the impact's DamageSetup-typed config by 0x141217210):
        // total = weapon item damage + ammo/spell item damage, each at its own item level (0x1403eaf00 → 0x1403b1660 →
        // 0x140e5dea0); distribution = weapon's + each active weapon perk's + the ammo/spell's, summed (no
        // normalisation); per type = total × share (0x140e75300)
        out.ammo_damage = ammo_item
            .map(|a| item_damage(self.bundle, a, self.ammo_level(b, ammo.unwrap_or_default(), a)))
            .unwrap_or(0.0);
        let share = |d: Option<&ens_data::DamageSetup>, k: &str| d.and_then(|d| d.distribution.get(k)).and_then(|x| x.as_f64()).unwrap_or(0.0);
        let mut dist: Vec<f64> = DAMAGE_TYPES
            .iter()
            .map(|(k, _, _)| share(w.damage.as_ref(), k) + share(ammo_item.and_then(|a| a.damage.as_ref()), k))
            .collect();
        let rarity = weapon.rarity.clone().or(w.rarity.clone()).unwrap_or_else(|| "Legendary".into());
        for pid in w.perks.iter().take(perk_count(weapon, &rarity)) {
            if let Some(dd) = self.bundle.perks.get(pid).and_then(|p| p.damage.as_ref()) {
                for (i, (k, _, _)) in DAMAGE_TYPES.iter().enumerate() {
                    dist[i] += dd.get(*k).copied().unwrap_or(0.0);
                }
            }
        }
        for x in dist.iter_mut() {
            *x += b.extra_dist;
        }
        let raw = match &base_setup {
            Some(shares) => {
                dist = shares.clone();
                out.base_damage * attack_mod
            }
            None => (out.item_damage + out.ammo_damage) * attack_mod,
        };
        // ApplyDamage (0x1403d4d50): the attribute scale and the weapon-category mod MULTIPLY the base (the helper is
        // called with the base array as both accumulator and weights: x += (f - 1) x). The base is then copied into the
        // accumulator and DamageMod, the attack-category mod and its per-element mod, "against" mods and the crit
        // multiplier are ADDED as (f - 1) x base.
        // the attack's DamageScaleAttribute and attack category come from the ammo/spell's config when it has one,
        // else the weapon's (ItemInfo.impactValues configs 1395916278 and 606890942); the type table is a fallback
        let cfgs: Vec<&ens_data::ItemCombat> = [ammo_item.and_then(|i| i.combat.as_ref()), w.combat.as_ref()].into_iter().flatten().collect();
        let cfg_cat = cfgs.iter()
            .find_map(|c| c.categories.as_ref().and_then(|cs| cs.iter().find(|x| matches!(x.as_str(), "Melee" | "Ranged" | "Magic")).cloned()));
        let cat: &str = match category.as_deref().or(cfg_cat.as_deref()) {
            Some("Melee") => "Melee",
            Some("Ranged") => "Ranged",
            Some("Magic") => "Magic",
            _ => attack_category(&wtype),
        };
        let cfg_scale = cfgs.iter().find_map(|c| c.scale.clone());
        let scale_attr: &str = match cfg_scale.as_deref() {
            Some("Strength") => "Strength",
            Some("Dexterity") => "Dexterity",
            Some("Intelligence") => "Intelligence",
            Some(_) => scaling_attribute(&wtype),
            None if w.combat.is_some() => "",
            None => scaling_attribute(&wtype),
        };
        let attr_points = st.get(scale_attr);
        let per = self.bundle.balance("damageScalePerAttributePoint").unwrap_or(0.05);
        let attr_factor = 1.0 + per * attr_points;
        let wcat_factor = wcat_attr.as_ref().map(|a| st.get(a)).unwrap_or(1.0);
        let mut terms = vec![
            (format!("× {scale_attr} ({attr_points:.0}) scale"), attr_factor),
            (format!("× {}", wcat_attr.clone().unwrap_or_else(|| "weapon category".into())), wcat_factor),
        ];
        let cat_attr = format!("{cat}DamageMod");
        let mut common = vec![("DamageMod".to_string(), st.get("DamageMod") - 1.0), (cat_attr.clone(), st.get(&cat_attr) - 1.0)];
        // "against" mods (ApplyDamage 0x1403d65ce–0x1403d673d): start at 1; the target's enemy class (Enemy01BehaviorState
        // byte +0x988: Melee/Ranger/Mage) replaces it with DamageModAgainst_Melee/_Ranged/_Mage; a flying target adds
        // DamageModAgainst_Flying − 1; the target's FactionType (jump table 0x1403d7658) adds its faction mod − 1. The
        // sum minus 1 joins the additive pool.
        let mut against = match target.enemy_class.as_deref() {
            Some("Melee") => st.get("DamageModAgainst_Melee"),
            Some("Ranger") => st.get("DamageModAgainst_Ranged"),
            Some("Mage") => st.get("DamageModAgainst_Mage"),
            _ => 1.0,
        };
        if target.flying {
            against += st.get("DamageModAgainst_Flying") - 1.0;
        }
        if let Some(a) = target.faction.as_deref().and_then(faction_against_attribute) {
            against += st.get(a) - 1.0;
        }
        if (against - 1.0).abs() > 1e-12 {
            common.push(("DamageModAgainst (target)".to_string(), against - 1.0));
        }
        // skill shot (ApplyDamage 0x1403d6324–0x1403d65b1): only when the hit is a skill shot and the target's
        // DamageSusceptibility_SkillShot is above 1; then DamageModSkillShot − 1 and that susceptibility − 1 join the pool
        let ss = target.susceptibility.get("SkillShot").copied().unwrap_or(1.0);
        // the hit is a skill shot when its attack category has bit 0x40 (`test byte ptr [rsp + 0x70], 0x40` at
        // 0x1403d6324; keen::impact::AttackCategory SkillShot = 6), from the item's categories or its projectile's own
        let skill_shot = cfgs.iter().any(|c| c.categories.as_ref().is_some_and(|cs| cs.iter().any(|x| x == "SkillShot")))
            || ammo_item.is_some_and(projectile_is_skill_shot);
        if target.weak_spot && skill_shot && ss > 1.0 {
            common.push(("DamageModSkillShot (weak spot)".to_string(), st.get("DamageModSkillShot") - 1.0));
            common.push(("DamageSusceptibility_SkillShot (target)".to_string(), ss - 1.0));
        }
        // sneak / merciless (0x1403d68d2–0x1403d6947): the hit-type byte 1 adds DamageModSneak − 1, 2 adds
        // DamageModMerciless − 1 to the pool
        // backstab (0x1403d61bb–0x1403d625a, 0x1403d68aa–0x1403d68c9): a damage hit that is not AoE (category bit
        // 0x20) on a target whose CombatFlags.canBeBackstabbed is set, landing within ±80° (1.396 rad) of its back,
        // adds DamageModBackstab − 1 to the pool (sneak and merciless take precedence)
        // the AoE bit can come from the item's categories or from its projectile's own AttackCategory (zone spells
        // such as Pillar Of Flames: combat [Magic], projectile config [Magic, AoE])
        let aoe = cfgs.iter().any(|c| c.categories.as_ref().map(|cs| cs.iter().any(|x| x == "AoE")).unwrap_or(false))
            || ammo_item.is_some_and(projectile_is_aoe);
        let backstabbable = target.enemy_type.as_ref().and_then(|n| self.bundle.enemy_types.get(n)).map(|e| e.backstab).unwrap_or(true);
        let kind = target.hit_kind.as_deref().filter(|k| self.hit_kind_problem(b, k).is_none());
        match kind {
            Some("Sneak") => common.push(("DamageModSneak (sneak attack)".to_string(), st.get("DamageModSneak") - 1.0)),
            Some("Merciless") => common.push(("DamageModMerciless (merciless attack)".to_string(), st.get("DamageModMerciless") - 1.0)),
            Some("Backstab") if !aoe && backstabbable => common.push(("DamageModBackstab (from behind)".to_string(), st.get("DamageModBackstab") - 1.0)),
            _ => {}
        }
        let common_sum: f64 = common.iter().map(|(_, x)| x).sum();
        for (k, v) in &common {
            terms.push((format!("+ {k}"), *v));
        }
        out.pool_terms = terms;
        // a soaked target (state 0x12) adds CritChance_Adder_Vs_Soaked in the crit function (0x1403de544) and
        // CritDamageMod_Vs_Soaked to the crit damage (ApplyDamage 0x1403d5ba0)
        let soak = |a: &str| if target.soaked { st.get(a) } else { 0.0 };
        out.crit_chance = (st.get("CritChance") + st.get(&format!("CritChance_Adder_{cat}")) + soak("CritChance_Adder_Vs_Soaked")).clamp(0.0, st.get("CritChance_Max").max(0.01));
        out.crit_damage_mod = st.get("CritDamageMod") + soak("CritDamageMod_Vs_Soaked");
        let crit_add = out.crit_damage_mod - 1.0;
        let (mut total, mut total_exp) = (0.0, 0.0);
        let mut per_type_crit = vec![];
        let eff_sus = self.effective_susceptibility(&target.susceptibility);
        for (i, (_, suffix, short)) in DAMAGE_TYPES.iter().enumerate() {
            if dist[i] == 0.0 {
                continue;
            }
            let elem = if suffix.is_empty() { 0.0 } else { st.get(&format!("{cat_attr}_{suffix}")) - 1.0 };
            // the attack-category bonus reaches only slots 0-6, cut to lightning: the melee one (FUN_1403d7b80, called
            // at ApplyDamage `if ((uVar17 & 1) != 0) FUN_1403d7b80(&local_708,&local_668,local_358,uVar25)`) writes
            // `param_1[k] = ((elem_k - 1.0) + fVar3) * param_2[k] + param_1[k]` for k = 0..6 and never slot 8, poison.
            // Logged: the Acid Cleaver's poison 21.0 beside 21.63 cut, a +3% MeleeDamageMod character
            // (combat-1792718413.csv); crit and backstab adds still reach it (31.5 and 34.65 at AttackMod 1.5)
            let category = if *short == "Poison" { st.get(&cat_attr) - 1.0 } else { 0.0 };
            let pool = 1.0 + common_sum - category + elem;
            let sus = eff_sus.get(*short).copied().unwrap_or(1.0);
            let scaled = raw * dist[i] * attr_factor * wcat_factor * sus;
            out.per_type.push((short.to_string(), scaled * pool));
            per_type_crit.push((short.to_string(), scaled * (pool + crit_add)));
            total += scaled * pool;
            total_exp += scaled * (pool + out.crit_chance * crit_add);
        }
        out.non_crit = total;
        out.expected = total_exp;
        // armor (0x1403d3f90) runs on every hit, and flat armor is not linear: a normal hit and a crit each lose
        // armor on their own, weighted by the crit chance (not the averaged hit once)
        let blow = self.bundle.balance("defaultArmorBlowthrough").unwrap_or(0.2);
        let crit_total: f64 = per_type_crit.iter().map(|(_, d)| d).sum();
        out.crit_multiplier = if out.non_crit > 0.0 { crit_total / out.non_crit } else { out.crit_damage_mod };
        out.after_armor = (1.0 - out.crit_chance) * apply_armor(out.non_crit, &out.per_type, target, blow)
            + out.crit_chance * apply_armor(crit_total, &per_type_crit, target, blow);
        out
    }
}

/// Expected projectiles of one attack from the ammo/spell's Primary sequence: every spawn whose entity tag matches
/// the item's sequence tag and whose required skill is owned, weighted by its chance (decoded actor sequence).
#[derive(Debug, Clone, Default)]
pub struct AttackProfile {
    pub projectiles: f64,
    pub parts: Vec<(String, f64)>,
    pub first_spawn_s: Option<f64>,
}

impl<'a> Engine<'a> {
    pub fn attack_profile(&self, b: &Build, ammo: Option<&str>, trigger: &str) -> AttackProfile {
        let mut out = AttackProfile::default();
        let Some(item) = ammo.and_then(|a| self.bundle.items.get(a)) else {
            // a wand fires its own projectiles: as many as its attack sequence spawns without a skill (spread wands 2)
            out.projectiles = b.weapon.as_ref().map(|w| self.wand_spawns(b, w).0).filter(|n| *n > 0.0).unwrap_or(1.0);
            return out;
        };
        let ranks = self.effective_ranks(b);
        let owned = |id: u64| self.skill_by_id.get(&(id as u32)).map(|&i| ranks[i] > 0).unwrap_or(false);
        for s in item.sequences.iter().filter(|s| s.trigger == trigger) {
            let Some(seq) = self.bundle.sequences.get(&s.sequence) else { continue };
            for sp in seq.get("spawns").and_then(|x| x.as_array()).into_iter().flatten() {
                let tag = sp.get("tag").and_then(|x| x.as_str());
                if let Some(t) = tag {
                    if !s.tags.is_empty() && !s.tags.iter().any(|x| x == t) {
                        continue;
                    }
                }
                if let Some(req) = sp.get("requiredSkillId").and_then(|x| x.as_u64()) {
                    if !owned(req) {
                        continue;
                    }
                }
                if let Some(fb) = sp.get("forbiddenSkillId").and_then(|x| x.as_u64()) {
                    if owned(fb) {
                        continue;
                    }
                }
                let chance = sp.get("chance").and_then(|x| x.as_f64()).unwrap_or(1.0);
                let t = sp.get("t").and_then(|x| x.as_f64()).unwrap_or(0.0);
                out.first_spawn_s = Some(out.first_spawn_s.map_or(t, |f: f64| f.min(t)));
                let who = sp.get("requiredSkillId").and_then(|x| x.as_u64()).and_then(|id| self.skill_by_id.get(&(id as u32))).map(|&i| self.bundle.skills[i].name.clone()).unwrap_or_else(|| "base".into());
                out.projectiles += chance;
                out.parts.push((format!("{who} @{t:.2}s"), chance));
            }
        }
        if out.projectiles == 0.0 {
            out.projectiles = 1.0;
        }
        out
    }
}

/// Seconds per attack from the item data: bows = draw (config 1469785687) × RangedCombat_Arrow_ChargeDurationMod +
/// fire phase (config 1101517683) × _FireDurationMod; staffs use the spell's own charge/fire configs with the Spell
/// mods; wands use their own charge and fire configs with no duration mods (proof rate.wand: the tooltip's "Attack
/// Speed", config 1674724000, changes nothing). Melee falls back to an assumed swing time.
impl<'a> Engine<'a> {
    pub fn attack_interval(&self, b: &Build, weapon: &Equip, ammo: Option<&str>) -> (f64, String) {
        let st = self.attributes(b);
        let Some(w) = self.bundle.items.get(&weapon.item) else { return (1.0, "no weapon".into()) };
        let kind = w.weapon_category.as_ref().and_then(|c| self.bundle.weapon_categories.get(c)).and_then(|c| c.kind.clone()).unwrap_or_default();
        let cfg = |i: &Item, k: &str| i.configs.get(k).and_then(|v| v.as_f64()).unwrap_or(0.0);
        match kind.as_str() {
            // ranged weapon parameters (0x14046ef90 → 0x14046e010): charge = config 1469785687 and fire = config
            // 1101517683 (ids at .rdata 0x141510c28/0x141510c2c) of the weapon and of its ammo/spell, the larger of
            // the two each; bow (ammunition type Arrow) and staff (Spell) multiply them by the ammo's
            // RangedCombat_Arrow_/_Spell_ ChargeDurationMod and FireDurationMod; a wand (no ammo type) keeps its own
            // values without mods. One shot = the charge phase then the fire phase.
            "RangedBow" | "RangedStaff" | "RangedWand" => {
                let ammo_item = if kind == "RangedWand" { None } else { ammo.and_then(|a| self.bundle.items.get(a)) };
                if kind == "RangedStaff" && ammo_item.is_none() {
                    return (1.0, "staff without spell".into());
                }
                let charge = cfg(w, "1469785687").max(ammo_item.map(|a| cfg(a, "1469785687")).unwrap_or(0.0));
                let fire = cfg(w, "1101517683").max(ammo_item.map(|a| cfg(a, "1101517683")).unwrap_or(0.0));
                let (cm, fm) = match kind.as_str() {
                    "RangedBow" => (st.get("RangedCombat_Arrow_ChargeDurationMod"), st.get("RangedCombat_Arrow_FireDurationMod")),
                    "RangedStaff" => (st.get("RangedCombat_Spell_ChargeDurationMod"), st.get("RangedCombat_Spell_FireDurationMod")),
                    _ => (1.0, 1.0),
                };
                let (cm, fm) = (if cm > 0.0 { cm } else { 1.0 }, if fm > 0.0 { fm } else { 1.0 });
                let t = charge * cm + fire * fm;
                (if t > 0.0 { t } else { 1.0 }, format!("charge {charge}s×{cm:.2} + fire {fire}s×{fm:.2}"))
            }
            _ => {
                // melee: walk the light-attack combo chain (Engine::melee_combo). Interval reported per hit.
                if let Some(c) = self.melee_combo(b, weapon) {
                    return (c.time / c.hits, format!("{}-step combo, {:.0} hits in {:.2}s, average attack multiplier {:.2}", c.steps, c.hits, c.time, c.mean_attack_mod()));
                }
                (1.0, "melee (no sequence length)".into())
            }
        }
    }
}

/// A melee weapon's light-attack combo: its length, its hits (distinct impact times) and their AttackMods.
#[derive(Debug, Clone)]
pub struct MeleeCombo {
    /// The repeating steps' sequence names, in order.
    pub names: Vec<String>,
    pub steps: usize,
    pub time: f64,
    pub hits: f64,
    pub attack_mod_sum: f64,
}

impl MeleeCombo {
    pub fn mean_attack_mod(&self) -> f64 {
        if self.hits > 0.0 { self.attack_mod_sum / self.hits } else { 1.0 }
    }
}

impl<'a> Engine<'a> {
    /// The light-attack combo the build can keep up with the most damage: every chain of Primary follow-ups
    /// (SetActionOportunity Primary -> followUp sequence) the build may take, early and late timings alike (the
    /// greatsword's third swing pressed at 0.683 s loops to the first, at 1.317 s plays `Late_1_Uppercut` at
    /// AttackMod 2.0), skill-gated ones when the skill is owned and none whose skill forbids it. A chain repeats once it
    /// reaches a step it has played, or restarts from the first swing after a step with no follow-up. Each step lasts
    /// until its follow-up starts (its followUpTime; a last step, its length) and lands its distinct impact times, each at
    /// its own AttackMod (the sequence's SpawnImpact impactValues, else the program default; the daggers' Spin lands 0.5
    /// and 3.0, the Greataxe Smash 4.0: combat-1792718413.csv). The repeating part with the most AttackMod per second
    /// wins. None when the weapon has no combo.
    ///
    /// With `Build::heavy` the same walk follows the Heavy trigger from the weapon's first heavy attack
    /// (`Attack_Player_Melee_Heavy_*`; the greatsword's Uppercut lands AttackMod 3.0 and repeats after 2.416 s); a
    /// weapon without one keeps its light combo. Heavy attacks hit every enemy within reach in 1.0; the reach is not in
    /// the sequence data, so one target is counted.
    pub fn melee_combo(&self, b: &Build, weapon: &Equip) -> Option<MeleeCombo> {
        let w = self.bundle.items.get(&weapon.item)?;
        let ranks = self.effective_ranks(b);
        let owned = |id: u64| self.skill_by_id.get(&(id as u32)).map(|&i| ranks[i] > 0).unwrap_or(false);
        // a sequence's names (None when it has no names list)
        let names_of = |sq: &ens_data::ItemSequence| self.bundle.sequences.get(&sq.sequence).and_then(|q| q.get("names")).and_then(|n| n.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str()).map(str::to_string).collect::<Vec<_>>());
        let heavy_first = b.heavy.then(|| w.sequences.iter().filter(|s| s.trigger == "Heavy")
            .find(|sq| names_of(sq).is_some_and(|n| !n.is_empty() && n.iter().all(|n| n.contains("_Heavy_") && !n.contains("Special"))))).flatten();
        let trigger = if heavy_first.is_some() { "Heavy" } else { "Primary" };
        let first = match heavy_first {
            Some(f) => f,
            None => w.sequences.iter().filter(|s| s.trigger == "Primary").find(|sq| names_of(sq).is_some_and(|n| n.iter().all(|n| !n.contains("Special"))))?,
        };
        // one step: (hits, AttackMod sum) of its distinct impact times; an impact without an AttackMod counts as 1
        let step = |q: &serde_json::Value| -> (f64, f64) {
            let mut ts: Vec<(f64, f64)> = q.get("impacts").and_then(|x| x.as_array()).into_iter().flatten()
                .filter_map(|i| {
                    let am = i.get("config").and_then(|c| c.get("AttackMod").or(c.get("AttackMod(default)"))).and_then(|x| x.as_f64())?;
                    Some((i.get("t")?.as_f64()?, am))
                })
                .collect();
            ts.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
            ts.dedup_by(|a, b| (a.0 - b.0).abs() < 1e-3);
            if ts.is_empty() { (1.0, 1.0) } else { (ts.len() as f64, ts.iter().map(|t| t.1).sum()) }
        };
        let allowed = |f: &serde_json::Value| {
            f.get("trigger").and_then(|t| t.as_str()) == Some(trigger)
                && f.get("requiredSkillId").and_then(|x| x.as_u64()).map(owned).unwrap_or(true)
                && !f.get("forbiddenSkillId").and_then(|x| x.as_u64()).map(owned).unwrap_or(false)
        };
        struct Search<'s> {
            best: Option<MeleeCombo>,
            path: Vec<(String, f64, f64, f64, String)>,
            first: &'s str,
        }
        fn close(s: &mut Search, from: usize) {
            let part = &s.path[from..];
            let c = MeleeCombo {
                names: part.iter().map(|p| p.4.clone()).collect(),
                steps: part.len(),
                time: part.iter().map(|p| p.3).sum(),
                hits: part.iter().map(|p| p.1).sum(),
                attack_mod_sum: part.iter().map(|p| p.2).sum(),
            };
            let rate = |c: &MeleeCombo| if c.time > 0.0 { c.attack_mod_sum / c.time } else { 0.0 };
            if c.time > 0.0 && s.best.as_ref().is_none_or(|b| rate(&c) > rate(b) + 1e-12) {
                s.best = Some(c);
            }
        }
        let mut search = Search { best: None, path: vec![], first: &first.sequence };
        fn explore(e: &Engine, g: &str, s: &mut Search, step: &dyn Fn(&serde_json::Value) -> (f64, f64), allowed: &dyn Fn(&serde_json::Value) -> bool) {
            let Some(q) = e.bundle.sequences.get(g) else { return };
            let (hits, ams) = step(q);
            let len = q.get("lengthS").and_then(|x| x.as_f64()).unwrap_or(1.0);
            let fus: Vec<&serde_json::Value> = q.get("followUps").and_then(|x| x.as_array()).into_iter().flatten().filter(|f| allowed(f)).collect();
            let name = q.get("names").and_then(|n| n.as_array()).and_then(|a| a.first()).and_then(|x| x.as_str()).unwrap_or("").to_string();
            if fus.is_empty() {
                // no follow-up: the next attack starts the combo again
                s.path.push((g.to_string(), hits, ams, len, name.clone()));
                let at = s.path.iter().position(|p| p.0 == s.first).unwrap_or(0);
                close(s, at);
                s.path.pop();
                return;
            }
            for f in fus {
                let dt = f.get("followUpTimeS").and_then(|x| x.as_f64()).unwrap_or(len);
                let next = f.get("followUp").and_then(|x| x.as_str()).unwrap_or("").to_string();
                s.path.push((g.to_string(), hits, ams, dt, name.clone()));
                if let Some(at) = s.path.iter().position(|p| p.0 == next) {
                    close(s, at);
                } else if s.path.len() < 8 {
                    explore(e, &next, s, step, allowed);
                }
                s.path.pop();
            }
        }
        explore(self, &first.sequence, &mut search, &step, &allowed);
        search.best
    }
}

impl DpsBreakdown {
    /// Hit events on the target per attack: the projectiles, each landing `zone_hits` times for a zone spell (on-hit
    /// procs, Focus and hit- or crit-triggered buffs count every one).
    pub fn hits_per_attack(&self) -> f64 {
        self.projectiles * self.zone_hits.max(1.0)
    }
}

/// Damage per second from regular attacks plus the weapon special (Focus).
#[derive(Debug, Clone, Default)]
pub struct DpsBreakdown {
    pub hit: HitBreakdown,
    pub projectiles: f64,
    pub attacks_per_s: f64,
    pub regular_dps: f64,
    /// Special ability: Focus per hit (weapon config SpecialAbilityGain), Focus cost (SpecialAbility_Max),
    /// projectiles and AttackMod of the special's spawns.
    pub focus_per_hit: f64,
    pub focus_cost: f64,
    pub special_projectiles: f64,
    pub special_attack_mod: f64,
    pub special_per_s: f64,
    pub special_dps: f64,
    /// Damage of one special use: its weapon hits (melee specials' impacts) and spawned hits, after armor.
    pub special_per_use: f64,
    /// Area effect of the ammo (explosion child template): radius (m), AttackMod, enemies covered.
    pub aoe_radius: f64,
    pub aoe_attack_mod: f64,
    pub aoe_targets: f64,
    /// Hits each enemy takes from one cast of a zone spell (1 for everything else).
    pub zone_hits: f64,
    pub total: f64,
}

impl<'a> Engine<'a> {
    /// For a wand's regular attack (its first Primary sequence that is not a special): (base spawns + Σ owned
    /// skill-gated spawns' chance × AttackMod) ÷ base spawns, from the sequence's spawn list. 1 for other weapons and
    /// when the build owns no gating skill. Lightning chain with Arcane Proliferation: (2 + 0.15 × 0.25) ÷ 2.
    pub fn wand_spawn_factor(&self, b: &Build, weapon: &Equip) -> f64 {
        let (base, extra) = self.wand_spawns(b, weapon);
        if base > 0.0 { (base + extra) / base } else { 1.0 }
    }

    /// A wand attack's (base spawns, Σ owned skill-gated spawns' chance × AttackMod) from its first Primary sequence
    /// that is not a special; (0, 0) for other weapons.
    pub fn wand_spawns(&self, b: &Build, weapon: &Equip) -> (f64, f64) {
        let Some(w) = self.bundle.items.get(&weapon.item) else { return (0.0, 0.0) };
        let kind = w.weapon_category.as_ref().and_then(|c| self.bundle.weapon_categories.get(c)).and_then(|c| c.kind.as_deref());
        if kind != Some("RangedWand") {
            return (0.0, 0.0);
        }
        let ranks = self.effective_ranks(b);
        let owned = |id: u64| self.skill_by_id.get(&(id as u32)).map(|&i| ranks[i] > 0).unwrap_or(false);
        let Some(q) = w.sequences.iter().filter(|s| s.trigger == "Primary")
            .filter_map(|s| self.bundle.sequences.get(&s.sequence))
            .find(|q| q.get("names").and_then(|n| n.as_array()).is_some_and(|a| a.iter().filter_map(|x| x.as_str()).all(|n| !n.contains("Special")))) else { return (0.0, 0.0) };
        let (mut base, mut extra) = (0.0, 0.0);
        for sp in q.get("spawns").and_then(|x| x.as_array()).into_iter().flatten() {
            let chance = sp.get("chance").and_then(|x| x.as_f64()).unwrap_or(1.0);
            match sp.get("requiredSkillId").and_then(|x| x.as_u64()) {
                None => base += chance,
                Some(id) if owned(id) => {
                    let am = sp.get("template").and_then(|t| t.get("config")).and_then(|c| c.get("AttackMod").or(c.get("AttackMod(default)"))).and_then(|x| x.as_f64()).unwrap_or(1.0);
                    extra += chance * am;
                }
                _ => {}
            }
        }
        (base, extra)
    }

    /// Why this build cannot make hits of `kind`, or None when it can. Sneak and Merciless Attacks are active skills
    /// that have to be bought, and their tooltips say "Wands, Staves and Bows cannot perform Sneak Attacks" (and
    /// Merciless Attacks). Backstab needs no skill.
    pub fn hit_kind_problem(&self, b: &Build, kind: &str) -> Option<String> {
        let skill = match kind {
            "Sneak" => "Sneak Attack",
            "Merciless" => "Merciless Attack",
            // an attack whose own category is AoE never backstabs (the backstab check skips category bit 0x20): the
            // explosive arrows' combat categories are [Ranged, AoE], so even their direct hit cannot
            "Backstab" => {
                let ammo = b.ammo.as_deref().and_then(|a| self.bundle.items.get(a));
                let aoe = ammo.is_some_and(|i| projectile_is_aoe(i) || i.combat.as_ref().and_then(|c| c.categories.as_ref()).is_some_and(|cs| cs.iter().any(|x| x == "AoE")));
                return aoe.then(|| format!("{}'s hits are area hits (attack category AoE), and area hits never backstab.", ammo.map(|i| i.name.as_str()).unwrap_or("This ammo")));
            }
            _ => return None,
        };
        let kind_of = b.weapon.as_ref().and_then(|w| self.bundle.items.get(&w.item)).and_then(|i| i.weapon_category.as_ref())
            .and_then(|c| self.bundle.weapon_categories.get(c)).and_then(|c| c.kind.as_deref());
        if matches!(kind_of, Some("RangedBow" | "RangedStaff" | "RangedWand")) {
            return Some(format!("Wands, staves and bows cannot perform {skill}s."));
        }
        let ranks = self.effective_ranks(b);
        if !self.bundle.skills.iter().enumerate().any(|(i, s)| s.name == skill && ranks.get(i).is_some_and(|&r| r > 0)) {
            return Some(format!("{skill} is not in your skill tree."));
        }
        None
    }

    pub fn dps(&self, b: &Build, weapon: &Equip, ammo: Option<&str>, target: &Target, attacks_per_s: f64) -> DpsBreakdown {
        // a spell that deals no damage (Mana Drain: dmgMod 0; heals, barriers, roots: only a healing share) is not
        // an attack, so it adds no damage of its own or the staff's
        if ammo.and_then(|a| self.bundle.items.get(a)).is_some_and(|i| i.slot.as_deref() == Some("Spell") && !deals_damage(i)) {
            return DpsBreakdown { attacks_per_s, ..Default::default() };
        }
        // a melee hit lands at its combo step's AttackMod: price the regular hit at the combo's mean
        let melee_am = ammo.is_none().then(|| self.melee_combo(b, weapon)).flatten().map(|c| c.mean_attack_mod()).unwrap_or(1.0);
        // a sneak or merciless attack is its own key-press action on one enemy (the weapon's Interact sequence, e.g.
        // Attack_Player_Melee_Special_SneakAttack_Greatsword), whose damage impact carries no AttackMod config, so the
        // program default 1 applies; it is not a step of the light or heavy combo
        let special_action = ammo.is_none() && target.hit_kind.as_deref().is_some_and(|k| matches!(k, "Sneak" | "Merciless") && self.hit_kind_problem(b, k).is_none());
        let hit = self.hit(b, weapon, ammo, if special_action { 1.0 } else { melee_am }, target);
        let prof = self.attack_profile(b, ammo, "Primary");
        let mut d = DpsBreakdown { projectiles: prof.projectiles, attacks_per_s, ..Default::default() };
        // only a share of the hits may be of the scenario's kind (a merciless attack needs an overpowered enemy); the
        // others are the combo's normal hits
        let share = target.hit_share.filter(|_| target.hit_kind.is_some()).map(|s| s.clamp(0.0, 1.0));
        let normal = target.hit_kind.is_some().then(|| self.hit(b, weapon, ammo, melee_am, &Target { hit_kind: None, hit_share: None, ..target.clone() }).after_armor);
        let regular_hit = match (share.unwrap_or(1.0), normal) {
            (s, Some(n)) if s < 1.0 => s * hit.after_armor + (1.0 - s) * n,
            _ => hit.after_armor,
        };
        d.regular_dps = regular_hit * prof.projectiles * attacks_per_s;
        // a sneak or merciless attack also has its own pace: one per its sequence's length (greatsword 2.0 s and
        // 2.27 s), whatever the combo or heavy setting; the share is then of your attacking time
        let special_len = special_action.then(|| {
            let name = if target.hit_kind.as_deref() == Some("Sneak") { "SneakAttack" } else { "MercilessAttack" };
            self.bundle.items.get(&weapon.item).into_iter().flat_map(|w| w.sequences.iter()).filter(|s| s.trigger == "Interact")
                .filter_map(|s| self.bundle.sequences.get(&s.sequence))
                .find(|q| q.get("names").and_then(|n| n.as_array()).is_some_and(|a| a.iter().filter_map(|x| x.as_str()).any(|n| n.contains(name))))
                .and_then(|q| q.get("lengthS")).and_then(|x| x.as_f64()).filter(|l| *l > 0.0)
        }).flatten();
        let s_share = share.unwrap_or(1.0);
        let normal_dps = normal.unwrap_or(hit.after_armor) * prof.projectiles * attacks_per_s * (1.0 - s_share);
        if let Some(len) = special_len {
            d.regular_dps = s_share * hit.after_armor / len + normal_dps;
            // the attacks actually made: one special action per its length for the share of attacking time, combo
            // attacks for the rest; Focus gain, hit procs and triggered buffs count these
            d.attacks_per_s = s_share / len + (1.0 - s_share) * attacks_per_s;
        }
        // a wand fires its own projectiles; skill-gated extra spawns on its attack (Arcane Proliferation: a chance of
        // more projectiles at a lower AttackMod) add their chance × AttackMod against the attack's base spawns
        if ammo.is_none() {
            d.regular_dps *= self.wand_spawn_factor(b, weapon);
        }
        let w = self.bundle.items.get(&weapon.item);
        d.focus_per_hit = w.and_then(|i| i.configs.get("1236200858")).and_then(|v| v.as_f64()).unwrap_or(0.0);
        d.focus_cost = self.attrs.base("SpecialAbility_Max").max(1.0);
        let ranks = self.effective_ranks(b);
        let owned = |id: u32| self.skill_by_id.get(&id).map(|&i| ranks[i] > 0).unwrap_or(false);
        // a special's damage per use (docs/plans/weapon-specials.md): the sequence's own impacts are weapon hits at their
        // AttackMod, one per distinct impact time (melee specials); each spawn the weapon's tags allow is priced as its
        // template's program prices it (Engine::template_hit), and a spawned child that deals damage (the wand's
        // implosion) hits everyone inside its radius
        let n = target.pack_size.max(1.0);
        let rr = if target.pack_radius > 0.0 { target.pack_radius } else { 4.5 };
        let inside = |r: f64| if n > 1.0 { 1.0 + (n - 1.0) * (r / rr).powi(2).min(1.0) } else { 1.0 };
        let am_of = |c: Option<&serde_json::Value>| c.and_then(|c| c.get("AttackMod").or(c.get("AttackMod(default)"))).and_then(|x| x.as_f64());
        let gate = |v: &serde_json::Value| {
            v.get("requiredSkillId").and_then(|x| x.as_u64()).map(|r| owned(r as u32)).unwrap_or(true)
                && v.get("forbiddenSkillId").and_then(|x| x.as_u64()).map(|f| !owned(f as u32)).unwrap_or(true)
        };
        let special_normal = Target { hit_kind: None, hit_share: None, ..target.clone() };
        // the weapon special is its own SpecialAbility sequence: its hits are never sneak or merciless attacks (that is
        // the Interact action), and only the backstab share of them lands from behind
        let back_share = match target.hit_kind.as_deref() {
            Some("Backstab") => share.unwrap_or(1.0),
            Some(_) => 0.0,
            None => 1.0,
        };
        let price_hit = |am: f64| {
            let n = self.hit(b, weapon, ammo, am, &special_normal).after_armor;
            if back_share > 0.0 && target.hit_kind.is_some() { back_share * self.hit(b, weapon, ammo, am, target).after_armor + (1.0 - back_share) * n } else { n }
        };
        let price_tpl = |name: &str, am: f64| {
            let n = self.template_hit(b, weapon, ammo, &special_normal, name, am, 0.5);
            if back_share > 0.0 && target.hit_kind.is_some() {
                let k = self.template_hit(b, weapon, ammo, target, name, am, 0.5);
                (back_share * k.after_armor + (1.0 - back_share) * n.after_armor, n.attack_mod)
            } else {
                (n.after_armor, n.attack_mod)
            }
        };
        if let Some(w) = w {
            for s in w.sequences.iter().filter(|s| s.trigger == "SpecialAbility") {
                let req = s.required_skill.as_deref().map(fnv1a_hex);
                if req.map(|id| !owned(id)).unwrap_or(false) {
                    continue;
                }
                let Some(seq) = self.bundle.sequences.get(&s.sequence) else { continue };
                let mut times: Vec<(f64, f64)> = seq.get("impacts").and_then(|x| x.as_array()).into_iter().flatten()
                    .filter(|i| gate(i))
                    .filter_map(|i| Some((i.get("t")?.as_f64()?, am_of(i.get("config"))?)))
                    .collect();
                times.sort_by(|a, b| a.0.total_cmp(&b.0));
                times.dedup_by(|a, b| (a.0 - b.0).abs() < 1e-3);
                for (_, am) in &times {
                    d.special_per_use += price_hit(*am);
                    d.special_projectiles += 1.0;
                    d.special_attack_mod = d.special_attack_mod.max(*am);
                }
                for sp in seq.get("spawns").and_then(|x| x.as_array()).into_iter().flatten().filter(|sp| gate(sp)) {
                    if let Some(tag) = sp.get("tag").and_then(|x| x.as_str()) {
                        if !s.tags.is_empty() && !s.tags.iter().any(|t| t == tag) {
                            continue;
                        }
                    }
                    let tpl = sp.get("template");
                    let am = am_of(tpl.and_then(|t| t.get("config"))).unwrap_or(1.0);
                    let ch = sp.get("chance").and_then(|x| x.as_f64()).unwrap_or(1.0);
                    let name = tpl.and_then(|t| t.get("name")).and_then(|n| n.as_str()).unwrap_or("");
                    let (h, h_am) = price_tpl(name, am);
                    d.special_per_use += ch * h;
                    d.special_projectiles += ch;
                    d.special_attack_mod = d.special_attack_mod.max(h_am);
                    for child in tpl.and_then(|t| t.get("spawns")).and_then(|x| x.as_object()).into_iter().flat_map(|o| o.values()) {
                        let cname = child.get("name").and_then(|n| n.as_str()).unwrap_or("");
                        let r = child.get("radius").and_then(|x| x.as_f64()).unwrap_or(0.0);
                        if r > 0.5 && self.bundle.special_templates.get(cname).is_some_and(|t| t.attack_mod.is_some()) {
                            d.special_per_use += ch * inside(r) * price_tpl(cname, 1.0).0;
                        }
                    }
                }
            }
        }
        if d.special_projectiles > 0.0 && d.focus_per_hit > 0.0 {
            d.special_per_s = d.focus_per_hit * prof.projectiles * d.attacks_per_s / d.focus_cost;
            d.special_dps = d.special_per_s * d.special_per_use;
        }
        // area effect from the ammo's projectile: a spawned child template with a collider radius (explosions);
        // enemies inside = 1 + (n - 1) × min(1, (r / R)^2) for a group of n spread over radius R
        // heavy melee attacks hit every enemy within reach (1.0); the reach is a setting (HEAVY_REACH_M)
        if b.heavy && ammo.is_none() && self.melee_combo(b, weapon).is_some_and(|c| c.names.iter().all(|x| x.contains("_Heavy_"))) {
            d.aoe_radius = if target.play.heavy_reach_m > 0.0 { target.play.heavy_reach_m } else { HEAVY_REACH_M };
            d.aoe_attack_mod = 1.0;
        }
        // a projectile that is itself an area (AttackCategory AoE and a radius: zones such as Pillar Of Flames or
        // Blizzard, Ball Lightning) hits everyone inside its radius
        if let Some(i) = ammo.and_then(|a| self.bundle.items.get(a)).filter(|i| projectile_is_aoe(i)) {
            let r = i.projectile.as_ref().and_then(|p| p.get("radius")).and_then(|x| x.as_f64()).unwrap_or(0.0);
            if r > 0.5 {
                d.aoe_radius = r;
                d.aoe_attack_mod = 1.0;
            }
        }
        if let Some(p) = ammo.and_then(|a| self.bundle.items.get(a)).and_then(|i| i.projectile.as_ref()) {
            if let Some(spawns) = p.get("spawns").and_then(|x| x.as_object()) {
                for child in spawns.values() {
                    let r = child.get("radius").and_then(|x| x.as_f64()).unwrap_or(0.0);
                    if r > 0.5 {
                        d.aoe_radius = d.aoe_radius.max(r);
                        let am = child.get("config").and_then(|c| c.get("AttackMod").or(c.get("AttackMod(default)"))).and_then(|x| x.as_f64()).unwrap_or(1.0);
                        d.aoe_attack_mod = d.aoe_attack_mod.max(am);
                    }
                }
            }
        }
        if d.aoe_radius > 0.0 && n > 1.0 {
            d.aoe_targets = inside(d.aoe_radius);
            // the blast hits everyone inside at the explosion's AttackMod; the direct target is among them. A blast
            // with AttackCategory AoE never backstabs (the backstab check skips category bit 0x20), so only the
            // direct target keeps a backstab bonus; the others take the hit without it
            // the other enemies inside take normal hits: a sneak or merciless attack lands on one enemy, and a backstab
            // needs the enemy's back (a blast with AttackCategory AoE never backstabs at all)
            let others = normal.unwrap_or(hit.after_armor);
            d.regular_dps = match special_len {
                // the special action lands on its one target; only the normal attacks reach the others
                Some(len) => s_share * hit.after_armor / len + (normal_dps + (d.aoe_targets - 1.0) * normal_dps) * d.aoe_attack_mod.max(1.0),
                None => (regular_hit + (d.aoe_targets - 1.0) * others) * prof.projectiles * attacks_per_s * d.aoe_attack_mod.max(1.0),
            };
        } else {
            d.aoe_targets = 1.0;
        }
        // Explosive Arrow skills (bundle explosiveArrow): Shrapnel Blast raises the blast's AttackMod per thing it hits
        // (here the enemies inside; buildings it also hits are not known), and each enemy hit has Echoing Explosives'
        // chance of a second explosion at its AttackMod over everyone within its radius
        // a zone (Zone_* templates, Ball Lightning) hits each enemy inside once per hit interval (its ClearConsecutiveHits
        // time, bundle projectile hitIntervalS: 0.5 s) for as long as the enemy stays in it: lifetime ÷ interval hits,
        // times the share of the lifetime enemies stay (a setting), and never fewer than the first hit
        if let Some(p) = ammo.and_then(|a| self.bundle.items.get(a)).and_then(|i| i.projectile.as_ref()) {
            let num = |k: &str| p.get(k).and_then(|x| x.as_f64()).unwrap_or(0.0);
            if num("hitIntervalS") > 0.0 && num("lifetimeS") > 0.0 {
                let stay = if target.play.zone_stay > 0.0 { target.play.zone_stay.min(1.0) } else { ZONE_STAY };
                d.zone_hits = ((num("lifetimeS") / num("hitIntervalS")).floor() * stay).max(1.0);
                // casting faster than a zone lasts would keep several alive; past the spawn pool's limit a new cast
                // replaces the oldest, which then deals only part of its hits
                let cap = if target.play.zone_cap > 0.0 { target.play.zone_cap } else { ZONE_CAP };
                let alive = attacks_per_s * num("lifetimeS");
                if alive > cap {
                    d.zone_hits *= cap / alive;
                }
                d.regular_dps *= d.zone_hits;
                // every zone hit is a hit event: it builds Focus like any other hit
                d.special_per_s *= d.zone_hits;
                d.special_dps *= d.zone_hits;
            }
        }
        if let Some((blast, echo)) = self.explosive_mods(b, ammo, d.aoe_targets) {
            let echo_share = echo.map(|(chance, am, r)| chance * am * inside(r)).unwrap_or(0.0);
            d.regular_dps *= blast + echo_share;
        }
        d.total = d.regular_dps + d.special_dps;
        d.hit = hit;
        d
    }
}

impl<'a> Engine<'a> {
    /// What the build's gem needs to count, when it adds nothing in this fight: the Combat setup situations that would
    /// make it change a number (each tried on its own), or an empty list when none does (the planner does not model
    /// it). None when the gem already counts here, or there is no gem.
    pub fn gem_needs(&self, b: &Build, target: &Target) -> Option<Vec<&'static str>> {
        let g = b.gem.as_ref()?;
        self.bundle.gems.get(&g.item)?;
        let mut bare = b.clone();
        bare.gem = None;
        let moves = |t: &Target| {
            let (w, o) = (self.total(b, t), self.total(&bare, t));
            (w.total - o.total).abs() > 1e-6 || w.procs.len() != o.procs.len()
                || (self.incoming_target(b, 50, t).1 - self.incoming_target(&bare, 50, t).1).abs() > 1e-9
        };
        if moves(target) {
            return None;
        }
        // kills and orbs are what many gems run on, so each situation is tried with them on too
        let base = || Target {
            enemy_health: if target.enemy_health > 0.0 { target.enemy_health } else { 3000.0 },
            orb_pickup: if target.orb_pickup > 0.0 { target.orb_pickup } else { 1.0 },
            ..target.clone()
        };
        let tries: Vec<(&'static str, Target)> = vec![
            ("backstab hits", Target { hit_kind: Some("Backstab".into()), ..base() }),
            ("sneak attacks", Target { hit_kind: Some("Sneak".into()), ..base() }),
            ("a soaked target", Target { soaked: true, ..base() }),
            ("a group of enemies", Target { pack_size: 4.0, pack_radius: 3.0, ..base() }),
            ("kills and orb pickups", base()),
            ("a target resistant to Fog", Target { susceptibility: [("Fog".to_string(), 0.5)].into_iter().collect(), ..base() }),
            ("sprinting into the fight", Target { play: Play { sprint_start: true, ..target.play.clone() }, ..base() }),
            ("evading", Target { play: Play { evades_per_min: 10.0, ..target.play.clone() }, ..base() }),
            ("attacking in the air", Target { play: Play { air_share: 0.5, ..target.play.clone() }, ..base() }),
            ("glide landings", Target { play: Play { glides_per_min: 2.0, ..target.play.clone() }, ..base() }),
            ("blocking hits", Target { play: Play { blocks_per_min: 10.0, ..target.play.clone() }, ..base() }),
            ("fighting at night", Target { play: Play { night: true, ..target.play.clone() }, ..base() }),
            ("an elixir active", Target { play: Play { elixir: true, ..target.play.clone() }, ..base() }),
        ];
        Some(tries.into_iter().filter(|(_, t)| moves(t)).map(|(n, _)| n).collect())
    }
}

/// Whether an item deals damage: a damage block with dmgMod above 0 and a share of some damage type other than healing
/// (Mana Drain has dmgMod 0; Chain Heal, Heal Beam, Entangle and Protective Barrier carry only a healing share).
pub fn deals_damage(i: &Item) -> bool {
    i.damage.as_ref().is_some_and(|d| d.dmg_mod > 0.0
        && d.distribution.iter().any(|(k, v)| k != "healing" && v.as_f64().is_some_and(|x| x > 0.0)))
}

/// Whether a spell summons something: its projectile template is a summon template (bundle allyTypes, the PlayerAlly
/// templates: totems, wisps, Hollow Wretches), so it counts against a summon cap instead of being cast as an attack.
pub fn is_summon_spell(bundle: &Bundle, i: &Item) -> bool {
    i.slot.as_deref() == Some("Spell")
        && i.projectile.as_ref().and_then(|p| p.get("name")).and_then(|n| n.as_str()).is_some_and(|n| bundle.ally_types.contains_key(n))
}

/// Whether an item's projectile is itself an area attack (its config's AttackCategory includes AoE).
pub fn projectile_is_aoe(i: &Item) -> bool {
    i.projectile.as_ref().and_then(|p| p.get("config")).and_then(|c| c.get("AttackCategory")).and_then(|x| x.as_array())
        .is_some_and(|cs| cs.iter().any(|x| x.as_str() == Some("AoE")))
}

/// Whether an item's projectile is a skill shot (its config's AttackCategory includes SkillShot).
pub fn projectile_is_skill_shot(i: &Item) -> bool {
    i.projectile.as_ref().and_then(|p| p.get("config")).and_then(|c| c.get("AttackCategory")).and_then(|x| x.as_array())
        .is_some_and(|cs| cs.iter().any(|x| x.as_str() == Some("SkillShot")))
}

/// A zone spell: its projectile is an area that stays (Zone_* templates) and keeps hitting for its lifetime.
pub fn is_zone_spell(i: &Item) -> bool {
    projectile_is_aoe(i) && i.projectile.as_ref().and_then(|p| p.get("name")).and_then(|x| x.as_str()).is_some_and(|n| n.starts_with("Zone_"))
}

/// One kind of summon kept up by the build.
#[derive(Debug, Clone, Default)]
pub struct SummonDps {
    pub name: String,
    pub kind: &'static str,
    pub count: f64,
    pub hit: f64,
    pub interval_s: f64,
    pub each_dps: f64,
    pub total_dps: f64,
    pub mana_per_s: f64,
    /// Share of the time this summon group can be kept up with the mana left after the main attack.
    pub uptime: f64,
    pub note: String,
}

impl<'a> Engine<'a> {
    /// Damage of the build's summons. The summon spell acts as the summon's weapon (SpawnEntityEvent copies the
    /// caster's combat attributes; ASSUMED that the spell item supplies the damage: trunc(base(level) x dmgMod)).
    /// Totems (Prop_Player_Ally_Totem_*): one projectile per `wait` seconds (template config wait_min/max = 1.0 s,
    /// c = 1) during their lifetime; magic attack scaled by Intelligence (ASSUMED). Hollow Wretch: melee, scaled by
    /// Strength (its hit program's DamageScaleAttribute), AttackMod 1.0; one attack per 2 s (ASSUMED, AI timing).
    /// The item level an arrow, spell or summon spell is priced at: an upgradable (Eternal) one at its recorded or
    /// default upgrade level (character level clamped into its range, +1 per unlocked perk, as for weapons), any
    /// other at the character level clamped into its range.
    pub fn ammo_level(&self, b: &Build, guid: &str, item: &Item) -> u32 {
        if upgradable(item) {
            let e = b.upgrade_of(guid);
            let rarity = e.rarity.clone().or(item.rarity.clone()).unwrap_or_else(|| "Legendary".into());
            return e.level.unwrap_or_else(|| default_item_level(item, &rarity, b.level, e.perks));
        }
        item.levels.as_ref().map(|l| l.max.min(b.level.max(l.min))).unwrap_or(b.level)
    }

    /// Whether the character can cast spells: a weapon that fires spells (a staff, ammo kind Spell; proof
    /// spells.staff_ammo, 0x14046ef90) in hand or among the weapons carried to switch to.
    pub fn can_cast_spells(&self, b: &Build) -> bool {
        b.weapon.iter().chain(b.carried.iter().map(|c| &c.weapon))
            .any(|w| self.bundle.items.get(&w.item).and_then(|i| i.ammo.as_deref()) == Some("Spell"))
    }

    /// Share of the fight spent recasting summon spells: for each one kept up, count × uptime ÷ its lifetime casts per
    /// second, each taking the cast time of that spell from the build's staff (in hand or carried). At most 1.
    pub fn summon_casting_share(&self, b: &Build, summons: &[SummonDps]) -> f64 {
        let Some(staff) = b.weapon.iter().chain(b.carried.iter().map(|c| &c.weapon))
            .find(|w| self.bundle.items.get(&w.item).and_then(|i| i.ammo.as_deref()) == Some("Spell")) else { return 0.0 };
        let mut share = 0.0;
        for g in &b.summons {
            let Some(sp) = self.bundle.items.get(g) else { continue };
            let Some(s) = summons.iter().find(|s| s.name == sp.name) else { continue };
            let life = sp.projectile.as_ref().and_then(|t| t.get("lifetimeS")).and_then(|x| x.as_f64()).unwrap_or(60.0).max(1.0);
            let (cast, _) = self.attack_interval(b, staff, Some(g));
            share += s.count * s.uptime / life * cast;
        }
        share.clamp(0.0, 1.0)
    }

    pub fn summons(&self, b: &Build, target: &Target) -> Vec<SummonDps> {
        let st0 = self.attributes(b);
        let per = self.bundle.balance("damageScalePerAttributePoint").unwrap_or(0.05);
        let blow = self.bundle.balance("defaultArmorBlowthrough").unwrap_or(0.2);
        // the target's armor reduces a summon's hits as it does yours (0x1403d3f90): per damage type, physical pool for
        // cut/pierce/blunt and magical pool for fire/ice/fog
        let armored = |parts: Vec<(String, f64)>| armor_reduce(&parts, &|_| 0.0, target.armor_physical, target.armor_magical, blow);
        let short_of = |k: &str| -> &'static str {
            if k == "poisonDamage" { "Poison" } else { DAMAGE_TYPES.iter().find(|(key, _, _)| *key == k).map(|(_, _, s)| *s).unwrap_or("") }
        };
        let mut out = vec![];
        // a summon spell is cast from the spell slot, so only a character with a staff (in hand or carried) casts it
        if !self.can_cast_spells(b) {
            return out;
        }
        for g in &b.summons {
            let Some(sp) = self.bundle.items.get(g) else { continue };
            let Some(d) = &sp.damage else { continue };
            // cast with the summon spell itself in the spell slot: its own perks count (mana cost, crit, element
            // damage, copied to the companion with copyCombatAttributes), the attack spell's do not
            let swaps_perks = upgradable(sp) || b.ammo.as_deref().and_then(|a| self.bundle.items.get(a)).is_some_and(upgradable);
            let cast_st;
            let st = if swaps_perks {
                cast_st = self.attributes(&Build { ammo: Some(g.clone()), ..b.clone() });
                &cast_st
            } else {
                &st0
            };
            let tpl = sp.projectile.as_ref();
            let tname = tpl.and_then(|t| t.get("name")).and_then(|x| x.as_str()).unwrap_or("");
            // the cap a summon counts against is its template's spawn pool (SpawnLimit.spawnLimitPool, bundle
            // projectile spawnPool: Totem → MaxTotemCount, Skeleton → MaxSkeletonSpellCount); a summon without one (the
            // Shock Wisps) shares no cap and one is kept up per spell
            let pool = tpl.and_then(|t| t.get("spawnPool")).and_then(|x| x.as_str());
            let totem = pool == Some("Totem");
            let kind: &'static str = match pool { Some("Totem") => "totem", Some("Skeleton") => "summon", _ => "companion" };
            let pool_cap = match pool { Some("Totem") => st.get("MaxTotemCount"), Some("Skeleton") => st.get("MaxSkeletonSpellCount"), _ => 1.0 };
            let (cat, scale, count, interval) = if totem {
                let wait = tpl.and_then(|t| t.get("config")).and_then(|c| c.get("wait_min")).and_then(|x| x.as_f64()).unwrap_or(1.0);
                ("Magic", "Intelligence", pool_cap, wait.max(0.1))
            } else {
                // a companion without arsenal data: one attack per 1.525 s (the Hollow Wretch's two slash sequences)
                ("Melee", "Strength", pool_cap, 1.525)
            };
            let level = self.ammo_level(b, g, sp);
            // totems and wisps shoot: their program waits a uniform random time in [wait_min, wait_max] (RandomValueNode
            // remap), passes FilterRandom with chance c, then SpawnEntity of the shot template copying only the parent's
            // Level and Faction. The shot's ImpactConfiguration has a damage distribution, so it uses the enemy formula
            // with the summon's level and DamageMod 1 (the shot has no DamageMod, attribute or crit components): per
            // shot enemy_damage(level, 1) × its shares; your other stats do not change it. The summon's Level is the
            // spell item's, as logged for the Hollow Wretch below (combat-1792717052.csv), and the shot copies it;
            // hotfix 1.0.0.1 pinning the Fire Wisp scroll's damage at level 27 agrees.
            if let Some(ally) = self.bundle.ally_types.get(tname).filter(|a| a.attacks.is_empty() && !a.shots.is_empty()) {
                let cfg = tpl.and_then(|t| t.get("config"));
                let num = |k: &str, d: f64| cfg.and_then(|c| c.get(k)).and_then(|x| x.as_f64()).unwrap_or(d);
                let wait = ((num("wait_min", 0.15) + num("wait_max", 1.0)) / 2.0).max(0.05);
                let chance = num("c", 1.0);
                let eff_sus = self.effective_susceptibility(&target.susceptibility);
                let shot_dmg = enemy_damage(self.bundle, level, 1.0);
                let hit: f64 = ally.shots.iter().map(|h| armored(h.shares.iter().map(|(k, x)| {
                    let short = short_of(k);
                    (short.to_string(), shot_dmg * x * h.attack_mod * eff_sus.get(short).copied().unwrap_or(1.0))
                }).collect())).sum::<f64>() / ally.shots.len() as f64;
                let mana = mana_cost(&st, sp);
                let life = tpl.and_then(|t| t.get("lifetimeS")).and_then(|x| x.as_f64()).unwrap_or(60.0);
                let each = hit * chance / wait;
                out.push(SummonDps {
                    name: sp.name.clone(),
                    kind,
                    count,
                    hit,
                    interval_s: wait / chance.max(1e-9),
                    each_dps: each,
                    total_dps: each * count,
                    mana_per_s: mana * count / life.max(1.0),
                    uptime: 1.0,
                    note: format!("1 shot / {wait:.2}s on average for {life:.0}s"),
                });
                continue;
            }
            // a summoned companion (PlayerAlly faction) hits with the enemy damage formula (0x1403e89e0 →
            // 0x140e5dbe0, faction 11 included): enemy_damage(Level, DamageMod) × each hit's shares × AttackMod. The
            // spell spawns its placedEntity with copyCombatAttributes (spawn 0x140221280 copies CritChance,
            // CritDamageMod, DamageMod, DamageModAgainst, Strength, Intelligence, Dexterity, and Level always), so these
            // are YOUR values: × (1 + 0.05 × Strength) (hit program DamageScaleAttribute Strength); "against" − 1 and on
            // a crit (canCrit unconnected; Melee category) CritDamageMod − 1 join the pool. DamageMod counts once, in the
            // formula: enemy hits on the player in combat-1792649437.csv (a DamageMod 0.5 critter: 4.8 pierce + 1.9 shroud
            // after armor = enemy_damage(3, 0.5) × its shares × the armor factor) leave no room for it in the pool too.
            // Per attack cycle: the mean over its arsenal attacks of the sum of their hits.
            if let Some(ally) = (!totem).then(|| self.bundle.ally_types.get(tname)).flatten().filter(|a| !a.attacks.is_empty()) {
                let dm = st.get("DamageMod");
                let mut against = match target.enemy_class.as_deref() {
                    Some("Melee") => st.get("DamageModAgainst_Melee"),
                    Some("Ranger") => st.get("DamageModAgainst_Ranged"),
                    Some("Mage") => st.get("DamageModAgainst_Mage"),
                    _ => 1.0,
                };
                if target.flying {
                    against += st.get("DamageModAgainst_Flying") - 1.0;
                }
                if let Some(a) = target.faction.as_deref().and_then(faction_against_attribute) {
                    against += st.get(a) - 1.0;
                }
                let soak = |a: &str| if target.soaked { st.get(a) } else { 0.0 };
                let cc = (st.get("CritChance") + st.get("CritChance_Adder_Melee") + soak("CritChance_Adder_Vs_Soaked")).clamp(0.0, st.get("CritChance_Max").max(0.01));
                let pool = 1.0 + (against - 1.0) + cc * (st.get("CritDamageMod") + soak("CritDamageMod_Vs_Soaked") - 1.0);
                // the summon's level is the spell item's (Hollow Wretch I: 17), not yours: combat-1792717052.csv logs a
                // level-19 character's Wretch at level 17, its hits = enemy_damage(17) × (1 + 0.05 × Strength) × shares
                let dmg = enemy_damage(self.bundle, level, dm) * (1.0 + per * st.get("Strength")) * pool;
                let eff_sus = self.effective_susceptibility(&target.susceptibility);
                let sus = |k: &str| -> f64 {
                    let short = if k == "poisonDamage" { "Poison" } else { DAMAGE_TYPES.iter().find(|(key, _, _)| *key == k).map(|(_, _, s)| *s).unwrap_or("") };
                    eff_sus.get(short).copied().unwrap_or(1.0)
                };
                let per_attack: Vec<f64> = ally.attacks.iter()
                    // every hit of the attack lands; the bundle keeps only the hits for the summon's own entity tags
                    // (the Wretch's lunge: 0.66 cut + 0.2 ice, as logged, not the other variant's 0.7 + 0.3)
                    .map(|a| a.hits.iter().map(|h| armored(h.shares.iter().map(|(k, x)| (short_of(k).to_string(), dmg * x * h.attack_mod * sus(k))).collect())).sum::<f64>())
                    .collect();
                // the arsenal picks among its attacks by selectionChance; attacking back to back, damage per second is
                // the chance-weighted mean damage per attack over the chance-weighted mean sequence length
                let wsum: f64 = ally.attacks.iter().map(|a| a.chance).sum::<f64>().max(1e-9);
                let hit = ally.attacks.iter().zip(&per_attack).map(|(a, d)| a.chance * d).sum::<f64>() / wsum;
                let mean_len = ally.attacks.iter().map(|a| a.chance * a.length_s).sum::<f64>() / wsum;
                let interval = target.play.summon_interval(if mean_len > 0.0 { mean_len } else { interval });
                let mana = mana_cost(&st, sp);
                let life = tpl.and_then(|t| t.get("lifetimeS")).and_then(|x| x.as_f64()).unwrap_or(60.0);
                let each = hit / interval;
                out.push(SummonDps {
                    name: sp.name.clone(),
                    kind,
                    count,
                    hit,
                    interval_s: interval,
                    each_dps: each,
                    total_dps: each * count,
                    mana_per_s: mana * count / life.max(1.0),
                    uptime: 1.0,
                    note: format!("{} attacks, one every {interval:.2}s, lasts {life:.0}s", per_attack.len()),
                });
                continue;
            }
            let raw = (weapon_base_damage(self.bundle, b.level.max(level)) * d.speed_mod * d.dmg_mod).trunc();
            let cat_attr = format!("{cat}DamageMod");
            let common = st.get("DamageMod") - 1.0 + st.get(&cat_attr) - 1.0;
            let crit = (st.get("CritChance") + st.get(&format!("CritChance_Adder_{cat}"))).clamp(0.0, 0.75);
            let crit_add = st.get("CritDamageMod") - 1.0;
            let mut parts = vec![];
            for (k, suffix, short) in DAMAGE_TYPES.iter() {
                let share = d.distribution.get(*k).and_then(|x| x.as_f64()).unwrap_or(0.0);
                if share == 0.0 {
                    continue;
                }
                // poison takes no attack-category bonus (FUN_1403d7b80 writes slots 0-6 only; see Engine::hit)
                let category = if *short == "Poison" { st.get(&cat_attr) - 1.0 } else { 0.0 };
                let pool = 1.0 + common - category + if suffix.is_empty() { 0.0 } else { st.get(&format!("{cat_attr}_{suffix}")) - 1.0 };
                let sus = target.susceptibility.get(*short).copied().unwrap_or(1.0);
                // same structure as ApplyDamage: scale multiplies, crit adds to the pool
                parts.push((short.to_string(), raw * share * (1.0 + per * st.get(scale)) * (pool + crit * crit_add) * sus));
            }
            let hit = armored(parts);
            let mana = mana_cost(&st, sp);
            let life = tpl.and_then(|t| t.get("lifetimeS")).and_then(|x| x.as_f64()).unwrap_or(60.0);
            let each = hit / interval;
            out.push(SummonDps {
                name: sp.name.clone(),
                kind,
                count,
                hit,
                interval_s: interval,
                each_dps: each,
                total_dps: each * count,
                mana_per_s: mana * count / life.max(1.0),
                uptime: 1.0,
                note: if totem { format!("1 shot / {interval:.1}s for {life:.0}s") } else { format!("attack every {interval:.2}s back to back, lasts {life:.0}s") },
            });
        }
        // one cap per kind (MaxTotemCount, MaxSkeletonSpellCount), shared by every spell of that kind: past the cap
        // the oldest summon is replaced (1.0 notes), so the cap's places go to the strongest spells first
        for (kind, cap) in [("totem", st0.get("MaxTotemCount")), ("summon", st0.get("MaxSkeletonSpellCount"))] {
            let mut idx: Vec<usize> = (0..out.len()).filter(|&i| out[i].kind == kind).collect();
            // ties (two totems shooting alike) go to the one cheaper in mana, then by name: never by list order
            let per_mana = |i: usize| if out[i].count > 0.0 { out[i].mana_per_s / out[i].count } else { 0.0 };
            idx.sort_by(|&x, &y| out[y].each_dps.partial_cmp(&out[x].each_dps).unwrap_or(std::cmp::Ordering::Equal)
                .then(per_mana(x).partial_cmp(&per_mana(y)).unwrap_or(std::cmp::Ordering::Equal))
                .then(out[x].name.cmp(&out[y].name)));
            let mut left = cap.max(0.0);
            for i in idx {
                let s = &mut out[i];
                let n = s.count.min(left);
                left -= n;
                if n < s.count {
                    s.mana_per_s = if s.count > 0.0 { s.mana_per_s * n / s.count } else { 0.0 };
                    s.note = format!("{}; {n:.0} of the shared cap of {cap:.0}", s.note);
                    s.count = n;
                    s.total_dps = s.each_dps * n;
                }
            }
        }
        // mana budget over the fight (Fight length setting, FIGHT_S when unset): full pool + regeneration; summons are
        // kept up first (the main attack gets what is left, see `total`). The game has no list order, so the mana goes
        // to the summons that deal the most per mana first (ties by name), whatever order they were added in.
        let window = target.play.fight_len();
        let pool = self.sheet(b).values.get("Mana").copied().unwrap_or(100.0);
        let mut left = pool + self.regen_per_s(&st0, "Mana") * window;
        let worth = |s: &SummonDps| if s.mana_per_s > 0.0 { s.total_dps / s.mana_per_s } else { f64::INFINITY };
        out.sort_by(|x, y| worth(y).partial_cmp(&worth(x)).unwrap_or(std::cmp::Ordering::Equal).then(x.name.cmp(&y.name)));
        for s in out.iter_mut() {
            if s.mana_per_s > 0.0 {
                let need = s.mana_per_s * window;
                s.uptime = (left / need).min(1.0);
                left = (left - need * s.uptime).max(0.0);
            }
            s.each_dps *= 1.0 - target.play.summon_idle.clamp(0.0, 1.0);
            s.total_dps *= s.uptime * (1.0 - target.play.summon_idle.clamp(0.0, 1.0));
        }
        out
    }
}

/// An on-hit resource proc from a trigger skill (ForEachHitEvent → HasWeaponCategoryType → RandomBranchNode →
/// ChangeAttribute). The weapon mask is little-endian over keen::WeaponCategoryType (bit 4 bow, 5 staff, 6 wand,
/// 12 throwable, 13 dagger, ...). Crit-only when the tooltip says "Critical Strike" (the wasCrit branch).
#[derive(Debug, Clone, Default)]
pub struct Proc {
    pub skill: String,
    pub resource: String,
    pub amount: f64,
    pub chance: f64,
    pub on_crit: bool,
    pub mask: u32,
    pub per_s: f64,
    /// What triggers it: "hit", "crit" or "kill" ("sneak kill" is listed but not estimated).
    pub event: String,
    /// Resource orbs expected per trigger (orb skills only); `amount` is then per orb.
    pub orbs: f64,
}

pub fn weapon_type_bit(kind: &str) -> u32 {
    const ORDER: [&str; 16] = ["OneHandAxe", "OneHandClub", "OneHandSword", "OneHandWeapon", "RangedBow", "RangedStaff", "RangedWand", "TwoHandAxe",
                               "TwoHandClub", "TwoHandHammer", "GreatSword", "UnarmedFists", "Throwable", "Dagger", "Skill", "Gem"];
    ORDER.iter().position(|k| *k == kind).map(|i| 1u32 << i).unwrap_or(0)
}

impl<'a> Engine<'a> {
    pub fn procs(&self, b: &Build, hits_per_s: f64, crit_chance: f64, kills_per_s: f64, hit_damage: f64, target: &Target) -> Vec<Proc> {
        let ranks = self.effective_ranks(b);
        let sheet = self.sheet(b);
        let wkind = b.weapon.as_ref().and_then(|w| self.bundle.items.get(&w.item)).and_then(|i| i.weapon_category.as_ref())
            .and_then(|c| self.bundle.weapon_categories.get(c)).and_then(|c| c.kind.clone()).unwrap_or_default();
        let wbit = weapon_type_bit(&wkind);
        let mut out = vec![];
        let mut seen = std::collections::HashSet::new();
        for (i, s) in self.bundle.skills.iter().enumerate() {
            let r = ranks[i];
            if r == 0 || !s.program.iter().any(|c| c.node == "ForEachHitEvent") || !seen.insert(s.name.clone()) {
                continue;
            }
            let lv = |a: &str| if a == "Level" { r as f64 } else { 0.0 };
            let num = |v: &serde_json::Value| serde_json::from_value::<ValueSpec>(v.clone()).ok().map(|x| x.eval(&lv));
            let mut mask = u32::MAX;
            let mut chance = 1.0;
            let mut gains = vec![];
            for c in &s.program {
                match c.node.as_str() {
                    "HasWeaponCategoryType" => {
                        for (k, v) in &c.inputs {
                            if k.ends_with("allowedCategories") {
                                if let Some(h) = v.get("value").and_then(|x| x.as_str()) {
                                    let bytes: Vec<u8> = (0..h.len() / 2).filter_map(|j| u8::from_str_radix(&h[2 * j..2 * j + 2], 16).ok()).collect();
                                    mask = bytes.iter().enumerate().fold(0u32, |m, (j, x)| m | (*x as u32) << (8 * j));
                                }
                            }
                        }
                    }
                    "RandomBranchNode" => {
                        for (k, v) in &c.inputs {
                            if k.contains("Chance") {
                                if let Some(x) = num(v) { chance = x; }
                            }
                        }
                    }
                    "ChangeAttribute" => {
                        for (k, v) in &c.inputs {
                            if k.contains("Gain::value") {
                                if let Some(x) = num(v) {
                                    let res = if k.starts_with("Mana") { "Mana" } else if k.starts_with("Stamina") { "Stamina" } else { "Health" };
                                    gains.push((res.to_string(), x));
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            if mask & wbit == 0 {
                continue;
            }
            let desc = s.desc.first().cloned().unwrap_or_default();
            let on_crit = desc.contains("Critical Strike");
            for (res, amount) in gains {
                let per_s = hits_per_s * chance * amount * if on_crit { crit_chance } else { 1.0 };
                out.push(Proc { skill: s.name.clone(), resource: res, amount, chance, on_crit, mask, per_s, event: if on_crit { "crit" } else { "hit" }.into(), orbs: 0.0 });
            }
            // Resource orbs (Bloodletting, Swiftshot Sustenance, Blood Warrior): SpawnEntity of Orb_* templates whose
            // restore share is the template's OrbMultiplier_* (closure, one triple per spawn in program order). A
            // RandomValueNode + IsLess cascade picks one group of spawns: group k has probability t_k - t_(k-1), the
            // last group 1 - t_last; without a cascade every spawn happens.
            let mult: Vec<(String, f64)> = s.closure.iter().filter_map(|(k, v)| k.strip_prefix("tpl OrbMultiplier_").map(|r| (r.replace("Stanima", "Stamina"), *v))).collect();
            if !mult.is_empty() {
                let thresholds: Vec<f64> = s.program.iter().filter(|c| c.node == "IsLess")
                    .filter_map(|c| c.inputs.iter().find(|(k, _)| k.starts_with("ChanceValue")).and_then(|(_, v)| num(v))).collect();
                let spawns = s.program.iter().filter(|c| c.node == "SpawnEntity").count().min(mult.len());
                let groups = thresholds.len() + 1;
                let mut per_res: Vec<(String, f64, f64)> = vec![];
                for (k, (res, m)) in mult.iter().take(spawns).enumerate() {
                    let p = if thresholds.is_empty() || spawns % groups != 0 {
                        1.0
                    } else {
                        let g = k / (spawns / groups);
                        let lo = if g == 0 { 0.0 } else { thresholds[g - 1] };
                        let hi = if g < thresholds.len() { thresholds[g] } else { 1.0 };
                        (hi - lo).max(0.0)
                    };
                    match per_res.iter_mut().find(|x| &x.0 == res) {
                        Some(x) => x.1 += p,
                        None => per_res.push((res.clone(), p, *m)),
                    }
                }
                let event = if desc.contains("Sneak Attack") { "sneak kill" } else if desc.contains("Defeating") { "kill" } else if on_crit { "crit" } else { "hit" };
                let rate = match event {
                    "kill" => kills_per_s,
                    "crit" => hits_per_s * crit_chance,
                    "hit" => hits_per_s,
                    _ => 0.0,
                };
                for (res, orbs, m) in per_res {
                    let amount = m * sheet.values.get(res.as_str()).copied().unwrap_or(0.0);
                    let per_s = rate * chance * orbs * amount * target.orb_pickup;
                    out.push(Proc { skill: s.name.clone(), resource: res, amount, chance, on_crit, mask, per_s, event: event.into(), orbs });
                }
            }
        }
        out.extend(self.perk_procs(b, wbit, hits_per_s, crit_chance, kills_per_s, hit_damage));
        out.extend(self.skill_resource_procs(b, &wkind, hits_per_s, crit_chance, kills_per_s));
        out
    }

    /// Skill resource triggers that restore a share of a maximum or a flat amount (Battle Heal, Unity, Soul Leech):
    /// ForEachHitEvent (wasCrit → per crit, wasKillingBlow → per kill, else per hit) → [HasWeaponCategoryType mask |
    /// HasAttackCategory Melee/Ranged/Magic] → [RandomBranchNode chance] → [FetchAttribute <X>_Max × MulValues
    /// percentage] → ChangeAttribute(attributeRef Health/Mana/Stamina). Triggers on damage you receive are left out.
    pub fn skill_resource_procs(&self, b: &Build, wkind: &str, hits_per_s: f64, crit_chance: f64, kills_per_s: f64) -> Vec<Proc> {
        let ranks = self.effective_ranks(b);
        let sheet = self.sheet(b);
        let wbit = weapon_type_bit(wkind);
        let mut out = vec![];
        let mut seen = std::collections::HashSet::new();
        for (i, s) in self.bundle.skills.iter().enumerate() {
            let r = ranks[i];
            if r == 0 || !seen.insert(s.name.clone()) {
                continue;
            }
            let Some(hit) = s.program.iter().find(|c| c.node == "ForEachHitEvent") else { continue };
            let desc = s.desc.last().cloned().unwrap_or_default();
            if desc.contains("receive") || s.program.iter().any(|c| c.node == "ChangeAttribute" && c.inputs.keys().any(|k| k.contains("Gain::value"))) {
                continue;
            }
            let lv = |a: &str| if a == "Level" { r as f64 } else { 0.0 };
            let num = |v: &serde_json::Value| serde_json::from_value::<ValueSpec>(v.clone()).ok().map(|x| x.eval(&lv));
            let trigger = if hit.inputs.keys().any(|k| k.ends_with("wasKillingBlow")) { "kill" } else if hit.inputs.keys().any(|k| k.ends_with("wasCrit")) { "crit" } else { "hit" };
            let (mut mask, mut cat, mut chance, mut pct, mut maxof, mut flat, mut res) = (u32::MAX, None::<&str>, 1.0, None, None::<String>, None, None::<String>);
            for c in &s.program {
                match c.node.as_str() {
                    "HasWeaponCategoryType" => {
                        for (k, v) in &c.inputs {
                            if k.ends_with("allowedCategories") {
                                if let Some(h) = v.get("value").and_then(|x| x.as_str()) {
                                    let bytes: Vec<u8> = (0..h.len() / 2).filter_map(|j| u8::from_str_radix(&h[2 * j..2 * j + 2], 16).ok()).collect();
                                    mask = bytes.iter().enumerate().fold(0u32, |m, (j, x)| m | (*x as u32) << (8 * j));
                                }
                            }
                        }
                    }
                    "HasAttackCategory" => {
                        for (k, v) in &c.inputs {
                            for name in ["Melee", "Ranged", "Magic"] {
                                if k.starts_with(&format!("{name}::")) {
                                    cat = Some(name);
                                }
                            }
                            if k.ends_with("::contains") && cat.is_none() {
                                cat = match v.get("value").and_then(|x| x.as_f64()).map(|x| x as u32) {
                                    Some(1) => Some("Melee"),
                                    Some(2) => Some("Ranged"),
                                    Some(4) => Some("Magic"),
                                    _ => None,
                                };
                            }
                        }
                    }
                    "RandomBranchNode" => {
                        for (k, v) in &c.inputs {
                            if k.contains("Chance") {
                                if let Some(x) = num(v) {
                                    chance = x;
                                }
                            }
                        }
                    }
                    "FetchAttribute" => {
                        for v in c.inputs.values() {
                            if let Some(a) = v.get("value").and_then(|x| x.as_str()).filter(|a| a.ends_with("_Max")) {
                                maxof = Some(a.trim_end_matches("_Max").to_string());
                            }
                        }
                    }
                    "MulValues" => {
                        for (k, v) in &c.inputs {
                            if v.get("format").and_then(|f| f.as_str()) == Some("Percentage") && !k.contains("FetchAttribute") {
                                pct = num(v);
                            }
                        }
                    }
                    "ChangeAttribute" => {
                        for (k, v) in &c.inputs {
                            if k.ends_with("attributeRef") {
                                res = v.get("value").and_then(|x| x.as_str()).map(|x| x.to_string());
                            } else if !k.ends_with("valueStorage") && !k.ends_with("operation") && !k.ends_with("result") && !k.contains("ntity") {
                                if v.get("kind").and_then(|x| x.as_str()).map(|k| k == "scaled" || k == "const").unwrap_or(false) {
                                    if let Some(x) = num(v).filter(|x| *x != 0.0) {
                                        flat = Some(x);
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            let Some(res) = res.filter(|x| matches!(x.as_str(), "Health" | "Mana" | "Stamina")) else { continue };
            if mask & wbit == 0 || cat.map(|c| c != attack_category(wkind)).unwrap_or(false) {
                continue;
            }
            let amount = match (pct, &maxof, flat) {
                (Some(p), Some(m), _) => p * sheet.values.get(m.as_str()).copied().unwrap_or(0.0),
                (_, _, Some(x)) => x,
                _ => continue,
            };
            let rate = match trigger {
                "crit" => hits_per_s * crit_chance,
                "kill" => kills_per_s,
                _ => hits_per_s,
            };
            out.push(Proc { skill: s.name.clone(), resource: res, amount, chance, on_crit: trigger == "crit", mask, per_s: rate * chance * amount, event: trigger.into(), orbs: 0.0 });
        }
        out
    }

    /// Resource gains from the active perks of equipped items (Health/Mana/Stamina Leech, on-kill gains), decoded from
    /// each perk's impact program: ForEachHitEvent → [HasWeaponCategoryType] → HasFaction(all enemies) →
    /// [RandomBranchNode] → ChangeAttribute(Health|Mana|Stamina) by a Sint32 value, or by MulValues(percentage) of the
    /// hit's damage ("…_OnDamage_Dealt_Percentage_…"). The trigger comes from the perk's debug name: OnKill → kills;
    /// OnSkillshot and OnDamage_Received are listed with 0/s (scenario not modelled).
    pub fn perk_procs(&self, b: &Build, wbit: u32, hits_per_s: f64, crit_chance: f64, kills_per_s: f64, hit_damage: f64) -> Vec<Proc> {
        let ranks: Vec<u32> = self.effective_ranks(b);
        let mut out = vec![];
        for e in &b.perk_items(self.bundle) {
            let Some(item) = self.bundle.items.get(&e.item) else { continue };
            let rarity = e.rarity.clone().or(item.rarity.clone()).unwrap_or_else(|| "Legendary".into());
            for pid in item.perks.iter().take(perk_count(e, &rarity)) {
                let Some(p) = self.bundle.perks.get(pid) else { continue };
                let Some(sp) = perk_proc_spec(p) else { continue };
                if sp.mask & wbit == 0 {
                    continue;
                }
                let rate = match sp.event.as_str() {
                    "kill" => kills_per_s,
                    "hit" => hits_per_s,
                    _ => 0.0,
                };
                let amount = sp.flat.unwrap_or_else(|| sp.pct.unwrap_or(0.0) * hit_damage);
                let _ = (&ranks, crit_chance);
                out.push(Proc { skill: format!("{} ({})", p.name, item.name), resource: sp.resource, amount, chance: sp.chance, on_crit: false, mask: sp.mask, per_s: rate * sp.chance * amount, event: sp.event, orbs: 0.0 });
            }
        }
        out
    }
}

/// A gear perk's resource trigger decoded from its impact program (see `Engine::perk_procs`).
#[derive(Debug, Clone)]
pub struct PerkProcSpec {
    pub resource: String,
    pub flat: Option<f64>,
    pub pct: Option<f64>,
    pub chance: f64,
    pub mask: u32,
    pub event: String,
}

pub fn perk_proc_spec(p: &ens_data::Perk) -> Option<PerkProcSpec> {
    let prog = p.program.as_ref()?;
    if !prog.iter().any(|c| c.node == "ForEachHitEvent" || c.node == "ForEachHitEventInStream") {
        return None;
    }
    let lv = |_: &str| 1.0;
    let num = |v: &serde_json::Value| serde_json::from_value::<ValueSpec>(v.clone()).ok().map(|x| x.eval(&lv));
    let (mut mask, mut chance, mut flat, mut pct, mut res) = (u32::MAX, 1.0, None, None, None::<String>);
    for c in prog {
        match c.node.as_str() {
            "HasWeaponCategoryType" => {
                for (k, v) in &c.inputs {
                    if k.ends_with("allowedCategories") {
                        if let Some(h) = v.get("value").and_then(|x| x.as_str()) {
                            let bytes: Vec<u8> = (0..h.len() / 2).filter_map(|j| u8::from_str_radix(&h[2 * j..2 * j + 2], 16).ok()).collect();
                            mask = bytes.iter().enumerate().fold(0u32, |m, (j, x)| m | (*x as u32) << (8 * j));
                        }
                    }
                }
            }
            "RandomBranchNode" => {
                for (k, v) in &c.inputs {
                    if k.contains("Chance") {
                        if let Some(x) = num(v) {
                            chance = x;
                        }
                    }
                }
            }
            "MulValues" => {
                for (k, v) in &c.inputs {
                    if k.starts_with("Percentage") {
                        pct = num(v);
                    }
                }
            }
            "ChangeAttribute" => {
                for (k, v) in &c.inputs {
                    if k.contains("Sint32ValueNode") || k.starts_with("Gain") {
                        flat = num(v);
                    }
                    if k.starts_with("Attribute") {
                        res = v.get("value").and_then(|x| x.as_str()).map(|x| x.to_string());
                    }
                }
            }
            _ => {}
        }
    }
    let debug = p.debug.clone().unwrap_or_default();
    // Headseeker-style perks gain stamina without naming the attribute; their debug name says which
    let res = res.or_else(|| ["Stamina", "Mana", "Health"].iter().find(|r| debug.contains(*r)).map(|r| r.to_string()));
    let resource = res.filter(|r| matches!(r.as_str(), "Health" | "Mana" | "Stamina"))?;
    if flat.is_none() && pct.is_none() {
        return None;
    }
    let event = if debug.contains("OnKill") { "kill" } else if debug.contains("Skillshot") { "skill shot" } else if debug.contains("Received") { "hit taken" } else { "hit" };
    Some(PerkProcSpec { resource, flat, pct, chance, mask, event: event.into() })
}

/// The template keys of the 19 DamageSusceptibility values (kfc/enemy_types.py) and their attribute names.
pub const SUSCEPTIBILITY_KEYS: [(&str, &str); 15] = [
    ("Physical", "DamageSusceptibility_Physical"), ("Cut", "DamageSusceptibility_Cut"), ("Pierce", "DamageSusceptibility_Pierce"),
    ("Blunt", "DamageSusceptibility_Blunt"), ("Poison", "DamageSusceptibility_Poison"), ("Explosion", "DamageSusceptibility_Explosion"),
    ("SkillShot", "DamageSusceptibility_SkillShot"), ("MagBase", "DamageSusceptibility_Magical_Base"), ("MagAdder", "DamageSusceptibility_Magical_Adder"),
    ("MagMult", "DamageSusceptibility_Magical_Multiplier"), ("Fire", "DamageSusceptibility_Fire"), ("Fog", "DamageSusceptibility_Fog"),
    ("Ice", "DamageSusceptibility_Ice"), ("Lightning", "DamageSusceptibility_Lightning"), ("Wood", "DamageSusceptibility_Wood"),
];

pub fn susceptibility_key(attr: &str) -> &str {
    if attr == "DamageSusceptibility_Magical" {
        return "MagBase";
    }
    SUSCEPTIBILITY_KEYS.iter().find(|(_, a)| *a == attr).map(|(k, _)| *k).unwrap_or(attr)
}

/// The factor one damage type is multiplied by (0x1403d84b0): cut, pierce, blunt × DamageSusceptibility_Physical ×
/// their own value; fire, ice, fog, lightning × DamageSusceptibility_Magical (= the game's formula over Magical_Base,
/// _Adder, _Multiplier) × their own value; poison its own value only.
pub fn susceptibility_factor(get: &dyn Fn(&str) -> f64, damage_type: &str) -> f64 {
    let own = get(&format!("DamageSusceptibility_{damage_type}"));
    match damage_type {
        "Cut" | "Pierce" | "Blunt" => get("DamageSusceptibility_Physical") * own,
        "Fire" | "Ice" | "Fog" | "Lightning" => get("DamageSusceptibility_Magical") * own,
        _ => own,
    }
}

impl<'a> Engine<'a> {
    /// A target's per-type damage factors from its raw DamageSusceptibility inputs (template values), evaluated with
    /// the game's attribute formulas and combined as in 0x1403d84b0. An empty map is a target without resistances.
    pub fn effective_susceptibility(&self, raw: &HashMap<String, f64>) -> HashMap<String, f64> {
        let mut st = self.attrs.state();
        for (k, a) in SUSCEPTIBILITY_KEYS {
            if let Some(v) = raw.get(k) {
                st.apply(a, Op::Add, v - self.attrs.base(a), "target");
            }
        }
        ["Cut", "Pierce", "Blunt", "Fire", "Ice", "Fog", "Lightning", "Poison"].iter()
            .map(|t| (t.to_string(), susceptibility_factor(&|a| st.get(a), t))).collect()
    }
}

/// Skills whose damage is a fixed amount per point of an attribute, from their spawned damage templates' programs
/// (kfc/dump_template_prog.py): name, trigger ("crit" on a staff or wand hit, or "aura" per second), damage per
/// Intelligence (per rank for the aura), damage type, targets (one, or everyone in range), faction filter.
const SKILL_DAMAGE: &[(&str, &str, f64, &str, &str, Option<&str>)] = &[
    // Skill_ChainHit_DealDamageSphere_ClosestEnemy: MulValues(BaseDamge 5, Intelligence) → ApplyDamage on one random
    // enemy (FilterRandom count 1) within 15 m; trigger ForEachHitEvent wasCrit, allowedCategories 0x0060 (staff, wand)
    ("Chain Hit", "crit", 5.0, "Lightning", "one", None),
    // Skill_MassDestruction_DealDamageSphere_AllEnemy: MulValues(BaseDamge 2, Intelligence) → ApplyDamage on every
    // enemy within 15 m (ForEachEntityWithFactionInRange 15)
    ("Mass Destruction", "crit", 2.0, "Lightning", "all", None),
    // Radiant Aura: MulValues(BaseDamage = rank, Intelligence) → ApplyDamage to Fogger-faction enemies within 2 × rank
    // + 0.1 m (EntityInRangeWithFaction mask 0x0004), about once per second (buff lifetime 1.1 s), canCrit 0
    ("Radiant Aura", "aura", 1.0, "Fire", "all", Some("Fogger")),
];

impl<'a> Engine<'a> {
    /// DPS of the SKILL_DAMAGE skills the build owns. Each hit goes through ApplyDamage with the Skill weapon category
    /// (no category mod) and attack category Other: base × (1 + (DamageMod − 1) + target "against" mods − 1) × the
    /// target's susceptibility to the type, then the target's armor. Returns (skill, DPS, explanation, comes from the
    /// main attack's crits).
    pub fn skill_damage(&self, b: &Build, target: &Target, hits_per_s: f64, crit_chance: f64) -> Vec<(String, f64, String, bool)> {
        let ranks = self.effective_ranks(b);
        let st = self.attributes(b);
        let wkind = b.weapon.as_ref().and_then(|w| self.bundle.items.get(&w.item)).and_then(|i| i.weapon_category.as_ref())
            .and_then(|c| self.bundle.weapon_categories.get(c)).and_then(|c| c.kind.clone()).unwrap_or_default();
        let int = st.get("Intelligence");
        let mut against = match target.enemy_class.as_deref() {
            Some("Melee") => st.get("DamageModAgainst_Melee"),
            Some("Ranger") => st.get("DamageModAgainst_Ranged"),
            Some("Mage") => st.get("DamageModAgainst_Mage"),
            _ => 1.0,
        };
        if target.flying {
            against += st.get("DamageModAgainst_Flying") - 1.0;
        }
        if let Some(a) = target.faction.as_deref().and_then(faction_against_attribute) {
            against += st.get(a) - 1.0;
        }
        let pool = 1.0 + (st.get("DamageMod") - 1.0) + (against - 1.0);
        let sus = self.effective_susceptibility(&target.susceptibility);
        let blow = self.bundle.balance("defaultArmorBlowthrough").unwrap_or(0.2);
        let n = target.pack_size.max(1.0);
        let mut out = vec![];
        for (name, trigger, per, dtype, targets, faction) in SKILL_DAMAGE {
            let r = self.bundle.skills.iter().enumerate().filter(|(_, s)| s.name == *name).map(|(i, _)| ranks[i]).max().unwrap_or(0);
            if r == 0 {
                continue;
            }
            if let Some(f) = faction {
                if target.faction.as_deref() != Some(*f) {
                    continue;
                }
            }
            let base = if *trigger == "aura" { *per * r as f64 * int } else { *per * int };
            // canCrit (ApplyDamage pin 20, [rbx+0x50]): an unconnected pin means the hit can crit (0x1403d62ee–
            // 0x1403d631f); Chain Hit and Mass Destruction leave it unconnected, Radiant Aura sets it to false. Their
            // attack category is Skill (mask 16), so only CritChance (+ the soaked adder) applies in 0x1403de4c0.
            let crit_term = if *trigger == "aura" {
                0.0
            } else {
                let soak = |a: &str| if target.soaked { st.get(a) } else { 0.0 };
                let cc = (st.get("CritChance") + soak("CritChance_Adder_Vs_Soaked")).clamp(0.0, st.get("CritChance_Max").max(0.01));
                cc * (st.get("CritDamageMod") + soak("CritDamageMod_Vs_Soaked") - 1.0)
            };
            let raw = base * (pool + crit_term) * sus.get(*dtype).copied().unwrap_or(1.0);
            let hit = apply_armor(raw, &[(dtype.to_string(), raw)], target, blow);
            let (rate, from_attack, how) = match *trigger {
                "crit" if matches!(wkind.as_str(), "RangedStaff" | "RangedWand") => (hits_per_s * crit_chance, true, format!("{hit:.0} {dtype} per crit")),
                // the aura reaches enemies in melee range (ASSUMED: you fight in melee with a melee weapon)
                "aura" if attack_category(&wkind) == "Melee" => (1.0, false, format!("{hit:.0} {dtype} per second to each Fell enemy in range")),
                _ => continue,
            };
            let count = if *targets == "all" { n } else { 1.0 };
            out.push((name.to_string(), hit * count * rate, how, from_attack));
        }
        out
    }
}

impl<'a> Engine<'a> {
    /// Seconds a ranged shot spends charging (InRangeCombatPreparing): the charge part of `attack_interval`.
    pub fn charge_time(&self, b: &Build, weapon: &Equip, ammo: Option<&str>) -> f64 {
        let st = self.attributes(b);
        let Some(w) = self.bundle.items.get(&weapon.item) else { return 0.0 };
        let kind = w.weapon_category.as_ref().and_then(|c| self.bundle.weapon_categories.get(c)).and_then(|c| c.kind.clone()).unwrap_or_default();
        let cfg = |i: &Item, k: &str| i.configs.get(k).and_then(|v| v.as_f64()).unwrap_or(0.0);
        let ammo_item = if kind == "RangedWand" { None } else { ammo.and_then(|a| self.bundle.items.get(a)) };
        let charge = cfg(w, "1469785687").max(ammo_item.map(|a| cfg(a, "1469785687")).unwrap_or(0.0));
        let m = match kind.as_str() {
            "RangedBow" => st.get("RangedCombat_Arrow_ChargeDurationMod"),
            "RangedStaff" => st.get("RangedCombat_Spell_ChargeDurationMod"),
            "RangedWand" => 1.0,
            _ => return 0.0,
        };
        charge * if m > 0.0 { m } else { 1.0 }
    }

    /// Your pool for a gem hit sourced from you (GetRoot / rootEntity): DamageMod + "against" − 1 (+ a category mod).
    fn own_pool(&self, st: &AttrState, target: &Target, cat: Option<&str>) -> f64 {
        let mut against = match target.enemy_class.as_deref() {
            Some("Melee") => st.get("DamageModAgainst_Melee"),
            Some("Ranger") => st.get("DamageModAgainst_Ranged"),
            Some("Mage") => st.get("DamageModAgainst_Mage"),
            _ => 1.0,
        };
        if target.flying {
            against += st.get("DamageModAgainst_Flying") - 1.0;
        }
        if let Some(a) = target.faction.as_deref().and_then(faction_against_attribute) {
            against += st.get(a) - 1.0;
        }
        st.get("DamageMod") + against - 1.0 + cat.map(|c| st.get(&format!("{c}DamageMod")) - 1.0).unwrap_or(0.0)
    }

    /// Gems that react to how you play or to charging (see each arm), plus Storm's Eye and Hyperspecialization.
    pub fn gem_extra(&self, b: &Build, target: &Target, main: &DpsBreakdown, kills_per_s: f64) -> Vec<(String, f64, String)> {
        let mut out = vec![];
        let Some(ge) = &b.gem else { return out };
        let Some(gem) = self.bundle.gems.get(&ge.item) else { return out };
        let Some(w) = &b.weapon else { return out };
        let level = ge.level.unwrap_or_else(|| w.level.unwrap_or(b.level)) as f64;
        let val = |id: &str| gem_value(self.bundle, gem, id, level);
        let st = self.attributes(b);
        let sus = self.effective_susceptibility(&target.susceptibility);
        let blow = self.bundle.balance("defaultArmorBlowthrough").unwrap_or(0.2);
        let hits = main.hits_per_attack() * main.attacks_per_s;
        let shots = main.attacks_per_s;
        let dt = 1.0 / hits.max(1e-9);
        let play = &target.play;
        let land = |dtype: &str, raw: f64| {
            let r = raw * sus.get(dtype).copied().unwrap_or(1.0);
            apply_armor(r, &[(dtype.to_string(), r)], target, blow)
        };
        let n = target.pack_size.max(1.0);
        let rr = if target.pack_radius > 0.0 { target.pack_radius } else { 4.5 };
        let in_radius = |r: f64| 1.0 + (n - 1.0) * (r / rr).powi(2).min(1.0);
        let soak = |a: &str| if target.soaked { st.get(a) } else { 0.0 };
        let crit_term = |adder: &str| {
            let cc = (st.get("CritChance") + if adder.is_empty() { 0.0 } else { st.get(adder) } + soak("CritChance_Adder_Vs_Soaked")).clamp(0.0, st.get("CritChance_Max").max(0.01));
            cc * (st.get("CritDamageMod") + soak("CritDamageMod_Vs_Soaked") - 1.0)
        };
        match gem.debug.as_str() {
            // Swiftness: sprinting primes (IsSprinting → primer → "Swiftness Ready"); the next own hit starts Swiftness
            // (ResetTime, config 3202798761 = 5 s) and every own hit while it is up re-applies it and adds config
            // 1580527771 as Cut, sourced from the weapon entity (no DamageMod component: ×1, no crit)
            "T2_Gem_GemOfSwiftness" if play.sprint_start && dt < val("3202798761").unwrap_or(5.0) => {
                let a = val("1580527771").unwrap_or(0.0);
                out.push(("Gem of Swiftness".into(), land("Cut", a) * hits, format!("+{a:.0} Cut per hit after sprinting in")));
            }
            // Agility: an evade (IsInState Dodging) applies the gem's buff (5 s); the next own hit removes it, restores
            // config 1569572621 stamina and adds config 1580527771 as Cut from you (rootEntity: your pool, canCrit 0)
            "T3_Gem_GemOfAgility" if play.evades_per_min > 0.0 => {
                let a = val("1580527771").unwrap_or(0.0);
                let rate = (play.evades_per_min / 60.0).min(hits);
                out.push(("Gem of Agility".into(), land("Cut", a * self.own_pool(&st, target, None)) * rate, format!("+{a:.0} Cut on the first hit after each evade")));
            }
            // Aerobatics: while not Grounded and attacking (InSnappingLerp / InRangeCombatShooting) it fires a volley,
            // then waits config 3793439602 (1 s): 1 homing projectile plus one per threshold k = 2…8 with
            // not (config 1355396659 < k), IsLess 0x1403d9240 being strict; each deals config 1580527771 Pierce from you (GetRoot: your pool, can crit)
            "T6_Gem_GemOfAerobatics_UnderFog" if play.air_share > 0.0 => {
                let a = val("1580527771").unwrap_or(0.0);
                let count = val("1355396659").unwrap_or(1.0);
                let projectiles = 1.0 + (2..=8).filter(|k| (*k as f64) < count + 1.0).count() as f64;
                let volleys = shots.min(1.0 / val("3793439602").unwrap_or(1.0).max(0.05)) * play.air_share.min(1.0);
                let each = land("Pierce", a * (self.own_pool(&st, target, None) + crit_term("")));
                out.push(("Gem of Aerobatics".into(), each * projectiles * volleys, format!("{projectiles:.0} homing shards of {a:.0} Pierce per airborne volley")));
            }
            // Meteoric Impact: landing after HangGliding spawns Explosion_Player_WeaponGem_MeteoricDescent (4 m, config
            // 2821950968 Fire, source the explosion: ×1, no crit) and applies "Meteoric Impact" for config 3384989681 s,
            // during which each own hit adds config 1580527771 Fire from the weapon entity (×1). Its fire trail applies
            // Burning whose AttackMod is left at the program's 0.0 (no damage).
            "T5_Gem_GemOfMeteoricDescent" if play.glides_per_min > 0.0 => {
                let rate = play.glides_per_min / 60.0;
                let blast = land("Fire", val("2821950968").unwrap_or(0.0)) * in_radius(4.0);
                let dur = val("3384989681").unwrap_or(5.0);
                let buff = land("Fire", val("1580527771").unwrap_or(0.0)) * hits * dur.min(60.0 / play.glides_per_min);
                out.push(("Gem of Meteoric Impact".into(), rate * (blast + buff), format!("blast + {dur:.1}s of bonus Fire per landing")));
            }
            // Focused Light: charging a ranged attack (InRangeCombatPreparing) or blocking at a locked enemy runs the primer
            // (f53b54c0…), which raises the enemy's mark one tier every 1.5 s; your next hit consumes it for config
            // 1580527771 Fire × the tier's AttackMod (1.0 / 2.25 / 4.5), from the weapon entity (×1, no crit)
            "T4_Gem_GemOfFocusedLight" => {
                let c = self.charge_time(b, w, b.ammo.as_deref());
                let tier = ((c / 1.5).floor() as usize).min(3);
                if tier > 0 {
                    let am = [0.0, val("1865448887").unwrap_or(1.0), val("3102738761").unwrap_or(2.25), val("232384132").unwrap_or(4.5)][tier];
                    let a = val("1580527771").unwrap_or(0.0);
                    out.push(("Gem of Focused Light".into(), land("Fire", a * am) * shots, format!("tier {tier} mark per {c:.1}s charge: +{:.0} Fire per shot", a * am)));
                }
            }
            // Cold Composure: 1.6 s of charging (or blocking) grants Shielded by Ice; shooting (InRangeCombatShooting) while
            // shielded removes it and spawns Explosion_Player_GemStone_IceArmorBreak (2.5 m): config 1580527771 Ice from
            // you (GetRoot) with attack category Ranged + AoE (your pool with RangedDamageMod, can crit, no scaling)
            "T5_Gem_GemOfColdComposure" if self.charge_time(b, w, b.ammo.as_deref()) >= 1.6 => {
                let a = val("1580527771").unwrap_or(0.0);
                let each = land("Ice", a * (self.own_pool(&st, target, Some("Ranged")) + crit_term("CritChance_Adder_Ranged"))) * in_radius(2.5);
                out.push(("Gem of Cold Composure".into(), each * shots, format!("{a:.0} Ice blast (2.5 m) on each shot after a 1.6 s charge")));
            }
            // Retaliation: a blocked hit rolls config 3873308466 for the Retaliation buff (3 s); your next own hit removes
            // it and adds config 1580527771 Blunt from you (rootEntity: your pool, canCrit 0)
            "T2_Gem_GemOfRetaliation" if play.blocks_per_min > 0.0 => {
                let a = val("1580527771").unwrap_or(0.0);
                let rate = (play.blocks_per_min / 60.0 * val("3873308466").unwrap_or(0.0)).min(hits);
                out.push(("Gem of Retaliation".into(), land("Blunt", a * self.own_pool(&st, target, None)) * rate, format!("+{a:.0} Blunt after {:.0}% of blocks", val("3873308466").unwrap_or(0.0) * 100.0)));
            }
            // Storm's Eye: your own hits (not Skill/Gem, not consecutive) mark enemies (config 3202798761 = 5 s). The mark's
            // program (8cf3e3b1…) answers any non-Skill hit on its enemy while that enemy has no cooldown: after WaitTime
            // 0.1 s it bolts another alive marked enemy within the mark's range (10 m), preferring one without cooldown,
            // for config 1580527771 Fog × AttackMod 1.0 from the mark entity (×1, canCrit 0), then gives the hit enemy
            // the cooldown (config 802324674 = 1 s) and refreshes its mark. Bolts are Gem hits, which the mark does not
            // skip, so one trigger runs through every marked enemy once; the next of your hits after 0.1 s + cooldown
            // starts the next round. Needs attacks that mark several enemies.
            "T3_Gem_GemOfConduction" if main.aoe_targets >= 2.0 && shots > 0.0 => {
                let a = val("1580527771").unwrap_or(0.0);
                let marked = main.aoe_targets.min(in_radius(10.0));
                let gap = 1.0 / shots;
                let round = gap * ((0.1 + val("802324674").unwrap_or(1.0)) / gap - 1e-9).ceil().max(1.0);
                out.push(("Gem of Storm's Eye".into(), land("Fog", a) * marked / round, format!("{marked:.1} marked enemies bolt once each every {round:.2}s ({a:.0} Fog)")));
            }
            // Hyperspecialization: when Focus is full and the special fires, the buff (config 3202798761 = 3 s) gives
            // +20 Focus (config 1236200858 in its program) per killing blow; more Focus means more specials
            "T7_Gem_GemOfHyperSpecialization" if kills_per_s > 0.0 && main.special_per_s > 0.0 && main.focus_cost > 0.0 => {
                let refund = (kills_per_s * val("3202798761").unwrap_or(3.0) * 20.0).min(main.focus_cost * 0.99);
                let extra = main.special_dps * (main.focus_cost / (main.focus_cost - refund) - 1.0);
                out.push(("Gem of Hyperspecialization".into(), extra, format!("{refund:.1} Focus back per special")));
            }
            _ => {}
        }
        out
    }

    /// Damage-over-time from the weapon's gem, against `target`. A gem's program (ItemInfo.equipment.program) reacts
    /// to your hits; its debuff's own program ticks ApplyDamage with the debuffed enemy as both source and target
    /// (pins 1 and 2), so each tick gets the enemy's DamageMod in the pool, cannot crit (canCrit 0), and then the
    /// enemy's susceptibility and armor. Gem values scale with the gem level (WeaponPowerCreep).
    /// - Gem of Bleeding: your crits with Cut damage (not Skill/Gem weapon categories, enemy targets) apply Bleeding
    ///   (ResetTime) for config 1073225515 s; it ticks config 1580527771 × AttackMod (0.5) every WaitTime (0.5 s).
    /// - Gem of Ignition: your hits with Fire damage apply Burning (Replace) for config 1073225515 s unless the 5 s
    ///   cooldown buff (config 802324674) is up; it ticks config 1580527771 × AttackMod (0.3334) every 0.333 s.
    pub fn gem_damage(&self, b: &Build, target: &Target, hits_per_s: f64, hit: &HitBreakdown, kills_per_s: f64) -> Vec<(String, f64, String)> {
        let mut out = vec![];
        let Some(ge) = &b.gem else { return out };
        let Some(gem) = self.bundle.gems.get(&ge.item) else { return out };
        let level = ge.level.unwrap_or_else(|| b.weapon.as_ref().and_then(|w| w.level).unwrap_or(b.level)) as f64;
        let val = |id: &str| gem_value(self.bundle, gem, id, level);
        let has = |t: &str| hit.per_type.iter().any(|(k, v)| k == t && *v > 0.0);
        // the hit kind the build can actually make (Engine::hit_kind_problem: no sneak or merciless with a bow, staff or
        // wand or without the skill, no backstab with area hits), and the share of hits of that kind
        let kind = target.hit_kind.as_deref().filter(|k| self.hit_kind_problem(b, k).is_none());
        let kind_share = target.hit_share.unwrap_or(1.0).clamp(0.0, 1.0);
        let victim_dm = target.enemy_type.as_ref().and_then(|n| self.bundle.enemy_types.get(n)).map(|e| e.damage_mod).unwrap_or(1.0);
        let sus = self.effective_susceptibility(&target.susceptibility);
        let blow = self.bundle.balance("defaultArmorBlowthrough").unwrap_or(0.2);
        let tick_dps = |dtype: &str, per_s: f64| {
            let raw = per_s * victim_dm * sus.get(dtype).copied().unwrap_or(1.0);
            apply_armor(raw, &[(dtype.to_string(), raw)], target, blow)
        };
        let dt = 1.0 / hits_per_s.max(1e-9);
        match gem.debug.as_str() {
            "T1_Gem_GemOfBleeding" if has("Cut") => {
                let amount = val("1580527771").unwrap_or(0.0);
                let dur = val("1073225515").unwrap_or(6.0);
                let per_s = amount * val("2236524794").unwrap_or(1.0) / val("3793439602").unwrap_or(1.0).max(0.01);
                let u = buff_uptime_events(hit.crit_chance, dt, dur, 1.0);
                out.push(("Gem of Bleeding".into(), tick_dps("Cut", per_s) * u, format!("{per_s:.0} Cut/s while bleeding, up {:.0}% ({dur:.1}s per crit)", u * 100.0)));
            }
            "T1_Gem_GemOfIgnition" if has("Fire") => {
                let amount = val("1580527771").unwrap_or(0.0);
                let dur = val("1073225515").unwrap_or(3.0);
                let cd = val("802324674").unwrap_or(5.0);
                let per_s = amount * val("2236524794").unwrap_or(1.0) / val("2411711896").unwrap_or(0.333).max(0.01);
                // a trigger starts the cooldown; the next fire hit after it re-applies (Replace restarts the burn)
                let cycle = (cd / dt).ceil().max(1.0) * dt;
                let u = (dur / cycle).min(1.0);
                out.push(("Gem of Ignition".into(), tick_dps("Fire", per_s) * u, format!("{per_s:.0} Fire/s while burning, up {:.0}% ({dur:.1}s every {cycle:.1}s)", u * 100.0)));
            }
            // Gem of Toxic Betrayal: backstab hits (wasBackstab; not Skill/Gem categories; enemy, alive) apply its Poisoned
            // debuff (ResetTime) for config 1073225515 s; that debuff (0a7343d4…) ticks the setup (config 1580527771,
            // Poison) at AttackMod 1.0 every 1.0 s and, unlike the player poison, also hurts enemies
            "T2_Gem_GemOfToxicReaction" if kind == Some("Backstab") && kind_share > 0.0 => {
                let amount = val("1580527771").unwrap_or(0.0);
                let dur = val("1073225515").unwrap_or(6.0);
                let backstabbable = target.enemy_type.as_ref().and_then(|n| self.bundle.enemy_types.get(n)).map(|e| e.backstab).unwrap_or(true);
                if backstabbable {
                    // each hit is a backstab with the share's probability
                    let u = buff_uptime_events(kind_share, dt, dur, 1.0);
                    out.push(("Gem of Toxic Betrayal".into(), tick_dps("Poison", amount) * u, format!("{amount:.0} Poison/s while poisoned, up {:.0}%", u * 100.0)));
                }
            }
            // Gem of Skulls: a sneak or merciless killing blow (wasVisceral + wasKillingBlow) on an enemy spawns, with chance
            // config 3873308466, a Flying Skull (template config 4138173901, level config 519137714 = 1 + gem level,
            // faction PlayerAlly, no spawn limit, no copied combat attributes) that lives for its template's
            // keen::ecs::LifeTime.lifeTime (allyTypes lifetimeS; 30 s for Player_Companion_FlyingSkull_GemOfSkullsPerk)
            "T1_Gem_GemOfSkulls" if kills_per_s > 0.0 && kind_share > 0.0 && matches!(kind, Some("Sneak") | Some("Merciless")) => {
                let tpl = gem.simple.get("4138173901").and_then(|v| v.as_str()).unwrap_or("");
                if let Some(ally) = self.bundle.ally_types.values().find(|a| a.guid.as_deref() == Some(tpl)).filter(|a| !a.attacks.is_empty()) {
                    let ally_level = val("519137714").unwrap_or(level + 1.0).max(1.0) as u32;
                    let dmg = enemy_damage(self.bundle, ally_level, ally.damage_mod);
                    let wsum: f64 = ally.attacks.iter().map(|a| a.chance).sum::<f64>().max(1e-9);
                    // the target's armor reduces each skull hit as it does yours (0x1403d3f90)
                    let blow = self.bundle.balance("defaultArmorBlowthrough").unwrap_or(0.2);
                    let per_attack = ally.attacks.iter().map(|a| a.chance * a.hits.iter().map(|h| armor_reduce(&h.shares.iter().map(|(k, x)| {
                        let short = if k == "poisonDamage" { "Poison" } else { DAMAGE_TYPES.iter().find(|(key, _, _)| *key == k).map(|(_, _, s)| *s).unwrap_or("") };
                        (short.to_string(), dmg * x * h.attack_mod * sus.get(short).copied().unwrap_or(1.0))
                    }).collect::<Vec<_>>(), &|_| 0.0, target.armor_physical, target.armor_magical, blow)).sum::<f64>()).sum::<f64>() / wsum;
                    let len = ally.attacks.iter().map(|a| a.chance * a.length_s).sum::<f64>() / wsum;
                    let chance = val("3873308466").unwrap_or(0.0).clamp(0.0, 1.0);
                    // only the share of killing blows that are sneak or merciless attacks
                    let life = ally.lifetime_s.unwrap_or(0.0);
                    let alive = kills_per_s * kind_share * chance * life;
                    let each = per_attack / target.play.summon_interval(len) * (1.0 - target.play.summon_idle.clamp(0.0, 1.0));
                    out.push(("Gem of Skulls".into(), each * alive, format!("{alive:.2} skulls at level {ally_level} alive on average ({:.0}% per sneak/merciless kill, {life:.0} s each)", chance * 100.0)));
                }
            }
            // Gem of Fracturing: a killing blow with Blunt damage on an enemy, with chance config 3873308466, spawns a homing
            // helper, then up to three more each with chance config 3726022082 (0.5): 1 + 0.5 + 0.25 + 0.125 projectiles.
            // Each flies to a random other enemy in range (needs a group) and deals config 1580527771 Blunt at AttackMod
            // 1.0 with you as source (GetRoot): your DamageMod, "against" mods and crits apply, no attribute scale.
            "T2_Gem_GemOfFracturing" if kills_per_s > 0.0 && target.pack_size > 1.0 && has("Blunt") => {
                let st = self.attributes(b);
                let amount = val("1580527771").unwrap_or(0.0);
                let chance = val("3873308466").unwrap_or(0.0).clamp(0.0, 1.0);
                let more = val("3726022082").unwrap_or(0.5).clamp(0.0, 1.0);
                let shards = 1.0 + more + more * more + more * more * more;
                let mut against = match target.enemy_class.as_deref() {
                    Some("Melee") => st.get("DamageModAgainst_Melee"),
                    Some("Ranger") => st.get("DamageModAgainst_Ranged"),
                    Some("Mage") => st.get("DamageModAgainst_Mage"),
                    _ => 1.0,
                };
                if target.flying {
                    against += st.get("DamageModAgainst_Flying") - 1.0;
                }
                if let Some(a) = target.faction.as_deref().and_then(faction_against_attribute) {
                    against += st.get(a) - 1.0;
                }
                let soak = |a: &str| if target.soaked { st.get(a) } else { 0.0 };
                let cc = (st.get("CritChance") + soak("CritChance_Adder_Vs_Soaked")).clamp(0.0, st.get("CritChance_Max").max(0.01));
                let pool = st.get("DamageMod") + (against - 1.0) + cc * (st.get("CritDamageMod") + soak("CritDamageMod_Vs_Soaked") - 1.0);
                let raw = amount * pool * sus.get("Blunt").copied().unwrap_or(1.0);
                let each = apply_armor(raw, &[("Blunt".to_string(), raw)], target, blow);
                let rate = kills_per_s * chance * shards;
                out.push(("Gem of Fracturing".into(), each * rate, format!("{shards:.2} shards of {amount:.0} Blunt per proc, {:.0}% per blunt kill", chance * 100.0)));
            }
            // Gem of Puncturing (T1_Gem_GemOfRepetition): your Pierce hits (not Skill/Gem categories) on an enemy apply
            // Punctured (Replace) for config 1073225515 s carrying the setup config 1580527771 (Pierce). Punctured's own
            // program (973441731…) answers every later Pierce hit on that enemy (not Skill/Gem categories, so not its own
            // bonus) with ApplyDamage(setup) from the hit's attacker (you: your DamageMod and "against" mods), canCrit 0,
            // AttackMod unconnected (×1). Hitting faster than the debuff lasts, every Pierce hit gets the bonus.
            "T1_Gem_GemOfRepetition" if has("Pierce") => {
                let amount = val("1580527771").unwrap_or(0.0);
                let dur = val("1073225515").unwrap_or(3.0);
                if dt < dur {
                    let st = self.attributes(b);
                    let mut against = match target.enemy_class.as_deref() {
                        Some("Melee") => st.get("DamageModAgainst_Melee"),
                        Some("Ranger") => st.get("DamageModAgainst_Ranged"),
                        Some("Mage") => st.get("DamageModAgainst_Mage"),
                        _ => 1.0,
                    };
                    if target.flying {
                        against += st.get("DamageModAgainst_Flying") - 1.0;
                    }
                    if let Some(a) = target.faction.as_deref().and_then(faction_against_attribute) {
                        against += st.get(a) - 1.0;
                    }
                    let raw = amount * (st.get("DamageMod") + against - 1.0) * sus.get("Pierce").copied().unwrap_or(1.0);
                    let each = apply_armor(raw, &[("Pierce".to_string(), raw)], target, blow);
                    out.push(("Gem of Puncturing".into(), each * hits_per_s, format!("+{amount:.0} Pierce on every pierce hit while punctured ({dur:.1}s)")));
                }
            }
            // Gem of Bloodlust: a killing blow on an enemy spawns a health orb with chance 0.05 (config 3726022082), or
            // with config 3873308466 while you have Bloodlust (the AddValues sum is discarded); picking an orb up applies
            // Bloodlust (Replace, 10 s), and while it is up each of your hits (not Skill/Gem categories) adds the setup
            // config 1580527771 (Cut) with the weapon entity as source (no DamageMod component → 1.0, no crit).
            // Uptime: an on-period lasts until 10 s pass without a pickup, (e^(λb·10) − 1)/λb, an off-period 1/λu.
            "T5_Gem_GemOfBloodlust" if kills_per_s > 0.0 && target.orb_pickup > 0.0 => {
                let amount = val("1580527771").unwrap_or(0.0);
                let lu = kills_per_s * val("3726022082").unwrap_or(0.05) * target.orb_pickup;
                let lb = kills_per_s * val("3873308466").unwrap_or(0.0).clamp(0.0, 1.0) * target.orb_pickup;
                let life = 10.0;
                let on = if lb > 0.0 { ((lb * life).exp() - 1.0) / lb } else { life };
                let u = if lu > 0.0 { on / (on + 1.0 / lu) } else { 0.0 };
                let raw = amount * sus.get("Cut").copied().unwrap_or(1.0);
                let each = apply_armor(raw, &[("Cut".to_string(), raw)], target, blow);
                out.push(("Gem of Bloodlust".into(), each * hits_per_s * u, format!("+{amount:.0} Cut per hit while Bloodlust is up ({:.0}% of the time)", u * 100.0)));
            }
            // Gem of Currents: each of your hits (not Skill/Gem categories) on a Soaked enemy rolls config 3873308466 and
            // adds the setup config 1580527771 as Lightning ("Shock") with you as source (your DamageMod and "against"
            // mods, canCrit 0). Being Wet or Soaked yourself is what applies Soaked to enemies (config 1073225515 s), so
            // the planner's "target soaked" choice covers it.
            "T6_Gem_GemOfCurrents_OverFog" if target.soaked => {
                let st = self.attributes(b);
                let amount = val("1580527771").unwrap_or(0.0);
                let chance = val("3873308466").unwrap_or(0.0).clamp(0.0, 1.0);
                let mut against = match target.enemy_class.as_deref() {
                    Some("Melee") => st.get("DamageModAgainst_Melee"),
                    Some("Ranger") => st.get("DamageModAgainst_Ranged"),
                    Some("Mage") => st.get("DamageModAgainst_Mage"),
                    _ => 1.0,
                };
                if target.flying {
                    against += st.get("DamageModAgainst_Flying") - 1.0;
                }
                if let Some(a) = target.faction.as_deref().and_then(faction_against_attribute) {
                    against += st.get(a) - 1.0;
                }
                let raw = amount * (st.get("DamageMod") + against - 1.0) * sus.get("Lightning").copied().unwrap_or(1.0);
                let each = apply_armor(raw, &[("Lightning".to_string(), raw)], target, blow);
                out.push(("Gem of Currents".into(), each * hits_per_s * chance, format!("{:.0}% per hit on a soaked enemy for {amount:.0} Lightning", chance * 100.0)));
            }
            // Gem of Corruption: each counted hit (not within 0.2 s of the last on that enemy, ClearConsecutiveHits) moves
            // the enemy's mark up a tier T1→T5 (Replace restarts the tier's program, which re-applies a 3 s marker
            // buff); T5 is ResetTime, so further hits only refresh it. A tier's program checks the marker every 0.1 s
            // and, once it has lapsed, explodes: the setup (config 1580527771, Fog) × the tier's AttackMod (1.0 + 0.5
            // per tier) on every enemy within the tier's range (1 m + 0.5 m per tier), source the buff entity (no
            // DamageMod component → 1.0, no crit). Hitting steadily, that is one T5 explosion (×3.0, 3 m) per enemy,
            // 3 s after its 5th counted hit, if it lives that long.
            "T4_Gem_GemOfCorruption" if kills_per_s > 0.0 => {
                let amount = val("1580527771").unwrap_or(0.0);
                let gap = dt.max(0.2);
                let ttk = 1.0 / kills_per_s;
                if ttk >= 4.0 * gap + 3.0 {
                    let n = target.pack_size.max(1.0);
                    let rr = if target.pack_radius > 0.0 { target.pack_radius } else { 4.5 };
                    let hit_n = 1.0 + (n - 1.0) * (3.0 / rr).powi(2).min(1.0);
                    let raw = amount * 3.0 * sus.get("Fog").copied().unwrap_or(1.0);
                    let each = apply_armor(raw, &[("Fog".to_string(), raw)], target, blow);
                    out.push(("Gem of Corruption".into(), each * hit_n * kills_per_s, format!("one {:.0} Fog blast (×3, 3 m, {hit_n:.1} enemies) per enemy", amount * 3.0)));
                }
            }
            // Necromancy gems: a kill (IsInState Dead) spawns, with chance config 3873308466, the companion template of
            // config 4138173901 at level config 519137714 (1 + gem level), faction PlayerAlly, copying no combat
            // attributes. Its SpawnLimit pool caps how many live (0x140336270: Lesser 4, Necromancy 2, Greater 1;
            // the newest replaces the oldest) and they stay while you fight, so with any kills the pool fills up.
            "T3_Gem_GemOfLesserNecromancy" | "T5_Gem_GemOfNecromancy" | "T6_Gem_GemOfGreaterNecromancy" if kills_per_s > 0.0 => {
                let cap = match gem.debug.as_str() { "T3_Gem_GemOfLesserNecromancy" => 4.0, "T5_Gem_GemOfNecromancy" => 2.0, _ => 1.0 };
                let tpl = gem.simple.get("4138173901").and_then(|v| v.as_str()).unwrap_or("");
                if let Some(ally) = self.bundle.ally_types.values().find(|a| a.guid.as_deref() == Some(tpl)).filter(|a| !a.attacks.is_empty()) {
                    let ally_level = val("519137714").unwrap_or(level + 1.0).max(1.0) as u32;
                    let per = self.bundle.balance("damageScalePerAttributePoint").unwrap_or(0.05);
                    let dmg = enemy_damage(self.bundle, ally_level, ally.damage_mod) * (1.0 + per * ally.strength);
                    let wsum: f64 = ally.attacks.iter().map(|a| a.chance).sum::<f64>().max(1e-9);
                    // the target's armor reduces each skull hit as it does yours (0x1403d3f90)
                    let blow = self.bundle.balance("defaultArmorBlowthrough").unwrap_or(0.2);
                    let per_attack = ally.attacks.iter().map(|a| a.chance * a.hits.iter().map(|h| armor_reduce(&h.shares.iter().map(|(k, x)| {
                        let short = if k == "poisonDamage" { "Poison" } else { DAMAGE_TYPES.iter().find(|(key, _, _)| *key == k).map(|(_, _, s)| *s).unwrap_or("") };
                        (short.to_string(), dmg * x * h.attack_mod * sus.get(short).copied().unwrap_or(1.0))
                    }).collect::<Vec<_>>(), &|_| 0.0, target.armor_physical, target.armor_magical, blow)).sum::<f64>()).sum::<f64>() / wsum;
                    let len = ally.attacks.iter().map(|a| a.chance * a.length_s).sum::<f64>() / wsum;
                    let chance = val("3873308466").unwrap_or(0.0).clamp(0.0, 1.0);
                    let each = per_attack / target.play.summon_interval(len) * (1.0 - target.play.summon_idle.clamp(0.0, 1.0));
                    let name = self.bundle.items.get(&ge.item).map(|i| i.name.clone()).unwrap_or_else(|| gem.debug.clone());
                    // only your own killing blows spawn (IsSameEntity(GetRoot, rootEntity); a summon's hits carry the
                    // summon as rootEntity) and none with a Skill or Gem weapon category (HasWeaponCategoryType 0xc000,
                    // BRF to the spawn): kills_per_s is your weapon's
                    let life = if target.play.summon_life_s > 0.0 { target.play.summon_life_s } else { SUMMON_LIFE_S };
                    let fight = if target.play.fight_s > 0.0 { target.play.fight_s } else { FIGHT_S };
                    let alive = expected_alive(cap, kills_per_s * chance, life, fight);
                    out.push((name, each * alive, format!("{alive:.1} of {cap:.0} companions alive on average at level {ally_level} ({:.0}% per kill, {life:.0} s each, {fight:.0} s fights)", chance * 100.0)));
                }
            }
            _ => {}
        }
        out
    }
}

/// A skill that buffs the player when a hit, crit or kill happens, decoded from its impact program and closure:
/// ForEachHitEvent (a wasCrit input = crit trigger, wasKillingBlow = kill trigger) → [HasWeaponCategory(requiredCategory)
/// | HasWeaponCategoryType(mask) | HasAttackCategory(Melee/Ranged/Magic)] → ApplyBuff on the player (GetRoot or the hit's
/// rootEntity), with the buff's lifetime and attribute changes from the closure.
#[derive(Debug, Clone)]
pub struct TriggeredBuff {
    pub skill: String,
    pub trigger: String,
    pub weapon_category: Option<String>,
    pub weapon_mask: u32,
    pub attack_category: Option<String>,
    pub lifetime: f64,
    pub effects: Vec<(String, f64)>,
    /// The buff lands on the hit target (a debuff such as Expose Weakness) instead of the player.
    pub on_target: bool,
    /// Share of the lifetime left at which a new trigger re-applies a running buff (BuffType.applyType via
    /// 0x1403af240): ResetTime / Replace 1.0 (always), ReplaceAtEnd buffReapplyPercentage 0.33, Invalid 0 (only
    /// after it ran out).
    pub reapply_at: f64,
}

/// Time share a triggered buff is up when each trigger event (spaced `dt` seconds apart) fires with probability `p`
/// and a running buff of `lifetime` s accepts a re-trigger only in its last `reapply_at` share (a renewal cycle starts
/// at the trigger that applies it).
pub fn buff_uptime_events(p: f64, dt: f64, lifetime: f64, reapply_at: f64) -> f64 {
    if p <= 0.0 || dt <= 0.0 || lifetime <= 0.0 {
        return 0.0;
    }
    let p = p.min(1.0);
    let ka = (((1.0 - reapply_at) * lifetime / dt).ceil() as i64).max(1);
    let kb = (lifetime / dt).ceil() as i64 - 1; // last event while the buff still runs
    let (mut on, mut cycle, mut stay) = (0.0, 0.0, 1.0);
    let mut k = ka;
    while k <= kb {
        let first = stay * p;
        on += first * k as f64 * dt;
        cycle += first * k as f64 * dt;
        stay *= 1.0 - p;
        k += 1;
    }
    // no re-trigger in the window: up for the full lifetime, then wait for the next trigger event
    let next = (kb.max(ka - 1) + 1) as f64 + (1.0 - p) / p;
    on += stay * lifetime;
    cycle += stay * next * dt;
    if cycle > 0.0 { (on / cycle).min(1.0) } else { 0.0 }
}

/// The same for triggers that arrive at random (kills): exponential gaps at `rate` per second.
pub fn buff_uptime_random(rate: f64, lifetime: f64, reapply_at: f64) -> f64 {
    if rate <= 0.0 || lifetime <= 0.0 {
        return 0.0;
    }
    let w = reapply_at * lifetime;
    let on = (1.0 - reapply_at) * lifetime + (1.0 - (-rate * w).exp()) / rate;
    on / (on + (-rate * w).exp() / rate)
}

impl<'a> Engine<'a> {
    pub fn triggered_buffs(&self, b: &Build) -> Vec<TriggeredBuff> {
        let ranks = self.effective_ranks(b);
        let mut out = vec![];
        let mut seen = std::collections::HashSet::new();
        for (i, s) in self.bundle.skills.iter().enumerate() {
            if ranks[i] == 0 || !seen.insert(s.name.clone()) {
                continue;
            }
            if let Some(mut tb) = decode_triggered_buff(s) {
                // the applied buff's applyType (closure key "buff <buffTypeId> lifetime s")
                let id = s.closure.iter().find_map(|(k, _)| k.strip_prefix("buff ").and_then(|r| r.strip_suffix(" lifetime s")).map(|x| x.to_string()));
                tb.reapply_at = match id.and_then(|i| self.bundle.buffs.get(&i)).and_then(|bf| bf.apply_type.as_deref()) {
                    Some("ReplaceAtEnd") => self.bundle.balance("buffReapplyPercentage").unwrap_or(0.33),
                    Some("Invalid") => 0.0,
                    _ => 1.0,
                };
                out.push(tb);
            }
        }
        out
    }
}

pub fn decode_triggered_buff(s: &ens_data::SkillNode) -> Option<TriggeredBuff> {
    let hit = s.program.iter().find(|c| c.node == "ForEachHitEvent")?;
    let apply = s.program.iter().find(|c| c.node == "ApplyBuff")?;
    // the buff must land on the player, not on the hit target or allies in range
    let on_target = apply.inputs.keys().any(|k| k == "ForEachHitEvent::targetRootEntity");
    let on_self = apply.inputs.keys().any(|k| k == "GetRoot::entityId" || k == "ForEachHitEvent::rootEntity");
    if !(on_self || on_target) || apply.inputs.keys().any(|k| k.contains("sourceEntity") || k.contains("ForEachPlayerInRange")) {
        return None;
    }
    // wasBlockBroken (Overpower) is a scenario the planner does not estimate: rate 0
    let trigger = if hit.inputs.keys().any(|k| k.ends_with("wasKillingBlow")) { "kill" } else if hit.inputs.keys().any(|k| k.ends_with("wasCrit")) { "crit" }
        else if hit.inputs.keys().any(|k| k.ends_with("wasBlockBroken")) { "block break" } else { "hit" };
    let mut weapon_category = None;
    let mut weapon_mask = u32::MAX;
    let mut attack_category = None;
    for c in &s.program {
        match c.node.as_str() {
            "HasWeaponCategory" => {
                for (k, v) in &c.inputs {
                    if k.ends_with("requiredCategory") {
                        weapon_category = v.get("value").and_then(|x| x.as_str()).map(|g| g.trim_start_matches("guid:").to_string());
                    }
                }
            }
            "HasWeaponCategoryType" => {
                for (k, v) in &c.inputs {
                    if k.ends_with("allowedCategories") {
                        if let Some(h) = v.get("value").and_then(|x| x.as_str()) {
                            let bytes: Vec<u8> = (0..h.len() / 2).filter_map(|j| u8::from_str_radix(&h[2 * j..2 * j + 2], 16).ok()).collect();
                            weapon_mask = bytes.iter().enumerate().fold(0u32, |m, (j, x)| m | (*x as u32) << (8 * j));
                        }
                    }
                }
            }
            "HasAttackCategory" => {
                for k in c.inputs.keys() {
                    for cat in ["Melee", "Ranged", "Magic"] {
                        if k.starts_with(&format!("{cat}::")) {
                            attack_category = Some(cat.to_string());
                        }
                    }
                }
            }
            _ => {}
        }
    }
    // closure: "buff <id> lifetime s" then that buff's "buff <Attribute>" values (config echoes end in "::value")
    let mut lifetime = None;
    let mut effects = vec![];
    for (k, v) in &s.closure {
        let Some(rest) = k.strip_prefix("buff ") else { continue };
        if rest.ends_with("lifetime s") {
            if lifetime.is_some() {
                break;
            }
            lifetime = Some(*v);
        } else if lifetime.is_some() && !rest.contains("::") {
            effects.push((rest.to_string(), *v));
        }
    }
    let lifetime = lifetime?;
    if effects.is_empty() {
        return None;
    }
    if on_target && !effects.iter().all(|(a, _)| a.starts_with("DamageSusceptibility")) {
        return None;
    }
    Some(TriggeredBuff { skill: s.name.clone(), trigger: trigger.into(), weapon_category, weapon_mask, attack_category, lifetime, effects, on_target, reapply_at: 1.0 })
}

/// Everything together: summons first, then the main attack limited by the mana left over a 60 s fight.
#[derive(Debug, Clone, Default)]
pub struct TotalDps {
    pub main: DpsBreakdown,
    pub main_mana_per_s: f64,
    /// Share of the fight the main attack is used: while its mana lasts (1 when the mana-free fallback does more and
    /// has become the main attack, see replaced).
    pub main_uptime: f64,
    /// Share of a 60 s fight the main attack's mana would last.
    pub mana_uptime: f64,
    /// The mana arrow and its DPS, when its mana-free fallback does more and is counted as the main attack instead.
    pub replaced: Option<(String, f64)>,
    /// Share of the fight spent casting summons (each kept up recast once per lifetime at the staff's cast time); the
    /// main attack runs in the rest.
    pub casting_share: f64,
    pub summons: Vec<SummonDps>,
    pub procs: Vec<Proc>,
    /// Mana-free arrow used while out of mana (bows), and its DPS.
    pub fallback: Option<(String, f64)>,
    /// Self-buffs from triggered skills: (skill, uptime, DPS while it is up).
    pub buffs: Vec<(String, f64, f64)>,
    /// Skills the build owns that add nothing to these numbers because what they need is not modeled: (skill, why).
    pub not_counted: Vec<(String, String)>,
    /// Damage from skills that deal their own hits (Chain Hit, Mass Destruction, Radiant Aura): (skill, DPS, how).
    pub skill_damage: Vec<(String, f64, String)>,
    pub total: f64,
}

impl<'a> Engine<'a> {
    pub fn total(&self, b: &Build, target: &Target) -> TotalDps {
        let mut t = TotalDps { main_uptime: 1.0, mana_uptime: 1.0, ..Default::default() };
        t.summons = self.summons(b, target);
        // a wand's 1.0 heavy attack (a piercing shot at AttackMod 2.0 over a 1.97 s animation) is not priced: the numbers
        // use the wand's normal shot, and say so
        if b.weapon.as_ref().and_then(|w| self.bundle.items.get(&w.item)).and_then(|i| i.weapon_category.as_ref())
            .and_then(|c| self.bundle.weapon_categories.get(c)).and_then(|c| c.kind.as_deref()) == Some("RangedWand")
        {
            t.not_counted.push(("Wand heavy attack".into(), "the 1.0 piercing heavy shot (2.0× damage, 1.97 s, through every enemy in its line) is not priced; the numbers use the wand's normal shot".into()));
        }
        // summon spells need a staff to cast them (in hand or carried); without one they add nothing, and say so
        if !b.summons.is_empty() && !self.can_cast_spells(b) {
            for g in &b.summons {
                let n = self.bundle.items.get(g).map(|i| i.name.clone()).unwrap_or_default();
                t.not_counted.push((n, "is a spell: with no staff in hand or carried this build cannot cast it, so it adds 0 DPS".into()));
            }
        }
        let summon_mana: f64 = t.summons.iter().map(|s| s.mana_per_s * s.uptime).sum();
        if let Some(w) = &b.weapon {
            let (secs, _) = self.attack_interval(b, w, b.ammo.as_deref());
            // recasting summons takes your time: each one kept up is cast once per lifetime, taking the staff's cast
            // time for that spell; the main attack runs in what is left
            t.casting_share = self.summon_casting_share(b, &t.summons);
            let free = 1.0 - t.casting_share;
            t.main = self.dps(b, w, b.ammo.as_deref(), target, free / secs.max(0.05));
            // a spell that is not an attack (drains, heals, barriers, roots)
            if let Some(sp) = b.ammo.as_deref().and_then(|a| self.bundle.items.get(a)).filter(|i| i.slot.as_deref() == Some("Spell") && !deals_damage(i)) {
                t.not_counted.push((sp.name.clone(), "deals no damage (it drains, heals, shields or roots), so it adds 0 DPS".into()));
            }
            // a zone spell's debuff (burn, slow) is not priced
            if let Some(z) = b.ammo.as_deref().and_then(|a| self.bundle.items.get(a)).filter(|i| is_zone_spell(i)) {
                t.not_counted.push((format!("{} debuff", z.name), "the zone's burn or slow debuff is applied but not priced".into()));
            }
            let st = self.attributes(b);
            let cost = b.ammo.as_deref().and_then(|a| self.bundle.items.get(a)).map(|i| mana_cost(&st, i)).unwrap_or(0.0);
            t.main_mana_per_s = cost * free / secs.max(0.05);
            // the attack only hits and kills while mana lasts, and its on-hit mana procs feed that mana. The procs grow in
            // step with the hit and kill rates (uptime u times the full rates), so the 60 s budget
            //   u × cost × W = pool + (regen + u × procs_full) × W − summons × W
            // solves directly: u = (pool + regen × W − summons × W) ÷ (W × (cost − procs_full)), capped to [0, 1]
            let full_kills = if target.enemy_health > 0.0 { t.main.total / target.enemy_health } else { 0.0 };
            let full_hits = t.main.hits_per_attack() * t.main.attacks_per_s;
            let full_procs = self.procs(b, full_hits, t.main.hit.crit_chance, full_kills, t.main.hit.after_armor, target);
            if t.main_mana_per_s > 0.0 {
                let procs_full: f64 = full_procs.iter().filter(|p| p.resource == "Mana").map(|p| p.per_s).sum();
                let st = self.attributes(b);
                // the fight's length (Fight length setting): a shorter fight spends a larger share on the full pool
                let window = target.play.fight_len();
                let pool = self.sheet(b).values.get("Mana").copied().unwrap_or(100.0);
                let base = pool + (self.regen_per_s(&st, "Mana") - summon_mana) * window;
                let net = t.main_mana_per_s - procs_full;
                t.main_uptime = if net <= 0.0 { 1.0 } else { (base / (window * net)).clamp(0.0, 1.0) };
                t.mana_uptime = t.main_uptime;
            }
            t.procs = if t.main_uptime < 1.0 {
                self.procs(b, full_hits * t.main_uptime, t.main.hit.crit_chance, full_kills * t.main_uptime, t.main.hit.after_armor, target)
            } else {
                full_procs
            };
        }
        let mut fallback_dps = 0.0;
        // a bow whose arrow costs mana can always shoot its best mana-free arrow instead, so that arrow is looked up
        // whenever the main one costs mana, not only once mana runs out
        if t.main_uptime < 1.0 || t.main_mana_per_s > 0.0 {
            if let Some(w) = &b.weapon {
                let is_bow = self.bundle.items.get(&w.item).and_then(|i| i.weapon_category.as_ref()).and_then(|c| self.bundle.weapon_categories.get(c))
                    .and_then(|c| c.kind.as_deref()) == Some("RangedBow");
                if is_bow {
                    // best mana-free arrow the character can use: highest level <= character level, positive dmgMod
                    // (the one with the highest DPS; ties broken by name so the choice is stable), cached per key.
                    // The key is the whole build and target: pack size, the tree, gear, food and hit kind all change
                    // which arrow wins.
                    let mut sus: Vec<String> = target.susceptibility.iter().map(|(k, v)| format!("{k}={v}")).collect();
                    sus.sort();
                    let mut kb = b.clone();
                    kb.name.clear();
                    kb.order.clear();
                    let kt = Target { susceptibility: HashMap::new(), ..target.clone() };
                    let key = format!("{kb:?}|{kt:?}|{}", sus.join(","));
                    let cached = self.fallback_cache.lock().ok().and_then(|c| c.get(&key).cloned());
                    let pick = match cached {
                        Some(p) => p,
                        None => {
                            let mut best: Option<(f64, String)> = None;
                            let mut cands: Vec<(&String, &ens_data::Item)> = self.bundle.items.iter()
                                .filter(|(_, i)| i.ammo.as_deref() == Some("Arrow") && i.category.as_deref() == Some("Ammunition") && !i.configs.contains_key("2938334926"))
                                .filter(|(_, i)| i.damage.as_ref().map(|d| d.dmg_mod > 0.2).unwrap_or(false) && !i.name.is_empty())
                                .filter(|(_, i)| i.levels.as_ref().map(|l| l.min <= b.level).unwrap_or(false))
                                .collect();
                            cands.sort_by(|x, y| x.1.name.cmp(&y.1.name).then(x.0.cmp(y.0)));
                            for (g, _) in cands {
                                let fb = Build { ammo: Some(g.clone()), ..b.clone() };
                                let (secs, _) = self.attack_interval(&fb, w, Some(g));
                                let d = self.dps(&fb, w, Some(g), target, 1.0 / secs.max(0.05)).total;
                                if best.as_ref().map(|(x, _)| d > *x + 1e-9).unwrap_or(true) {
                                    best = Some((d, g.clone()));
                                }
                            }
                            let p = best.map(|x| x.1);
                            if let Ok(mut c) = self.fallback_cache.lock() {
                                c.insert(key, p.clone());
                            }
                            p
                        }
                    };
                    if let Some(g) = pick {
                        // the fallback arrow in the ammo slot: the mana arrow's own perks (Eternal arrows) do not count
                        let fb = Build { ammo: Some(g.clone()), ..b.clone() };
                        let (secs, _) = self.attack_interval(&fb, w, Some(&g));
                        let fd = self.dps(&fb, w, Some(&g), target, 1.0 / secs.max(0.05));
                        let d = fd.total;
                        fallback_dps = d;
                        t.fallback = Some((self.bundle.items.get(&g).map(|i| i.name.clone()).unwrap_or_default(), d));
                        // a mana arrow weaker than the free one is not worth its mana: the free arrow is shot the whole
                        // fight, so more mana can never lower the total
                        // (it becomes the main attack: its hits, Focus, procs and kills are what the build makes)
                        if d > t.main.total + 1e-9 {
                            let fb = t.fallback.as_ref().map(|f| f.0.clone()).unwrap_or_default();
                            let main = b.ammo.as_deref().and_then(|a| self.bundle.items.get(a)).map(|i| i.name.clone()).unwrap_or_default();
                            t.not_counted.push((main.clone(), format!("does less than {fb}, which costs no mana, so {fb} is counted for the whole fight")));
                            t.replaced = Some((main, t.main.total));
                            t.main = fd;
                            t.main_uptime = 1.0;
                            let kills = if target.enemy_health > 0.0 { t.main.total / target.enemy_health } else { 0.0 };
                            t.procs = self.procs(b, t.main.hits_per_attack() * t.main.attacks_per_s, t.main.hit.crit_chance, kills, t.main.hit.after_armor, target);
                        }
                    }
                }
            }
        }
        t.total = t.main.total * t.main_uptime + fallback_dps * (1.0 - t.main_uptime) + t.summons.iter().map(|s| s.total_dps).sum::<f64>();
        // kills per second from what the weapon actually deals: the main attack while mana lasts, then the fallback
        let kill_rate = if target.enemy_health > 0.0 { (t.main.total * t.main_uptime + fallback_dps * (1.0 - t.main_uptime)) / target.enemy_health } else { 0.0 };
        // skills that deal their own damage on crits or as an aura
        if !b.no_triggers {
            if b.weapon.is_some() {
                let hits = t.main.hits_per_attack() * t.main.attacks_per_s;
                for (name, dps, how, from_attack) in self.skill_damage(b, target, hits, t.main.hit.crit_chance) {
                    let d = if from_attack { dps * t.main_uptime } else { dps };
                    t.total += d;
                    t.skill_damage.push((name, d, how));
                }
                // gem effects are scaled by the main attack's uptime below (their hit-based part needs it), so the kill
                // rate they get is the one while the attack is up; times the uptime it is the real kill rate again
                let kills = if t.main_uptime > 0.0 { kill_rate / t.main_uptime } else { 0.0 };
                for (name, dps, how) in self.gem_damage(b, target, hits, &t.main.hit, kills) {
                    let d = dps * t.main_uptime;
                    t.total += d;
                    t.skill_damage.push((name, d, how));
                }
                for (name, dps, how) in self.gem_extra(b, target, &t.main, kills) {
                    let d = dps * t.main_uptime;
                    t.total += d;
                    t.skill_damage.push((name, d, how));
                }
            }
        }
        // triggered self-buffs: uptime from the buff's re-apply rule (buff_uptime_events / buff_uptime_random); the total
        // is the uptime-weighted mix of the build without and with the buff's attribute changes
        if !b.no_triggers {
            if let Some(w) = &b.weapon {
                let item = self.bundle.items.get(&w.item);
                let wcat = item.and_then(|i| i.weapon_category.clone()).unwrap_or_default();
                let wkind = self.bundle.weapon_categories.get(&wcat).and_then(|c| c.kind.clone()).unwrap_or_default();
                let hits = t.main.hits_per_attack() * t.main.attacks_per_s;
                let kills = kill_rate;
                // Relentless (Buff_CritChance_Until_MeleeDamage, AttributeBuff 0.08): a two-handed crit (allowedCategories
                // 0x0780) grants +8% crit chance until your next melee hit, so it is up on the share of hits that
                // follow a crit (buff template: AttributeReferenceConfig → CritChance_Adder_Melee, FloatImpactConfig 0.08)
                let ranks = self.effective_ranks(b);
                // Necromancer: a killing blow with a staff or wand (allowedCategories 0x0060) on an enemy-faction target
                // spawns a Skull Companion with chance 0.05 × skill level (scaled config). The SpawnEntity node copies
                // only your Level and Faction; the skull (Player_Companion_FlyingSkull_NecromancerSkill) lives 60 s with
                // no spawn limit, DamageMod 1 and no crit, and attacks back to back: one tag-gated projectile per 0.8 s
                // attack (0.3 Fire), each hit = enemy_damage(level, 1) × share. Skulls alive on average = spawn rate ×
                // lifetime.
                let mut necro_dps = 0.0;
                let necro = self.bundle.skills.iter().enumerate().filter(|(_, s)| s.name == "Necromancer").map(|(i, _)| ranks[i]).max().unwrap_or(0);
                if necro > 0 && weapon_type_bit(&wkind) & 0x0060 != 0 && kills > 0.0 {
                    if let Some(skull) = self.bundle.ally_types.get("Player_Companion_FlyingSkull_NecromancerSkill").filter(|a| !a.attacks.is_empty()) {
                        let eff_sus = self.effective_susceptibility(&target.susceptibility);
                        let shot = enemy_damage(self.bundle, b.level, skull.damage_mod);
                        let wsum: f64 = skull.attacks.iter().map(|a| a.chance).sum::<f64>().max(1e-9);
                        // the target's armor reduces each skull hit as it does yours (0x1403d3f90)
                        let blow = self.bundle.balance("defaultArmorBlowthrough").unwrap_or(0.2);
                        let per_attack = skull.attacks.iter().map(|a| a.chance * a.hits.iter().map(|h| armor_reduce(&h.shares.iter().map(|(k, x)| {
                            let short = if k == "poisonDamage" { "Poison" } else { DAMAGE_TYPES.iter().find(|(key, _, _)| *key == k).map(|(_, _, s)| *s).unwrap_or("") };
                            (short.to_string(), shot * x * h.attack_mod * eff_sus.get(short).copied().unwrap_or(1.0))
                        }).collect::<Vec<_>>(), &|_| 0.0, target.armor_physical, target.armor_magical, blow)).sum::<f64>()).sum::<f64>() / wsum;
                        let len = skull.attacks.iter().map(|a| a.chance * a.length_s).sum::<f64>() / wsum;
                        // its template's keen::ecs::LifeTime (allyTypes lifetimeS, 60 s); a fight starts with none, so the
                        // count is averaged over the fight as for the necromancy gems (expected_alive; no spawn limit)
                        let life = skull.lifetime_s.unwrap_or(60.0);
                        let fight = if target.play.fight_s > 0.0 { target.play.fight_s } else { FIGHT_S };
                        let alive = expected_alive(f64::INFINITY, kills * 0.05 * necro as f64, life, fight);
                        let each = per_attack / target.play.summon_interval(len) * (1.0 - target.play.summon_idle.clamp(0.0, 1.0));
                        necro_dps = each * alive;
                        t.summons.push(SummonDps {
                            name: "Skull Companion (Necromancer)".into(),
                            kind: "summon",
                            count: alive,
                            hit: per_attack,
                            interval_s: len,
                            each_dps: each,
                            total_dps: each * alive,
                            mana_per_s: 0.0,
                            uptime: 1.0,
                            note: format!("{:.0}% per magic-weapon kill, lives {life:.0}s, {alive:.2} alive on average over {fight:.0} s fights", 5.0 * necro as f64),
                        });
                    }
                }
                // a buff's worth: what it adds to the total without triggers (the same baseline its "with" total is
                // computed on), times its uptime. Mixing (1 − u) × total + u × with would drop the skill and gem damage
                // that `with` leaves out (it is computed with no_triggers), so a buff could lower the total.
                let base_nt = std::cell::OnceCell::new();
                let base_nt = || *base_nt.get_or_init(|| {
                    let mut bb = b.clone();
                    bb.no_triggers = true;
                    self.total(&bb, target).total
                });
                let add_buff = |t: &mut TotalDps, name: &str, u: f64, with: f64| {
                    let gain = with - base_nt();
                    t.buffs.push((name.to_string(), u, t.total + gain));
                    t.total += u * gain;
                };
                let relentless = self.bundle.skills.iter().enumerate().any(|(i, s)| s.name == "Relentless" && ranks[i] > 0);
                if relentless && weapon_type_bit(&wkind) & 0x0780 != 0 {
                    let u = t.main.hit.crit_chance;
                    let mut bb = b.clone();
                    bb.no_triggers = true;
                    bb.base_extra.push(("CritChance_Adder_Melee".into(), 0.08));
                    let with = self.total(&bb, target).total;
                    add_buff(&mut t, "Relentless", u, with);
                }
                for tb in self.triggered_buffs(b) {
                    let ok = tb.weapon_category.as_ref().map(|g| *g == wcat).unwrap_or(true)
                        && tb.weapon_mask & weapon_type_bit(&wkind) != 0
                        && tb.attack_category.as_deref().map(|c| c == attack_category(&wkind)).unwrap_or(true);
                    // a buff that only raises one weapon category's damage (Blast and Cast: a wand crit, then the next
                    // staff attack; Slice And Dice: dagger, then bow) pays only after switching weapons, which is not
                    // modeled: it is listed as not counted instead of as a buff with an uptime
                    let swap = !tb.effects.is_empty() && tb.effects.iter().all(|(a, _)| a.starts_with("WeaponCategoryDamageMod_"));
                    if !ok {
                        if swap {
                            t.not_counted.push((tb.skill.clone(), "needs a switch from the weapon that triggers it to the weapon it buffs; switching weapons is not modeled".into()));
                        }
                        continue;
                    }
                    // uptime from the buff's re-apply rule over the attack cadence: crit and hit triggers come with each
                    // hit event (probability = crit chance or 1), kills at random
                    let dt = 1.0 / hits.max(1e-9);
                    let u = match tb.trigger.as_str() {
                        "crit" => buff_uptime_events(t.main.hit.crit_chance, dt, tb.lifetime, tb.reapply_at),
                        "hit" => buff_uptime_events(1.0, dt, tb.lifetime, tb.reapply_at),
                        "kill" => buff_uptime_random(kills, tb.lifetime, tb.reapply_at),
                        _ => 0.0,
                    };
                    if u <= 0.0 {
                        continue;
                    }
                    let mut bb = b.clone();
                    bb.no_triggers = true;
                    let with = if tb.on_target {
                        // a debuff raises the target's susceptibility inputs (DamageSusceptibility_Physical → "Physical")
                        let mut tt = target.clone();
                        for (a, v) in &tb.effects {
                            let key = susceptibility_key(a);
                            let cur = tt.susceptibility.get(key).copied().unwrap_or_else(|| self.attrs.base(a));
                            tt.susceptibility.insert(key.to_string(), cur + v);
                        }
                        self.total(&bb, &tt).total
                    } else {
                        bb.base_extra.extend(tb.effects.iter().cloned());
                        self.total(&bb, target).total
                    };
                    if swap && (with - base_nt()).abs() < 1e-9 {
                        t.not_counted.push((tb.skill.clone(), "needs a switch from the weapon that triggers it to the weapon it buffs; switching weapons is not modeled".into()));
                        continue;
                    }
                    add_buff(&mut t, &tb.skill, u, with);
                }
                // Gem of Cleansing: your hits (not Skill/Gem categories) on an enemy whose own DamageSusceptibility_Fog is
                // below 1 apply Cleansed (ResetTime, config 1073225515 s), whose effects set that value to 1 (0 → +1,
                // otherwise ÷ itself); mixed by uptime like the other target debuffs
                if let Some(ge) = &b.gem {
                    if let Some(gem) = self.bundle.gems.get(&ge.item).filter(|g| g.debug == "T7_Gem_GemOfCleansing") {
                        let fog = target.susceptibility.get("Fog").copied().unwrap_or(1.0);
                        if fog < 1.0 {
                            let level = ge.level.unwrap_or_else(|| b.weapon.as_ref().and_then(|w| w.level).unwrap_or(b.level)) as f64;
                            let dur = gem_value(self.bundle, gem, "1073225515", level).unwrap_or(3.0);
                            let u = buff_uptime_events(1.0, 1.0 / hits.max(1e-9), dur, 1.0);
                            let mut tt = target.clone();
                            tt.susceptibility.insert("Fog".into(), 1.0);
                            let mut bb = b.clone();
                            bb.no_triggers = true;
                            let with = self.total(&bb, &tt).total;
                            add_buff(&mut t, "Gem of Cleansing", u, with);
                        }
                    }
                }
                // sustain and buff gems (mixed or added like other procs/buffs)
                if let Some(ge) = &b.gem {
                    if let Some(gem) = self.bundle.gems.get(&ge.item) {
                        let level = ge.level.unwrap_or_else(|| b.weapon.as_ref().and_then(|w| w.level).unwrap_or(b.level)) as f64;
                        let val = |id: &str| gem_value(self.bundle, gem, id, level);
                        let pick = target.orb_pickup;
                        let mix = |t: &mut TotalDps, name: &str, u: f64, extra: Vec<(String, f64)>| {
                            if u <= 0.0 {
                                return;
                            }
                            let mut bb = b.clone();
                            bb.no_triggers = true;
                            bb.base_extra.extend(extra);
                            let with = self.total(&bb, target).total;
                            add_buff(t, name, u, with);
                        };
                        let proc = |skill: &str, resource: &str, amount: f64, chance: f64, event: &str, per_s: f64| Proc {
                            skill: skill.into(), resource: resource.into(), amount, chance, on_crit: false, mask: 0, per_s, event: event.into(), orbs: 0.0,
                        };
                        match gem.debug.as_str() {
                            // Lycanthropy: at night (IsDaytime false) a killing blow rolls config 3873308466 for a Lunar Orb;
                            // picking it up gives config 2433146344 Health and 3818255048 Mana and Lunar Howl for config
                            // 3384989681 s: Health/Mana/Stamina max adders (1986484213 / 3817825337 / 893974534) and
                            // MeleeDamageMod + config 2508806332
                            "T6_Gem_GemOfLycanthropy" if target.play.night && kills > 0.0 && pick > 0.0 => {
                                let orbs = kills * val("3873308466").unwrap_or(0.0) * pick;
                                t.procs.push(proc("Gem of Lycanthropy", "Health", val("2433146344").unwrap_or(0.0), val("3873308466").unwrap_or(0.0), "night kill", orbs * val("2433146344").unwrap_or(0.0)));
                                t.procs.push(proc("Gem of Lycanthropy", "Mana", val("3818255048").unwrap_or(0.0), val("3873308466").unwrap_or(0.0), "night kill", orbs * val("3818255048").unwrap_or(0.0)));
                                let u = buff_uptime_random(orbs, val("3384989681").unwrap_or(60.0), 1.0);
                                mix(&mut t, "Gem of Lycanthropy (Lunar Howl)", u, vec![
                                    ("Health_Max_Adder".into(), val("1986484213").unwrap_or(0.0)),
                                    ("Mana_Max_Adder".into(), val("3817825337").unwrap_or(0.0)),
                                    ("Stamina_Max_Adder".into(), val("893974534").unwrap_or(0.0)),
                                    ("MeleeDamageMod".into(), val("2508806332").unwrap_or(0.0)),
                                ]);
                            }
                            // Soul Reaping: hits mark enemies (62 s); a marked enemy's death leaves a phial that heals
                            // config 2433146344 when you break it
                            "T4_Gem_GemOfSoulReaping" if kills > 0.0 && pick > 0.0 => {
                                let a = val("2433146344").unwrap_or(0.0);
                                t.procs.push(proc("Gem of Soul Reaping", "Health", a, 1.0, "kill", kills * pick * a));
                            }
                            // Destiny: one enemy within 30 m at a time is Marked by Destiny (20 s); killing it drops an orb
                            // for your most depleted resource (config 2433146344 Health / 3818255048 Mana / 1569572621
                            // Stamina); the planner counts it as Mana when the attack costs mana, else Health
                            "T7_Gem_GemOfDestiny" if kills > 0.0 && pick > 0.0 => {
                                let rate = kills / target.pack_size.max(1.0) * pick;
                                let (res, id) = if t.main_mana_per_s > 0.0 { ("Mana", "3818255048") } else { ("Health", "2433146344") };
                                let a = val(id).unwrap_or(0.0);
                                t.procs.push(proc("Gem of Destiny", res, a, 1.0, "marked kill", rate * a));
                            }
                            // Battle Mania: with the Elixir buff, each own hit keeps Satisfied Battle Mania (3 s, ResetTime)
                            // up: Health/Mana/Stamina_Recharge + configs 3045947274 / 1655495777 / 2255959795
                            "T4_Gem_GemOfObsession" if target.play.elixir && t.main.hits_per_attack() * t.main.attacks_per_s > 1.0 / 3.0 => {
                                mix(&mut t, "Gem of Battle Mania", 1.0, vec![
                                    ("Health_Recharge".into(), val("3045947274").unwrap_or(0.0)),
                                    ("Mana_Recharge".into(), val("1655495777").unwrap_or(0.0)),
                                    ("Stamina_Recharge".into(), val("2255959795").unwrap_or(0.0)),
                                ]);
                            }
                            _ => {}
                        }
                    }
                }
                // skulls last: the buff mix above compares totals computed without triggers
                t.total += necro_dps;
            }
        }
        t
    }
}

/// FNV-1a of a GUID given as 32 hex chars (skill node id from a GUID reference).
pub fn fnv1a_hex(guid: &str) -> u32 {
    let mut h: u32 = 0x811c9dc5;
    for i in 0..guid.len() / 2 {
        if let Ok(byte) = u8::from_str_radix(&guid[2 * i..2 * i + 2], 16) {
            h = (h ^ byte as u32).wrapping_mul(0x01000193);
        }
    }
    h
}

/// Target for damage calculations.
#[derive(Debug, Clone, Default)]
pub struct Target {
    pub susceptibility: HashMap<String, f64>,
    pub armor_physical: f64,
    pub armor_magical: f64,
    /// The target's keen::FactionType, enemy class (Melee/Ranger/Mage) and whether it flies (selects the
    /// DamageModAgainst_* mods).
    pub faction: Option<String>,
    pub enemy_class: Option<String>,
    pub flying: bool,
    /// The enemy template (bundle enemyTypes key): its DamageMod and attack hits size the hits you take.
    pub enemy_type: Option<String>,
    /// Enemies in the group (1 = single target) and how far they spread from its centre (m).
    pub pack_size: f64,
    pub pack_radius: f64,
    /// Health of one enemy (for on-kill procs; 0 = ignore them) and the share of resource orbs the player
    /// actually picks up (ASSUMED, player behaviour; 0 = ignore orbs).
    pub enemy_health: f64,
    pub orb_pickup: f64,
    /// Damage type of the enemy hits used for survival (a scenario you choose; None = Pierce).
    pub incoming_type: Option<String>,
    /// Scenario: every hit is a sneak attack ("Sneak"), a merciless attack ("Merciless") or lands from behind
    /// ("Backstab"); None = normal hits. A kind the build cannot make (`Engine::hit_kind_problem`) counts as normal.
    pub hit_kind: Option<String>,
    /// Scenario: the share of regular hits that are of `hit_kind` (the rest are normal); None = every hit.
    pub hit_share: Option<f64>,
    /// Scenario: hits land on the target's weak spot (skill shot), and the target is soaked (wet).
    pub weak_spot: bool,
    pub soaked: bool,
    /// How you play: situations some gems react to (sprinting, evading, the air, gliding, blocking, night, elixir).
    pub play: Play,
}

/// Play situations (scenarios you choose; the game rules for each are proven per gem).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Play {
    /// You sprint into each fight (Swiftness primes; hits then keep it up).
    pub sprint_start: bool,
    /// Evades per minute (Agility).
    pub evades_per_min: f64,
    /// Share of your attacks made in the air (Aerobatics).
    pub air_share: f64,
    /// Glide landings per minute (Meteoric Impact).
    pub glides_per_min: f64,
    /// Enemy hits you block per minute (Retaliation).
    pub blocks_per_min: f64,
    /// Fighting at night (Lycanthropy).
    pub night: bool,
    /// An Elixir buff is active (Battle Mania).
    pub elixir: bool,
    /// Share of the time your companions spend not attacking (moving, finding a target); 0 = back to back.
    pub summon_idle: f64,
    /// Seconds between one companion's attacks (summons, gem summons, the necromancer's skull); 0 = SUMMON_ATTACK_S.
    /// A companion never attacks faster than its attack animation lasts.
    pub summon_attack_s: f64,
    /// Seconds a gem companion lasts before it dies or decays (when no newer one replaces it); 0 = SUMMON_LIFE_S.
    pub summon_life_s: f64,
    /// Seconds of one fight; gem companions start each fight from none; 0 = FIGHT_S.
    pub fight_s: f64,
    /// How far a heavy melee attack reaches (m); 0 = HEAVY_REACH_M.
    pub heavy_reach_m: f64,
    /// Share of a zone spell's lifetime an enemy stays inside it; 0 = ZONE_STAY.
    pub zone_stay: f64,
    /// Zones of one spell alive at once; 0 = ZONE_CAP.
    pub zone_cap: f64,
}

/// Seconds between a summoned companion's attacks when the setting is left at 0: measured on a Hollow Wretch that
/// was not being hit (combat-1792717052.csv / combat-1792718413.csv: 2.4 and 2.6 s between swings on one target). Its
/// AI decides when to swing (attack slots, prowling, blocking, staggers when hit), so the animation length (1.5–1.8 s)
/// is only the fastest it can go; under attack or walking between targets the logs show 6–13 s.
pub const SUMMON_ATTACK_S: f64 = 2.6;

/// Scenario default: how far from you a heavy melee attack reaches (m). 1.0 made heavy attacks hit every nearby
/// enemy (1.0 notes, https://www.keengamer.com/articles/guides/enshrouded-1-0-patch-notes-all-new-features-and-changes/),
/// but the attack sequences carry no reach, so this is a setting (Play::heavy_reach_m), not a game value.
pub const HEAVY_REACH_M: f64 = 2.5;

/// Scenario default: the share of a zone's lifetime an enemy stays inside it (Pillar Of Flames, Blizzard, Ball
/// Lightning). The zone hits each enemy inside once per hit interval; how long enemies stand in it is how you fight.
pub const ZONE_STAY: f64 = 0.5;

/// Scenario default: zones of one spell alive at once. Each zone template has a SpawnLimit pool (FirePillarSpellZone,
/// BlizzardSpellZone, RootSpellZone: bundle projectile spawnPool), but the pools' limits are not in the game data, so
/// this is a setting; 1 = a new cast replaces the running zone (never more damage than the game can give).
pub const ZONE_CAP: f64 = 1.0;

/// Seconds a gem companion lasts when the setting is left at 0. The game data gives none: the skeletons carry no
/// lifetime or decay timer (they "decay when out of combat for too long"), so this is the 60 s the planner already uses
/// for the necromancer's skull and the summon spells.
pub const SUMMON_LIFE_S: f64 = 60.0;
/// Seconds of one fight when the setting is left at 0.
pub const FIGHT_S: f64 = 60.0;

/// Expected companions alive when kills spawn them at `rate` per second, each lasts `life` seconds and the newest
/// replaces the oldest once `cap` are out (the necromancy gems): the alive ones are the last `cap` spawns of the past
/// `life` seconds, so the count is min(cap, N) with N Poisson of mean rate × life (Little's law, capped). A fight starts
/// with none, so `fight` > 0 averages the count over the fight's first `fight` seconds.
pub fn expected_alive(cap: f64, rate: f64, life: f64, fight: f64) -> f64 {
    let capped = |mean: f64| {
        if mean <= 0.0 {
            return 0.0;
        }
        // no spawn limit: the count is the mean itself
        if !cap.is_finite() {
            return mean;
        }
        let c = cap.max(0.0).floor() as usize;
        let (mut p, mut below, mut part) = ((-mean).exp(), 0.0, 0.0);
        for n in 0..c {
            below += p;
            part += n as f64 * p;
            p *= mean / (n + 1) as f64;
        }
        part + c as f64 * (1.0 - below).max(0.0)
    };
    if fight <= 0.0 {
        return capped(rate * life);
    }
    let steps = 400;
    (0..steps).map(|i| capped(rate * ((i as f64 + 0.5) / steps as f64 * fight).min(life))).sum::<f64>() / steps as f64
}

impl Play {
    /// Seconds of one fight: the Fight length setting, FIGHT_S when unset. The mana budget and the averaged summon
    /// counts run over it.
    pub fn fight_len(&self) -> f64 {
        if self.fight_s > 0.0 { self.fight_s } else { FIGHT_S }
    }

    /// The pace a companion attacks at, given its attack animation length: the setting (or SUMMON_ATTACK_S), never
    /// faster than the animation.
    pub fn summon_interval(&self, animation_s: f64) -> f64 {
        let set = if self.summon_attack_s > 0.0 { self.summon_attack_s } else { SUMMON_ATTACK_S };
        set.max(animation_s)
    }
}

/// Flat armor (0x1403d3f90): `blow` of each type always lands; the rest is reduced by the shared physical
/// pool (cut, pierce, blunt) or magical pool (fire, ice, fog).
pub fn apply_armor(expected: f64, per_type: &[(String, f64)], t: &Target, blow: f64) -> f64 {
    let total: f64 = per_type.iter().map(|(_, d)| d).sum();
    if total <= 0.0 {
        return 0.0;
    }
    let scale = expected / total;
    let scaled: Vec<(String, f64)> = per_type.iter().map(|(k, d)| (k.clone(), d * scale)).collect();
    armor_reduce(&scaled, &|_| 0.0, t.armor_physical, t.armor_magical, blow)
}

/// The armor step exactly as 0x1403d3f90 does it, for cut, pierce, blunt, fire, ice and fog (other types, such as
/// lightning and poison, pass through unarmored): the reducible part r = d − blow × d; subtract the type's own
/// ArmorPoints_<Type> (r = 0 if the points reach it); then the shared pool ArmorPoints_Physical × Armor_Physical_Mod
/// is consumed by cut → pierce → blunt and ArmorPoints_Magical × Armor_Magical_Mod by fire → ice → fog; the result
/// is r + blow × d. `points` gives the per-type points; the pools come already multiplied by their mods.
pub fn armor_reduce(per_type: &[(String, f64)], points: &dyn Fn(&str) -> f64, phys_pool: f64, mag_pool: f64, blow: f64) -> f64 {
    let (mut phys, mut mag) = (phys_pool.max(0.0), mag_pool.max(0.0));
    let mut out = 0.0;
    for order in [["Cut", "Pierce", "Blunt"], ["Fire", "Ice", "Fog"]] {
        for k in order {
            for (t, d) in per_type.iter().filter(|(t, _)| t == k) {
                let mut r = d - blow * d;
                let p = points(t);
                r = if p < r { r - p } else { 0.0 };
                let pool = if k == "Cut" || k == "Pierce" || k == "Blunt" { &mut phys } else { &mut mag };
                if r > 0.0 {
                    let take = r.min(*pool);
                    r -= take;
                    *pool -= take;
                }
                out += r + blow * d;
            }
        }
    }
    out + per_type.iter().filter(|(t, _)| !matches!(t.as_str(), "Cut" | "Pierce" | "Blunt" | "Fire" | "Ice" | "Fog")).map(|(_, d)| d).sum::<f64>()
}

impl<'a> Engine<'a> {
    /// One enemy hit against this build: the enemy hit of that level (0x140e5df00) of the scenario's damage type,
    /// × the build's DamageSusceptibility_<type>, then the build's armor (armor_reduce with its ArmorPoints_<Type>,
    /// ArmorPoints_Physical × Armor_Physical_Mod and ArmorPoints_Magical × Armor_Magical_Mod). Returns (raw, taken).
    pub fn incoming(&self, b: &Build, enemy_level: u32, damage_type: &str) -> (f64, f64) {
        self.incoming_near(b, enemy_level, damage_type, 1.0)
    }

    /// `incoming` with `enemies_near` enemies around you: skills that buff you while at least N enemies are within
    /// range (Tower, Warden: GetEnemyCountInRange → IsLess(MinEnemies) → ApplyBuff) add their buff's attribute changes.
    pub fn incoming_near(&self, b: &Build, enemy_level: u32, damage_type: &str, enemies_near: f64) -> (f64, f64) {
        let t = Target { incoming_type: Some(damage_type.to_string()), pack_size: enemies_near, ..Default::default() };
        self.incoming_target(b, enemy_level, &t)
    }

    /// The biggest hit one enemy of `target` deals you, before and after your resistances and armor. Enemy damage
    /// (0x1403e89e0 → 0x140e5dbe0): `trunc(|expected player health(L) ÷ hits-to-die(L) × 2.5 × DamageMod|)`, spread
    /// over the attack's damage distribution (its SpawnImpact or projectile ImpactConfiguration), then ApplyDamage:
    /// × AttackMod, × (1 + 0.05 × Strength); the attacker's DamageMod counts once, in the formula. Without an
    /// enemy template (training dummy) the hit is the chosen damage type with DamageMod 1.
    pub fn incoming_target(&self, b: &Build, enemy_level: u32, target: &Target) -> (f64, f64) {
        let enemies_near = target.pack_size.max(1.0);
        let mut b2 = b.clone();
        let ranks = self.effective_ranks(b);
        for (i, s) in self.bundle.skills.iter().enumerate() {
            if ranks[i] == 0 || !s.program.iter().any(|c| c.node == "GetEnemyCountInRange") {
                continue;
            }
            let min = s.program.iter().filter(|c| c.node == "IsLess").flat_map(|c| c.inputs.iter())
                .find(|(k, _)| k.starts_with("MinEnemies")).and_then(|(_, v)| v.get("value").and_then(|x| x.as_f64())).unwrap_or(f64::INFINITY);
            if enemies_near < min {
                continue;
            }
            let mut started = false;
            for (k, v) in &s.closure {
                let Some(rest) = k.strip_prefix("buff ") else { continue };
                if rest.ends_with("lifetime s") {
                    if started {
                        break;
                    }
                    started = true;
                } else if started && !rest.contains("::") {
                    b2.base_extra.push((rest.to_string(), *v));
                }
            }
        }
        let b = &b2;
        let st = self.attributes(b);
        let mut hit = enemy_hit_types(self.bundle, enemy_level, target);
        // Gem of Armoring: kills drop an orb; picking it up gives Armored (config 3384989681 s: Physical and Magical
        // susceptibility × 0). The average hit taken shrinks by the share of time you are Armored.
        if let Some(ge) = &b.gem {
            if let Some(gem) = self.bundle.gems.get(&ge.item).filter(|g| g.debug == "T7_Gem_GemOfArmoring") {
                if target.orb_pickup > 0.0 && target.enemy_health > 0.0 {
                    let kills = self.total(b, target).total / target.enemy_health;
                    let level = ge.level.unwrap_or_else(|| b.weapon.as_ref().and_then(|w| w.level).unwrap_or(b.level)) as f64;
                    let u = buff_uptime_random(kills * target.orb_pickup, gem_value(self.bundle, gem, "3384989681", level).unwrap_or(1.0), 1.0);
                    for (_, d) in hit.iter_mut() {
                        *d *= 1.0 - u;
                    }
                }
            }
        }
        let raw: f64 = hit.iter().map(|(_, d)| d).sum();
        let after: Vec<(String, f64)> = hit.iter().map(|(t, d)| (t.clone(), d * susceptibility_factor(&|a| st.get(a), t))).collect();
        let blow = self.bundle.balance("defaultArmorBlowthrough").unwrap_or(0.2);
        let taken = armor_reduce(&after, &|t| st.get(&format!("ArmorPoints_{t}")),
                                 st.get("ArmorPoints_Physical") * st.get("Armor_Physical_Mod"), st.get("ArmorPoints_Magical") * st.get("Armor_Magical_Mod"), blow);
        (raw, taken)
    }
}

#[cfg(test)]
mod item_stat_tests {
    use super::*;
    use ens_data::default_bundle_path;

    /// Roth's seven equipped items (save ids, 2026-10-21) add what program a620334b resolves for them: Stamina
    /// +24 +9 +5 +6, regeneration +3 +5 +6, Skill Shot +0.13 +0.06.
    #[test]
    fn equipped_items_add_their_stat_configs() {
        let b = Bundle::load(default_bundle_path()).unwrap();
        let guid = |id: u32| b.items.keys().find(|g| ens_data::save::item_id(g) == Some(id)).unwrap().clone();
        let ids = [2422827884, 2778778931, 4065374720, 1505192014, 3996283384, 1652235601, 857219651];
        let e = Engine::new(&b);
        let mut with = Build::new(&b, 13);
        with.gear = ids.iter().map(|&id| Some(Equip { item: guid(id), rarity: None, level: None, perks: Some(0) })).collect();
        let without = Build::new(&b, 13);
        let (a, z) = (e.attributes(&with), e.attributes(&without));
        let d = |k: &str| a.get(k) - z.get(k);
        assert_eq!(d("Stamina_Max_Adder"), 44.0);
        assert_eq!(d("Stamina_Recharge"), 14.0);
        assert!((d("DamageModSkillShot_Base") - 0.19).abs() < 1e-9, "{}", d("DamageModSkillShot_Base"));
        // a carried weapon adds nothing; the same weapon in hand adds its stat (Cyclone Axe: ParryProficiency +0.55)
        let axe = Equip { item: guid(101298280), rarity: None, level: None, perks: Some(0) };
        let mut carried = Build::new(&b, 13);
        carried.carried.push(build::Carried { weapon: axe.clone(), gem: None, ammo: None });
        assert_eq!(e.attributes(&carried).get("ParryProficiency_Base"), z.get("ParryProficiency_Base"));
        let mut held = Build::new(&b, 13);
        held.weapon = Some(axe);
        assert!((e.attributes(&held).get("ParryProficiency_Base") - z.get("ParryProficiency_Base") - 0.55).abs() < 1e-9);
    }

    /// Rested: Stamina 8 × (5 + level / 2, integer) and regeneration 4 × level; level 13 gives the 88 / 52 the
    /// character screen showed beyond items and tree.
    #[test]
    fn rested_scales_with_character_level() {
        let b = Bundle::load(default_bundle_path()).unwrap();
        let e = Engine::new(&b);
        for (level, stamina, regen) in [(13, 88.0, 52.0), (1, 40.0, 4.0), (50, 240.0, 200.0)] {
            let on = Build::new(&b, level);
            let mut off = on.clone();
            off.rested = false;
            let (a, z) = (e.attributes(&on), e.attributes(&off));
            assert_eq!((a.get("Stamina_Max_Adder") - z.get("Stamina_Max_Adder"), a.get("Stamina_Recharge") - z.get("Stamina_Recharge")), (stamina, regen), "level {level}");
        }
        let code = Build::new(&b, 13).to_code();
        assert!(!String::from_utf8(build::unb64(&code).unwrap()).unwrap().contains("rested"), "on by default: not in the code");
        let mut off = Build::new(&b, 13);
        off.rested = false;
        assert!(!Build::from_code(&off.to_code()).unwrap().rested);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ens_data::default_bundle_path;

    fn bundle() -> Bundle {
        Bundle::load(default_bundle_path()).unwrap()
    }

    #[test]
    fn base_damage_matches_code() {
        let b = bundle();
        assert_eq!(weapon_base_damage(&b, 1), 11.0);
        assert_eq!(weapon_base_damage(&b, 50), 85.0);
    }

    #[test]
    fn crit_formula_and_marksman() {
        let b = bundle();
        let eng = Engine::new(&b);
        let mut build = Build::new(&b, 30);
        let st0 = eng.attributes(&build);
        assert!((st0.get("CritChance") - st0.get("CritChance_Base")).abs() < 1e-9);
        build.set_rank_by_name(&b, "Marksman", 3);
        let st = eng.attributes(&build);
        assert!((st.get("WeaponCategoryDamageMod_Bow") - 1.15).abs() < 1e-6, "{}", st.get("WeaponCategoryDamageMod_Bow"));
    }

    /// Hand-checked: Tarred Bow L50 (item 63) + Eternal Frost Arrow III (item level 45: trunc(77 × 1.0 × 0.5) = 38) = 101 raw;
    /// × (1 + 0.05 × 24 Dex: 15 from skills + 9 from Flame level 10) × 1.15 Marksman; ice share 1.0 × pool 1.24
    /// (Pumpkin + Chicken soup RangedDamageMod) + pierce share 0.2 (two "Heavy" pierce perks) × 1.24. The arrow is held
    /// at its base level with no perks unlocked (an upgraded Eternal arrow is level 50 with its own perks).
    #[test]
    fn sample_bow_hit() {
        let b = bundle();
        let eng = Engine::new(&b);
        let mut build = Build::sample_bow(&b);
        let arrow = build.ammo.clone().unwrap();
        build.upgrades.insert(arrow.clone(), build::Equip { item: arrow, rarity: None, level: Some(45), perks: Some(0) });
        let w = build.weapon.clone().unwrap();
        let h = eng.hit(&build, &w, build.ammo.as_deref(), 1.0, &Target::default());
        assert_eq!(h.item_damage, 63.0);
        assert_eq!(h.ammo_damage, 38.0);
        assert!((h.non_crit - 101.0 * 2.2 * 1.15 * 1.24 * 1.2).abs() < 0.05, "{}", h.non_crit);
        let (secs, _) = eng.attack_interval(&build, &w, build.ammo.as_deref());
        assert!((secs - 1.5).abs() < 1e-9);
    }

    #[test]
    fn armor_blowthrough() {
        let t = Target { armor_physical: 1000.0, ..Default::default() };
        let d = apply_armor(100.0, &[("Pierce".into(), 100.0)], &t, 0.2);
        assert!((d - 20.0).abs() < 1e-9);
    }
}

/// Player damage per hit the designers expect at a level (shared by the enemy balancing formulas):
/// `(1 + baseCritChance × critBonus) × trunc(seed × (1 + creep × L)) × (1 + 0.01 L)^4 × epicWeaponBonus`.
pub fn expected_player_hit(bundle: &Bundle, level: u32) -> f64 {
    let g = |k: &str, d: f64| bundle.balance(k).unwrap_or(d);
    let l = level as f64;
    let weapon = (g("genericWeaponSeed", 10.0) * (1.0 + g("powerCreepWeapon", 0.15) * l)).trunc();
    (1.0 + g("baseCritChance", 0.1) * g("critBonus", 0.8)) * weapon * (1.0 + 0.01 * l).powi(4) * g("epicWeaponBonus", 1.6)
}

/// Enemy health at a level (0x140e75670; world setting enemyHealthFactor/bossHealthFactor at default 1.0):
/// `max(1, trunc(expected_player_hit × htk × healthFactor))`, `htk = L / playerLevelMax × (htkEndPve − htkStartPve) + htkStartPve`
/// (5 hits at level 0 rising to 10 at level 100). healthFactor is the enemy template's EnemyScaling.healthScaleFactor.
pub fn enemy_health(bundle: &Bundle, level: u32, health_factor: f64) -> f64 {
    let g = |k: &str, d: f64| bundle.balance(k).unwrap_or(d);
    let l = level as f64;
    let htk = l / g("playerLevelMax", 100.0) * (g("htkEndPve", 10.0) - g("htkStartPve", 5.0)) + g("htkStartPve", 5.0);
    (expected_player_hit(bundle, level) * htk * health_factor).trunc().max(1.0)
}

/// Enemy hit damage at a level (0x140e5df00): the player health the designers expect at that level divided by an
/// expected hits-to-die (htkStartEvp 5 at level 0 down to htkEndEvp 3 at level 100), × expectedBaseDamageFactor (2.5)
/// × playerStaminaPerAP / playerHealthPerAP, × the attack's damage factor.
/// `expected health = L × 3 × healthPerAP × flameLevelMax / playerLevelMax + playerBaseHealth`.
pub fn enemy_damage(bundle: &Bundle, level: u32, damage_mod: f64) -> f64 {
    let g = |k: &str, d: f64| bundle.balance(k).unwrap_or(d);
    let l = level as f64;
    let lmax = g("playerLevelMax", 100.0);
    let health = l * 3.0 * g("playerHealthPerAP", 50.0) * g("flameLevelMax", 20.0) / lmax + g("playerBaseHealth", 100.0);
    let (hs, he) = (g("htkStartEvp", 5.0), g("htkEndEvp", 3.0));
    let htd = hs - (hs - he) * (l / lmax);
    (health / htd * g("expectedBaseDamageFactor", 2.5) * damage_mod).abs().trunc()
}

/// Per damage type, the biggest hit one enemy of `target` lands (before your resistances): the enemy template's
/// attack hit with the largest total, each share × enemy_damage × AttackMod × (1 + 0.05 × Strength); DamageMod
/// counts once, inside enemy_damage (see Engine::summons).
pub fn enemy_hit_types(bundle: &Bundle, level: u32, target: &Target) -> Vec<(String, f64)> {
    let short = |k: &str| -> Option<&'static str> {
        if k == "poisonDamage" {
            return Some("Poison");
        }
        DAMAGE_TYPES.iter().find(|(key, _, _)| *key == k).map(|(_, _, s)| *s)
    };
    let chosen = target.incoming_type.clone().unwrap_or_else(|| "Pierce".into());
    if let Some(et) = target.enemy_type.as_ref().and_then(|n| bundle.enemy_types.get(n)) {
        let d = enemy_damage(bundle, level, et.damage_mod);
        let per = bundle.balance("damageScalePerAttributePoint").unwrap_or(0.05);
        let k = d * (1.0 + per * et.strength);
        let total = |h: &ens_data::EnemyHit| h.shares.values().sum::<f64>() * h.attack_mod;
        let best = et.attacks.iter().flat_map(|a| a.hits.iter())
            .max_by(|a, b| total(a).partial_cmp(&total(b)).unwrap_or(std::cmp::Ordering::Equal));
        if let Some(h) = best {
            let mut out: Vec<(String, f64)> = h.shares.iter().filter_map(|(key, s)| short(key).map(|t| (t.to_string(), k * s * h.attack_mod))).collect();
            out.sort_by(|a, b| a.0.cmp(&b.0));
            return out;
        }
        return vec![(chosen, k)];
    }
    vec![(chosen, enemy_damage(bundle, level, 1.0))]
}

#[cfg(test)]
mod enemy_tests {
    use super::*;
    #[test]
    fn enemy_health_grows() {
        let b = Bundle::load(ens_data::default_bundle_path()).unwrap();
        // hand-computed from the decoded formula: L50 = trunc(1.08 × 85 × 1.01^200... ) see enemy_health docs
        let e50 = 1.08 * 85.0 * 1.5f64.powi(4) * 1.6;
        assert!((enemy_health(&b, 50, 1.0) - (e50 * 7.5).trunc()).abs() < 1.0);
        assert!(enemy_health(&b, 10, 1.0) < enemy_health(&b, 50, 1.0));
        assert!(enemy_damage(&b, 50, 1.0) > enemy_damage(&b, 10, 1.0));
    }
}

#[cfg(test)]
mod aoe_tests {
    use super::*;
    #[test]
    fn explosive_arrow_hits_group() {
        let b = Bundle::load(ens_data::default_bundle_path()).unwrap();
        let eng = Engine::new(&b);
        let mut build = Build::sample_bow(&b);
        build.ammo = build::find_item(&b, "Explosive Arrow III");
        let w = build.weapon.clone().unwrap();
        let single = eng.dps(&build, &w, build.ammo.as_deref(), &Target::default(), 1.0);
        let group = Target { pack_size: 5.0, pack_radius: 4.5, ..Default::default() };
        let g = eng.dps(&build, &w, build.ammo.as_deref(), &group, 1.0);
        // 3 m blast in a 4.5 m spread: 1 + 4 × (3/4.5)^2 = 2.78 enemies
        assert!((g.aoe_targets - (1.0 + 4.0 * (3.0f64 / 4.5).powi(2))).abs() < 1e-9);
        assert!((g.regular_dps / single.regular_dps - g.aoe_targets).abs() < 1e-6);
    }

    /// Every class of weapon special (docs/plans/weapon-specials.md) on one target: the bow's Strong Shot at the middle
    /// roll; the greatsword's four impacts as weapon hits; the one-handed Whirlwind's impact plus its AttackMod 4
    /// projectile; the wand's single element projectile (its tag) from the weapon base plus its 0.75 implosion.
    #[test]
    fn weapon_specials_by_class() {
        let b = Bundle::load(ens_data::default_bundle_path()).unwrap();
        let eng = Engine::new(&b);
        let t = Target::default();
        let special = |cat: &str| {
            let (g, i) = b.items.iter().filter(|(_, i)| {
                i.weapon_category.as_ref().and_then(|c| b.weapon_categories.get(c)).is_some_and(|c| c.name == cat)
                    && i.sequences.iter().any(|s| s.trigger == "SpecialAbility")
            }).min_by_key(|(g, _)| (*g).clone()).unwrap();
            let mut build = Build::new(&b, 20);
            for s in i.sequences.iter().filter(|s| s.trigger == "SpecialAbility") {
                if let Some(id) = s.required_skill.as_deref().map(fnv1a_hex) {
                    build.ranks[b.skills.iter().position(|k| k.id == id).unwrap()] = 1;
                }
            }
            let w = Equip { item: g.clone(), rarity: Some("Legendary".into()), level: Some(20), perks: Some(0) };
            let ammo = match i.ammo.as_deref() {
                Some(k) => b.items.iter().filter(|(_, a)| a.slot.as_deref() == Some(k)).min_by_key(|(g, _)| (*g).clone()).map(|(g, _)| g.clone()),
                None => None,
            };
            let d = eng.dps(&build, &w, ammo.as_deref(), &t, 1.0);
            (build, w, ammo, d)
        };
        let (build, w, ammo, d) = special("Bow");
        let strong = eng.template_hit(&build, &w, ammo.as_deref(), &t, "Projectile_Player_Arrow_SpecialAbility_Strong_Shot", 1.0, 0.5);
        assert!((d.special_per_use - strong.after_armor).abs() < 1e-9 && strong.attack_mod == 6.0);
        let (build, w, ammo, d) = special("Two-Handed Greatsword");
        assert_eq!(d.special_projectiles, 4.0);
        assert!((d.special_per_use - 4.0 * eng.hit(&build, &w, ammo.as_deref(), 1.0, &t).after_armor).abs() < 1e-6);
        let (build, w, ammo, d) = special("One-Handed Sword");
        let proj = eng.template_hit(&build, &w, ammo.as_deref(), &t, "Projectile_Player_SpecialAbility_Sword_Projectile", 1.0, 0.5);
        assert_eq!((d.special_projectiles, proj.attack_mod), (2.0, 4.0));
        assert!((d.special_per_use - eng.hit(&build, &w, ammo.as_deref(), 1.0, &t).after_armor - proj.after_armor).abs() < 1e-6);
        let (_, _, _, d) = special("Wand");
        assert_eq!(d.special_projectiles, 1.0, "one element variant, picked by the wand's tags");
        let spawner = b.special_templates.iter().find(|(k, _)| k.starts_with("Projectile_Player_SpecialAbility_Wand_ImplosionSpawner")).unwrap().1;
        assert_eq!((spawner.base_damage.as_deref(), spawner.attack_mod), (Some(&["Pierce".to_string()][..]), Some(1.0)));
        assert!(d.special_per_use > 0.0 && d.special_dps > 0.0);
    }

    /// Strong Shot: Pierce only, from the weapon base damage at the bow's item level (not the bow's or the arrow's item
    /// damage), AttackMod 4 × (1 + r): the highest roll doubles the lowest, whatever the arrow.
    #[test]
    fn strong_shot_from_weapon_base_damage() {
        let b = Bundle::load(ens_data::default_bundle_path()).unwrap();
        let eng = Engine::new(&b);
        let build = Build::new(&b, 20);
        let bow = build::find_item(&b, "Talion's Bow").unwrap();
        let w = Equip { item: bow, rarity: Some("Legendary".into()), level: Some(20), perks: Some(5) };
        let t = Target::default();
        let lo = eng.strong_shot(&build, &w, build::find_item(&b, "Wooden Arrow").as_deref(), &t, 0.0).unwrap();
        let hi = eng.strong_shot(&build, &w, build::find_item(&b, "Explosive Arrow I").as_deref(), &t, 1.0).unwrap();
        assert_eq!(lo.per_type.iter().filter(|(_, v)| *v > 0.0).map(|(k, _)| k.as_str()).collect::<Vec<_>>(), ["Pierce"]);
        assert!((hi.non_crit / lo.non_crit - 2.0).abs() < 1e-9, "{} / {}", hi.non_crit, lo.non_crit);
        assert_eq!((lo.base_damage, lo.attack_mod, hi.attack_mod), (weapon_base_damage(&b, 20), 4.0, 8.0));
    }

    /// Shrapnel Blast adds 10% AttackMod per collision of the blast; Echoing Explosives adds a 40% chance of a 50%
    /// explosion; without the skills the blast is the plain hit and there is no echo; an arrow without a blast has none.
    #[test]
    fn explosive_arrow_blast_and_echo() {
        let b = Bundle::load(ens_data::default_bundle_path()).unwrap();
        let eng = Engine::new(&b);
        let mut build = Build::new(&b, 20);
        let w = Equip { item: build::find_item(&b, "Talion's Bow").unwrap(), rarity: None, level: None, perks: None };
        let arrow = build::find_item(&b, "Explosive Arrow I");
        let t = Target::default();
        let plain = eng.hit(&build, &w, arrow.as_deref(), 1.0, &t).non_crit;
        let none = eng.explosive_hits(&build, &w, arrow.as_deref(), &t, 3).unwrap();
        assert!((none.blast.non_crit - plain).abs() < 1e-9 && none.echo.is_none());
        let dps_before = eng.dps(&build, &w, arrow.as_deref(), &t, 1.0).regular_dps;
        for name in ["Shrapnel Blast", "Echoing Explosives"] {
            let i = b.skills.iter().position(|s| s.name == name).unwrap();
            build.ranks[i] = 1;
        }
        let x = eng.explosive_hits(&build, &w, arrow.as_deref(), &t, 3).unwrap();
        assert!((x.blast_attack_mod - 1.3).abs() < 1e-9);
        assert!((x.blast.non_crit / plain - 1.3).abs() < 1e-6, "{} / {plain}", x.blast.non_crit);
        let echo = x.echo.unwrap();
        assert!((echo.non_crit / plain - 0.5).abs() < 1e-6 && (x.echo_chance - 0.4).abs() < 1e-9);
        // DPS on one target: the blast at 1.1 (one collision) plus 40% of a 0.5 echo
        let dps_after = eng.dps(&build, &w, arrow.as_deref(), &t, 1.0).regular_dps;
        assert!((dps_after / dps_before - 1.3).abs() < 1e-6, "{dps_after} / {dps_before}");
        assert!(eng.explosive_hits(&build, &w, build::find_item(&b, "Wooden Arrow").as_deref(), &t, 3).is_none());
    }
}

#[cfg(test)]
mod melee_tests {
    use super::*;

    /// The best combo takes late follow-ups when they pay: the greatsword's late chain (Late_1_Uppercut ×2.0 and
    /// Late_2 ×2.0) beats looping its early swings (×1, ×1, ×1.5), and the daggers keep their plain four-hit chain.
    #[test]
    fn combo_takes_the_late_finisher_when_it_pays() {
        let b = Bundle::load(ens_data::default_bundle_path()).unwrap();
        let e = Engine::new(&b);
        let build = Build::new(&b, 19);
        let combo = |n: &str| e.melee_combo(&build, &Equip { item: build::find_item(&b, n).unwrap(), rarity: None, level: Some(15), perks: Some(0) }).unwrap();
        let gs = combo("Barbarian Greatsword");
        assert!(gs.names.iter().any(|n| n.contains("Late")), "{:?}", gs.names);
        assert!((gs.mean_attack_mod() - 2.0).abs() < 1e-9);
        let early_rate = 3.5 / (0.683 + 0.683 + 0.683); // the early loop's three swings, each until the next at 0.683 s
        assert!(gs.attack_mod_sum / gs.time > early_rate * 0.9, "late chain {} / {} s", gs.attack_mod_sum, gs.time);
        let d = combo("Baroness Daggers");
        assert_eq!((d.steps, d.hits, d.mean_attack_mod()), (4, 4.0, 1.0));
    }

    #[test]
    fn sword_combo_and_swift_blades() {
        let b = Bundle::load(ens_data::default_bundle_path()).unwrap();
        let eng = Engine::new(&b);
        let mut build = Build::new(&b, 30);
        build.weapon = build::find_item(&b, "Hailscourge").map(|g| Equip { item: g, rarity: None, level: Some(30), perks: None });
        let w = build.weapon.clone().unwrap();
        let (slow, how) = eng.attack_interval(&build, &w, None);
        assert!((slow - 1.9 / 3.0).abs() < 0.02, "{slow} {how}");
        build.set_rank_by_name(&b, "Swift Blades", 1);
        let (fast, _) = eng.attack_interval(&build, &w, None);
        assert!(fast < slow, "{fast} vs {slow}");
    }

    #[test]
    fn bloodletting_orb_cascade() {
        let b = ens_data::Bundle::load(concat!(env!("CARGO_MANIFEST_DIR"), "/../../data/bundle.json")).unwrap();
        let e = Engine::new(&b);
        let mut bd = Build::new(&b, 50);
        bd.set_rank_by_name(&b, "Bloodletting", 3);
        bd.set_rank_by_name(&b, "Swiftshot Sustenance", 3);
        let t = Target { orb_pickup: 1.0, ..Default::default() };
        let orb = |w: &Build, kind: &str| -> Vec<Proc> {
            let mut w = w.clone();
            let g = b.items.iter().find(|(_, i)| i.category.as_deref() == Some("Weapons") && i.weapon_category.as_ref().and_then(|c| b.weapon_categories.get(c)).and_then(|c| c.kind.as_deref()) == Some(kind)).unwrap().0.clone();
            w.weapon = Some(Equip { item: g, rarity: None, level: Some(50), perks: None });
            e.procs(&w, 1.0, 0.5, 0.1, 50.0, &t).into_iter().filter(|p| p.orbs > 0.0).collect()
        };
        // staff: Bloodletting fires on crits; the six groups give 2 orbs in total per trigger
        let staff = orb(&bd, "RangedStaff");
        let n: f64 = staff.iter().filter(|p| p.skill == "Bloodletting").map(|p| p.orbs).sum();
        assert!((n - 2.0).abs() < 1e-9, "{staff:?}");
        let h = staff.iter().find(|p| p.resource == "Health").unwrap();
        assert!((h.orbs - 0.74).abs() < 1e-9 && (h.chance - 0.6).abs() < 1e-9 && h.event == "crit", "{h:?}");
        // bow: Swiftshot Sustenance, one stamina orb on 36% of kills
        let bow = orb(&bd, "RangedBow");
        let sw = bow.iter().find(|p| p.skill == "Swiftshot Sustenance").unwrap();
        assert!(sw.resource == "Stamina" && sw.event == "kill" && (sw.chance - 0.36).abs() < 1e-9, "{sw:?}");
        assert!(bow.iter().all(|p| p.skill != "Bloodletting"));
    }
}

#[cfg(test)]
mod against_tests {
    use super::*;
    #[test]
    fn faction_class_and_flying_mods_join_the_pool() {
        let b = ens_data::Bundle::load(concat!(env!("CARGO_MANIFEST_DIR"), "/../../data/bundle.json")).unwrap();
        let e = Engine::new(&b);
        let bd = Build::sample_bow(&b);
        let w = bd.weapon.clone().unwrap();
        let plain = e.hit(&bd, &w, bd.ammo.as_deref(), 1.0, &Target::default()).non_crit;
        let mut boosted = bd.clone();
        boosted.extra = vec![("DamageModAgainst_Faction_Fogger".into(), 0.2), ("DamageModAgainst_Flying".into(), 0.1), ("DamageModAgainst_Ranged".into(), 0.05)];
        let fell = Target { faction: Some("Fogger".into()), enemy_class: Some("Ranger".into()), flying: true, ..Default::default() };
        let other = Target { faction: Some("Scavenger".into()), ..Default::default() };
        let h = e.hit(&boosted, &w, bd.ammo.as_deref(), 1.0, &fell);
        let pool: f64 = h.pool_terms.iter().filter(|(k, _)| k.contains("Against")).map(|(_, v)| v).sum();
        assert!((pool - 0.35).abs() < 1e-9, "{:?}", h.pool_terms);
        assert!(h.non_crit > plain);
        assert!((e.hit(&boosted, &w, bd.ammo.as_deref(), 1.0, &other).non_crit - plain).abs() < 1e-9);
        // every faction the enemy data uses that has a mod maps to an attribute the game defines
        for t in b.enemy_types.values() {
            if let Some(a) = faction_against_attribute(&t.faction) {
                assert!(b.attributes.iter().any(|c| c.names.iter().any(|n| n == a)), "{a}");
            }
        }
    }
}

#[cfg(test)]
mod perk_proc_tests {
    use super::*;
    #[test]
    fn leech_perks_on_equipped_gear() {
        let b = ens_data::Bundle::load(concat!(env!("CARGO_MANIFEST_DIR"), "/../../data/bundle.json")).unwrap();
        let e = Engine::new(&b);
        // an item whose first perk is a Mana or Stamina Leech (ring) and one with Health Leech (weapon)
        let find = |name: &str| b.items.iter().filter(|(_, i)| i.perks.first().and_then(|p| b.perks.get(p)).map(|p| p.name == name).unwrap_or(false)).map(|(g, _)| g.clone()).min();
        let mut bd = Build::sample_bow(&b);
        let bow = bd.weapon.clone().unwrap();
        let ring = find("Mana Leech").expect("ring with Mana Leech first");
        bd.gear[5] = Some(Equip { item: ring, rarity: Some("Common".into()), level: None, perks: None });
        let wbit = weapon_type_bit("RangedBow");
        let pr = e.perk_procs(&bd, wbit, 2.0, 0.1, 0.0, 40.0);
        let mana = pr.iter().find(|p| p.resource == "Mana").expect("mana leech proc");
        assert!(mana.amount > 0.0 && (mana.per_s - 2.0 * mana.amount).abs() < 1e-9, "{pr:?}");
        if let Some(w) = find("Health Leech") {
            let kind = b.items[&w].weapon_category.as_ref().and_then(|c| b.weapon_categories.get(c)).and_then(|c| c.kind.clone()).unwrap_or_default();
            let mut bh = Build::sample_bow(&b);
            bh.weapon = Some(Equip { item: w, rarity: Some("Common".into()), level: Some(50), perks: None });
            let hp = e.perk_procs(&bh, weapon_type_bit(&kind), 1.0, 0.1, 0.0, 100.0);
            let h = hp.iter().find(|p| p.resource == "Health").expect("health leech proc");
            assert!(h.amount > 0.0 && h.amount < 100.0, "{hp:?}");
        }
        let _ = bow;
    }
}

#[cfg(test)]
mod armor_rule_tests {
    use super::*;
    #[test]
    fn armor_matches_the_game_rule() {
        // 100 fire with 10 fire points and a 30-point magical pool: r = 80 - 10 = 70, pool takes 30 -> 40 + 20 = 60
        let v = armor_reduce(&[("Fire".into(), 100.0)], &|t| if t == "Fire" { 10.0 } else { 0.0 }, 0.0, 30.0, 0.2);
        assert!((v - 60.0).abs() < 1e-9, "{v}");
        // lightning is never armored
        assert!((armor_reduce(&[("Lightning".into(), 100.0)], &|_| 50.0, 500.0, 500.0, 0.2) - 100.0).abs() < 1e-9);
        // the physical pool is spent on cut before pierce
        let v = armor_reduce(&[("Pierce".into(), 50.0), ("Cut".into(), 50.0)], &|_| 0.0, 40.0, 0.0, 0.2);
        assert!((v - (10.0 + 40.0 + 10.0 + 0.0)).abs() < 1e-9, "{v}");
        // an unarmored, unresisted build takes the whole enemy hit
        let b = ens_data::Bundle::load(concat!(env!("CARGO_MANIFEST_DIR"), "/../../data/bundle.json")).unwrap();
        let e = Engine::new(&b);
        let bd = Build::new(&b, 50);
        let (raw, taken) = e.incoming(&bd, 50, "Pierce");
        assert!(raw > 0.0 && (taken - raw).abs() < 1e-6, "{raw} {taken}");
        // Fire Resistance (DamageSusceptibility_Fire) lowers fire hits
        let mut r = bd.clone();
        r.extra = vec![("DamageSusceptibility_Fire".into(), -0.3)];
        assert!(e.incoming(&r, 50, "Fire").1 < e.incoming(&bd, 50, "Fire").1);
    }
}

#[cfg(test)]
mod situational_tests {
    use super::*;
    #[test]
    fn sneak_merciless_weak_spot_and_soaked() {
        let b = ens_data::Bundle::load(concat!(env!("CARGO_MANIFEST_DIR"), "/../../data/bundle.json")).unwrap();
        let e = Engine::new(&b);
        let bd = Build::sample_bow(&b);
        let w = bd.weapon.clone().unwrap();
        let hit = |t: &Target| e.hit(&bd, &w, bd.ammo.as_deref(), 1.0, t);
        let base = hit(&Target::default());
        // sneak and merciless attacks need their skills and a weapon that is not a wand, staff or bow
        let mut melee = Build::new(&b, 50);
        melee.weapon = Some(Equip { item: crate::build::find_item(&b, "Shroud Vessel's Greatsword").unwrap(), rarity: None, level: None, perks: None });
        melee.set_rank_by_name(&b, "Sneak Attack", 1);
        melee.set_rank_by_name(&b, "Merciless Attack", 1);
        let mw = melee.weapon.clone().unwrap();
        let mhit = |t: &Target| e.hit(&melee, &mw, None, 1.0, t);
        let sneak = mhit(&Target { hit_kind: Some("Sneak".into()), ..Default::default() });
        let pool = |h: &HitBreakdown, k: &str| h.pool_terms.iter().find(|(n, _)| n.contains(k)).map(|x| x.1).unwrap_or(0.0);
        // player template DamageModSneak_Base 10, DamageModMerciless_Base 6
        assert!((pool(&sneak, "Sneak") - 9.0).abs() < 1e-9, "{:?}", sneak.pool_terms);
        assert!(sneak.non_crit > mhit(&Target::default()).non_crit * 5.0);
        assert_eq!(pool(&hit(&Target { hit_kind: Some("Sneak".into()), ..Default::default() }), "Sneak"), 0.0, "a bow cannot sneak attack");
        let merc = mhit(&Target { hit_kind: Some("Merciless".into()), ..Default::default() });
        assert!((pool(&merc, "Merciless") - 5.0).abs() < 1e-9);
        // weak spot only counts when the target has a SkillShot susceptibility above 1
        let mut sus = HashMap::new();
        sus.insert("SkillShot".to_string(), 1.5);
        let weak = hit(&Target { weak_spot: true, susceptibility: sus, ..Default::default() });
        assert!((pool(&weak, "SkillShot (target)") - 0.5).abs() < 1e-9 && weak.non_crit > base.non_crit);
        assert!((hit(&Target { weak_spot: true, ..Default::default() }).non_crit - base.non_crit).abs() < 1e-9);
        // soaked: crit damage gains CritDamageMod_Vs_Soaked
        let mut bs = bd.clone();
        bs.extra = vec![("CritDamageMod_Vs_Soaked".into(), 0.5)];
        let wet = e.hit(&bs, &w, bd.ammo.as_deref(), 1.0, &Target { soaked: true, ..Default::default() });
        assert!((wet.crit_damage_mod - base.crit_damage_mod - 0.5).abs() < 1e-9);
    }
}

#[cfg(test)]
mod triggered_buff_tests {
    use super::*;
    #[test]
    fn blood_rage_and_sting_decode_and_raise_dps() {
        let b = ens_data::Bundle::load(concat!(env!("CARGO_MANIFEST_DIR"), "/../../data/bundle.json")).unwrap();
        let rage = b.skills.iter().find(|s| s.name == "Blood Rage").unwrap();
        let tb = decode_triggered_buff(rage).expect("Blood Rage decodes");
        assert_eq!(tb.trigger, "kill");
        assert_eq!(tb.attack_category.as_deref(), Some("Melee"));
        assert!((tb.lifetime - 10.0).abs() < 1e-9 && tb.effects == vec![("MeleeDamageMod".to_string(), 0.15)], "{tb:?}");
        let sting = decode_triggered_buff(b.skills.iter().find(|s| s.name == "Sting").unwrap()).expect("Sting decodes");
        assert_eq!(sting.trigger, "hit");
        // debuffs on the enemy decode as target debuffs
        assert!(decode_triggered_buff(b.skills.iter().find(|s| s.name == "Expose Weakness").unwrap()).map(|t| t.on_target).unwrap_or(false));
        // a melee build with Blood Rage gains DPS when enemies die
        let e = Engine::new(&b);
        let arch = crate::audit::archetypes(&b);
        let (_, sword) = arch.iter().find(|(n, _)| n == "one-hand sword").unwrap();
        let t = Target { enemy_health: 500.0, ..Default::default() };
        let plain = e.total(sword, &t).total;
        let mut x = sword.clone();
        x.set_rank_by_name(&b, "Blood Rage", 3);
        let tot = e.total(&x, &t);
        assert!(tot.total > plain && tot.buffs.iter().any(|(n, u, _)| n == "Blood Rage" && *u > 0.0 && *u < 1.0), "{plain} {:?}", tot.buffs);
    }
}

#[cfg(test)]
mod susceptibility_tests {
    use super::*;
    #[test]
    fn physical_and_magical_multiply_the_per_type_value() {
        let b = ens_data::Bundle::load(concat!(env!("CARGO_MANIFEST_DIR"), "/../../data/bundle.json")).unwrap();
        let e = Engine::new(&b);
        let mut raw = HashMap::new();
        raw.insert("Physical".to_string(), 0.8);
        raw.insert("Pierce".to_string(), 1.25);
        raw.insert("MagBase".to_string(), 1.2);
        raw.insert("Fire".to_string(), 1.5);
        raw.insert("Poison".to_string(), 0.5);
        let f = e.effective_susceptibility(&raw);
        assert!((f["Pierce"] - 1.0).abs() < 1e-9, "{f:?}");
        assert!((f["Fire"] - 1.8).abs() < 1e-9 && (f["Ice"] - 1.2).abs() < 1e-9 && (f["Poison"] - 0.5).abs() < 1e-9, "{f:?}");
        // the training dummy has no resistances
        assert!(e.effective_susceptibility(&HashMap::new()).values().all(|v| (v - 1.0).abs() < 1e-9));
        // Expose Weakness decodes as a debuff on the target
        let ew = decode_triggered_buff(b.skills.iter().find(|s| s.name == "Expose Weakness").unwrap()).expect("decodes");
        assert!(ew.on_target && ew.trigger == "crit" && ew.effects == vec![("DamageSusceptibility_Physical".to_string(), 0.25)], "{ew:?}");
    }
}

#[cfg(test)]
mod skill_damage_tests {
    use super::*;
    #[test]
    fn chain_hit_mass_destruction_radiant_aura() {
        let b = ens_data::Bundle::load(concat!(env!("CARGO_MANIFEST_DIR"), "/../../data/bundle.json")).unwrap();
        let e = Engine::new(&b);
        let arch = crate::audit::archetypes(&b);
        let staff = arch.iter().find(|(n, _)| n == "staff").unwrap().1.clone();
        let mut x = staff.clone();
        x.set_rank_by_name(&b, "Chain Hit", 1);
        x.set_rank_by_name(&b, "Mass Destruction", 1);
        let t = Target { pack_size: 3.0, ..Default::default() };
        let d = e.skill_damage(&x, &t, 2.0, 0.25);
        let st = e.attributes(&x);
        let int = st.get("Intelligence");
        // both can crit (canCrit unconnected): × (1 + CritChance × (CritDamageMod − 1))
        let crit = 1.0 + st.get("CritChance").clamp(0.0, st.get("CritChance_Max").max(0.01)) * (st.get("CritDamageMod") - 1.0);
        let ch = d.iter().find(|s| s.0 == "Chain Hit").expect("chain hit");
        assert!((ch.1 - 5.0 * int * crit * 2.0 * 0.25).abs() < 1e-6, "{d:?}");
        let md = d.iter().find(|s| s.0 == "Mass Destruction").expect("mass destruction");
        assert!((md.1 - 2.0 * int * crit * 3.0 * 2.0 * 0.25).abs() < 1e-6, "{d:?}");
        assert!(e.total(&x, &t).total > e.total(&staff, &t).total);
        // Radiant Aura only hurts Fell (Fogger) enemies, with a melee weapon
        let sword = arch.iter().find(|(n, _)| n == "one-hand sword").unwrap().1.clone();
        let mut y = sword.clone();
        y.set_rank_by_name(&b, "Radiant Aura", 3);
        let fell = Target { faction: Some("Fogger".into()), ..Default::default() };
        assert!(e.skill_damage(&y, &fell, 1.0, 0.1).iter().any(|s| s.0 == "Radiant Aura" && s.1 > 0.0));
        assert!(e.skill_damage(&y, &Target::default(), 1.0, 0.1).is_empty());
    }
}

#[cfg(test)]
mod resource_skill_tests {
    use super::*;
    #[test]
    fn battle_heal_unity_and_tower() {
        let b = ens_data::Bundle::load(concat!(env!("CARGO_MANIFEST_DIR"), "/../../data/bundle.json")).unwrap();
        let e = Engine::new(&b);
        let arch = crate::audit::archetypes(&b);
        let mut sword = arch.iter().find(|(n, _)| n == "one-hand sword").unwrap().1.clone();
        sword.set_rank_by_name(&b, "Battle Heal", 3);
        let p = e.skill_resource_procs(&sword, "OneHandSword", 2.0, 0.25, 0.0);
        let bh = p.iter().find(|x| x.skill == "Battle Heal").expect("battle heal");
        let hp = e.sheet(&sword).values["Health"];
        assert!(bh.resource == "Health" && (bh.amount - 0.03 * hp).abs() < 1e-6 && (bh.per_s - 0.5 * bh.amount).abs() < 1e-6, "{p:?}");
        let mut wand = arch.iter().find(|(n, _)| n == "wand").unwrap().1.clone();
        wand.set_rank_by_name(&b, "Unity", 3);
        let u = e.skill_resource_procs(&wand, "RangedWand", 2.0, 0.25, 0.0);
        let un = u.iter().find(|x| x.skill == "Unity").expect("unity");
        assert!(un.resource == "Mana" && (un.chance - 0.3).abs() < 1e-9, "{u:?}");
        // Tower: 10% less physical damage taken with three or more enemies near
        let mut t = Build::new(&b, 50);
        t.set_rank_by_name(&b, "Tower", 1);
        let one = e.incoming_near(&t, 50, "Pierce", 1.0).1;
        let three = e.incoming_near(&t, 50, "Pierce", 3.0).1;
        assert!(three < one, "{one} {three}");
    }
}

#[cfg(test)]
mod perk_armor_tests {
    use super::*;
    #[test]
    fn armor_perks_add_points_at_their_slot_level() {
        let b = ens_data::Bundle::load(concat!(env!("CARGO_MANIFEST_DIR"), "/../../data/bundle.json")).unwrap();
        let e = Engine::new(&b);
        // an item whose first perk is an armor perk (Perk_Armor_*), equipped at Common (1 active perk) and level 50
        let (g, item, share_key) = b.items.iter().filter_map(|(g, i)| {
            let p = b.perks.get(i.perks.first()?)?;
            let (k, _) = p.armor.as_ref()?.iter().find(|(_, v)| **v > 0.0)?;
            Some((g.clone(), i, k.clone()))
        }).min_by(|a, b| a.0.cmp(&b.0)).expect("an item with an armor perk first");
        let attr = format!("ArmorPoints_{}_Adder", { let mut c = share_key.chars(); c.next().unwrap().to_uppercase().collect::<String>() + c.as_str() });
        let mut bd = Build::new(&b, 50);
        let slot = ens_calc_slot(item.slot.as_deref());
        let eq = Equip { item: g, rarity: Some("Common".into()), level: Some(50), perks: None };
        match slot { Some(k) => bd.gear[k] = Some(eq), None => bd.weapon = Some(eq) }
        let share = b.perks[item.perks.first().unwrap()].armor.as_ref().unwrap()[&share_key];
        // one active perk: slot 0 counts at level 50 − 1 + 1 + 0 = 50
        let expect = ((23.0 + 15.0 * 50.0) * share).round();
        let got: f64 = e.attributes(&bd).sources(&attr).iter().filter(|c| c.source.starts_with("armor perk")).map(|c| c.value).sum();
        assert!((got - expect).abs() < 1e-9, "{attr}: got {got}, expected {expect}");
    }

    fn ens_calc_slot(slot: Option<&str>) -> Option<usize> {
        crate::build::GEAR_SLOTS.iter().position(|s| Some(*s) == slot)
    }
}

#[cfg(test)]
mod flame_tests {
    use super::*;
    #[test]
    fn flame_level_gives_every_attribute_points() {
        let b = ens_data::Bundle::load(concat!(env!("CARGO_MANIFEST_DIR"), "/../../data/bundle.json")).unwrap();
        let e = Engine::new(&b);
        let mut one = Build::new(&b, 50);
        one.flame_level = 1;
        let mut ten = one.clone();
        ten.flame_level = 10;
        for a in ["Strength", "Dexterity", "Intelligence", "Constitution", "Endurance", "Spirit"] {
            let d = e.attributes(&ten).get(a) - e.attributes(&one).get(a);
            assert!((d - 9.0).abs() < 1e-9, "{a} +{d}");
        }
        // Graceful Stride adds floor(10 / 2) = 5 more Dexterity
        let mut gs = ten.clone();
        gs.set_rank_by_name(&b, "Graceful Stride", 1);
        assert!((e.attributes(&gs).get("Dexterity") - e.attributes(&ten).get("Dexterity") - 5.0).abs() < 1e-9);
    }
}

#[cfg(test)]
mod enemy_damage_tests {
    use super::*;

    /// Hand-checked from 0x140e5dbe0: L50 health 50 × 3 × 50 × 20 / 100 + 100 = 1600, hits-to-die 5 − 2 × 0.5 = 4,
    /// so 1600 / 4 × 2.5 = 1000 × DamageMod. A Skeleton Hound (DamageMod 0.5) deals 500 × the attack's shares: DamageMod
    /// counts once (a DamageMod 0.5 critter's logged hits on the player, see Engine::summons).
    #[test]
    fn enemy_damage_formula_and_hound_hit() {
        let b = Bundle::load(ens_data::default_bundle_path()).unwrap();
        assert_eq!(enemy_damage(&b, 50, 1.0), 1000.0);
        assert_eq!(enemy_damage(&b, 50, 0.5), 500.0);
        let hound = &b.enemy_types["Enemy_Skeleton_Hound"];
        assert!((hound.damage_mod - 0.5).abs() < 1e-9);
        let t = Target { enemy_type: Some("Enemy_Skeleton_Hound".into()), ..Default::default() };
        let best: f64 = hound.attacks.iter().flat_map(|a| a.hits.iter()).map(|h| h.shares.values().sum::<f64>() * h.attack_mod).fold(0.0, f64::max);
        let raw: f64 = enemy_hit_types(&b, 50, &t).iter().map(|(_, d)| d).sum();
        assert!((raw - 500.0 * best).abs() < 1e-6, "{raw} vs {}", 500.0 * best);
        let e = Engine::new(&b);
        let mut build = Build::new(&b, 50);
        // a summon spell is cast from a staff (R8-TC-01)
        build.weapon = build::find_item(&b, "Luminary Staff").map(|g| build::Equip { item: g, rarity: None, level: None, perks: None });
        build.summons = build::find_item(&b, "Hollow Wretch I").into_iter().collect();
        let s = e.summons(&build, &Target::default());
        assert!(s.first().map(|x| x.hit > 0.0 && x.note.contains("attacks")).unwrap_or(false), "{s:?}");
    }

    /// A summon attacks every SUMMON_ATTACK_S (2.6 s, measured) by default, at the setting when one is given, and
    /// never faster than its attack animation (the Hollow Wretch's averages about 1.6 s).
    #[test]
    fn summon_attack_pace_setting() {
        let b = Bundle::load(ens_data::default_bundle_path()).unwrap();
        let e = Engine::new(&b);
        let mut build = Build::new(&b, 19);
        // a summon spell is cast from a staff (R8-TC-01)
        build.weapon = build::find_item(&b, "Luminary Staff").map(|g| build::Equip { item: g, rarity: None, level: None, perks: None });
        build.summons = build::find_item(&b, "Hollow Wretch I").into_iter().collect();
        let at = |s: f64| {
            let t = Target { play: Play { summon_attack_s: s, ..Default::default() }, ..Default::default() };
            e.summons(&build, &t)[0].clone()
        };
        let (default, slow, fast) = (at(0.0), at(5.2), at(0.5));
        assert!((default.interval_s - SUMMON_ATTACK_S).abs() < 1e-9, "{}", default.interval_s);
        assert!((slow.interval_s - 5.2).abs() < 1e-9 && (slow.each_dps * 2.0 - default.each_dps).abs() < 1e-6);
        assert!(fast.interval_s > 1.4 && fast.interval_s < 2.0, "capped at the animation: {}", fast.interval_s);
    }
}

#[cfg(test)]
mod skill_level_tests {
    use super::*;

    /// 0x140e7c0c0: level = 0 without a tree rank, else tree rank + gear ranks, not capped at the max.
    #[test]
    fn gear_ranks_need_a_tree_rank_and_are_uncapped() {
        let b = Bundle::load(ens_data::default_bundle_path()).unwrap();
        let e = Engine::new(&b);
        // an item whose first perk grants a level of a multi-rank skill
        let (guid, skill_idx) = b.items.iter().find_map(|(g, it)| {
            let p = b.perks.get(it.perks.first()?)?;
            let id = p.skill_level.as_ref()?.get("skillId")?.get("value")?.as_u64()? as u32;
            let i = b.skills.iter().position(|s| s.id == id && s.max > 0)?;
            Some((g.clone(), i))
        }).expect("an item granting a skill rank");
        let mut build = Build::new(&b, 50);
        build.weapon = Some(build::Equip { item: guid, rarity: Some("Legendary".into()), level: None, perks: None });
        assert_eq!(e.effective_ranks(&build)[skill_idx], 0);
        let max = b.skills[skill_idx].max as u8;
        build.ranks[skill_idx] = max;
        assert!(e.effective_ranks(&build)[skill_idx] > max as u32);
    }
}

#[cfg(test)]
mod uptime_tests {
    use super::*;

    #[test]
    fn buff_uptime_follows_the_reapply_rule() {
        // 1–2 hits fall in any 1.5 s window (half the time each): 1 − (0.5 × 0.5 + 0.5 × 0.25) = 0.625
        assert!((buff_uptime_events(0.5, 1.0, 1.5, 1.0) - 0.625).abs() < 1e-12);
        // every hit triggers and hits come faster than the buff runs out: always up
        assert!((buff_uptime_events(1.0, 0.5, 2.0, 1.0) - 1.0).abs() < 1e-12);
        // ReplaceAtEnd can only be re-applied late, so it is up less than a ResetTime buff
        assert!(buff_uptime_events(0.3, 0.5, 6.0, 0.33) < buff_uptime_events(0.3, 0.5, 6.0, 1.0));
        // random triggers with re-apply at any time: 1 − e^(−rate × lifetime)
        assert!((buff_uptime_random(0.2, 5.0, 1.0) - (1.0 - (-1.0f64).exp())).abs() < 1e-12);
    }
}

#[cfg(test)]
mod backstab_tests {
    use super::*;

    /// 0x1403d68aa: a backstab adds DamageModBackstab − 1 to the pool, only on targets that can be backstabbed.
    #[test]
    fn backstab_adds_its_mod_only_where_allowed() {
        let b = ens_data::Bundle::load(concat!(env!("CARGO_MANIFEST_DIR"), "/../../data/bundle.json")).unwrap();
        let e = Engine::new(&b);
        let mut bd = Build::sample_bow(&b);
        bd.set_rank_by_name(&b, "Backstab Mastery", 1);
        let w = bd.weapon.clone().unwrap();
        let pool = |t: &Target| e.hit(&bd, &w, bd.ammo.as_deref(), 1.0, t).pool_terms.iter().find(|(n, _)| n.contains("Backstab")).map(|x| x.1);
        let want = e.attributes(&bd).get("DamageModBackstab") - 1.0;
        assert!(want > 0.0);
        assert!((pool(&Target { hit_kind: Some("Backstab".into()), ..Default::default() }).unwrap() - want).abs() < 1e-9);
        let immune = b.enemy_types.iter().find(|(_, t)| !t.backstab).map(|(n, _)| n.clone()).unwrap();
        assert!(pool(&Target { hit_kind: Some("Backstab".into()), enemy_type: Some(immune), ..Default::default() }).is_none());
        assert!(pool(&Target::default()).is_none());
    }
}

#[cfg(test)]
mod gem_tests {
    use super::*;

    #[test]
    fn bleeding_gem_scales_and_uses_the_victims_damage_mod() {
        let b = ens_data::Bundle::load(concat!(env!("CARGO_MANIFEST_DIR"), "/../../data/bundle.json")).unwrap();
        let e = Engine::new(&b);
        let (gid, gem) = b.gems.iter().find(|(_, g)| g.debug == "T1_Gem_GemOfBleeding").unwrap();
        assert_eq!(gem_value(&b, gem, "1580527771", 50.0), Some(28.0));
        assert!((gem_value(&b, gem, "1073225515", 50.0).unwrap() - 9.75).abs() < 1e-9);
        let arch = crate::audit::archetypes(&b);
        let mut sword = arch.iter().find(|(n, _)| n == "one-hand sword").unwrap().1.clone();
        sword.gem = Some(build::Equip { item: gid.clone(), rarity: None, level: Some(50), perks: None });
        let w = sword.weapon.clone().unwrap();
        let hit = e.hit(&sword, &w, None, 1.0, &Target::default());
        let d = e.gem_damage(&sword, &Target::default(), 2.0, &hit, 0.0);
        let u = buff_uptime_events(hit.crit_chance, 0.5, 9.75, 1.0);
        assert!((d[0].1 - 28.0 * u).abs() < 1e-6, "{d:?}");
        let hound = Target { enemy_type: Some("Enemy_Skeleton_Hound".into()), ..Default::default() };
        let dh = e.gem_damage(&sword, &hound, 2.0, &hit, 0.0);
        assert!((dh[0].1 - 0.5 * d[0].1).abs() < 1e-6 * (1.0 + d[0].1), "{dh:?}");
    }
}

#[cfg(test)]
mod necro_gem_tests {
    use super::*;

    /// Spawn-limit caps from 0x140336270 and the level overwrite 1 + 0.15 × 6.6666 × gem level.
    #[test]
    fn lesser_necromancy_fills_four_companions() {
        let b = ens_data::Bundle::load(concat!(env!("CARGO_MANIFEST_DIR"), "/../../data/bundle.json")).unwrap();
        let e = Engine::new(&b);
        let (gid, gem) = b.gems.iter().find(|(_, g)| g.debug == "T3_Gem_GemOfLesserNecromancy").unwrap();
        assert_eq!(gem_value(&b, gem, "519137714", 50.0), Some(51.0));
        let mut x = Build::new(&b, 50);
        x.gem = Some(build::Equip { item: gid.clone(), rarity: None, level: Some(50), perks: None });
        let hit = HitBreakdown::default();
        assert!(e.gem_damage(&x, &Target::default(), 1.0, &hit, 0.0).is_empty());
        let d = e.gem_damage(&x, &Target::default(), 1.0, &hit, 0.2);
        assert!(d.len() == 1 && d[0].1 > 0.0 && d[0].2.contains("of 4 companions alive on average at level 51"), "{d:?}");
    }

    /// An attack's SpawnImpacts hold one variant per entity tag at the same time; the bundle keeps the summon's own.
    /// The Hollow Reaper's combo is 2 hits of 0.4 cut + 0.2 ice (not 4), so one Reaper out-hits one Wretch, while the
    /// Lesser gem's 4 Wretches (DamageMod 1.0) still out-damage the Greater gem's 1 Reaper (DamageMod 0.9).
    #[test]
    fn necromancy_gems_price_one_variant_per_hit() {
        let b = ens_data::Bundle::load(concat!(env!("CARGO_MANIFEST_DIR"), "/../../data/bundle.json")).unwrap();
        let reaper = &b.ally_types["Player_Companion_Skeleton_Elite_Melee_GemOfGreaterNecromancy"];
        assert_eq!(reaper.attacks[0].hits.len(), 2);
        let wretch = &b.ally_types["Player_Companion_Skeleton_Weakling_GemOfLesserNecromancy"];
        assert!(wretch.attacks.iter().all(|a| a.hits.len() == 1));
        let e = Engine::new(&b);
        let dps = |debug: &str| {
            let (gid, _) = b.gems.iter().find(|(_, g)| g.debug == debug).unwrap();
            let mut x = Build::new(&b, 50);
            x.gem = Some(build::Equip { item: gid.clone(), rarity: None, level: Some(50), perks: None });
            e.gem_damage(&x, &Target::default(), 1.0, &HitBreakdown::default(), 0.2)[0].1
        };
        let (lesser, mid, greater) = (dps("T3_Gem_GemOfLesserNecromancy"), dps("T5_Gem_GemOfNecromancy"), dps("T6_Gem_GemOfGreaterNecromancy"));
        println!("gem level 50: Lesser {lesser:.0}, Necromancy {mid:.0}, Greater {greater:.0} DPS");
        assert!(lesser > greater, "{lesser} {mid} {greater}");
    }

    /// min(cap, Poisson(rate × life)), and a fight starting from none averages lower.
    #[test]
    fn expected_alive_counts() {
        // no cap pressure: Little's law, rate × life
        assert!((expected_alive(1000.0, 0.1, 30.0, 0.0) - 3.0).abs() < 1e-9);
        // cap 1: the chance at least one spawned in the last life, 1 - e^-m
        assert!((expected_alive(1.0, 0.07, 30.0, 0.0) - (1.0 - (-2.1f64).exp())).abs() < 1e-9);
        // cap 4, mean 3.6: 3.03 (sum of min(4, n) over the Poisson)
        assert!((expected_alive(4.0, 0.12, 30.0, 0.0) - 3.03).abs() < 0.01);
        // a 30 s fight from none, life 30: the mean over t of rate × t, capped; uncapped it is half of rate × life
        assert!((expected_alive(1000.0, 0.1, 30.0, 30.0) - 1.5).abs() < 1e-6);
        assert!(expected_alive(4.0, 0.12, 30.0, 30.0) < expected_alive(4.0, 0.12, 30.0, 0.0));
        assert_eq!(expected_alive(4.0, 0.0, 30.0, 60.0), 0.0);
    }
}

#[cfg(test)]
mod all_gems_tests {
    use super::*;

    /// Every gem changes some number once its situation is chosen (except the two with no combat effect).
    #[test]
    fn every_gem_simulates() {
        let b = ens_data::Bundle::load(concat!(env!("CARGO_MANIFEST_DIR"), "/../../data/bundle.json")).unwrap();
        let e = Engine::new(&b);
        let mut arch = crate::audit::archetypes(&b);
        let mut fire = arch.iter().find(|(n, _)| n == "staff").unwrap().1.clone();
        fire.ammo = build::find_item(&b, "Fireball IV");
        arch.push(("staff + Fireball".into(), fire));
        let mut bow = Build::sample_bow(&b);
        let wi = &b.items[&bow.weapon.as_ref().unwrap().item];
        if let Some(req) = wi.sequences.iter().find(|q| q.trigger == "SpecialAbility").and_then(|q| q.required_skill.clone()) {
            if let Some(i) = b.skills.iter().position(|sk| sk.id == fnv1a_hex(&req)) {
                bow.ranks[i] = b.skills[i].max.max(1) as u8;
            }
        }
        arch.push(("sample bow".into(), bow));
        let play = Play { sprint_start: true, evades_per_min: 10.0, air_share: 0.5, glides_per_min: 2.0, blocks_per_min: 10.0, night: true, elixir: true, summon_idle: 0.0, summon_attack_s: 0.0, summon_life_s: 0.0, fight_s: 0.0, heavy_reach_m: 0.0, zone_stay: 0.0, zone_cap: 0.0 };
        let mut missing = vec![];
        for (gid, gem) in &b.gems {
            if matches!(gem.debug.as_str(), "T3_Gem_GemOfOverpowering" | "T2_Gem_GemOfHopefulEmbers") {
                continue;
            }
            let mut any = false;
            for (_, base) in &arch {
                let mut x = base.clone();
                x.gem = Some(build::Equip { item: gid.clone(), rarity: None, level: Some(50), perks: None });
                for hk in [None, Some("Backstab"), Some("Sneak")] {
                    let t = Target {
                        pack_size: 4.0, pack_radius: 3.0, enemy_health: 3000.0, orb_pickup: 1.0, soaked: true, hit_kind: hk.map(|s| s.to_string()),
                        susceptibility: [("Fog".to_string(), 0.5)].into_iter().collect(), play: play.clone(), ..Default::default()
                    };
                    let with = e.total(&x, &t);
                    let without = e.total(base, &t);
                    let hit_w = e.incoming_target(&x, 50, &t).1;
                    let hit_0 = e.incoming_target(base, 50, &t).1;
                    if (with.total - without.total).abs() > 1e-6 || with.procs.len() != without.procs.len() || (hit_w - hit_0).abs() > 1e-9 {
                        any = true;
                    }
                }
                if any {
                    break;
                }
            }
            if !any {
                missing.push(gem.debug.clone());
            }
        }
        assert!(missing.is_empty(), "gems with no effect: {missing:?}");
    }
}
