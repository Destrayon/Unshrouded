//! Load any DLL into the running game: `cargo run --release -p ens-inject --example inject_dll -- <path.dll>`.

fn main() {
    let Some(dll) = std::env::args().nth(1) else {
        eprintln!("usage: inject_dll <path.dll>");
        std::process::exit(2);
    };
    let Some(g) = ens_inject::find_game() else {
        eprintln!("enshrouded.exe is not running");
        std::process::exit(1);
    };
    let dll = std::fs::canonicalize(&dll).unwrap_or_else(|_| dll.into());
    let dll = std::path::PathBuf::from(dll.to_string_lossy().trim_start_matches(r"\\?\"));
    match ens_inject::inject(g.pid, &dll) {
        Ok(()) => println!("loaded {} into pid {}", dll.display(), g.pid),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}
