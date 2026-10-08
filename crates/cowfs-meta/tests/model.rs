//! Model-based test: random operation sequences against an in-memory POSIX tree.

use cowfs_meta::{
    BlockId, ChunkRef, Error, FileType, Ino, Meta, Options, Removed, SetAttr, Timestamp,
};
use proptest::prelude::*;
use std::collections::BTreeMap;

const NAMES: [&[u8]; 10] = [b"a", b"b", b"c", b"d", b"e", b"f", b"g", b"", b".", b"x/y"];
const MISSING: u64 = u64::MAX / 2;

type R<T> = Result<T, &'static str>;

fn kind_name(e: &Error) -> &'static str {
    match e {
        Error::NotFound => "NotFound",
        Error::Exists => "Exists",
        Error::NotDir => "NotDir",
        Error::IsDir => "IsDir",
        Error::NotEmpty => "NotEmpty",
        Error::Invalid(_) => "Invalid",
        Error::NeedsRechunk => "NeedsRechunk",
        Error::Conflict => "Conflict",
        Error::NameTooLong => "NameTooLong",
        Error::NoAttr => "NoAttr",
        Error::TooBig => "TooBig",
        Error::NoSuchSnapshot => "NoSuchSnapshot",
        Error::SnapshotExists => "SnapshotExists",
        other => panic!("unexpected error {other}"),
    }
}

#[derive(Clone, Debug)]
struct MI {
    kind: FileType,
    mode: u32,
    size: u64,
    chunks: Vec<ChunkRef>,
    xattrs: BTreeMap<Vec<u8>, Vec<u8>>,
    target: Vec<u8>,
    entries: Vec<(Vec<u8>, u64)>,
    parent: u64,
    version: u64,
}

impl MI {
    fn new(kind: FileType, mode: u32, parent: u64) -> Self {
        Self {
            kind,
            mode: mode & 0o7777,
            size: 0,
            chunks: Vec::new(),
            xattrs: BTreeMap::new(),
            target: Vec::new(),
            entries: Vec::new(),
            parent,
            version: 0,
        }
    }
}

#[derive(Clone, Debug)]
struct MTree {
    inodes: BTreeMap<u64, MI>,
}

impl MTree {
    fn new() -> Self {
        let mut inodes = BTreeMap::new();
        inodes.insert(1, MI::new(FileType::Dir, 0o755, 1));
        Self { inodes }
    }

    fn nlink(&self, ino: u64) -> u32 {
        let i = &self.inodes[&ino];
        if i.kind == FileType::Dir {
            2 + i
                .entries
                .iter()
                .filter(|(_, c)| self.inodes[c].kind == FileType::Dir)
                .count() as u32
        } else {
            self.inodes
                .values()
                .flat_map(|d| d.entries.iter())
                .filter(|(_, c)| *c == ino)
                .count() as u32
        }
    }

    fn ino(&self, ino: u64) -> R<&MI> {
        self.inodes.get(&ino).ok_or("NotFound")
    }

    fn dir(&self, ino: u64) -> R<&MI> {
        let i = self.ino(ino)?;
        if i.kind == FileType::Dir {
            Ok(i)
        } else {
            Err("NotDir")
        }
    }

    fn entry(&self, dir: u64, name: &[u8]) -> Option<u64> {
        self.inodes[&dir]
            .entries
            .iter()
            .find(|(n, _)| n == name)
            .map(|e| e.1)
    }

    fn removed(&self, ino: u64, freed: bool, nlink: u32) -> RemovedM {
        let i = &self.inodes[&ino];
        RemovedM {
            ino,
            kind: i.kind,
            mode: i.mode,
            size: i.size,
            nlink,
            freed,
            chunks: if freed && i.kind == FileType::File {
                i.chunks.clone()
            } else {
                Vec::new()
            },
        }
    }

    fn detach(&mut self, dir: u64, name: &[u8]) {
        let d = self.inodes.get_mut(&dir).unwrap();
        d.entries.retain(|(n, _)| n != name);
    }

    fn attach(&mut self, dir: u64, name: &[u8], child: u64) {
        self.inodes
            .get_mut(&dir)
            .unwrap()
            .entries
            .push((name.to_vec(), child));
    }

    /// Drops one name of a non-directory; returns (freed, nlink after).
    fn unref(&mut self, ino: u64) -> RemovedM {
        let n = self.nlink(ino);
        let freed = n == 0;
        let r = self.removed(ino, freed, n);
        if freed {
            self.inodes.remove(&ino);
        }
        r
    }
}

#[derive(Debug, PartialEq)]
struct RemovedM {
    ino: u64,
    kind: FileType,
    mode: u32,
    size: u64,
    nlink: u32,
    freed: bool,
    chunks: Vec<ChunkRef>,
}

fn removed_m(r: &Removed) -> RemovedM {
    RemovedM {
        ino: r.attr.ino.0,
        kind: r.attr.kind,
        mode: r.attr.mode,
        size: r.attr.size,
        nlink: r.attr.nlink,
        freed: r.freed,
        chunks: r.chunks.clone(),
    }
}

fn valid_name(name: &[u8]) -> R<()> {
    if name.is_empty() || name == b"." || name == b".." {
        return Err("Invalid");
    }
    if name.len() > 255 {
        return Err("NameTooLong");
    }
    if name.iter().any(|&c| c == b'/' || c == 0) {
        return Err("Invalid");
    }
    Ok(())
}

struct Model {
    snaps: BTreeMap<String, MTree>,
    next_ino: u64,
    next_snap: u32,
}

impl Model {
    fn new() -> Self {
        let mut snaps = BTreeMap::new();
        snaps.insert("s0".to_string(), MTree::new());
        Self {
            snaps,
            next_ino: 2,
            next_snap: 1,
        }
    }

    fn create(&mut self, s: &str, dir: u64, name: &[u8], kind: FileType, target: &[u8]) -> R<u64> {
        valid_name(name)?;
        let next = self.next_ino;
        let t = self.snaps.get_mut(s).unwrap();
        t.dir(dir)?;
        if t.entry(dir, name).is_some() {
            return Err("Exists");
        }
        if kind == FileType::Symlink {
            if target.is_empty() {
                return Err("Invalid");
            }
            if target.len() > 4096 {
                return Err("TooBig");
            }
        }
        let mode = match kind {
            FileType::Symlink => 0o777,
            FileType::Dir => 0o755,
            FileType::File => 0o644,
        };
        let mut i = MI::new(kind, mode, dir);
        if kind == FileType::Symlink {
            i.size = target.len() as u64;
            i.target = target.to_vec();
        }
        t.inodes.insert(next, i);
        t.attach(dir, name, next);
        self.next_ino += 1;
        Ok(next)
    }

    fn link(&mut self, s: &str, ino: u64, dir: u64, name: &[u8]) -> R<()> {
        valid_name(name)?;
        let t = self.snaps.get_mut(s).unwrap();
        let rec = t.ino(ino)?;
        t.dir(dir)?;
        if rec.kind == FileType::Dir {
            return Err("Invalid");
        }
        if t.entry(dir, name).is_some() {
            return Err("Exists");
        }
        t.attach(dir, name, ino);
        Ok(())
    }

    fn unlink(&mut self, s: &str, dir: u64, name: &[u8]) -> R<RemovedM> {
        valid_name(name)?;
        let t = self.snaps.get_mut(s).unwrap();
        t.dir(dir)?;
        let child = t.entry(dir, name).ok_or("NotFound")?;
        if t.inodes[&child].kind == FileType::Dir {
            return Err("IsDir");
        }
        t.detach(dir, name);
        Ok(t.unref(child))
    }

    fn rmdir(&mut self, s: &str, dir: u64, name: &[u8]) -> R<RemovedM> {
        valid_name(name)?;
        let t = self.snaps.get_mut(s).unwrap();
        t.dir(dir)?;
        let child = t.entry(dir, name).ok_or("NotFound")?;
        if t.inodes[&child].kind != FileType::Dir {
            return Err("NotDir");
        }
        if !t.inodes[&child].entries.is_empty() {
            return Err("NotEmpty");
        }
        t.detach(dir, name);
        let r = t.removed(child, true, 0);
        t.inodes.remove(&child);
        Ok(r)
    }

    fn rename(
        &mut self,
        s: &str,
        fd: u64,
        fname: &[u8],
        td: u64,
        tname: &[u8],
    ) -> R<Option<RemovedM>> {
        valid_name(fname)?;
        valid_name(tname)?;
        let t = self.snaps.get_mut(s).unwrap();
        t.dir(fd)?;
        t.dir(td)?;
        let src = t.entry(fd, fname).ok_or("NotFound")?;
        if fd == td && fname == tname {
            return Ok(None);
        }
        let skind = t.inodes[&src].kind;
        let dst = t.entry(td, tname);
        if let Some(d) = dst {
            if d == src {
                return Ok(None);
            }
            match (skind, t.inodes[&d].kind) {
                (FileType::Dir, FileType::Dir) => {
                    if !t.inodes[&d].entries.is_empty() {
                        return Err("NotEmpty");
                    }
                }
                (FileType::Dir, _) => return Err("NotDir"),
                (_, FileType::Dir) => return Err("IsDir"),
                _ => {}
            }
        }
        if skind == FileType::Dir {
            let mut cur = td;
            loop {
                if cur == src {
                    return Err("Invalid");
                }
                if cur == 1 {
                    break;
                }
                cur = t.inodes[&cur].parent;
            }
        }
        let mut replaced = None;
        if let Some(d) = dst {
            t.detach(td, tname);
            replaced = Some(if t.inodes[&d].kind == FileType::Dir {
                let r = t.removed(d, true, 0);
                t.inodes.remove(&d);
                r
            } else {
                t.unref(d)
            });
        }
        if fd == td {
            let d = t.inodes.get_mut(&fd).unwrap();
            let at = d.entries.iter().position(|(n, _)| n == fname).unwrap();
            d.entries[at].0 = tname.to_vec();
        } else {
            t.detach(fd, fname);
            t.attach(td, tname, src);
        }
        if skind == FileType::Dir {
            t.inodes.get_mut(&src).unwrap().parent = td;
        }
        Ok(replaced)
    }

    fn set_content(&mut self, s: &str, ino: u64, chunks: &[ChunkRef], size: u64) -> R<()> {
        let t = self.snaps.get_mut(s).unwrap();
        match t.ino(ino)?.kind {
            FileType::Dir => return Err("IsDir"),
            FileType::Symlink => return Err("Invalid"),
            FileType::File => {}
        }
        let total: u64 = chunks.iter().map(|c| u64::from(c.len)).sum();
        if size < total {
            return Err("Invalid");
        }
        let i = t.inodes.get_mut(&ino).unwrap();
        i.chunks = chunks.to_vec();
        i.size = size;
        i.version += 1;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn splice(
        &mut self,
        s: &str,
        ino: u64,
        expected: u64,
        start: u64,
        end: u64,
        new: &[ChunkRef],
        size: u64,
    ) -> R<u64> {
        let t = self.snaps.get_mut(s).unwrap();
        match t.ino(ino)?.kind {
            FileType::Dir => return Err("IsDir"),
            FileType::Symlink => return Err("Invalid"),
            FileType::File => {}
        }
        let i = t.inodes.get_mut(&ino).unwrap();
        if i.version != expected {
            return Err("Conflict");
        }
        let covered: u64 = i.chunks.iter().map(|c| u64::from(c.len)).sum();
        if start > end || end > covered {
            return Err("Invalid");
        }
        let mut bounds = vec![0u64];
        for c in &i.chunks {
            bounds.push(bounds.last().unwrap() + u64::from(c.len));
        }
        if start != end && !(bounds.contains(&start) && bounds.contains(&end)) {
            return Err("Invalid");
        }
        let new_len: u64 = new.iter().map(|c| u64::from(c.len)).sum();
        if end < covered && new_len != end - start {
            return Err("Invalid");
        }
        let nc = covered - (end - start) + new_len;
        if size < nc {
            return Err("Invalid");
        }
        if start != end || !new.is_empty() {
            let from = bounds.iter().position(|b| *b == start).unwrap_or(0);
            let to = bounds.iter().position(|b| *b == end).unwrap_or(from);
            i.chunks.splice(from..to, new.iter().copied());
        }
        i.size = size;
        i.version += 1;
        Ok(i.version)
    }

    fn setattr(&mut self, s: &str, ino: u64, set: &SetAttr) -> R<()> {
        let t = self.snaps.get_mut(s).unwrap();
        let kind = t.ino(ino)?.kind;
        if let Some(size) = set.size {
            match kind {
                FileType::Dir => return Err("IsDir"),
                FileType::Symlink => return Err("Invalid"),
                FileType::File => {}
            }
            let i = t.inodes.get_mut(&ino).unwrap();
            if size != i.size {
                let total: u64 = i.chunks.iter().map(|c| u64::from(c.len)).sum();
                if size < total {
                    let mut acc = 0;
                    let mut keep = if size == 0 { Some(0) } else { None };
                    for (n, c) in i.chunks.iter().enumerate() {
                        if keep.is_some() || acc >= size {
                            break;
                        }
                        acc += u64::from(c.len);
                        if acc == size {
                            keep = Some(n + 1);
                        }
                    }
                    let keep = keep.ok_or("NeedsRechunk")?;
                    i.chunks.truncate(keep);
                }
                i.size = size;
                i.version += 1;
            }
        }
        if let Some(m) = set.mode {
            t.inodes.get_mut(&ino).unwrap().mode = m & 0o7777;
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
enum Op {
    Create {
        s: usize,
        d: usize,
        n: usize,
        k: u8,
        t: usize,
    },
    SetContent {
        s: usize,
        i: usize,
        cnt: usize,
        seed: u8,
        hole: u8,
    },
    Splice {
        s: usize,
        i: usize,
        a: usize,
        b: usize,
        skew: bool,
        cnt: usize,
        seed: u8,
        hole: u8,
        stale: bool,
    },
    Rename {
        s: usize,
        fd: usize,
        fnm: usize,
        td: usize,
        tn: usize,
    },
    Link {
        s: usize,
        i: usize,
        d: usize,
        n: usize,
    },
    Unlink {
        s: usize,
        d: usize,
        n: usize,
    },
    Rmdir {
        s: usize,
        d: usize,
        n: usize,
    },
    SetAttr {
        s: usize,
        i: usize,
        mode: Option<u32>,
        size: Option<u8>,
        mtime: Option<i64>,
    },
    SetX {
        s: usize,
        i: usize,
        n: usize,
        v: Vec<u8>,
    },
    RmX {
        s: usize,
        i: usize,
        n: usize,
    },
    Fork {
        s: usize,
    },
    RemoveSnap {
        s: usize,
    },
    Reopen,
}

fn op() -> impl Strategy<Value = Op> {
    let u = 0usize..1000;
    prop_oneof![
        6 => (u.clone(), u.clone(), u.clone(), 0u8..6, 0usize..4)
            .prop_map(|(s, d, n, k, t)| Op::Create { s, d, n, k, t }),
        4 => (u.clone(), u.clone(), 0usize..6, any::<u8>(), 0u8..4)
            .prop_map(|(s, i, cnt, seed, hole)| Op::SetContent { s, i, cnt, seed, hole }),
        4 => (u.clone(), u.clone(), 0usize..8, 0usize..8, any::<bool>(), 0usize..4, any::<u8>(), 0u8..3, proptest::bool::weighted(0.15))
            .prop_map(|(s, i, a, b, skew, cnt, seed, hole, stale)| Op::Splice { s, i, a, b, skew, cnt, seed, hole, stale }),
        3 => (u.clone(), u.clone(), u.clone(), u.clone(), u.clone())
            .prop_map(|(s, fd, fnm, td, tn)| Op::Rename { s, fd, fnm, td, tn }),
        3 => (u.clone(), u.clone(), u.clone(), u.clone())
            .prop_map(|(s, i, d, n)| Op::Link { s, i, d, n }),
        3 => (u.clone(), u.clone(), u.clone()).prop_map(|(s, d, n)| Op::Unlink { s, d, n }),
        2 => (u.clone(), u.clone(), u.clone()).prop_map(|(s, d, n)| Op::Rmdir { s, d, n }),
        2 => (u.clone(), u.clone(), proptest::option::of(0u32..0o10000),
              proptest::option::of(0u8..12), proptest::option::of(0i64..1000))
            .prop_map(|(s, i, mode, size, mtime)| Op::SetAttr { s, i, mode, size, mtime }),
        2 => (u.clone(), u.clone(), 0usize..4, proptest::collection::vec(any::<u8>(), 0..4))
            .prop_map(|(s, i, n, v)| Op::SetX { s, i, n, v }),
        1 => (u.clone(), u.clone(), 0usize..4).prop_map(|(s, i, n)| Op::RmX { s, i, n }),
        1 => u.clone().prop_map(|s| Op::Fork { s }),
        1 => u.prop_map(|s| Op::RemoveSnap { s }),
        1 => Just(Op::Reopen),
    ]
}

fn chunks(cnt: usize, seed: u8) -> Vec<ChunkRef> {
    (0..cnt)
        .map(|j| {
            ChunkRef::block(
                BlockId::of(&[seed, j as u8]),
                1 + (j as u32 + u32::from(seed)) % 5,
            )
        })
        .collect()
}

fn xname(n: usize) -> Vec<u8> {
    match n {
        0 => b"user.a".to_vec(),
        1 => b"user.b".to_vec(),
        2 => b"security.c".to_vec(),
        _ => b"user.d".to_vec(),
    }
}

fn pick(t: &MTree, i: usize) -> u64 {
    let keys: Vec<u64> = t.inodes.keys().copied().collect();
    let idx = i % (keys.len() + 1);
    keys.get(idx).copied().unwrap_or(MISSING)
}

fn pick_dir(t: &MTree, i: usize) -> u64 {
    if i.is_multiple_of(16) {
        return pick(t, i / 16);
    }
    let dirs: Vec<u64> = t
        .inodes
        .iter()
        .filter(|(_, v)| v.kind == FileType::Dir)
        .map(|(k, _)| *k)
        .collect();
    dirs[i % dirs.len()]
}

fn same<T: std::fmt::Debug + PartialEq>(
    what: &str,
    real: Result<T, Error>,
    model: R<T>,
) -> Result<(), TestCaseError> {
    let real = real.map_err(|e| kind_name(&e));
    prop_assert_eq!(real, model, "{}", what);
    Ok(())
}

fn verify(m: &Meta, model: &Model) -> Result<(), TestCaseError> {
    prop_assert_eq!(m.snapshots().unwrap().len(), model.snaps.len());
    for (name, t) in &model.snaps {
        let s = m.snapshot(name).unwrap();
        let mut seen = vec![1u64];
        let mut work = vec![1u64];
        while let Some(d) = work.pop() {
            let mi = &t.inodes[&d];
            let mut got = Vec::new();
            let mut cookie = 0;
            loop {
                let page = s.readdir(Ino(d), cookie, 3).unwrap();
                for e in &page.entries {
                    prop_assert!(e.cookie > cookie);
                    cookie = e.cookie;
                    got.push((e.name.clone(), e.ino.0, e.kind));
                }
                if page.end {
                    break;
                }
            }
            let want: Vec<_> = mi
                .entries
                .iter()
                .map(|(n, c)| (n.clone(), *c, t.inodes[c].kind))
                .collect();
            prop_assert_eq!(&got, &want, "listing of {} in {}", d, name);
            for (n, c, kind) in got {
                prop_assert_eq!(s.lookup(Ino(d), &n).unwrap().ino.0, c);
                if !seen.contains(&c) {
                    seen.push(c);
                    if kind == FileType::Dir {
                        work.push(c);
                    }
                }
            }
        }
        seen.sort();
        let want: Vec<u64> = t.inodes.keys().copied().collect();
        prop_assert_eq!(seen, want, "reachable inodes in {}", name);
        for (&ino, mi) in &t.inodes {
            let a = s.getattr(Ino(ino)).unwrap();
            prop_assert_eq!(
                (a.kind, a.mode, a.nlink, a.size),
                (mi.kind, mi.mode, t.nlink(ino), mi.size),
                "attr of {} in {}",
                ino,
                name
            );
            match mi.kind {
                FileType::File => {
                    prop_assert_eq!(&s.chunks(Ino(ino)).unwrap(), &mi.chunks);
                    prop_assert_eq!(s.content_version(Ino(ino)).unwrap(), mi.version);
                }
                FileType::Symlink => prop_assert_eq!(&s.readlink(Ino(ino)).unwrap(), &mi.target),
                FileType::Dir => {
                    prop_assert_eq!(s.lookup(Ino(ino), b"..").unwrap().ino.0, mi.parent);
                }
            }
            let xs = s.listxattr(Ino(ino)).unwrap();
            prop_assert_eq!(&xs, &mi.xattrs.keys().cloned().collect::<Vec<_>>());
            for (k, v) in &mi.xattrs {
                prop_assert_eq!(&s.getxattr(Ino(ino), k).unwrap(), v);
            }
        }
    }
    m.check().unwrap();
    Ok(())
}

fn run(ops: &[Op], node_size: usize) -> Result<(), TestCaseError> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("m.redb");
    let opts = Options {
        node_size,
        sync_every_ops: 7,
        ..Options::default()
    };
    let mut m = Meta::open(&path, opts.clone()).unwrap();
    m.new_snapshot("s0").unwrap();
    let mut model = Model::new();
    for op in ops {
        let names: Vec<String> = model.snaps.keys().cloned().collect();
        let sname = |s: usize| names[s % names.len()].clone();
        match op {
            Op::Reopen => {
                drop(m);
                m = Meta::open(&path, opts.clone()).unwrap();
            }
            Op::Fork { s } => {
                let src = sname(*s);
                let new = format!("s{}", model.next_snap);
                model.next_snap += 1;
                let c = m.snapshot(&src).unwrap().fork(&new).unwrap();
                prop_assert_eq!(c.root().unwrap(), m.snapshot(&src).unwrap().root().unwrap());
                let t = model.snaps[&src].clone();
                model.snaps.insert(new, t);
            }
            Op::RemoveSnap { s } => {
                if names.len() > 1 {
                    let n = sname(*s);
                    let snap = m.snapshot(&n).unwrap();
                    m.remove_snapshot(snap.id()).unwrap();
                    model.snaps.remove(&n);
                    prop_assert!(matches!(m.snapshot(&n), Err(Error::NoSuchSnapshot)));
                }
            }
            Op::Create { s, d, n, k, t } => {
                let sn = sname(*s);
                let snap = m.snapshot(&sn).unwrap();
                let dir = pick_dir(&model.snaps[&sn], *d);
                let name = NAMES[n % NAMES.len()];
                let target: &[u8] = [&b"tgt"[..], b"../x", b"", b"long-target-name"][*t];
                let (kind, real) = match k {
                    0..=2 => (FileType::File, snap.create(Ino(dir), name, 0o644)),
                    3 | 4 => (FileType::Dir, snap.mkdir(Ino(dir), name, 0o755)),
                    _ => (FileType::Symlink, snap.symlink(Ino(dir), name, target)),
                };
                let want = model.create(&sn, dir, name, kind, target);
                if let (Ok(a), Ok(w)) = (&real, &want) {
                    prop_assert_eq!(a.ino.0, *w);
                }
                same("create", real.map(|a| a.ino.0), want)?;
            }
            Op::SetContent {
                s,
                i,
                cnt,
                seed,
                hole,
            } => {
                let sn = sname(*s);
                let snap = m.snapshot(&sn).unwrap();
                let ino = pick(&model.snaps[&sn], *i);
                let cs = chunks(*cnt, *seed);
                let size = cs.iter().map(|c| u64::from(c.len)).sum::<u64>() + u64::from(*hole);
                let real = snap.set_content(Ino(ino), &cs, size);
                same(
                    "set_content",
                    real.map(|_| ()),
                    model.set_content(&sn, ino, &cs, size),
                )?;
            }
            Op::Splice {
                s,
                i,
                a,
                b,
                skew,
                cnt,
                seed,
                hole,
                stale,
            } => {
                let sn = sname(*s);
                let snap = m.snapshot(&sn).unwrap();
                let ino = pick(&model.snaps[&sn], *i);
                let (version, bounds) = match model.snaps[&sn].inodes.get(&ino) {
                    Some(mi) => {
                        let mut bounds = vec![0u64];
                        for c in &mi.chunks {
                            bounds.push(bounds.last().unwrap() + u64::from(c.len));
                        }
                        (mi.version, bounds)
                    }
                    None => (0, vec![0]),
                };
                let (from, to) = (bounds[a % bounds.len()], bounds[b % bounds.len()]);
                let start = from + u64::from(*skew);
                let cs = chunks(*cnt, *seed);
                let new_len: u64 = cs.iter().map(|c| u64::from(c.len)).sum();
                let covered = *bounds.last().unwrap();
                let size =
                    covered.saturating_sub(to.saturating_sub(start)) + new_len + u64::from(*hole);
                let expected = if *stale { version + 1 } else { version };
                let real = snap.splice_content(Ino(ino), expected, start, to, &cs, size);
                same(
                    "splice",
                    real,
                    model.splice(&sn, ino, expected, start, to, &cs, size),
                )?;
            }
            Op::Rename { s, fd, fnm, td, tn } => {
                let sn = sname(*s);
                let snap = m.snapshot(&sn).unwrap();
                let t = &model.snaps[&sn];
                let (fd, td) = (pick_dir(t, *fd), pick_dir(t, *td));
                let (f, to) = (NAMES[fnm % NAMES.len()], NAMES[tn % NAMES.len()]);
                let real = snap
                    .rename(Ino(fd), f, Ino(td), to)
                    .map(|r| r.map(|r| removed_m(&r)));
                same("rename", real, model.rename(&sn, fd, f, td, to))?;
            }
            Op::Link { s, i, d, n } => {
                let sn = sname(*s);
                let snap = m.snapshot(&sn).unwrap();
                let t = &model.snaps[&sn];
                let (ino, dir) = (pick(t, *i), pick_dir(t, *d));
                let name = NAMES[n % NAMES.len()];
                let real = snap.link(Ino(ino), Ino(dir), name).map(|_| ());
                same("link", real, model.link(&sn, ino, dir, name))?;
            }
            Op::Unlink { s, d, n } => {
                let sn = sname(*s);
                let snap = m.snapshot(&sn).unwrap();
                let dir = pick_dir(&model.snaps[&sn], *d);
                let name = NAMES[n % NAMES.len()];
                let real = snap.unlink(Ino(dir), name).map(|r| removed_m(&r));
                same("unlink", real, model.unlink(&sn, dir, name))?;
            }
            Op::Rmdir { s, d, n } => {
                let sn = sname(*s);
                let snap = m.snapshot(&sn).unwrap();
                let dir = pick_dir(&model.snaps[&sn], *d);
                let name = NAMES[n % NAMES.len()];
                let real = snap.rmdir(Ino(dir), name).map(|r| removed_m(&r));
                same("rmdir", real, model.rmdir(&sn, dir, name))?;
            }
            Op::SetAttr {
                s,
                i,
                mode,
                size,
                mtime,
            } => {
                let sn = sname(*s);
                let snap = m.snapshot(&sn).unwrap();
                let ino = pick(&model.snaps[&sn], *i);
                let set = SetAttr {
                    mode: *mode,
                    size: size.map(u64::from),
                    mtime: mtime.map(|secs| Timestamp { secs, nanos: 0 }),
                    atime: mtime.map(|secs| Timestamp {
                        secs: secs + 1,
                        nanos: 5,
                    }),
                };
                let real = snap.setattr(Ino(ino), set);
                if let Ok(a) = &real {
                    if let (Some(mt), Some(at)) = (set.mtime, set.atime) {
                        prop_assert_eq!((a.mtime, a.atime), (mt, at));
                    }
                }
                same("setattr", real.map(|_| ()), model.setattr(&sn, ino, &set))?;
            }
            Op::SetX { s, i, n, v } => {
                let sn = sname(*s);
                let snap = m.snapshot(&sn).unwrap();
                let ino = pick(&model.snaps[&sn], *i);
                let name = xname(*n);
                let real = snap.setxattr(Ino(ino), &name, v);
                let want = match model.snaps.get_mut(&sn).unwrap().inodes.get_mut(&ino) {
                    Some(mi) => {
                        mi.xattrs.insert(name, v.clone());
                        Ok(())
                    }
                    None => Err("NotFound"),
                };
                same("setxattr", real, want)?;
            }
            Op::RmX { s, i, n } => {
                let sn = sname(*s);
                let snap = m.snapshot(&sn).unwrap();
                let ino = pick(&model.snaps[&sn], *i);
                let name = xname(*n);
                let real = snap.removexattr(Ino(ino), &name);
                let want = match model.snaps.get_mut(&sn).unwrap().inodes.get_mut(&ino) {
                    Some(mi) => mi.xattrs.remove(&name).map(|_| ()).ok_or("NoAttr"),
                    None => Err("NotFound"),
                };
                same("removexattr", real, want)?;
            }
        }
        verify(&m, &model)?;
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 40, failure_persistence: None, ..ProptestConfig::default() })]

    #[test]
    fn matches_posix_model_small_nodes(ops in proptest::collection::vec(op(), 1..70)) {
        run(&ops, 256)?;
    }

    #[test]
    fn matches_posix_model_default_nodes(ops in proptest::collection::vec(op(), 1..70)) {
        run(&ops, 4096)?;
    }
}
