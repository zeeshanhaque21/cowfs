//! Namespace operations: lookup, readdir, create, link, unlink, rmdir, rename.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use cowfs_vfs::{
    validate_name, Attr, DirEntry, Error, FileKind, Ino, ReadDir, RenameFlags, Result, Timestamp,
    MODE_MASK, ROOT_INO,
};

use crate::dcache::Target;
use crate::error::{from_meta, stale};
use crate::file::FileData;
use crate::inner::{kind_of, mino, Inner};
use crate::ino::{pack, snap_of};
use crate::node::{Node, NodeState, DIR_SIZE};
use crate::queue::{Create, Op, SnapCtx};
use crate::swap;
use crate::util::{MutexExt, RwExt};

impl Inner {
    pub(crate) fn root_attr(&self) -> Attr {
        let t = *self.root_time.lk();
        Attr {
            ino: ROOT_INO,
            kind: FileKind::Directory,
            mode: 0o755,
            nlink: 2 + self.snaps.rd().by_id.len() as u32,
            uid: self.uid,
            gid: self.gid,
            size: DIR_SIZE,
            blocks: DIR_SIZE / 512,
            rdev: 0,
            atime: t,
            mtime: t,
            ctime: t,
        }
    }

    /// Wakes the flusher at `max_pending_ops` queued operations and flushes in the caller at four
    /// times that, so the queue is bounded even without the background thread.
    fn maybe_wake(&self, sc: &SnapCtx) {
        let n = sc.q.lk().op_count();
        if n >= self.opts.max_pending_ops {
            self.wake();
        }
        if n >= self.opts.max_pending_ops.saturating_mul(4) {
            let _ = self.flush_snapshot(sc);
        }
    }

    pub(crate) fn op_getattr(&self, ino: Ino) -> Result<Attr> {
        if ino == ROOT_INO {
            return Ok(self.root_attr());
        }
        let n = self.live(ino)?;
        let a = n.st.rd().report();
        Ok(a)
    }

    pub(crate) fn op_lookup(&self, parent: Ino, name: &[u8]) -> Result<Attr> {
        if parent == ROOT_INO {
            return self.root_lookup(name);
        }
        let (sc, pn) = self.dir(parent)?;
        validate_name(name)?;
        let (child, _) = self.dent_lookup(&sc, &pn, name)?.ok_or(Error::NotFound)?;
        let cn = self.node(child).map_err(|e| match e {
            Error::Stale => Error::NotFound,
            e => e,
        })?;
        cn.add_ref();
        let a = cn.st.rd().report();
        Ok(a)
    }

    fn root_lookup(&self, name: &[u8]) -> Result<Attr> {
        validate_name(name)?;
        let id = {
            let name = std::str::from_utf8(name).map_err(|_| Error::NotFound)?;
            *self.snaps.rd().by_name.get(name).ok_or(Error::NotFound)?
        };
        let n = self.node(pack(id, cowfs_meta::ROOT_INO.0)?)?;
        n.add_ref();
        let a = n.st.rd().report();
        Ok(a)
    }

    pub(crate) fn op_readdir(&self, dir: Ino, cookie: u64, max: usize) -> Result<ReadDir> {
        if dir == ROOT_INO {
            if max == 0 {
                return Err(Error::InvalidArgument);
            }
            return self.root_readdir(cookie, max);
        }
        let (sc, dn) = self.dir(dir)?;
        if max == 0 {
            return Err(Error::InvalidArgument);
        }
        if dn.st.rd().attr.nlink == 0 {
            return Ok(ReadDir {
                entries: Vec::new(),
                eof: true,
            });
        }
        if dn.ns_seq.load(Ordering::Acquire) > sc.flushed() {
            self.barrier(&sc)?;
        }
        let Some(m) = self.meta_of(dir) else {
            return Ok(ReadDir {
                entries: Vec::new(),
                eof: true,
            });
        };
        let epoch = self.dents.epoch(dir);
        let r = sc
            .snap
            .readdir(mino(m), cookie, max)
            .map_err(from_meta)
            .map_err(stale)?;
        let mut entries = Vec::with_capacity(r.entries.len());
        let mut fill: Vec<(Vec<u8>, Target)> = Vec::with_capacity(r.entries.len());
        for e in r.entries {
            let meta_ino = pack(sc.id, e.ino.0)?;
            let kind = kind_of(e.kind);
            fill.push((e.name.clone(), Some((meta_ino, kind))));
            entries.push(DirEntry {
                ino: self.canon(sc.id, e.ino.0)?,
                kind,
                name: e.name,
                cookie: e.cookie,
            });
        }
        let fill: Vec<(&[u8], Target)> = fill.iter().map(|(n, t)| (n.as_slice(), *t)).collect();
        self.dents.fill_many(dir, &fill, epoch);
        self.shrink_dents(dir);
        Ok(ReadDir {
            entries,
            eof: r.end,
        })
    }

    fn root_readdir(&self, cookie: u64, max: usize) -> Result<ReadDir> {
        let snaps = self.snaps.rd();
        let mut ids: Vec<(&String, &u64)> = snaps
            .by_name
            .iter()
            .filter(|(n, id)| **id > cookie && !swap::is_staging(n))
            .collect();
        ids.sort_by_key(|(_, id)| **id);
        let eof = ids.len() <= max;
        let mut entries = Vec::new();
        for (name, id) in ids.into_iter().take(max) {
            entries.push(DirEntry {
                ino: pack(*id, cowfs_meta::ROOT_INO.0)?,
                kind: FileKind::Directory,
                name: name.as_bytes().to_vec(),
                cookie: *id,
            });
        }
        Ok(ReadDir { entries, eof })
    }

    pub(crate) fn make(&self, parent: Ino, name: &[u8], what: Create, mode: u32) -> Result<Attr> {
        validate_name(name)?;
        if parent == ROOT_INO {
            return Err(Error::ReadOnly);
        }
        // Refuse at the session ceiling before reserving: the number would be handed out now and
        // released later, and a client still holding it would see `Stale`, so the create fails
        // here instead. Every live inode keeps one alias, so the alias count is the live count.
        let live = self.aliases.rd().len();
        if live >= self.opts.alias_limit {
            *self.last_error.lk() = Some(format!(
                "session inode limit reached: {live} inodes are live, the ceiling is {}",
                self.opts.alias_limit
            ));
            return Err(Error::NoSpace);
        }
        // Reserve before any Core lock: the refill opens a meta reservation, and no lock of ours
        // may be held across a meta transaction. A ticket popped and then unused (an early
        // `Exists`/`NotFound` below) is wasted, never reused.
        let ticket = self.take_reserved()?;
        let snap = snap_of(parent).ok_or(Error::Stale)?;
        let ino = pack(snap, ticket.ino().0)?;
        let (sc, pn) = self.dir(parent)?;
        let _ns = sc.ns.lk();
        if pn.st.rd().attr.nlink == 0 {
            return Err(Error::NotFound);
        }
        if self.dent_lookup(&sc, &pn, name)?.is_some() {
            return Err(Error::Exists);
        }
        let now = Timestamp::now();
        let (kind, nlink, size, target, mode) = match &what {
            Create::File => (FileKind::Regular, 1, 0, None, mode & MODE_MASK),
            Create::Dir => (FileKind::Directory, 2, 0, None, mode & MODE_MASK),
            Create::Symlink(t) => (FileKind::Symlink, 1, t.len() as u64, Some(t.clone()), 0o777),
        };
        let st = NodeState {
            attr: Attr {
                ino,
                kind,
                mode,
                nlink,
                uid: self.uid,
                gid: self.gid,
                size,
                blocks: 0,
                rdev: 0,
                atime: now,
                mtime: now,
                ctime: now,
            },
            target,
            file: (kind == FileKind::Regular).then(|| FileData::new(Vec::new())),
            xattrs: None,
            kids: (kind == FileKind::Directory).then_some(0),
        };
        let node = Arc::new(Node::new(ino, st));
        node.add_ref();
        {
            let mut ps = pn.st.wr();
            ps.attr.mtime = now;
            ps.attr.ctime = now;
            if kind == FileKind::Directory {
                ps.attr.nlink += 1;
            }
            if let Some(k) = ps.kids.as_mut() {
                *k += 1;
            }
        }
        let seq = {
            let mut q = sc.q.lk();
            let op = Op::Create {
                parent,
                name: name.into(),
                mode,
                child: ino,
                reserved: Some(ticket),
                what,
            };
            let s = q.push(op, &[&pn, &node], &[&pn, &node]);
            node.set_created_gen(q.gen);
            s
        };
        pn.ns_seq.store(seq, Ordering::Release);
        // A reserved create's number is a packed meta number, so `meta_of` answers for it before meta
        // has seen it. The child records the same sequence its create was queued at, so the gates
        // that read meta through the child (`op_readdir`, `require_empty`, `barrier_if_needed`) commit
        // the create first instead of reading an inode meta does not have, which is `Stale`.
        node.ns_seq.store(seq, Ordering::Release);
        self.nodes.upsert(ino, node.clone());
        self.shrink_nodes(ino);
        self.dents.put(parent, name, Some((ino, kind)), seq);
        self.shrink_dents(parent);
        self.ctr.inodes_net.fetch_add(1, Ordering::Relaxed);
        let a = node.st.rd().report();
        drop(_ns);
        self.maybe_wake(&sc);
        Ok(a)
    }

    pub(crate) fn op_symlink(&self, parent: Ino, name: &[u8], target: &[u8]) -> Result<Attr> {
        if target.is_empty() {
            return Err(Error::InvalidArgument);
        }
        if target.len() > cowfs_meta::SYMLINK_MAX {
            return Err(Error::NameTooLong);
        }
        self.make(parent, name, Create::Symlink(target.into()), 0o777)
    }

    pub(crate) fn op_link(&self, ino: Ino, new_parent: Ino, new_name: &[u8]) -> Result<Attr> {
        if ino == ROOT_INO {
            return Err(Error::PermissionDenied);
        }
        let n = self.live(ino)?;
        {
            let s = n.st.rd();
            if s.attr.kind == FileKind::Directory {
                return Err(Error::PermissionDenied);
            }
            if s.attr.nlink == 0 {
                return Err(Error::NotFound);
            }
        }
        validate_name(new_name)?;
        if new_parent == ROOT_INO {
            return Err(Error::ReadOnly);
        }
        let (sc, qn) = self.dir(new_parent)?;
        if snap_of(ino) != Some(sc.id) {
            return Err(Error::CrossDevice);
        }
        let _ns = sc.ns.lk();
        if qn.st.rd().attr.nlink == 0 {
            return Err(Error::NotFound);
        }
        if self.dent_lookup(&sc, &qn, new_name)?.is_some() {
            return Err(Error::Exists);
        }
        let now = Timestamp::now();
        let kind = {
            let mut s = n.st.wr();
            if s.attr.nlink == 0 {
                return Err(Error::NotFound);
            }
            s.attr.nlink = s.attr.nlink.checked_add(1).ok_or(Error::TooManyLinks)?;
            s.attr.ctime = now;
            s.attr.kind
        };
        {
            let mut qs = qn.st.wr();
            qs.attr.mtime = now;
            qs.attr.ctime = now;
            if let Some(k) = qs.kids.as_mut() {
                *k += 1;
            }
        }
        let seq = {
            let mut q = sc.q.lk();
            let op = Op::Link {
                ino,
                parent: new_parent,
                name: new_name.into(),
            };
            q.push(op, &[&n, &qn], &[&n, &qn])
        };
        qn.ns_seq.store(seq, Ordering::Release);
        n.add_ref();
        self.dents.put(new_parent, new_name, Some((ino, kind)), seq);
        self.shrink_dents(new_parent);
        let a = n.st.rd().report();
        drop(_ns);
        self.maybe_wake(&sc);
        Ok(a)
    }

    pub(crate) fn op_unlink(&self, parent: Ino, name: &[u8]) -> Result<()> {
        if parent == ROOT_INO {
            return Err(Error::ReadOnly);
        }
        let (sc, pn) = self.dir(parent)?;
        validate_name(name)?;
        let _ns = sc.ns.lk();
        let (child, kind) = self.dent_lookup(&sc, &pn, name)?.ok_or(Error::NotFound)?;
        if kind == FileKind::Directory {
            return Err(Error::IsDir);
        }
        let cn = self.node(child)?;
        let last = cn.st.rd().attr.nlink == 1;
        if last && cn.pinned() {
            self.preserve_orphan(&sc, &cn)?;
        }
        let now = Timestamp::now();
        {
            let mut cs = cn.st.wr();
            cs.attr.nlink = cs.attr.nlink.saturating_sub(1);
            cs.attr.ctime = now;
        }
        {
            let mut ps = pn.st.wr();
            ps.attr.mtime = now;
            ps.attr.ctime = now;
            if let Some(k) = ps.kids.as_mut() {
                *k = k.saturating_sub(1);
            }
        }
        if last {
            self.ctr.inodes_net.fetch_sub(1, Ordering::Relaxed);
        }
        let mut q = sc.q.lk();
        if last && q.try_elide(&cn) {
            let seq = q.touch(&pn);
            drop(q);
            self.ctr.elided.fetch_add(1, Ordering::Relaxed);
            // `seq`, not 0: eliding the create says nothing about an earlier queued unlink of the
            // same name, which meta has not seen yet. Marking the entry clean would let a cache
            // drop lose the only record that the name is gone, and the next create would read the
            // name back out of meta and answer Exists.
            self.dents.put(parent, name, None, seq);
            return Ok(());
        }
        let seq = q.push(
            Op::Unlink {
                parent,
                name: name.into(),
            },
            &[&pn, &cn],
            &[&pn, &cn],
        );
        drop(q);
        pn.ns_seq.store(seq, Ordering::Release);
        self.dents.put(parent, name, None, seq);
        drop(_ns);
        self.maybe_wake(&sc);
        Ok(())
    }

    pub(crate) fn op_rmdir(&self, parent: Ino, name: &[u8]) -> Result<()> {
        if parent == ROOT_INO {
            return Err(Error::ReadOnly);
        }
        let (sc, pn) = self.dir(parent)?;
        validate_name(name)?;
        let before = {
            let _ns = sc.ns.lk();
            let (child, kind) = self.dent_lookup(&sc, &pn, name)?.ok_or(Error::NotFound)?;
            if kind != FileKind::Directory {
                return Err(Error::NotDir);
            }
            self.node(child)?
        };
        // `require_empty` may need a commit, which waits for meta's writer lock. That runs without
        // the namespace lock: whatever another thread queues meanwhile is not committed, so the
        // emptiness check that follows still sees everything meta knows.
        self.barrier_if_needed(&sc, &before)?;
        let _ns = sc.ns.lk();
        // the barrier may have applied another thread's queued rename, so the name must be resolved
        // again and the node it now names is the one removed. It is rarely the same node: committing
        // releases a clean directory's virtual alias, so the name is then carried by the meta number.
        let cn = match self.dent_lookup(&sc, &pn, name)? {
            Some((c, FileKind::Directory)) => self.node(c)?,
            _ => return Err(Error::NotFound),
        };
        self.require_empty(&sc, &cn)?;
        if cn.pinned() {
            self.preserve_orphan(&sc, &cn)?;
        }
        let now = Timestamp::now();
        {
            let mut cs = cn.st.wr();
            cs.attr.nlink = 0;
            cs.attr.ctime = now;
        }
        {
            let mut ps = pn.st.wr();
            ps.attr.nlink = ps.attr.nlink.saturating_sub(1);
            ps.attr.mtime = now;
            ps.attr.ctime = now;
            if let Some(k) = ps.kids.as_mut() {
                *k = k.saturating_sub(1);
            }
        }
        self.ctr.inodes_net.fetch_sub(1, Ordering::Relaxed);
        let mut q = sc.q.lk();
        if q.try_elide(&cn) {
            let seq = q.touch(&pn);
            drop(q);
            self.ctr.elided.fetch_add(1, Ordering::Relaxed);
            // the same reason as the unlink above: the queued removal meta has not seen yet
            self.dents.put(parent, name, None, seq);
            return Ok(());
        }
        let seq = q.push(
            Op::Rmdir {
                parent,
                name: name.into(),
            },
            &[&pn, &cn],
            &[&pn, &cn],
        );
        drop(q);
        pn.ns_seq.store(seq, Ordering::Release);
        self.dents.put(parent, name, None, seq);
        drop(_ns);
        self.maybe_wake(&sc);
        Ok(())
    }

    /// `NotEmpty` unless directory `cn` has no entries. Commits pending changes first if the
    /// answer cannot be known from memory.
    /// Commits this directory's pending work if it has any, without any lock of ours.
    fn barrier_if_needed(&self, sc: &SnapCtx, cn: &Node) -> Result<()> {
        let unknown = cn.st.rd().kids.is_none();
        if unknown && cn.ns_seq.load(Ordering::Acquire) > sc.flushed() {
            self.barrier(sc)?;
        }
        Ok(())
    }

    fn require_empty(&self, sc: &SnapCtx, cn: &Arc<Node>) -> Result<()> {
        match cn.st.rd().kids {
            Some(0) => return Ok(()),
            Some(_) => return Err(Error::NotEmpty),
            None => {}
        }
        if cn.ns_seq.load(Ordering::Acquire) > sc.flushed() {
            self.barrier(sc)?;
        }
        let Some(m) = self.meta_of(cn.ino) else {
            return Ok(());
        };
        let r = sc
            .snap
            .readdir(mino(m), 0, 1)
            .map_err(from_meta)
            .map_err(stale)?;
        if r.entries.is_empty() {
            Ok(())
        } else {
            Err(Error::NotEmpty)
        }
    }

    pub(crate) fn op_rename(
        &self,
        parent: Ino,
        name: &[u8],
        new_parent: Ino,
        new_name: &[u8],
        flags: RenameFlags,
    ) -> Result<()> {
        if parent == ROOT_INO || new_parent == ROOT_INO {
            return Err(Error::ReadOnly);
        }
        let (sc, pn) = self.dir(parent)?;
        let (sc2, qn) = self.dir(new_parent)?;
        if sc.id != sc2.id {
            return Err(Error::CrossDevice);
        }
        if qn.st.rd().attr.nlink == 0 {
            return Err(Error::NotFound);
        }
        validate_name(name)?;
        validate_name(new_name)?;
        let mut ns = sc.ns.lk();
        let (mut src, mut skind) = self.dent_lookup(&sc, &pn, name)?.ok_or(Error::NotFound)?;
        let mut dst = self.dent_lookup(&sc, &qn, new_name)?;
        if skind == FileKind::Directory {
            // A directory rename writes to meta directly, and needs everything queued committed
            // first. That commit waits for meta's writer lock, so it runs without the namespace
            // lock; the names are looked up again afterwards, because the commit may have applied
            // another thread's rename of either of them.
            drop(ns);
            self.barrier(&sc)?;
            ns = sc.ns.lk();
            (src, skind) = self.dent_lookup(&sc, &pn, name)?.ok_or(Error::NotFound)?;
            dst = self.dent_lookup(&sc, &qn, new_name)?;
        }
        if let Some((d, _)) = dst {
            if flags.no_replace {
                return Err(Error::Exists);
            }
            if d == src {
                return Ok(());
            }
        }
        let sn = self.node(src)?;
        if skind == FileKind::Directory {
            // the barrier commits queued work and waits for meta's writer lock, so it runs without
            // the namespace lock; the meta write below keeps it, because that write is not queued
            return self.rename_dir(&sc, ns, (&pn, name), (&qn, new_name), &sn, dst);
        }
        if let Some((_, FileKind::Directory)) = dst {
            return Err(Error::IsDir);
        }
        let dn = match dst {
            Some((d, _)) => Some(self.node(d)?),
            None => None,
        };
        let dlast = dn.as_ref().is_some_and(|d| d.st.rd().attr.nlink == 1);
        if let Some(d) = &dn {
            if dlast && d.pinned() {
                self.preserve_orphan(&sc, d)?;
            }
        }
        let now = Timestamp::now();
        self.adjust_kids(&pn, &qn, dst.is_some());
        for n in [&pn, &qn] {
            let mut s = n.st.wr();
            s.attr.mtime = now;
            s.attr.ctime = now;
        }
        sn.st.wr().attr.ctime = now;
        if let Some(d) = &dn {
            let mut s = d.st.wr();
            s.attr.nlink = s.attr.nlink.saturating_sub(1);
            s.attr.ctime = now;
        }
        if dlast {
            self.ctr.inodes_net.fetch_sub(1, Ordering::Relaxed);
        }
        let seq = {
            let mut q = sc.q.lk();
            let op = Op::Rename {
                from: parent,
                from_name: name.into(),
                to: new_parent,
                to_name: new_name.into(),
            };
            let mut structural: Vec<&Node> = vec![&pn, &qn, &sn];
            let mut touch: Vec<&Node> = vec![&pn, &qn, &sn];
            if let Some(d) = &dn {
                structural.push(d);
                touch.push(d);
            }
            q.push(op, &structural, &touch)
        };
        pn.ns_seq.store(seq, Ordering::Release);
        qn.ns_seq.store(seq, Ordering::Release);
        self.dents.put(parent, name, None, seq);
        self.dents
            .put(new_parent, new_name, Some((src, skind)), seq);
        self.shrink_dents(new_parent);
        drop(ns);
        self.maybe_wake(&sc);
        Ok(())
    }

    /// Entry counts after moving one entry from `from` to `to`, replacing one if `replaced`.
    fn adjust_kids(&self, from: &Node, to: &Node, replaced: bool) {
        if from.ino == to.ino {
            if replaced {
                if let Some(k) = from.st.wr().kids.as_mut() {
                    *k = k.saturating_sub(1);
                }
            }
            return;
        }
        if let Some(k) = from.st.wr().kids.as_mut() {
            *k = k.saturating_sub(1);
        }
        if let Some(k) = to.st.wr().kids.as_mut() {
            if !replaced {
                *k += 1;
            }
        }
    }

    /// Directory rename: a barrier, then the operation runs against meta directly.
    fn rename_dir(
        &self,
        sc: &Arc<SnapCtx>,
        _ns: std::sync::MutexGuard<'_, ()>,
        from: (&Arc<Node>, &[u8]),
        to: (&Arc<Node>, &[u8]),
        sn: &Arc<Node>,
        dst: Target,
    ) -> Result<()> {
        let (pn, name) = from;
        let (qn, new_name) = to;
        self.barrier(sc)?;
        // The barrier commits, which may evict the nodes behind these names from the node table.
        // Re-resolve both names to the numbers the mount uses now.
        let re = |ino: Ino| match self.meta_of(ino) {
            Some(m) => self.canon(sc.id, m).unwrap_or(ino),
            None => ino,
        };
        let src = re(sn.ino);
        let dst = dst.map(|(d, k)| (re(d), k));
        let mut cur = qn.ino;
        loop {
            if cur == src {
                return Err(Error::InvalidArgument);
            }
            let m = self.meta_of(cur).ok_or(Error::Stale)?;
            if m == cowfs_meta::ROOT_INO.0 {
                break;
            }
            let up = sc
                .snap
                .lookup(mino(m), b"..")
                .map_err(from_meta)
                .map_err(stale)?;
            cur = self.canon(sc.id, up.ino.0)?;
        }
        let dn = match dst {
            Some((d, dk)) => match dk {
                FileKind::Directory => {
                    let dn = self.node(d)?;
                    self.require_empty(sc, &dn)?;
                    Some(dn)
                }
                _ => return Err(Error::NotDir),
            },
            None => None,
        };
        if let Some(d) = &dn {
            if d.pinned() {
                self.preserve_orphan(sc, d)?;
            }
        }
        let pm = self.meta_of(pn.ino).ok_or(Error::Stale)?;
        let qm = self.meta_of(qn.ino).ok_or(Error::Stale)?;
        // This write is the one place core talks to meta directly instead of queueing, so it has to
        // be atomic with the queue: releasing the namespace lock here lets a queued rename of an
        // entry of this directory commit first, and its dentry update then describes a name that no
        // longer exists (the suite's `concurrent_rename_unlink_lookup` catches exactly that). The
        // write is `Ack::Applied`, so it does not fsync; it only waits for meta's writer lock,
        // which the two other lock-across-commit sites no longer hold.
        sc.snap
            .rename(mino(pm), name, mino(qm), new_name)
            .map_err(from_meta)?;
        if let Some(d) = &dn {
            let mut s = d.st.wr();
            s.attr.nlink = 0;
            self.ctr.inodes_net.fetch_sub(1, Ordering::Relaxed);
        }
        self.adjust_kids(pn, qn, dst.is_some());
        for n in [pn, qn] {
            self.refresh_dir_attr(sc, n)?;
        }
        // The moved directory's number comes from meta now: the node we were handed may be keyed by
        // a virtual number the barrier above released, which nothing can name any more.
        let moved = sc
            .snap
            .lookup(mino(qm), new_name)
            .map_err(from_meta)
            .map_err(stale)?;
        let moved_ino = pack(sc.id, moved.ino.0)?;
        self.seed_node(moved_ino, &moved);
        let moved_node = self.node(moved_ino)?;
        self.refresh_dir_attr(sc, &moved_node)?;
        self.dents.put(pn.ino, name, None, 0);
        self.dents
            .put(qn.ino, new_name, Some((moved_ino, FileKind::Directory)), 0);
        if let Some(d) = &dn {
            self.try_reclaim(d);
        }
        Ok(())
    }

    /// Copies meta's current attributes of a directory into the cached node.
    fn refresh_dir_attr(&self, sc: &SnapCtx, n: &Node) -> Result<()> {
        let m = self.meta_of(n.ino).ok_or(Error::Stale)?;
        let a = sc.snap.getattr(mino(m)).map_err(from_meta).map_err(stale)?;
        let fresh = self.attr_from_meta(n.ino, &a);
        n.st.wr().attr = fresh;
        Ok(())
    }
}
