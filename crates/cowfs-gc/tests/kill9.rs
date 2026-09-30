//! `kill -9` in a loop: a child writes and collects, the parent kills it at a random moment.
//!
//! After each kill the store is reopened and every block the metadata says is live must read back.
//! Unreferenced data may be missing, which is what a collect is for; a live block may not.
//!
//! Ignored by default because it forks processes:
//! `cargo test -p cowfs-gc --release --test kill9 -- --ignored --nocapture --test-threads 1`

mod common;

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use common::{eager, small_store_opts, Fixture, Roots};

const CHILD_DIR: &str = "COWFS_GC_KILL9_DIR";
const CHILD_MODE: &str = "COWFS_GC_KILL9_MODE";

fn body(n: usize, seed: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(n);
    let mut h = seed.wrapping_mul(2654435761).wrapping_add(1);
    for _ in 0..n {
        h = h.wrapping_mul(1664525).wrapping_add(1013904223);
        out.push(if h >> 29 == 0 {
            b'a'.wrapping_add((h >> 8) as u8)
        } else {
            (h >> 16) as u8
        });
    }
    out
}

/// Rounds. The store agent uses the same convention.
fn rounds() -> u32 {
    std::env::var("COWFS_KILL_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(30)
}

/// Open an existing store and database and check that every live block reads.
fn check(dir: &Path) -> Result<usize, String> {
    let store = cowfs_store::Store::open(dir.join("store"), small_store_opts(32 << 10))
        .map_err(|e| format!("store does not open: {e}"))?;
    if store.recovery().has_corruption() {
        return Err(format!("a kill left corruption: {:?}", store.recovery()));
    }
    let meta = cowfs_meta::Meta::open(
        dir.join("meta"),
        cowfs_meta::Options {
            background: false,
            ..cowfs_meta::Options::default()
        },
    )
    .map_err(|e| format!("meta does not open: {e}"))?;
    let mut marker = cowfs_meta::Marker::new();
    let mut live = 0usize;
    for info in meta.durable_snapshots().map_err(|e| e.to_string())? {
        let snap = meta
            .snapshot_by_id(info.id)
            .map_err(|e| format!("snapshot {} is gone: {e}", info.id.0))?;
        for b in snap
            .live_blocks(&mut marker)
            .map_err(|e| format!("walk: {e}"))?
        {
            let b = b.map_err(|e| format!("walk: {e}"))?;
            if b == cowfs_gc::HOLE {
                continue;
            }
            live += 1;
            store
                .get(b)
                .map_err(|e| format!("live block {} lost: {e}", &b.to_string()[..8]))?;
        }
    }
    let fsck = store.fsck().map_err(|e| e.to_string())?;
    if !fsck.is_clean() {
        return Err(format!("fsck found {} problems", fsck.damage.len()));
    }
    Ok(live)
}

/// The child: write, sync, collect, in a loop, until it is killed.
#[test]
fn child_writes_and_collects() {
    let Ok(dir) = std::env::var(CHILD_DIR) else {
        // Not the child: nothing to do.
        return;
    };
    let dir = Path::new(&dir).to_path_buf();
    let store = std::sync::Arc::new(
        cowfs_store::Store::open(dir.join("store"), small_store_opts(32 << 10))
            .expect("child store"),
    );
    let meta = std::sync::Arc::new(
        cowfs_meta::Meta::open(
            dir.join("meta"),
            cowfs_meta::Options {
                background: false,
                ..cowfs_meta::Options::default()
            },
        )
        .expect("child meta"),
    );
    let gc = cowfs_gc::Gc::open(
        dir.join("gc"),
        Arc::clone(&store),
        Arc::clone(&meta),
        eager(),
    )
    .expect("child gc");
    let roots = Roots::new();
    let names: Vec<String> = match std::env::var(CHILD_MODE).as_deref() {
        Ok("reopen") => Vec::new(),
        _ => (0..4).map(|i| format!("s{i}")).collect(),
    };
    for n in &names {
        if meta.snapshot(n).is_err() {
            meta.new_snapshot(n).expect("child snapshot");
        }
    }
    meta.sync().expect("child meta sync");
    let mut i = 0u32;
    loop {
        let name = format!("c{i:04}");
        let data = body(9000, i);
        let snap = meta.snapshot("s0").expect("child snapshot");
        let chunks = store.ingest_bytes(&data).expect("child ingest");
        let ino = match snap.lookup(cowfs_meta::ROOT_INO, name.as_bytes()) {
            Ok(a) => a.ino,
            Err(_) => {
                snap.batch(|tx| tx.create(cowfs_meta::ROOT_INO, name.as_bytes(), 0o644))
                    .expect("child create")
                    .ino
            }
        };
        snap.batch(|tx| tx.set_content(ino, &chunks, data.len() as u64))
            .expect("child set content");
        // Garbage, so every cycle has something to reclaim.
        store.put(&body(4000, i)).expect("child put");
        // Sync most iterations, so a kill leaves durable live blocks to check. A kill during the
        // unsynced ones is a bounded loss, which the store documents as acceptable.
        if i.is_multiple_of(2) {
            meta.sync().expect("child sync");
            store.sync().expect("child store sync");
        }
        if i.is_multiple_of(3) {
            let r = gc.collect(Some(&*roots)).expect("child collect");
            assert!(r.errors.is_empty(), "child collect: {:?}", r.errors);
        }
        i = i.wrapping_add(1);
    }
}

/// The parent: kill the child at a random moment, then check the store.
#[test]
#[ignore = "forks processes; see the module docs for the command"]
fn kill_9_in_a_loop_loses_nothing() {
    let n = rounds();
    let exe = std::env::current_exe().expect("current exe");
    let mut killed = 0u32;
    let mut checked = 0usize;
    for round in 0..n {
        let dir = tempfile::tempdir().expect("tempdir");
        // Seed a store and a database with real content, so the child has something to collect.
        {
            let f = Fixture::new(small_store_opts(32 << 10), eager());
            let snap = f.meta.new_snapshot("s0").expect("snapshot");
            f.write(&snap, b"seed", &body(120_000, 1));
            f.meta.sync().expect("meta sync");
            f.store.sync().expect("store sync");
            copy_out(f.path(), dir.path());
        }
        let mut child = Command::new(&exe)
            .args(["child_writes_and_collects", "--exact", "--nocapture"])
            .env(CHILD_DIR, dir.path())
            .env(CHILD_MODE, "write")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn child");
        // A random moment, from 5 to 400 ms, which covers a write, a sync and a collect.
        std::thread::sleep(std::time::Duration::from_millis(
            5 + (round as u64 * 37) % 400,
        ));
        // Verify the pid is the child before killing it.
        assert!(child.id() > 0, "the child has a pid");
        child.kill().expect("kill");
        let status = child.wait().expect("wait");
        assert!(
            !status.success(),
            "the child was killed, not clean: {status:?}"
        );
        killed += 1;
        let live = check(dir.path()).unwrap_or_else(|e| panic!("round {round}: {e}"));
        assert!(
            live > 0,
            "round {round}: the seed survived, so live is {live}"
        );
        checked += live;
    }
    assert_eq!(killed, n, "every round killed the child");
    assert!(checked > 0, "every round had live blocks to check");
    eprintln!("kill -9: {killed} rounds, {checked} live block reads verified");
}

/// Copy a fixture's store, database and collector state into a fresh directory, so the child
/// starts from known bytes. The database is one file, the others are directories.
fn copy_out(from: &Path, to: &Path) {
    for name in ["store", "meta", "gc"] {
        let src = from.join(name);
        if src.is_dir() {
            copy_tree(&src, &to.join(name));
        } else if src.exists() {
            std::fs::copy(&src, to.join(name)).expect("copy database");
        }
    }
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("mkdir");
    for e in std::fs::read_dir(from).expect("read_dir").flatten() {
        let src = e.path();
        let dst = to.join(e.file_name());
        if src.is_dir() {
            copy_tree(&src, &dst);
        } else {
            std::fs::copy(&src, &dst).expect("copy");
        }
    }
}
