//! `ens-inject`            load ens_hook.dll (next to this exe) into the running enshrouded.exe
//! `ens-inject --check X`  check that exe X contains the hook signature exactly once (no game needed)

use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["--check", exe] => check(exe),
        [] => inject(),
        _ => {
            eprintln!("usage: ens-inject [--check <enshrouded.exe>]");
            2
        }
    };
    std::process::exit(code);
}

fn check(exe: &str) -> i32 {
    let Ok(data) = std::fs::read(exe) else {
        eprintln!("cannot read {exe}");
        return 1;
    };
    let hits = ens_hook::sig::find(&data);
    println!("{exe}: signature found {} time(s)", hits.len());
    match hits.as_slice() {
        [h] if data[h + ens_hook::sig::PATCH_AT..][..ens_hook::sig::PATCH_LEN] == ens_hook::sig::REPLACED => {
            println!("supported: hook point at file offset {:#x}", h + ens_hook::sig::PATCH_AT);
            0
        }
        _ => {
            println!("not supported");
            1
        }
    }
}

fn inject() -> i32 {
    let dll: PathBuf = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("ens_hook.dll"))).unwrap_or_default();
    let Some(game) = ens_inject::find_game() else {
        eprintln!("enshrouded.exe is not running");
        return 1;
    };
    if let Err(e) = ens_inject::inject(game.pid, &dll) {
        eprintln!("{e}");
        return 1;
    }
    println!("loaded {} into enshrouded.exe; see hook.log and combat-*.csv next to it", dll.display());
    0
}
