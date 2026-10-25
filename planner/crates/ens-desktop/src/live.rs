//! The live meter's side of the app: finds the recorder's newest log and feeds what was written since the last poll
//! into an ens_calc::meter::Meter (docs/plans/live-meter.md).

use ens_calc::meter::Meter;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// The recorder's folder: ENS_LOG_DIR, else a `Combat Logger` folder next to the app or up to four folders above it
/// (the planner's exe sits in the project root or in planner/dist/EnshroudedPlanner).
pub fn log_dir() -> Option<PathBuf> {
    if let Ok(d) = std::env::var("ENS_LOG_DIR") {
        return Some(PathBuf::from(d));
    }
    let exe = std::env::current_exe().ok()?;
    exe.ancestors().skip(1).take(5).map(|a| a.join("Combat Logger")).find(|d| d.is_dir())
}

/// The newest `combat-*.csv` in `dir` (by name: the recorder names files by unix time).
pub fn newest(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir).ok()?.flatten().map(|e| e.path())
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("combat-") && n.ends_with(".csv")))
        .max_by_key(|p| p.file_name().map(|n| n.to_os_string()))
}

#[derive(Default)]
pub struct Live {
    pub meter: Meter,
    pub path: Option<PathBuf>,
    offset: u64,
    /// The planner's DPS for the open build and when it was worked out (recomputed every few seconds).
    pub planner: Option<(f64, std::time::Instant)>,
}

impl Live {
    /// Follow the newest log in `dir`: a new file starts a fresh meter; otherwise read what was appended.
    pub fn poll(&mut self, dir: &Path) -> Result<usize, String> {
        let Some(path) = newest(dir) else { return Ok(0) };
        if self.path.as_ref() != Some(&path) {
            (self.meter, self.offset, self.path) = (Meter::default(), 0, Some(path.clone()));
        }
        let mut f = std::fs::File::open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        if len < self.offset {
            // the file was replaced or truncated: start over
            (self.meter, self.offset) = (Meter::default(), 0);
        }
        if len == self.offset {
            return Ok(0);
        }
        f.seek(SeekFrom::Start(self.offset)).map_err(|e| e.to_string())?;
        let mut buf = Vec::with_capacity((len - self.offset) as usize);
        f.take(len - self.offset).read_to_end(&mut buf).map_err(|e| e.to_string())?;
        self.offset += buf.len() as u64;
        Ok(self.meter.feed(&String::from_utf8_lossy(&buf)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// The meter follows the newest log as it grows, row by row, and starts over on a newer file.
    #[test]
    fn follows_the_newest_log_as_it_grows() {
        let dir = std::env::temp_dir().join(format!("ens-live-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../Combat Logger/combat-1792718413.csv"));
        let Ok(src) = src else { return };
        let lines: Vec<&str> = src.lines().collect();
        let path = dir.join("combat-1000.csv");
        let mut f = std::fs::File::create(&path).unwrap();
        let mut live = Live::default();
        let mut fed = 0;
        // write in flush-sized pieces, polling between them, with a line cut in half once
        for chunk in lines[..400].chunks(37) {
            let text = chunk.join("\n") + "\n";
            let (a, b) = text.split_at(text.len() / 2);
            f.write_all(a.as_bytes()).unwrap();
            f.flush().unwrap();
            fed += live.poll(&dir).unwrap();
            f.write_all(b.as_bytes()).unwrap();
            f.flush().unwrap();
            fed += live.poll(&dir).unwrap();
        }
        assert_eq!(fed, 399, "every row once (the header is not a row)");
        // a newer log replaces the old one
        std::fs::write(dir.join("combat-2000.csv"), format!("{}\n{}\n", lines[0], lines[1])).unwrap();
        assert_eq!(live.poll(&dir).unwrap(), 1);
        assert!(live.path.as_ref().unwrap().ends_with("combat-2000.csv"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
