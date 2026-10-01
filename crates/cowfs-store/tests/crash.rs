//! Full power-loss model (unsynced ops persist in any subset) over whole histories AND over
//! open-time recovery, with crash-reopen loops. Ported from the round-3 critic's harness.
//! `C7C_SEEDS=1000 C7C_START=...` runs the 4000-case sweep.
mod common;

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::Path;
use std::sync::Mutex;

use common::{compressible, random};
use cowfs_store::{oplog_marker, oplog_start, oplog_take, BlockId, LogOp, Options, Store};

static SERIAL: Mutex<()> = Mutex::new(());

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

type Image = BTreeMap<String, Vec<u8>>;

fn read_image(dir: &Path) -> Image {
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

fn write_image(img: &Image, dir: &Path) {
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
                *b = rng.next() as u8;
            }
        }
    }
    if buf.len() < end {
        buf.resize(end, 0);
    }
    buf[off as usize..end].copy_from_slice(data);
}

/// Disk state if power fails while op `k` is in flight.
fn crash_image(base: &Image, ops: &[LogOp], k: usize, rng: &mut Rng, mode: u64) -> Image {
    let mut img = base.clone();
    let k = k.min(ops.len().saturating_sub(1));
    let upto = if ops.is_empty() { 0 } else { k + 1 };
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
    let mut last_dirsync: Option<usize> = None;
    let mut created: HashMap<String, usize> = HashMap::new();
    for (p, op) in ops[..k.min(ops.len())].iter().enumerate() {
        match op {
            LogOp::Sync { file } => {
                last_sync.insert(file.clone(), p);
            }
            LogOp::DirSync => last_dirsync = Some(p),
            LogOp::Create { file } => {
                created.insert(file.clone(), p);
            }
            _ => {}
        }
    }
    let mut vanished: Vec<String> = Vec::new();
    for (f, &p) in &created {
        if f.starts_with("pack-") || f.contains(".torn-") || f == "SYNCED" || f == "ACKED" {
            let durable = last_dirsync.is_some_and(|d| d > p);
            if !durable && rng.below(2) == 0 {
                vanished.push(f.clone());
            }
        }
    }
    for f in &vanished {
        img.remove(f);
    }
    for (p, op) in ops[..upto].iter().enumerate() {
        let Some(f) = name_of(op) else { continue };
        if f == "LOCK" || f == "packs" || f.ends_with(".tmp") {
            continue;
        }
        if vanished.contains(&f) {
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
                            *x = rng.next() as u8;
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

fn opts(rng: &mut Rng) -> Options {
    Options {
        max_pack_size: 20_000 + rng.below(120_000),
        checkpoint_on_drop: false,
        ..Options::default()
    }
}

struct Hist {
    base: Image,
    ops: Vec<LogOp>,
    puts: Vec<(BlockId, Vec<u8>)>,
}

fn history(seed: u64, o: Options) -> Hist {
    let mut rng = Rng(seed ^ 0xA5A5);
    let dir = tempfile::tempdir().unwrap();
    let mut puts: Vec<(BlockId, Vec<u8>)> = Vec::new();
    oplog_start();
    {
        let s = Store::open_unsynced(dir.path(), o).unwrap();
        let steps = 8 + rng.below(40);
        for _ in 0..steps {
            match rng.below(7) {
                0 | 1 => {
                    s.sync().unwrap();
                    oplog_marker(puts.len() as u64);
                }
                2 if rng.below(3) == 0 => {
                    s.checkpoint().unwrap();
                    oplog_marker(puts.len() as u64);
                }
                3 if !puts.is_empty() => {
                    // duplicate put
                    let i = rng.below(puts.len() as u64) as usize;
                    let d = puts[i].1.clone();
                    s.put(&d).unwrap();
                }
                _ => {
                    let len = 500 + rng.below(9000) as usize;
                    let d = if rng.below(3) == 0 {
                        compressible(rng.next(), len)
                    } else {
                        random(rng.next(), len)
                    };
                    let id = s.put(&d).unwrap();
                    puts.push((id, d));
                }
            }
        }
    }
    Hist {
        base: Image::new(),
        ops: oplog_take(),
        puts,
    }
}

fn acked_at(ops: &[LogOp], k: usize) -> usize {
    let mut a = 0;
    for op in &ops[..k.min(ops.len())] {
        if let LogOp::Marker(n) = op {
            a = *n as usize;
        }
    }
    a
}

fn check(s: &Store, puts: &[(BlockId, Vec<u8>)], acked: usize, tag: &str) -> Result<(), String> {
    for (i, (id, d)) in puts.iter().enumerate() {
        match s.get(*id) {
            Ok(got) if &got == d => {}
            Ok(_) => return Err(format!("{tag}: WRONG BYTES for put #{i}")),
            Err(e) if i < acked => {
                return Err(format!("{tag}: ACKED LOST put #{i}/{acked}: {e:?}"))
            }
            Err(_) => {}
        }
    }
    Ok(())
}

#[derive(Default, Debug)]
struct Tally {
    cases: u32,
    lost: u32,
    wrong: u32,
    false_corrupt: u32,
    fsck_dirty: u32,
    other: u32,
    recovery_ops: u64,
    sidecars_max: usize,
    examples: Vec<String>,
}

fn case(seed: u64, depth: u32, t: &mut Tally) {
    let mut rng = Rng(seed);
    let o = opts(&mut rng);
    let h = history(seed, o);
    let mode = rng.below(4);
    let k = if rng.below(2) == 0 {
        rng.below(h.ops.len() as u64 + 1) as usize
    } else {
        // just after / around a sync
        let syncs: Vec<usize> = h
            .ops
            .iter()
            .enumerate()
            .filter(|(_, o)| matches!(o, LogOp::Sync { .. }))
            .map(|(i, _)| i)
            .collect();
        if syncs.is_empty() {
            0
        } else {
            (syncs[rng.below(syncs.len() as u64) as usize] + rng.below(4) as usize).min(h.ops.len())
        }
    };
    let acked = acked_at(&h.ops, k);
    let mut img = crash_image(&h.base, &h.ops, k, &mut rng, mode);
    let tag = format!(
        "seed={seed} depth={depth} mode={mode} k={k}/{}",
        h.ops.len()
    );
    // crash-reopen loop: each round, open (recovery logged), crash mid-recovery
    for round in 0..depth {
        let dir = tempfile::tempdir().unwrap();
        write_image(&img, dir.path());
        oplog_start();
        let r = Store::open_unsynced(dir.path(), o);
        let ops = oplog_take();
        match r {
            Ok(s) => {
                if let Err(e) = check(&s, &h.puts, acked, &format!("{tag} r{round}")) {
                    record(t, e);
                }
            }
            Err(e) => {
                record(t, format!("{tag} r{round}: OPEN FAILED {e:?}"));
                return;
            }
        }
        t.recovery_ops += ops.len() as u64;
        let base = read_image(dir.path());
        let _ = base;
        let kk = rng.below(ops.len() as u64 + 1) as usize;
        let m2 = rng.below(4);
        img = crash_image(&img, &ops, kk, &mut rng, m2);
    }
    // final full recovery, then one more cycle
    let dir = tempfile::tempdir().unwrap();
    write_image(&img, dir.path());
    let mut extra: Vec<(BlockId, Vec<u8>)> = Vec::new();
    {
        let s = match Store::open_unsynced(dir.path(), o) {
            Ok(s) => s,
            Err(e) => {
                record(t, format!("{tag} final: OPEN FAILED {e:?}"));
                return;
            }
        };
        if let Err(e) = check(&s, &h.puts, acked, &format!("{tag} final")) {
            record(t, e);
        }
        let rep = s.recovery();
        if !rep.corrupt_synced.is_empty() || !rep.missing_synced.is_empty() {
            t.false_corrupt += 1;
            if t.examples.len() < 6 {
                t.examples
                    .push(format!("{tag} final FALSE CORRUPT: {rep:?}"));
            }
        }
        for i in 0..4 {
            let d = random(rng.next() ^ i, 600 + rng.below(3000) as usize);
            extra.push((s.put(&d).unwrap(), d));
        }
        s.sync().unwrap();
        s.checkpoint().unwrap();
    }
    let s = Store::open_unsynced(dir.path(), o).unwrap();
    if let Err(e) = check(&s, &h.puts, acked, &format!("{tag} open2")) {
        record(t, e);
    }
    for (id, d) in &extra {
        if s.get(*id).ok().as_ref() != Some(d) {
            record(t, format!("{tag} open2: second-cycle block lost"));
        }
    }
    let rep = s.recovery();
    if rep.has_corruption() {
        t.false_corrupt += 1;
        if t.examples.len() < 6 {
            t.examples
                .push(format!("{tag} open2 FALSE CORRUPT: {rep:?}"));
        }
    }
    match s.fsck() {
        Ok(f) if !f.is_clean() => {
            t.fsck_dirty += 1;
            if t.examples.len() < 6 {
                t.examples.push(format!("{tag} fsck dirty {:?}", f.damage));
            }
        }
        Err(e) => record(t, format!("{tag} fsck err {e:?}")),
        _ => {}
    }
    let sc = fs::read_dir(dir.path().join("packs"))
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".torn-")
        })
        .count();
    t.sidecars_max = t.sidecars_max.max(sc);
    t.cases += 1;
}

fn record(t: &mut Tally, e: String) {
    if e.contains("ACKED LOST") {
        t.lost += 1;
    } else if e.contains("WRONG BYTES") {
        t.wrong += 1;
    } else {
        t.other += 1;
    }
    if t.examples.len() < 6 {
        t.examples.push(e);
    }
}

#[test]
fn crash_loops() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let n: u64 = std::env::var("C7C_SEEDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(200);
    let start: u64 = std::env::var("C7C_START")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut t = Tally::default();
    for seed in start..start + n {
        for depth in 0..4u32 {
            case(seed * 10 + u64::from(depth), depth, &mut t);
        }
    }
    println!("TALLY {t:#?}");
    assert!(
        t.lost + t.wrong + t.other + t.false_corrupt + t.fsck_dirty == 0,
        "{t:#?}"
    );
}
