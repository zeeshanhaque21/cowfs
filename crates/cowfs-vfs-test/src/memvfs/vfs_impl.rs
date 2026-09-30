use super::*;

const XATTR_VALUE_MAX: usize = 65536;

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
        let ino = st.child(parent, name)?;
        st.handed_out(ino)
    }

    fn forget(&self, ino: Ino, count: u64) {
        let mut st = self.lock();
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
        let raw_mode = st.f(Fault::ModeNotMasked);
        let n = st.node_mut(ino)?;
        if let (Some(size), Body::File(p)) = (ch.size, &mut n.body) {
            p.truncate(size, zero_tail);
            n.mtime = t;
        }
        if let Some(mode) = ch.mode {
            n.mode = if raw_mode { mode } else { mode & MODE_MASK };
        }
        apply_times(n, &ch, t);
        st.bump_ctime(ino, t);
        st.attr(ino)
    }

    fn readlink(&self, ino: Ino) -> Result<Vec<u8>> {
        match &self.lock().node(ino)?.body {
            Body::Symlink { target, .. } => Ok(target.clone()),
            _ => Err(Error::InvalidArgument),
        }
    }

    fn create(&self, parent: Ino, name: &[u8], mode: u32) -> Result<Attr> {
        self.lock()
            .new_entry(parent, name, Body::File(Pages::default()), mode)
    }

    fn mkdir(&self, parent: Ino, name: &[u8], mode: u32) -> Result<Attr> {
        self.lock()
            .new_entry(parent, name, Body::Dir(Dir::new(parent)), mode)
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
        if n.nlink == 0 {
            return Err(Error::NotFound);
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
        st.bump_ctime(ino, t);
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
        let p = st.node_mut(parent)?;
        p.nlink = p.nlink.saturating_sub(1);
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
                    if !st.dir(d)?.entries.is_empty() {
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
            if dest_dir == Some(true) {
                let p = st.node_mut(new_parent)?;
                p.nlink = p.nlink.saturating_sub(1);
            }
            st.drop_name(d, t);
        }
        st.del_entry(parent, name)?;
        st.add_entry(new_parent, new_name, src)?;
        if src_dir {
            if parent != new_parent {
                let p = st.node_mut(parent)?;
                p.nlink = p.nlink.saturating_sub(1);
                st.node_mut(new_parent)?.nlink += 1;
            }
            st.dir_mut(src)?.parent = new_parent;
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
            Body::File(p) => Ok(p.read(offset, u64::from(size), st.f(Fault::ReadPadsEof))),
            Body::Dir(_) => Err(Error::IsDir),
            Body::Symlink { .. } => Err(Error::InvalidArgument),
        }
    }

    fn write(&self, ino: Ino, offset: u64, data: &[u8]) -> Result<u32> {
        let mut st = self.lock();
        let len = u32::try_from(data.len()).map_err(|_| Error::InvalidArgument)?;
        let n = st.node_mut(ino)?;
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
        p.write(offset, data);
        let t = st.now();
        st.touch_data(ino, t);
        Ok(len)
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

    fn statfs(&self) -> Result<StatFs> {
        let st = self.lock();
        let used: u64 = st
            .nodes
            .values()
            .map(|n| match &n.body {
                Body::File(p) => p.blocks() / 8,
                _ => 1,
            })
            .sum();
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
        let n = st.node_mut(ino)?;
        if name.is_empty() || (flags.create && flags.replace) {
            return Err(Error::InvalidArgument);
        }
        if value.len() > XATTR_VALUE_MAX {
            return Err(Error::Range);
        }
        let exists = n.xattrs.contains_key(name);
        if flags.create && exists {
            return Err(Error::Exists);
        }
        if flags.replace && !exists {
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
        st.node_mut(ino)?.xattrs.remove(name).ok_or(Error::NoAttr)?;
        let t = st.now();
        st.bump_ctime(ino, t);
        Ok(())
    }
}
