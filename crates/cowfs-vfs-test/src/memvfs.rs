use std::collections::{BTreeMap, HashMap};
use std::ops::Bound;
use std::sync::{Mutex, MutexGuard};

use cowfs_vfs::{
    validate_name, Attr, DirEntry, Error, FileHandle, FileKind, Ino, ReadDir, RenameFlags, Result,
    SetAttr, SetTime, StatFs, Timestamp, Vfs, XattrFlags, MODE_MASK, ROOT_INO,
};

use crate::pages::{Pages, MAX_FILE, PAGE};

/// Default limit on the number of names of one file or symlink (like ext4's 65000).
pub const LINK_MAX: u32 = 65_000;

/// Deliberate defects, used only to prove the conformance suite fails when a backend is wrong.
/// Never enable one outside tests.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    InodeCookies,
    PositionCookies,
    EofOffByOne,
    DotEntries,
    NoHardlinkNlink,
    RenameNoReplace,
    LinkReplaces,
    NoCtimeUpdate,
    WriteNoMtime,
    UnlinkFreesOpen,
    StaleNeverReclaims,
    TruncateNoZeroFill,
    ModeNotMasked,
    ReadPadsEof,
    SymlinkSetattrFollows,
    LinkNoCtime,
    UnlinkLeaksSpace,
    TruncateKeepsPages,
    RenameNonEmptyDirOk,
    HoleGarbage,
    ReaddirMax1Skips,
    ForgetOffByOne,
    RmdirNoParentNlink,
    SymlinkSizeChars,
    CreateModeNotMasked,
    SetattrModeNotMasked,
    XattrReplaceIgnored,
    XattrCreateIgnored,
    ReadlinkAnyOk,
    UnlinkedNlinkOne,
    RenameNewIno,
    ReadPadsCrossing,
    ConcurrentCreateRace,
    WriteNoCtime,
    BlocksZero,
    DirNlinkCountsFiles,
    DirParentStale,
    RenameDestNoDrop,
    RenameDirOverDirNlink,
    ShortWrite,
    ShortRead32K,
    LookupNoRef,
    LinkResurrects,
    UnlinkGhostEntry,
    RenameDirZeroesChildren,
    WriteEnforcesMode,
    ModeDriftOnWrite,
    StaleNlinkLookup,
    HidesDotUnderscore,
    TruncateNoMtime,
    SetattrNoCtime,
    CreateNoParentTimes,
    BlocksDouble,
    ReadShort1,
    XattrRemoveNoop,
    ReaddirMaxZeroOk,
    XattrNameUnchecked,
}

impl Fault {
    /// Every fault, so tests can prove each one is expected to be caught.
    pub const ALL: &[Fault] = &[
        Fault::InodeCookies,
        Fault::PositionCookies,
        Fault::EofOffByOne,
        Fault::DotEntries,
        Fault::NoHardlinkNlink,
        Fault::RenameNoReplace,
        Fault::LinkReplaces,
        Fault::NoCtimeUpdate,
        Fault::WriteNoMtime,
        Fault::UnlinkFreesOpen,
        Fault::StaleNeverReclaims,
        Fault::TruncateNoZeroFill,
        Fault::ModeNotMasked,
        Fault::ReadPadsEof,
        Fault::SymlinkSetattrFollows,
        Fault::LinkNoCtime,
        Fault::UnlinkLeaksSpace,
        Fault::TruncateKeepsPages,
        Fault::RenameNonEmptyDirOk,
        Fault::HoleGarbage,
        Fault::ReaddirMax1Skips,
        Fault::ForgetOffByOne,
        Fault::RmdirNoParentNlink,
        Fault::SymlinkSizeChars,
        Fault::CreateModeNotMasked,
        Fault::SetattrModeNotMasked,
        Fault::XattrReplaceIgnored,
        Fault::XattrCreateIgnored,
        Fault::ReadlinkAnyOk,
        Fault::UnlinkedNlinkOne,
        Fault::RenameNewIno,
        Fault::ReadPadsCrossing,
        Fault::ConcurrentCreateRace,
        Fault::WriteNoCtime,
        Fault::BlocksZero,
        Fault::DirNlinkCountsFiles,
        Fault::DirParentStale,
        Fault::RenameDestNoDrop,
        Fault::RenameDirOverDirNlink,
        Fault::ShortWrite,
        Fault::ShortRead32K,
        Fault::LookupNoRef,
        Fault::LinkResurrects,
        Fault::UnlinkGhostEntry,
        Fault::RenameDirZeroesChildren,
        Fault::WriteEnforcesMode,
        Fault::ModeDriftOnWrite,
        Fault::StaleNlinkLookup,
        Fault::HidesDotUnderscore,
        Fault::TruncateNoMtime,
        Fault::SetattrNoCtime,
        Fault::CreateNoParentTimes,
        Fault::BlocksDouble,
        Fault::ReadShort1,
        Fault::XattrRemoveNoop,
        Fault::ReaddirMaxZeroOk,
        Fault::XattrNameUnchecked,
    ];
}

struct Dir {
    entries: BTreeMap<Vec<u8>, (u64, Ino)>,
    order: BTreeMap<u64, (Vec<u8>, Ino)>,
    next_seq: u64,
    parent: Ino,
}

enum Body {
    File(Pages),
    Dir(Dir),
    Symlink { target: Vec<u8>, parent: Ino },
}

struct Node {
    body: Body,
    mode: u32,
    nlink: u32,
    atime: Timestamp,
    mtime: Timestamp,
    ctime: Timestamp,
    lookups: u64,
    opens: u64,
    xattrs: BTreeMap<Vec<u8>, Vec<u8>>,
}

struct State {
    nodes: HashMap<Ino, Node>,
    handles: HashMap<u64, Ino>,
    next_ino: Ino,
    next_handle: u64,
    last: Timestamp,
    fault: Option<Fault>,
    leak: u64,
    link_max: u32,
}

/// In-memory reference implementation of `Vfs`, guarded by one coarse lock.
/// It is the executable definition of the semantics the conformance suite checks.
#[derive(Debug)]
pub struct MemVfs {
    state: Mutex<State>,
}

impl std::fmt::Debug for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "State({} inodes)", self.nodes.len())
    }
}

impl Default for MemVfs {
    fn default() -> Self {
        Self::new()
    }
}

impl MemVfs {
    pub fn new() -> Self {
        Self::build(None, LINK_MAX)
    }

    /// A filesystem whose files and symlinks refuse a new name once they have `link_max` names,
    /// with `Error::TooManyLinks`.
    pub fn with_link_max(link_max: u32) -> Self {
        Self::build(None, link_max)
    }

    /// A deliberately broken filesystem for mutation checks.
    #[doc(hidden)]
    pub fn with_fault(fault: Fault) -> Self {
        Self::build(Some(fault), LINK_MAX)
    }

    fn build(fault: Option<Fault>, link_max: u32) -> Self {
        let mut st = State {
            nodes: HashMap::new(),
            handles: HashMap::new(),
            next_ino: ROOT_INO + 1,
            next_handle: 1,
            last: Timestamp::default(),
            fault,
            leak: 0,
            link_max,
        };
        let t = st.now();
        st.nodes.insert(
            ROOT_INO,
            Node::new(Body::Dir(Dir::new(ROOT_INO)), 0o755, 2, t),
        );
        Self {
            state: Mutex::new(st),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }
}

impl Dir {
    fn new(parent: Ino) -> Self {
        Self {
            entries: BTreeMap::new(),
            order: BTreeMap::new(),
            next_seq: 1,
            parent,
        }
    }
}

impl Node {
    fn new(body: Body, mode: u32, nlink: u32, t: Timestamp) -> Self {
        Self {
            body,
            mode,
            nlink,
            atime: t,
            mtime: t,
            ctime: t,
            lookups: 0,
            opens: 0,
            xattrs: BTreeMap::new(),
        }
    }

    fn kind(&self) -> FileKind {
        match self.body {
            Body::File(_) => FileKind::Regular,
            Body::Dir(_) => FileKind::Directory,
            Body::Symlink { .. } => FileKind::Symlink,
        }
    }
}

fn add_nanos(t: Timestamp, n: u32) -> Timestamp {
    let nanos = t.nanos + n;
    if nanos >= 1_000_000_000 {
        Timestamp {
            secs: t.secs + 1,
            nanos: nanos - 1_000_000_000,
        }
    } else {
        Timestamp { nanos, ..t }
    }
}

impl State {
    fn f(&self, x: Fault) -> bool {
        self.fault == Some(x)
    }

    /// Strictly increasing wall-clock time.
    fn now(&mut self) -> Timestamp {
        let t = Timestamp::now().max(add_nanos(self.last, 1));
        self.last = t;
        t
    }

    fn node(&self, ino: Ino) -> Result<&Node> {
        self.nodes.get(&ino).ok_or(Error::Stale)
    }

    fn node_mut(&mut self, ino: Ino) -> Result<&mut Node> {
        self.nodes.get_mut(&ino).ok_or(Error::Stale)
    }

    fn dir(&self, ino: Ino) -> Result<&Dir> {
        match &self.node(ino)?.body {
            Body::Dir(d) => Ok(d),
            _ => Err(Error::NotDir),
        }
    }

    fn dir_mut(&mut self, ino: Ino) -> Result<&mut Dir> {
        match &mut self.node_mut(ino)?.body {
            Body::Dir(d) => Ok(d),
            _ => Err(Error::NotDir),
        }
    }

    /// A directory that still has a name, so new entries may be added to it.
    fn live_dir(&self, ino: Ino) -> Result<&Dir> {
        let d = self.dir(ino)?;
        if self.node(ino)?.nlink == 0 {
            return Err(Error::NotFound);
        }
        Ok(d)
    }

    fn attr(&self, ino: Ino) -> Result<Attr> {
        let n = self.node(ino)?;
        let (size, blocks) = match &n.body {
            Body::File(p) => (
                p.size,
                if self.f(Fault::BlocksZero) {
                    0
                } else if self.f(Fault::BlocksDouble) {
                    p.blocks() * 2
                } else {
                    p.blocks()
                },
            ),
            Body::Dir(_) => (PAGE, PAGE / 512),
            Body::Symlink { target, .. } => (
                if self.f(Fault::SymlinkSizeChars) {
                    String::from_utf8_lossy(target).chars().count() as u64
                } else {
                    target.len() as u64
                },
                0,
            ),
        };
        let nlink = if self.f(Fault::NoHardlinkNlink) && n.kind() == FileKind::Regular {
            n.nlink.min(1)
        } else if self.f(Fault::UnlinkedNlinkOne) && n.nlink == 0 && n.kind() == FileKind::Regular {
            1
        } else {
            n.nlink
        };
        Ok(Attr {
            ino,
            kind: n.kind(),
            mode: n.mode,
            nlink,
            uid: 0,
            gid: 0,
            size,
            blocks,
            atime: n.atime,
            mtime: n.mtime,
            ctime: n.ctime,
        })
    }

    fn bump_ctime(&mut self, ino: Ino, t: Timestamp) {
        if self.f(Fault::NoCtimeUpdate) {
            return;
        }
        if let Some(n) = self.nodes.get_mut(&ino) {
            n.ctime = t;
        }
    }

    fn touch_dir(&mut self, ino: Ino, t: Timestamp) {
        if let Some(n) = self.nodes.get_mut(&ino) {
            n.mtime = t;
            n.ctime = t;
        }
    }

    fn touch_data(&mut self, ino: Ino, t: Timestamp) {
        if !self.f(Fault::WriteNoMtime) {
            if let Some(n) = self.nodes.get_mut(&ino) {
                n.mtime = t;
            }
        }
        if !self.f(Fault::WriteNoCtime) {
            self.bump_ctime(ino, t);
        }
    }

    fn alloc(&mut self, body: Body, mode: u32, nlink: u32) -> (Ino, Timestamp) {
        let t = self.now();
        let ino = self.next_ino;
        self.next_ino += 1;
        let mode = if self.f(Fault::ModeNotMasked) || self.f(Fault::CreateModeNotMasked) {
            mode
        } else {
            mode & MODE_MASK
        };
        self.nodes.insert(ino, Node::new(body, mode, nlink, t));
        (ino, t)
    }

    fn add_entry(&mut self, parent: Ino, name: &[u8], ino: Ino) -> Result<()> {
        let d = self.dir_mut(parent)?;
        let seq = d.next_seq;
        d.next_seq += 1;
        d.entries.insert(name.to_vec(), (seq, ino));
        d.order.insert(seq, (name.to_vec(), ino));
        Ok(())
    }

    fn del_entry(&mut self, parent: Ino, name: &[u8]) -> Result<Ino> {
        let ghost = self.f(Fault::UnlinkGhostEntry);
        let d = self.dir_mut(parent)?;
        let (seq, ino) = d.entries.remove(name).ok_or(Error::NotFound)?;
        if !ghost {
            d.order.remove(&seq);
        }
        Ok(ino)
    }

    fn child(&self, parent: Ino, name: &[u8]) -> Result<Ino> {
        self.dir(parent)?
            .entries
            .get(name)
            .map(|e| e.1)
            .ok_or(Error::NotFound)
    }

    fn reclaim(&mut self, ino: Ino) {
        if ino == ROOT_INO || self.f(Fault::StaleNeverReclaims) {
            return;
        }
        let Some(n) = self.nodes.get(&ino) else {
            return;
        };
        let pinned = n.opens > 0 || n.lookups > 0;
        let leak_it = self.f(Fault::UnlinkLeaksSpace);
        let leaked = if let Body::File(p) = &n.body {
            p.blocks() / 8
        } else {
            0
        };
        if n.nlink == 0
            && (!pinned || (self.f(Fault::UnlinkFreesOpen) && n.kind() == FileKind::Regular))
        {
            if leak_it {
                self.leak += leaked;
            }
            self.nodes.remove(&ino);
        }
    }

    fn handed_out(&mut self, ino: Ino) -> Result<Attr> {
        self.node_mut(ino)?.lookups += 1;
        self.attr(ino)
    }

    fn new_entry(&mut self, parent: Ino, name: &[u8], body: Body, mode: u32) -> Result<Attr> {
        validate_name(name)?;
        self.live_dir(parent)?;
        if self.dir(parent)?.entries.contains_key(name) {
            return Err(Error::Exists);
        }
        let is_dir = matches!(body, Body::Dir(_));
        let (ino, t) = self.alloc(body, mode, if is_dir { 2 } else { 1 });
        self.add_entry(parent, name, ino)?;
        if !self.f(Fault::CreateNoParentTimes) {
            self.touch_dir(parent, t);
        }
        if is_dir || self.f(Fault::DirNlinkCountsFiles) {
            self.node_mut(parent)?.nlink += 1;
        }
        self.handed_out(ino)
    }

    fn is_within(&self, mut dir: Ino, ancestor: Ino) -> bool {
        loop {
            if dir == ancestor {
                return true;
            }
            if dir == ROOT_INO {
                return false;
            }
            match self.dir(dir) {
                Ok(d) => dir = d.parent,
                Err(_) => return false,
            }
        }
    }

    fn drop_name(&mut self, ino: Ino, t: Timestamp) {
        let is_dir = matches!(self.nodes.get(&ino).map(|n| &n.body), Some(Body::Dir(_)));
        if let Some(n) = self.nodes.get_mut(&ino) {
            n.nlink = if is_dir { 0 } else { n.nlink.saturating_sub(1) };
        }
        if !is_dir {
            self.bump_ctime(ino, t);
        }
        self.reclaim(ino);
    }
}

mod vfs_impl;
