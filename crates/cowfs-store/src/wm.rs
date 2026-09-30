//! Durable watermark: the last pack and length that a completed `sync` made durable.
//!
//! Two 32-byte slots are written alternately, each with its own CRC, so a torn write leaves the
//! older slot intact. A lower watermark than the truth is always safe. Writers must fsync the pack
//! data first and the watermark second.

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

fn encode(seq: u64, mark: Mark) -> [u8; SLOT] {
    let mut b = [0u8; SLOT];
    b[..8].copy_from_slice(&seq.to_le_bytes());
    b[8..12].copy_from_slice(&mark.pack.to_le_bytes());
    b[16..24].copy_from_slice(&mark.len.to_le_bytes());
    let crc = crc32c::crc32c(&b[..24]);
    b[24..28].copy_from_slice(&crc.to_le_bytes());
    b
}

fn decode(b: &[u8]) -> Option<(u64, Mark)> {
    let b: &[u8; SLOT] = b.try_into().ok()?;
    if crc32c::crc32c(&b[..24]).to_le_bytes() != b[24..28] {
        return None;
    }
    let seq = u64::from_le_bytes(b[..8].try_into().ok()?);
    let pack = u32::from_le_bytes(b[8..12].try_into().ok()?);
    let len = u64::from_le_bytes(b[16..24].try_into().ok()?);
    Some((seq, Mark { pack, len }))
}

pub(crate) struct Wm {
    file: File,
    path: PathBuf,
    seq: u64,
    mark: Option<Mark>,
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
            .max_by_key(|(seq, _)| *seq);
        Ok(Wm {
            file,
            path,
            seq: best.map_or(0, |(s, _)| s),
            mark: best.map(|(_, m)| m),
        })
    }

    pub(crate) fn mark(&self) -> Option<Mark> {
        self.mark
    }

    /// Record `mark` durably. The caller must already have fsynced the data it describes.
    pub(crate) fn advance(&mut self, io: &Io, mark: Mark) -> io::Result<()> {
        if self.mark.is_some_and(|m| m >= mark) {
            return Ok(());
        }
        let seq = self.seq + 1;
        self.file
            .write_all_at(&encode(seq, mark), (seq % 2) * SLOT as u64)?;
        io.sync_file(&self.file, &self.path)?;
        self.seq = seq;
        self.mark = Some(mark);
        Ok(())
    }
}
