//! The meter image the planner writes and the layer draws: `overlay.bin` in the recorder's folder.
//!
//! Layout (little-endian): magic `ENSO`, version u32, width u32, height u32, sequence u64 (raised on every write),
//! unix ms u64 (when it was written), then width × height pixels, 4 bytes each in B, G, R, A order, rows top to bottom.
//! Version 2 (docs/plans/overlay-parity.md) adds, before the pixels, where to draw: corner u32 (0 top-left,
//! 1 top-right, 2 bottom-left, 3 bottom-right), x f32 and y f32 (distance from that corner as a fraction of the game's
//! image width and height), and a u32 holding the planner's input port in its low half (UDP on 127.0.0.1 where the
//! layer sends the game's keys and clicks on the meter; 0: none) and the meter page's scale in percent in its high half
//! (0: 100 %), which tells the layer where the page's title bar and resize grip are. Version 3
//! (docs/plans/all-players-meter.md) adds, after the pixels, more images: a count u32, then for each a kind u32
//! (1: drawn beside the mouse pointer, the breakdown tooltip), width u32, height u32 and its BGRA pixels. Alpha is straight (not premultiplied); the layer blends with it.

use std::path::Path;

pub const MAGIC: [u8; 4] = *b"ENSO";
pub const VERSION: u32 = 3;
pub const HEADER: usize = 32;
pub const HEADER_V2: usize = 48;
/// The file the planner writes in the recorder's folder.
pub const FILE: &str = "overlay.bin";
/// A frame older than this is not drawn (the planner was closed or stopped writing).
pub const STALE_MS: u64 = 10_000;

/// Where the meter goes on the game's image.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Place {
    pub corner: u32,
    pub x: f32,
    pub y: f32,
}

impl Default for Place {
    /// Version 1 frames: 16 px from the top-left corner of a 1600 × 900 image.
    fn default() -> Place {
        Place { corner: 0, x: 0.01, y: 0.0178 }
    }
}

/// An extra image of a frame (version 3).
#[derive(Debug, Clone, PartialEq)]
pub struct Extra {
    /// 1: beside the mouse pointer.
    pub kind: u32,
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

pub const AT_CURSOR: u32 = 1;

#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub seq: u64,
    pub unix_ms: u64,
    pub place: Place,
    /// The planner's UDP port for input from the game (0: none).
    pub port: u16,
    /// The meter page's pixels per CSS pixel, in percent (0: 100).
    pub scale: u16,
    /// BGRA, rows top to bottom.
    pub pixels: Vec<u8>,
    /// Images drawn with the meter (version 3): the breakdown tooltip beside the pointer.
    pub extras: Vec<Extra>,
}

impl Frame {
    pub fn new(width: u32, height: u32) -> Frame {
        Frame { width, height, seq: 0, unix_ms: 0, place: Place::default(), port: 0, scale: 0, pixels: vec![0; width as usize * height as usize * 4], extras: vec![] }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER_V2 + self.pixels.len());
        out.extend(MAGIC);
        out.extend(VERSION.to_le_bytes());
        out.extend(self.width.to_le_bytes());
        out.extend(self.height.to_le_bytes());
        out.extend(self.seq.to_le_bytes());
        out.extend(self.unix_ms.to_le_bytes());
        out.extend(self.place.corner.to_le_bytes());
        out.extend(self.place.x.to_le_bytes());
        out.extend(self.place.y.to_le_bytes());
        out.extend((self.port as u32 | (self.scale as u32) << 16).to_le_bytes());
        out.extend(&self.pixels);
        out.extend((self.extras.len() as u32).to_le_bytes());
        for e in &self.extras {
            out.extend(e.kind.to_le_bytes());
            out.extend(e.width.to_le_bytes());
            out.extend(e.height.to_le_bytes());
            out.extend(&e.pixels);
        }
        out
    }

    /// A frame from file bytes; None when the bytes are not a whole frame of a known version.
    pub fn decode(b: &[u8]) -> Option<Frame> {
        if b.len() < HEADER || b[..4] != MAGIC {
            return None;
        }
        let u32_at = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
        let u64_at = |o: usize| u64::from_le_bytes(b[o..o + 8].try_into().unwrap());
        let (place, port, scale, start) = match u32_at(4) {
            1 => (Place::default(), 0, 0, HEADER),
            2 | 3 if b.len() >= HEADER_V2 => {
                let f32_at = |o: usize| f32::from_le_bytes(b[o..o + 4].try_into().unwrap());
                (Place { corner: u32_at(32) & 3, x: f32_at(36), y: f32_at(40) }, u32_at(44) as u16, (u32_at(44) >> 16) as u16, HEADER_V2)
            }
            _ => return None,
        };
        let (width, height) = (u32_at(8), u32_at(12));
        if width == 0 || height == 0 || width > 4096 || height > 4096 {
            return None;
        }
        let n = width as usize * height as usize * 4;
        let pixels = b.get(start..start + n)?.to_vec();
        let mut extras = vec![];
        if u32_at(4) == 3 {
            let mut at = start + n;
            let count = u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?);
            at += 4;
            for _ in 0..count.min(8) {
                let h = b.get(at..at + 12)?;
                let (kind, w, hh) = (u32::from_le_bytes(h[0..4].try_into().ok()?), u32::from_le_bytes(h[4..8].try_into().ok()?), u32::from_le_bytes(h[8..12].try_into().ok()?));
                if w == 0 || hh == 0 || w > 4096 || hh > 4096 {
                    return None;
                }
                let len = w as usize * hh as usize * 4;
                extras.push(Extra { kind, width: w, height: hh, pixels: b.get(at + 12..at + 12 + len)?.to_vec() });
                at += 12 + len;
            }
        }
        Some(Frame { width, height, seq: u64_at(16), unix_ms: u64_at(24), place, port, scale, pixels, extras })
    }
}

/// Write a frame so a reader never sees half of one: to a temporary file, then renamed over the old frame.
pub fn write_atomic(path: &Path, frame: &Frame) -> std::io::Result<()> {
    let tmp = path.with_extension("bin.tmp");
    std::fs::write(&tmp, frame.encode())?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_round_trips_and_bad_bytes_are_refused() {
        let mut f = Frame::new(3, 2);
        f.seq = 7;
        f.unix_ms = 1_792_700_000_000;
        f.place = Place { corner: 3, x: 0.02, y: 0.25 };
        f.port = 51234;
        f.scale = 125;
        f.extras.push(Extra { kind: AT_CURSOR, width: 1, height: 2, pixels: vec![9; 8] });
        f.pixels.iter_mut().enumerate().for_each(|(i, p)| *p = i as u8);
        let b = f.encode();
        assert_eq!(b.len(), HEADER_V2 + 24 + 4 + 12 + 8);
        assert_eq!(Frame::decode(&b), Some(f.clone()));
        assert_eq!(Frame::decode(&b[..b.len() - 1]), None, "a cut-off frame");
        let mut wrong = b.clone();
        wrong[0] = b'X';
        assert_eq!(Frame::decode(&wrong), None);
        // a version 2 frame (no extras) still reads
        let mut v2 = b[..HEADER_V2 + 24].to_vec();
        v2[4..8].copy_from_slice(&2u32.to_le_bytes());
        assert_eq!(Frame::decode(&v2).unwrap().extras, vec![]);
        // a version 1 frame (no placement) still reads, at the old spot
        let mut v1 = b[..HEADER].to_vec();
        v1[4..8].copy_from_slice(&1u32.to_le_bytes());
        v1.extend(&f.pixels);
        let old = Frame::decode(&v1).unwrap();
        assert_eq!((old.place, old.port, &old.pixels), (Place::default(), 0, &f.pixels));
        let dir = std::env::temp_dir().join(format!("ens-overlay-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(FILE);
        write_atomic(&path, &f).unwrap();
        f.seq = 8;
        write_atomic(&path, &f).unwrap();
        assert_eq!(Frame::decode(&std::fs::read(&path).unwrap()).unwrap().seq, 8, "a second write replaces the first");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
