use std::collections::HashMap;
use std::fs::File;
use std::io;
use std::os::fd::{AsFd, OwnedFd};
use std::sync::Arc;

use cowfs_vfs::{Attr, DirEntry, Error, FileKind, Ino, Result, Timestamp, MODE_MASK, ROOT_INO};

use crate::cookies::Cookies;
use crate::sys::{self, Stat};

/// Unpinned descriptors kept for speed. The table is cleared when it fills up.
const CACHE_CAP: usize = 256;

/// Maps a failed system call to the `Vfs` error for its errno.
pub(crate) fn io_err(e: io::Error) -> Error {
    let Some(n) = e.raw_os_error() else {
        return Error::Io(e.to_string());
    };
    if n == Error::NoAttr.errno() {
        return Error::NoAttr;
    }
    match n {
        libc::ENOENT => Error::NotFound,
        libc::EEXIST => Error::Exists,
        libc::ENOTDIR => Error::NotDir,
        libc::EISDIR => Error::IsDir,
        libc::ENOTEMPTY => Error::NotEmpty,
        libc::EINVAL => Error::InvalidArgument,
        libc::ENAMETOOLONG => Error::NameTooLong,
        libc::ENOSPC | libc::EDQUOT => Error::NoSpace,
        libc::EACCES | libc::EPERM => Error::PermissionDenied,
        libc::EMLINK => Error::TooManyLinks,
        n if n == libc::ENOTSUP || n == libc::EOPNOTSUPP => Error::NotSupported,
        libc::ESTALE => Error::Stale,
        libc::ERANGE | libc::E2BIG => Error::Range,
        libc::EROFS => Error::ReadOnly,
        libc::EXDEV => Error::CrossDevice,
        libc::ENOSYS => Error::NotSupported,
        // A non-exhaustive enum: an error the crate does not know must not pass silently.
        other => Error::Io(format!("errno {other}")),
    }
}

pub(crate) fn kind_of(st: &Stat) -> Result<FileKind> {
    match st.file_type() {
        t if t == sys::S_IFREG => Ok(FileKind::Regular),
        t if t == sys::S_IFDIR => Ok(FileKind::Directory),
        t if t == sys::S_IFLNK => Ok(FileKind::Symlink),
        _ => Err(Error::NotSupported),
    }
}

fn ts((secs, nanos): (i64, u32)) -> Timestamp {
    Timestamp { secs, nanos }
}

/// An open descriptor and whether it can write.
#[derive(Clone, Debug)]
pub(crate) struct Open {
    pub file: Arc<File>,
    pub rw: bool,
}

/// Where a node can be reached by name right now.
pub(crate) enum Loc {
    Named(Arc<File>, Vec<u8>),
    Fd(Open),
}

/// One live inode: its identity on the backing filesystem, the names we know it by, and the
/// references and handles that keep it alive.
#[derive(Debug)]
pub(crate) struct Node {
    /// The file's identity on the backing filesystem. A number belongs to one file only while
    /// that file is alive, and the filesystem hands a freed number to another file, so this is
    /// worth trusting only while one of the node's names still resolves to it. `register` proves
    /// that before it reuses an entry, and unbinds the node when it cannot.
    pub id: (u64, u64),
    pub kind: FileKind,
    /// `(parent, name)` pairs known to lead here. Enough to reopen the file, never a full list.
    pub names: Vec<(Ino, Vec<u8>)>,
    pub refs: u64,
    pub handles: u64,
    /// Held while a handle is open, and for a node with no known name (it would be lost).
    pinned: Option<Open>,
    /// Every name is gone: the node lives only through its references and handles.
    unlinked: bool,
    /// A symlink's target, read once: it never changes.
    pub target: Option<Vec<u8>>,
    pub cookies: Cookies,
    /// The directory's names by cookie as of the last read, dropped by every change to it.
    pub listing: Option<Vec<(u64, Vec<u8>)>>,
    /// The directory's (mtime, ctime, size, links) when the listing was read: a change made by
    /// anyone else, including an adapter under a mountpoint, moves at least one of them.
    pub listing_stamp: Option<(i64, u32, i64, u32, u64, u64)>,
}

/// The inode table. Every method runs under the single lock in `PathVfs`.
#[derive(Debug)]
pub(crate) struct State {
    nodes: HashMap<Ino, Node>,
    /// Backing identities to inode numbers. Entries are dropped with the node they name.
    by_id: HashMap<(u64, u64), Ino>,
    cache: HashMap<Ino, Open>,
    open_handles: HashMap<u64, Ino>,
    next_ino: Ino,
    next_handle: u64,
}

impl State {
    pub(crate) fn new(root: OwnedFd) -> io::Result<Self> {
        let st = sys::fstat(root.as_fd())?;
        if st.file_type() != sys::S_IFDIR {
            return Err(io::Error::from_raw_os_error(libc::ENOTDIR));
        }
        let node = Node {
            id: (st.dev, st.ino),
            kind: FileKind::Directory,
            names: Vec::new(),
            refs: 1,
            handles: 0,
            pinned: Some(Open {
                file: Arc::new(File::from(root)),
                rw: false,
            }),
            unlinked: false,
            target: None,
            cookies: Cookies::default(),
            listing: None,
            listing_stamp: None,
        };
        let mut nodes = HashMap::new();
        nodes.insert(ROOT_INO, node);
        let mut by_id = HashMap::new();
        by_id.insert((st.dev, st.ino), ROOT_INO);
        Ok(Self {
            nodes,
            by_id,
            cache: HashMap::new(),
            open_handles: HashMap::new(),
            next_ino: ROOT_INO + 1,
            next_handle: 1,
        })
    }

    pub(crate) fn node(&self, ino: Ino) -> Result<&Node> {
        self.nodes.get(&ino).ok_or(Error::Stale)
    }

    pub(crate) fn node_mut(&mut self, ino: Ino) -> Result<&mut Node> {
        self.nodes.get_mut(&ino).ok_or(Error::Stale)
    }

    /// Drops the cached listing of `dir`: a name is about to be added or removed in it.
    pub(crate) fn invalidate(&mut self, dir: Ino) {
        if let Some(n) = self.nodes.get_mut(&dir) {
            n.listing = None;
            n.listing_stamp = None;
        }
    }

    /// One page of a directory listing, with attributes.
    ///
    /// The directory is read again when the call starts from the beginning or when the directory
    /// itself changed since the last read, which is the only signal that a name appeared or
    /// vanished without `PathVfs` doing it. Between the pages of one listing the cached names are
    /// used as they are, so paging a large directory stays linear.
    pub(crate) fn readdir_page(
        &mut self,
        dir: Ino,
        cookie: u64,
        max: usize,
    ) -> Result<(Vec<(DirEntry, Attr)>, bool)> {
        let open = self.dir_fd(dir)?;
        let dstamp = sys::fstat(open.file.as_fd()).map_err(io_err)?;
        let stamp = (
            dstamp.mtime.0,
            dstamp.mtime.1,
            dstamp.ctime.0,
            dstamp.ctime.1,
            dstamp.size,
            dstamp.nlink,
        );
        let restart = cookie == 0
            || self
                .nodes
                .get(&dir)
                .is_none_or(|n| n.listing.is_none() || n.listing_stamp != Some(stamp));
        let listing = if restart {
            let names = sys::list_dir(open.file.as_fd()).map_err(io_err)?;
            self.node_mut(dir)?.cookies.sync(names)
        } else {
            self.node_mut(dir)?.listing.take().ok_or(Error::Stale)?
        };
        let start = listing.partition_point(|(c, _)| *c <= cookie);
        let mut entries = Vec::new();
        let mut consumed = 0;
        for (c, name) in &listing[start..] {
            if entries.len() >= max {
                break;
            }
            consumed += 1;
            let st = match sys::fstatat(open.file.as_fd(), name) {
                Ok(st) => st,
                Err(e) if e.raw_os_error() == Some(libc::ENOENT) => continue,
                Err(e) => return Err(io_err(e)),
            };
            let Ok(kind) = kind_of(&st) else {
                continue;
            };
            let ino = self.register(dir, &open.file, name, &st)?;
            let attr = self.attr_of(ino, &st)?;
            entries.push((
                DirEntry {
                    ino,
                    kind,
                    name: name.clone(),
                    cookie: *c,
                },
                attr,
            ));
        }
        let eof = consumed == listing.len() - start;
        let n = self.node_mut(dir)?;
        n.listing = Some(listing);
        n.listing_stamp = Some(stamp);
        Ok((entries, eof))
    }

    /// The inode for a backing identity, when the node behind it is still live.
    pub(crate) fn node_by_id(&self, id: (u64, u64)) -> Option<Ino> {
        let ino = *self.by_id.get(&id)?;
        match self.nodes.get(&ino) {
            Some(n) if n.id == id => Some(ino),
            _ => None,
        }
    }

    fn cached(&self, ino: Ino) -> Option<Open> {
        let n = self.nodes.get(&ino)?;
        n.pinned.clone().or_else(|| self.cache.get(&ino).cloned())
    }

    fn cache_put(&mut self, ino: Ino, open: Open) {
        if self.cache.len() >= CACHE_CAP {
            self.cache.clear();
        }
        self.cache.insert(ino, open);
    }

    /// Adds a descriptor from a fresh `create` or `mkdir` to the cache.
    pub(crate) fn remember(&mut self, ino: Ino, open: Open) {
        self.cache_put(ino, open);
    }

    /// A usable descriptor for `ino`: the pinned or cached one, else reopened through a known name.
    pub(crate) fn open_fd(&mut self, ino: Ino) -> Result<Open> {
        self.node(ino)?;
        if let Some(o) = self.cached(ino) {
            return Ok(o);
        }
        let o = self.reopen(ino, false)?;
        self.cache_put(ino, o.clone());
        Ok(o)
    }

    /// A descriptor that can write, reopened read-write: a cached read-only one is not enough.
    pub(crate) fn open_rw(&mut self, ino: Ino) -> Result<Open> {
        self.node(ino)?;
        if let Some(o) = self.cached(ino) {
            if o.rw {
                return Ok(o);
            }
        }
        let o = self.reopen(ino, true)?;
        self.replace(ino, o.clone());
        Ok(o)
    }

    fn replace(&mut self, ino: Ino, open: Open) {
        match self.nodes.get_mut(&ino) {
            Some(n) if n.pinned.is_some() => n.pinned = Some(open),
            _ => self.cache_put(ino, open),
        }
    }

    fn reopen(&mut self, ino: Ino, write: bool) -> Result<Open> {
        let (id, kind, names) = {
            let n = self.node(ino)?;
            (n.id, n.kind, n.names.clone())
        };
        let mut last = Error::Stale;
        let mut parents: Vec<Ino> = Vec::new();
        for (parent, name) in names {
            let dir = match self.dir_fd(parent) {
                Ok(d) => d,
                Err(e) => {
                    last = e;
                    continue;
                }
            };
            if !parents.contains(&parent) {
                parents.push(parent);
            }
            match open_named(&dir.file, &name, kind, write) {
                Ok((o, st)) if (st.dev, st.ino) == id => return Ok(o),
                Ok(_) => last = Error::Stale,
                Err(e) => last = io_err(e),
            }
        }
        // Another writer may have renamed or replaced the file under a name the Vfs knows
        // (an adapter under a mountpoint does), so look for it by identity in those parents.
        for parent in parents {
            if let Some(o) = self.scan_for(parent, id, kind, write) {
                return Ok(o);
            }
        }
        Err(last)
    }

    /// Finds a file by backing identity inside one directory and opens it.
    fn scan_for(
        &mut self,
        parent: Ino,
        id: (u64, u64),
        kind: FileKind,
        write: bool,
    ) -> Option<Open> {
        let dir = self.dir_fd(parent).ok()?;
        let names = sys::list_dir(dir.file.as_fd()).ok()?;
        for name in names {
            let Ok(st) = sys::fstatat(dir.file.as_fd(), &name) else {
                continue;
            };
            if (st.dev, st.ino) != id {
                continue;
            }
            if let Ok((o, _)) = open_named(&dir.file, &name, kind, write) {
                return Some(o);
            }
        }
        None
    }

    pub(crate) fn dir_fd(&mut self, ino: Ino) -> Result<Open> {
        if self.node(ino)?.kind != FileKind::Directory {
            return Err(Error::NotDir);
        }
        self.open_fd(ino)
    }

    /// Reaches the node by a verified name, or else by a descriptor.
    pub(crate) fn locate(&mut self, ino: Ino) -> Result<Loc> {
        let names = self.node(ino)?.names.clone();
        let id = self.node(ino)?.id;
        for (parent, name) in names {
            let Ok(dir) = self.dir_fd(parent) else {
                continue;
            };
            if let Ok(st) = sys::fstatat(dir.file.as_fd(), &name) {
                if (st.dev, st.ino) == id {
                    return Ok(Loc::Named(dir.file, name));
                }
            }
        }
        self.cached(ino).map(Loc::Fd).ok_or(Error::Stale)
    }

    pub(crate) fn stat(&mut self, ino: Ino) -> Result<Stat> {
        if let Some(o) = self.cached(ino) {
            return sys::fstat(o.file.as_fd()).map_err(io_err);
        }
        match self.locate(ino)? {
            Loc::Named(dir, name) => sys::fstatat(dir.as_fd(), &name).map_err(io_err),
            Loc::Fd(o) => sys::fstat(o.file.as_fd()).map_err(io_err),
        }
    }

    pub(crate) fn attr(&mut self, ino: Ino) -> Result<Attr> {
        let st = self.stat(ino)?;
        self.attr_of(ino, &st)
    }

    pub(crate) fn attr_of(&self, ino: Ino, st: &Stat) -> Result<Attr> {
        Ok(Attr {
            ino,
            kind: kind_of(st)?,
            mode: st.mode & MODE_MASK,
            nlink: u32::try_from(st.nlink).unwrap_or(u32::MAX),
            uid: st.uid,
            gid: st.gid,
            size: st.size,
            blocks: st.blocks,
            atime: ts(st.atime),
            mtime: ts(st.mtime),
            ctime: ts(st.ctime),
        })
    }

    /// Whether the file behind `ino` is still the file `id` names: one of the names the Vfs
    /// knows must still resolve to it. A number the filesystem has handed to another file
    /// resolves to nothing, which is the only sign that the node's file is gone.
    fn still_here(&mut self, ino: Ino, id: (u64, u64)) -> bool {
        let names = self.node(ino).map(|n| n.names.clone()).unwrap_or_default();
        for (parent, name) in names {
            let Ok(dir) = self.dir_fd(parent) else {
                continue;
            };
            if let Ok(st) = sys::fstatat(dir.file.as_fd(), &name) {
                if (st.dev, st.ino) == id {
                    return true;
                }
            }
        }
        false
    }

    /// Drops the binding of a node whose file the filesystem has replaced: its number now names
    /// another file, so nothing may resolve through it, and the node keeps no way back.
    fn unbind(&mut self, ino: Ino) {
        let Some(n) = self.nodes.get_mut(&ino) else {
            return;
        };
        n.names.clear();
        n.unlinked = true;
        let id = n.id;
        self.by_id.remove(&id);
        self.cache.remove(&ino);
        self.reclaim(ino);
    }

    /// Finds or creates the node for the file at `parent`/`name` whose stat is `st`, and
    /// records that name for it. Does not take a reference.
    pub(crate) fn register(
        &mut self,
        parent: Ino,
        dir: &File,
        name: &[u8],
        st: &Stat,
    ) -> Result<Ino> {
        let id = (st.dev, st.ino);
        let kind = kind_of(st)?;
        if let Some(ino) = self.node_by_id(id) {
            let known = self
                .node(ino)
                .is_ok_and(|n| n.names.iter().any(|(p, nm)| *p == parent && nm == name));
            if known || self.still_here(ino, id) {
                if let Some(n) = self.nodes.get_mut(&ino) {
                    if !n.names.iter().any(|(p, nm)| *p == parent && nm == name) {
                        n.names.push((parent, name.to_vec()));
                    }
                    return Ok(ino);
                }
            }
            // The file this node named is gone and the filesystem has given its number to the
            // file at `name`, so the two are unrelated and get numbers of their own.
            self.unbind(ino);
        }
        let ino = self.alloc_ino();
        let target = if kind == FileKind::Symlink {
            Some(sys::readlinkat(dir.as_fd(), name).map_err(io_err)?)
        } else {
            None
        };
        self.nodes.insert(
            ino,
            Node {
                id,
                kind,
                names: vec![(parent, name.to_vec())],
                refs: 0,
                handles: 0,
                pinned: None,
                unlinked: false,
                target,
                cookies: Cookies::default(),
                listing: None,
                listing_stamp: None,
            },
        );
        self.by_id.insert(id, ino);
        Ok(ino)
    }

    /// A number no other file has ever been given.
    fn alloc_ino(&mut self) -> Ino {
        let ino = self.next_ino;
        self.next_ino = self
            .next_ino
            .checked_add(1)
            .unwrap_or_else(|| panic!("inode numbers exhausted: {} live nodes", self.nodes.len()));
        ino
    }

    /// Holds a descriptor for `ino` so it survives losing its last name. Best effort.
    pub(crate) fn pin(&mut self, ino: Ino) {
        if self.nodes.get(&ino).is_none_or(|n| n.pinned.is_some()) {
            return;
        }
        if let Ok(o) = self.open_rw(ino).or_else(|_| self.open_fd(ino)) {
            self.cache.remove(&ino);
            if let Some(n) = self.nodes.get_mut(&ino) {
                n.pinned = Some(o);
            }
        }
    }

    /// Opens `ino` the way the operation needs it: read-write for a regular file, as it is for
    /// a directory or a symlink, so a mode change never goes through a path.
    pub(crate) fn open_kind(&mut self, ino: Ino) -> Result<Open> {
        if self.node(ino)?.kind == FileKind::Regular {
            self.open_rw(ino)
        } else {
            self.open_fd(ino)
        }
    }

    /// Drops a pinned descriptor: the file can be reopened for writing, so it need not be held.
    pub(crate) fn unpin(&mut self, ino: Ino) {
        let Some(n) = self.nodes.get_mut(&ino) else {
            return;
        };
        if let Some(o) = n.pinned.take() {
            if o.rw {
                self.cache_put(ino, o);
            }
        }
    }

    /// Gives back the pin of a node that has a name and no handle.
    pub(crate) fn settle(&mut self, ino: Ino) {
        let Some(n) = self.nodes.get_mut(&ino) else {
            return;
        };
        if n.handles == 0 && !n.names.is_empty() {
            if let Some(o) = n.pinned.take() {
                self.cache_put(ino, o);
            }
        }
    }

    /// Notes that the name `parent`/`name` no longer leads to `ino`.
    ///
    /// The names PathVfs knows are the whole truth here: a hardlink made outside the Vfs, or by
    /// an adapter underneath a mount, keeps the file alive on the backing filesystem but is not
    /// a name the Vfs can reach, and the `Vfs` contract counts names the Vfs handed out.
    pub(crate) fn name_removed(&mut self, ino: Ino, parent: Ino, name: &[u8]) {
        let Some(n) = self.nodes.get_mut(&ino) else {
            return;
        };
        n.names.retain(|(p, nm)| !(*p == parent && nm == name));
        if n.names.is_empty() {
            n.unlinked = true;
            let id = n.id;
            self.by_id.remove(&id);
            self.reclaim(ino);
        }
    }

    pub(crate) fn rename_name(&mut self, ino: Ino, from: (Ino, &[u8]), to: (Ino, &[u8])) {
        let Some(n) = self.nodes.get_mut(&ino) else {
            return;
        };
        n.names.retain(|(p, nm)| !(*p == from.0 && nm == from.1));
        if !n.names.iter().any(|(p, nm)| *p == to.0 && nm == to.1) {
            n.names.push((to.0, to.1.to_vec()));
        }
    }

    fn reclaim(&mut self, ino: Ino) {
        let dead = self
            .nodes
            .get(&ino)
            .is_some_and(|n| n.unlinked && n.refs == 0 && n.handles == 0);
        if dead {
            self.nodes.remove(&ino);
            self.cache.remove(&ino);
        }
    }

    /// Live nodes and cached descriptors, for the leak regression tests.
    #[cfg(test)]
    pub(crate) fn counts(&self) -> (usize, usize) {
        (
            self.nodes.len(),
            self.cache.len() + self.nodes.values().filter(|n| n.pinned.is_some()).count(),
        )
    }

    pub(crate) fn add_ref(&mut self, ino: Ino) {
        if let Some(n) = self.nodes.get_mut(&ino) {
            n.refs += 1;
        }
    }

    pub(crate) fn forget(&mut self, ino: Ino, count: u64) {
        if ino == ROOT_INO {
            return;
        }
        if let Some(n) = self.nodes.get_mut(&ino) {
            n.refs = n.refs.saturating_sub(count);
        }
        self.reclaim(ino);
    }

    pub(crate) fn open_handle(&mut self, ino: Ino) -> Result<u64> {
        let o = self.open_fd(ino)?;
        self.cache.remove(&ino);
        let h = self.next_handle;
        self.next_handle += 1;
        let n = self.node_mut(ino)?;
        n.handles += 1;
        n.pinned = Some(o);
        self.open_handles.insert(h, ino);
        Ok(h)
    }

    pub(crate) fn release_handle(&mut self, handle: u64) -> Result<()> {
        let ino = self
            .open_handles
            .remove(&handle)
            .ok_or(Error::InvalidArgument)?;
        if let Some(n) = self.nodes.get_mut(&ino) {
            n.handles = n.handles.saturating_sub(1);
        }
        self.settle(ino);
        self.reclaim(ino);
        Ok(())
    }
}

/// Opens `name` inside `dir` for its kind, never following a final symlink.
fn open_named(dir: &File, name: &[u8], kind: FileKind, write: bool) -> io::Result<(Open, Stat)> {
    let d = dir.as_fd();
    let (fd, rw) = match kind {
        FileKind::Regular => match sys::openat(d, name, libc::O_RDWR | libc::O_NOFOLLOW, 0) {
            Ok(fd) => (fd, true),
            // A write that must succeed needs the read-write descriptor, so no fallback here.
            Err(e) if write => return Err(e),
            // A file whose mode has no write bit is still readable, and `Vfs` does no
            // permission enforcement, so reads go on.
            Err(e)
                if [libc::EACCES, libc::EPERM, libc::EROFS]
                    .contains(&e.raw_os_error().unwrap_or(0)) =>
            {
                (
                    sys::openat(d, name, libc::O_RDONLY | libc::O_NOFOLLOW, 0)?,
                    false,
                )
            }
            Err(e) => return Err(e),
        },
        FileKind::Directory => (
            sys::openat(
                d,
                name,
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW,
                0,
            )?,
            false,
        ),
        FileKind::Symlink => (sys::openat(d, name, sys::OPEN_SYMLINK, 0)?, false),
        // A non-exhaustive enum: refusing a kind we do not know is the only safe answer.
        _ => return Err(io::Error::from_raw_os_error(libc::ENOTSUP)),
    };
    let st = sys::fstat(fd.as_fd())?;
    Ok((
        Open {
            file: Arc::new(File::from(fd)),
            rw,
        },
        st,
    ))
}

/// Wraps an owned descriptor from `create`/`mkdir` as an `Open`.
pub(crate) fn open_from(fd: OwnedFd, rw: bool) -> Open {
    Open {
        file: Arc::new(File::from(fd)),
        rw,
    }
}
