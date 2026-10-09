use super::*;
use cowfs_vfs::{DirEntryPlus, ReadDirPlus};

const XATTR_VALUE_MAX: usize = 65536;

fn st_fault_readdir_attrs(v: &MemVfs) -> bool {
    let st = v.lock();
    st.f(Fault::ReaddirAttrsNoAttrs)
}

fn apply_times(n: &mut Node, ch: &SetAttr, t: Timestamp) {
    let pick = |s: SetTime| match s {
        SetTime::Now => t,
        SetTime::At(x) => x,
    };
    if let Some(a) = ch.atime {
        n.atime = pick(a);
    }
    if let Some(m) = ch.mtime {
        n.mtime = pick(m);
    }
}

impl Vfs for MemVfs {
    fn lookup(&self, parent: Ino, name: &[u8]) -> Result<Attr> {
        let mut st = self.lock();
        st.dir(parent)?;
        validate_name(name)?;
        if st.f(Fault::HidesDotUnderscore) && name.starts_with(b"._") {
            return Err(Error::NotFound);
        }
        let ino = st.child(parent, name)?;
        if st.f(Fault::LookupNoRef) {
            return st.attr(ino);
        }
        let mut a = st.handed_out(ino)?;
        if st.f(Fault::StaleNlinkLookup) && a.kind == FileKind::Regular {
            a.nlink = a.nlink.min(1);
        }
        Ok(a)
    }

    fn forget(&self, ino: Ino, count: u64) {
        let mut st = self.lock();
        let count = if st.f(Fault::ForgetOffByOne) {
            count.saturating_sub(1)
        } else {
            count
        };
        if let Some(n) = st.nodes.get_mut(&ino) {
            n.lookups = n.lookups.saturating_sub(count);
        }
        st.reclaim(ino);
    }

    fn getattr(&self, ino: Ino) -> Result<Attr> {
        self.lock().attr(ino)
    }

    fn setattr(&self, ino: Ino, ch: SetAttr) -> Result<Attr> {
        let mut st = self.lock();
        let kind = st.node(ino)?.kind();
        if let Some(size) = ch.size {
            match kind {
                FileKind::Directory => return Err(Error::IsDir),
                FileKind::Symlink => return Err(Error::InvalidArgument),
                FileKind::Regular if size > MAX_FILE => return Err(Error::NoSpace),
                FileKind::Regular => {}
                _ => return Err(Error::InvalidArgument),
            }
        }
        let t = st.now();
        if st.f(Fault::SymlinkSetattrFollows) {
            if let Body::Symlink { target, parent } = &st.node(ino)?.body {
                let (target, parent) = (target.clone(), *parent);
                if let Ok(tino) = st.child(parent, &target) {
                    apply_times(st.node_mut(tino)?, &ch, t);
                    return st.attr(ino);
                }
            }
        }
        let zero_tail = !st.f(Fault::TruncateNoZeroFill);
        let keep = st.f(Fault::TruncateKeepsPages);
        let raw_mode = st.f(Fault::ModeNotMasked) || st.f(Fault::SetattrModeNotMasked);
        let no_mtime = st.f(Fault::TruncateNoMtime);
        let no_ctime = st.f(Fault::SetattrNoCtime);
        let n = st.node_mut(ino)?;
        if let (Some(size), Body::File(p)) = (ch.size, &mut n.body) {
            p.truncate(size, zero_tail, keep);
            if !no_mtime {
                n.mtime = t;
            }
        }
        if let Some(mode) = ch.mode {
            n.mode = if raw_mode { mode } else { mode & MODE_MASK };
        }
        apply_times(n, &ch, t);
        if !no_ctime {
            st.bump_ctime(ino, t);
        }
        st.attr(ino)
    }

    fn readlink(&self, ino: Ino) -> Result<Vec<u8>> {
        let st = self.lock();
        let lax = st.f(Fault::ReadlinkAnyOk);
        match &st.node(ino)?.body {
            Body::Symlink { target, .. } => Ok(target.clone()),
            _ if lax => Ok(Vec::new()),
            _ => Err(Error::InvalidArgument),
        }
    }

    fn create(&self, parent: Ino, name: &[u8], mode: u32) -> Result<Attr> {
        if self.lock().f(Fault::ConcurrentCreateRace) {
            {
                let st = self.lock();
                validate_name(name)?;
                st.live_dir(parent)?;
                if st.dir(parent)?.entries.contains_key(name) {
                    return Err(Error::Exists);
                }
            }
            std::thread::yield_now();
            let mut st = self.lock();
            let (ino, t) = st.alloc(Body::File(Pages::default()), mode, 1);
            st.add_entry(parent, name, ino)?;
            st.touch_dir(parent, t);
            return st.handed_out(ino);
        }
        self.lock()
            .new_entry(parent, name, Body::File(Pages::default()), mode)
    }

    fn mkdir(&self, parent: Ino, name: &[u8], mode: u32) -> Result<Attr> {
        self.lock()
            .new_entry(parent, name, Body::Dir(Dir::new(parent)), mode)
    }

    fn mknod(
        &self,
        parent: Ino,
        name: &[u8],
        kind: FileKind,
        mode: u32,
        rdev: u64,
    ) -> Result<Attr> {
        if !kind.is_special() || (rdev != 0 && !kind.is_device()) {
            return Err(Error::InvalidArgument);
        }
        let mut st = self.lock();
        let rdev = if st.f(Fault::MknodDropsRdev) { 0 } else { rdev };
        let skip_times = st.f(Fault::MknodNoParentTimes);
        let before = st.nodes.get(&parent).map(|n| (n.mtime, n.ctime));
        let a = st.new_entry(parent, name, Body::Special { kind, rdev }, mode)?;
        if let (true, Some((m, c))) = (skip_times, before) {
            if let Some(n) = st.nodes.get_mut(&parent) {
                n.mtime = m;
                n.ctime = c;
            }
        }
        Ok(a)
    }

    fn symlink(&self, parent: Ino, name: &[u8], target: &[u8]) -> Result<Attr> {
        if target.is_empty() {
            return Err(Error::InvalidArgument);
        }
        let body = Body::Symlink {
            target: target.to_vec(),
            parent,
        };
        self.lock().new_entry(parent, name, body, 0o777)
    }

    fn link(&self, ino: Ino, new_parent: Ino, new_name: &[u8]) -> Result<Attr> {
        let mut st = self.lock();
        let n = st.node(ino)?;
        if n.kind() == FileKind::Directory {
            return Err(Error::PermissionDenied);
        }
        if n.nlink == 0 && !st.f(Fault::LinkResurrects) {
            return Err(Error::NotFound);
        }
        if n.nlink >= st.link_max {
            return Err(Error::TooManyLinks);
        }
        validate_name(new_name)?;
        st.live_dir(new_parent)?;
        if let Some(&(_, old)) = st.dir(new_parent)?.entries.get(new_name) {
            if !st.f(Fault::LinkReplaces) {
                return Err(Error::Exists);
            }
            st.del_entry(new_parent, new_name)?;
            let t = st.now();
            st.drop_name(old, t);
        }
        let t = st.now();
        st.add_entry(new_parent, new_name, ino)?;
        st.node_mut(ino)?.nlink += 1;
        if !st.f(Fault::LinkNoCtime) {
            st.bump_ctime(ino, t);
        }
        st.touch_dir(new_parent, t);
        st.handed_out(ino)
    }

    fn unlink(&self, parent: Ino, name: &[u8]) -> Result<()> {
        let mut st = self.lock();
        st.dir(parent)?;
        validate_name(name)?;
        let ino = st.child(parent, name)?;
        if st.node(ino)?.kind() == FileKind::Directory {
            return Err(Error::IsDir);
        }
        st.del_entry(parent, name)?;
        let t = st.now();
        st.touch_dir(parent, t);
        st.drop_name(ino, t);
        Ok(())
    }

    fn rmdir(&self, parent: Ino, name: &[u8]) -> Result<()> {
        let mut st = self.lock();
        st.dir(parent)?;
        validate_name(name)?;
        let ino = st.child(parent, name)?;
        if !st.dir(ino)?.entries.is_empty() {
            return Err(Error::NotEmpty);
        }
        st.del_entry(parent, name)?;
        let t = st.now();
        let skip = st.f(Fault::RmdirNoParentNlink);
        let p = st.node_mut(parent)?;
        if !skip {
            p.nlink = p.nlink.saturating_sub(1);
        }
        st.touch_dir(parent, t);
        st.drop_name(ino, t);
        Ok(())
    }

    fn rename(
        &self,
        parent: Ino,
        name: &[u8],
        new_parent: Ino,
        new_name: &[u8],
        flags: RenameFlags,
    ) -> Result<()> {
        let mut st = self.lock();
        st.dir(parent)?;
        st.live_dir(new_parent)?;
        validate_name(name)?;
        validate_name(new_name)?;
        let src = st.child(parent, name)?;
        let dest = st.dir(new_parent)?.entries.get(new_name).map(|e| e.1);
        if let Some(d) = dest {
            if flags.no_replace || st.f(Fault::RenameNoReplace) {
                return Err(Error::Exists);
            }
            if d == src {
                return Ok(());
            }
        }
        let src_dir = st.node(src)?.kind() == FileKind::Directory;
        if src_dir && st.is_within(new_parent, src) {
            return Err(Error::InvalidArgument);
        }
        let dest_dir = match dest {
            Some(d) => Some(st.node(d)?.kind() == FileKind::Directory),
            None => None,
        };
        match (src_dir, dest_dir) {
            (true, Some(true)) => {
                if let Some(d) = dest {
                    if !st.dir(d)?.entries.is_empty() && !st.f(Fault::RenameNonEmptyDirOk) {
                        return Err(Error::NotEmpty);
                    }
                }
            }
            (true, Some(false)) => return Err(Error::NotDir),
            (false, Some(true)) => return Err(Error::IsDir),
            _ => {}
        }
        let t = st.now();
        if let Some(d) = dest {
            st.del_entry(new_parent, new_name)?;
            if dest_dir == Some(true) && !st.f(Fault::RenameDirOverDirNlink) {
                let p = st.node_mut(new_parent)?;
                p.nlink = p.nlink.saturating_sub(1);
            }
            if !st.f(Fault::RenameDestNoDrop) {
                st.drop_name(d, t);
            }
        }
        st.del_entry(parent, name)?;
        st.add_entry(new_parent, new_name, src)?;
        if src_dir {
            if parent != new_parent {
                let p = st.node_mut(parent)?;
                p.nlink = p.nlink.saturating_sub(1);
                st.node_mut(new_parent)?.nlink += 1;
            }
            if !st.f(Fault::DirParentStale) {
                st.dir_mut(src)?.parent = new_parent;
            }
        }
        if st.f(Fault::RenameDirZeroesChildren) && src_dir {
            let mut stack = vec![src];
            while let Some(d) = stack.pop() {
                let kids: Vec<Ino> = st.dir(d)?.entries.values().map(|e| e.1).collect();
                for k in kids {
                    match &mut st.node_mut(k)?.body {
                        Body::File(p) => p.truncate(0, true, false),
                        Body::Dir(_) => stack.push(k),
                        _ => {}
                    }
                }
            }
        }
        if st.f(Fault::RenameNewIno) && !src_dir {
            let ni = st.next_ino;
            st.next_ino += 1;
            if let Some(node) = st.nodes.remove(&src) {
                st.nodes.insert(ni, node);
            }
            let d = st.dir_mut(new_parent)?;
            if let Some(e) = d.entries.get_mut(new_name) {
                e.1 = ni;
                let seq = e.0;
                if let Some(o) = d.order.get_mut(&seq) {
                    o.1 = ni;
                }
            }
        }
        st.touch_dir(parent, t);
        st.touch_dir(new_parent, t);
        st.bump_ctime(src, t);
        Ok(())
    }

    fn open(&self, ino: Ino) -> Result<FileHandle> {
        let mut st = self.lock();
        st.node_mut(ino)?.opens += 1;
        let h = st.next_handle;
        st.next_handle += 1;
        st.handles.insert(h, ino);
        Ok(FileHandle(h))
    }

    fn release(&self, handle: FileHandle) -> Result<()> {
        let mut st = self.lock();
        let ino = st.handles.remove(&handle.0).ok_or(Error::InvalidArgument)?;
        if let Some(n) = st.nodes.get_mut(&ino) {
            n.opens = n.opens.saturating_sub(1);
        }
        st.reclaim(ino);
        Ok(())
    }

    fn read(&self, ino: Ino, offset: u64, size: u32) -> Result<Vec<u8>> {
        let st = self.lock();
        match &st.node(ino)?.body {
            Body::File(p) => {
                let size = if st.f(Fault::ShortRead32K) {
                    size.min(32768)
                } else {
                    size
                };
                let pad =
                    st.f(Fault::ReadPadsEof) || (st.f(Fault::ReadPadsCrossing) && offset < p.size);
                let mut data = p.read(offset, u64::from(size), pad);
                if st.f(Fault::ReadShort1) && data.len() > 4096 {
                    data.pop();
                }
                Ok(data)
            }
            Body::Dir(_) => Err(Error::IsDir),
            Body::Special { .. } if st.f(Fault::SpecialReadOk) => Ok(Vec::new()),
            Body::Symlink { .. } | Body::Special { .. } => Err(Error::InvalidArgument),
        }
    }

    fn write(&self, ino: Ino, offset: u64, data: &[u8]) -> Result<u32> {
        let mut st = self.lock();
        let len = u32::try_from(data.len()).map_err(|_| Error::InvalidArgument)?;
        let garbage = st.f(Fault::HoleGarbage);
        let enforce = st.f(Fault::WriteEnforcesMode);
        let n = st.node_mut(ino)?;
        if enforce && n.mode & 0o222 == 0 {
            return Err(Error::PermissionDenied);
        }
        let Body::File(p) = &mut n.body else {
            return Err(if n.kind() == FileKind::Directory {
                Error::IsDir
            } else {
                Error::InvalidArgument
            });
        };
        if offset.saturating_add(u64::from(len)) > MAX_FILE {
            return Err(Error::NoSpace);
        }
        if len == 0 {
            return Ok(0);
        }
        p.write(offset, data, garbage);
        let t = st.now();
        st.touch_data(ino, t);
        if st.f(Fault::ModeDriftOnWrite) {
            st.node_mut(ino)?.mode |= 0o200;
        }
        Ok(if st.f(Fault::ShortWrite) && len > 65536 {
            len - 1
        } else {
            len
        })
    }

    fn fallocate(&self, ino: Ino, mode: FallocMode, offset: u64, len: u64) -> Result<Attr> {
        let mut st = self.lock();
        let no_extend = st.f(Fault::ZeroRangeNoExtend);
        let (noop, shrinks, punch_grows) = (
            st.f(Fault::PunchNoop),
            st.f(Fault::AllocateShrinks),
            st.f(Fault::PunchChangesSize),
        );
        let n = st.node_mut(ino)?;
        let kind = n.kind();
        let Body::File(p) = &mut n.body else {
            return Err(if kind == FileKind::Directory {
                Error::IsDir
            } else {
                Error::InvalidArgument
            });
        };
        if len == 0 {
            return Err(Error::InvalidArgument);
        }
        let end = offset
            .checked_add(len)
            .filter(|&e| e <= MAX_FILE)
            .ok_or(Error::NoSpace)?;
        let (zero, extend) = match mode {
            FallocMode::Allocate => (false, true),
            FallocMode::KeepSize => (false, false),
            FallocMode::PunchHole => (true, punch_grows),
            FallocMode::ZeroRangeKeepSize => (true, false),
            FallocMode::ZeroRange => (true, !no_extend),
            _ => return Err(Error::NotSupported),
        };
        let old = p.size;
        if zero && !noop {
            p.punch(offset, end.min(old));
        }
        if extend && (end > old || shrinks && mode == FallocMode::Allocate) {
            p.size = end;
        }
        let grew = p.size > old;
        let t = st.now();
        if zero || grew {
            st.touch_data(ino, t);
        } else {
            st.bump_ctime(ino, t);
        }
        st.attr(ino)
    }

    fn flush(&self, ino: Ino) -> Result<()> {
        self.lock().node(ino).map(|_| ())
    }

    fn fsync(&self, ino: Ino, _data_only: bool) -> Result<()> {
        self.lock().node(ino).map(|_| ())
    }

    fn readdir(&self, dir: Ino, cookie: u64, max: usize) -> Result<ReadDir> {
        let st = self.lock();
        let d = st.dir(dir)?;
        if max == 0 && !st.f(Fault::ReaddirMaxZeroOk) {
            return Err(Error::InvalidArgument);
        }
        let kind_of = |ino: Ino| st.nodes.get(&ino).map(Node::kind);
        let mk = |name: &Vec<u8>, ino: Ino, cookie: u64| {
            kind_of(ino).map(|kind| DirEntry {
                ino,
                kind,
                name: name.clone(),
                cookie,
            })
        };
        let by_position = st.f(Fault::PositionCookies);
        let by_inode = st.f(Fault::InodeCookies);
        let dots = st.f(Fault::DotEntries);
        let dot_cookie = u64::MAX - 1;
        let (cookie, mut entries) = if dots && cookie == 0 {
            let mk_dot = |n: &[u8], c| DirEntry {
                ino: dir,
                kind: FileKind::Directory,
                name: n.to_vec(),
                cookie: c,
            };
            (0, vec![mk_dot(b".", dot_cookie), mk_dot(b"..", u64::MAX)])
        } else if dots && cookie >= dot_cookie {
            (0, Vec::new())
        } else {
            (cookie, Vec::new())
        };
        let mut eof;
        if by_position || by_inode {
            let list: Vec<(&Vec<u8>, Ino)> = d.order.values().map(|(n, i)| (n, *i)).collect();
            let start = if cookie == 0 {
                0
            } else if by_inode {
                list.iter()
                    .position(|e| e.1 == cookie)
                    .map_or(list.len(), |i| i + 1)
            } else {
                usize::try_from(cookie)
                    .unwrap_or(usize::MAX)
                    .min(list.len())
            };
            for (i, (name, ino)) in list.iter().enumerate().skip(start).take(max) {
                let c = if by_inode { *ino } else { i as u64 + 1 };
                entries.extend(mk(name, *ino, c));
            }
            eof = start + max >= list.len();
        } else {
            let mut it = d
                .order
                .range((Bound::Excluded(cookie), Bound::Unbounded))
                .peekable();
            if max == 1 && cookie != 0 && st.f(Fault::ReaddirMax1Skips) {
                it.next();
            }
            let mut taken = 0;
            while taken < max {
                let Some((seq, (name, ino))) = it.next() else {
                    break;
                };
                entries.extend(mk(name, *ino, *seq));
                taken += 1;
            }
            eof = it.peek().is_none();
        }
        if st.f(Fault::EofOffByOne) {
            eof = entries.len() < max;
        }
        Ok(ReadDir { entries, eof })
    }

    fn readdir_attrs(&self, dir: Ino, cookie: u64, max: usize) -> Result<ReadDirPlus> {
        let listing = self.readdir(dir, cookie, max)?;
        let mut entries = Vec::with_capacity(listing.entries.len());
        if st_fault_readdir_attrs(self) {
            return Ok(ReadDirPlus {
                entries: Vec::new(),
                eof: listing.eof,
            });
        }
        for entry in listing.entries {
            match self.lock().attr(entry.ino) {
                Ok(attr) => entries.push(DirEntryPlus { entry, attr }),
                Err(Error::Stale | Error::NotFound) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(ReadDirPlus {
            entries,
            eof: listing.eof,
        })
    }

    fn statfs(&self) -> Result<StatFs> {
        let st = self.lock();
        let leak = st.leak;
        let used: u64 = st
            .nodes
            .values()
            .map(|n| match &n.body {
                Body::File(p) => p.blocks() / 8,
                _ => 1,
            })
            .sum::<u64>()
            + leak;
        let blocks = 1 << 30;
        let files = 1 << 32;
        Ok(StatFs {
            block_size: PAGE as u32,
            blocks,
            blocks_free: blocks - used,
            blocks_available: blocks - used,
            files,
            files_free: files - st.nodes.len() as u64,
            name_max: cowfs_vfs::NAME_MAX as u32,
        })
    }

    fn getxattr(&self, ino: Ino, name: &[u8]) -> Result<Vec<u8>> {
        let st = self.lock();
        st.node(ino)?.xattrs.get(name).cloned().ok_or(Error::NoAttr)
    }

    fn setxattr(&self, ino: Ino, name: &[u8], value: &[u8], flags: XattrFlags) -> Result<()> {
        let mut st = self.lock();
        let (ig_c, ig_r) = (
            st.f(Fault::XattrCreateIgnored),
            st.f(Fault::XattrReplaceIgnored),
        );
        let unchecked = st.f(Fault::XattrNameUnchecked);
        let n = st.node_mut(ino)?;
        if !unchecked && (name.is_empty() || name.contains(&0)) {
            return Err(Error::InvalidArgument);
        }
        if !unchecked && name.len() > cowfs_vfs::NAME_MAX {
            return Err(Error::Range);
        }
        if flags.create && flags.replace {
            return Err(Error::InvalidArgument);
        }
        if value.len() > XATTR_VALUE_MAX {
            return Err(Error::Range);
        }
        let exists = n.xattrs.contains_key(name);
        if flags.create && exists && !ig_c {
            return Err(Error::Exists);
        }
        if flags.replace && !exists && !ig_r {
            return Err(Error::NoAttr);
        }
        n.xattrs.insert(name.to_vec(), value.to_vec());
        let t = st.now();
        st.bump_ctime(ino, t);
        Ok(())
    }

    fn listxattr(&self, ino: Ino) -> Result<Vec<Vec<u8>>> {
        Ok(self.lock().node(ino)?.xattrs.keys().cloned().collect())
    }

    fn removexattr(&self, ino: Ino, name: &[u8]) -> Result<()> {
        let mut st = self.lock();
        if st.f(Fault::XattrRemoveNoop) && st.node(ino)?.xattrs.contains_key(name) {
            return Ok(());
        }
        st.node_mut(ino)?.xattrs.remove(name).ok_or(Error::NoAttr)?;
        let t = st.now();
        st.bump_ctime(ino, t);
        Ok(())
    }
}
