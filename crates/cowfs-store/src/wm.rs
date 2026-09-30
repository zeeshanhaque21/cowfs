//! Durable watermark: the last pack and length that a completed `sync` made durable.
//!
//! Two 32-byte slots are written alternately, each with its own CRC, so a torn write leaves the
//! older slot intact. A lower watermark than the truth is always safe. Writers must fsync the pack
//! data first and the watermark second.
//!
//! Each slot also records `base`, the lowest pack id that must exist, so a deleted pack is noticed.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use crate::fsio::Io;

pub(crate) const FILE_NAME: &str = "SYNCED";
const SLOT: usize = 32;

/// Everything before `len` in pack `pack`, and every lower-numbered pack, is durable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Mark {
    pub pack: u32,
    pub len: u64,
}

fn encode(seq: u64, mark: Mark, base: u32) -> [u8; SLOT] {
    let mut b = [0u8; SLOT];
    b[..8].copy_from_slice(&seq.to_le_bytes());
    b[8..12].copy_from_slice(&mark.pack.to_le_bytes());
    b[12..16].copy_from_slice(&base.to_le_bytes());
    b[16..24].copy_from_slice(&mark.len.to_le_bytes());
    let crc = crc32c::crc32c(&b[..24]);
    b[24..28].copy_from_slice(&crc.to_le_bytes());
    b
}

fn decode(b: &[u8]) -> Option<(u64, Mark, u32)> {
    let b: &[u8; SLOT] = b.try_into().ok()?;
    if crc32c::crc32c(&b[..24]).to_le_bytes() != b[24..28] {
        return None;
    }
    let seq = u64::from_le_bytes(b[..8].try_into().ok()?);
    let pack = u32::from_le_bytes(b[8..12].try_into().ok()?);
    let base = u32::from_le_bytes(b[12..16].try_into().ok()?);
    let len = u64::from_le_bytes(b[16..24].try_into().ok()?);
    Some((seq, Mark { pack, len }, base))
}

pub(crate) struct Wm {
    io: Io,
    file: File,
    path: PathBuf,
    seq: u64,
    mark: Option<Mark>,
    base: u32,
}

impl std::fmt::Debug for Wm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Wm").field("mark", &self.mark).finish()
    }
}

impl Wm {
    /// Open or create the watermark file. `mark()` is `None` when it is absent or unreadable.
    pub(crate) fn open(io: &Io, dir: &Path) -> io::Result<Wm> {
        let path = dir.join(FILE_NAME);
        let existed = path.exists();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        if !existed {
            io.created(&path);
            io.sync_file(&file, &path)?;
            io.sync_dir(dir)?;
        }
        let mut buf = [0u8; 2 * SLOT];
        let mut n = 0;
        while n < buf.len() {
            match file.read_at(&mut buf[n..], n as u64)? {
                0 => break,
                k => n += k,
            }
        }
        let best = [&buf[..SLOT.min(n)], &buf[SLOT.min(n)..n]]
            .iter()
            .filter_map(|s| decode(s))
            .max_by_key(|(seq, _, _)| *seq);
        Ok(Wm {
            io: io.clone(),
            file,
            path,
            seq: best.map_or(0, |(s, _, _)| s),
            mark: best.map(|(_, m, _)| m),
            base: best.map_or(0, |(_, _, b)| b),
        })
    }

    pub(crate) fn mark(&self) -> Option<Mark> {
        self.mark
    }

    /// Lowest pack id that must exist.
    pub(crate) fn base(&self) -> u32 {
        self.base
    }

    /// Record `mark` durably. The caller must already have fsynced the data it describes.
    /// A mark that is not above the current one is ignored.
    pub(crate) fn advance(&mut self, mark: Mark) -> io::Result<()> {
        if self.mark.is_some_and(|m| m >= mark) {
            return Ok(());
        }
        self.write(mark, self.base)
    }

    /// First mark of a store. Writes two slots so tearing the newest one still leaves a valid mark.
    pub(crate) fn init(&mut self, mark: Mark) -> io::Result<()> {
        self.write(mark, self.base)?;
        self.write(mark, self.base)
    }

    /// Set the mark and base unconditionally, even downward. Used to accept a reported loss.
    pub(crate) fn reset(&mut self, mark: Mark, base: u32) -> io::Result<()> {
        self.write(mark, base)
    }

    fn write(&mut self, mark: Mark, base: u32) -> io::Result<()> {
        let io = self.io.clone();
        let seq = self.seq + 1;
        self.io.write_at(
            &self.file,
            &self.path,
            (seq % 2) * SLOT as u64,
            &encode(seq, mark, base),
        )?;
        io.sync_file(&self.file, &self.path)?;
        self.seq = seq;
        self.mark = Some(mark);
        self.base = base;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slots(path: &Path) -> Vec<Option<(u64, Mark, u32)>> {
        let b = std::fs::read(path).unwrap();
        b.chunks(SLOT).map(decode).collect()
    }

    #[test]
    fn two_slots_alternate_and_both_stay_valid() {
        let dir = tempfile::tempdir().unwrap();
        let io = Io::new(None, true);
        let mut wm = Wm::open(&io, dir.path()).unwrap();
        assert_eq!(wm.mark(), None);
        for len in [16, 20, 30] {
            wm.advance(Mark { pack: 0, len }).unwrap();
        }
        let s = slots(&dir.path().join(FILE_NAME));
        assert_eq!(s.len(), 2, "two slots at two offsets");
        let seqs: Vec<u64> = s.iter().map(|x| x.unwrap().0).collect();
        assert_eq!(seqs, vec![2, 3]);
        assert_eq!(wm.mark(), Some(Mark { pack: 0, len: 30 }));
    }

    #[test]
    fn a_torn_newest_slot_falls_back_to_the_older_one() {
        let dir = tempfile::tempdir().unwrap();
        let io = Io::new(None, true);
        let mut wm = Wm::open(&io, dir.path()).unwrap();
        wm.advance(Mark { pack: 0, len: 16 }).unwrap();
        wm.advance(Mark { pack: 0, len: 99 }).unwrap();
        drop(wm);
        let path = dir.path().join(FILE_NAME);
        let mut b = std::fs::read(&path).unwrap();
        b[16] ^= 0xff;
        std::fs::write(&path, &b).unwrap();
        let wm = Wm::open(&io, dir.path()).unwrap();
        assert_eq!(wm.mark(), Some(Mark { pack: 0, len: 16 }));
        for b in b.iter_mut() {
            *b ^= 0x55;
        }
        std::fs::write(&path, &b).unwrap();
        assert_eq!(Wm::open(&io, dir.path()).unwrap().mark(), None);
    }

    #[test]
    fn the_first_mark_is_written_twice_so_tearing_the_newest_slot_leaves_another() {
        let dir = tempfile::tempdir().unwrap();
        let io = Io::new(None, true);
        let mut wm = Wm::open(&io, dir.path()).unwrap();
        wm.init(Mark { pack: 0, len: 16 }).unwrap();
        drop(wm);
        let path = dir.path().join(FILE_NAME);
        let mut b = std::fs::read(&path).unwrap();
        let newest = slots(&path)
            .iter()
            .enumerate()
            .filter_map(|(i, s)| s.map(|s| (s.0, i)))
            .max()
            .unwrap()
            .1;
        b[newest * SLOT + 16] ^= 0xff;
        std::fs::write(&path, &b).unwrap();
        let wm = Wm::open(&io, dir.path()).unwrap();
        assert_eq!(wm.mark(), Some(Mark { pack: 0, len: 16 }));
    }

    #[test]
    fn advance_never_regresses_but_reset_does() {
        let dir = tempfile::tempdir().unwrap();
        let io = Io::new(None, true);
        let mut wm = Wm::open(&io, dir.path()).unwrap();
        wm.advance(Mark { pack: 3, len: 50 }).unwrap();
        wm.advance(Mark { pack: 2, len: 90 }).unwrap();
        assert_eq!(wm.mark(), Some(Mark { pack: 3, len: 50 }));
        wm.reset(Mark { pack: 1, len: 20 }, 1).unwrap();
        drop(wm);
        let wm = Wm::open(&io, dir.path()).unwrap();
        assert_eq!((wm.mark(), wm.base()), (Some(Mark { pack: 1, len: 20 }), 1));
    }
}
