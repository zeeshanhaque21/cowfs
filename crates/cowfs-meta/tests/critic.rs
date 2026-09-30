//! Review tests (from the critic of PR 25). `CRIT_STRIDE=1` crashes at every log event; the default of 4 keeps the suite fast.

mod common;

use common::backend::{apply, Be, Ev};

use common::{digest, step, Rng, WState};
use cowfs_meta::*;
use std::io;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Mutex};
use std::time::Duration;

type Fp = Vec<(u64, String, [u8; 32])>;

fn fp(m: &Meta) -> Fp {
    m.durable_snapshots()
        .unwrap()
        .into_iter()
        .map(|i| (i.id.0, i.name, *i.root.as_bytes()))
        .collect()
}

struct Mark {
    end: usize,
    durable: bool,
    fp: Fp,
    dg: Option<[u8; 32]>,
}

fn opts() -> Options {
    Options {
        node_size: 512,
        sync_every_ops: 4,
        sync_interval: Duration::from_secs(3600),
        ..Options::default()
    }
}

fn record(o: Options, work: impl FnOnce(&Meta, &mut dyn FnMut(&Meta))) -> (Vec<Ev>, Vec<Mark>) {
    let be = Be::default();
    let m = Meta::open_with_backend(be.clone(), o).unwrap();
    let mut marks = vec![Mark {
        end: be.log().len(),
        durable: true,
        fp: fp(&m),
        dg: Some(digest(&m)),
    }];
    let mut prev = be.log().len();
    let mut mk = |m: &Meta| {
        let log = be.log();
        marks.push(Mark {
            end: log.len(),
            durable: log[prev..].iter().any(|e| matches!(e, Ev::S(_))),
            fp: fp(m),
            dg: (m
                .snapshots()
                .unwrap()
                .into_iter()
                .map(|i| (i.id.0, i.name, *i.root.as_bytes()))
                .collect::<Fp>()
                == fp(m))
            .then(|| digest(m)),
        });
        prev = log.len();
    };
    work(&m, &mut mk);
    (be.log(), marks)
}

fn allowed(marks: &[Mark], p: usize) -> Vec<usize> {
    let mut out = Vec::new();
    if let Some(i) = marks.iter().rposition(|m| m.end <= p && m.durable) {
        out.push(i);
    }
    if let Some(i) = marks.iter().position(|m| m.end > p) {
        if marks[i].durable {
            out.push(i);
        }
    }
    out
}

fn shred(base: &[u8], evs: &[&Ev], sector: usize, rng: &mut Rng) -> Vec<u8> {
    let mut img = base.to_vec();
    let mut pieces: Vec<(u64, Vec<u8>)> = Vec::new();
    for ev in evs {
        match ev {
            Ev::L(n) => {
                if rng.below(2) == 0 {
                    let l = (*n).max(img.len() as u64);
                    apply(&mut img, &Ev::L(l));
                }
            }
            Ev::W(off, d) => {
                let mut o = *off as usize;
                let end = o + d.len();
                while o < end {
                    let next = ((o / sector) + 1) * sector;
                    let e = next.min(end);
                    if rng.below(2) == 0 {
                        pieces.push((o as u64, d[o - *off as usize..e - *off as usize].to_vec()));
                    }
                    o = e;
                }
            }
            Ev::S(_) => {}
        }
    }
    for i in (1..pieces.len()).rev() {
        pieces.swap(i, rng.below(i + 1));
    }
    for (o, d) in pieces {
        apply(&mut img, &Ev::W(o, d));
    }
    img
}

fn verify(img: Vec<u8>, p: usize, marks: &[Mark], any: bool, what: &str) -> Result<(), String> {
    let creating = p < marks[0].end;
    let m = match Meta::open_with_backend(Be::from_image(img.clone()), opts()) {
        Ok(m) => m,
        Err(_) if creating => return Ok(()),
        Err(e) if any => return recover(img, marks, what, &e.to_string()),
        Err(e) => return Err(format!("{what}: open failed: {e}")),
    };
    m.check().map_err(|e| format!("{what}: check: {e}"))?;
    let got = fp(&m);
    let cands: Vec<usize> = if any {
        (0..marks.len()).collect()
    } else {
        allowed(marks, p)
    };
    let hit = cands.iter().find(|&&i| marks[i].fp == got);
    let Some(&i) = hit else {
        if creating && got.is_empty() {
            return Ok(());
        }
        return Err(format!(
            "{what}: reopened state matches no allowed boundary (allowed {cands:?})"
        ));
    };
    if marks[i].dg.is_some_and(|d| d != digest(&m)) {
        return Err(format!(
            "{what}: roots equal but content digest differs at mark {i}"
        ));
    }
    if let Some(s) = m.snapshots().unwrap().first() {
        let s = m.snapshot(&s.name).unwrap();
        let (mut rng, mut st) = (Rng(p as u64 + 1), WState::new());
        let _ = s.create(ROOT_INO, b"after-crash", 0o644);
        for _ in 0..3 {
            step(&m, &mut rng, &mut st);
        }
        m.check()
            .map_err(|e| format!("{what}: check after post-crash writes: {e}"))?;
    }
    Ok(())
}

thread_local! {
    static RECOVER_FAILED_CLOSED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static RECOVERED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The lost-fsync case: open fails closed, `open_recover` must then give an earlier commit.
fn recover(img: Vec<u8>, marks: &[Mark], what: &str, why: &str) -> Result<(), String> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("lost.redb");
    std::fs::write(&path, &img).unwrap();
    let (m, rec) = match Meta::open_recover(&path, opts()) {
        Ok(v) => v,
        Err(e) => {
            // Damage recovery cannot repair (for example a file length that is not a valid
            // region layout): it must fail closed and leave the original bytes in place.
            return if std::fs::read(&path).unwrap() == img {
                RECOVER_FAILED_CLOSED.with(|c| c.set(c.get() + 1));
                Ok(())
            } else {
                Err(format!(
                    "{what}: open_recover failed ({e}) and changed the file"
                ))
            };
        }
    };
    m.check()
        .map_err(|e| format!("{what}: check after recovery: {e}"))?;
    let got = fp(&m);
    if !marks.iter().any(|mk| mk.fp == got) && !got.is_empty() {
        return Err(format!(
            "{what}: recovered state matches no committed boundary"
        ));
    }
    if !rec.rolled_back {
        return Err(format!(
            "{what}: open failed ({why}) but recovery reports no rollback"
        ));
    }
    RECOVERED.with(|c| c.set(c.get() + 1));
    Ok(())
}

fn crash_all(name: &str, log: &[Ev], marks: &[Mark], stride: usize, seed: u64) -> Vec<String> {
    let mut rng = Rng(seed);
    let (mut running, mut synced, mut synced_prev) = (Vec::new(), Vec::new(), Vec::new());
    let (mut pending, mut pending_prev): (Vec<usize>, Vec<usize>) = (Vec::new(), Vec::new());
    let mut fails = Vec::new();
    let mut opened = 0;
    for p in 0..=log.len() {
        let is_sync = matches!(log.get(p), Some(Ev::S(_)));
        if p % stride == 0 || is_sync || p == log.len() {
            let cur = log.get(p);
            let evs =
                |idx: &[usize]| -> Vec<&Ev> { idx.iter().map(|&i| &log[i]).chain(cur).collect() };
            let mut prefix = running.clone();
            if let Some(ev) = cur {
                apply(&mut prefix, ev);
            }
            let mut cases: Vec<(&str, Vec<u8>, bool)> = vec![
                ("prefix", prefix, false),
                ("synced-only", synced.clone(), false),
                (
                    "shred512",
                    shred(&synced, &evs(&pending), 512, &mut rng),
                    false,
                ),
                (
                    "shred4096",
                    shred(&synced, &evs(&pending), 4096, &mut rng),
                    false,
                ),
            ];
            let both: Vec<usize> = pending_prev.iter().chain(&pending).copied().collect();
            cases.push((
                "lost-last-fsync-shred512",
                shred(&synced_prev, &evs(&both), 512, &mut rng),
                true,
            ));
            for (n, img, any) in cases {
                opened += 1;
                if let Err(e) = verify(
                    img,
                    p,
                    marks,
                    any,
                    &format!("{name} p{p}/{} {n}", log.len()),
                ) {
                    fails.push(e);
                }
            }
        }
        if let Some(ev) = log.get(p) {
            apply(&mut running, ev);
            if matches!(ev, Ev::S(_)) {
                synced_prev = synced.clone();
                pending_prev = std::mem::take(&mut pending);
                for &i in &pending_prev {
                    apply(&mut synced, &log[i]);
                }
            } else {
                pending.push(p);
            }
        }
    }
    eprintln!(
        "{name}: {} lost-fsync images recovered with open_recover, {} failed closed with the file untouched",
        RECOVERED.with(std::cell::Cell::get),
        RECOVER_FAILED_CLOSED.with(std::cell::Cell::get)
    );
    eprintln!(
        "{name}: log {} events, marks {}, images {opened}, failures {}",
        log.len(),
        marks.len(),
        fails.len()
    );
    let mut kinds: std::collections::BTreeMap<String, usize> = Default::default();
    for f in &fails {
        let pol = f.split(' ').nth(2).unwrap_or("?").to_string();
        let kind = if f.contains("open failed") {
            "open refused"
        } else if f.contains("check") {
            "check failed"
        } else {
            "SILENT wrong state"
        };
        *kinds.entry(format!("{pol} / {kind}")).or_default() += 1;
    }
    eprintln!("{name}: failures by policy: {kinds:?}");
    fails
}

fn stride() -> usize {
    std::env::var("CRIT_STRIDE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(4)
}

#[test]
fn crash_every_event_random_workload() {
    let (log, marks) = record(opts(), |m, mk| {
        m.new_snapshot("s0").unwrap();
        mk(m);
        let (mut rng, mut st) = (Rng(4242), WState::new());
        for _ in 0..70 {
            step(m, &mut rng, &mut st);
            mk(m);
        }
    });
    let f = crash_all("random", &log, &marks, stride(), 1);
    assert!(f.is_empty(), "{}", f[..f.len().min(6)].join("\n"));
    assert!(
        RECOVERED.with(std::cell::Cell::get) > 0,
        "no lost-fsync image exercised open_recover"
    );
}

#[test]
fn crash_every_event_snapshot_rm_workload() {
    let (log, marks) = record(opts(), |m, mk| {
        let a = m.new_snapshot("A").unwrap();
        mk(m);
        for d in 0..6u32 {
            a.batch(|tx| {
                let dir = tx.mkdir(ROOT_INO, format!("d{d}").as_bytes(), 0o755)?;
                for i in 0..25u32 {
                    let f = tx.create(dir.ino, format!("f{i}").as_bytes(), 0o644)?;
                    let c = ChunkRef {
                        id: BlockId::of(&[d as u8, i as u8]),
                        len: 3,
                    };
                    tx.set_content(f.ino, &[c], 3)?;
                }
                Ok(())
            })
            .unwrap();
            mk(m);
        }
        let b = a.fork("B").unwrap();
        mk(m);
        b.create(ROOT_INO, b"only-in-b", 0o644).unwrap();
        mk(m);
        let a_id = a.id();
        m.remove_snapshot(a_id).unwrap();
        mk(m);
        let c = b.fork("C").unwrap();
        mk(m);
        c.unlink(ROOT_INO, b"only-in-b").unwrap();
        mk(m);
        m.remove_snapshot(b.id()).unwrap();
        mk(m);
    });
    let f = crash_all("rm", &log, &marks, stride(), 2);
    assert!(f.is_empty(), "{}", f[..f.len().min(6)].join("\n"));
}

/// F13: crash at every event while a removed snapshot is freed in several small steps.
#[test]
fn crash_every_event_reap_steps() {
    let o = Options {
        background: false,
        ..opts()
    };
    let (log, marks) = record(o, |m, mk| {
        let a = m.new_snapshot("A").unwrap();
        mk(m);
        let b = m.new_snapshot("B").unwrap();
        b.create(ROOT_INO, b"keep", 0o644).unwrap();
        mk(m);
        a.batch(|tx| {
            for i in 0..400u32 {
                let f = tx.create(ROOT_INO, format!("f{i}").as_bytes(), 0o644)?;
                let c = ChunkRef {
                    id: BlockId::of(&i.to_le_bytes()),
                    len: 3,
                };
                tx.set_content(f.ino, &[c], 3)?;
            }
            Ok(())
        })
        .unwrap();
        m.sync().unwrap();
        mk(m);
        m.remove_snapshot(a.id()).unwrap();
        mk(m);
        let mut steps = 0;
        while m.reap_step().unwrap() {
            steps += 1;
            mk(m);
        }
        assert!(steps >= 1, "the workload must need several reap steps");
        b.create(ROOT_INO, b"after", 0o644).unwrap();
        m.sync().unwrap();
        mk(m);
    });
    let f = crash_all("reap", &log, &marks, stride(), 3);
    assert!(f.is_empty(), "{}", f[..f.len().min(6)].join("\n"));
}

struct Fs {
    put: Mutex<Vec<BlockId>>,
    dur: Arc<AtomicUsize>,
}

impl Fs {
    fn put(&self, b: &[u8]) -> BlockId {
        let id = BlockId::of(b);
        self.put.lock().unwrap().push(id);
        id
    }
    fn sync(&self) {
        self.dur.store(self.put.lock().unwrap().len(), SeqCst);
    }
}

fn dangling(inside_batch: bool) -> (usize, usize) {
    let dur = Arc::new(AtomicUsize::new(0));
    let fs = Arc::new(Fs {
        put: Mutex::new(Vec::new()),
        dur: dur.clone(),
    });
    let be = Be {
        tag: dur,
        ..Be::default()
    };
    let f2 = fs.clone();
    let o = Options {
        sync_every_ops: 3,
        before_sync: Some(Arc::new(move || {
            f2.sync();
            Ok(())
        })),
        ..opts()
    };
    let m = Meta::open_with_backend(be.clone(), o).unwrap();
    let s = m.new_snapshot("s").unwrap();
    for i in 0..30u8 {
        if inside_batch {
            s.batch(|tx| {
                let id = fs.put(&[i]);
                let a = tx.create(ROOT_INO, format!("f{i}").as_bytes(), 0o644)?;
                tx.set_content(a.ino, &[ChunkRef { id, len: 1 }], 1)?;
                Ok(())
            })
            .unwrap();
        } else {
            let id = fs.put(&[i]);
            let a = s
                .create(ROOT_INO, format!("f{i}").as_bytes(), 0o644)
                .unwrap();
            s.set_content(a.ino, &[ChunkRef { id, len: 1 }], 1).unwrap();
        }
    }
    let log = be.log();
    let (mut img, mut bad, mut syncs) = (Vec::new(), 0, 0);
    for ev in &log {
        apply(&mut img, ev);
        if let Ev::S(tag) = ev {
            syncs += 1;
            let Ok(r) = Meta::open_with_backend(Be::from_image(img.clone()), opts()) else {
                continue;
            };
            let known: Vec<BlockId> = fs.put.lock().unwrap()[..*tag].to_vec();
            let Ok(sn) = r.snapshot("s") else { continue };
            let mut mk = Marker::new();
            let dangling = sn
                .live_blocks(&mut mk)
                .unwrap()
                .filter(|b| !known.contains(b.as_ref().unwrap()))
                .count();
            if dangling > 0 {
                bad += 1;
            }
        }
    }
    (bad, syncs)
}

#[test]
fn store_before_meta_ordering_normal_use() {
    let (bad, syncs) = dangling(false);
    eprintln!(
        "put outside batch: {bad} of {syncs} sync points expose chunks not durable in the store"
    );
    assert_eq!(bad, 0);
}

#[test]
fn store_before_meta_ordering_put_inside_batch() {
    let (bad, syncs) = dangling(true);
    eprintln!("put INSIDE batch closure: {bad} of {syncs} sync points expose chunks not durable in the store");
    assert_eq!(
        bad, 0,
        "dangling chunk refs: the hook runs before the closure"
    );
}

#[test]
fn idle_loss_and_inode_reuse() {
    let be = Be::default();
    let o = Options {
        sync_every_ops: 256,
        ..Options::default()
    };
    let m = Meta::open_with_backend(be.clone(), o.clone()).unwrap();
    let s = m.new_snapshot("s").unwrap();
    m.sync().unwrap();
    let mut last = 0;
    for i in 0..100u32 {
        last = s
            .create(ROOT_INO, format!("f{i}").as_bytes(), 0o644)
            .unwrap()
            .ino
            .0;
    }
    std::thread::sleep(Duration::from_millis(2500));
    let log = be.log();
    let mut img = Vec::new();
    let last_sync = log.iter().rposition(|e| matches!(e, Ev::S(_))).unwrap();
    for e in &log[..=last_sync] {
        apply(&mut img, e);
    }
    let r = Meta::open_with_backend(Be::from_image(img), o).unwrap();
    let sn = r.snapshot("s").unwrap();
    let n = sn.readdir(ROOT_INO, 0, 1000).unwrap().entries.len();
    eprintln!("100 creates, 2.5 s idle (interval 1 s), crash: {n} of 100 survive; no background flush exists");
    let again = sn.create(ROOT_INO, b"new", 0o644).unwrap().ino.0;
    eprintln!("last inode handed out before crash {last}; first inode after reopen {again}");
    assert!(n > 0, "timer never fired");
    assert!(
        again > last,
        "inode number {again} reused after crash (was {last})"
    );
}

fn try_digest(m: &Meta) -> Result<[u8; 32]> {
    let mut h = blake3::Hasher::new();
    for info in m.snapshots()? {
        h.update(info.name.as_bytes());
        let s = m.snapshot(&info.name)?;
        let mut work = vec![ROOT_INO];
        while let Some(d) = work.pop() {
            let mut cookie = 0;
            loop {
                let page = s.readdir(d, cookie, 64)?;
                for e in &page.entries {
                    let a = s.getattr(e.ino)?;
                    h.update(&e.name);
                    h.update(&e.ino.0.to_le_bytes());
                    h.update(&a.nlink.to_le_bytes());
                    h.update(&a.size.to_le_bytes());
                    match a.kind {
                        FileType::File => {
                            for c in s.chunks(e.ino)? {
                                h.update(c.id.as_bytes());
                            }
                        }
                        FileType::Symlink => {
                            h.update(&s.readlink(e.ino)?);
                        }
                        FileType::Dir => work.push(e.ino),
                    }
                }
                cookie = page.next_cookie;
                if page.end {
                    break;
                }
            }
        }
    }
    Ok(*h.finalize().as_bytes())
}

static PANICS: AtomicUsize = AtomicUsize::new(0);

#[test]
fn live_handle_fails_closed_under_read_corruption() {
    std::panic::set_hook(Box::new(|_| {
        PANICS.fetch_add(1, SeqCst);
    }));
    for flip_every in [500usize, 60, 13] {
        let be = Be {
            flip_every,
            ..Be::default()
        };
        let o = Options {
            node_size: 512,
            sync_every_ops: 1,
            node_cache: 0,
            cache_size: 1 << 16,
            ..Options::default()
        };
        let m = Meta::open_with_backend(be.clone(), o).unwrap();
        m.new_snapshot("s0").unwrap();
        let (mut rng, mut st) = (Rng(5), WState::new());
        for _ in 0..80 {
            step(&m, &mut rng, &mut st);
        }
        m.sync().unwrap();
        let golden = try_digest(&m).unwrap();
        be.flaky.store(true, SeqCst);
        PANICS.store(0, SeqCst);
        let (mut ok_right, mut ok_wrong, mut errs) = (0, 0, 0);
        for _ in 0..40 {
            match catch_unwind(AssertUnwindSafe(|| try_digest(&m))) {
                Ok(Ok(d)) if d == golden => ok_right += 1,
                Ok(Ok(_)) => ok_wrong += 1,
                _ => errs += 1,
            }
        }
        let read_panics = PANICS.load(SeqCst);
        let mut wr_err = 0;
        let mut wr_panic = 0;
        for _ in 0..150 {
            let r = catch_unwind(AssertUnwindSafe(|| {
                let s = m.snapshot("s0")?;
                s.create(ROOT_INO, &rng.next().to_le_bytes(), 0o644)
            }));
            match r {
                Ok(Ok(_)) => {}
                Ok(Err(_)) => wr_err += 1,
                Err(_) => wr_panic += 1,
            }
        }
        be.flaky.store(false, SeqCst);
        let same_handle_digest = catch_unwind(AssertUnwindSafe(|| try_digest(&m)));
        let same_handle_check = catch_unwind(AssertUnwindSafe(|| m.check()));
        let fresh = Meta::open_with_backend(
            Be::from_image(be.image()),
            Options {
                node_size: 512,
                ..Options::default()
            },
        );
        let fresh_check = fresh
            .as_ref()
            .map(|f| f.check().map_err(|e| e.to_string()))
            .map_err(|e| e.to_string());
        eprintln!(
            "flip 1/{flip_every} reads: reads ok-right {ok_right}, ok-WRONG {ok_wrong}, err {errs}, redb panics caught {read_panics}; writes err {wr_err} escaped-panic {wr_panic}; after heal same handle: digest {:?} check {:?}; fresh reopen check {:?}",
            same_handle_digest.as_ref().map(|r| r.as_ref().map(|_| "ok").map_err(|e| e.to_string())),
            same_handle_check.as_ref().map(|r| r.as_ref().map(|_| "ok").map_err(|e| e.to_string())),
            fresh_check
        );
        assert_eq!(ok_wrong, 0, "wrong data returned as success");
    }
}

#[test]
fn hook_reentry_deadlocks() {
    let cell: Arc<Mutex<Option<Meta>>> = Arc::new(Mutex::new(None));
    let c2 = cell.clone();
    let o = Options {
        sync_every_ops: 1,
        before_sync: Some(Arc::new(move || {
            if let Some(m) = c2.lock().unwrap().clone() {
                let _ = m.sync();
            }
            Ok(())
        })),
        ..Options::default()
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("x.redb");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let m = Meta::open(&path, o).unwrap();
        *cell.lock().unwrap() = Some(m.clone());
        let r = m.new_snapshot("s");
        tx.send(r.is_ok()).ok();
    });
    let r = rx.recv_timeout(Duration::from_secs(5));
    eprintln!("hook that calls Meta::sync: {:?}", r);
    assert!(
        r.is_ok(),
        "deadlock: hook re-entering the crate hangs the writer"
    );
}

/// F2: with a failing hook, neither `drop` nor `close` may make the unsynced changes durable.
#[test]
fn failing_hook_makes_nothing_durable_on_drop_or_close() {
    for explicit_close in [false, true] {
        let fail = Arc::new(AtomicBool::new(false));
        let f2 = fail.clone();
        let o = Options {
            background: false,
            before_sync: Some(Arc::new(move || {
                if f2.load(SeqCst) {
                    Err(io::Error::other("store gone"))
                } else {
                    Ok(())
                }
            })),
            ..Options::default()
        };
        let be = Be::default();
        {
            let m = Meta::open_with_backend(be.clone(), o.clone()).unwrap();
            let s = m.new_snapshot("s").unwrap();
            m.sync().unwrap();
            for i in 0..20 {
                s.create(ROOT_INO, format!("f{i}").as_bytes(), 0o644)
                    .unwrap();
            }
            fail.store(true, SeqCst);
            if explicit_close {
                assert!(matches!(m.close(), Err(Error::Hook(_))));
            }
        }
        let r = Meta::open_with_backend(Be::from_image(be.image()), Options::default()).unwrap();
        r.check().unwrap();
        let n = r
            .snapshot("s")
            .unwrap()
            .readdir(ROOT_INO, 0, 100)
            .unwrap()
            .entries
            .len();
        assert_eq!(
            n, 0,
            "unsynced creates became durable (close={explicit_close})"
        );
    }
}

/// F2: a healthy hook lets `close` and `drop` persist everything.
#[test]
fn close_and_drop_persist_when_the_hook_is_healthy() {
    for explicit_close in [false, true] {
        let be = Be::default();
        {
            let m = Meta::open_with_backend(
                be.clone(),
                Options {
                    background: false,
                    ..Options::default()
                },
            )
            .unwrap();
            let s = m.new_snapshot("s").unwrap();
            for i in 0..20 {
                s.create(ROOT_INO, format!("f{i}").as_bytes(), 0o644)
                    .unwrap();
            }
            if explicit_close {
                m.close().unwrap();
                assert!(matches!(
                    s.create(ROOT_INO, b"late", 0o644),
                    Err(Error::Closed)
                ));
            }
        }
        let r = Meta::open_with_backend(Be::from_image(be.image()), Options::default()).unwrap();
        r.check().unwrap();
        let s = r.snapshot("s").unwrap();
        assert_eq!(s.readdir(ROOT_INO, 0, 100).unwrap().entries.len(), 20);
        let ino = s.create(ROOT_INO, b"next", 0o644).unwrap().ino.0;
        assert!(
            ino >= 22,
            "clean close must not waste the inode counter down to a stale value: {ino}"
        );
    }
}

#[test]
fn batch_panic_rolls_back_and_unlocks() {
    let dir = tempfile::tempdir().unwrap();
    let m = Meta::open(dir.path().join("x.redb"), Options::default()).unwrap();
    let s = m.new_snapshot("s").unwrap();
    let before = s.root().unwrap();
    let r = catch_unwind(AssertUnwindSafe(|| {
        s.batch(|tx| -> Result<()> {
            tx.create(ROOT_INO, b"a", 0o644)?;
            panic!("boom")
        })
    }));
    assert!(r.is_err());
    assert_eq!(s.root().unwrap(), before);
    let s2 = s.clone();
    let h = std::thread::spawn(move || s2.create(ROOT_INO, b"b", 0o644).is_ok());
    assert!(h.join().unwrap());
    m.check().unwrap();
}

#[test]
fn semantics_probes() {
    let dir = tempfile::tempdir().unwrap();
    let m = Meta::open(dir.path().join("x.redb"), Options::default()).unwrap();
    let s = m.new_snapshot("s").unwrap();
    let f = s.create(ROOT_INO, b"f", 0o644).unwrap().ino;
    let cs: Vec<ChunkRef> = (0..3)
        .map(|i| ChunkRef {
            id: BlockId::of(&[i]),
            len: 100,
        })
        .collect();
    s.set_content(f, &cs, 300).unwrap();
    eprintln!(
        "setattr(size=250) on 3x100 chunks: {:?}",
        s.setattr(
            f,
            SetAttr {
                size: Some(250),
                ..SetAttr::default()
            }
        )
        .map(|_| ())
    );
    eprintln!(
        "setattr(size=200): {:?}",
        s.setattr(
            f,
            SetAttr {
                size: Some(200),
                ..SetAttr::default()
            }
        )
        .map(|a| a.size)
    );
    eprintln!(
        "lookup in missing dir inode: {:?}; lookup missing name: {:?}",
        s.lookup(Ino(999), b"x").err(),
        s.lookup(ROOT_INO, b"nope").err()
    );
    eprintln!("link dir: {:?}", s.link(ROOT_INO, ROOT_INO, b"self").err());
    eprintln!(
        "name 255: {:?}, 256: {:?}",
        s.create(ROOT_INO, &[b'a'; 255], 0o644).map(|_| ()),
        s.create(ROOT_INO, &[b'a'; 256], 0o644).map(|_| ()).err()
    );
    eprintln!(
        "non-utf8 name: {:?}",
        s.create(ROOT_INO, &[0xff, 0xfe, b'x'], 0o644).map(|_| ())
    );
    eprintln!(
        "set_content size u64::MAX: {:?}",
        s.set_content(f, &cs, u64::MAX).map(|a| a.size)
    );
    for n in ["a", "b", "c", "d"] {
        s.create(ROOT_INO, n.as_bytes(), 0o644).unwrap();
    }
    let p1 = s.readdir(ROOT_INO, 0, 2).unwrap();
    let mut seen = p1
        .entries
        .iter()
        .map(|e| e.name.clone())
        .collect::<Vec<_>>();
    let mut cookie = p1.next_cookie;
    let mut renames = 0;
    for _ in 0..200 {
        let pg = s.readdir(ROOT_INO, cookie, 1).unwrap();
        let Some(e) = pg.entries.first() else { break };
        let new = [e.name.clone(), b"x".to_vec()].concat();
        if new.len() < 200 {
            s.rename(ROOT_INO, &e.name, ROOT_INO, &new).unwrap();
            renames += 1;
        }
        cookie = e.cookie;
        seen.push(e.name.clone());
    }
    eprintln!("rename-every-entry-seen listing after 200 iterations: {renames} renames, listing not finished (livelock) = {}", s.readdir(ROOT_INO, cookie, 1).unwrap().entries.len() == 1);
    let sa = m.new_snapshot("a").unwrap();
    let fa = sa.create(ROOT_INO, b"only-a", 0o644).unwrap().ino;
    let sb = sa.fork("b").unwrap();
    let fb = sb.create(ROOT_INO, b"only-b", 0o644).unwrap().ino;
    eprintln!("inode numbers after fork: a {fa:?} b {fb:?}");
    let a1 = s.create(ROOT_INO, b"cas", 0o644).unwrap().ino;
    let c0 = ChunkRef {
        id: BlockId::of(b"base"),
        len: 1,
    };
    s.set_content(a1, &[c0], 1).unwrap();
    let (s1, s2) = (s.clone(), s.clone());
    let l1 = s1.chunks(a1).unwrap();
    let l2 = s2.chunks(a1).unwrap();
    let mut n1 = l1.clone();
    n1.push(ChunkRef {
        id: BlockId::of(b"writer1"),
        len: 1,
    });
    let mut n2 = l2.clone();
    n2.push(ChunkRef {
        id: BlockId::of(b"writer2"),
        len: 1,
    });
    s1.set_content(a1, &n1, 2).unwrap();
    s2.set_content(a1, &n2, 2).unwrap();
    let fin = s.chunks(a1).unwrap();
    eprintln!(
        "two read-modify-write of one file: final chunk list has writer1 = {}",
        fin.iter().any(|c| c.id == BlockId::of(b"writer1"))
    );
}
