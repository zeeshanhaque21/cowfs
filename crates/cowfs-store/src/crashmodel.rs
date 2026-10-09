//! Power-loss crash model for the store: rebuild the disk image a cut at any op of a recorded
//! history could leave, from the log `oplog_start` records. Only with `fault-injection`.
//! Shared so a gc or core crash test can reuse it instead of carrying its own copy.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::Path;

use crate::LogOp;

#[derive(Debug)]
pub struct Rng(pub u64);
impl Rng {
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n.max(1)
    }
}

pub type Image = BTreeMap<String, Vec<u8>>;

pub fn read_image(dir: &Path) -> Image {
    let mut img = Image::new();
    for e in fs::read_dir(dir).unwrap() {
        let e = e.unwrap();
        let n = e.file_name().into_string().unwrap();
        if e.path().is_file() && n != "LOCK" && !n.ends_with(".tmp") {
            img.insert(n, fs::read(e.path()).unwrap());
        }
    }
    if dir.join("packs").exists() {
        for e in fs::read_dir(dir.join("packs")).unwrap() {
            let e = e.unwrap();
            img.insert(
                e.file_name().into_string().unwrap(),
                fs::read(e.path()).unwrap(),
            );
        }
    }
    img
}

pub fn write_image(img: &Image, dir: &Path) {
    fs::create_dir_all(dir.join("packs")).unwrap();
    for (n, b) in img {
        if n.starts_with("pack-") {
            fs::write(dir.join("packs").join(n), b).unwrap();
        } else {
            fs::write(dir.join(n), b).unwrap();
        }
    }
}

fn apply_write(buf: &mut Vec<u8>, off: u64, data: &[u8], rng: &mut Rng, junk: bool) {
    let end = off as usize + data.len();
    if buf.len() < off as usize {
        let old = buf.len();
        buf.resize(off as usize, 0);
        if junk {
            for b in &mut buf[old..] {
                *b = rng.next_u64() as u8;
            }
        }
    }
    if buf.len() < end {
        buf.resize(end, 0);
    }
    buf[off as usize..end].copy_from_slice(data);
}

/// Which directory a file lives in, for directory fsyncs: `packs`, or the store root (`.`).
fn dir_of(file: &str) -> &'static str {
    if file.starts_with("pack-") {
        "packs"
    } else {
        "."
    }
}

fn dirsync_dir(dir: &str) -> &'static str {
    if dir == "packs" {
        "packs"
    } else {
        "."
    }
}

/// Disk state if power fails while op `k` is in flight. `k == ops.len()` is the cut after the last
/// op returned: every op completed, nothing was in flight.
///
/// Model: fsynced file data survives; unsynced writes survive in any subset and may tear at 512 B or
/// 4 KiB sectors (`mode` 0 and 1), whole (2) or as a prefix (3). A created or unlinked file keeps its
/// directory entry change only if that directory was fsynced after it, else it is a coin flip. A
/// `Whole` write (temporary file, fsync, rename, directory fsync) is atomic.
pub fn crash_image(base: &Image, ops: &[LogOp], k: usize, rng: &mut Rng, mode: u64) -> Image {
    let mut img = base.clone();
    let k = k.min(ops.len());
    let upto = (k + 1).min(ops.len());
    let name_of = |op: &LogOp| -> Option<String> {
        match op {
            LogOp::Write { file, .. }
            | LogOp::SetLen { file, .. }
            | LogOp::Sync { file }
            | LogOp::Create { file }
            | LogOp::Whole { file, .. } => Some(file.clone()),
            _ => None,
        }
    };
    let mut last_sync: HashMap<String, usize> = HashMap::new();
    let mut last_dirsync: HashMap<&str, usize> = HashMap::new();
    let mut created: BTreeMap<String, usize> = BTreeMap::new();
    let mut unlinked: BTreeMap<String, usize> = BTreeMap::new();
    // `k` itself is in flight: its effect may or may not have reached the disk.
    for (p, op) in ops[..upto].iter().enumerate() {
        match op {
            LogOp::Sync { file } if p < k => {
                last_sync.insert(file.clone(), p);
            }
            LogOp::DirSync { dir } if p < k => {
                last_dirsync.insert(dirsync_dir(dir), p);
            }
            LogOp::Create { file } if p < k => {
                created.insert(file.clone(), p);
            }
            LogOp::Unlink { file } => {
                unlinked.insert(file.clone(), p);
            }
            _ => {}
        }
    }
    let durable_entry = |f: &str, p: usize| last_dirsync.get(dir_of(f)).is_some_and(|&d| d > p);
    let mut vanished: Vec<String> = Vec::new();
    for (f, &p) in &created {
        let tracked =
            f.starts_with("pack-") || f.contains(".torn-") || f == "SYNCED" || f == "ACKED";
        if tracked && !durable_entry(f, p) && rng.below(2) == 0 {
            vanished.push(f.clone());
        }
    }
    // An unlink whose directory fsync completed before the cut is gone for good; one without it is
    // gone or not at random. The file's own data ops before the unlink still ran, so a gone file is
    // skipped below rather than removed first and resurrected by `entry()`.
    let mut gone: Vec<String> = Vec::new();
    for (f, &p) in &unlinked {
        if (p < k && durable_entry(f, p)) || rng.below(2) == 0 {
            gone.push(f.clone());
        }
    }
    for f in vanished.iter().chain(&gone) {
        img.remove(f);
    }
    for (p, op) in ops[..upto].iter().enumerate() {
        let Some(f) = name_of(op) else { continue };
        if f == "LOCK" || f == "packs" || f.ends_with(".tmp") {
            continue;
        }
        if vanished.contains(&f) || gone.contains(&f) {
            continue;
        }
        let durable = last_sync.get(&f).is_some_and(|&s| p < s);
        let junk = rng.below(2) == 0;
        match op {
            LogOp::Create { file } => {
                if file.starts_with("pack-") || file == "SYNCED" || file == "ACKED" {
                    img.entry(file.clone()).or_default();
                }
            }
            LogOp::Whole { data, .. } => {
                if p < k {
                    img.insert(f, data.clone());
                }
            }
            LogOp::SetLen { len, .. } => {
                if durable || rng.below(2) == 0 {
                    let b = img.entry(f).or_default();
                    let old = b.len();
                    b.resize(*len as usize, 0);
                    if junk && *len as usize > old {
                        for x in &mut b[old..] {
                            *x = rng.next_u64() as u8;
                        }
                    }
                }
            }
            LogOp::Write { off, data, .. } => {
                let b = img.entry(f).or_default();
                if durable {
                    apply_write(b, *off, data, rng, junk);
                    continue;
                }
                match mode {
                    0 | 1 => {
                        let sector = if mode == 0 { 512u64 } else { 4096 };
                        let mut pos = *off;
                        let end = *off + data.len() as u64;
                        while pos < end {
                            let nxt = ((pos / sector) + 1) * sector;
                            let e = nxt.min(end);
                            if rng.below(2) == 0 {
                                apply_write(
                                    b,
                                    pos,
                                    &data[(pos - off) as usize..(e - off) as usize],
                                    rng,
                                    junk,
                                );
                            }
                            pos = e;
                        }
                    }
                    2 => {
                        if rng.below(2) == 0 {
                            apply_write(b, *off, data, rng, junk);
                        }
                    }
                    _ => {
                        let n = rng.below(data.len() as u64 + 1) as usize;
                        apply_write(b, *off, &data[..n], rng, junk);
                    }
                }
            }
            _ => {}
        }
    }
    img
}
