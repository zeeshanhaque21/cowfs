//! File content: read, write, holes, truncate.
//!
//! Decisions pinned here: reads are never short except at end of file; `blocks` counts the
//! logical allocation of non-hole data in 512-byte units (0 for an empty file, at least the
//! file size for a fully written file, less than the file size for a sparse one); read and
//! write on a directory are `IsDir`, on a symlink `InvalidArgument`.

use cowfs_vfs::{Error, SetAttr, SetTime, Timestamp, ROOT_INO};

use super::basic::near;
use super::{pattern, Ctx, Outcome};

const OLD: Timestamp = Timestamp {
    secs: 1000,
    nanos: 0,
};

fn set_old_times(c: &Ctx, ino: u64) -> Outcome {
    let t = Some(SetTime::At(OLD));
    c.fs.setattr(
        ino,
        SetAttr {
            atime: t,
            mtime: t,
            ..Default::default()
        },
    )?;
    Ok(())
}

fn roundtrip(c: &Ctx, name: &str, len: usize, chunk: usize) -> Outcome {
    let ino = c.file(ROOT_INO, name)?;
    let data = pattern(len, len as u64 + 1);
    let mut off = 0;
    for part in data.chunks(chunk.max(1)) {
        c.write_all(ino, off, part)?;
        off += part.len() as u64;
    }
    let a = c.fs.getattr(ino)?;
    ensure_eq!(a.size, len as u64, "size after writing {len} bytes");
    if len > 0 {
        ensure!(a.blocks > 0, "blocks is 0 for a {len} byte file");
    }
    let back = c.read_all(ino, 0, len + 10)?;
    ensure!(
        back == data,
        "content differs after roundtrip of {len} bytes (got {} bytes)",
        back.len()
    );
    Ok(())
}

pub fn roundtrip_sizes(c: &Ctx) -> Outcome {
    for len in [0usize, 1, 4095, 4096, 4097, 65_536, 1 << 20] {
        roundtrip(c, &format!("whole-{len}"), len, usize::MAX)?;
        roundtrip(c, &format!("chunked-{len}"), len, 1000)?;
    }
    Ok(())
}

/// Heavy: an 8 MiB file written in one piece and in 64 KiB pieces.
pub fn roundtrip_8_mib(c: &Ctx) -> Outcome {
    roundtrip(c, "whole", 8 << 20, usize::MAX)?;
    roundtrip(c, "chunked", 8 << 20, 65_536)
}

pub fn read_past_eof_is_empty(c: &Ctx) -> Outcome {
    let f = c.file(ROOT_INO, "f")?;
    ensure!(c.fs.read(f, 0, 100)?.is_empty(), "read of empty file");
    c.write_all(f, 0, b"0123456789")?;
    for off in [10, 11, 4096, 1 << 40, u64::MAX >> 1] {
        let r = c.fs.read(f, off, 5)?;
        ensure!(
            r.is_empty(),
            "read at offset {off} past the end returned {} bytes",
            r.len()
        );
    }
    ensure!(
        c.fs.read(f, 3, 0)?.is_empty(),
        "zero-length read returned data"
    );
    Ok(())
}

pub fn read_crossing_eof_is_short(c: &Ctx) -> Outcome {
    let f = c.file(ROOT_INO, "f")?;
    c.write_all(f, 0, b"0123456789")?;
    ensure_eq!(
        c.fs.read(f, 4, 100)?,
        b"456789".to_vec(),
        "read crossing the end"
    );
    ensure_eq!(
        c.fs.read(f, 0, 10)?,
        b"0123456789".to_vec(),
        "read of exactly the whole file"
    );
    ensure_eq!(c.fs.read(f, 9, 1)?, b"9".to_vec(), "read of the last byte");
    ensure_eq!(
        c.fs.read(f, 0, 1 << 20)?,
        b"0123456789".to_vec(),
        "huge read"
    );
    Ok(())
}

pub fn write_past_eof_makes_zero_hole(c: &Ctx) -> Outcome {
    let f = c.file(ROOT_INO, "f")?;
    c.write_all(f, 0, b"abc")?;
    c.write_all(f, 10, b"Z")?;
    ensure_eq!(c.fs.getattr(f)?.size, 11, "size after write at 10");
    let mut want = b"abc".to_vec();
    want.extend([0u8; 7]);
    want.push(b'Z');
    ensure_eq!(c.content(f)?, want, "content with hole");

    let g = c.file(ROOT_INO, "g")?;
    c.write_all(g, 3 * 4096 + 5, b"tail")?;
    let got = c.content(g)?;
    ensure_eq!(got.len(), 3 * 4096 + 9, "length of file written past EOF");
    ensure!(
        got[..3 * 4096 + 5].iter().all(|&b| b == 0),
        "hole before the write is not zero"
    );
    ensure_eq!(&got[3 * 4096 + 5..], b"tail", "written bytes");
    Ok(())
}

pub fn overwrite_in_middle(c: &Ctx) -> Outcome {
    let f = c.file(ROOT_INO, "f")?;
    let mut model = pattern(3 * 4096 + 100, 7);
    c.write_all(f, 0, &model)?;
    let patch = pattern(5000, 8);
    c.write_all(f, 4000, &patch)?;
    model[4000..9000].copy_from_slice(&patch);
    ensure_eq!(
        c.fs.getattr(f)?.size,
        model.len() as u64,
        "size after in-place overwrite"
    );
    ensure!(
        c.content(f)? == model,
        "content after overwrite across page boundaries"
    );
    Ok(())
}

pub fn sparse_write_far_past_eof(c: &Ctx) -> Outcome {
    let f = c.file(ROOT_INO, "sparse")?;
    let far = 1u64 << 30;
    c.write_all(f, far, b"end")?;
    let a = c.fs.getattr(f)?;
    ensure_eq!(a.size, far + 3, "size after far write");
    ensure!(
        a.blocks * 512 < far,
        "hole was materialised: {} blocks for a sparse file",
        a.blocks
    );
    for off in [0, 4096, far / 2, far - 4096] {
        let r = c.fs.read(f, off, 4096)?;
        ensure_eq!(r.len(), 4096, "hole read length at {off}");
        ensure!(r.iter().all(|&b| b == 0), "hole at {off} is not zero");
    }
    ensure_eq!(
        c.fs.read(f, far - 2, 100)?,
        b"\0\0end".to_vec(),
        "read across the end of the hole"
    );
    c.write_all(f, far / 2, b"mid")?;
    ensure_eq!(
        c.fs.read(f, far / 2 - 1, 5)?,
        b"\0mid\0".to_vec(),
        "write in the middle of a hole"
    );
    c.fs.setattr(
        f,
        SetAttr {
            size: Some(0),
            ..Default::default()
        },
    )?;
    let a = c.fs.getattr(f)?;
    ensure_eq!(
        (a.size, a.blocks),
        (0, 0),
        "size and blocks after truncate to 0"
    );
    Ok(())
}

pub fn truncate_shrink_then_grow_zero_fills(c: &Ctx) -> Outcome {
    let f = c.file(ROOT_INO, "f")?;
    let data = pattern(10_000, 5);
    c.write_all(f, 0, &data)?;
    let a = c.fs.setattr(
        f,
        SetAttr {
            size: Some(5000),
            ..Default::default()
        },
    )?;
    ensure_eq!(a.size, 5000, "size after shrink");
    ensure!(c.content(f)? == data[..5000], "content after shrink");
    let a = c.fs.setattr(
        f,
        SetAttr {
            size: Some(20_000),
            ..Default::default()
        },
    )?;
    ensure_eq!(a.size, 20_000, "size after grow");
    let got = c.content(f)?;
    ensure_eq!(got.len(), 20_000, "content length after grow");
    ensure!(got[..5000] == data[..5000], "prefix changed by grow");
    ensure!(
        got[5000..].iter().all(|&b| b == 0),
        "grown region is not zero-filled"
    );
    c.fs.setattr(
        f,
        SetAttr {
            size: Some(0),
            ..Default::default()
        },
    )?;
    ensure!(c.fs.read(f, 0, 100)?.is_empty(), "read after truncate to 0");
    c.write_all(f, 100, b"x")?;
    let got = c.content(f)?;
    ensure!(
        got[..100].iter().all(|&b| b == 0),
        "old data reappeared after truncate to 0"
    );
    Ok(())
}

pub fn truncate_to_same_size(c: &Ctx) -> Outcome {
    let f = c.file(ROOT_INO, "f")?;
    c.write_all(f, 0, b"hello")?;
    let a = c.fs.setattr(
        f,
        SetAttr {
            size: Some(5),
            ..Default::default()
        },
    )?;
    ensure_eq!(a.size, 5, "size after no-op truncate");
    ensure_eq!(
        c.content(f)?,
        b"hello".to_vec(),
        "content after no-op truncate"
    );
    Ok(())
}

pub fn write_updates_mtime_and_ctime(c: &Ctx) -> Outcome {
    let f = c.file(ROOT_INO, "f")?;
    for (what, size_op) in [
        ("write", None),
        ("truncate shrink", Some(1)),
        ("truncate grow", Some(50)),
    ] {
        c.write_all(f, 0, b"0123456789")?;
        set_old_times(c, f)?;
        let old = c.fs.getattr(f)?;
        c.tick();
        let before = Timestamp::now();
        match size_op {
            None => c.write_all(f, 3, b"xyz")?,
            Some(n) => {
                c.fs.setattr(
                    f,
                    SetAttr {
                        size: Some(n),
                        ..Default::default()
                    },
                )?;
            }
        }
        let after = Timestamp::now();
        let a = c.fs.getattr(f)?;
        ensure!(
            a.mtime > old.mtime && near(a.mtime, before, after),
            "{what} did not update mtime: {:?}",
            a.mtime
        );
        ensure!(
            a.ctime > old.ctime && near(a.ctime, before, after),
            "{what} did not update ctime: {:?}",
            a.ctime
        );
    }
    Ok(())
}

pub fn blocks_accounting(c: &Ctx) -> Outcome {
    let f = c.file(ROOT_INO, "f")?;
    ensure_eq!(c.fs.getattr(f)?.blocks, 0, "blocks of an empty file");
    c.write_all(f, 0, b"x")?;
    ensure!(c.fs.getattr(f)?.blocks > 0, "blocks of a 1 byte file is 0");
    let len = 1usize << 20;
    c.write_all(f, 0, &pattern(len, 9))?;
    let b = c.fs.getattr(f)?.blocks * 512;
    ensure!(
        b >= len as u64,
        "fully written 1 MiB file reports only {b} bytes of blocks"
    );
    ensure!(
        b <= len as u64 + (1 << 20),
        "1 MiB file reports {b} bytes of blocks"
    );
    c.fs.setattr(
        f,
        SetAttr {
            size: Some(0),
            ..Default::default()
        },
    )?;
    ensure_eq!(c.fs.getattr(f)?.blocks, 0, "blocks after truncate to 0");
    Ok(())
}

pub fn io_on_non_regular_files(c: &Ctx) -> Outcome {
    let d = c.dir(ROOT_INO, "d")?;
    ensure_err!(c.fs.write(d, 0, b"x"), Error::IsDir, "write to a directory");
    ensure_err!(c.fs.read(d, 0, 1), Error::IsDir, "read of a directory");
    let s = c.symlink(ROOT_INO, b"s", b"target")?.ino;
    ensure_err!(
        c.fs.write(s, 0, b"x"),
        Error::InvalidArgument,
        "write to a symlink"
    );
    ensure_err!(
        c.fs.read(s, 0, 1),
        Error::InvalidArgument,
        "read of a symlink"
    );
    Ok(())
}

pub fn random_overlapping_writes_match_model(c: &Ctx) -> Outcome {
    let f = c.file(ROOT_INO, "f")?;
    let mut model: Vec<u8> = Vec::new();
    let mut x = 0x2545_F491_4F6C_DD1Du64;
    let mut next = |m: u64| {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x % m
    };
    for i in 0..200u64 {
        let off = next(100_000) as usize;
        let len = 1 + next(20_000) as usize;
        let data = pattern(len, i + 100);
        c.write_all(f, off as u64, &data)?;
        if model.len() < off + len {
            model.resize(off + len, 0);
        }
        model[off..off + len].copy_from_slice(&data);
        if i % 25 == 24 {
            ensure_eq!(
                c.fs.getattr(f)?.size,
                model.len() as u64,
                "size after write {i}"
            );
            let ro = next(model.len() as u64) as usize;
            let rl = 1 + next(30_000) as usize;
            let end = (ro + rl).min(model.len());
            ensure!(
                c.fs.read(f, ro as u64, rl as u32)? == model[ro..end],
                "read {ro}+{rl} differs after write {i}"
            );
        }
    }
    ensure!(
        c.content(f)? == model,
        "final content differs from the model"
    );
    Ok(())
}
