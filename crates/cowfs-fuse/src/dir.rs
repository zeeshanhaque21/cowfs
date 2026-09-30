//! Directory listing for FUSE `readdir`, independent of the kernel reply types.
//!
//! FUSE offsets are `Vfs` cookies shifted by two: offset 0 starts the listing, offset 1 is
//! after `.`, offset 2 is after `..` (equal to `Vfs` cookie 0), and offset `n >= 2` resumes
//! after `Vfs` cookie `n - 2`. Nothing depends on inode numbers, and the adapter keeps no
//! per-directory state, so a listing survives entries coming and going.

use cowfs_vfs::{Error, FileKind, Ino, Result, Vfs};

/// Entries requested from the `Vfs` per call. The reply buffer decides how many are used.
pub const BATCH: usize = 256;

const OFFSET_SHIFT: u64 = 2;

/// Where a `readdir` request resumes.
#[derive(Debug, PartialEq, Eq)]
pub enum Start {
    /// From the beginning: emit `.` first.
    Dot,
    /// After `.`: emit `..` first.
    DotDot,
    /// After this `Vfs` cookie.
    After(u64),
}

/// Decodes a FUSE `readdir` offset.
pub fn start_position(offset: i64) -> Result<Start> {
    match u64::try_from(offset) {
        Ok(0) => Ok(Start::Dot),
        Ok(1) => Ok(Start::DotDot),
        Ok(n) => Ok(Start::After(n - OFFSET_SHIFT)),
        Err(_) => Err(Error::InvalidArgument),
    }
}

/// The FUSE offset that resumes after an entry with this `Vfs` cookie, if it is representable.
pub fn offset_for_cookie(cookie: u64) -> Option<i64> {
    i64::try_from(cookie.checked_add(OFFSET_SHIFT)?).ok()
}

/// Receives directory entries until it reports that its buffer is full.
pub trait DirSink {
    /// Adds an entry. Returns true when the buffer is full and the entry was not added.
    fn add(&mut self, ino: Ino, offset: i64, kind: FileKind, name: &[u8]) -> bool;
}

/// Lists `dir` from FUSE `offset` into `sink`, synthesizing `.` and `..`.
/// Both synthesized entries carry the directory's own inode number, because the `Vfs`
/// has no parent lookup and the kernel does not use the value.
pub fn fill(vfs: &dyn Vfs, dir: Ino, offset: i64, sink: &mut dyn DirSink) -> Result<()> {
    let start = start_position(offset)?;
    if start == Start::Dot && sink.add(dir, 1, FileKind::Directory, b".") {
        return Ok(());
    }
    if matches!(start, Start::Dot | Start::DotDot) && sink.add(dir, 2, FileKind::Directory, b"..") {
        return Ok(());
    }
    let mut cookie = match start {
        Start::After(c) => c,
        _ => 0,
    };
    loop {
        let batch = vfs.readdir(dir, cookie, BATCH)?;
        for e in &batch.entries {
            if e.cookie <= cookie {
                return Err(Error::Io("readdir cookie did not advance".into()));
            }
            let off = offset_for_cookie(e.cookie).ok_or(Error::Range)?;
            if sink.add(e.ino, off, e.kind, &e.name) {
                return Ok(());
            }
            cookie = e.cookie;
        }
        if batch.eof || batch.entries.is_empty() {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
        loop {
            let mut sink = Collect {
                cap: page,
                got: vec![],
            };
            fill(vfs, dir, offset, &mut sink).unwrap();
            let Some(last) = sink.got.last() else { break };
            offset = last.0;
            names.extend(sink.got.into_iter().map(|(_, n)| n));
        }
        names
    }

    #[test]
    fn offsets_are_cookies_shifted_by_two() {
        assert_eq!(start_position(0), Ok(Start::Dot));
        assert_eq!(start_position(1), Ok(Start::DotDot));
        assert_eq!(start_position(2), Ok(Start::After(0)));
        assert_eq!(start_position(9), Ok(Start::After(7)));
        assert_eq!(start_position(-1), Err(Error::InvalidArgument));
        assert_eq!(offset_for_cookie(0), Some(2));
        assert_eq!(offset_for_cookie(7), Some(9));
        assert_eq!(offset_for_cookie(u64::MAX), None);
        assert_eq!(offset_for_cookie(i64::MAX as u64), None);
        for c in [0, 1, 5, 1 << 40] {
            let off = offset_for_cookie(c).unwrap();
            assert_eq!(start_position(off), Ok(Start::After(c)));
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
        fill(&vfs, 1, 0, &mut sink).unwrap();
        assert!(sink.got.is_empty());
        let mut sink = Collect {
            cap: 1,
            got: vec![],
        };
        fill(&vfs, 1, 0, &mut sink).unwrap();
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
        loop {
            let mut sink = Collect {
                cap: 10,
                got: vec![],
            };
            fill(&vfs, 1, offset, &mut sink).unwrap();
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
    fn rejects_bad_offsets_and_unrepresentable_cookies() {
        struct Bad;
        impl DirSink for Bad {
            fn add(&mut self, _: Ino, _: i64, _: FileKind, _: &[u8]) -> bool {
                false
            }
        }
        let vfs = MemVfs::new();
        vfs.create(1, b"a", 0o644).unwrap();
        assert!(fill(&vfs, 1, 2, &mut Bad).is_ok());
        assert_eq!(fill(&vfs, 1, -3, &mut Bad), Err(Error::InvalidArgument));
        let broken = MemVfs::with_fault(Fault::DotEntries);
        assert_eq!(fill(&broken, 1, 2, &mut Bad), Err(Error::Range));
    }
}
