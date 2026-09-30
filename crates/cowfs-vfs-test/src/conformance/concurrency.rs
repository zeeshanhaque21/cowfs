//! Concurrency stress. Each check is capped in run time, so a slow backend does fewer
//! iterations instead of timing out; a hang is caught by the runner's timeout.
//!
//! Decisions pinned here: a read of one aligned 4 KiB block never observes a mix of two
//! writes of that block (cowfs; native Linux torn reads at that size, see the docs); a read of
//! 8 bytes never observes a mix of two writes either (that held natively, `Cowfs`-free);
//! with concurrent namespace changes the only acceptable errors are the ones a racing
//! caller can legitimately see, and the directory tree stays a tree.

use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use cowfs_vfs::{Error, FileKind, Ino, RenameFlags, ROOT_INO};

use super::{pattern, Ctx, Failure, Outcome};

const CAP: Duration = Duration::from_secs(15);

/// Runs `f(0..n)` on `n` threads and returns the first failure. A panic is a failure.
pub(crate) fn par(n: usize, f: impl Fn(usize) -> Outcome + Sync) -> Outcome {
    let f = &f;
    std::thread::scope(|s| {
        let handles: Vec<_> = (0..n).map(|i| s.spawn(move || f(i))).collect();
        let mut first = Ok(());
        for h in handles {
            let r = h
                .join()
                .unwrap_or_else(|_| Err(Failure("worker thread panicked".into())));
            if first.is_ok() {
                first = r;
            }
        }
        first
    })
}

pub fn concurrent_creates_in_one_directory(c: &Ctx) -> Outcome {
    const THREADS: usize = 8;
    const N: usize = 250;
    let d = c.dir(ROOT_INO, "d")?;
    let wins = AtomicUsize::new(0);
    par(THREADS, |t| {
        for i in 0..N {
            match c.fs.create(d, format!("shared{i}").as_bytes(), 0o644) {
                Ok(_) => {
                    wins.fetch_add(1, Ordering::Relaxed);
                }
                Err(Error::Exists) => {}
                Err(e) => return Err(e.into()),
            }
            c.fs.create(d, format!("own{t}-{i}").as_bytes(), 0o644)?;
        }
        Ok(())
    })?;
    ensure_eq!(
        wins.load(Ordering::Relaxed),
        N,
        "threads that won the race for a shared name (exactly one per name)"
    );
    let list = c.list(d)?;
    ensure_eq!(
        list.len(),
        N + THREADS * N,
        "entries after concurrent creates"
    );
    let names: HashSet<&Vec<u8>> = list.iter().map(|e| &e.name).collect();
    ensure_eq!(names.len(), list.len(), "duplicate names");
    let inos: HashSet<Ino> = list.iter().map(|e| e.ino).collect();
    ensure_eq!(inos.len(), list.len(), "two names share an inode");
    for e in &list {
        ensure_eq!(
            c.fs.lookup(d, &e.name)?.ino,
            e.ino,
            "lookup of {:?}",
            String::from_utf8_lossy(&e.name)
        );
    }
    Ok(())
}

pub fn concurrent_writes_to_different_files(c: &Ctx) -> Outcome {
    const THREADS: usize = 8;
    const SIZE: usize = 256 << 10;
    let inos: Vec<Ino> = (0..THREADS)
        .map(|t| c.file(ROOT_INO, &format!("f{t}")))
        .collect::<Result<_, _>>()?;
    par(THREADS, |t| {
        let data = pattern(SIZE, t as u64 + 1);
        let mut off = 0;
        for piece in data.chunks(4093) {
            c.write_all(inos[t], off, piece)?;
            off += piece.len() as u64;
        }
        Ok(())
    })?;
    for (t, &ino) in inos.iter().enumerate() {
        ensure!(
            c.content(ino)? == pattern(SIZE, t as u64 + 1),
            "content of file {t} differs"
        );
    }
    Ok(())
}

/// cowfs contract: a read of one aligned 4 KiB block never observes a mix of two writes of that
/// block. Native Linux does not provide this (ext4, btrfs and tmpfs tore 8 to 25 reads in about
/// 300k at this size, APFS never did), so this is a `Cowfs` check, not POSIX.
pub fn concurrent_readers_and_writers_of_one_file(c: &Ctx) -> Outcome {
    const PAGES: u64 = 16;
    const PG: usize = 4096;
    const ROUNDS: usize = 2000;
    let f = c.file(ROOT_INO, "f")?;
    c.write_all(f, 0, &vec![1u8; PAGES as usize * PG])?;
    let start = Instant::now();
    par(6, |t| {
        for i in 0..ROUNDS {
            if start.elapsed() > CAP {
                break;
            }
            let off = ((i as u64 * 7 + t as u64) % PAGES) * PG as u64;
            if t < 3 {
                let v = (t * 60 + i % 50 + 2) as u8;
                ensure_eq!(
                    c.fs.write(f, off, &vec![v; PG])? as usize,
                    PG,
                    "short write"
                );
            } else {
                let got = c.fs.read(f, off, PG as u32)?;
                ensure_eq!(got.len(), PG, "read length at {off}");
                ensure!(
                    got.iter().all(|&b| b == got[0]),
                    "torn read at offset {off}: block mixes two writes"
                );
            }
        }
        Ok(())
    })?;
    let a = c.fs.getattr(f)?;
    ensure_eq!(a.size, PAGES * PG as u64, "size after concurrent writes");
    for (i, page) in c.content(f)?.chunks(PG).enumerate() {
        ensure!(page.iter().all(|&b| b == page[0]), "final page {i} is torn");
    }
    Ok(())
}

/// Writes and reads of one aligned 8-byte word never tear, unlike the 4 KiB case. Measured on
/// ext4, btrfs, tmpfs and APFS in about 1M reads per size, where only 8 and 64 byte accesses
/// stayed untorn. Two readers and one writer is the POSIX atomically-avoided interleaving.
pub fn small_reads_are_never_torn(c: &Ctx) -> Outcome {
    const WORDS: u64 = 8192;
    const N: usize = 8;
    let f = c.file(ROOT_INO, "f")?;
    c.write_all(f, 0, &vec![1u8; WORDS as usize * N])?;
    let start = Instant::now();
    par(3, |t| {
        for i in 0..2000 {
            if start.elapsed() > CAP {
                break;
            }
            let word = (i as u64 * 7 + t as u64) % WORDS;
            let off = word * N as u64;
            if t < 3 {
                let v = (t * 60 + i % 50 + 2) as u8;
                ensure_eq!(c.fs.write(f, off, &vec![v; N])? as usize, N, "short write");
                let got = c.fs.read(f, off, N as u32)?;
                ensure_eq!(got.len(), N, "read length at {off}");
                ensure!(
                    got.iter().all(|&b| b == got[0]),
                    "torn read at offset {off}: a write and its own read differ"
                );
            } else {
                let got = c.fs.read(f, off, N as u32)?;
                ensure_eq!(got.len(), N, "read length at {off}");
                ensure!(
                    got.iter().all(|&b| b == got[0]),
                    "torn 8 byte read at offset {off}: a plain read saw two writes"
                );
            }
        }
        Ok(())
    })?;
    Ok(())
}

fn benign(e: Error) -> Outcome {
    match e {
        Error::NotFound
        | Error::Exists
        | Error::NotEmpty
        | Error::Stale
        | Error::InvalidArgument
        | Error::NotDir
        | Error::IsDir => Ok(()),
        e => Err(e.into()),
    }
}

fn swallow<T>(r: cowfs_vfs::Result<T>) -> Outcome {
    r.map(|_| ()).or_else(benign)
}

pub fn concurrent_rename_unlink_lookup(c: &Ctx) -> Outcome {
    const THREADS: usize = 8;
    const ROUNDS: usize = 3000;
    let a = c.dir(ROOT_INO, "a")?;
    let b = c.dir(ROOT_INO, "b")?;
    let start = Instant::now();
    let fs = &*c.fs;
    par(THREADS, |t| {
        let mut x = 0x9E37_79B9_7F4A_7C15u64 ^ ((t as u64 + 1) << 32);
        let mut next = |m: u64| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x % m
        };
        for _ in 0..ROUNDS {
            if start.elapsed() > CAP {
                break;
            }
            let (k, j) = (format!("n{}", next(6)), format!("n{}", next(6)));
            let (p, q) = if next(2) == 0 { (a, b) } else { (b, a) };
            let (top, other) = (format!("t{}", next(3)), format!("t{}", next(3)));
            match next(11) {
                0 | 1 => swallow(fs.create(p, k.as_bytes(), 0o644))?,
                2 => swallow(fs.unlink(p, k.as_bytes()))?,
                3 | 4 => {
                    swallow(fs.rename(p, k.as_bytes(), q, j.as_bytes(), RenameFlags::default()))?
                }
                5 => match fs.lookup(p, k.as_bytes()) {
                    Ok(at) => swallow(fs.link(at.ino, q, j.as_bytes()))?,
                    Err(e) => benign(e)?,
                },
                6 => match fs.lookup(p, k.as_bytes()) {
                    Ok(at) => swallow(fs.getattr(at.ino))?,
                    Err(e) => benign(e)?,
                },
                7 => {
                    let mut cookie = 0;
                    for _ in 0..1000 {
                        match fs.readdir(p, cookie, 3) {
                            Ok(r) => match r.entries.last() {
                                Some(l) if !r.eof => cookie = l.cookie,
                                _ => break,
                            },
                            Err(e) => {
                                benign(e)?;
                                break;
                            }
                        }
                    }
                }
                8 => swallow(fs.mkdir(ROOT_INO, top.as_bytes(), 0o755))?,
                9 => match fs.lookup(ROOT_INO, other.as_bytes()) {
                    Ok(at) => swallow(fs.rename(
                        ROOT_INO,
                        top.as_bytes(),
                        at.ino,
                        format!("m{top}").as_bytes(),
                        RenameFlags::default(),
                    ))?,
                    Err(e) => benign(e)?,
                },
                _ => match fs.lookup(ROOT_INO, other.as_bytes()) {
                    Ok(at) => {
                        swallow(fs.rename(
                            at.ino,
                            format!("m{top}").as_bytes(),
                            ROOT_INO,
                            top.as_bytes(),
                            RenameFlags::default(),
                        ))?;
                        swallow(fs.rmdir(ROOT_INO, other.as_bytes()))?
                    }
                    Err(e) => benign(e)?,
                },
            }
        }
        Ok(())
    })?;
    let mut seen: HashSet<Ino> = HashSet::new();
    let mut stack = vec![ROOT_INO];
    while let Some(dir) = stack.pop() {
        ensure!(
            seen.insert(dir),
            "directory {dir} is reachable twice: the tree has a cycle or a shared directory"
        );
        let list = c.list(dir)?;
        let names: HashSet<&Vec<u8>> = list.iter().map(|e| &e.name).collect();
        ensure_eq!(
            names.len(),
            list.len(),
            "duplicate names in directory {dir}"
        );
        let subdirs = list
            .iter()
            .filter(|e| e.kind == FileKind::Directory)
            .count();
        ensure_eq!(
            c.fs.getattr(dir)?.nlink as usize,
            2 + subdirs,
            "nlink of directory {dir}"
        );
        for e in list {
            let at = c.fs.lookup(dir, &e.name)?;
            ensure_eq!(
                (at.ino, at.kind),
                (e.ino, e.kind),
                "lookup of {:?} disagrees with the listing",
                String::from_utf8_lossy(&e.name)
            );
            if e.kind == FileKind::Directory {
                stack.push(e.ino);
            }
        }
    }
    Ok(())
}
