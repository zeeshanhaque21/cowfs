//! Files whose mode has no write bit.
//!
//! Spike 4 lesson: parallel WRITE requests to a file created with mode 0444 or 0400 raced in
//! the passthrough server and failed with EACCES at fsync about half the time, and the mode
//! drifted. `Vfs` has no permission enforcement (everything is owned by the mounter), so the
//! mode is only metadata: writes, truncates and fsync must always succeed and the mode must
//! come back exactly as set.

use cowfs_vfs::{SetAttr, ROOT_INO};

use super::concurrency::par;
use super::{pattern, Ctx, Outcome};

const SIZE: usize = 8 << 20;
const ROUNDS: usize = 20;
const WRITERS: usize = 4;
const PIECE: usize = 64 << 10;

fn run(c: &Ctx, mode: u32) -> Outcome {
    let data = pattern(SIZE, u64::from(mode));
    let part = SIZE / WRITERS;
    for round in 0..ROUNDS {
        let name = format!("ro-{mode:o}-{round}");
        let f = c.create(ROOT_INO, name.as_bytes(), mode)?.ino;
        ensure_eq!(c.fs.getattr(f)?.mode, mode, "mode at creation");
        par(WRITERS, |t| {
            let base = t * part;
            for (i, piece) in data[base..base + part].chunks(PIECE).enumerate() {
                c.write_all(f, (base + i * PIECE) as u64, piece)?;
            }
            Ok(())
        })?;
        c.fs.fsync(f, false)?;
        c.fs.fsync(f, true)?;
        c.fs.flush(f)?;
        let a = c.fs.getattr(f)?;
        ensure_eq!(a.mode, mode, "mode after writing, round {round}");
        ensure_eq!(a.size, SIZE as u64, "size after writing, round {round}");
        ensure!(c.content(f)? == data, "content differs, round {round}");
        c.fs.unlink(ROOT_INO, name.as_bytes())?;
        c.forget_all(f);
    }
    let f = c.create(ROOT_INO, b"late", 0o644)?.ino;
    c.write_all(f, 0, b"before")?;
    c.fs.setattr(
        f,
        SetAttr {
            mode: Some(mode),
            ..Default::default()
        },
    )?;
    c.write_all(f, 6, b" after")?;
    c.fs.setattr(
        f,
        SetAttr {
            size: Some(3),
            ..Default::default()
        },
    )?;
    c.fs.fsync(f, false)?;
    let a = c.fs.getattr(f)?;
    ensure_eq!(a.mode, mode, "mode after chmod then write");
    ensure_eq!(
        c.content(f)?,
        b"bef".to_vec(),
        "content after chmod, write and truncate"
    );
    Ok(())
}

pub fn readonly_mode_0444_writes_always_succeed(c: &Ctx) -> Outcome {
    run(c, 0o444)
}

pub fn readonly_mode_0400_writes_always_succeed(c: &Ctx) -> Outcome {
    run(c, 0o400)
}
