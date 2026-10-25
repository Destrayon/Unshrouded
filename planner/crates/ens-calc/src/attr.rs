//! Attribute system: the game's own attribute containers and formulas (keen::AttributeOps bytecode).
//!
//! Each container is a tree of named nodes (e.g. `CritChance`, `CritChance_Base`, ...). Nodes with a calculation are
//! derived by a small stack program; the rest are inputs whose start values come from the player template (`1_Player`)
//! and are changed by effects (skills, perks, food) with add/sub/mul/div.

use ens_data::{Bundle, Op};
use std::cell::RefCell;
use std::collections::HashMap;

#[derive(Debug, Clone)]
struct Node {
    container: usize,
    index: usize,
}

#[derive(Debug, Clone)]
struct Container {
    names: Vec<String>,
    calcs: Vec<Vec<u32>>,
}

/// One change to an input attribute, kept for breakdowns.
#[derive(Debug, Clone)]
pub struct Contribution {
    pub attribute: String,
    pub source: String,
    pub op: Op,
    pub value: f64,
}

#[derive(Debug, Clone)]
pub struct AttrSystem {
    containers: Vec<Container>,
    nodes: HashMap<String, Node>,
    base: HashMap<String, f64>,
}

impl AttrSystem {
    pub fn new(bundle: &Bundle) -> Self {
        let mut containers = Vec::new();
        let mut nodes = HashMap::new();
        for (ci, c) in bundle.attributes.iter().enumerate() {
            let calcs = c
                .structure
                .iter()
                .map(|n| {
                    n.get("calculation")
                        .and_then(|v| v.as_array())
                        .map(|a| a.iter().filter_map(|x| x.as_u64()).map(|x| x as u32).collect())
                        .unwrap_or_default()
                })
                .collect();
            for (i, n) in c.names.iter().enumerate() {
                nodes.entry(n.clone()).or_insert(Node { container: ci, index: i });
            }
            containers.push(Container { names: c.names.clone(), calcs });
        }
        let base = bundle
            .player_base
            .iter()
            .filter_map(|(k, v)| v.as_f64().map(|f| (k.clone(), f)))
            .collect();
        AttrSystem { containers, nodes, base }
    }

    pub fn knows(&self, name: &str) -> bool {
        self.nodes.contains_key(name)
    }

    pub fn base(&self, name: &str) -> f64 {
        self.base.get(name).copied().unwrap_or(0.0)
    }

    /// Start a fresh attribute state from the player template values.
    pub fn state(&self) -> AttrState<'_> {
        AttrState { sys: self, inputs: HashMap::new(), contributions: Vec::new(), cache: RefCell::new(HashMap::new()) }
    }
}

/// Inputs changed by effects, with derived values evaluated on demand.
pub struct AttrState<'a> {
    sys: &'a AttrSystem,
    inputs: HashMap<String, (f64, f64, f64)>, // (adds - subs, product of mul/div, overrides count unused)
    pub contributions: Vec<Contribution>,
    cache: RefCell<HashMap<String, f64>>,
}

impl<'a> AttrState<'a> {
    pub fn apply(&mut self, attribute: &str, op: Op, value: f64, source: &str) {
        let e = self.inputs.entry(attribute.to_string()).or_insert((0.0, 1.0, 0.0));
        match op {
            Op::Add => e.0 += value,
            Op::Sub => e.0 -= value,
            Op::Mul => e.1 *= value,
            Op::Div => {
                if value != 0.0 {
                    e.1 /= value
                }
            }
        }
        self.contributions.push(Contribution { attribute: attribute.to_string(), source: source.to_string(), op, value });
        self.cache.borrow_mut().clear();
    }

    /// Raw input value: template base, plus adds, times multipliers.
    pub fn raw(&self, name: &str) -> f64 {
        let b = self.sys.base(name);
        match self.inputs.get(name) {
            Some((add, mul, _)) => (b + add) * mul,
            None => b,
        }
    }

    /// Final value of an attribute (evaluates its formula if it has one).
    pub fn get(&self, name: &str) -> f64 {
        if let Some(v) = self.cache.borrow().get(name) {
            return *v;
        }
        let v = match self.sys.nodes.get(name) {
            Some(n) => {
                let calc = &self.sys.containers[n.container].calcs[n.index];
                if calc.is_empty() { self.raw(name) } else { self.eval(n.container, n.index, calc, 0) }
            }
            None => self.raw(name),
        };
        self.cache.borrow_mut().insert(name.to_string(), v);
        v
    }

    fn node_value(&self, container: usize, index: usize, current: usize, depth: usize) -> f64 {
        let c = &self.sys.containers[container];
        let Some(name) = c.names.get(index) else { return 0.0 };
        if index == current || c.calcs[index].is_empty() || depth > 8 {
            self.raw(name)
        } else {
            self.eval(container, index, &c.calcs[index], depth + 1)
        }
    }

    fn eval(&self, container: usize, current: usize, calc: &[u32], depth: usize) -> f64 {
        let mut st: Vec<f64> = Vec::new();
        let mut i = 0;
        while i < calc.len() {
            let w = calc[i];
            let (op, arg) = (w >> 16, (w & 0xffff) as usize);
            i += 1;
            let pop = |st: &mut Vec<f64>| st.pop().unwrap_or(0.0);
            match op {
                1 => st.push(0.0),                                             // Load0
                2 => st.push(self.node_value(container, arg, current, depth)), // Load
                3 => {
                    let c = &self.sys.containers[container];
                    st.push(c.names.get(arg).map(|n| self.raw(n)).unwrap_or(0.0)) // LoadRef
                }
                5 => {
                    // Push: next word is a literal
                    let lit = calc.get(i).copied().unwrap_or(0) as i32 as f64;
                    i += 1;
                    st.push(lit);
                }
                6 => { let b = pop(&mut st); let a = pop(&mut st); st.push(a + b) }
                7 => { let b = pop(&mut st); let a = pop(&mut st); st.push(a - b) }
                8 => { let b = pop(&mut st); let a = pop(&mut st); st.push(a * b) }
                9 => { let b = pop(&mut st); let a = pop(&mut st); st.push(if b != 0.0 { a / b } else { 0.0 }) }
                10 => { let b = pop(&mut st); let a = pop(&mut st); st.push(a.powf(b)) }
                11 => { let b = pop(&mut st); let a = pop(&mut st); st.push(a * b / 100.0) }
                12 => {
                    let hi = pop(&mut st); let lo = pop(&mut st); let v = pop(&mut st);
                    st.push(v.max(lo).min(hi.max(lo)))
                }
                13 => {
                    // ScaleToNewMax: cur, min, oldMax, newMax -> newMax
                    let new_max = pop(&mut st); let _old = pop(&mut st); let _min = pop(&mut st); let _cur = pop(&mut st);
                    st.push(new_max)
                }
                14 => { let b = pop(&mut st); let a = pop(&mut st); st.push(a.min(b)) }
                15 => { let b = pop(&mut st); let a = pop(&mut st); st.push(a.max(b)) }
                21 => break,
                _ => {}
            }
        }
        st.pop().unwrap_or(0.0)
    }

    /// Contributions to one attribute (for the breakdown view).
    pub fn sources(&self, attribute: &str) -> Vec<&Contribution> {
        self.contributions.iter().filter(|c| c.attribute == attribute).collect()
    }
}
