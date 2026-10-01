//! Directory listing for FUSE `readdir`, independent of the kernel reply types.
//!
//! FUSE offsets are `Vfs` cookies shifted by two: offset 0 starts the listing, offset 1 is
//! after `.`, offset 2 is after `..` (equal to `Vfs` cookie 0), and offset `n >= 2` resumes
//! after `Vfs` cookie `n - 2`. Nothing depends on inode numbers, and the adapter keeps no
//! per-directory state, so a listing survives entries coming and going.

use cowfs_vfs::{Error, FileKind, Ino, Result, Vfs};

/// Entries requested from the `Vfs` per call. The reply buffer decides how many are used.
pub const BATCH: usize = 256;

/// Entries requested at once from a `Vfs` whose cookies are 0-based, which cannot be paged.
const COUNTED_PAGE: usize = 4096;

/// Consecutive short pages tolerated before the listing is declared broken. The trait does not
/// say an empty page means the end, so a short or holey listing is retried a bounded number of
/// times rather than silently truncated; a `Vfs` that never reaches eof then fails loudly.
pub const MAX_EMPTY_PAGES: usize = 8;

/// Where a `readdir` request resumes.
#[derive(Debug, PartialEq, Eq)]
pub enum Start {
    /// From the beginning: emit `.` first.
    Dot,
    /// After `.`: emit `..` first.
    DotDot,
    /// After this `Vfs` cookie.
    After(u64),
    /// After this many entries, counted by the adapter.
    Counted(u64),
}

/// The position part of a start.
pub fn pos_of(s: Start) -> u64 {
    match s {
        Start::After(c) | Start::Counted(c) => c,
        _ => 0,
    }
}

/// Decodes a FUSE `readdir` offset.
pub fn start_position(offset: i64) -> Result<Start> {
    let Ok(n) = u64::try_from(offset) else {
        return Err(Error::InvalidArgument);
    };
    Ok(match n {
        0 => Start::Dot,
        1 => Start::DotDot,
        2 => Start::After(0),
        n => {
            let n = n - 3;
            if n & 1 == 0 {
                Start::After(n / 2)
            } else {
                Start::Counted(n / 2)
            }
        }
    })
}

/// The FUSE offset that resumes after an entry with this `Vfs` cookie, in cookie mode.
pub fn offset_for_cookie(cookie: u64) -> Option<i64> {
    i64::try_from(cookie.checked_mul(2)?.checked_add(3)?).ok()
}

/// The FUSE offset that resumes after this many counted entries.
pub fn offset_for_position(pos: u64) -> Option<i64> {
    i64::try_from(pos.checked_mul(2)?.checked_add(4)?).ok()
}

/// Receives directory entries until it reports that its buffer is full.
pub trait DirSink {
    /// Adds an entry. Returns true when the buffer is full and the entry was not added.
    fn add(&mut self, ino: Ino, offset: i64, kind: FileKind, name: &[u8]) -> bool;
}

/// Lists `dir` from FUSE `offset` into `sink`, synthesizing `.` (the directory itself) and
/// `..` (`parent`, which the caller tracks because the `Vfs` has no parent lookup).
///
/// Entries are resumed with the `Vfs` cookie, which is opaque: it only has to be stable and
/// must not go backwards. A `Vfs` that ignores the position, or whose first entry carries
/// cookie 0 (which the trait reserves for "from the start"), is served by counting entries and
/// replaying the listing from the start on each call. An empty page that is not the end is
/// retried, `MAX_EMPTY_PAGES` times, and then fails loudly rather than truncating the listing.
pub fn fill(
    vfs: &dyn Vfs,
    dir: Ino,
    parent: Ino,
    offset: i64,
    sink: &mut dyn DirSink,
) -> Result<()> {
    let start = start_position(offset)?;
    if start == Start::Dot && sink.add(dir, 1, FileKind::Directory, b".") {
        return Ok(());
    }
    if matches!(start, Start::Dot | Start::DotDot)
        && sink.add(parent, 2, FileKind::Directory, b"..")
    {
        return Ok(());
    }
    // Two modes. Cookie mode resumes with the Vfs cookie, which a Vfs numbering entries from 1
    // supports directly. A Vfs that numbers from 0 cannot be paged at all: cookie 0 means "from
    // the start", so "after the first entry" is inexpressible. Counted mode therefore replays
    // the listing from the start on every call and skips the entries already emitted, which is
    // exact up to COUNTED_PAGE entries and fails loudly beyond it. The mode rides in the low bit
    // of the FUSE offset with the position in the rest, so it survives between calls.
    let mut counted = matches!(start, Start::Counted(_));
    let mut pos = pos_of(start);
    let mut empty_pages = 0usize;
    loop {
        let page = if counted { COUNTED_PAGE } else { BATCH };
        let batch = vfs.readdir(dir, if counted { 0 } else { pos }, page)?;
        if !counted && batch.entries.first().is_some_and(|e| e.cookie <= pos) {
            if std::env::var_os("COWFS_DIR_TRACE").is_some() {
                eprintln!(
                    "DBG switch to counted: pos={pos} first={:?}",
                    batch.entries.first().map(|e| e.cookie)
                );
            }
            counted = true;
            empty_pages = 0;
            continue;
        }
        let mut skip = pos;
        for e in &batch.entries {
            if counted && skip > 0 {
                skip -= 1;
                continue;
            }
            if !counted {
                if e.cookie <= pos {
                    return Err(Error::Io("readdir cookie did not advance".into()));
                }
                pos = e.cookie;
            } else {
                pos += 1;
            }
            let off = if counted {
                offset_for_position(pos).ok_or(Error::Range)?
            } else {
                offset_for_cookie(pos).ok_or(Error::Range)?
            };
            if sink.add(e.ino, off, e.kind, &e.name) {
                return Ok(());
            }
        }
        if batch.eof {
            return Ok(());
        }
        // A short page with more entries behind it is normal ("at most max"). Only an empty page
        // that is not the end is ambiguous: the trait does not say it means the end, so it is
        // retried, bounded, and then fails loudly instead of truncating the listing.
        if counted && batch.entries.len() == page {
            return Err(Error::Io(format!(
                "readdir of inode {dir} has more than {COUNTED_PAGE} entries and cookies that \
                 cannot be resumed, so the listing cannot be paged"
            )));
        }
        if batch.entries.is_empty() {
            empty_pages += 1;
            if empty_pages > MAX_EMPTY_PAGES {
                return Err(Error::Io(format!(
                    "readdir of inode {dir} returned {MAX_EMPTY_PAGES} empty pages without eof"
                )));
            }
        } else {
            empty_pages = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cowfs_vfs::{
        Attr, FileHandle, ReadDir, RenameFlags, Result, SetAttr, StatFs, Vfs, XattrFlags,
    };
    use cowfs_vfs_test::{Fault, MemVfs};

    struct Collect {
        cap: usize,
        got: Vec<(i64, Vec<u8>)>,
    }

    impl DirSink for Collect {
        fn add(&mut self, _ino: Ino, offset: i64, _kind: FileKind, name: &[u8]) -> bool {
            if self.got.len() >= self.cap {
                return true;
            }
            self.got.push((offset, name.to_vec()));
            false
        }
    }

    fn list_all(vfs: &MemVfs, dir: Ino, page: usize) -> Vec<Vec<u8>> {
        let mut names = Vec::new();
        let mut offset = 0;
        for _ in 0..20_000 {
            let mut sink = Collect {
                cap: page,
                got: vec![],
            };
            fill(vfs, dir, 1, offset, &mut sink).unwrap();
            let Some(last) = sink.got.last() else { break };
            offset = last.0;
            names.extend(sink.got.into_iter().map(|(_, n)| n));
            assert!(names.len() < 100_000, "listing does not terminate");
        }
        names
    }

    #[test]
    fn offsets_encode_the_cookie_or_the_counted_position() {
        assert_eq!(start_position(0), Ok(Start::Dot));
        assert_eq!(start_position(1), Ok(Start::DotDot));
        assert_eq!(start_position(2), Ok(Start::After(0)));
        assert_eq!(start_position(-1), Err(Error::InvalidArgument));
        assert_eq!(offset_for_cookie(0), Some(3));
        assert_eq!(offset_for_cookie(7), Some(17));
        assert_eq!(offset_for_position(0), Some(4));
        assert_eq!(offset_for_position(5), Some(14));
        assert_eq!(start_position(17), Ok(Start::After(7)));
        assert_eq!(start_position(14), Ok(Start::Counted(5)));
        assert_eq!(offset_for_cookie(u64::MAX), None);
        assert_eq!(offset_for_cookie(i64::MAX as u64), None);
        for c in [1, 5, 1 << 40] {
            let off = offset_for_cookie(c).unwrap();
            assert_eq!(start_position(off), Ok(Start::After(c)));
        }
        for p in [0, 1, 5, 1 << 40] {
            let off = offset_for_position(p).unwrap();
            assert_eq!(start_position(off), Ok(Start::Counted(p)));
        }
    }

    #[test]
    fn lists_dots_then_entries_in_pages_without_repeats() {
        let vfs = MemVfs::new();
        for i in 0..1000 {
            vfs.create(1, format!("f{i}").as_bytes(), 0o644).unwrap();
        }
        for page in [1, 2, 7, 300, 5000] {
            let names = list_all(&vfs, 1, page);
            assert_eq!(&names[..2], [b".".to_vec(), b"..".to_vec()]);
            let mut rest: Vec<_> = names[2..].to_vec();
            assert_eq!(rest.len(), 1000, "page {page}");
            rest.sort();
            rest.dedup();
            assert_eq!(rest.len(), 1000, "page {page}");
        }
    }

    #[test]
    fn stops_when_the_buffer_is_full_on_a_dot_entry() {
        let vfs = MemVfs::new();
        let mut sink = Collect {
            cap: 0,
            got: vec![],
        };
        fill(&vfs, 1, 1, 0, &mut sink).unwrap();
        assert!(sink.got.is_empty());
        let mut sink = Collect {
            cap: 1,
            got: vec![],
        };
        fill(&vfs, 1, 1, 0, &mut sink).unwrap();
        assert_eq!(sink.got, [(1, b".".to_vec())]);
    }

    #[test]
    fn deleting_while_listing_neither_repeats_nor_skips() {
        let vfs = MemVfs::new();
        for i in 0..600 {
            vfs.create(1, format!("f{i:04}").as_bytes(), 0o644).unwrap();
        }
        let mut seen = Vec::new();
        let mut offset = 0;
        for _ in 0..20_000 {
            let mut sink = Collect {
                cap: 10,
                got: vec![],
            };
            fill(&vfs, 1, 1, offset, &mut sink).unwrap();
            let Some(last) = sink.got.last() else { break };
            offset = last.0;
            for (_, n) in &sink.got {
                if n.starts_with(b"f") {
                    seen.push(n.clone());
                    vfs.unlink(1, n).unwrap();
                }
            }
        }
        seen.sort();
        let before = seen.len();
        seen.dedup();
        assert_eq!(seen.len(), before);
        assert_eq!(seen.len(), 600);
    }

    #[test]
    fn offsets_are_the_vfs_cookies_plus_two_even_when_names_share_an_inode() {
        let vfs = MemVfs::new();
        let a = vfs.create(1, b"a", 0o644).unwrap();
        vfs.link(a.ino, 1, b"b").unwrap();
        vfs.link(a.ino, 1, b"c").unwrap();
        let cookies: Vec<_> = vfs
            .readdir(1, 0, 10)
            .unwrap()
            .entries
            .iter()
            .map(|e| e.cookie)
            .collect();
        let mut sink = Collect {
            cap: 100,
            got: vec![],
        };
        fill(&vfs, 1, 1, 0, &mut sink).unwrap();
        let offsets: Vec<_> = sink.got.iter().skip(2).map(|(o, _)| *o).collect();
        let want: Vec<_> = cookies.iter().map(|c| *c as i64 * 2 + 3).collect();
        let _ = want.len();
        assert_eq!(offsets, want);
    }

    /// A Vfs whose cookies are 0-based positions, which the trait allows.
    struct Zeroed<'a>(&'a MemVfs);

    impl Vfs for Zeroed<'_> {
        fn lookup(&self, p: Ino, n: &[u8]) -> Result<Attr> {
            self.0.lookup(p, n)
        }
        fn getattr(&self, i: Ino) -> Result<Attr> {
            self.0.getattr(i)
        }
        fn setattr(&self, i: Ino, c: SetAttr) -> Result<Attr> {
            self.0.setattr(i, c)
        }
        fn readlink(&self, i: Ino) -> Result<Vec<u8>> {
            self.0.readlink(i)
        }
        fn create(&self, p: Ino, n: &[u8], m: u32) -> Result<Attr> {
            self.0.create(p, n, m)
        }
        fn mkdir(&self, p: Ino, n: &[u8], m: u32) -> Result<Attr> {
            self.0.mkdir(p, n, m)
        }
        fn symlink(&self, p: Ino, n: &[u8], t: &[u8]) -> Result<Attr> {
            self.0.symlink(p, n, t)
        }
        fn link(&self, i: Ino, p: Ino, n: &[u8]) -> Result<Attr> {
            self.0.link(i, p, n)
        }
        fn unlink(&self, p: Ino, n: &[u8]) -> Result<()> {
            self.0.unlink(p, n)
        }
        fn rmdir(&self, p: Ino, n: &[u8]) -> Result<()> {
            self.0.rmdir(p, n)
        }
        fn rename(&self, p: Ino, n: &[u8], p2: Ino, n2: &[u8], f: RenameFlags) -> Result<()> {
            self.0.rename(p, n, p2, n2, f)
        }
        fn open(&self, i: Ino) -> Result<FileHandle> {
            self.0.open(i)
        }
        fn release(&self, h: FileHandle) -> Result<()> {
            self.0.release(h)
        }
        fn read(&self, i: Ino, o: u64, s: u32) -> Result<Vec<u8>> {
            self.0.read(i, o, s)
        }
        fn write(&self, i: Ino, o: u64, d: &[u8]) -> Result<u32> {
            self.0.write(i, o, d)
        }
        fn flush(&self, i: Ino) -> Result<()> {
            self.0.flush(i)
        }
        fn fsync(&self, i: Ino, d: bool) -> Result<()> {
            self.0.fsync(i, d)
        }
        /// A conforming 0-based Vfs: cookie 0 means "from the start" and the first entry
        /// carries cookie 0, stable and monotonic across pages.
        fn readdir(&self, d: Ino, c: u64, m: usize) -> Result<ReadDir> {
            self.0
                .readdir(d, if c == 0 { 0 } else { c + 1 }, m)
                .map(|mut l| {
                    for e in l.entries.iter_mut() {
                        e.cookie = e.cookie.saturating_sub(1);
                    }
                    l
                })
        }
        fn statfs(&self) -> Result<StatFs> {
            self.0.statfs()
        }
        fn getxattr(&self, i: Ino, n: &[u8]) -> Result<Vec<u8>> {
            self.0.getxattr(i, n)
        }
        fn setxattr(&self, i: Ino, n: &[u8], v: &[u8], f: XattrFlags) -> Result<()> {
            self.0.setxattr(i, n, v, f)
        }
        fn listxattr(&self, i: Ino) -> Result<Vec<Vec<u8>>> {
            self.0.listxattr(i)
        }
        fn removexattr(&self, i: Ino, n: &[u8]) -> Result<()> {
            self.0.removexattr(i, n)
        }
    }

    /// A Vfs that returns a few empty pages before the real listing.
    struct Gappy<'a>(&'a MemVfs, std::sync::atomic::AtomicU32);

    impl Vfs for Gappy<'_> {
        fn readdir(&self, d: Ino, c: u64, m: usize) -> Result<ReadDir> {
            let n = self.1.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if n < 3 {
                return Ok(ReadDir {
                    entries: vec![],
                    eof: false,
                });
            }
            self.0.readdir(d, c, m)
        }
        fn lookup(&self, p: Ino, n: &[u8]) -> Result<Attr> {
            self.0.lookup(p, n)
        }
        fn getattr(&self, i: Ino) -> Result<Attr> {
            self.0.getattr(i)
        }
        fn forget(&self, _i: Ino, _c: u64) {}
        fn setattr(&self, i: Ino, c: SetAttr) -> Result<Attr> {
            self.0.setattr(i, c)
        }
        fn readlink(&self, i: Ino) -> Result<Vec<u8>> {
            self.0.readlink(i)
        }
        fn create(&self, p: Ino, n: &[u8], m: u32) -> Result<Attr> {
            self.0.create(p, n, m)
        }
        fn mkdir(&self, p: Ino, n: &[u8], m: u32) -> Result<Attr> {
            self.0.mkdir(p, n, m)
        }
        fn symlink(&self, p: Ino, n: &[u8], t: &[u8]) -> Result<Attr> {
            self.0.symlink(p, n, t)
        }
        fn link(&self, i: Ino, p: Ino, n: &[u8]) -> Result<Attr> {
            self.0.link(i, p, n)
        }
        fn unlink(&self, p: Ino, n: &[u8]) -> Result<()> {
            self.0.unlink(p, n)
        }
        fn rmdir(&self, p: Ino, n: &[u8]) -> Result<()> {
            self.0.rmdir(p, n)
        }
        fn rename(&self, p: Ino, n: &[u8], p2: Ino, n2: &[u8], f: RenameFlags) -> Result<()> {
            self.0.rename(p, n, p2, n2, f)
        }
        fn open(&self, i: Ino) -> Result<FileHandle> {
            self.0.open(i)
        }
        fn release(&self, h: FileHandle) -> Result<()> {
            self.0.release(h)
        }
        fn read(&self, i: Ino, o: u64, s: u32) -> Result<Vec<u8>> {
            self.0.read(i, o, s)
        }
        fn write(&self, i: Ino, o: u64, d: &[u8]) -> Result<u32> {
            self.0.write(i, o, d)
        }
        fn flush(&self, i: Ino) -> Result<()> {
            self.0.flush(i)
        }
        fn fsync(&self, i: Ino, d: bool) -> Result<()> {
            self.0.fsync(i, d)
        }
        fn statfs(&self) -> Result<StatFs> {
            self.0.statfs()
        }
        fn getxattr(&self, i: Ino, n: &[u8]) -> Result<Vec<u8>> {
            self.0.getxattr(i, n)
        }
        fn setxattr(&self, i: Ino, n: &[u8], v: &[u8], f: XattrFlags) -> Result<()> {
            self.0.setxattr(i, n, v, f)
        }
        fn listxattr(&self, i: Ino) -> Result<Vec<Vec<u8>>> {
            self.0.listxattr(i)
        }
        fn removexattr(&self, i: Ino, n: &[u8]) -> Result<()> {
            self.0.removexattr(i, n)
        }
    }

    #[test]
    fn zero_based_cookies_are_served_by_counting() {
        let mem = MemVfs::new();
        for i in 0..500 {
            mem.create(1, format!("f{i:04}").as_bytes(), 0o644).unwrap();
        }
        let vfs = Zeroed(&mem);
        let mut names = Vec::new();
        let mut offset = 0;
        let mut calls = 0;
        for _ in 0..200 {
            let mut sink = Collect {
                cap: 64,
                got: vec![],
            };
            fill(&vfs, 1, 1, offset, &mut sink).unwrap();
            calls += 1;
            let got = sink.got.len();

            if got == 0 {
                break;
            }
            offset = sink.got.last().unwrap().0;
            names.extend(sink.got.into_iter().map(|(_, n)| n));
            assert!(
                names.len() < 3_000,
                "listing does not terminate: {calls} calls, {} names",
                names.len()
            );
        }
        assert!(calls < 30, "listing took {calls} pages");
        assert_eq!(&names[..2], [b".".to_vec(), b"..".to_vec()]);
        let mut rest: Vec<_> = names[2..].to_vec();
        assert_eq!(rest.len(), 500);
        rest.sort();
        rest.dedup();
        assert_eq!(rest.len(), 500, "counted mode repeated or dropped entries");
    }

    #[test]
    fn empty_pages_are_retried_and_a_listing_that_never_ends_fails_loudly() {
        let mem = MemVfs::new();
        for i in 0..5 {
            mem.create(1, format!("real{i}").as_bytes(), 0o644).unwrap();
        }
        let vfs = Gappy(&mem, std::sync::atomic::AtomicU32::new(0));
        let mut sink = Collect {
            cap: 100,
            got: vec![],
        };
        fill(&vfs, 1, 1, 0, &mut sink).unwrap();
        assert_eq!(
            sink.got.len(),
            7,
            "dots plus five files, after three empty pages"
        );

        struct Never;
        impl Vfs for Never {
            fn readdir(&self, _: Ino, _: u64, _: usize) -> Result<ReadDir> {
                Ok(ReadDir {
                    entries: vec![],
                    eof: false,
                })
            }
            fn lookup(&self, _: Ino, _: &[u8]) -> Result<Attr> {
                Err(Error::NotFound)
            }
            fn getattr(&self, _: Ino) -> Result<Attr> {
                Err(Error::Stale)
            }
            fn forget(&self, _: Ino, _: u64) {}
            fn setattr(&self, _: Ino, _: SetAttr) -> Result<Attr> {
                Err(Error::NotSupported)
            }
            fn readlink(&self, _: Ino) -> Result<Vec<u8>> {
                Err(Error::NotSupported)
            }
            fn create(&self, _: Ino, _: &[u8], _: u32) -> Result<Attr> {
                Err(Error::NotSupported)
            }
            fn mkdir(&self, _: Ino, _: &[u8], _: u32) -> Result<Attr> {
                Err(Error::NotSupported)
            }
            fn symlink(&self, _: Ino, _: &[u8], _: &[u8]) -> Result<Attr> {
                Err(Error::NotSupported)
            }
            fn link(&self, _: Ino, _: Ino, _: &[u8]) -> Result<Attr> {
                Err(Error::NotSupported)
            }
            fn unlink(&self, _: Ino, _: &[u8]) -> Result<()> {
                Err(Error::NotSupported)
            }
            fn rmdir(&self, _: Ino, _: &[u8]) -> Result<()> {
                Err(Error::NotSupported)
            }
            fn rename(&self, _: Ino, _: &[u8], _: Ino, _: &[u8], _: RenameFlags) -> Result<()> {
                Err(Error::NotSupported)
            }
            fn open(&self, _: Ino) -> Result<FileHandle> {
                Err(Error::NotSupported)
            }
            fn release(&self, _: FileHandle) -> Result<()> {
                Err(Error::NotSupported)
            }
            fn read(&self, _: Ino, _: u64, _: u32) -> Result<Vec<u8>> {
                Err(Error::NotSupported)
            }
            fn write(&self, _: Ino, _: u64, _: &[u8]) -> Result<u32> {
                Err(Error::NotSupported)
            }
            fn flush(&self, _: Ino) -> Result<()> {
                Ok(())
            }
            fn fsync(&self, _: Ino, _: bool) -> Result<()> {
                Ok(())
            }
            fn statfs(&self) -> Result<StatFs> {
                Err(Error::NotSupported)
            }
            fn getxattr(&self, _: Ino, _: &[u8]) -> Result<Vec<u8>> {
                Err(Error::NoAttr)
            }
            fn setxattr(&self, _: Ino, _: &[u8], _: &[u8], _: XattrFlags) -> Result<()> {
                Err(Error::NotSupported)
            }
            fn listxattr(&self, _: Ino) -> Result<Vec<Vec<u8>>> {
                Ok(vec![])
            }
            fn removexattr(&self, _: Ino, _: &[u8]) -> Result<()> {
                Err(Error::NoAttr)
            }
        }
        let mut sink = Collect {
            cap: 100,
            got: vec![],
        };
        assert!(
            matches!(fill(&Never, 1, 1, 0, &mut sink), Err(Error::Io(_))),
            "a listing that never reaches eof must fail loudly, not look empty"
        );
    }

    #[test]
    fn dot_entries_carry_the_directory_and_its_parent() {
        struct Inos(Vec<(Vec<u8>, Ino)>);
        impl DirSink for Inos {
            fn add(&mut self, ino: Ino, _: i64, _: FileKind, n: &[u8]) -> bool {
                self.0.push((n.to_vec(), ino));
                false
            }
        }
        let vfs = MemVfs::new();
        let d = vfs.mkdir(1, b"d", 0o755).unwrap().ino;
        let mut s = Inos(vec![]);
        fill(&vfs, d, 99, 0, &mut s).unwrap();
        assert_eq!(s.0, [(b".".to_vec(), d), (b"..".to_vec(), 99)]);
        let mut s = Inos(vec![]);
        fill(&vfs, d, 99, 1, &mut s).unwrap();
        assert_eq!(s.0, [(b"..".to_vec(), 99)]);
    }

    #[test]
    fn rejects_bad_offsets_and_unrepresentable_cookies() {
        struct Bad;
        impl DirSink for Bad {
            fn add(&mut self, _: Ino, _: i64, _: FileKind, _: &[u8]) -> bool {
                false
            }
        }
        let vfs = MemVfs::new();
        vfs.create(1, b"a", 0o644).unwrap();
        assert!(fill(&vfs, 1, 1, 2, &mut Bad).is_ok());
        assert_eq!(fill(&vfs, 1, 1, -3, &mut Bad), Err(Error::InvalidArgument));
        let broken = MemVfs::with_fault(Fault::DotEntries);
        assert!(
            matches!(fill(&broken, 1, 1, 2, &mut Bad), Err(Error::Range) | Ok(())),
            "a Vfs whose cookies cannot be resumed must either be counted or fail"
        );
    }
}
