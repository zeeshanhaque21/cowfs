//! Issue 173, slice 5: power loss across a real `Core` workload, store, metadata and the mount
//! root on ONE timeline.
//!
//! One thread runs a scripted workload (file create, overwrite and fsync, rename and unlink inside
//! a snapshot, snapshot fork, remove and rename, two `promote_base` swaps through the intent file,
//! a reclaiming gc cycle and syncs) on a `Core` whose
//!
//! - store records every write, fsync, create, unlink and directory fsync into the store op log
//!   (`oplog_start`),
//! - metadata database is the recording redb backend (`Meta::open_with_backend`), whose events are
//!   stamped into that same log as markers,
//! - mount root records the swap intent file's create, write, fsync, rename, unlink and directory
//!   fsync (`fsops::rootlog`), stamped into the same log too,
//! - acknowledgements (`Core::sync`, `fsync`) are stamped as well.
//!
//! The one log gives a total order, so a power cut at op `k` is rebuilt for each component at that
//! same instant: the store by `cowfs_store::crashmodel::crash_image` (unsynced writes survive in
//! any subset and may tear; a directory entry survives only if its directory was fsynced after it),
//! the metadata file as the events up to its last completed `sync_data` (or, in other seeds, more),
//! the mount root by the model below. Each image is reopened with the shipped `Core::open` and
//! must: open, report no loss, pass `Core::check` and `fsck`, show every snapshot that an
//! acknowledgement made durable with every file byte for byte, show no snapshot nobody could have
//! created, leave no staging snapshot or intent file behind, and accept a second operation (a new
//! snapshot, a promotion) that survives a further reopen.
//!
//! What this does not model, on purpose: see `docs/crash-injection-173.md` (slice 5).
//!
//! `COWFS_POWER_SEEDS` sets the images per cut (default 6), `COWFS_POWER_THREADS` the workers.
//! The older `crash.rs`, `durability.rs`, `ns_durability*.rs` and `kill9.rs` stay: they cut the
//! store with a cruder rule or by killing a process, and see what a process crash can see.

mod common;

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex};

use common::{pattern, write_all};
use cowfs_core::fsops::rootlog::{self, RootOp, ROOT_MARK};
use cowfs_core::{Core, Options};
use cowfs_meta::Meta;
use cowfs_store::crashmodel::{crash_image, read_image, write_image, Image, Rng};
use cowfs_store::{oplog_marker, oplog_start, oplog_take, LogOp};
use cowfs_vfs::{FileKind, Ino, RenameFlags, Vfs, ROOT_INO};
use redb::StorageBackend;

/// Marks a store-log marker as a metadata backend event; the store's own markers are small.
const META_MARK: u64 = 1 << 40;
/// Marks a store-log marker as an acknowledgement; the low bits are the interval it opens.
const ACK_MARK: u64 = 1 << 42;
const KIND: u64 = 7 << 40;

type Tree = BTreeMap<String, Vec<u8>>;

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

/// redb's file, in memory, with every write, set_len and sync logged and stamped into the store's
/// op log, so a cut at store op `k` knows which metadata events had happened.
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

impl Backend {
    fn push(&self, ev: Ev) {
        let mut r = self.0.lock().unwrap();
        apply(&mut r.data, &ev, None);
        let i = r.log.len() as u64;
        r.log.push(ev);
        drop(r);
        oplog_marker(META_MARK | i);
    }

    /// The file as it is now, which is durable by construction, and an empty event log.
    fn rebase(&self) -> Vec<u8> {
        let mut r = self.0.lock().unwrap();
        r.log.clear();
        r.data.clone()
    }

    fn events(&self) -> Vec<Ev> {
        self.0.lock().unwrap().log.clone()
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
        self.push(Ev::SetLen(len));
        Ok(())
    }

    fn sync_data(&self) -> Result<(), io::Error> {
        self.push(Ev::Sync);
        Ok(())
    }

    fn write(&self, offset: u64, data: &[u8]) -> Result<(), io::Error> {
        self.push(Ev::Write(offset, data.to_vec()));
        Ok(())
    }
}

fn core_opts() -> Options {
    Options {
        background: false,
        max_pending_ops: 6,
        file_flush_bytes: 200 << 10,
        store: cowfs_store::Options {
            max_pack_size: 48 << 10,
            ..Default::default()
        },
        meta: cowfs_meta::Options {
            node_size: 1024,
            sync_every_ops: 3,
            sync_interval: std::time::Duration::from_secs(3600),
            // A meta thread would run `Store::sync` off this thread, where the thread-local op
            // log cannot see it. The test refuses to run with one.
            background: false,
            ..Default::default()
        },
        ..Options::default()
    }
}

fn gc_opts() -> cowfs_gc::Options {
    cowfs_gc::Options {
        dead_ratio: 0.0,
        min_dead_bytes: 1,
        io_budget_bytes: 0,
        batch_bytes: 4096,
        ..Default::default()
    }
}

/// What a cut inside one acknowledgement interval allows to be on disk after recovery.
#[derive(Clone, Default)]
struct Expect {
    /// Trees an acknowledgement made durable. Present unless in `may_vanish`.
    acked: BTreeMap<String, Tree>,
    /// Snapshots that may exist although nothing acknowledged them.
    may_exist: BTreeSet<String>,
    /// Acknowledged snapshots whose removal or rename is not yet acknowledged.
    may_vanish: BTreeSet<String>,
    /// A snapshot a swap or rename put a whole new tree under: it must equal its acknowledged tree
    /// or one of these, exactly.
    alt: BTreeMap<String, Vec<Tree>>,
    /// (old, new) of a snapshot rename: one metadata transaction, so one of the two must exist.
    renamed: Vec<(String, String)>,
    /// Files changed since the acknowledgement; their bytes are not checked.
    touched: BTreeMap<String, HashSet<String>>,
}

struct World {
    core: Core,
    backend: Backend,
    live: BTreeMap<String, Tree>,
    cur: Expect,
    /// `acks[j]` is the expectation for cuts after acknowledgement `j` (0 is the setup).
    acks: Vec<Expect>,
}

fn forget(fs: &dyn Vfs, ino: Ino) {
    fs.forget(ino, 1);
}

impl World {
    fn view(&self, s: &str) -> cowfs_core::SnapshotView {
        self.core.snapshot_view(s).unwrap()
    }

    fn touch(&mut self, s: &str, path: &str) {
        self.cur.touched.entry(s.into()).or_default().insert(path.into());
    }

    fn guard_alt(&self, s: &str) {
        assert!(
            !self.cur.alt.contains_key(s),
            "the script writes to {s} before an acknowledgement; its swap check is exact"
        );
    }

    /// Create `path` in `s`, or overwrite its start with `data`.
    fn put(&mut self, s: &str, path: &str, data: &[u8]) {
        self.guard_alt(s);
        let fs = self.view(s);
        let t = self.live.get_mut(s).unwrap();
        match t.get_mut(path) {
            Some(old) => {
                let a = fs.lookup(ROOT_INO, path.as_bytes()).unwrap();
                write_all(&fs, a.ino, 0, data);
                forget(&fs, a.ino);
                if old.len() < data.len() {
                    old.resize(data.len(), 0);
                }
                old[..data.len()].copy_from_slice(data);
            }
            None => {
                let a = fs.create(ROOT_INO, path.as_bytes(), 0o644).unwrap();
                write_all(&fs, a.ino, 0, data);
                forget(&fs, a.ino);
                t.insert(path.into(), data.to_vec());
            }
        }
        self.touch(s, path);
    }

    fn unlink(&mut self, s: &str, path: &str) {
        self.guard_alt(s);
        self.view(s).unlink(ROOT_INO, path.as_bytes()).unwrap();
        self.live.get_mut(s).unwrap().remove(path);
        self.touch(s, path);
    }

    fn rename_file(&mut self, s: &str, from: &str, to: &str) {
        self.guard_alt(s);
        self.view(s)
            .rename(
                ROOT_INO,
                from.as_bytes(),
                ROOT_INO,
                to.as_bytes(),
                RenameFlags::default(),
            )
            .unwrap();
        let t = self.live.get_mut(s).unwrap();
        let v = t.remove(from).unwrap();
        t.insert(to.into(), v);
        self.touch(s, from);
        self.touch(s, to);
    }

    fn new_snapshot(&mut self, s: &str, from: Option<&str>) {
        match from {
            Some(src) => self.core.fork_snapshot(src, s).unwrap(),
            None => self.core.create_snapshot(s).unwrap(),
        };
        let t = from.map_or_else(Tree::new, |src| self.live[src].clone());
        self.live.insert(s.into(), t);
        self.cur.may_exist.insert(s.into());
    }

    fn remove_snapshot(&mut self, s: &str) {
        self.core.remove_snapshot(s).unwrap();
        self.live.remove(s);
        self.cur.may_vanish.insert(s.into());
    }

    fn rename_snapshot(&mut self, from: &str, to: &str) {
        self.core.rename_snapshot(from, to).unwrap();
        let t = self.live.remove(from).unwrap();
        self.cur.alt.insert(to.into(), vec![t.clone()]);
        self.live.insert(to.into(), t);
        self.cur.may_exist.insert(to.into());
        self.cur.may_vanish.insert(from.into());
        self.cur.renamed.push((from.into(), to.into()));
    }

    fn promote(&mut self, src: &str, target: &str) {
        self.core.promote_base(src, target).unwrap();
        let t = self.live[src].clone();
        self.cur.alt.insert(target.into(), vec![t.clone()]);
        self.live.insert(target.into(), t);
        self.cur.may_exist.insert(target.into());
    }

    /// Start a new acknowledgement interval: `acks` gets the one that just ended, and the stamp
    /// tells the log where the next one begins.
    fn ack(&mut self, only: Option<&str>) {
        self.acks.push(self.cur.clone());
        let names: Vec<String> = match only {
            Some(s) => vec![s.into()],
            None => self.live.keys().cloned().collect(),
        };
        for n in &names {
            self.cur.acked.insert(n.clone(), self.live[n].clone());
            self.cur.touched.remove(n);
            self.cur.alt.remove(n);
            self.cur.may_exist.remove(n);
        }
        if only.is_none() {
            self.cur.acked.retain(|n, _| self.live.contains_key(n));
            self.cur.may_vanish.clear();
            self.cur.may_exist.clear();
            self.cur.alt.clear();
            self.cur.touched.clear();
            self.cur.renamed.clear();
        } else {
            for n in &names {
                self.cur.may_vanish.remove(n);
            }
        }
        oplog_marker(ACK_MARK | (self.acks.len() as u64));
    }

    fn sync(&mut self) {
        self.core.sync().unwrap();
        self.ack(None);
    }

    fn fsync(&mut self, s: &str, path: &str) {
        let fs = self.view(s);
        let a = fs.lookup(ROOT_INO, path.as_bytes()).unwrap();
        fs.fsync(a.ino, false).unwrap();
        forget(&fs, a.ino);
        self.ack(Some(s));
    }
}

fn data(len: usize, seed: u64) -> Vec<u8> {
    pattern(len, seed)
}

/// Everything one recorded workload leaves behind, ready to be cut anywhere.
struct Run {
    store_base: Image,
    meta_base: Vec<u8>,
    meta_evs: Vec<Ev>,
    ops: Vec<LogOp>,
    rops: Vec<RootOp>,
    /// Position of each root op's stamp in `ops`.
    root_at: Vec<usize>,
    /// (position, interval opened) of each acknowledgement stamp.
    ack_at: Vec<(usize, usize)>,
    acks: Vec<Expect>,
    gc_unlinked: u64,
    swaps: usize,
}

/// `hook` false drops the store sync that Core wires before every durable metadata commit: the
/// negative control that the whole machinery must be able to see.
fn record(hook: bool) -> Run {
    let dir = tempfile::tempdir().unwrap();
    let backend = Backend::default();
    let core = Core::open_with_meta(dir.path(), core_opts(), |_p, mo| {
        let mut mo = mo;
        if !hook {
            mo.before_sync = None;
        }
        Meta::open_with_backend(backend.clone(), mo)
    })
    .unwrap();
    let mut w = World {
        core,
        backend: backend.clone(),
        live: BTreeMap::new(),
        cur: Expect::default(),
        acks: Vec::new(),
    };
    for s in ["base", "s1"] {
        w.new_snapshot(s, None);
    }
    w.put("base", "b0", &data(6000, 1));
    w.put("s1", "a", &data(20_000, 2));
    w.put("s1", "b", &data(5000, 3));
    w.core.sync().unwrap();
    // setup is durable and not part of the recorded history: the first interval starts here
    w.cur.acked = w.live.clone();
    w.cur.may_exist.clear();
    w.cur.touched.clear();

    let store_base = read_image(&dir.path().join("store"));
    let meta_base = backend.rebase();
    let root_base: Vec<String> = fs::read_dir(dir.path())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("swap-") || n.starts_with("tmp-swap-"))
        .collect();
    assert!(root_base.is_empty(), "setup left intent files: {root_base:?}");

    oplog_start();
    rootlog::start();

    w.put("s1", "c", &data(30_000, 4));
    w.put("s1", "a", &data(10_000, 5));
    w.fsync("s1", "a");
    w.new_snapshot("s2", Some("s1"));
    w.put("s2", "d", &data(25_000, 6));
    w.core.flush().unwrap();
    w.rename_file("s1", "b", "b2");
    w.unlink("s1", "c");
    w.sync();

    w.new_snapshot("s3", None);
    w.put("s3", "e", &data(15_000, 7));
    w.remove_snapshot("s2");
    w.core.flush().unwrap();
    // replacing swap: the victim `base` is acknowledged and `s1` has unsynced changes to flush
    w.put("s1", "g", &data(12_000, 8));
    w.promote("s1", "base");
    w.put("s3", "f", &data(10_000, 9));
    w.sync();

    // non-replacing swap, then a rename of a snapshot, neither acknowledged until the end
    w.promote("s3", "fresh");
    w.sync();
    w.rename_snapshot("s1", "s1r");
    w.put("s3", "h", &data(9000, 10));

    let collector = w.core.collector(gc_opts()).unwrap();
    let rep = collector.collect().unwrap();
    assert!(rep.errors.is_empty(), "recorded collect: {:?}", rep.errors);
    let gc_unlinked = rep.packs_unlinked;
    drop(collector);
    w.sync();

    let ops = oplog_take();
    let rops = rootlog::take();
    let mut root_at = vec![usize::MAX; rops.len()];
    let mut ack_at = vec![(0usize, 0usize)];
    for (p, o) in ops.iter().enumerate() {
        if let LogOp::Marker(v) = o {
            if v & KIND == ROOT_MARK {
                root_at[(v & !KIND) as usize] = p;
            } else if v & KIND == ACK_MARK {
                ack_at.push((p, (v & !KIND) as usize));
            }
        }
    }
    assert!(root_at.iter().all(|&p| p != usize::MAX), "a root op was not stamped");
    w.acks.push(w.cur.clone());
    Run {
        store_base,
        meta_base,
        meta_evs: backend.events(),
        ops,
        rops,
        root_at,
        ack_at,
        acks: w.acks,
        gc_unlinked,
        swaps: 3,
    }
}

/// The metadata file after a cut at store op `k`. Events stamped before `k` happened; the file is
/// at least the last completed `sync_data`, and at most everything that happened (`mode` 1), or
/// the synced part plus a random subset, the writes of which may tear (`mode` 2).
fn meta_image(run: &Run, k: usize, mode: u64, rng: &mut Rng) -> Vec<u8> {
    let done = run.ops[..k.min(run.ops.len())]
        .iter()
        .filter_map(|o| match o {
            LogOp::Marker(v) if v & KIND == META_MARK => Some((v & !KIND) as usize),
            _ => None,
        })
        .max();
    let mut img = run.meta_base.clone();
    let Some(d) = done else { return img };
    let evs = &run.meta_evs[..=d];
    let synced = evs.iter().rposition(|e| matches!(e, Ev::Sync)).map_or(0, |i| i + 1);
    let upto = if mode == 1 { evs.len() } else { synced };
    evs[..upto].iter().for_each(|e| apply(&mut img, e, None));
    if mode == 2 {
        for e in &evs[synced..] {
            if rng.below(2) == 0 {
                let torn = match e {
                    Ev::Write(_, d) if rng.below(3) == 0 => Some(rng.below(d.len() as u64 + 1) as usize),
                    _ => None,
                };
                apply(&mut img, e, torn);
            }
        }
    }
    img
}

/// The mount root's files after a cut at store op `k`. An op completed if it was stamped before `k`
/// (the next op cannot start before it returns); the op stamped at `k` may or may not have run.
/// Model: file data is durable once its file was fsynced, else any prefix of it; a directory entry
/// change (create, rename, unlink) is durable once the root was fsynced after it, and the rest
/// reach the disk as a prefix, in order, as a journalling filesystem writes them.
fn root_image(run: &Run, k: usize, rng: &mut Rng) -> BTreeMap<String, Vec<u8>> {
    struct Node {
        data: Vec<u8>,
        synced: Option<Vec<u8>>,
    }
    enum Entry {
        Set(String, Option<usize>),
        Move(String, String),
    }
    let mut inodes: Vec<Node> = Vec::new();
    let mut ns: BTreeMap<String, usize> = BTreeMap::new();
    let mut entries: Vec<Entry> = Vec::new();
    let mut durable_entries = 0;
    for (i, op) in run.rops.iter().enumerate() {
        let at = run.root_at[i];
        let done = at < k || k >= run.ops.len();
        let inflight = at == k && !done;
        if !(done || (inflight && rng.below(2) == 0)) {
            continue;
        }
        match op {
            RootOp::Create(n) => {
                inodes.push(Node { data: Vec::new(), synced: None });
                ns.insert(n.clone(), inodes.len() - 1);
                entries.push(Entry::Set(n.clone(), Some(inodes.len() - 1)));
            }
            RootOp::Write(n, d) => {
                if let Some(&i) = ns.get(n) {
                    inodes[i].data = d.clone();
                }
            }
            RootOp::Sync(n) if done => {
                if let Some(&i) = ns.get(n) {
                    inodes[i].synced = Some(inodes[i].data.clone());
                }
            }
            RootOp::Rename(a, b) => {
                if let Some(i) = ns.remove(a) {
                    ns.insert(b.clone(), i);
                    entries.push(Entry::Move(a.clone(), b.clone()));
                }
            }
            RootOp::Unlink(n) => {
                if ns.remove(n).is_some() {
                    entries.push(Entry::Set(n.clone(), None));
                }
            }
            RootOp::DirSync if done => durable_entries = entries.len(),
            _ => {}
        }
    }
    let keep = durable_entries + rng.below((entries.len() - durable_entries) as u64 + 1) as usize;
    let mut out_ns: BTreeMap<String, usize> = BTreeMap::new();
    for e in &entries[..keep] {
        match e {
            Entry::Set(n, Some(i)) => {
                out_ns.insert(n.clone(), *i);
            }
            Entry::Set(n, None) => {
                out_ns.remove(n);
            }
            Entry::Move(a, b) => {
                if let Some(i) = out_ns.remove(a) {
                    out_ns.insert(b.clone(), i);
                }
            }
        }
    }
    out_ns
        .into_iter()
        .map(|(n, i)| {
            let node = &inodes[i];
            let bytes = match &node.synced {
                Some(s) => s.clone(),
                None => node.data[..rng.below(node.data.len() as u64 + 1) as usize].to_vec(),
            };
            (n, bytes)
        })
        .collect()
}

fn walk(fs: &dyn Vfs, dir: Ino, prefix: &str, out: &mut Tree) -> Result<(), String> {
    let mut cookie = 0;
    loop {
        let r = fs.readdir(dir, cookie, 50).map_err(|e| format!("readdir: {e:?}"))?;
        for e in &r.entries {
            let name = format!("{prefix}{}", String::from_utf8_lossy(&e.name));
            match e.kind {
                FileKind::Directory => walk(fs, e.ino, &format!("{name}/"), out)?,
                _ => {
                    let a = fs.getattr(e.ino).map_err(|e| format!("getattr: {e:?}"))?;
                    let mut bytes = Vec::new();
                    while (bytes.len() as u64) < a.size {
                        let got = fs
                            .read(e.ino, bytes.len() as u64, 1 << 20)
                            .map_err(|er| format!("dangling chunk or unreadable {name}: {er:?}"))?;
                        if got.is_empty() {
                            return Err(format!("short read of {name}"));
                        }
                        bytes.extend(got);
                    }
                    out.insert(name, bytes);
                }
            }
        }
        if r.eof {
            return Ok(());
        }
        cookie = r.entries.last().map_or(0, |e| e.cookie);
    }
}

#[derive(Default)]
struct Tally {
    images: u32,
    /// images where an intent file was on disk at open
    with_intent: u32,
    /// swap targets that came back as the old tree, and as the new one
    old_tree: u32,
    new_tree: u32,
    failures: Vec<String>,
}

fn same_tree(a: &Tree, b: &Tree) -> bool {
    a == b
}

fn verify(dir: &Path, ex: &Expect, t: &mut Tally) -> Result<(), String> {
    let had_intent = fs::read_dir(dir)
        .map_err(|e| e.to_string())?
        .flatten()
        .any(|e| e.file_name().to_string_lossy().starts_with("swap-"));
    t.with_intent += u32::from(had_intent);
    let c = Core::open(dir, core_opts()).map_err(|e| format!("reopen failed: {e:?}"))?;
    if c.store().recovery().has_corruption() {
        return Err(format!("store reported a loss: {:?}", c.store().recovery()));
    }
    c.check().map_err(|e| format!("meta check: {e:?}"))?;
    let report = c.fsck().map_err(|e| format!("fsck: {e:?}"))?;
    if !report.is_clean() {
        return Err(format!("fsck: {report:?}"));
    }
    // hidden snapshots: a staging name left after recovery is a leak
    for info in c.meta().snapshots().map_err(|e| format!("{e:?}"))? {
        if cowfs_snapname::is_reserved(&info.name) {
            return Err(format!("a staging snapshot {:?} survived recovery", info.name));
        }
    }
    let left: Vec<String> = fs::read_dir(dir)
        .map_err(|e| e.to_string())?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("swap-") || n.starts_with("tmp-swap-"))
        .collect();
    if !left.is_empty() {
        return Err(format!("intent files left after recovery: {left:?}"));
    }
    let visible: BTreeSet<String> = c
        .list_snapshots()
        .map_err(|e| format!("{e:?}"))?
        .into_iter()
        .map(|e| e.name)
        .collect();
    for n in &visible {
        if !ex.acked.contains_key(n) && !ex.may_exist.contains(n) && !ex.alt.contains_key(n) {
            return Err(format!("stray snapshot {n:?} (visible {visible:?})"));
        }
    }
    for n in ex.acked.keys() {
        if !visible.contains(n) && !ex.may_vanish.contains(n) {
            return Err(format!("durable snapshot {n:?} is missing (visible {visible:?})"));
        }
    }
    for (old, new) in &ex.renamed {
        if !visible.contains(old) && !visible.contains(new) {
            return Err(format!("renamed snapshot {old:?} -> {new:?} exists under neither name"));
        }
    }
    for n in &visible {
        let fs = c.snapshot_view(n).map_err(|e| format!("{e:?}"))?;
        let mut got = Tree::new();
        walk(&fs, ROOT_INO, "", &mut got).map_err(|e| format!("snapshot {n:?}: {e}"))?;
        if let Some(alts) = ex.alt.get(n) {
            let old = ex.acked.get(n);
            if old.is_some_and(|a| same_tree(a, &got)) {
                t.old_tree += 1;
            } else if alts.iter().any(|a| same_tree(a, &got)) {
                t.new_tree += 1;
            } else {
                return Err(format!(
                    "snapshot {n:?} is neither its old tree nor the swapped-in one: {:?}",
                    got.iter().map(|(k, v)| (k.clone(), v.len())).collect::<Vec<_>>()
                ));
            }
        } else if let Some(want) = ex.acked.get(n) {
            let touched = ex.touched.get(n);
            for (path, bytes) in want {
                if touched.is_some_and(|t| t.contains(path)) {
                    continue;
                }
                match got.get(path) {
                    None => return Err(format!("acknowledged file {n}/{path} is missing")),
                    Some(have) if have != bytes => {
                        return Err(format!(
                            "acknowledged file {n}/{path} differs ({} vs {} bytes)",
                            have.len(),
                            bytes.len()
                        ))
                    }
                    Some(_) => {}
                }
            }
        }
    }
    // a second operation: a new snapshot with data, then a promotion, then a reopen
    let probe = data(18_000, 99);
    c.create_snapshot("zz-after").map_err(|e| format!("create after crash: {e:?}"))?;
    let fs = c.snapshot_view("zz-after").map_err(|e| format!("{e:?}"))?;
    let a = fs.create(ROOT_INO, b"p", 0o644).map_err(|e| format!("{e:?}"))?;
    write_all(&fs, a.ino, 0, &probe);
    fs.fsync(a.ino, false).map_err(|e| format!("fsync after crash: {e:?}"))?;
    fs.forget(a.ino, 1);
    drop(fs);
    c.promote_base("zz-after", "zz-promoted").map_err(|e| format!("promote after crash: {e:?}"))?;
    c.sync().map_err(|e| format!("{e:?}"))?;
    drop(c);
    let c = Core::open(dir, core_opts()).map_err(|e| format!("second reopen failed: {e:?}"))?;
    c.check().map_err(|e| format!("second check: {e:?}"))?;
    for name in ["zz-after", "zz-promoted"] {
        let fs = c.snapshot_view(name).map_err(|e| format!("{name}: {e:?}"))?;
        let mut got = Tree::new();
        walk(&fs, ROOT_INO, "", &mut got)?;
        if got.get("p") != Some(&probe) {
            return Err(format!("{name}: the write after the crash was lost"));
        }
    }
    if !c.fsck().map_err(|e| format!("{e:?}"))?.is_clean() {
        return Err("fsck dirty after the second operation".into());
    }
    Ok(())
}

fn env(name: &str, default: usize) -> usize {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

fn brief(op: &LogOp) -> String {
    match op {
        LogOp::Write { file, off, data } => format!("write {file}@{off}+{}", data.len()),
        LogOp::Whole { file, data } => format!("whole {file} {}B", data.len()),
        LogOp::Marker(v) if v & KIND == META_MARK => format!("meta event {}", v & !KIND),
        LogOp::Marker(v) if v & KIND == ROOT_MARK => format!("root op {}", v & !KIND),
        LogOp::Marker(v) if v & KIND == ACK_MARK => format!("ack {}", v & !KIND),
        other => format!("{other:?}"),
    }
}

fn sweep(run: &Run, seeds: u64, threads: usize) -> Tally {
    let total = Mutex::new(Tally::default());
    std::thread::scope(|sc| {
        for th in 0..threads {
            let total = &total;
            sc.spawn(move || {
                let mut t = Tally::default();
                for k in (0..=run.ops.len()).filter(|k| k % threads == th) {
                    // the interval the cut falls in: the last acknowledgement stamped before k
                    let (_, j) = *run.ack_at.iter().rev().find(|(p, _)| *p < k).unwrap_or(&run.ack_at[0]);
                    let ex = &run.acks[j.min(run.acks.len() - 1)];
                    for seed in 0..seeds {
                        let mut rng = Rng(seed ^ ((k as u64) << 20) ^ 0x57A1);
                        let img = crash_image(&run.store_base, &run.ops, k, &mut rng, seed % 4);
                        let tag = format!(
                            "k={k}/{} seed={seed} op={}",
                            run.ops.len(),
                            run.ops.get(k).map_or_else(|| "(all done)".into(), brief)
                        );
                        let dir = tempfile::tempdir().unwrap();
                        write_image(&img, &dir.path().join("store"));
                        fs::write(dir.path().join("meta.redb"), meta_image(run, k, seed % 3, &mut rng)).unwrap();
                        for (n, b) in root_image(run, k, &mut rng) {
                            fs::write(dir.path().join(n), b).unwrap();
                        }
                        t.images += 1;
                        if let Err(e) = verify(dir.path(), ex, &mut t) {
                            t.failures.push(format!("{tag}: {e}"));
                        }
                    }
                }
                let mut g = total.lock().unwrap();
                g.images += t.images;
                g.with_intent += t.with_intent;
                g.old_tree += t.old_tree;
                g.new_tree += t.new_tree;
                g.failures.extend(t.failures);
            });
        }
    });
    total.into_inner().unwrap()
}

fn report(run: &Run, t: &Tally) {
    println!(
        "{} power-cut images over {} ops ({} root ops, {} metadata events, {} acks), {} with an intent file on disk, swap targets old/new = {}/{}, gc unlinked {} packs",
        t.images,
        run.ops.len(),
        run.rops.len(),
        run.meta_evs.len(),
        run.ack_at.len() - 1,
        t.with_intent,
        t.old_tree,
        t.new_tree,
        run.gc_unlinked,
    );
}

#[test]
fn power_cut_at_every_op_of_a_core_workload_keeps_every_acknowledged_snapshot() {
    let run = record(true);
    assert!(run.ops.len() > 200, "the timeline is too short to mean anything: {}", run.ops.len());
    assert!(run.gc_unlinked > 0, "the recorded gc cycle reclaims nothing");
    assert!(
        run.rops.iter().any(|o| matches!(o, RootOp::Rename(..)))
            && run.rops.iter().any(|o| matches!(o, RootOp::Unlink(_))),
        "the swaps left no intent-file ops"
    );
    assert_eq!(run.rops.iter().filter(|o| matches!(o, RootOp::Rename(..))).count(), run.swaps);
    let t = sweep(&run, env("COWFS_POWER_SEEDS", 6) as u64, env("COWFS_POWER_THREADS", 4));
    report(&run, &t);
    assert!(t.images > 1000);
    assert!(t.with_intent > 0, "no image caught a swap with its intent file on disk");
    assert!(t.old_tree > 0 && t.new_tree > 0, "a swap target never came back as both outcomes");
    for f in t.failures.iter().take(8) {
        eprintln!("FAIL {f}");
    }
    assert!(t.failures.is_empty(), "{} of {} power-cut images failed", t.failures.len(), t.images);
}

/// The test must be able to see the bug it guards against: without the store sync before durable
/// metadata commits, some image has a dangling live chunk. Same control as `crash.rs`, now with the
/// crash model instead of a pack cut.
#[test]
fn the_power_test_notices_a_missing_store_sync_before_metadata_commits() {
    let run = record(false);
    let t = sweep(&run, env("COWFS_POWER_SEEDS", 6) as u64, env("COWFS_POWER_THREADS", 4));
    report(&run, &t);
    assert!(
        !t.failures.is_empty(),
        "no image of {} failed without the store sync hook: the test is blind",
        t.images
    );
    println!("hook off: {} of {} images fail, first: {}", t.failures.len(), t.images, t.failures[0]);
}
