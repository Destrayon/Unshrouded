//! Plain-language names and value formats for the engine's attribute keys, so the UI never shows
//! `WeaponCategoryDamageMod_Bow` where a player expects "Bow damage". The raw key stays available in tooltips.

fn qualifier(q: &str) -> String {
    match q {
        "Single" => "One-handed".into(),
        "TwoHand" => "Two-handed".into(),
        "GreatSword" => "Greatsword".into(),
        "Unarmed" => "Unarmed".into(),
        "Vs" => "vs".into(),
        _ => q.replace("Soaked", "soaked").replace('_', " "),
    }
}

fn words(camel: &str) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    let mut cur = String::new();
    for c in camel.chars() {
        if c.is_uppercase() && !cur.is_empty() && !cur.chars().last().map(|p| p.is_uppercase()).unwrap_or(false) {
            out.push(std::mem::take(&mut cur));
        }
        cur.push(c);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// A player-facing name for an attribute key.
pub fn attr_label(key: &str) -> String {
    let fixed = match key {
        "Health" | "Health_Max_Base" => "Health",
        "Health_Max_Adder" => "Max health",
        "Mana" | "Mana_Max_Base" => "Mana",
        "Mana_Max_Adder" => "Max mana",
        "Stamina" | "Stamina_Max_Base" => "Stamina",
        "Stamina_Max_Adder" => "Max stamina",
        "Health_Recharge" => "Health regeneration",
        "Mana_Recharge" => "Mana regeneration",
        "Stamina_Recharge" => "Stamina regeneration",
        "Mana regen /s" => "Mana regen per second",
        "Stamina regen /s" => "Stamina regen per second",
        "CritChance" | "CritChance_Adder" => "Crit chance",
        "CritChance_Max" => "Crit chance cap",
        "CritDamageMod" | "CritDamageMod_Adder" => "Crit damage",
        "DamageMod" => "Damage",
        "RangedDamageMod" => "Ranged damage",
        "MeleeDamageMod" => "Melee damage",
        "MagicDamageMod" => "Magic damage",
        "ArmorPoints_Physical" => "Physical armour",
        "ArmorPoints_Magical" => "Magical armour",
        "MaxTotemCount" => "Max totems",
        "MaxSkeletonSpellCount" => "Max skeletons",
        "Movement_Sprint" => "Sprint speed",
        "ManaCostMod" => "Mana cost",
        "ChargeDurationMod" => "Charge time",
        "AttackSpeedMod" => "Attack speed",
        "DamageModSneak" => "Sneak attack damage",
        "DamageModMerciless" => "Merciless attack damage",
        _ => "",
    };
    if !fixed.is_empty() {
        return fixed.into();
    }
    let k = key.replace("_Adder", "").replace("_Base", "").replace("_Max", " Max");
    if let Some(q) = k.strip_prefix("WeaponCategoryDamageMod_") {
        return format!("{} damage", qualifier(q));
    }
    if let Some(q) = k.strip_prefix("DamageModAgainst_") {
        return format!("Damage against {}", qualifier(q).to_lowercase());
    }
    if let Some(q) = k.strip_prefix("DamageSusceptibility_") {
        return format!("{} damage taken", qualifier(q));
    }
    if let Some(q) = k.strip_prefix("ArmorPoints_") {
        return format!("{} armour", qualifier(q));
    }
    let mut parts = k.split('_');
    let head = parts.next().unwrap_or("");
    let quals: Vec<String> = parts.map(qualifier).collect();
    let base: Vec<String> = words(head).into_iter().filter(|w| w != "Mod").map(|w| w.to_lowercase()).collect();
    let base = base.join(" ");
    if quals.is_empty() {
        capitalize(&base)
    } else {
        capitalize(&format!("{} {}", quals.join(" ").to_lowercase(), base))
    }
}

/// Whether an attribute is a multiplier around 1 (shown as ±%) rather than a count.
pub fn is_ratio(key: &str) -> bool {
    key.contains("Mod") || key.contains("Susceptibility") || key.contains("Chance")
}

/// The attribute's current value as a player reads it.
pub fn attr_value(key: &str, v: f64, base: f64) -> String {
    if key.contains("Chance") {
        format!("{}%", trim_num(v * 100.0))
    } else if is_ratio(key) && (base - 1.0).abs() < 1e-9 {
        format!("{:+}%", trim_num_signed((v - 1.0) * 100.0))
    } else if key.contains("regen /s") {
        format!("{v:.2} /s")
    } else {
        trim_num(v)
    }
}

/// One contribution (Add / Sub / Mul / …) as a player reads it.
pub fn contribution(key: &str, op: &str, v: f64) -> String {
    let pct = is_ratio(key) && v.abs() < 5.0;
    match op {
        "Add" if pct => format!("+{}%", trim_num(v * 100.0)),
        "Sub" if pct => format!("−{}%", trim_num(v * 100.0)),
        "Add" => format!("+{}", trim_num(v)),
        "Sub" => format!("−{}", trim_num(v)),
        "Mul" => format!("×{}", trim_num(v)),
        "Div" => format!("÷{}", trim_num(v)),
        o => format!("{o} {}", trim_num(v)),
    }
}

pub fn trim_num(v: f64) -> String {
    if (v - v.round()).abs() < 1e-9 {
        format!("{v:.0}")
    } else if v.abs() < 1.0 {
        format!("{v:.3}").trim_end_matches('0').to_string()
    } else {
        format!("{v:.2}").trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

fn trim_num_signed(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// Which character-sheet group an attribute belongs to.
pub fn group(key: &str) -> &'static str {
    if ["Strength", "Dexterity", "Intelligence", "Constitution", "Endurance", "Spirit"].contains(&key) {
        "Attributes"
    } else if key.starts_with("Health") || key.starts_with("Mana") && !key.contains("Cost") || key.starts_with("Stamina") {
        "Resources"
    } else if key.contains("Armor") || key.contains("Susceptibility") || key.contains("Resist") || key.contains("Block") {
        "Defence"
    } else if key.contains("Damage") || key.contains("Crit") || key.contains("Attack") || key.contains("Charge") || key.contains("Cost") {
        "Offence"
    } else {
        "Other"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_read_like_words() {
        assert_eq!(attr_label("WeaponCategoryDamageMod_Bow"), "Bow damage");
        assert_eq!(attr_label("CritChance_Adder_Ranged"), "Ranged crit chance");
        assert_eq!(attr_label("DamageModAgainst_Flying"), "Damage against flying");
        assert_eq!(attr_label("DamageSusceptibility_Physical"), "Physical damage taken");
        assert_eq!(attr_label("RangedDamageMod"), "Ranged damage");
        assert_eq!(attr_value("RangedDamageMod", 1.24, 1.0), "+24%");
        assert_eq!(attr_value("CritChance", 0.12, 0.1), "12%");
    }
}
