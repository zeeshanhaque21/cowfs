//! File content, attributes, handles, xattrs and statfs.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use cowfs_vfs::{
    Attr, Error, FileHandle, FileKind, Ino, Result, SetAttr, SetTime, StatFs, Timestamp,
    XattrFlags, MODE_MASK, NAME_MAX, ROOT_INO,
};

use crate::error::{from_meta, stale};
use crate::file::{read_range, verify_partial, MAX_FILE};
use crate::inner::{mino, Inner};
use crate::node::{Node, NodeState};
use crate::queue::SnapCtx;
use crate::util::{MutexExt, RwExt};

const XATTR_VALUE_MAX: usize = 65536;
const FILES: u64 = 1 << 32;
const BLOCK: u64 = 4096;

impl Inner {
    /// A live regular file, with the right error for other kinds.
    fn file_node(&self, ino: Ino) -> Result<(Arc<SnapCtx>, Arc<Node>)> {
        if ino == ROOT_INO {
            return Err(Error::IsDir);
        }
        let n = self.live(ino)?;
        let kind = n.st.rd().attr.kind;
        match kind {
            FileKind::Regular => {}
            FileKind::Directory => return Err(Error::IsDir),
            _ => return Err(Error::InvalidArgument),
        }
        Ok((self.snapctx(ino)?, n))
    }

    pub(crate) fn op_read(&self, ino: Ino, off: u64, size: u32) -> Result<Vec<u8>> {
        let (sc, node) = self.file_node(ino)?;
        if let Some(e) = node.poisoned() {
            return Err(e);
        }
        self.ensure_file(&sc, &node)?;
        let (chunks, overlay, end) = {
            let st = node.st.rd();
            let fsize = st.attr.size;
            if off >= fsize || size == 0 {
                return Ok(Vec::new());
            }
            let end = fsize
                .min(off.saturating_add(u64::from(size)))
                .min(off.saturating_add(crate::MAX_READ_BYTES));
            let Some(f) = st.file.as_ref() else {
                return Err(Error::Stale);
            };
            (f.chunks.clone(), f.overlay(off, end), end)
        };
        read_range(&self.blocks, &chunks, &overlay, off, end)
    }

    pub(crate) fn op_write(&self, ino: Ino, off: u64, data: &[u8]) -> Result<u32> {
        let len = u32::try_from(data.len()).map_err(|_| Error::InvalidArgument)?;
        let (sc, node) = self.file_node(ino)?;
        if off.saturating_add(u64::from(len)) > MAX_FILE {
            return Err(Error::NoSpace);
        }
        if len == 0 {
            return Ok(0);
        }
        self.ensure_file(&sc, &node)?;
        // A write that only partly covers a chunk needs that chunk's old bytes, so verify it now:
        // a damaged block is EIO at the write, not a latent failure at the next flush.
        for _ in 0..8 {
            let chunks = node.st.rd().file.as_ref().map(|f| f.chunks.clone());
            let Some(chunks) = chunks else {
                return Err(Error::Stale);
            };
            let view = node.st.rd();
            let Some(f) = view.file.as_ref() else {
                return Err(Error::Stale);
            };
            if Arc::ptr_eq(&f.chunks, &chunks) {
                drop(view);
                verify_partial(&self.blocks, &chunks, off, off + u64::from(len))?;
                break;
            }
        }
        let now = Timestamp::now();
        {
            let mut st = node.st.wr();
            if let Some(e) = node.poisoned() {
                return Err(e);
            }
            let NodeState { attr, file, .. } = &mut *st;
            let Some(f) = file.as_mut() else {
                return Err(Error::Stale);
            };
            let before = f.dirty_bytes();
            attr.size = attr.size.max(off + u64::from(len));
            f.write(off, data);
            let after = f.dirty_bytes();
            attr.mtime = now;
            attr.ctime = now;
            if after >= before {
                self.dirty_bytes.fetch_add(after - before, Ordering::AcqRel);
            } else {
                self.dirty_bytes.fetch_sub(before - after, Ordering::AcqRel);
            }
            if before == 0 || node.seq.load(Ordering::Acquire) <= sc.drained() {
                let mut q = sc.q.lk();
                if before == 0 {
                    q.add_dirty_file(ino);
                }
                if node.seq.load(Ordering::Acquire) <= sc.drained() {
                    q.touch(&node);
                }
            }
            if after >= self.opts.file_flush_bytes {
                if let Err(e) = self.flush_locked(&sc, &node, &mut st) {
                    // the write is reported as failed either way; only a corruption is permanent
                    let err = if Node::classify(&e) {
                        let err = node.poison(format!("{e} (file {ino:#x})"));
                        self.ctr.poisoned.fetch_add(1, Ordering::Relaxed);
                        err
                    } else {
                        node.degrade(format!("{e} (file {ino:#x})"));
                        self.ctr.transient.fetch_add(1, Ordering::Relaxed);
                        e
                    };
                    *self.last_error.lk() = Some(err.to_string());
                    return Err(err);
                }
            }
        }
        self.relieve();
        Ok(len)
    }

    pub(crate) fn op_setattr(&self, ino: Ino, ch: SetAttr) -> Result<Attr> {
        if ino == ROOT_INO {
            if ch.size.is_some() {
                return Err(Error::IsDir);
            }
            return Ok(self.root_attr());
        }
        let node = self.live(ino)?;
        let sc = self.snapctx(ino)?;
        let kind = node.st.rd().attr.kind;
        if let Some(size) = ch.size {
            match kind {
                FileKind::Directory => return Err(Error::IsDir),
                FileKind::Regular if size > MAX_FILE => return Err(Error::NoSpace),
                FileKind::Regular => {}
                _ => return Err(Error::InvalidArgument),
            }
        }
        if ch.size.is_some() {
            self.ensure_file(&sc, &node)?;
        }
        if let Some(e) = node.poisoned() {
            return Err(e);
        }
        let now = Timestamp::now();
        let pick = |t: SetTime| match t {
            SetTime::Now => now,
            SetTime::At(x) => x,
        };
        let mut st = node.st.wr();
        if let Some(size) = ch.size {
            let cur = st.attr.size;
            let NodeState { attr, file, .. } = &mut *st;
            let Some(f) = file.as_mut() else {
                return Err(Error::Stale);
            };
            if size < cur {
                let n0 = f.dirty_bytes();
                f.flush(&self.blocks)?;
                self.dirty_bytes.fetch_sub(n0, Ordering::AcqRel);
                f.truncate(&self.blocks, size)?;
            }
            attr.size = size;
            attr.mtime = now;
            self.queue_content(&sc, &node, &st);
        }
        if let Some(m) = ch.mode {
            st.attr.mode = m & MODE_MASK;
        }
        if let Some(t) = ch.atime {
            st.attr.atime = pick(t);
        }
        if let Some(t) = ch.mtime {
            st.attr.mtime = pick(t);
        }
        st.attr.ctime = now;
        {
            let mut q = sc.q.lk();
            if node.seq.load(Ordering::Acquire) <= sc.drained() {
                q.touch(&node);
            }
        }
        Ok(st.report())
    }

    pub(crate) fn op_readlink(&self, ino: Ino) -> Result<Vec<u8>> {
        if ino == ROOT_INO {
            return Err(Error::InvalidArgument);
        }
        let n = self.live(ino)?;
        if n.st.rd().attr.kind != FileKind::Symlink {
            return Err(Error::InvalidArgument);
        }
        let sc = self.snapctx(ino)?;
        Ok(self.ensure_target(&sc, &n)?.to_vec())
    }

    pub(crate) fn op_open(&self, ino: Ino) -> Result<FileHandle> {
        if ino != ROOT_INO {
            let n = self.live(ino)?;
            n.handles.fetch_add(1, Ordering::AcqRel);
            self.snapctx(ino)?
                .open_handles
                .fetch_add(1, Ordering::AcqRel);
        }
        let h = self.next_handle.fetch_add(1, Ordering::Relaxed);
        self.handles.lk().insert(h, ino);
        Ok(FileHandle(h))
    }

    pub(crate) fn op_release(&self, h: FileHandle) -> Result<()> {
        let ino = self
            .handles
            .lk()
            .remove(&h.0)
            .ok_or(Error::InvalidArgument)?;
        if ino != ROOT_INO {
            if let Some(n) = self.nodes.get(&ino) {
                let _ = n
                    .handles
                    .fetch_update(Ordering::AcqRel, Ordering::Acquire, |v| v.checked_sub(1));
                if let Ok(sc) = self.snapctx(ino) {
                    let _ =
                        sc.open_handles
                            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |v| {
                                v.checked_sub(1)
                            });
                }
                self.try_reclaim(&n);
            }
        }
        Ok(())
    }

    pub(crate) fn op_forget(&self, ino: Ino, count: u64) {
        if ino == ROOT_INO {
            return;
        }
        let Some(n) = self.nodes.get(&ino) else {
            return;
        };
        let mut under = false;
        let _ = n
            .refs
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |v| {
                under = v < count;
                Some(v.saturating_sub(count))
            });
        if under {
            self.ctr.underflows.fetch_add(1, Ordering::Relaxed);
        }
        self.try_reclaim(&n);
    }

    pub(crate) fn op_fsync(&self, ino: Ino) -> Result<()> {
        if ino == ROOT_INO {
            // the trait defines this as the whole-mount barrier
            return self.sync_all();
        }
        let n = self.live(ino)?;
        if let Some(e) = n.poisoned() {
            return Err(e);
        }
        let sc = self.snapctx(ino)?;
        // the file is back in the dirty set if a transient failure kept it there, so this retries
        // the store for it; only a failure that survives is reported
        self.fsync_snapshot(&sc)?;
        if let Some(e) = n.poisoned() {
            return Err(e);
        }
        if let Some(e) = n.degraded() {
            return Err(e);
        }
        Ok(())
    }

    pub(crate) fn op_flush(&self, ino: Ino) -> Result<()> {
        if ino != ROOT_INO {
            if let Some(e) = self.live(ino)?.poisoned() {
                return Err(e);
            }
        }
        Ok(())
    }

    /// Meta inode number of `node`, committing pending work first when it has none yet.
    fn committed_meta(&self, sc: &SnapCtx, node: &Node) -> Result<u64> {
        if let Some(m) = self.meta_of(node.ino) {
            return Ok(m);
        }
        self.barrier(sc)?;
        self.meta_of(node.ino).ok_or(Error::Stale)
    }

    pub(crate) fn op_getxattr(&self, ino: Ino, name: &[u8]) -> Result<Vec<u8>> {
        if ino == ROOT_INO {
            return Err(Error::NoAttr);
        }
        let n = self.live(ino)?;
        {
            let st = n.st.rd();
            if let Some(map) = &st.xattrs {
                return map.get(name).cloned().ok_or(Error::NoAttr);
            }
        }
        let sc = self.snapctx(ino)?;
        let Some(m) = self.meta_of(ino) else {
            return Err(Error::NoAttr);
        };
        sc.snap
            .getxattr(mino(m), name)
            .map_err(from_meta)
            .map_err(stale)
    }

    pub(crate) fn op_listxattr(&self, ino: Ino) -> Result<Vec<Vec<u8>>> {
        if ino == ROOT_INO {
            return Ok(Vec::new());
        }
        let n = self.live(ino)?;
        {
            let st = n.st.rd();
            if let Some(map) = &st.xattrs {
                return Ok(map.keys().cloned().collect());
            }
        }
        let sc = self.snapctx(ino)?;
        let Some(m) = self.meta_of(ino) else {
            return Ok(Vec::new());
        };
        sc.snap.listxattr(mino(m)).map_err(from_meta).map_err(stale)
    }

    pub(crate) fn op_setxattr(
        &self,
        ino: Ino,
        name: &[u8],
        value: &[u8],
        flags: XattrFlags,
    ) -> Result<()> {
        if ino == ROOT_INO {
            return Err(Error::ReadOnly);
        }
        let n = self.live(ino)?;
        let sc = self.snapctx(ino)?;
        if name.is_empty() || name.contains(&0) || (flags.create && flags.replace) {
            return Err(Error::InvalidArgument);
        }
        if value.len() > XATTR_VALUE_MAX || name.len() > NAME_MAX {
            return Err(Error::Range);
        }
        let _ns = sc.ns.lk();
        let now = Timestamp::now();
        let exists = self.xattr_exists(&sc, &n, name)?;
        if flags.create && exists {
            return Err(Error::Exists);
        }
        if flags.replace && !exists {
            return Err(Error::NoAttr);
        }
        {
            let mut st = n.st.wr();
            if let Some(map) = st.xattrs.as_mut() {
                map.insert(name.to_vec(), value.to_vec());
                st.attr.ctime = now;
                return Ok(());
            }
        }
        // The barrier and the meta write can wait for another thread's store fsync, so they run
        // without the namespace lock: an xattr belongs to an inode, and nothing here renames it.
        drop(_ns);
        let m = self.committed_meta(&sc, &n)?;
        sc.snap
            .setxattr(mino(m), name, value)
            .map_err(from_meta)
            .map_err(stale)?;
        n.st.wr().attr.ctime = now;
        Ok(())
    }

    fn xattr_exists(&self, sc: &SnapCtx, n: &Node, name: &[u8]) -> Result<bool> {
        if let Some(map) = &n.st.rd().xattrs {
            return Ok(map.contains_key(name));
        }
        let Some(m) = self.meta_of(n.ino) else {
            return Ok(false);
        };
        match sc.snap.getxattr(mino(m), name) {
            Ok(_) => Ok(true),
            Err(cowfs_meta::Error::NoAttr) => Ok(false),
            Err(e) => Err(stale(from_meta(e))),
        }
    }

    pub(crate) fn op_removexattr(&self, ino: Ino, name: &[u8]) -> Result<()> {
        if ino == ROOT_INO {
            return Err(Error::ReadOnly);
        }
        let n = self.live(ino)?;
        let sc = self.snapctx(ino)?;
        let ns = sc.ns.lk();
        let now = Timestamp::now();
        {
            let mut st = n.st.wr();
            if let Some(map) = st.xattrs.as_mut() {
                map.remove(name).ok_or(Error::NoAttr)?;
                st.attr.ctime = now;
                return Ok(());
            }
        }
        let Some(m) = self.meta_of(ino) else {
            return Err(Error::NoAttr);
        };
        // a meta commit can wait for another thread's store fsync, so it runs unlocked
        drop(ns);
        sc.snap
            .removexattr(mino(m), name)
            .map_err(from_meta)
            .map_err(stale)?;
        n.st.wr().attr.ctime = now;
        Ok(())
    }

    pub(crate) fn op_statfs(&self) -> Result<StatFs> {
        let used_bytes = self
            .blocks
            .store
            .stats()
            .pack_bytes
            .saturating_sub(self.base_pack_bytes)
            + self.dirty_bytes.load(Ordering::Acquire) as u64;
        let used = used_bytes.div_ceil(BLOCK);
        let free = self.capacity_blocks.saturating_sub(used);
        let net = self.ctr.inodes_net.load(Ordering::Relaxed).max(0) as u64;
        Ok(StatFs {
            block_size: BLOCK as u32,
            blocks: self.capacity_blocks,
            blocks_free: free,
            blocks_available: free,
            files: FILES,
            files_free: FILES.saturating_sub(net),
            name_max: NAME_MAX as u32,
        })
    }
}
