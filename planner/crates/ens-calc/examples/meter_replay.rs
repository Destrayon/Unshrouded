//! Replay a combat log through the live meter and print what it would show (players, your sources, names):
//! `cargo run --release -p ens-calc --example meter_replay -- <combat-*.csv>`.

use ens_calc::meter::Meter;

fn main() {
    let path = std::env::args().nth(1).expect("usage: meter_replay <combat-*.csv>");
    let text = std::fs::read_to_string(&path).expect("read the log");
    let b = ens_data::Bundle::load(ens_data::default_bundle_path()).expect("bundle");
    let mut m = Meter::default();
    // ENS_REPLAY_CHUNK=<bytes>: feed it in pieces, as the planner reads a growing log
    let chunk: usize = std::env::var("ENS_REPLAY_CHUNK").ok().and_then(|v| v.parse().ok()).unwrap_or(usize::MAX);
    let bytes = text.as_bytes();
    for piece in bytes.chunks(chunk.max(1)) {
        m.feed(&String::from_utf8_lossy(piece));
    }
    let v = m.view(&b, u64::MAX / 2);
    println!("me {:?}, rows {}", v.me, v.rows);
    for h in &v.history {
        println!("past fight: {} · {:.0} dmg · {:.1} s", h.label, h.damage, h.seconds);
    }
    if let Some(f) = v.current {
        println!("fight: {} · {:.0} dmg · {:.1} s", f.label, f.damage, f.seconds);
        for s in &f.sources {
            println!("  your source {}: {:.0}", s.name, s.damage);
        }
        for p in &f.players {
            println!("  player {} (#{}{}): {:.0}", p.name, p.id, if p.me { ", you" } else { "" }, p.damage);
            for w in &p.weapons {
                println!("    {}: {:.0}", w.name, w.damage);
                for r in &w.rows {
                    println!("      {}: {:.0} ({} hits)", r.name, r.damage, r.hits);
                }
            }
        }
    }
}
