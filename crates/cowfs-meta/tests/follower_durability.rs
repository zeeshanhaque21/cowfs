//! Port of the round-2 critic's follower/leader durable-ack harness (issue #40:
//! "follower acked before the leader's fsync ... port it").
//!
//! Recorded mutant: `follower-acked-early`. At the top of `wait_durable`, a follower that sees a
//! leader already running returns `Ok(())` instead of waiting for that leader's commit to become
//! durable (critic `mut/fol/crates/cowfs-meta/src/db.rs`: an `if *led { return Ok(()); }` before the
//! `gc_cv.wait_timeout`). It SURVIVED the builder suite (`mutate.log`: "follower-acked-early:
//! SURVIVED 154s") because no builder test checks whether a group-commit follower's own write is
//! durable when its `Ack::Durable` call returns.
//!
//! Invariant under test: an `Ack::Durable` call returns only after the change is durable, including
//! when it is a follower in a commit led by another caller. This harness asserts, at the exact
//! return instant of every call, that the created name is already present in the tree the file
//! holds (`durable_snapshots`/`snapshot`, what a crash right now would leave). Presence is
//! monotonic under later commits, so the assertion cannot false-fail when another writer advances
//! the root after this call returned. It also reopens from the crash image and requires every
//! acknowledged write to survive.
//!
//! Boundary note: the follower/leader split is decided inside `wait_durable` by a private lock in
//! `db.rs`. The deterministic, forced-follower proof is the crate-internal unit test
//! `db::tests::follower_wait_does_not_ack_before_the_leader_publishes_durable_seq`; this
//! integration test drives real concurrent durable callers through the shared-commit path and
//! checks the crash image, so it is the end-to-end port rather than the discriminating gate. The
//! harness does not sleep to fake a pass and it does not check only the leader.

use cowfs_meta::*;
use redb::StorageBackend;
use std::io;
use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Recording store whose blocks are durable only after `sync_data`; `synced_image` is what a crash
/// right now would leave (only fsynced writes).
#[derive(Clone, Debug)]
enum Ev {
    W(u64, Vec<u8>),
    L(u64),
    S,
}

#[derive(Default, Debug)]
struct Rec {
    data: Vec<u8>,
    log: Vec<Ev>,
}

#[derive(Clone, Default, Debug)]
struct Be {
    r: Arc<Mutex<Rec>>,
}

fn apply(img: &mut Vec<u8>, ev: &Ev) {
    match ev {
        Ev::W(off, d) => {
            let end = *off as usize + d.len();
            if img.len() < end {
                img.resize(end, 0);
            }
            img[*off as usize..end].copy_from_slice(d);
        }
        Ev::L(n) => img.resize(*n as usize, 0),
        Ev::S => {}
    }
}

impl StorageBackend for Be {
    fn len(&self) -> Result<u64, io::Error> {
        Ok(self.r.lock().unwrap().data.len() as u64)
    }
    fn read(&self, offset: u64, out: &mut [u8]) -> Result<(), io::Error> {
        let r = self.r.lock().unwrap();
        let end = offset as usize + out.len();
        if end > r.data.len() {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        out.copy_from_slice(&r.data[offset as usize..end]);
        Ok(())
    }
    fn set_len(&self, len: u64) -> Result<(), io::Error> {
        let mut r = self.r.lock().unwrap();
        let ev = Ev::L(len);
        apply(&mut r.data, &ev);
        r.log.push(ev);
        Ok(())
    }
    fn sync_data(&self) -> Result<(), io::Error> {
        self.r.lock().unwrap().log.push(Ev::S);
        Ok(())
    }
    fn write(&self, offset: u64, data: &[u8]) -> Result<(), io::Error> {
        let mut r = self.r.lock().unwrap();
        let ev = Ev::W(offset, data.to_vec());
        apply(&mut r.data, &ev);
        r.log.push(ev);
        Ok(())
    }
}

impl Be {
    fn from_image(img: Vec<u8>) -> Self {
        let b = Be::default();
        b.r.lock().unwrap().data = img;
        b
    }
    fn log(&self) -> Vec<Ev> {
        self.r.lock().unwrap().log.clone()
    }
    /// The image a crash leaves now: only writes that follow an fsync are kept.
    fn synced_image(&self) -> Vec<u8> {
        let mut img = Vec::new();
        let mut pending: Vec<Ev> = Vec::new();
        for ev in self.log() {
            if matches!(ev, Ev::S) {
                for e in pending.drain(..) {
                    apply(&mut img, &e);
                }
            } else {
                pending.push(ev);
            }
        }
        img
    }
}

fn quiet() -> Options {
    Options {
        background: false,
        sync_interval: Duration::from_secs(3600),
        ..Options::default()
    }
}

/// A real store sync model: yields once between "take the unsynced set" and "log the sync" so a
/// concurrent caller can enter the follower branch, exactly as the critic's `W::sync` did. It never
/// fails and never fabricates a delay long enough to be a sleep-based pass.
fn yielding_hook(calls: &Arc<AtomicUsize>) -> SyncHook {
    let calls = calls.clone();
    Arc::new(move || {
        calls.fetch_add(1, SeqCst);
        std::thread::yield_now();
        Ok(())
    })
}

/// The root of `name` as recorded in the file's last durable commit, or `None` if the snapshot is
/// not durable at all.
fn durable_root(m: &Meta, name: &str) -> Option<NodeId> {
    m.durable_snapshots()
        .unwrap()
        .into_iter()
        .find(|i| i.name == name)
        .map(|i| i.root)
}

/// True when `fname` is present in `name`'s tree as the file holds it right now.
///
/// Presence is monotonic under more commits, so this cannot false-fail the way comparing an exact
/// durable root can: a later commit from another writer advances the root but keeps every entry a
/// previous commit published. Under `follower-acked-early` a follower returns before the leader's
/// commit, so its name is absent and this fires.
fn durable_has(m: &Meta, name: &str, fname: &str) -> bool {
    let Ok(snap) = m.snapshot(name) else {
        return false;
    };
    match snap.readdir(ROOT_INO, 0, 10_000) {
        Ok(rd) => rd.entries.iter().any(|e| e.name == fname.as_bytes()),
        Err(_) => false,
    }
}

/// Every acknowledged `Ack::Durable` create is durable at the instant its call returns, even when
/// the commit was led by another caller (the follower path).
///
/// The authoritative, deterministic kill of `follower-acked-early` is the crate-internal
/// `db::tests::follower_wait_does_not_ack_before_the_leader_publishes_durable_seq`, which forces the
/// follower branch against a held leader. This test is the end-to-end port: it drives real
/// concurrent durable callers through the shared-commit path and reopens from the crash image.
#[test]
fn every_durable_ack_is_durable_on_return_including_followers() {
    let calls = Arc::new(AtomicUsize::new(0));
    let be = Be::default();
    let m = Meta::open_with_backend(
        be.clone(),
        Options {
            ack: Ack::Durable,
            before_sync: Some(yielding_hook(&calls)),
            ..quiet()
        },
    )
    .unwrap();

    let threads = 8usize;
    let per = 200usize;
    // Disjoint snapshots so each thread's create names its own snapshot; the commit is shared.
    let snaps: Vec<Snapshot> = (0..threads)
        .map(|t| m.new_snapshot(&format!("s{t}")).unwrap())
        .collect();

    let failures: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    std::thread::scope(|sc| {
        for (t, snap) in snaps.into_iter().enumerate() {
            let m = m.clone();
            let failures = failures.clone();
            let name = format!("s{t}");
            sc.spawn(move || {
                for i in 0..per {
                    let fname = format!("t{t}-{i}");
                    if snap.create(ROOT_INO, fname.as_bytes(), 0o644).is_err() {
                        break;
                    }
                    // At the instant the ack returned, this create must already be in the durable
                    // tree. Presence cannot false-fail from a later writer advancing the root.
                    if !durable_has(&m, &name, &fname) {
                        failures.lock().unwrap().push(format!(
                            "{name}/{fname}: ack returned before the create was durable"
                        ));
                        if failures.lock().unwrap().len() >= 5 {
                            return;
                        }
                    }
                }
            });
        }
    });

    let fails = failures.lock().unwrap().clone();
    assert!(
        fails.is_empty(),
        "{} durable acks returned while not durable (follower-acked-early); shared commits: {}, first: {:?}",
        fails.len(),
        calls.load(SeqCst),
        fails.iter().take(3).collect::<Vec<_>>()
    );
    // The point of the test: commits must actually have been shared, or the follower path was
    // never exercised and a green result would be meaningless.
    assert!(
        calls.load(SeqCst) < threads * per,
        "no commit was shared ({} hook runs for {} calls); follower path not exercised",
        calls.load(SeqCst),
        threads * per
    );

    // Crash image: every acknowledged create is still there after reopen.
    let reopened = Meta::open_with_backend(Be::from_image(be.synced_image()), quiet()).unwrap();
    reopened.check().unwrap();
    let total: usize = (0..threads)
        .map(|t| {
            reopened
                .snapshot(&format!("s{t}"))
                .unwrap()
                .readdir(ROOT_INO, 0, 10_000)
                .unwrap()
                .entries
                .len()
        })
        .sum();
    assert_eq!(
        total,
        threads * per,
        "an acknowledged create was lost from the crash image"
    );
}

/// A single durable caller must never acknowledge before the hook (its own sync) has run, and after
/// it returns the file's committed root must equal the applied root. This is the leader-side half
/// of the same invariant and pins the non-shared case.
#[test]
fn single_durable_ack_follows_the_hook() {
    let entered = Arc::new(AtomicUsize::new(0));
    let be = Be::default();
    let ent = entered.clone();
    let m = Meta::open_with_backend(
        be.clone(),
        Options {
            ack: Ack::Durable,
            before_sync: Some(Arc::new(move || {
                ent.fetch_add(1, SeqCst);
                std::thread::yield_now();
                Ok(())
            })),
            ..quiet()
        },
    )
    .unwrap();
    let s = m.new_snapshot("s").unwrap();
    for i in 0..50 {
        s.create(ROOT_INO, format!("f{i}").as_bytes(), 0o644)
            .unwrap();
        let applied = s.root().unwrap();
        assert_eq!(
            durable_root(&m, "s"),
            Some(applied),
            "durable ack returned before the write was durable"
        );
        assert!(entered.load(SeqCst) >= 1, "hook never ran");
    }
}
