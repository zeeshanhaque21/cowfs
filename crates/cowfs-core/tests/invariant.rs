//! Control-plane invariants under random operation sequences with crashes.
//!
//! Each run drives a seeded random sequence of control operations and file writes, syncing and
//! checkpointing what it acknowledged. A child process may die at any point; the parent reopens and
//! checks that the four invariants hold:
//!
//! 1. no acknowledged name is lost or duplicated (names are unique after the mount's name fold),
//! 2. no staging name of an interrupted swap is ever visible,
//! 3. every acknowledged, fsynced file still reads back with the bytes it was acknowledged with,
//! 4. `pinned_blocks` never omits a block and never names one the store does not hold.
//!
//! Heavy: `INVARIANT_ITERS=2000 INVARIANT_CRASHES=40 cargo test -p cowfs-core --release --test
//! invariant`.

mod common;

use std::collections::BTreeMap;
use std::io::Write as _;
use std::process::{Command, Stdio};

use common::*;
use cowfs_core::{ControlError, Core, Options};
use cowfs_vfs::{SetAttr, Vfs, ROOT_INO};

const NAMES: [&str; 6] = ["alpha", "beta", "gamma", "delta", "eps", "zeta"];

/// What a run acknowledged, so a reopen can check it. `None` means the name is gone on purpose.
type Acked = BTreeMap<String, (u64, Vec<u8>)>;

fn iters() -> u64 {
    std::env::var("INVARIANT_ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(400)
}

fn crashes() -> u64 {
    std::env::var("INVARIANT_CRASHES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(12)
}

fn child(which: u64) -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().to_string_lossy().into_owned();
    // the parent drives the same sequence up to `which`, then this child aborts somewhere inside
    let exe = std::env::current_exe().expect("current_exe");
    let out = Command::new(exe)
        .args([
            "invariant_child",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("INV_DIR", &path)
        .env("INV_WHICH", which.to_string())
        .env("INV_ITERS", iters().to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .expect("child");
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    use std::os::unix::process::ExitStatusExt;
    assert_eq!(out.status.signal(), Some(6), "child did not abort: {text}");
    assert!(
        text.contains("CHECKPOINT"),
        "child did not finish its checkpoint: {text}"
    );
    (dir, text)
}

/// The child: replay a prefix of the sequence, run a swap with a fault injected at a random step
/// (so both the rollback and the roll-forward paths run), then print the checkpoint and abort.
/// A swap that has returned leaves no intent behind, so the checkpoint is what the next open must
/// find.
fn replay(dir: &std::path::Path, upto: u64) -> Acked {
    let c = Core::open(dir, opts()).expect("open");
    let mut acked = Acked::new();
    let mut rng = Rng(0xC0FFEE ^ upto.wrapping_mul(31));
    for step in 0..upto {
        drive(&c, &mut rng, step, &mut acked);
    }
    c.set_swap_fault((rng.below(5) + 1) as u8);
    if !acked.is_empty() {
        let src = acked.keys().next().cloned().expect("a name");
        let base = format!("base{}", rng.below(3));
        let source = acked.get(&src).cloned().unwrap();
        if c.promote_base(&src, &base).is_ok() {
            acked.insert(base, source);
        }
        c.set_swap_fault(0);
    }
    c.sync().expect("sync");
    report(&acked);
    println!("CHECKPOINT");
    std::io::stdout().flush().unwrap();
    std::process::abort();
}

#[allow(dead_code)]
fn report(acked: &Acked) {
    let mut o = std::io::stdout();
    writeln!(o).unwrap();
    for (k, (ino, bytes)) in acked {
        writeln!(o, "A {k} {ino}").unwrap();
        for chunk in bytes.chunks(64) {
            writeln!(
                o,
                "B {}",
                chunk.iter().map(|b| format!("{b:02x}")).collect::<String>()
            )
            .unwrap();
        }
        writeln!(o, "E").unwrap();
    }
    o.flush().unwrap();
}

fn parse_acked(text: &str) -> Acked {
    let mut out = Acked::new();
    let mut name = String::new();
    let mut ino = 0u64;
    let mut bytes: Vec<u8> = Vec::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("A ") {
            let mut it = rest.split(' ');
            name = it.next().unwrap_or_default().to_string();
            ino = it.next().and_then(|v| v.parse().ok()).unwrap_or(0);
            bytes = Vec::new();
        } else if let Some(rest) = line.strip_prefix("B ") {
            bytes.extend(
                (0..rest.len() / 2)
                    .filter_map(|i| u8::from_str_radix(&rest[2 * i..2 * i + 2], 16).ok()),
            );
        } else if line == "E" {
            assert!(!name.is_empty(), "checkpoint frame has no name: {text}");
            out.insert(std::mem::take(&mut name), (ino, std::mem::take(&mut bytes)));
        }
    }
    if !name.is_empty() {
        out.insert(name, (ino, bytes));
    }
    out
}

fn opts() -> Options {
    Options {
        background: false,
        ..Options::default()
    }
}

/// One random control operation plus a file write, recording what was acknowledged.
fn drive(c: &Core, rng: &mut Rng, step: u64, acked: &mut Acked) {
    let live: Vec<String> = c
        .list_snapshots()
        .unwrap_or_default()
        .into_iter()
        .map(|e| e.name)
        .collect();
    let name = NAMES[rng.below(NAMES.len() as u64) as usize].to_string();
    match rng.below(10) {
        0..=3 => {
            if c.create_snapshot(&name).is_ok() {
                let fs = c.snapshot_view(&name).expect("view");
                let a = fs.create(ROOT_INO, b"f", 0o644).expect("create");
                let data = pattern(3000 + rng.below(9000) as usize, step);
                fs.write(a.ino, 0, &data).expect("write");
                fs.fsync(a.ino, false).expect("fsync");
                let _ = fs.release(fs.open(a.ino).expect("open"));
                c.sync().expect("sync");
                acked.insert(name, (a.ino, data));
            }
        }
        4 | 5 => {
            if !live.is_empty() {
                let src = live[rng.below(live.len() as u64) as usize].clone();
                let target = format!("f{step}");
                if c.fork_snapshot(&src, &target).is_ok() {
                    if let Some(v) = acked.get(&src).cloned() {
                        acked.insert(target, v);
                    }
                }
            }
        }
        6 | 7 => {
            if !live.is_empty() {
                let victim = live[rng.below(live.len() as u64) as usize].clone();
                if c.remove_snapshot(&victim).is_ok() {
                    acked.remove(&victim);
                }
            }
        }
        8 => {
            if !live.is_empty() {
                let src = live[rng.below(live.len() as u64) as usize].clone();
                let other = NAMES[rng.below(NAMES.len() as u64) as usize].to_string();
                if c.rename_snapshot(&src, &other).is_ok() {
                    // the target is replaced, whatever it held
                    acked.remove(&other);
                    if let Some(v) = acked.remove(&src) {
                        acked.insert(other, v);
                    }
                }
            }
        }
        _ => {
            if !live.is_empty() {
                let src = live[rng.below(live.len() as u64) as usize].clone();
                let base = format!("base{}", rng.below(3));
                match c.promote_base(&src, &base) {
                    Ok(_) => {
                        // the base is replaced, whatever it held
                        acked.remove(&base);
                        if let Some(v) = acked.get(&src).cloned() {
                            acked.insert(base, v);
                        }
                    }
                    // a refusal, or an injected swap fault, is a legitimate outcome
                    Err(ControlError::Busy)
                    | Err(ControlError::NotFound)
                    | Err(ControlError::Exists)
                    | Err(ControlError::InvalidName(_)) => {}
                    Err(ControlError::Fs(e)) if e.to_string().contains("injected") => {}
                    Err(e) => panic!("promote_base: {e:?}"),
                }
            }
        }
    }
    c.set_swap_fault(if rng.below(4) == 0 {
        (rng.below(5) + 1) as u8
    } else {
        0
    });
}

/// The four invariants, checked against what a run acknowledged.
fn check(c: &Core, acked: &Acked, when: &str) {
    let listed: Vec<String> = c
        .list_snapshots()
        .unwrap_or_else(|e| panic!("{when}: list_snapshots: {e:?}"))
        .into_iter()
        .map(|e| e.name)
        .collect();
    // 2. no staging name, ever
    for n in &listed {
        assert!(
            !n.contains(".cowfs-swap"),
            "{when}: a staging snapshot is visible: {listed:?}"
        );
    }
    // 1. no duplicate name after the mount's name fold
    let mut keys: Vec<String> = listed.iter().map(|n| cowfs_core::name_key(n)).collect();
    keys.sort();
    let before = keys.len();
    keys.dedup();
    assert_eq!(
        before,
        keys.len(),
        "{when}: duplicate snapshot names: {listed:?}"
    );
    for (name, (_, bytes)) in acked {
        if !listed.contains(name) {
            // only a name we removed or renamed may be gone; the caller keeps `acked` in step
            panic!("{when}: acknowledged snapshot {name} is gone: {listed:?}");
        }
        let fs = c
            .snapshot_view(name)
            .unwrap_or_else(|e| panic!("{when}: snapshot_view({name}): {e:?}"));
        let a = fs
            .lookup(ROOT_INO, b"f")
            .unwrap_or_else(|e| panic!("{when}: lookup of f in {name}: {e:?}"));
        // 3. the acknowledged bytes are still there
        assert_eq!(
            read_all(&fs, a.ino),
            *bytes,
            "{when}: {name}/f does not read back the bytes it acknowledged"
        );
        fs.setattr(a.ino, SetAttr::default()).expect("setattr");
    }
    // 4. the pin set only names blocks the store holds
    match c.pinned_blocks() {
        Ok(blocks) => {
            for b in blocks {
                assert!(
                    c.store().contains(b),
                    "{when}: the pin set names a missing block {b}"
                );
            }
        }
        Err(ControlError::Busy) => {}
        Err(e) => panic!("{when}: pinned_blocks: {e:?}"),
    }
}

#[test]
fn invariant_child() {
    let Ok(dir) = std::env::var("INV_DIR") else {
        return;
    };
    let which: u64 = std::env::var("INV_WHICH")
        .unwrap_or_default()
        .parse()
        .unwrap_or(0);
    replay(std::path::Path::new(&dir), which);
}

#[test]
fn control_plane_invariants_hold_across_random_sequences() {
    let mut rng = Rng(7);
    for round in 0..crashes() {
        let (dir, text) = child(round * 3 + 1);
        let acked = parse_acked(&text);
        let c = Core::open(dir.path(), opts())
            .unwrap_or_else(|e| panic!("round {round}: reopen: {e:?}"));
        check(&c, &acked, &format!("round {round} after a crash"));
        // a clean run of the same length, then the invariants again
        let dir2 = tempfile::tempdir().expect("tempdir");
        let mut a2 = Acked::new();
        {
            let c2 = Core::open(dir2.path(), opts()).expect("open");
            for step in 0..iters() {
                drive(&c2, &mut rng, step, &mut a2);
            }
            c2.sync().expect("sync");
            check(&c2, &a2, &format!("round {round} live"));
            c2.check().expect("check");
        }
        let c2 = Core::open(dir2.path(), opts()).expect("reopen");
        check(&c2, &a2, &format!("round {round} after a clean reopen"));
    }
    // one long sequence with a crash in the middle of it
    let dir = tempfile::tempdir().expect("tempdir");
    let mut a = Acked::new();
    {
        let c = Core::open(dir.path(), opts()).expect("open");
        for step in 0..iters() {
            drive(&c, &mut rng, step, &mut a);
        }
    }
    let c = Core::open(dir.path(), opts()).expect("reopen");
    check(&c, &a, "long sequence after a close and reopen");
    c.check().expect("check");
}

#[test]
fn checkpoint_child_really_runs_and_retains_its_store() {
    let (dir, text) = child(10);
    assert!(
        text.contains("running 1 test"),
        "checkpoint child was not selected: {text}"
    );
    assert!(
        dir.path().join("meta.redb").exists(),
        "checkpoint fixture was deleted"
    );
    let acked = parse_acked(&text);
    assert!(acked.keys().all(|name| !name.is_empty()));
    let c = Core::open(dir.path(), opts()).unwrap();
    check(&c, &acked, "checkpoint regression");
}

#[test]
fn checkpoint_spike_compares_unchanged_and_replaced_targets() {
    for replace in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let c = Core::open(dir.path(), opts()).unwrap();
        let mut acked = Acked::new();
        for (name, bytes) in [("src", b"NEW".as_slice()), ("base", b"OLD".as_slice())] {
            c.create_snapshot(name).unwrap();
            let fs = c.snapshot_view(name).unwrap();
            let a = mkfile(&fs, ROOT_INO, "f", bytes);
            fs.fsync(a.ino, false).unwrap();
            acked.insert(name.to_string(), (a.ino, bytes.to_vec()));
        }
        if replace {
            c.promote_base("src", "base").unwrap();
        }
        let stale = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            check(&c, &acked, "controlled checkpoint spike");
        }));
        assert_eq!(
            stale.is_err(),
            replace,
            "replacement must change the oracle"
        );
        let fs = c.snapshot_view("base").unwrap();
        let a = fs.lookup(ROOT_INO, b"f").unwrap();
        assert_eq!(read_all(&fs, a.ino), if replace { b"NEW" } else { b"OLD" });
    }
}
