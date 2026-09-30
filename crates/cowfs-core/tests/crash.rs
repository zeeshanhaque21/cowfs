//! Power-loss simulation at the `Core` level.
//!
//! A seeded workload runs on a `Core` whose metadata database sits on a recording redb backend and
//! whose store is a real directory. At random operation boundaries the test remembers the length of
//! the backend log, a copy of the store files and what the model says is durable. Crash images are
//! rebuilt from that: metadata from the log under three loss policies, the store with its last pack
//! cut anywhere beyond the durable watermark. Each image is reopened and verified.
//!
//! `COWFS_CRASH_SEEDS` sets the number of workloads (default 1) and `COWFS_CRASH_OPS` their length.

mod common;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex};

use common::{pattern, Rng};
use cowfs_core::{store_sync_hook, Core, Options};
use cowfs_meta::Meta;
use cowfs_store::Store;
use cowfs_vfs::{FileKind, Ino, RenameFlags, Vfs, ROOT_INO};
use redb::StorageBackend;

#[derive(Debug, Clone)]
enum Ev {
    Write(u64, Vec<u8>),
    SetLen(u64),
    Sync,
}

#[derive(Debug, Default)]
struct Rec {
    data: Vec<u8>,
    log: Vec<Ev>,
}

#[derive(Debug, Clone, Default)]
struct Backend(Arc<Mutex<Rec>>);

fn apply(img: &mut Vec<u8>, ev: &Ev, torn: Option<usize>) {
    match ev {
        Ev::Write(off, data) => {
            let data = &data[..torn.unwrap_or(data.len()).min(data.len())];
            let end = *off as usize + data.len();
            if img.len() < end {
                img.resize(end, 0);
            }
            img[*off as usize..end].copy_from_slice(data);
        }
        Ev::SetLen(n) => img.resize(*n as usize, 0),
        Ev::Sync => {}
    }
}

impl StorageBackend for Backend {
    fn len(&self) -> Result<u64, io::Error> {
        Ok(self.0.lock().unwrap().data.len() as u64)
    }

    fn read(&self, offset: u64, out: &mut [u8]) -> Result<(), io::Error> {
        let r = self.0.lock().unwrap();
        let end = offset as usize + out.len();
        if end > r.data.len() {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        out.copy_from_slice(&r.data[offset as usize..end]);
        Ok(())
    }

    fn set_len(&self, len: u64) -> Result<(), io::Error> {
        let mut r = self.0.lock().unwrap();
        let ev = Ev::SetLen(len);
        apply(&mut r.data, &ev, None);
        r.log.push(ev);
        Ok(())
    }

    fn sync_data(&self) -> Result<(), io::Error> {
        self.0.lock().unwrap().log.push(Ev::Sync);
        Ok(())
    }

    fn write(&self, offset: u64, data: &[u8]) -> Result<(), io::Error> {
        let mut r = self.0.lock().unwrap();
        let ev = Ev::Write(offset, data.to_vec());
        apply(&mut r.data, &ev, None);
        r.log.push(ev);
        Ok(())
    }
}

const DIRS: [&str; 3] = ["d0", "d1", "d2"];

#[derive(Clone, Default)]
struct Tree {
    names: BTreeMap<String, u32>,
    files: HashMap<u32, Vec<u8>>,
}

/// What a completed durability point promised for one snapshot.
#[derive(Clone, Default)]
struct Dur {
    tree: Tree,
    touched: HashSet<u32>,
}

struct Point {
    log_len: usize,
    store: tempfile::TempDir,
    dur: BTreeMap<String, Dur>,
}

struct World {
    core: Core,
    backend: Backend,
    rng: Rng,
    trees: BTreeMap<String, Tree>,
    dur: BTreeMap<String, Dur>,
    next_nid: u32,
    next_snap: u32,
    points: Vec<Point>,
    dir: tempfile::TempDir,
}

fn store_opts() -> cowfs_store::Options {
    cowfs_store::Options {
        max_pack_size: 400 << 10,
        ..Default::default()
    }
}

fn meta_opts() -> cowfs_meta::Options {
    cowfs_meta::Options {
        node_size: 1024,
        sync_every_ops: 3,
        sync_interval: std::time::Duration::from_secs(3600),
        ..Default::default()
    }
}

fn core_opts() -> Options {
    Options {
        background: false,
        max_pending_ops: 6,
        file_flush_bytes: 200 << 10,
        store: store_opts(),
        meta: meta_opts(),
        ..Options::default()
    }
}

fn resolve(fs: &dyn Vfs, path: &str) -> (Ino, Vec<Ino>) {
    let mut cur = ROOT_INO;
    let mut refs = Vec::new();
    for c in path.split('/') {
        let a = fs.lookup(cur, c.as_bytes()).expect("lookup");
        refs.push(a.ino);
        cur = a.ino;
    }
    (cur, refs)
}

fn forget(fs: &dyn Vfs, refs: Vec<Ino>) {
    for i in refs {
        fs.forget(i, 1);
    }
}

fn split(path: &str) -> (&str, &str) {
    match path.rsplit_once('/') {
        Some((d, n)) => (d, n),
        None => ("", path),
    }
}

fn parent_of(fs: &dyn Vfs, path: &str) -> (Ino, Vec<Ino>) {
    let (d, _) = split(path);
    if d.is_empty() {
        (ROOT_INO, Vec::new())
    } else {
        resolve(fs, d)
    }
}

fn model_write(v: &mut Vec<u8>, off: usize, data: &[u8]) {
    if v.len() < off + data.len() {
        v.resize(off + data.len(), 0);
    }
    v[off..off + data.len()].copy_from_slice(data);
}

fn size_mix(rng: &mut Rng) -> usize {
    match rng.below(10) {
        0 => 0,
        1..=4 => rng.below(4000) as usize,
        5..=7 => rng.below(120_000) as usize,
        _ => 200_000 + rng.below(400_000) as usize,
    }
}

impl World {
    fn new(seed: u64, hook: bool) -> World {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(dir.path().join("store"), store_opts()).unwrap());
        let backend = Backend::default();
        let mut mo = meta_opts();
        if hook {
            mo.before_sync = Some(store_sync_hook(&store));
        }
        let meta = Meta::open_with_backend(backend.clone(), mo).unwrap();
        let core = Core::from_parts(store, meta, core_opts(), dir.path()).unwrap();
        core.create_snapshot("s0").unwrap();
        let view = core.snapshot_view("s0").unwrap();
        for d in DIRS {
            view.mkdir(ROOT_INO, d.as_bytes(), 0o755).unwrap();
        }
        core.sync().unwrap();
        let mut w = World {
            core,
            backend,
            rng: Rng(seed | 1),
            trees: BTreeMap::new(),
            dur: BTreeMap::new(),
            next_nid: 1,
            next_snap: 1,
            points: Vec::new(),
            dir,
        };
        w.trees.insert("s0".into(), Tree::default());
        w.dur.insert("s0".into(), Dur::default());
        w
    }

    fn view(&self, s: &str) -> cowfs_core::SnapshotView {
        self.core.snapshot_view(s).unwrap()
    }

    fn all_paths() -> Vec<String> {
        let mut v = Vec::new();
        for d in DIRS {
            for i in 0..8 {
                v.push(format!("{d}/f{i}"));
            }
        }
        for i in 0..3 {
            v.push(format!("r{i}"));
        }
        v
    }

    fn pick(&mut self, s: &str, exists: bool) -> Option<String> {
        let t = &self.trees[s];
        let c: Vec<String> = Self::all_paths()
            .into_iter()
            .filter(|p| t.names.contains_key(p) == exists)
            .collect();
        if c.is_empty() {
            None
        } else {
            let i = self.rng.below(c.len() as u64) as usize;
            Some(c[i].clone())
        }
    }

    fn touch(&mut self, s: &str, nids: &[u32]) {
        let d = self.dur.get_mut(s).unwrap();
        d.touched.extend(nids.iter().copied());
    }

    fn mark_durable(&mut self, s: &str) {
        let t = self.trees[s].clone();
        self.dur.insert(
            s.to_string(),
            Dur {
                tree: t,
                touched: HashSet::new(),
            },
        );
    }

    fn step(&mut self) {
        let names: Vec<String> = self.trees.keys().cloned().collect();
        let s = names[self.rng.below(names.len() as u64) as usize].clone();
        let fs = self.view(&s);
        match self.rng.below(20) {
            0..=4 => {
                let Some(p) = self.pick(&s, false) else {
                    return;
                };
                let (par, refs) = parent_of(&fs, &p);
                let a = fs.create(par, split(&p).1.as_bytes(), 0o644).unwrap();
                let data = pattern(size_mix(&mut self.rng), self.rng.next());
                common::write_all(&fs, a.ino, 0, &data);
                fs.forget(a.ino, 1);
                forget(&fs, refs);
                let nid = self.next_nid;
                self.next_nid += 1;
                let t = self.trees.get_mut(&s).unwrap();
                t.names.insert(p, nid);
                t.files.insert(nid, data);
            }
            5..=8 => {
                let Some(p) = self.pick(&s, true) else { return };
                let (ino, refs) = resolve(&fs, &p);
                let nid = self.trees[&s].names[&p];
                let len = self.trees[&s].files[&nid].len();
                let off = self.rng.below(len as u64 + 2000) as usize;
                let data = pattern(size_mix(&mut self.rng).min(300_000) + 1, self.rng.next());
                common::write_all(&fs, ino, off as u64, &data);
                forget(&fs, refs);
                model_write(
                    self.trees.get_mut(&s).unwrap().files.get_mut(&nid).unwrap(),
                    off,
                    &data,
                );
                self.touch(&s, &[nid]);
            }
            9..=10 => {
                let Some(p) = self.pick(&s, true) else { return };
                let (ino, refs) = resolve(&fs, &p);
                let nid = self.trees[&s].names[&p];
                let len = self.trees[&s].files[&nid].len() as u64;
                let size = self.rng.below(len + len / 2 + 10);
                common::truncate(&fs, ino, size).unwrap();
                forget(&fs, refs);
                self.trees
                    .get_mut(&s)
                    .unwrap()
                    .files
                    .get_mut(&nid)
                    .unwrap()
                    .resize(size as usize, 0);
                self.touch(&s, &[nid]);
            }
            11..=12 => {
                let Some(src) = self.pick(&s, true) else {
                    return;
                };
                let dst = Self::all_paths()
                    [self.rng.below(Self::all_paths().len() as u64) as usize]
                    .clone();
                let t = &self.trees[&s];
                let nid = t.names[&src];
                if t.names.get(&dst) == Some(&nid) {
                    return;
                }
                let victim = t.names.get(&dst).copied();
                let (sp, r1) = parent_of(&fs, &src);
                let (dp, r2) = parent_of(&fs, &dst);
                fs.rename(
                    sp,
                    split(&src).1.as_bytes(),
                    dp,
                    split(&dst).1.as_bytes(),
                    RenameFlags::default(),
                )
                .unwrap();
                forget(&fs, r1);
                forget(&fs, r2);
                let t = self.trees.get_mut(&s).unwrap();
                t.names.remove(&src);
                t.names.insert(dst, nid);
                let mut touched = vec![nid];
                touched.extend(victim);
                self.touch(&s, &touched);
            }
            13..=14 => {
                let Some(p) = self.pick(&s, true) else { return };
                let (par, refs) = parent_of(&fs, &p);
                fs.unlink(par, split(&p).1.as_bytes()).unwrap();
                forget(&fs, refs);
                let nid = self.trees.get_mut(&s).unwrap().names.remove(&p).unwrap();
                self.touch(&s, &[nid]);
            }
            15 => {
                let Some(src) = self.pick(&s, true) else {
                    return;
                };
                let Some(dst) = self.pick(&s, false) else {
                    return;
                };
                let (ino, r1) = resolve(&fs, &src);
                let (dp, r2) = parent_of(&fs, &dst);
                let a = fs.link(ino, dp, split(&dst).1.as_bytes()).unwrap();
                fs.forget(a.ino, 1);
                forget(&fs, r1);
                forget(&fs, r2);
                let nid = self.trees[&s].names[&src];
                self.trees.get_mut(&s).unwrap().names.insert(dst, nid);
                self.touch(&s, &[nid]);
            }
            16..=17 => {
                let Some(p) = self.pick(&s, true) else { return };
                let (ino, refs) = resolve(&fs, &p);
                fs.fsync(ino, false).unwrap();
                forget(&fs, refs);
                self.mark_durable(&s);
            }
            18 => {
                if self.trees.len() >= 4 {
                    return;
                }
                let name = format!("s{}", self.next_snap);
                self.next_snap += 1;
                self.core.fork_snapshot(&s, &name).unwrap();
                let t = self.trees[&s].clone();
                self.trees.insert(name.clone(), t);
                self.mark_durable(&s);
                self.mark_durable(&name);
            }
            _ => self.core.flush().unwrap(),
        }
        let _ = FileKind::Regular;
    }

    fn point(&mut self) {
        let store = tempfile::tempdir().unwrap();
        let src = self.dir.path().join("store");
        std::fs::create_dir_all(store.path().join("packs")).unwrap();
        for e in std::fs::read_dir(src.join("packs")).unwrap() {
            let e = e.unwrap();
            std::fs::copy(e.path(), store.path().join("packs").join(e.file_name())).unwrap();
        }
        for f in ["SYNCED", "index.cix"] {
            if src.join(f).exists() {
                std::fs::copy(src.join(f), store.path().join(f)).unwrap();
            }
        }
        self.points.push(Point {
            log_len: self.backend.0.lock().unwrap().log.len(),
            store,
            dur: self.dur.clone(),
        });
    }
}

fn synced_mark(dir: &Path) -> Option<(u32, u64)> {
    let b = std::fs::read(dir.join("SYNCED")).ok()?;
    let mut best: Option<(u64, u32, u64)> = None;
    for slot in b.chunks(32).filter(|c| c.len() == 32) {
        if crc32c::crc32c(&slot[..24]).to_le_bytes() != slot[24..28] {
            continue;
        }
        let seq = u64::from_le_bytes(slot[..8].try_into().unwrap());
        let pack = u32::from_le_bytes(slot[8..12].try_into().unwrap());
        let len = u64::from_le_bytes(slot[16..24].try_into().unwrap());
        if best.is_none_or(|(s, _, _)| seq > s) {
            best = Some((seq, pack, len));
        }
    }
    best.map(|(_, p, l)| (p, l))
}

fn pack_name(id: u32) -> String {
    format!("pack-{id:08}.cpk")
}

/// Copies a store copy, cutting the last pack anywhere beyond what a crash cannot take away.
fn crash_store(from: &Path, to: &Path, rng: &mut Rng) {
    std::fs::create_dir_all(to.join("packs")).unwrap();
    let mut ids: Vec<u32> = std::fs::read_dir(from.join("packs"))
        .unwrap()
        .filter_map(|e| {
            let n = e.unwrap().file_name().into_string().unwrap();
            n.strip_prefix("pack-")?.strip_suffix(".cpk")?.parse().ok()
        })
        .collect();
    ids.sort_unstable();
    let mark = synced_mark(from);
    let last = ids.last().copied();
    for id in ids {
        let bytes = std::fs::read(from.join("packs").join(pack_name(id))).unwrap();
        let keep = if Some(id) == last {
            let lo = match mark {
                Some((p, l)) if p == id => l as usize,
                Some((p, _)) if p > id => bytes.len(),
                _ => 16,
            }
            .min(bytes.len());
            lo + rng.below((bytes.len() - lo) as u64 + 1) as usize
        } else {
            bytes.len()
        };
        std::fs::write(to.join("packs").join(pack_name(id)), &bytes[..keep]).unwrap();
    }
    if from.join("SYNCED").exists() {
        std::fs::copy(from.join("SYNCED"), to.join("SYNCED")).unwrap();
    }
    if rng.below(2) == 0 && from.join("index.cix").exists() {
        std::fs::copy(from.join("index.cix"), to.join("index.cix")).unwrap();
    }
}

#[derive(Debug, Clone, Copy)]
enum Policy {
    All,
    SyncedOnly,
    SyncedPlusSome,
}

fn crash_meta(log: &[Ev], policy: Policy, rng: &mut Rng) -> Vec<u8> {
    let last_sync = log.iter().rposition(|e| matches!(e, Ev::Sync));
    let mut img = Vec::new();
    match policy {
        Policy::All => log.iter().for_each(|e| apply(&mut img, e, None)),
        Policy::SyncedOnly | Policy::SyncedPlusSome => {
            let n = last_sync.map_or(0, |i| i + 1);
            log[..n].iter().for_each(|e| apply(&mut img, e, None));
            if matches!(policy, Policy::SyncedPlusSome) {
                for e in &log[n..] {
                    if rng.below(2) == 0 {
                        let torn = match e {
                            Ev::Write(_, d) if rng.below(3) == 0 => {
                                Some(rng.below(d.len() as u64 + 1) as usize)
                            }
                            _ => None,
                        };
                        apply(&mut img, e, torn);
                    }
                }
            }
        }
    }
    img
}

fn walk(fs: &dyn Vfs, dir: Ino, prefix: &str, out: &mut BTreeMap<String, Vec<u8>>) {
    let mut cookie = 0;
    loop {
        let r = fs.readdir(dir, cookie, 50).expect("readdir");
        for e in &r.entries {
            let name = format!("{prefix}{}", String::from_utf8_lossy(&e.name));
            match e.kind {
                FileKind::Directory => walk(fs, e.ino, &format!("{name}/"), out),
                FileKind::Regular => {
                    let a = fs.getattr(e.ino).expect("getattr");
                    let mut data = Vec::new();
                    while (data.len() as u64) < a.size {
                        let got = fs
                            .read(e.ino, data.len() as u64, 1 << 20)
                            .unwrap_or_else(|er| {
                                panic!("dangling chunk or unreadable {name}: {er:?}")
                            });
                        assert!(!got.is_empty(), "short read of {name}");
                        data.extend(got);
                    }
                    out.insert(name, data);
                }
                FileKind::Symlink => {}
            }
        }
        if r.eof {
            return;
        }
        cookie = r.entries.last().map_or(0, |e| e.cookie);
    }
}

fn verify(dir: &Path, p: &Point, what: &str) {
    let c = Core::open(dir, core_opts()).unwrap_or_else(|e| panic!("{what}: reopen failed: {e:?}"));
    assert!(
        !c.store().recovery().has_corruption(),
        "{what}: recovery reported corruption"
    );
    c.check()
        .unwrap_or_else(|e| panic!("{what}: meta check failed: {e:?}"));
    let report = c.fsck().unwrap();
    assert!(report.is_clean(), "{what}: fsck: {report:?}");
    let listed: HashSet<String> = c
        .list_snapshots()
        .unwrap()
        .into_iter()
        .map(|e| e.name)
        .collect();
    for (s, d) in &p.dur {
        assert!(
            listed.contains(s),
            "{what}: durable snapshot {s} is missing"
        );
        let fs = c.snapshot_view(s).unwrap();
        let mut got = BTreeMap::new();
        walk(&fs, ROOT_INO, "", &mut got);
        for (path, nid) in &d.tree.names {
            if d.touched.contains(nid) {
                continue;
            }
            let want = &d.tree.files[nid];
            let have = got
                .get(path)
                .unwrap_or_else(|| panic!("{what}: fsynced file {s}/{path} is missing"));
            assert!(
                have == want,
                "{what}: fsynced file {s}/{path} differs ({} vs {} bytes)",
                have.len(),
                want.len()
            );
        }
    }
    for s in &listed {
        let fs = c.snapshot_view(s).unwrap();
        let mut got = BTreeMap::new();
        walk(&fs, ROOT_INO, "", &mut got);
    }
    let fs = c.snapshot_view("s0").unwrap();
    let a = fs.create(ROOT_INO, b"after-crash", 0o644).unwrap();
    common::write_all(&fs, a.ino, 0, &pattern(300_000, 99));
    fs.fsync(a.ino, false).unwrap();
    drop(fs);
    drop(c);
    let c = Core::open(dir, core_opts())
        .unwrap_or_else(|e| panic!("{what}: second reopen failed: {e:?}"));
    c.check().unwrap();
    let fs = c.snapshot_view("s0").unwrap();
    let a = fs.lookup(ROOT_INO, b"after-crash").unwrap();
    assert_eq!(
        common::read_all(&fs, a.ino),
        pattern(300_000, 99),
        "{what}: write after crash lost"
    );
    assert!(c.fsck().unwrap().is_clean());
}

fn env(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn run(seed: u64, ops: usize, hook: bool) -> usize {
    let mut w = World::new(seed, hook);
    for _ in 0..ops {
        w.step();
        if w.rng.below(6) == 0 {
            w.point();
        }
    }
    w.point();
    let log = w.backend.0.lock().unwrap().log.clone();
    let mut rng = Rng(seed.wrapping_mul(31) | 1);
    let mut images = 0;
    for (i, p) in w.points.iter().enumerate() {
        for policy in [
            Policy::All,
            Policy::SyncedOnly,
            Policy::SyncedPlusSome,
            Policy::SyncedPlusSome,
        ] {
            let img = tempfile::tempdir().unwrap();
            crash_store(p.store.path(), &img.path().join("store"), &mut rng);
            std::fs::write(
                img.path().join("meta.redb"),
                crash_meta(&log[..p.log_len], policy, &mut rng),
            )
            .unwrap();
            verify(img.path(), p, &format!("seed {seed} point {i} {policy:?}"));
            images += 1;
        }
    }
    images
}

#[test]
fn crash_images_reopen_consistent_and_keep_fsynced_data() {
    let seeds = env("COWFS_CRASH_SEEDS", 1);
    let ops = env("COWFS_CRASH_OPS", 100);
    let mut total = 0;
    for s in 0..seeds {
        total += run(0xC0DE + s as u64 * 7919, ops, true);
    }
    println!("{total} crash images verified over {seeds} workloads of {ops} operations");
}

/// The test must be able to see the bug it guards against: without the store sync before durable
/// metadata commits, some image has a dangling chunk or a lost fsynced file.
#[test]
#[should_panic]
fn the_crash_test_notices_a_missing_store_sync_before_metadata_commits() {
    for s in 0..4 {
        run(0xBAD + s * 13, 100, false);
    }
}
