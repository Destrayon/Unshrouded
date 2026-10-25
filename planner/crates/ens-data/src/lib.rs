//! Enshrouded 1.0 game data: typed model of the exported bundle (`planner/data/bundle.json`).
//!
//! The bundle is produced by `kfc/export_bundle.py` from the game's KFC data. Effects are decoded from the game's own
//! impact programs: each effect changes one named attribute (e.g. `WeaponCategoryDamageMod_Bow`) by a value that is a
//! constant, a scaled value (factor x source attribute such as the skill's `Level`), another attribute, or an expression.

pub mod save;
pub mod skill_point_unlocks;

use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Op {
    Add,
    Sub,
    Mul,
    Div,
}

/// How an effect's value is computed.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ValueSpec {
    Const { value: Value },
    Scaled {
        factor: f64,
        #[serde(rename = "fn")]
        function: String,
        #[serde(default)]
        base: Value,
        source: Option<String>,
        #[serde(rename = "sourceEntity")]
        source_entity: Option<String>,
    },
    Attribute { value: Option<String> },
    Expr { op: String, args: Vec<ValueSpec> },
}

impl ValueSpec {
    /// Evaluate with a lookup for source attributes (e.g. `Level` -> skill rank, `Intelligence` -> attribute total).
    pub fn eval(&self, lookup: &dyn Fn(&str) -> f64) -> f64 {
        match self {
            ValueSpec::Const { value } => value.as_f64().unwrap_or(0.0),
            ValueSpec::Scaled { factor, function, base, source, .. } => {
                let x = source.as_deref().map(lookup).unwrap_or(0.0);
                let b = base.as_f64().unwrap_or(0.0);
                // exe 0x140e97300 (keen::ScaledImpactValue): None = value, Linear = value + factor × input,
                // Percentage = factor × input, WeaponPowerCreep = value + powerCreepWeapon × factor × input
                match function.as_str() {
                    "None" => b,
                    "Percentage" => factor * x,
                    "WeaponPowerCreep" => b + lookup("$powerCreepWeapon") * factor * x,
                    _ => b + factor * x,
                }
            }
            ValueSpec::Attribute { value } => value.as_deref().map(lookup).unwrap_or(0.0),
            ValueSpec::Expr { op, args } => {
                let v: Vec<f64> = args.iter().map(|a| a.eval(lookup)).collect();
                match op.as_str() {
                    "mul" => v.iter().product(),
                    "add" => v.iter().sum(),
                    "sub" => v.first().copied().unwrap_or(0.0) - v.iter().skip(1).sum::<f64>(),
                    "div" => v.first().copied().unwrap_or(0.0) / v.get(1).copied().unwrap_or(1.0),
                    _ => 0.0,
                }
            }
        }
    }
}

/// A branch condition guarding an effect (from the program's control flow).
#[derive(Debug, Clone, Deserialize)]
pub struct Condition {
    pub node: String,
    pub is: bool,
    /// For IsSkillUnlocked: the skill node id.
    pub skill: Option<u32>,
    #[serde(default)]
    pub inputs: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Effect {
    pub node: String,
    pub attribute: Option<String>,
    pub operation: Op,
    pub value: Option<ValueSpec>,
    /// Alternatives (OR) of condition lists (AND); empty = unconditional.
    #[serde(default)]
    pub when: Vec<Vec<Condition>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProgramCall {
    pub node: String,
    #[serde(default)]
    pub inputs: HashMap<String, Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SkillNode {
    pub index: usize,
    pub id: u32,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub class: String,
    pub cost: u32,
    pub max: u32,
    pub x: f64,
    pub y: f64,
    #[serde(default)]
    pub links: Vec<u32>,
    #[serde(default)]
    pub effects: Vec<Effect>,
    #[serde(default)]
    pub program: Vec<ProgramCall>,
    pub unparsed: Option<String>,
    #[serde(default)]
    pub triggered: bool,
    /// Tooltip text per rank (game loca with values filled in).
    #[serde(default)]
    pub desc: Vec<String>,
    /// Numbers reachable from a triggered program: applied buffs and spawned templates (label, value).
    #[serde(default)]
    pub closure: Vec<(String, f64)>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WeaponCategory {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub class: Option<String>,
    #[serde(rename = "damageAttribute")]
    pub damage_attribute: Option<String>,
    #[serde(rename = "durabilityMod")]
    pub durability_mod: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Perk {
    pub name: String,
    pub debug: Option<String>,
    #[serde(default)]
    pub effects: Vec<Effect>,
    #[serde(default)]
    pub calls: Vec<String>,
    /// Decoded impact program when the perk does more than plain attribute changes (on-hit, on-kill triggers…).
    #[serde(default)]
    pub program: Option<Vec<ProgramCall>>,
    pub damage: Option<HashMap<String, f64>>,
    pub armor: Option<HashMap<String, f64>>,
    #[serde(rename = "skillLevel")]
    pub skill_level: Option<Value>,
    /// Tooltip numbers (value + format) shown on the perk.
    #[serde(default)]
    pub ui: Vec<Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LevelRange {
    #[serde(rename = "minLevel")]
    pub min: u32,
    #[serde(rename = "maxLevel")]
    pub max: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DamageSetup {
    #[serde(rename = "speedMod")]
    pub speed_mod: f64,
    #[serde(rename = "dmgMod")]
    pub dmg_mod: f64,
    pub distribution: HashMap<String, Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ItemSequence {
    pub trigger: String,
    pub sequence: String,
    #[serde(rename = "requiredSkill")]
    pub required_skill: Option<String>,
    pub cost: Option<String>,
    /// Entity tags this sequence runs with (selects tag-gated spawn variants).
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ItemBuff {
    pub id: Option<u32>,
    pub name: Option<String>,
    pub slot: Option<String>,
    #[serde(rename = "lifetimeS")]
    pub lifetime_s: Option<f64>,
    #[serde(default)]
    pub effects: Vec<Effect>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Item {
    pub name: String,
    #[serde(default)]
    pub desc: String,
    pub category: Option<String>,
    pub slot: Option<String>,
    pub ammo: Option<String>,
    pub rarity: Option<String>,
    #[serde(rename = "generateRarity")]
    pub generate_rarity: Option<bool>,
    pub class: Option<String>,
    #[serde(rename = "weaponCategory")]
    pub weapon_category: Option<String>,
    pub levels: Option<LevelRange>,
    pub damage: Option<DamageSetup>,
    pub armor: Option<Value>,
    pub block: Option<f64>,
    #[serde(default)]
    pub perks: Vec<String>,
    #[serde(default)]
    pub sequences: Vec<ItemSequence>,
    pub buff: Option<ItemBuff>,
    /// Tooltip numbers (value + format).
    #[serde(default)]
    pub ui: Vec<Value>,
    /// Projectile / hit template spawned when the item is used (arrows, spells, summons).
    pub projectile: Option<Value>,
    /// Combat config values the item's attack programs read (kfc/export_item_combat_cfg.py).
    #[serde(default)]
    pub combat: Option<ItemCombat>,
    /// Numeric impact config values by configId (e.g. 1236200858 = SpecialAbilityGain, Focus per hit).
    #[serde(default)]
    pub configs: HashMap<String, Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Buff {
    pub name: String,
    pub slot: Option<String>,
    /// keen::BuffApplyType: ResetTime / Replace / ReplaceAtEnd / Invalid (what a re-apply does while it runs).
    #[serde(rename = "applyType", default)]
    pub apply_type: Option<String>,
    #[serde(rename = "lifetimeS")]
    pub lifetime_s: f64,
    #[serde(default)]
    pub effects: Vec<Effect>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AttributeContainer {
    #[serde(rename = "type")]
    pub kind: u32,
    pub names: Vec<String>,
    pub ids: Vec<u32>,
    pub structure: Vec<Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Bundle {
    pub meta: Value,
    pub balancing: Value,
    pub attributes: Vec<AttributeContainer>,
    #[serde(rename = "weaponCategories")]
    pub weapon_categories: HashMap<String, WeaponCategory>,
    pub perks: HashMap<String, Perk>,
    pub items: HashMap<String, Item>,
    pub buffs: HashMap<String, Buff>,
    pub skills: Vec<SkillNode>,
    #[serde(rename = "skillLinks")]
    pub skill_links: Vec<Value>,
    pub enemies: Vec<Value>,
    pub factions: Value,
    #[serde(rename = "rarityDropRates")]
    pub rarity_drop_rates: Option<Vec<f64>>,
    /// Start values of every attribute input on the player template (`1_Player`).
    #[serde(rename = "playerBase", default)]
    pub player_base: HashMap<String, Value>,
    /// Attack sequences: guid -> spawns/impacts/timing (see kfc/export_attacks.py).
    #[serde(default)]
    pub sequences: HashMap<String, Value>,
    /// Hostile enemy templates by debug name (kfc/enemy_types.py).
    #[serde(rename = "enemyTypes", default)]
    pub enemy_types: std::collections::BTreeMap<String, EnemyType>,
    /// Weapon gems by item GUID (kfc/export_gems.py).
    #[serde(default)]
    pub gems: HashMap<String, Gem>,
    /// Summon templates (PlayerAlly faction) by debug name, same shape as enemyTypes.
    #[serde(rename = "allyTypes", default)]
    pub ally_types: std::collections::BTreeMap<String, EnemyType>,
    /// How the Explosive Arrow blast and Echoing Explosives price their hits (kfc/export_explosive.py).
    #[serde(rename = "explosiveArrow", default)]
    pub explosive_arrow: Option<ExplosiveArrow>,
    /// How the bow's Strong Shot prices its hit (kfc/export_strong_shot.py).
    #[serde(rename = "strongShot", default)]
    pub strong_shot: Option<StrongShot>,
    /// How each template a weapon special spawns prices its hit (kfc/export_specials.py), by template name.
    #[serde(rename = "specialTemplates", default)]
    pub special_templates: HashMap<String, SpecialTemplate>,
    /// FNV-1a 64 of the bundle file's text: changes whenever any game value in it changes.
    #[serde(skip)]
    pub content_hash: u64,
}

/// From a special template's impact program: damage from the weapon base (`base_damage` types) or else the
/// inherited item damage; AttackMod = attack_mod × (offset + attack_variation × r); its attack category.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpecialTemplate {
    #[serde(default)]
    pub program: Option<u32>,
    #[serde(default)]
    pub base_damage: Option<Vec<String>>,
    #[serde(default)]
    pub attack_mod: Option<f64>,
    #[serde(default)]
    pub offset: Option<f64>,
    #[serde(default)]
    pub attack_variation: Option<f64>,
    #[serde(default)]
    pub attack_category: Option<Vec<String>>,
}

/// From Strong Shot's impact program: raw damage = the generic weapon base at the bow's item level
/// (GetWeaponBaseDamage) in `damage_types`, AttackMod = attack_mod × (offset + attack_variation × r), r uniform in [0, 1).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StrongShot {
    pub program: u32,
    pub damage_types: Vec<String>,
    pub offset: f64,
    pub attack_variation: f64,
    pub attack_mod: f64,
    #[serde(default)]
    pub template: Option<String>,
}

/// From the blast's impact program: AttackMod = shrapnel_base + shrapnel_per_target × (collisions in the blast)
/// with Shrapnel Blast unlocked; with Echoing Explosives, each hit target has `echo_chance` of a second explosion at
/// AttackMod `echo_attack_mod` within `echo_radius` m. Skill ids are the bundle's skill ids.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExplosiveArrow {
    pub blast_program: u32,
    pub shrapnel_skill: u32,
    pub shrapnel_base: f64,
    pub shrapnel_per_target: f64,
    pub echo_skill: u32,
    pub echo_chance: f64,
    pub echo_attack_mod: f64,
    pub echo_program: u32,
    pub echo_radius: f64,
}

/// An item's combat configs: DamageScaleAttribute (config 1395916278), attack categories (606890942), AttackMod
/// (2236524794), as the item's impactValues give them.
#[derive(Debug, Clone, Deserialize)]
pub struct ItemCombat {
    pub scale: Option<String>,
    #[serde(default)]
    pub categories: Option<Vec<String>>,
    #[serde(rename = "attackMod")]
    pub attack_mod: Option<f64>,
}

/// One enemy template's combat facts: keen::FactionType, keen::enemy01::EnemyClass (Melee/Ranger/Mage), whether it
/// can fly, and its DamageSusceptibility values (DS, Physical, Cut, Pierce, Blunt, …, Fire, Fog, Ice, Lightning).
#[derive(Debug, Clone, Deserialize)]
pub struct EnemyType {
    pub faction: String,
    pub class: String,
    #[serde(default)]
    pub flying: bool,
    #[serde(default)]
    pub boss: bool,
    #[serde(default)]
    pub susceptibility: HashMap<String, f64>,
    /// CombatFlags.canBeBackstabbed: hits from behind can be backstabs.
    #[serde(default)]
    pub backstab: bool,
    /// DamageMod = Base × Multiplier + Adder from the template (default 1).
    #[serde(default = "one", rename = "damageMod")]
    pub damage_mod: f64,
    #[serde(default)]
    pub strength: f64,
    /// EnemyScaling.enemyLevelScaling.healthScaleFactor: max health at a level is `ens_calc::enemy_health(level, this)`.
    #[serde(default, rename = "healthFactor")]
    pub health_factor: Option<f64>,
    /// DamageSusceptibility_SkillShot of its weak-spot sub-entities (the highest; they copy the rest of the
    /// enemy's susceptibility, CopyParentDamageSusceptibility.keepSkillShotAttribute). None: no weak spot.
    #[serde(default, rename = "weakSpot")]
    pub weak_spot: Option<f64>,
    /// Attached parts with their own health and susceptibility (no CopyParentDamageSusceptibility), such as a
    /// Fogger's core. A hit on one is logged with targetId != rootTargetId and the part's max health.
    #[serde(default)]
    pub hitables: Vec<Hitable>,
    /// Arsenal attacks with the hits they spawn (SpawnImpact or projectile ImpactConfiguration damage shares).
    #[serde(default)]
    pub attacks: Vec<EnemyAttack>,
    /// Shots its own impact program spawns (totems, wisps): the shot template's damage shares.
    #[serde(default)]
    pub shots: Vec<EnemyHit>,
    /// Template GUID (allyTypes only).
    #[serde(default)]
    pub guid: Option<String>,
    /// In-game name from the spawn table, or its base template's for a variant (kfc/enemy_names.py).
    #[serde(default, rename = "displayName")]
    pub display_name: Option<String>,
    /// The spawn table names this template itself (not a variant that took its base's name).
    #[serde(default, rename = "displayNameOwn")]
    pub display_name_own: bool,
    /// Seconds a summon lives before it despawns: keen::ecs::LifeTime.lifeTime (allyTypes only).
    #[serde(default, rename = "lifetimeS")]
    pub lifetime_s: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Hitable {
    pub name: String,
    pub health: f64,
    #[serde(default)]
    pub susceptibility: HashMap<String, f64>,
}

fn one() -> f64 {
    1.0
}

/// A weapon gem: its program's config values (ItemInfo.impactValues).
#[derive(Debug, Clone, Deserialize)]
pub struct Gem {
    #[serde(default)]
    pub debug: String,
    /// configId → scaled value {value, factor, fn, int}
    #[serde(default)]
    pub scaled: HashMap<String, GemScaled>,
    /// configId → plain value (numbers, times in seconds, GUID strings)
    #[serde(default)]
    pub simple: HashMap<String, Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GemScaled {
    #[serde(default)]
    pub value: Value,
    #[serde(default)]
    pub factor: f64,
    #[serde(rename = "fn", default)]
    pub function: String,
    #[serde(default)]
    pub int: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EnemyAttack {
    #[serde(default)]
    pub sequence: String,
    /// The attack sequence's length and the arsenal's selectionChance.
    #[serde(rename = "lengthS", default)]
    pub length_s: f64,
    #[serde(default = "one")]
    pub chance: f64,
    #[serde(default)]
    pub hits: Vec<EnemyHit>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EnemyHit {
    /// damage-type key (physicalCutDamage, …) → share of the enemy damage value
    #[serde(default)]
    pub shares: HashMap<String, f64>,
    #[serde(default)]
    pub category: Option<Vec<String>>,
    #[serde(default = "one", rename = "attackMod")]
    pub attack_mod: f64,
    #[serde(default)]
    pub source: String,
}

impl Bundle {
    pub fn from_json(text: &str) -> Result<Self, String> {
        let mut b: Bundle = serde_json::from_str(text).map_err(|e| format!("parse bundle: {e}"))?;
        b.content_hash = text.bytes().fold(0xcbf29ce484222325u64, |h, x| (h ^ x as u64).wrapping_mul(0x100000001b3));
        Ok(b)
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Self, String> {
        let text = std::fs::read_to_string(path.as_ref()).map_err(|e| format!("read {}: {e}", path.as_ref().display()))?;
        Self::from_json(&text)
    }

    /// Balancing table value by key (e.g. `critChanceBase`).
    pub fn balance(&self, key: &str) -> Option<f64> {
        self.balancing.get(key).and_then(|v| v.as_f64())
    }
}

/// Default bundle location: `<workspace>/data/bundle.json`.
pub fn default_bundle_path() -> std::path::PathBuf {
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    here.join("../../data/bundle.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_bundle() {
        let b = Bundle::load(default_bundle_path()).expect("bundle");
        assert!(b.skills.len() > 200);
        assert!(b.items.len() > 4000);
        let marksman = b.skills.iter().find(|s| s.name == "Marksman").unwrap();
        let e = &marksman.effects[0];
        assert_eq!(e.attribute.as_deref(), Some("WeaponCategoryDamageMod_Bow"));
        let v = e.value.as_ref().unwrap().eval(&|a| if a == "Level" { 3.0 } else { 0.0 });
        assert!((v - 0.15).abs() < 1e-9);
    }
}
