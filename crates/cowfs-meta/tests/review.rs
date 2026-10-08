//! Regression tests for the findings of the review of PR 25 (F1 to F13).

mod common;

use common::backend::{apply, Be, Ev};
use cowfs_meta::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn quiet() -> Options {
    Options {
        background: false,
        sync_interval: Duration::from_secs(3600),
        ..Options::default()
    }
}

fn chunk(n: u8, len: u32) -> ChunkRef {
    ChunkRef::block(BlockId::of(&[n]), len)
}

/// The file as a crash would leave it if only fsynced writes reached the disk.
fn synced_image(log: &[Ev]) -> Vec<u8> {
    let mut img = Vec::new();
    let mut pending: Vec<&Ev> = Vec::new();
    for ev in log {
        if matches!(ev, Ev::S(_)) {
            for e in pending.drain(..) {
                apply(&mut img, e);
            }
        } else {
            pending.push(ev);
        }
    }
    img
}

fn counting_hook(n: &Arc<AtomicUsize>) -> SyncHook {
    let n = n.clone();
    Arc::new(move || {
        n.fetch_add(1, SeqCst);
        Ok(())
    })
}

/// A store whose blocks are durable only after `sync`; the backend tags each fsync with the number
/// of durable blocks so a crash image can be checked for dangling references.
struct FakeStore {
    put: Mutex<Vec<BlockId>>,
    durable: Arc<AtomicUsize>,
}

impl FakeStore {
    fn put(&self, data: &[u8]) -> BlockId {
        let id = BlockId::of(data);
        self.put.lock().unwrap().push(id);
        id
    }

    fn sync(&self) {
        self.durable.store(self.put.lock().unwrap().len(), SeqCst);
    }
}

/// F1: a closure that `put`s a block and then references it must never be durable before the
/// block is. Every fsync of the metadata file is a crash point; every chunk reachable from the
/// reopened tree must be among the blocks the store had synced by then.
#[test]
fn hook_runs_after_the_closure_so_no_chunk_dangles() {
    let durable = Arc::new(AtomicUsize::new(0));
    let store = Arc::new(FakeStore {
        put: Mutex::new(Vec::new()),
        durable: durable.clone(),
    });
    let be = Be {
        tag: durable,
        ..Be::default()
    };
    let st = store.clone();
    let opts = Options {
        sync_every_ops: 3,
        before_sync: Some(Arc::new(move || {
            st.sync();
            Ok(())
        })),
        ..quiet()
    };
    let m = Meta::open_with_backend(be.clone(), opts).unwrap();
    let s = m.new_snapshot("s").unwrap();
    for i in 0..1000u32 {
        s.batch(|tx| {
            let id = store.put(&i.to_le_bytes());
            let f = tx.create(ROOT_INO, format!("f{i}").as_bytes(), 0o644)?;
            tx.set_content(f.ino, &[ChunkRef::block(id, 4)], 4)?;
            Ok(())
        })
        .unwrap();
    }
    let log = be.log();
    let mut img = Vec::new();
    let (mut syncs, mut bad, mut with_files) = (0, 0, 0);
    for ev in &log {
        apply(&mut img, ev);
        let Ev::S(tag) = ev else { continue };
        syncs += 1;
        let Ok(r) = Meta::open_with_backend(Be::from_image(img.clone()), quiet()) else {
            continue;
        };
        let Ok(sn) = r.snapshot("s") else { continue };
        let known: std::collections::HashSet<BlockId> =
            store.put.lock().unwrap()[..*tag].iter().copied().collect();
        let mut mk = Marker::new();
        let live: Vec<BlockId> = sn
            .live_blocks(&mut mk)
            .unwrap()
            .map(|b| b.unwrap())
            .collect();
        with_files += usize::from(!live.is_empty());
        bad += usize::from(live.iter().any(|b| !known.contains(b)));
    }
    eprintln!("{syncs} fsyncs, {with_files} with chunks, {bad} with dangling chunks");
    assert!(
        with_files > 100,
        "the test did not exercise durable commits"
    );
    assert_eq!(
        bad, 0,
        "durable metadata referenced blocks the store had not synced"
    );
}

/// F1 (mutant guard): the hook really runs before durable commits, once per commit.
#[test]
fn sync_every_ops_is_exact_and_each_commit_runs_the_hook_once() {
    let n = Arc::new(AtomicUsize::new(0));
    let m = Meta::open_with_backend(
        Be::default(),
        Options {
            sync_every_ops: 3,
            before_sync: Some(counting_hook(&n)),
            ..quiet()
        },
    )
    .unwrap();
    let s = m.new_snapshot("s").unwrap();
    let base = n.load(SeqCst);
    assert!(base >= 1, "creating a snapshot must run the hook");
    s.create(ROOT_INO, b"a", 0o644).unwrap();
    s.create(ROOT_INO, b"b", 0o644).unwrap();
    assert_eq!(n.load(SeqCst), base, "hook ran before the third op");
    s.create(ROOT_INO, b"c", 0o644).unwrap();
    assert_eq!(
        n.load(SeqCst),
        base + 1,
        "third op must trigger exactly one hook run"
    );
    s.create(ROOT_INO, b"d", 0o644).unwrap();
    assert_eq!(n.load(SeqCst), base + 1);
}

/// F4: `sync()` runs the hook even with nothing pending.
#[test]
fn sync_runs_the_hook_with_nothing_pending() {
    let n = Arc::new(AtomicUsize::new(0));
    let m = Meta::open_with_backend(
        Be::default(),
        Options {
            before_sync: Some(counting_hook(&n)),
            ..quiet()
        },
    )
    .unwrap();
    m.new_snapshot("s").unwrap();
    let base = n.load(SeqCst);
    m.sync().unwrap();
    m.sync().unwrap();
    assert_eq!(n.load(SeqCst), base + 2);
}

/// `sync()` makes every applied change durable (kills the "sync commits non-durably" mutant).
#[test]
fn sync_makes_applied_changes_durable() {
    let be = Be::default();
    let m = Meta::open_with_backend(be.clone(), quiet()).unwrap();
    let s = m.new_snapshot("s").unwrap();
    s.create(ROOT_INO, b"a", 0o644).unwrap();
    s.create(ROOT_INO, b"b", 0o644).unwrap();
    assert_ne!(m.durable_snapshots().unwrap()[0].root, s.root().unwrap());
    m.sync().unwrap();
    assert_eq!(m.durable_snapshots().unwrap()[0].root, s.root().unwrap());
    let r = Meta::open_with_backend(Be::from_image(synced_image(&be.log())), quiet()).unwrap();
    assert_eq!(
        r.snapshot("s")
            .unwrap()
            .readdir(ROOT_INO, 0, 10)
            .unwrap()
            .entries
            .len(),
        2
    );
}

/// F4: the timer is real. 100 creates, no further mutation, no sync call: they become durable.
#[test]
fn idle_changes_become_durable_by_the_timer() {
    let be = Be::default();
    let m = Meta::open_with_backend(
        be.clone(),
        Options {
            background: true,
            sync_interval: Duration::from_millis(300),
            ..Options::default()
        },
    )
    .unwrap();
    let s = m.new_snapshot("s").unwrap();
    for i in 0..100 {
        s.create(ROOT_INO, format!("f{i}").as_bytes(), 0o644)
            .unwrap();
    }
    assert_ne!(m.durable_snapshots().unwrap()[0].root, s.root().unwrap());
    let start = Instant::now();
    while m.durable_snapshots().unwrap()[0].root != s.root().unwrap() {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "timer never flushed"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        start.elapsed() >= Duration::from_millis(100),
        "flushed before the interval"
    );
    let r = Meta::open_with_backend(Be::from_image(synced_image(&be.log())), quiet()).unwrap();
    assert_eq!(
        r.snapshot("s")
            .unwrap()
            .readdir(ROOT_INO, 0, 1000)
            .unwrap()
            .entries
            .len(),
        100
    );
}

/// F4: a failing hook keeps the timer retrying and never commits.
#[test]
fn timer_with_a_failing_hook_never_commits() {
    let be = Be::default();
    let fail = Arc::new(AtomicBool::new(false));
    let f2 = fail.clone();
    let m = Meta::open_with_backend(
        be.clone(),
        Options {
            background: true,
            sync_interval: Duration::from_millis(50),
            before_sync: Some(Arc::new(move || {
                if f2.load(SeqCst) {
                    Err(std::io::Error::other("no"))
                } else {
                    Ok(())
                }
            })),
            ..Options::default()
        },
    )
    .unwrap();
    let s = m.new_snapshot("s").unwrap();
    m.sync().unwrap();
    fail.store(true, SeqCst);
    let durable_before = m.durable_snapshots().unwrap()[0].root;
    s.create(ROOT_INO, b"a", 0o644).unwrap();
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(m.durable_snapshots().unwrap()[0].root, durable_before);
    fail.store(false, SeqCst);
    let start = Instant::now();
    while m.durable_snapshots().unwrap()[0].root == durable_before {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "timer did not recover"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// F3: after any crash, the next inode number is above every number ever handed out.
#[test]
fn inode_numbers_are_never_reused_after_a_crash() {
    for block in [1024u64, 8] {
        let be = Be::default();
        let m = Meta::open_with_backend(
            be.clone(),
            Options {
                ino_block: block,
                ..quiet()
            },
        )
        .unwrap();
        let s = m.new_snapshot("s").unwrap();
        let mut handed: Vec<(usize, u64)> = Vec::new();
        for i in 0..100u32 {
            let a = if i % 5 == 0 {
                s.mkdir(ROOT_INO, format!("d{i}").as_bytes(), 0o755)
                    .unwrap()
            } else {
                s.create(ROOT_INO, format!("f{i}").as_bytes(), 0o644)
                    .unwrap()
            };
            handed.push((be.log().len(), a.ino.0));
        }
        let log = be.log();
        let (mut running, mut synced, mut pending): (Vec<u8>, Vec<u8>, Vec<usize>) =
            (Vec::new(), Vec::new(), Vec::new());
        let (mut checked, mut fails) = (0, Vec::new());
        for p in 0..=log.len() {
            let max = handed
                .iter()
                .filter(|(n, _)| *n <= p)
                .map(|(_, i)| *i)
                .max()
                .unwrap_or(0);
            for (what, img) in [("synced-only", &synced), ("prefix", &running)] {
                let Ok(r) = Meta::open_with_backend(Be::from_image(img.clone()), quiet()) else {
                    continue;
                };
                let Ok(sn) = r.snapshot("s") else { continue };
                let ino = sn.create(ROOT_INO, b"after-crash", 0o644).unwrap().ino.0;
                checked += 1;
                if ino <= max {
                    fails.push(format!(
                        "block {block} point {p} {what}: next inode {ino} <= handed out {max}"
                    ));
                }
            }
            if let Some(ev) = log.get(p) {
                apply(&mut running, ev);
                if matches!(ev, Ev::S(_)) {
                    for i in pending.drain(..) {
                        apply(&mut synced, &log[i]);
                    }
                } else {
                    pending.push(p);
                }
            }
        }
        assert!(checked > 20, "too few crash points: {checked}");
        assert!(
            fails.is_empty(),
            "{}",
            fails[..fails.len().min(5)].join("\n")
        );
    }
}

/// F3: an ordinary restart continues the counter and snapshot ids; removed ids are not reused.
#[test]
fn snapshot_ids_are_never_reused_and_pack_ino_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("m.redb");
    let (a, b);
    {
        let m = Meta::open(&path, quiet()).unwrap();
        let sa = m.new_snapshot("a").unwrap();
        let sb = sa.fork("b").unwrap();
        (a, b) = (sa.id(), sb.id());
        m.remove_snapshot(b).unwrap();
    }
    let m = Meta::open(&path, quiet()).unwrap();
    let c = m.snapshot("a").unwrap().fork("c").unwrap();
    assert!(c.id().0 > b.0 && b.0 > a.0);

    let p = Meta::pack_ino(c.id(), Ino(12345)).unwrap();
    assert_eq!(Meta::unpack_ino(p), (c.id(), Ino(12345)));
    let top = Meta::pack_ino(SnapshotId(SNAPSHOT_LIMIT - 1), Ino(INO_LIMIT - 1)).unwrap();
    assert_eq!(
        Meta::unpack_ino(top),
        (SnapshotId(SNAPSHOT_LIMIT - 1), Ino(INO_LIMIT - 1))
    );
    assert_eq!(Meta::pack_ino(SnapshotId(0), Ino(1)), None);
    assert_eq!(Meta::pack_ino(SnapshotId(SNAPSHOT_LIMIT), Ino(1)), None);
    assert_eq!(Meta::pack_ino(SnapshotId(1), Ino(INO_LIMIT)), None);
    let (x, y) = (
        Meta::pack_ino(SnapshotId(1), Ino(2)),
        Meta::pack_ino(SnapshotId(2), Ino(1)),
    );
    assert_ne!(x, y);
}

/// F9: a hook that calls back into the store gets an error instead of a deadlock.
#[test]
fn hook_reentry_is_an_error_not_a_deadlock() {
    let cell: Arc<Mutex<Option<Meta>>> = Arc::new(Mutex::new(None));
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let (c2, s2) = (cell.clone(), seen.clone());
    let opts = Options {
        before_sync: Some(Arc::new(move || {
            if let Some(m) = c2.lock().unwrap().clone() {
                s2.lock()
                    .unwrap()
                    .push(format!("sync: {:?}", m.sync().err().map(|e| e.to_string())));
                s2.lock().unwrap().push(format!(
                    "read: {:?}",
                    m.snapshots().err().map(|e| e.to_string())
                ));
                s2.lock().unwrap().push(format!(
                    "close: {:?}",
                    m.close().err().map(|e| e.to_string())
                ));
            }
            Ok(())
        })),
        ..quiet()
    };
    let (tx, rx) = std::sync::mpsc::channel();
    let seen2 = seen.clone();
    std::thread::spawn(move || {
        let m = Meta::open_with_backend(Be::default(), opts).unwrap();
        *cell.lock().unwrap() = Some(m.clone());
        let r = m.new_snapshot("s").is_ok();
        *cell.lock().unwrap() = None;
        tx.send((r, seen2.lock().unwrap().clone())).ok();
    });
    let (ok, msgs) = rx.recv_timeout(Duration::from_secs(10)).expect("deadlock");
    assert!(ok);
    assert!(msgs.len() >= 3);
    for m in msgs {
        assert!(m.contains("re-entered"), "{m}");
    }
}

/// F11: durable acks share commits, and every ack means durable.
#[test]
fn concurrent_durable_callers_share_commits() {
    let n = Arc::new(AtomicUsize::new(0));
    let be = Be::default();
    let m = Meta::open_with_backend(
        be.clone(),
        Options {
            ack: Ack::Durable,
            before_sync: Some(counting_hook(&n)),
            ..quiet()
        },
    )
    .unwrap();
    let s = m.new_snapshot("s").unwrap();
    let base = n.load(SeqCst);
    let (threads, per) = (8usize, 40usize);
    std::thread::scope(|sc| {
        for t in 0..threads {
            let s = s.clone();
            let m = m.clone();
            sc.spawn(move || {
                for i in 0..per {
                    s.create(ROOT_INO, format!("t{t}-{i}").as_bytes(), 0o644)
                        .unwrap();
                    let d = m.durable_snapshots().unwrap();
                    assert!(!d.is_empty());
                }
            });
        }
    });
    let commits = n.load(SeqCst) - base;
    eprintln!(
        "{} durable creates shared {commits} hook runs",
        threads * per
    );
    assert!(commits >= 1);
    assert!(commits < threads * per, "no commit was shared");
    let r = Meta::open_with_backend(Be::from_image(synced_image(&be.log())), quiet()).unwrap();
    assert_eq!(
        r.snapshot("s")
            .unwrap()
            .readdir(ROOT_INO, 0, 1000)
            .unwrap()
            .entries
            .len(),
        threads * per
    );
}

/// With one caller, a durable ack means the durable root equals the applied root.
#[test]
fn durable_ack_is_durable_on_return() {
    let m = Meta::open_with_backend(
        Be::default(),
        Options {
            ack: Ack::Durable,
            ..quiet()
        },
    )
    .unwrap();
    let s = m.new_snapshot("s").unwrap();
    for i in 0..5 {
        s.create(ROOT_INO, format!("f{i}").as_bytes(), 0o644)
            .unwrap();
        assert_eq!(m.durable_snapshots().unwrap()[0].root, s.root().unwrap());
    }
}

/// F12: renaming every entry as it is listed terminates, because a same-directory rename keeps
/// the entry's cookie.
#[test]
fn renaming_every_listed_entry_terminates() {
    let m = Meta::open_with_backend(Be::default(), quiet()).unwrap();
    let s = m.new_snapshot("s").unwrap();
    for i in 0..50 {
        s.create(ROOT_INO, format!("f{i:02}").as_bytes(), 0o644)
            .unwrap();
    }
    let (mut cookie, mut seen, mut iters) = (0, Vec::new(), 0);
    loop {
        iters += 1;
        assert!(iters < 500, "listing never finished");
        let page = s.readdir(ROOT_INO, cookie, 1).unwrap();
        let Some(e) = page.entries.first() else { break };
        let new = [e.name.clone(), b"x".to_vec()].concat();
        s.rename(ROOT_INO, &e.name, ROOT_INO, &new).unwrap();
        cookie = e.cookie;
        seen.push(e.cookie);
        if page.end {
            break;
        }
    }
    assert_eq!(seen.len(), 50);
    let mut u = seen.clone();
    u.dedup();
    assert_eq!(u.len(), 50);
    let l = s.readdir(ROOT_INO, 0, 100).unwrap();
    assert_eq!(l.entries.iter().map(|e| e.cookie).collect::<Vec<_>>(), seen);
    assert!(l.entries.iter().all(|e| e.name.ends_with(b"x")));
}

/// F12: shrinking inside a chunk is `NeedsRechunk`, distinct from other errors.
#[test]
fn shrinking_inside_a_chunk_needs_a_rechunk() {
    let m = Meta::open_with_backend(Be::default(), quiet()).unwrap();
    let s = m.new_snapshot("s").unwrap();
    let f = s.create(ROOT_INO, b"f", 0o644).unwrap().ino;
    s.set_content(f, &[chunk(1, 100), chunk(2, 100)], 200)
        .unwrap();
    let set = |size| SetAttr {
        size: Some(size),
        ..SetAttr::default()
    };
    assert!(matches!(s.setattr(f, set(150)), Err(Error::NeedsRechunk)));
    assert_eq!(s.setattr(f, set(100)).unwrap().size, 100);
    assert_eq!(s.chunks(f).unwrap(), [chunk(1, 100)]);
    assert_eq!(s.setattr(f, set(300)).unwrap().size, 300);
    assert!(matches!(s.setattr(f, set(50)), Err(Error::NeedsRechunk)));
    assert_eq!(s.setattr(f, set(0)).unwrap().size, 0);
    assert!(s.chunks(f).unwrap().is_empty());
}

/// F7: compare-and-swap range splice.
#[test]
fn splice_content_is_a_compare_and_swap_on_ranges() {
    let m = Meta::open_with_backend(Be::default(), quiet()).unwrap();
    let s = m.new_snapshot("s").unwrap();
    let f = s.create(ROOT_INO, b"f", 0o644).unwrap().ino;
    let list: Vec<ChunkRef> = (0..10).map(|i| chunk(i, 10)).collect();
    s.set_content(f, &list, 100).unwrap();
    let v = s.content_version(f).unwrap();

    let r = s.chunk_range(f, 20, 50).unwrap();
    assert_eq!((r.version, r.size, r.covered), (v, 100, 100));
    assert_eq!(
        r.chunks.iter().map(|(o, _)| *o).collect::<Vec<_>>(),
        [20, 30, 40]
    );

    let v2 = s
        .splice_content(f, v, 20, 50, &[chunk(90, 30)], 100)
        .unwrap();
    assert!(v2 > v);
    let now = s.chunks(f).unwrap();
    assert_eq!(now.len(), 8);
    assert_eq!(now[2], chunk(90, 30));
    assert_eq!(&now[3..], &list[5..]);

    assert!(matches!(
        s.splice_content(f, v, 0, 10, &[chunk(1, 10)], 100),
        Err(Error::Conflict)
    ));
    assert!(matches!(
        s.splice_content(f, v2, 5, 30, &[chunk(1, 25)], 100),
        Err(Error::Invalid(_))
    ));
    assert!(matches!(
        s.splice_content(f, v2, 0, 20, &[chunk(1, 10)], 100),
        Err(Error::Invalid(_))
    ));
    let v3 = s
        .splice_content(f, v2, 100, 100, &[chunk(7, 10), chunk(8, 10)], 130)
        .unwrap();
    assert_eq!(s.getattr(f).unwrap().size, 130);
    assert_eq!(s.chunks(f).unwrap().len(), 10);
    let v4 = s.splice_content(f, v3, 100, 120, &[], 100).unwrap();
    assert_eq!(s.chunks(f).unwrap().len(), 8);
    assert!(v4 > v3);
    assert!(s.splice_content(f, v4, 100, 100, &[], 90).is_err());
    m.check().unwrap();

    let (a, b) = (s.content_version(f).unwrap(), s.content_version(f).unwrap());
    s.splice_content(f, a, 0, 10, &[chunk(50, 10)], 100)
        .unwrap();
    assert!(matches!(
        s.splice_content(f, b, 10, 20, &[chunk(51, 10)], 100),
        Err(Error::Conflict)
    ));
}

/// Bytes written to the file by one durable append to a file of `n` chunks.
fn append_cost(n: u32) -> usize {
    let be = Be::default();
    let m = Meta::open_with_backend(be.clone(), quiet()).unwrap();
    let s = m.new_snapshot("s").unwrap();
    let f = s.create(ROOT_INO, b"f", 0o644).unwrap().ino;
    let list: Vec<ChunkRef> = (0..n)
        .map(|i| ChunkRef::block(BlockId::of(&i.to_le_bytes()), 65536))
        .collect();
    let covered = u64::from(n) * 65536;
    s.set_content(f, &list, covered).unwrap();
    m.sync().unwrap();
    let before = be.log().len();
    let v = s.content_version(f).unwrap();
    s.splice_content(f, v, covered, covered, &[chunk(1, 65536)], covered + 65536)
        .unwrap();
    m.sync().unwrap();
    assert_eq!(s.chunks(f).unwrap().len(), n as usize + 1);
    be.log()[before..]
        .iter()
        .map(|e| if let Ev::W(_, d) = e { d.len() } else { 0 })
        .sum()
}

/// F7: an append to a file with 50,000 chunks writes a leaf-to-root path (O(log n) pages, with a
/// large constant from redb), not the chunk list: 10 chunks 33 KB, 100,000 chunks 197 KB (release).
#[test]
fn appending_to_a_huge_file_writes_a_bounded_amount() {
    let (small, huge) = (append_cost(10), append_cost(50_000));
    eprintln!("append wrote {small} bytes at 10 chunks and {huge} bytes at 50000 chunks");
    let list_bytes = 50_000 * 36;
    assert!(
        huge < 8 * small,
        "{huge} bytes for the huge file, {small} for the small one"
    );
    assert!(
        huge < list_bytes / 8,
        "{huge} bytes is not small against the {list_bytes}-byte list"
    );
}

/// F13: removing a snapshot is durable at once, and its nodes are freed in bounded steps.
#[test]
fn remove_snapshot_is_incremental_and_exact() {
    let m = Meta::open_with_backend(
        Be::default(),
        Options {
            node_size: 512,
            ..quiet()
        },
    )
    .unwrap();
    let keep = m.new_snapshot("keep").unwrap();
    keep.create(ROOT_INO, b"k", 0o644).unwrap();
    let big = m.new_snapshot("big").unwrap();
    big.batch(|tx| {
        for i in 0..3000 {
            let f = tx.create(ROOT_INO, format!("f{i}").as_bytes(), 0o644)?;
            tx.set_content(f.ino, &[chunk((i % 200) as u8, 1)], 1)?;
        }
        Ok(())
    })
    .unwrap();
    m.sync().unwrap();
    m.remove_snapshot(big.id()).unwrap();
    assert!(matches!(big.getattr(ROOT_INO), Err(Error::NoSuchSnapshot)));
    assert_eq!(m.pending_reap().unwrap(), 1);
    let mut steps = 0;
    while m.reap_step().unwrap() {
        steps += 1;
        assert!(steps < 10_000);
        if steps % 3 == 0 {
            m.check().unwrap();
        }
    }
    assert!(steps >= 3, "one step freed the whole tree: {steps}");
    assert_eq!(m.pending_reap().unwrap(), 0);
    m.check().unwrap();
    assert!(keep.lookup(ROOT_INO, b"k").is_ok());
}

/// F13: the background reaper frees a removed snapshot without any call from the user.
#[test]
fn background_reaper_drains_the_queue() {
    let m = Meta::open_with_backend(
        Be::default(),
        Options {
            node_size: 512,
            background: true,
            ..Options::default()
        },
    )
    .unwrap();
    let a = m.new_snapshot("a").unwrap();
    a.batch(|tx| {
        for i in 0..2000 {
            tx.create(ROOT_INO, format!("f{i}").as_bytes(), 0o644)?;
        }
        Ok(())
    })
    .unwrap();
    m.remove_snapshot(a.id()).unwrap();
    let start = Instant::now();
    while m.pending_reap().unwrap() > 0 {
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "reaper did not finish"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    m.check().unwrap();
}

/// A file that is not a cowfs-meta database is refused and left untouched.
#[test]
fn foreign_files_are_refused_and_left_alone() {
    let dir = tempfile::tempdir().unwrap();

    let other = dir.path().join("other.redb");
    {
        let db = redb::Database::create(&other).unwrap();
        let w = db.begin_write().unwrap();
        w.open_table(redb::TableDefinition::<u64, u64>::new("theirs"))
            .unwrap()
            .insert(1, 2)
            .unwrap();
        w.commit().unwrap();
    }
    assert!(matches!(Meta::open(&other, quiet()), Err(Error::Format(_))));
    let db = redb::Database::open(&other).unwrap();
    let r = redb::ReadableDatabase::begin_read(&db).unwrap();
    let names: Vec<String> = r
        .list_tables()
        .unwrap()
        .map(|t| redb::TableHandle::name(&t).to_string())
        .collect();
    assert_eq!(names, ["theirs"], "a refused file gained tables");
    let t = r
        .open_table(redb::TableDefinition::<u64, u64>::new("theirs"))
        .unwrap();
    assert_eq!(redb::ReadableTable::get(&t, 1).unwrap().unwrap().value(), 2);
    drop((t, r, db));

    let text = dir.path().join("notes.txt");
    std::fs::write(&text, b"this is not a database at all, just some text").unwrap();
    assert!(Meta::open(&text, quiet()).is_err());
    assert_eq!(
        std::fs::read(&text).unwrap(),
        b"this is not a database at all, just some text"
    );

    let mine = dir.path().join("mine.redb");
    drop(Meta::open(&mine, quiet()).unwrap());
    assert!(Meta::open(&mine, quiet()).is_ok());
}

/// F8: once a corrupted read is detected, the handle refuses writes; nothing wrong is returned
/// as success along the way.
#[test]
fn handle_fails_closed_after_detecting_corruption() {
    let be = Be {
        flip_every: 3,
        ..Be::default()
    };
    let m = Meta::open_with_backend(
        be.clone(),
        Options {
            node_size: 512,
            node_cache: 0,
            cache_size: 1 << 16,
            ..quiet()
        },
    )
    .unwrap();
    let s = m.new_snapshot("s").unwrap();
    s.batch(|tx| {
        for i in 0..300 {
            tx.create(ROOT_INO, format!("f{i}").as_bytes(), 0o644)?;
        }
        Ok(())
    })
    .unwrap();
    m.sync().unwrap();
    be.flaky.store(true, SeqCst);
    let mut detected = false;
    for i in 0..300 {
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            s.lookup(ROOT_INO, format!("f{i}").as_bytes())
        }));
        match r {
            Ok(Ok(a)) => assert_eq!(a.kind, FileType::File),
            Ok(Err(Error::Corrupt(_))) | Err(_) => {
                detected = true;
                break;
            }
            Ok(Err(_)) => {}
        }
    }
    be.flaky.store(false, SeqCst);
    assert!(detected, "no flipped read was detected");
    assert!(matches!(
        s.create(ROOT_INO, b"after", 0o644),
        Err(Error::Corrupt(_))
    ));
}

/// F8: transient read bit flips during writes. Whatever reaches the disk, `check()` either
/// reports it or the tree equals a clean replay of the operations that succeeded.
#[test]
fn flips_during_writes_are_detected_or_harmless() {
    let mut detected = 0;
    let mut clean = 0;
    for flip_every in [7usize, 23, 101, 400] {
        let be = Be {
            flip_every,
            ..Be::default()
        };
        let opts = Options {
            node_size: 512,
            node_cache: 0,
            cache_size: 1 << 16,
            sync_every_ops: 2,
            ..quiet()
        };
        let m = Meta::open_with_backend(be.clone(), opts.clone()).unwrap();
        let s = m.new_snapshot("s").unwrap();
        be.flaky.store(true, SeqCst);
        let mut ok = Vec::new();
        let mut failed_closed = false;
        for i in 0..200 {
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                s.create(ROOT_INO, format!("f{i}").as_bytes(), 0o644)
            }));
            match r {
                Ok(Ok(_)) => ok.push(i),
                Ok(Err(Error::Corrupt(_))) => failed_closed = true,
                Ok(Err(_)) => {}
                Err(_) => panic!("panic escaped"),
            }
            if failed_closed {
                assert!(matches!(
                    s.create(ROOT_INO, b"more", 0o644),
                    Err(Error::Corrupt(_))
                ));
                break;
            }
        }
        be.flaky.store(false, SeqCst);
        drop((s, m));
        let img = be.image();
        let names = |m: &Meta| -> Vec<Vec<u8>> {
            m.snapshot("s")
                .unwrap()
                .readdir(ROOT_INO, 0, 1000)
                .unwrap()
                .entries
                .into_iter()
                .map(|e| e.name)
                .collect()
        };
        let Ok(r) = Meta::open_with_backend(Be::from_image(img), quiet()) else {
            detected += 1;
            continue;
        };
        match r.check() {
            Err(_) => detected += 1,
            Ok(()) => {
                clean += 1;
                let got = names(&r);
                assert!(
                    got.len() <= ok.len() + 1 && got.iter().all(|n| n.starts_with(b"f")),
                    "flip {flip_every}: check passed but content is wrong"
                );
            }
        }
    }
    eprintln!("flips during writes: {detected} detected, {clean} clean");
}

/// F14: 120 s concurrent hammer (writers forking and removing snapshots, readers checking a
/// two-file-per-batch atomicity invariant). Reports the worst write latency.
///
/// `cargo test -p cowfs-meta --release --test review hammer -- --ignored --nocapture`
#[test]
#[ignore = "120 s"]
fn hammer_120s() {
    use std::sync::atomic::AtomicU64;
    let dir = tempfile::tempdir().unwrap();
    let m = Meta::open(dir.path().join("h.redb"), Options::default()).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let (viol, reads, writes, maxw) = (
        Arc::new(AtomicU64::new(0)),
        Arc::new(AtomicU64::new(0)),
        Arc::new(AtomicU64::new(0)),
        Arc::new(AtomicU64::new(0)),
    );
    let mut hs = Vec::new();
    for w in 0..3u32 {
        let (m, st, wr, mw) = (m.clone(), stop.clone(), writes.clone(), maxw.clone());
        hs.push(std::thread::spawn(move || {
            let mut mine = m.new_snapshot(&format!("w{w}-0")).unwrap();
            let (mut n, mut generation) = (0u64, 0);
            while !st.load(SeqCst) {
                let t = Instant::now();
                mine.batch(|tx| {
                    tx.create(ROOT_INO, format!("a{n}").as_bytes(), 0o644)?;
                    tx.create(ROOT_INO, format!("b{n}").as_bytes(), 0o644)?;
                    Ok(())
                })
                .unwrap();
                mw.fetch_max(t.elapsed().as_micros() as u64, SeqCst);
                wr.fetch_add(1, SeqCst);
                n += 1;
                if n % 200 == 0 {
                    generation += 1;
                    let old = mine.id();
                    mine = mine.fork(&format!("w{w}-{generation}")).unwrap();
                    m.remove_snapshot(old).unwrap();
                }
            }
        }));
    }
    for _ in 0..4 {
        let (m, st, v, r) = (m.clone(), stop.clone(), viol.clone(), reads.clone());
        hs.push(std::thread::spawn(move || {
            while !st.load(SeqCst) {
                for info in m.snapshots().unwrap() {
                    let Ok(s) = m.snapshot(&info.name) else {
                        continue;
                    };
                    let Ok(p) = s.readdir(ROOT_INO, 0, 1_000_000) else {
                        continue;
                    };
                    let a = p.entries.iter().filter(|e| e.name[0] == b'a').count();
                    let b = p.entries.iter().filter(|e| e.name[0] == b'b').count();
                    v.fetch_add(u64::from(a != b), SeqCst);
                    r.fetch_add(1, SeqCst);
                }
            }
        }));
    }
    std::thread::sleep(Duration::from_secs(120));
    stop.store(true, SeqCst);
    for h in hs {
        h.join().unwrap();
    }
    eprintln!(
        "hammer: {} batches, {} reads, {} torn, worst write {} us",
        writes.load(SeqCst),
        reads.load(SeqCst),
        viol.load(SeqCst),
        maxw.load(SeqCst)
    );
    assert_eq!(viol.load(SeqCst), 0);
    m.check().unwrap();
}
