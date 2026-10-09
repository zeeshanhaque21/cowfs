//! An independent, deliberately simple oracle for `Vfs` semantics, and a driver that runs
//! random operation sequences against a backend and the oracle in lockstep.
//!
//! The oracle is a plain in-memory tree addressed by path. It shares no code with `MemVfs`:
//! files are `Rc` cells (a hardlink is another `Rc` to the same cell, `nlink` is the strong
//! count), names are single bytes, and there are no inode numbers, handles or timestamps.
//!
//! When the trait does not say which error wins if several apply (for example `link` of a
//! directory onto an existing name), the oracle returns every applicable error and the
//! backend may return any of them. Success is required exactly when none applies. Path
//! resolution errors are exclusive and ordered (a file in the middle of a path is `NotDir`).

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use cowfs_vfs::{Error, FileKind, Ino, RenameFlags, SetAttr, Vfs, XattrFlags, MODE_MASK, ROOT_INO};

use crate::conformance::pattern;

/// One operation. A path is a list of name indexes, component `i` is the byte `b'a' + i`.
/// Paths are never empty, never contain `.` or `..`, and links are never followed.
#[derive(Clone, Debug)]
pub enum Op {
    Create(Vec<u8>, u16),
    Mkdir(Vec<u8>, u16),
    /// The target is the one-letter name given by the index.
    Symlink(Vec<u8>, u8),
    Link(Vec<u8>, Vec<u8>),
    Unlink(Vec<u8>),
    Rmdir(Vec<u8>),
    Rename(Vec<u8>, Vec<u8>, bool),
    /// Path, offset, length, seed of the written bytes.
    Write(Vec<u8>, u16, u16, u8),
    Truncate(Vec<u8>, u16),
    /// Mode with bits outside `MODE_MASK` on purpose. Ignored for symlinks.
    SetMode(Vec<u8>, u16),
    Read(Vec<u8>, u16, u16),
    /// Sets or removes an attribute named by the index. Symlink targets and directories too.
    Xattr(Vec<u8>, u8, Option<u8>),
    /// A fifo, socket or device node: path, mode, kind index (taken modulo 4) and device byte.
    Mknod(Vec<u8>, u16, u8, u8),
}

#[derive(Debug, PartialEq, Eq)]
enum Obs {
    Unit,
    Bytes(Vec<u8>),
}

struct Leaf {
    data: RefCell<Vec<u8>>,
    mode: Cell<u32>,
    target: Option<Vec<u8>>,
    /// A fifo, socket or device: its kind and device number. No data, `read` and `write` fail.
    special: Option<(FileKind, u64)>,
    xattrs: RefCell<BTreeMap<u8, u8>>,
}

struct Dir {
    mode: Cell<u32>,
    kids: BTreeMap<u8, Node>,
    xattrs: RefCell<BTreeMap<u8, u8>>,
}

enum Node {
    Leaf(Rc<Leaf>),
    Dir(Dir),
}

type Errs = Vec<Error>;

fn new_dir(mode: u32) -> Dir {
    Dir {
        mode: Cell::new(mode),
        kids: BTreeMap::new(),
        xattrs: RefCell::default(),
    }
}

fn new_leaf(mode: u32, target: Option<Vec<u8>>) -> Rc<Leaf> {
    Rc::new(Leaf {
        data: RefCell::default(),
        mode: Cell::new(mode),
        target,
        special: None,
        xattrs: RefCell::default(),
    })
}

fn special_kind(idx: u8) -> FileKind {
    [
        FileKind::Fifo,
        FileKind::Socket,
        FileKind::CharDevice,
        FileKind::BlockDevice,
    ][usize::from(idx % 4)]
}

/// The device number a `Mknod` op gives its node: zero unless the kind is a device.
fn special_rdev(kind: FileKind, dev: u8) -> u64 {
    if kind.is_device() {
        cowfs_vfs::makedev(u32::from(dev), u32::from(dev) * 3 + 1)
    } else {
        0
    }
}

fn split(p: &[u8]) -> (&[u8], u8) {
    match p.split_last() {
        Some((last, head)) => (head, *last),
        None => (&[], 0),
    }
}

fn name(i: u8) -> [u8; 1] {
    [b'a' + i]
}

fn xname(i: u8) -> Vec<u8> {
    format!("user.x{i}").into_bytes()
}

struct Oracle {
    root: Node,
}

impl Oracle {
    fn new() -> Self {
        Self {
            root: Node::Dir(new_dir(0o755)),
        }
    }

    fn get(&self, comps: &[u8]) -> Result<&Node, Errs> {
        let mut cur = &self.root;
        for c in comps {
            match cur {
                Node::Dir(d) => cur = d.kids.get(c).ok_or_else(|| vec![Error::NotFound])?,
                Node::Leaf(_) => return Err(vec![Error::NotDir]),
            }
        }
        Ok(cur)
    }

    fn dir_mut(&mut self, comps: &[u8]) -> Result<&mut Dir, Errs> {
        let mut cur = &mut self.root;
        for c in comps {
            match cur {
                Node::Dir(d) => cur = d.kids.get_mut(c).ok_or_else(|| vec![Error::NotFound])?,
                Node::Leaf(_) => return Err(vec![Error::NotDir]),
            }
        }
        match cur {
            Node::Dir(d) => Ok(d),
            Node::Leaf(_) => Err(vec![Error::NotDir]),
        }
    }

    fn insert(&mut self, p: &[u8], node: Node) -> Result<Obs, Errs> {
        let (parent, n) = split(p);
        let d = self.dir_mut(parent)?;
        if d.kids.contains_key(&n) {
            return Err(vec![Error::Exists]);
        }
        d.kids.insert(n, node);
        Ok(Obs::Unit)
    }

    fn apply(&mut self, op: &Op) -> Result<Obs, Errs> {
        match op {
            Op::Create(p, m) => {
                self.insert(p, Node::Leaf(new_leaf(u32::from(*m) & MODE_MASK, None)))
            }
            Op::Mkdir(p, m) => self.insert(p, Node::Dir(new_dir(u32::from(*m) & MODE_MASK))),
            Op::Symlink(p, t) => {
                self.insert(p, Node::Leaf(new_leaf(0o777, Some(name(*t).to_vec()))))
            }
            Op::Mknod(p, m, k, dev) => {
                let kind = special_kind(*k);
                let leaf = Leaf {
                    data: RefCell::default(),
                    mode: Cell::new(u32::from(*m) & MODE_MASK),
                    target: None,
                    special: Some((kind, special_rdev(kind, *dev))),
                    xattrs: RefCell::default(),
                };
                self.insert(p, Node::Leaf(Rc::new(leaf)))
            }
            Op::Link(s, d) => {
                let leaf = match self.get(s)? {
                    Node::Leaf(l) => Some(Rc::clone(l)),
                    Node::Dir(_) => None,
                };
                let (dp, n) = split(d);
                let parent_is_file = matches!(self.get(dp), Ok(Node::Leaf(_)));
                if parent_is_file && leaf.is_none() {
                    return Err(vec![Error::NotDir, Error::PermissionDenied]);
                }
                let dir = self.dir_mut(dp)?;
                let exists = dir.kids.contains_key(&n);
                match leaf {
                    Some(l) if !exists => {
                        dir.kids.insert(n, Node::Leaf(l));
                        Ok(Obs::Unit)
                    }
                    other => {
                        let mut errs = Vec::new();
                        if other.is_none() {
                            errs.push(Error::PermissionDenied);
                        }
                        if exists {
                            errs.push(Error::Exists);
                        }
                        Err(errs)
                    }
                }
            }
            Op::Unlink(p) => {
                let (parent, n) = split(p);
                let d = self.dir_mut(parent)?;
                match d.kids.get(&n) {
                    None => Err(vec![Error::NotFound]),
                    Some(Node::Dir(_)) => Err(vec![Error::IsDir]),
                    Some(Node::Leaf(_)) => {
                        d.kids.remove(&n);
                        Ok(Obs::Unit)
                    }
                }
            }
            Op::Rmdir(p) => {
                let (parent, n) = split(p);
                let d = self.dir_mut(parent)?;
                match d.kids.get(&n) {
                    None => Err(vec![Error::NotFound]),
                    Some(Node::Leaf(_)) => Err(vec![Error::NotDir]),
                    Some(Node::Dir(x)) if !x.kids.is_empty() => Err(vec![Error::NotEmpty]),
                    Some(Node::Dir(_)) => {
                        d.kids.remove(&n);
                        Ok(Obs::Unit)
                    }
                }
            }
            Op::Rename(a, b, no_replace) => self.rename(a, b, *no_replace),
            Op::Write(p, off, len, seed) => match self.get(p)? {
                Node::Dir(_) => Err(vec![Error::IsDir]),
                Node::Leaf(l) if l.target.is_some() || l.special.is_some() => {
                    Err(vec![Error::InvalidArgument])
                }
                Node::Leaf(l) => {
                    let (off, len) = (usize::from(*off), usize::from(*len));
                    if len > 0 {
                        let mut data = l.data.borrow_mut();
                        if data.len() < off + len {
                            data.resize(off + len, 0);
                        }
                        data[off..off + len].copy_from_slice(&pattern(len, u64::from(*seed)));
                    }
                    Ok(Obs::Unit)
                }
            },
            Op::Truncate(p, size) => match self.get(p)? {
                Node::Dir(_) => Err(vec![Error::IsDir]),
                Node::Leaf(l) if l.target.is_some() || l.special.is_some() => {
                    Err(vec![Error::InvalidArgument])
                }
                Node::Leaf(l) => {
                    l.data.borrow_mut().resize(usize::from(*size), 0);
                    Ok(Obs::Unit)
                }
            },
            Op::SetMode(p, m) => {
                let mode = u32::from(*m) & MODE_MASK;
                match self.get(p)? {
                    Node::Dir(d) => d.mode.set(mode),
                    Node::Leaf(l) if l.target.is_none() => l.mode.set(mode),
                    Node::Leaf(_) => {}
                }
                Ok(Obs::Unit)
            }
            Op::Read(p, off, len) => match self.get(p)? {
                Node::Dir(_) => Err(vec![Error::IsDir]),
                Node::Leaf(l) if l.target.is_some() || l.special.is_some() => {
                    Err(vec![Error::InvalidArgument])
                }
                Node::Leaf(l) => {
                    let data = l.data.borrow();
                    let start = usize::from(*off).min(data.len());
                    let end = (usize::from(*off) + usize::from(*len)).min(data.len());
                    Ok(Obs::Bytes(data[start..end].to_vec()))
                }
            },
            Op::Xattr(p, k, v) => {
                let map = match self.get(p)? {
                    Node::Dir(d) => &d.xattrs,
                    Node::Leaf(l) => &l.xattrs,
                };
                match v {
                    Some(v) => {
                        map.borrow_mut().insert(*k, *v);
                        Ok(Obs::Unit)
                    }
                    None if map.borrow_mut().remove(k).is_some() => Ok(Obs::Unit),
                    None => Err(vec![Error::NoAttr]),
                }
            }
        }
    }

    fn rename(&mut self, a: &[u8], b: &[u8], no_replace: bool) -> Result<Obs, Errs> {
        let ((ap, an), (bp, bn)) = (split(a), split(b));
        self.get(ap)?;
        self.get(bp)?;
        self.dir_mut(ap)?;
        self.dir_mut(bp)?;
        let src = self.get(a)?;
        let src_dir = matches!(src, Node::Dir(_));
        if let Ok(dest) = self.get(b) {
            if no_replace {
                return Err(vec![Error::Exists]);
            }
            let same = match (src, dest) {
                (Node::Leaf(x), Node::Leaf(y)) => Rc::ptr_eq(x, y),
                (Node::Dir(_), Node::Dir(_)) => a == b,
                _ => false,
            };
            if same {
                return Ok(Obs::Unit);
            }
        }
        let mut errs = Vec::new();
        if src_dir && bp.starts_with(a) {
            errs.push(Error::InvalidArgument);
        }
        match (src, self.get(b)) {
            (Node::Dir(_), Ok(Node::Leaf(_))) => errs.push(Error::NotDir),
            (Node::Leaf(_), Ok(Node::Dir(d))) => {
                errs.push(Error::IsDir);
                if !d.kids.is_empty() && a.starts_with(b) {
                    errs.push(Error::NotEmpty);
                }
            }
            (Node::Dir(_), Ok(Node::Dir(d))) if !d.kids.is_empty() => errs.push(Error::NotEmpty),
            _ => {}
        }
        if !errs.is_empty() {
            return Err(errs);
        }
        let node = self.dir_mut(ap)?.kids.remove(&an);
        let Some(node) = node else {
            return Err(vec![Error::NotFound]);
        };
        self.dir_mut(bp)?.kids.insert(bn, node);
        Ok(Obs::Unit)
    }
}

fn resolve(fs: &dyn Vfs, comps: &[u8]) -> Result<Ino, Error> {
    let mut cur = ROOT_INO;
    for c in comps {
        cur = fs.lookup(cur, &name(*c))?.ino;
    }
    Ok(cur)
}

fn unit<T>(r: Result<T, Error>) -> Result<Obs, Error> {
    r.map(|_| Obs::Unit)
}

fn exec(fs: &dyn Vfs, op: &Op) -> Result<Obs, Error> {
    match op {
        Op::Create(p, m) => {
            let (pp, n) = split(p);
            unit(fs.create(resolve(fs, pp)?, &name(n), u32::from(*m)))
        }
        Op::Mkdir(p, m) => {
            let (pp, n) = split(p);
            unit(fs.mkdir(resolve(fs, pp)?, &name(n), u32::from(*m)))
        }
        Op::Symlink(p, t) => {
            let (pp, n) = split(p);
            unit(fs.symlink(resolve(fs, pp)?, &name(n), &name(*t)))
        }
        Op::Mknod(p, m, k, dev) => {
            let (pp, n) = split(p);
            let kind = special_kind(*k);
            unit(fs.mknod(
                resolve(fs, pp)?,
                &name(n),
                kind,
                u32::from(*m),
                special_rdev(kind, *dev),
            ))
        }
        Op::Link(s, d) => {
            let src = resolve(fs, s)?;
            let (dp, n) = split(d);
            unit(fs.link(src, resolve(fs, dp)?, &name(n)))
        }
        Op::Unlink(p) => {
            let (pp, n) = split(p);
            unit(fs.unlink(resolve(fs, pp)?, &name(n)))
        }
        Op::Rmdir(p) => {
            let (pp, n) = split(p);
            unit(fs.rmdir(resolve(fs, pp)?, &name(n)))
        }
        Op::Rename(a, b, no_replace) => {
            let ((ap, an), (bp, bn)) = (split(a), split(b));
            let (ad, bd) = (resolve(fs, ap)?, resolve(fs, bp)?);
            unit(fs.rename(
                ad,
                &name(an),
                bd,
                &name(bn),
                RenameFlags {
                    no_replace: *no_replace,
                },
            ))
        }
        Op::Write(p, off, len, seed) => {
            let ino = resolve(fs, p)?;
            let data = pattern(usize::from(*len), u64::from(*seed));
            let n = fs.write(ino, u64::from(*off), &data)?;
            if n as usize == data.len() {
                Ok(Obs::Unit)
            } else {
                Err(Error::Io(format!("short write: {n} of {}", data.len())))
            }
        }
        Op::Truncate(p, size) => {
            let ino = resolve(fs, p)?;
            unit(fs.setattr(
                ino,
                SetAttr {
                    size: Some(u64::from(*size)),
                    ..Default::default()
                },
            ))
        }
        Op::SetMode(p, m) => {
            let ino = resolve(fs, p)?;
            if fs.getattr(ino)?.kind == FileKind::Symlink {
                return Ok(Obs::Unit);
            }
            unit(fs.setattr(
                ino,
                SetAttr {
                    mode: Some(u32::from(*m)),
                    ..Default::default()
                },
            ))
        }
        Op::Read(p, off, len) => {
            let ino = resolve(fs, p)?;
            fs.read(ino, u64::from(*off), u32::from(*len))
                .map(Obs::Bytes)
        }
        Op::Xattr(p, k, v) => {
            let ino = resolve(fs, p)?;
            match v {
                Some(v) => unit(fs.setxattr(ino, &xname(*k), &[*v], XattrFlags::default())),
                None => unit(fs.removexattr(ino, &xname(*k))),
            }
        }
    }
}

type Ids = (HashMap<*const Leaf, Ino>, HashMap<Ino, *const Leaf>);

fn list_all(fs: &dyn Vfs, dir: Ino) -> Result<Vec<(Vec<u8>, Ino, FileKind)>, String> {
    let (mut out, mut cookie) = (Vec::new(), 0);
    for _ in 0..1000 {
        let r = fs
            .readdir(dir, cookie, 2)
            .map_err(|e| format!("readdir: {e:?}"))?;
        out.extend(r.entries.iter().map(|e| (e.name.clone(), e.ino, e.kind)));
        match r.entries.last() {
            Some(l) if !r.eof => cookie = l.cookie,
            _ => return Ok(out),
        }
    }
    Err("readdir does not terminate".into())
}

fn xattrs_match(fs: &dyn Vfs, ino: Ino, want: &BTreeMap<u8, u8>, at: &str) -> Result<(), String> {
    let mut names = fs
        .listxattr(ino)
        .map_err(|e| format!("{at}: listxattr {e:?}"))?;
    names.sort();
    let mut expect: Vec<Vec<u8>> = want.keys().map(|k| xname(*k)).collect();
    expect.sort();
    if names != expect {
        return Err(format!("{at}: xattr names {names:?}, want {expect:?}"));
    }
    for (k, v) in want {
        let got = fs
            .getxattr(ino, &xname(*k))
            .map_err(|e| format!("{at}: getxattr {e:?}"))?;
        if got != [*v] {
            return Err(format!("{at}: xattr {k} is {got:?}, want {v}"));
        }
    }
    Ok(())
}

fn compare_dir(fs: &dyn Vfs, d: &Dir, ino: Ino, at: &str, ids: &mut Ids) -> Result<(), String> {
    let a = fs
        .getattr(ino)
        .map_err(|e| format!("{at}: getattr {e:?}"))?;
    let subdirs = d
        .kids
        .values()
        .filter(|n| matches!(n, Node::Dir(_)))
        .count();
    if a.kind != FileKind::Directory || a.nlink as usize != 2 + subdirs {
        return Err(format!(
            "{at}: kind {:?} nlink {}, want directory nlink {}",
            a.kind,
            a.nlink,
            2 + subdirs
        ));
    }
    if ino != ROOT_INO && a.mode != d.mode.get() {
        return Err(format!("{at}: mode {:o}, want {:o}", a.mode, d.mode.get()));
    }
    xattrs_match(fs, ino, &d.xattrs.borrow(), at)?;
    let listed = list_all(fs, ino).map_err(|e| format!("{at}: {e}"))?;
    let mut got: Vec<Vec<u8>> = listed.iter().map(|e| e.0.clone()).collect();
    got.sort();
    let want: Vec<Vec<u8>> = d.kids.keys().map(|k| name(*k).to_vec()).collect();
    if got != want {
        return Err(format!("{at}: listing {got:?}, want {want:?}"));
    }
    for (n, child_ino, kind) in listed {
        let key = n[0] - b'a';
        let here = format!("{at}/{}", n[0] as char);
        let Some(node) = d.kids.get(&key) else {
            return Err(format!("{here}: not in the oracle"));
        };
        let looked = fs
            .lookup(ino, &n)
            .map_err(|e| format!("{here}: lookup {e:?}"))?;
        if looked.ino != child_ino || looked.kind != kind {
            return Err(format!("{here}: lookup disagrees with the listing"));
        }
        match node {
            Node::Dir(child) => compare_dir(fs, child, child_ino, &here, ids)?,
            Node::Leaf(l) => compare_leaf(fs, l, child_ino, &here, ids)?,
        }
    }
    Ok(())
}

fn compare_leaf(
    fs: &dyn Vfs,
    l: &Rc<Leaf>,
    ino: Ino,
    at: &str,
    ids: &mut Ids,
) -> Result<(), String> {
    let a = fs
        .getattr(ino)
        .map_err(|e| format!("{at}: getattr {e:?}"))?;
    let ptr = Rc::as_ptr(l);
    if *ids.0.entry(ptr).or_insert(ino) != ino || *ids.1.entry(ino).or_insert(ptr) != ptr {
        return Err(format!(
            "{at}: hardlink identity differs from the oracle (inode {ino})"
        ));
    }
    if a.nlink as usize != Rc::strong_count(l) {
        return Err(format!(
            "{at}: nlink {}, want {}",
            a.nlink,
            Rc::strong_count(l)
        ));
    }
    xattrs_match(fs, ino, &l.xattrs.borrow(), at)?;
    match &l.target {
        Some(t) => {
            let got = fs
                .readlink(ino)
                .map_err(|e| format!("{at}: readlink {e:?}"))?;
            if a.kind != FileKind::Symlink || &got != t || a.size != t.len() as u64 {
                return Err(format!(
                    "{at}: symlink {:?} {got:?} size {}, want {t:?}",
                    a.kind, a.size
                ));
            }
        }
        None if l.special.is_some() => {
            let (kind, rdev) = l.special.unwrap_or((FileKind::Fifo, 0));
            if (a.kind, a.rdev, a.size, a.mode) != (kind, rdev, 0, l.mode.get()) {
                return Err(format!(
                    "{at}: {:?} rdev {:x} size {} mode {:o}, want {kind:?} rdev {rdev:x} size 0 mode {:o}",
                    a.kind,
                    a.rdev,
                    a.size,
                    a.mode,
                    l.mode.get()
                ));
            }
        }
        None => {
            let data = l.data.borrow();
            if a.kind != FileKind::Regular || a.size != data.len() as u64 || a.mode != l.mode.get()
            {
                return Err(format!(
                    "{at}: kind {:?} size {} mode {:o}, want file size {} mode {:o}",
                    a.kind,
                    a.size,
                    a.mode,
                    data.len(),
                    l.mode.get()
                ));
            }
            let got = fs
                .read(ino, 0, data.len() as u32 + 10)
                .map_err(|e| format!("{at}: read {e:?}"))?;
            if got != *data {
                return Err(format!(
                    "{at}: content differs ({} bytes, want {})",
                    got.len(),
                    data.len()
                ));
            }
        }
    }
    Ok(())
}

/// Runs `ops` against `fs` (which must be empty) and the oracle, comparing every result and,
/// after every operation, the whole tree. Returns a description of the first difference.
pub fn run(fs: &dyn Vfs, ops: &[Op]) -> Result<(), String> {
    let mut oracle = Oracle::new();
    for (i, op) in ops.iter().enumerate() {
        let want = oracle.apply(op);
        let got = exec(fs, op);
        let agree = match (&want, &got) {
            (Ok(a), Ok(b)) => a == b,
            (Err(errs), Err(e)) => crate::conformance::errno_matches(e, errs),
            _ => false,
        };
        if !agree {
            return Err(format!(
                "op {i} {op:?}: oracle says {want:?}, backend says {got:?}"
            ));
        }
        let Node::Dir(root) = &oracle.root else {
            return Err("oracle root is not a directory".into());
        };
        let mut ids = Ids::default();
        compare_dir(fs, root, ROOT_INO, "", &mut ids)
            .map_err(|e| format!("after op {i} {op:?}: {e}"))?;
    }
    Ok(())
}

/// Deterministic random operation sequences for `run`, for callers without proptest.
/// The same `(seed, len)` always gives the same sequence.
pub fn random_ops(seed: u64, len: usize) -> Vec<Op> {
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, m: u64) -> u64 {
            self.next() % m
        }
    }
    fn path(r: &mut Rng) -> Vec<u8> {
        let n = match r.below(7) {
            0..=2 => 1,
            3..=5 => 2,
            _ => 3,
        };
        (0..n).map(|_| r.below(3) as u8).collect()
    }
    fn mode(r: &mut Rng) -> u16 {
        match r.below(3) {
            0 => 0o644,
            1 => 0o444,
            _ => r.next() as u16,
        }
    }
    let mut r = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let table = [5u64, 6, 2, 3, 3, 2, 5, 4, 2, 1, 2, 2, 2];
    (0..len)
        .map(|_| {
            let mut x = r.below(table.iter().sum());
            let mut k = table.len() - 1;
            for (i, w) in table.iter().enumerate() {
                if x < *w {
                    k = i;
                    break;
                }
                x -= w;
            }
            match k {
                0 => Op::Create(path(&mut r), mode(&mut r)),
                1 => Op::Mkdir(path(&mut r), mode(&mut r)),
                2 => Op::Symlink(path(&mut r), r.below(3) as u8),
                3 => Op::Link(path(&mut r), path(&mut r)),
                4 => Op::Unlink(path(&mut r)),
                5 => Op::Rmdir(path(&mut r)),
                6 => Op::Rename(path(&mut r), path(&mut r), r.below(2) == 0),
                7 => Op::Write(
                    path(&mut r),
                    r.below(10_000) as u16,
                    r.below(6000) as u16,
                    r.next() as u8,
                ),
                8 => Op::Truncate(path(&mut r), r.below(12_000) as u16),
                9 => Op::SetMode(path(&mut r), r.next() as u16),
                10 => Op::Read(path(&mut r), r.below(12_000) as u16, r.below(8000) as u16),
                11 => Op::Mknod(
                    path(&mut r),
                    mode(&mut r),
                    r.below(4) as u8,
                    r.below(5) as u8,
                ),
                _ => Op::Xattr(
                    path(&mut r),
                    r.below(3) as u8,
                    if r.below(2) == 0 {
                        None
                    } else {
                        Some(r.next() as u8)
                    },
                ),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use proptest::test_runner::{Config, RngAlgorithm, TestError, TestRng, TestRunner};

    use super::*;
    use crate::{Fault, MemVfs};

    fn path() -> impl Strategy<Value = Vec<u8>> {
        prop_oneof![
            3 => proptest::collection::vec(0u8..3, 1..=1),
            3 => proptest::collection::vec(0u8..3, 2..=2),
            1 => proptest::collection::vec(0u8..3, 3..=3),
        ]
    }

    fn mode() -> impl Strategy<Value = u16> {
        prop_oneof![Just(0o644), Just(0o444), any::<u16>()]
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            5 => (path(), mode()).prop_map(|(p, m)| Op::Create(p, m)),
            6 => (path(), mode()).prop_map(|(p, m)| Op::Mkdir(p, m)),
            2 => (path(), 0u8..3).prop_map(|(p, t)| Op::Symlink(p, t)),
            3 => (path(), path()).prop_map(|(a, b)| Op::Link(a, b)),
            3 => path().prop_map(Op::Unlink),
            2 => path().prop_map(Op::Rmdir),
            5 => (path(), path(), any::<bool>()).prop_map(|(a, b, n)| Op::Rename(a, b, n)),
            4 => (path(), 0u16..10_000, 0u16..6000, any::<u8>()).prop_map(|(p, o, l, s)| Op::Write(p, o, l, s)),
            2 => (path(), 0u16..12_000).prop_map(|(p, s)| Op::Truncate(p, s)),
            1 => (path(), any::<u16>()).prop_map(|(p, m)| Op::SetMode(p, m)),
            2 => (path(), 0u16..12_000, 0u16..8000).prop_map(|(p, o, l)| Op::Read(p, o, l)),
            2 => (path(), 0u8..3, proptest::option::of(any::<u8>())).prop_map(|(p, k, v)| Op::Xattr(p, k, v)),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(300))]

        #[test]
        fn memvfs_matches_oracle(ops in proptest::collection::vec(op(), 1..80)) {
            let r = run(&MemVfs::new(), &ops);
            prop_assert!(r.is_ok(), "{}", r.err().unwrap_or_default());
        }
    }

    /// Fill a file, shrink it to a size that is not page aligned, grow it again, then read the region
    /// the shrink vacated. That region must read back as zeros.
    ///
    /// The random sweep alone did not find this: CI run 37266648495 failed at `4c2d7ec` with
    /// `TruncateNoZeroFill was not detected`, and 5 of 5 runs of the unmodified test passed here, so
    /// catching it depended on the generator happening to emit a shrink, a grow and a read of the
    /// gap. Written out, the oracle's answer is fixed, and every byte `pattern` writes is `| 1`, so
    /// the stale tail is never zero by luck.
    fn truncate_tail_witness() -> Vec<Op> {
        vec![
            Op::Create(vec![0], 0o644),
            Op::Write(vec![0], 0, 8_000, 7),
            Op::Truncate(vec![0], 5_000),
            Op::Truncate(vec![0], 6_000),
            Op::Read(vec![0], 5_000, 1_000),
        ]
    }

    /// One fixed op sequence per fault, each the minimal way to show it. The oracle's answer for
    /// each is fixed, so catching a fault does not depend on a random search. A new fault that the
    /// search can miss gets its witness here.
    fn witnesses() -> Vec<(Fault, Vec<Op>)> {
        let two_files = || vec![Op::Create(vec![0], 0o644), Op::Create(vec![1], 0o644)];
        let with = |mut v: Vec<Op>, more: Vec<Op>| {
            v.extend(more);
            v
        };
        vec![
            (Fault::TruncateNoZeroFill, truncate_tail_witness()),
            (
                Fault::RenameNoReplace,
                with(two_files(), vec![Op::Rename(vec![0], vec![1], false)]),
            ),
            (
                Fault::LinkReplaces,
                with(two_files(), vec![Op::Link(vec![0], vec![1])]),
            ),
            (
                Fault::NoHardlinkNlink,
                vec![Op::Create(vec![0], 0o644), Op::Link(vec![0], vec![1])],
            ),
            // Bits above `MODE_MASK` must be dropped at create.
            (Fault::ModeNotMasked, vec![Op::Create(vec![0], 0xFFFF)]),
            // Reads past EOF return only the bytes that exist. The tree check after `Create` already
            // catches the padding (10 bytes, want 0); the later ops keep the shape a real read has.
            (
                Fault::ReadPadsEof,
                vec![
                    Op::Create(vec![0], 0o644),
                    Op::Write(vec![0], 0, 100, 7),
                    Op::Read(vec![0], 50, 1_000),
                ],
            ),
        ]
    }

    #[test]
    fn oracle_catches_broken_backends() {
        // Deterministic and gating: every fault has a fixed witness, and the witness must pass on
        // a healthy backend, or catching the fault would prove nothing. This goes through the same
        // `run` and the same oracle as the sweeps, with neither changed.
        for (fault, witness) in witnesses() {
            run(&MemVfs::new(), &witness)
                .unwrap_or_else(|e| panic!("{fault:?} witness fails on a healthy backend: {e}"));
            assert!(
                run(&MemVfs::with_fault(fault), &witness).is_err(),
                "{fault:?} survives its own witness"
            );
        }
    }

    /// The random search over the same faults, with the seed pinned so the result is the same on
    /// every run and platform. It was unseeded and flaked once on CI; a seed that stops detecting a
    /// fault after a generator change fails here every time, and gets a new seed or witness.
    #[test]
    fn random_search_finds_each_fault_with_a_small_counterexample() {
        for (fault, _) in witnesses() {
            let mut runner = TestRunner::new_with_rng(
                Config {
                    cases: 2000,
                    failure_persistence: None,
                    ..Config::default()
                },
                TestRng::from_seed(RngAlgorithm::ChaCha, &[0x5a; 32]),
            );
            let strategy = proptest::collection::vec(op(), 1..80);
            let res = runner.run(&strategy, |ops| {
                run(&MemVfs::with_fault(fault), &ops).map_err(TestCaseError::fail)
            });
            match res {
                Err(TestError::Fail(reason, ops)) => {
                    assert!(
                        ops.len() <= 12,
                        "{fault:?}: shrunk to {} ops ({reason})",
                        ops.len()
                    );
                }
                other => panic!("{fault:?} was not detected: {other:?}"),
            }
        }
    }

    #[test]
    fn oracle_accepts_the_linux_answer_for_a_file_onto_its_ancestor() {
        let mut o = Oracle::new();
        o.apply(&Op::Mkdir(vec![0], 0o755)).unwrap();
        o.apply(&Op::Create(vec![0, 0], 0o644)).unwrap();
        let errs = o
            .apply(&Op::Rename(vec![0, 0], vec![0], false))
            .unwrap_err();
        assert!(errs.contains(&Error::IsDir) && errs.contains(&Error::NotEmpty));
    }
}
