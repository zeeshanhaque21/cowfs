//! Model-based tests: random operation sequences run on a `Core` (with snapshots, restarts and
//! cache drops) and on `MemVfs` (one per snapshot, forked by replaying the source's history),
//! comparing every result and the whole tree.

mod common;

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use common::pattern;
use cowfs_core::{Core, Options};
use cowfs_vfs::{
    Error, FileHandle, FileKind, Ino, RenameFlags, SetAttr, Vfs, XattrFlags, ROOT_INO,
};
use cowfs_vfs_test::MemVfs;
use proptest::prelude::*;

type Path = Vec<u8>;

#[derive(Clone, Debug)]
enum MOp {
    Create(Path, u16),
    Mkdir(Path, u16),
    Symlink(Path, u8),
    Link(Path, Path),
    Unlink(Path),
    Rmdir(Path),
    Rename(Path, Path, bool),
    Write(Path, u32, u32, u8),
    Truncate(Path, u32),
    SetMode(Path, u16),
    Read(Path, u32, u32),
    Xattr(Path, u8, Option<u8>),
    /// Path, mode, kind index (modulo 4) and device byte.
    Mknod(Path, u16, u8, u8),
    Hold(Path),
    HeldRead(u8, u32, u32),
    HeldWrite(u8, u32, u32, u8),
    HeldStat(u8),
    HeldRelease(u8),
    Fsync(Path),
    Fork(u8),
    Flush,
    Sync,
    DropCaches,
    Reopen,
    Compare(u8),
}

impl MOp {
    /// Operations that only exist in the live session and are not part of a snapshot's history.
    fn session_only(&self) -> bool {
        matches!(
            self,
            MOp::HeldRead(..)
                | MOp::HeldStat(_)
                | MOp::Fsync(_)
                | MOp::Fork(_)
                | MOp::Flush
                | MOp::Sync
                | MOp::DropCaches
                | MOp::Reopen
                | MOp::Compare(_)
        )
    }
}

fn path() -> impl Strategy<Value = Path> {
    prop::collection::vec(0u8..4, 1..4)
}

fn len() -> impl Strategy<Value = u32> {
    prop_oneof![
        6 => 0u32..5_000,
        3 => 0u32..80_000,
        1 => 200_000u32..400_000,
    ]
}

fn off() -> impl Strategy<Value = u32> {
    prop_oneof![
        4 => 0u32..10_000,
        3 => 0u32..300_000,
        1 => Just(0u32),
    ]
}

fn op() -> impl Strategy<Value = MOp> {
    prop_oneof![
        4 => (path(), any::<u16>()).prop_map(|(p, m)| MOp::Create(p, m)),
        3 => (path(), any::<u16>()).prop_map(|(p, m)| MOp::Mkdir(p, m)),
        2 => (path(), any::<u16>(), 0u8..4, 0u8..5).prop_map(|(p, m, k, d)| MOp::Mknod(p, m, k, d)),
        2 => (path(), 0u8..4).prop_map(|(p, t)| MOp::Symlink(p, t)),
        3 => (path(), path()).prop_map(|(a, b)| MOp::Link(a, b)),
        4 => path().prop_map(MOp::Unlink),
        3 => path().prop_map(MOp::Rmdir),
        4 => (path(), path(), any::<bool>()).prop_map(|(a, b, n)| MOp::Rename(a, b, n)),
        8 => (path(), off(), len(), any::<u8>()).prop_map(|(p, o, l, s)| MOp::Write(p, o, l, s)),
        3 => (path(), off()).prop_map(|(p, s)| MOp::Truncate(p, s)),
        2 => (path(), any::<u16>()).prop_map(|(p, m)| MOp::SetMode(p, m)),
        4 => (path(), off(), len()).prop_map(|(p, o, l)| MOp::Read(p, o, l)),
        2 => (path(), 0u8..3, prop::option::of(0u8..3)).prop_map(|(p, n, v)| MOp::Xattr(p, n, v)),
        2 => path().prop_map(MOp::Hold),
        2 => (any::<u8>(), off(), len()).prop_map(|(k, o, l)| MOp::HeldRead(k, o, l)),
        2 => (any::<u8>(), off(), len(), any::<u8>()).prop_map(|(k, o, l, s)| MOp::HeldWrite(k, o, l, s)),
        1 => any::<u8>().prop_map(MOp::HeldStat),
        1 => any::<u8>().prop_map(MOp::HeldRelease),
        1 => path().prop_map(MOp::Fsync),
        2 => any::<u8>().prop_map(MOp::Fork),
        1 => Just(MOp::Flush),
        1 => Just(MOp::Sync),
        1 => Just(MOp::DropCaches),
        1 => Just(MOp::Reopen),
        2 => any::<u8>().prop_map(MOp::Compare),
    ]
}

fn comp(c: u8) -> [u8; 1] {
    [b'a' + c]
}

struct Held {
    ino: Ino,
    handle: FileHandle,
}

struct Fs {
    fs: Arc<dyn Vfs>,
    held: Vec<Option<Held>>,
}

impl Fs {
    fn new(fs: Arc<dyn Vfs>) -> Self {
        Self {
            fs,
            held: Vec::new(),
        }
    }
}

fn walk(fs: &dyn Vfs, comps: &[u8], held: &mut Vec<Ino>) -> Result<Ino, Error> {
    let mut cur = ROOT_INO;
    for c in comps {
        let a = fs.lookup(cur, &comp(*c))?;
        held.push(a.ino);
        cur = a.ino;
    }
    Ok(cur)
}

fn split(p: &[u8]) -> (&[u8], u8) {
    (&p[..p.len() - 1], p[p.len() - 1])
}

type Out = Result<Vec<u8>, Error>;

fn apply(side: &mut Fs, op: &MOp) -> Out {
    let mut refs = Vec::new();
    let r = apply_inner(side, op, &mut refs);
    for i in refs {
        side.fs.forget(i, 1);
    }
    r
}

fn held_key(op: &MOp) -> Option<u8> {
    match op {
        MOp::HeldRead(k, ..) | MOp::HeldWrite(k, ..) | MOp::HeldStat(k) | MOp::HeldRelease(k) => {
            Some(*k)
        }
        _ => None,
    }
}

fn held_of(side: &Fs, k: u8) -> Option<(Ino, FileHandle)> {
    let live: Vec<&Held> = side.held.iter().flatten().collect();
    if live.is_empty() {
        return None;
    }
    let h = live[k as usize % live.len()];
    Some((h.ino, h.handle))
}

fn apply_inner(side: &mut Fs, op: &MOp, refs: &mut Vec<Ino>) -> Out {
    let fs = side.fs.clone();
    let fs = &*fs;
    let unit = |r: Result<(), Error>| r.map(|()| Vec::new());
    match op {
        MOp::Create(p, mode) => {
            let (pp, n) = split(p);
            let parent = walk(fs, pp, refs)?;
            let a = fs.create(parent, &comp(n), u32::from(*mode))?;
            refs.push(a.ino);
            Ok(Vec::new())
        }
        MOp::Mkdir(p, mode) => {
            let (pp, n) = split(p);
            let parent = walk(fs, pp, refs)?;
            let a = fs.mkdir(parent, &comp(n), u32::from(*mode))?;
            refs.push(a.ino);
            Ok(Vec::new())
        }
        MOp::Mknod(p, mode, k, dev) => {
            let (pp, n) = split(p);
            let parent = walk(fs, pp, refs)?;
            let kind = [
                FileKind::Fifo,
                FileKind::Socket,
                FileKind::CharDevice,
                FileKind::BlockDevice,
            ][usize::from(*k % 4)];
            let rdev = if kind.is_device() {
                cowfs_vfs::makedev(u32::from(*dev), u32::from(*dev) * 3 + 1)
            } else {
                0
            };
            let a = fs.mknod(parent, &comp(n), kind, u32::from(*mode), rdev)?;
            refs.push(a.ino);
            Ok(Vec::new())
        }
        MOp::Symlink(p, t) => {
            let (pp, n) = split(p);
            let parent = walk(fs, pp, refs)?;
            let a = fs.symlink(parent, &comp(n), &comp(*t))?;
            refs.push(a.ino);
            Ok(Vec::new())
        }
        MOp::Link(from, to) => {
            let src = walk(fs, from, refs)?;
            let (tp, tn) = split(to);
            let parent = walk(fs, tp, refs)?;
            let a = fs.link(src, parent, &comp(tn))?;
            refs.push(a.ino);
            Ok(Vec::new())
        }
        MOp::Unlink(p) => {
            let (pp, n) = split(p);
            let parent = walk(fs, pp, refs)?;
            unit(fs.unlink(parent, &comp(n)))
        }
        MOp::Rmdir(p) => {
            let (pp, n) = split(p);
            let parent = walk(fs, pp, refs)?;
            unit(fs.rmdir(parent, &comp(n)))
        }
        MOp::Rename(a, b, no_replace) => {
            let (ap, an) = split(a);
            let src = walk(fs, ap, refs)?;
            let (bp, bn) = split(b);
            let dst = walk(fs, bp, refs)?;
            unit(fs.rename(
                src,
                &comp(an),
                dst,
                &comp(bn),
                RenameFlags {
                    no_replace: *no_replace,
                },
            ))
        }
        MOp::Write(p, o, l, s) => {
            let ino = walk(fs, p, refs)?;
            let n = fs.write(ino, u64::from(*o), &pattern(*l as usize, u64::from(*s)))?;
            assert_eq!(n, *l);
            Ok(Vec::new())
        }
        MOp::Truncate(p, size) => {
            let ino = walk(fs, p, refs)?;
            fs.setattr(
                ino,
                SetAttr {
                    size: Some(u64::from(*size)),
                    ..SetAttr::default()
                },
            )
            .map(|_| Vec::new())
        }
        MOp::SetMode(p, m) => {
            let ino = walk(fs, p, refs)?;
            fs.setattr(
                ino,
                SetAttr {
                    mode: Some(u32::from(*m)),
                    ..SetAttr::default()
                },
            )
            .map(|_| Vec::new())
        }
        MOp::Read(p, o, l) => {
            let ino = walk(fs, p, refs)?;
            fs.read(ino, u64::from(*o), *l)
        }
        MOp::Xattr(p, n, v) => {
            let ino = walk(fs, p, refs)?;
            let name = format!("user.x{n}").into_bytes();
            match v {
                Some(v) => unit(fs.setxattr(ino, &name, &[*v], XattrFlags::default())),
                None => unit(fs.removexattr(ino, &name)),
            }
        }
        MOp::Hold(p) => {
            let ino = walk(fs, p, refs)?;
            let handle = fs.open(ino)?;
            side.held.push(Some(Held { ino, handle }));
            Ok(Vec::new())
        }
        MOp::HeldRead(k, o, l) => match held_of(side, *k) {
            Some((ino, _)) => fs.read(ino, u64::from(*o), *l),
            None => Ok(Vec::new()),
        },
        MOp::HeldWrite(k, o, l, s) => match held_of(side, *k) {
            Some((ino, _)) => fs
                .write(ino, u64::from(*o), &pattern(*l as usize, u64::from(*s)))
                .map(|_| Vec::new()),
            None => Ok(Vec::new()),
        },
        MOp::HeldStat(k) => match held_of(side, *k) {
            Some((ino, _)) => fs.getattr(ino).map(|a| {
                let mut v = a.size.to_le_bytes().to_vec();
                v.extend(a.nlink.to_le_bytes());
                v.extend(a.mode.to_le_bytes());
                v
            }),
            None => Ok(Vec::new()),
        },
        MOp::HeldRelease(k) => {
            let live: Vec<usize> = side
                .held
                .iter()
                .enumerate()
                .filter(|(_, h)| h.is_some())
                .map(|(i, _)| i)
                .collect();
            if live.is_empty() {
                return Ok(Vec::new());
            }
            let i = live[*k as usize % live.len()];
            if let Some(h) = side.held[i].take() {
                fs.release(h.handle)?;
                fs.forget(h.ino, 0);
            }
            Ok(Vec::new())
        }
        MOp::Fsync(p) => {
            let ino = walk(fs, p, refs)?;
            unit(fs.fsync(ino, false))
        }
        _ => Ok(Vec::new()),
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Entry {
    kind: u8,
    mode: u32,
    nlink: u32,
    size: u64,
    digest: [u8; 32],
    target: Vec<u8>,
    xattrs: BTreeMap<Vec<u8>, Vec<u8>>,
    group: usize,
}

fn dump(fs: &dyn Vfs) -> BTreeMap<Vec<u8>, Entry> {
    let mut out = BTreeMap::new();
    let mut inos: HashMap<Vec<u8>, Ino> = HashMap::new();
    let mut stack = vec![(ROOT_INO, Vec::new())];
    while let Some((dir, prefix)) = stack.pop() {
        let mut cookie = 0;
        let mut entries = Vec::new();
        loop {
            let r = fs.readdir(dir, cookie, 7).expect("readdir");
            if let Some(l) = r.entries.last() {
                cookie = l.cookie;
            }
            entries.extend(r.entries);
            if r.eof {
                break;
            }
        }
        for e in entries {
            let mut p = prefix.clone();
            p.push(b'/');
            p.extend(&e.name);
            let a = fs.lookup(dir, &e.name).expect("lookup of a listed name");
            assert_eq!(
                a.ino, e.ino,
                "readdir and lookup disagree on the inode of {p:?}"
            );
            inos.insert(p.clone(), a.ino);
            let group = 0;
            let (digest, target) = match a.kind {
                FileKind::Regular => {
                    let mut data = Vec::new();
                    while (data.len() as u64) < a.size {
                        let got = fs.read(a.ino, data.len() as u64, 1 << 20).expect("read");
                        assert!(!got.is_empty(), "short file {p:?}");
                        data.extend(got);
                    }
                    (*blake(&data).as_bytes(), Vec::new())
                }
                FileKind::Symlink => ([0; 32], fs.readlink(a.ino).expect("readlink")),
                FileKind::Directory | _ => ([0; 32], Vec::new()),
            };
            let mut xattrs = BTreeMap::new();
            for n in fs.listxattr(a.ino).expect("listxattr") {
                let v = fs.getxattr(a.ino, &n).expect("getxattr");
                xattrs.insert(n, v);
            }
            if a.kind == FileKind::Directory {
                stack.push((a.ino, p.clone()));
            }
            out.insert(
                p,
                Entry {
                    kind: a.kind as u8,
                    mode: a.mode,
                    nlink: a.nlink,
                    size: if a.kind == FileKind::Directory {
                        0
                    } else {
                        a.size
                    },
                    digest,
                    target,
                    xattrs,
                    group,
                },
            );
            fs.forget(a.ino, 1);
        }
    }
    // hardlink groups are numbered in path order, because listing order after a rename may differ
    let mut groups: HashMap<Ino, usize> = HashMap::new();
    for (p, e) in &mut out {
        let next = groups.len();
        e.group = *groups.entry(inos[p]).or_insert(next);
    }
    out
}

fn blake(data: &[u8]) -> cowfs_store::BlockId {
    cowfs_store::BlockId::of(data)
}

struct Side {
    name: String,
    core: Fs,
    mem: Fs,
    log: Vec<MOp>,
}

fn release_side(side: &mut Fs) {
    for h in side.held.drain(..).flatten() {
        side.fs.release(h.handle).unwrap();
        side.fs.forget(h.ino, 0);
    }
}

struct World {
    dir: tempfile::TempDir,
    opts: Options,
    core: Option<Core>,
    sides: Vec<Side>,
    next_name: usize,
}

impl World {
    fn new(opts: Options) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let core = Core::open(dir.path(), opts.clone()).unwrap();
        let mut w = Self {
            dir,
            opts,
            core: Some(core),
            sides: Vec::new(),
            next_name: 0,
        };
        w.c().create_snapshot("s0").unwrap();
        w.sides.push(w.make_side("s0".into(), Vec::new()));
        w.next_name = 1;
        w
    }

    fn c(&self) -> &Core {
        self.core.as_ref().unwrap()
    }

    fn make_side(&self, name: String, log: Vec<MOp>) -> Side {
        let view = self.c().snapshot_view(&name).unwrap();
        let mem = Arc::new(MemVfs::new());
        let mut side = Side {
            name,
            core: Fs::new(Arc::new(view) as Arc<dyn Vfs>),
            mem: Fs::new(mem as Arc<dyn Vfs>),
            log: Vec::new(),
        };
        for op in log {
            if matches!(op, MOp::Reopen) {
                release_side(&mut side.mem);
            } else {
                let _ = apply(&mut side.mem, &op);
            }
            side.log.push(op);
        }
        release_side(&mut side.mem);
        side
    }

    fn compare(&self, i: usize, what: &str) {
        let s = &self.sides[i];
        let a = dump(&*s.core.fs);
        let b = dump(&*s.mem.fs);
        assert_eq!(a, b, "tree of snapshot {} differs after {what}", s.name);
    }

    fn run(&mut self, ops: &[MOp]) {
        for (n, op) in ops.iter().enumerate() {
            let what = format!("op {n}: {op:?}");
            match op {
                MOp::Fork(k) => {
                    if self.sides.len() >= 4 {
                        continue;
                    }
                    let src = *k as usize % self.sides.len();
                    let name = format!("s{}", self.next_name);
                    self.next_name += 1;
                    self.c()
                        .fork_snapshot(&self.sides[src].name, &name)
                        .unwrap();
                    let log = self.sides[src].log.clone();
                    let side = self.make_side(name, log);
                    self.sides.push(side);
                    self.compare(self.sides.len() - 1, &what);
                }
                MOp::Flush => self.c().flush().unwrap(),
                MOp::Sync => self.c().sync().unwrap(),
                MOp::DropCaches => self.c().drop_caches(),
                MOp::Compare(k) => self.compare(*k as usize % self.sides.len(), &what),
                MOp::Reopen => self.reopen(),
                _ => {
                    let i = op_side(op, self.sides.len()).unwrap_or_default();
                    let s = &mut self.sides[i];
                    // a held op with no live handle on this side changed nothing, so it must not
                    // enter the log: replaying it in a later fork would give it a handle the
                    // snapshot never had and make the memory model diverge from the core
                    let effective = held_key(op).is_none_or(|k| held_of(&s.core, k).is_some());
                    let a = apply(&mut s.core, op);
                    let b = apply(&mut s.mem, op);
                    assert_eq!(a, b, "snapshot {} result differs at {what}", s.name);
                    if !op.session_only() && effective {
                        s.log.push(op.clone());
                    }
                }
            }
        }
        for i in 0..self.sides.len() {
            self.compare(i, "the last operation");
        }
        self.c().check().unwrap();
        for i in 0..self.sides.len() {
            self.release_all(i);
        }
    }

    fn release_all(&mut self, i: usize) {
        let s = &mut self.sides[i];
        release_side(&mut s.core);
        release_side(&mut s.mem);
    }

    fn reopen(&mut self) {
        for i in 0..self.sides.len() {
            self.release_all(i);
        }
        self.c().sync().unwrap();
        for s in &mut self.sides {
            s.log.push(MOp::Reopen);
        }
        let names: Vec<String> = self.sides.iter().map(|s| s.name.clone()).collect();
        let kept: Vec<(Fs, Vec<MOp>)> = self.sides.drain(..).map(|s| (s.mem, s.log)).collect();
        self.core = None;
        self.core = Some(Core::open(self.dir.path(), self.opts.clone()).unwrap());
        for (name, (mem, log)) in names.into_iter().zip(kept) {
            let view = self.c().snapshot_view(&name).unwrap();
            self.sides.push(Side {
                name,
                core: Fs::new(Arc::new(view) as Arc<dyn Vfs>),
                mem,
                log,
            });
        }
    }
}

/// Which snapshot an operation targets: the first path byte picks it, so a sequence spreads
/// over all snapshots.
fn op_side(op: &MOp, n: usize) -> Option<usize> {
    let p = match op {
        MOp::Create(p, _)
        | MOp::Mkdir(p, _)
        | MOp::Symlink(p, _)
        | MOp::Mknod(p, ..)
        | MOp::Unlink(p)
        | MOp::Rmdir(p)
        | MOp::Truncate(p, _)
        | MOp::SetMode(p, _)
        | MOp::Read(p, _, _)
        | MOp::Xattr(p, _, _)
        | MOp::Hold(p)
        | MOp::Fsync(p)
        | MOp::Write(p, _, _, _)
        | MOp::Link(p, _)
        | MOp::Rename(p, _, _) => p,
        MOp::HeldRead(k, ..) | MOp::HeldWrite(k, ..) | MOp::HeldStat(k) | MOp::HeldRelease(k) => {
            return Some(*k as usize % n)
        }
        _ => return None,
    };
    let last = p.last().copied().unwrap_or(0) as usize;
    Some((last + p.len()) % n)
}

fn small_opts() -> Options {
    Options {
        background: false,
        max_pending_ops: 16,
        file_flush_bytes: 64 << 10,
        node_cache: 256,
        dentry_cache: 256,
        block_cache_bytes: 1 << 20,
        ..Options::default()
    }
}

fn big_opts() -> Options {
    Options {
        background: false,
        ..Options::default()
    }
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 32,
        max_shrink_iters: 300,
        ..ProptestConfig::default()
    })]

    #[test]
    fn core_matches_memvfs_with_tiny_caches(ops in prop::collection::vec(op(), 1..70)) {
        let mut w = World::new(small_opts());
        w.run(&ops);
    }

    #[test]
    fn core_matches_memvfs_with_default_options(ops in prop::collection::vec(op(), 1..70)) {
        let mut w = World::new(big_opts());
        w.run(&ops);
    }
}
