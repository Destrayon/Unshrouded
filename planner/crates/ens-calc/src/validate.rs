//! Self-validation against the game's own tooltip numbers.
//!
//! * Skills: every decoded effect value at every rank must appear in that rank's tooltip (as shown, or ×100 when the
//!   tooltip is a percentage). Effects scaled by another attribute (e.g. "3 × Intelligence") are checked by their factor.
//! * Perks: every decoded effect value must match one of the perk's UI numbers.
//! Mismatches are listed with the decoded and shown numbers so each one can be explained.

use ens_data::{Bundle, Op, ValueSpec};

#[derive(Debug, Default)]
pub struct Report {
    pub skills_checked: usize,
    pub skill_ranks_checked: usize,
    pub skill_mismatches: Vec<String>,
    pub perks_checked: usize,
    pub perk_mismatches: Vec<String>,
    /// Effects whose value matches the tooltip only relative to the attribute's start value (absolute units).
    pub relative_matches: Vec<String>,
    /// Hidden effects: another effect of the same rank matches the tooltip, this one does not appear.
    pub hidden_effects: Vec<String>,
    pub triggered_checked: usize,
    pub triggered_missing: Vec<String>,
}

fn tooltip_numbers(text: &str) -> Vec<f64> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let flush = |cur: &mut String, out: &mut Vec<f64>| {
        if let Ok(v) = cur.trim_end_matches('.').parse::<f64>() {
            out.push(v);
        }
        cur.clear();
    };
    for ch in text.chars() {
        if ch.is_ascii_digit() || (ch == '.' && !cur.is_empty()) || (ch == '-' && cur.is_empty()) {
            cur.push(ch);
        } else {
            flush(&mut cur, &mut out);
        }
    }
    flush(&mut cur, &mut out);
    out.into_iter().filter(|v| *v != 0.0 || text.contains('0')).collect()
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-3 * a.abs().max(b.abs()).max(1.0)
}

fn matches(v: f64, nums: &[f64]) -> bool {
    let cands = [v, v.abs(), v * 100.0, v.abs() * 100.0, (v - 1.0) * 100.0, (1.0 - v) * 100.0];
    nums.iter().any(|n| cands.iter().any(|c| close(*c, n.abs())))
}

/// Per-unit factor of a value spec (sources other than Level count as 1, so "3 × Intelligence" checks 3).
fn eval_rank(v: &ValueSpec, rank: u32) -> f64 {
    v.eval(&|a| if a == "Level" { rank as f64 } else { 1.0 })
}

pub fn run(b: &Bundle) -> Report {
    let mut r = Report::default();
    let mut seen = std::collections::HashSet::new();
    for s in &b.skills {
        if s.effects.is_empty() || s.desc.is_empty() || !seen.insert(s.name.clone()) {
            continue;
        }
        r.skills_checked += 1;
        for rank in 1..=s.max.max(1) {
            let Some(text) = s.desc.get(rank as usize - 1).or(s.desc.last()) else { continue };
            let nums = tooltip_numbers(text);
            r.skill_ranks_checked += 1;
            let effs: Vec<_> = s.effects.iter().filter(|e| e.when.is_empty() && e.value.is_some()).collect();
            let any_match = effs.iter().any(|e| matches(eval_rank(e.value.as_ref().unwrap(), rank), &nums));
            for e in effs {
                let Some(v) = &e.value else { continue };
                let val = eval_rank(v, rank);
                if val == 0.0 {
                    continue;
                }
                let base = e.attribute.as_ref().and_then(|a| b.player_base.get(a)).and_then(|x| x.as_f64()).unwrap_or(0.0);
                if !matches(val, &nums) && base != 0.0 && matches(val / base, &nums) {
                    r.relative_matches.push(format!("{} rank {rank}: {} +{val} on start {base} = {:.1}%", s.name, e.attribute.as_deref().unwrap_or("?"), val / base * 100.0));
                    continue;
                }
                if !matches(val, &nums) && any_match {
                    r.hidden_effects.push(format!("{} rank {rank}: {} {:?} {val:.4} (not shown in tooltip)", s.name, e.attribute.as_deref().unwrap_or("?"), e.operation));
                    continue;
                }
                if !matches(val, &nums) {
                    r.skill_mismatches.push(format!(
                        "{} rank {rank}: {} {:?} {val:.4} not in tooltip {:?} — \"{}\"",
                        s.name,
                        e.attribute.as_deref().unwrap_or("?"),
                        e.operation,
                        nums,
                        text.replace('\n', " ").chars().take(140).collect::<String>()
                    ));
                }
            }
        }
    }
    // triggered / flag skills: every tooltip number should appear among the program's numeric inputs at that rank
    let mut seen2 = std::collections::HashSet::new();
    for s in &b.skills {
        if s.program.is_empty() || !s.effects.iter().all(|e| !e.when.is_empty()) && !s.effects.is_empty() || !seen2.insert(s.name.clone()) {
            continue;
        }
        if s.desc.is_empty() {
            continue;
        }
        r.triggered_checked += 1;
        for rank in 1..=s.max.max(1) {
            let Some(text) = s.desc.get(rank as usize - 1).or(s.desc.last()) else { continue };
            let mut vals = vec![];
            for call in &s.program {
                for v in call.inputs.values() {
                    if let Ok(spec) = serde_json::from_value::<ValueSpec>(v.clone()) {
                        let x = eval_rank(&spec, rank);
                        if x != 0.0 {
                            vals.push(x);
                        }
                    }
                }
            }
            vals.extend(s.closure.iter().map(|(_, v)| *v));
            for n in tooltip_numbers(text) {
                let ok = vals.iter().any(|v| [*v, v.abs(), v * 100.0, v.abs() * 100.0, v / 1e9].iter().any(|c| close(*c, n)));
                if !ok {
                    r.triggered_missing.push(format!("{} rank {rank}: tooltip number {n} not among program inputs — \"{}\"", s.name, text.replace('\n', " ").chars().take(120).collect::<String>()));
                }
            }
        }
    }
    for p in b.perks.values() {
        if p.effects.is_empty() || p.ui.is_empty() {
            continue;
        }
        r.perks_checked += 1;
        let nums: Vec<f64> = p.ui.iter().filter_map(|u| u.get("value").and_then(|x| x.as_f64())).collect();
        for e in p.effects.iter().filter(|e| e.when.is_empty()) {
            let Some(v) = &e.value else { continue };
            let val = eval_rank(v, 1);
            if val == 0.0 || matches!(e.operation, Op::Mul | Op::Div) && close(val, 1.0) {
                continue;
            }
            if !matches(val, &nums) {
                r.perk_mismatches.push(format!("{} ({}): {} {:?} {val:.4} vs UI {:?}", p.name, p.debug.as_deref().unwrap_or(""), e.attribute.as_deref().unwrap_or("?"), e.operation, nums));
            }
        }
    }
    r
}
