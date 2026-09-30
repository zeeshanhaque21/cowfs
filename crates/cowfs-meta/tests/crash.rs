//! Crash injection through redb's `StorageBackend`.
//!
//! A recording backend logs every write, set_len and sync of a deterministic workload. Crash images
//! are rebuilt from the log at many points under three loss policies, reopened, checked, and
//! compared with the states the workload passed through.

mod common;

use common::{step, Rng, WState};
use cowfs_meta::{Meta, Options};
use redb::StorageBackend;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Debug, Clone)]
enum Ev {
    Write(u64, Vec<u8>),
    SetLen(u64),
    Sync,
}

#[derive(Debug, Default)]
struct Rec {
    data: Vec<u8>,
    log: Vec<Ev>,
}

#[derive(Debug, Clone, Default)]
struct Backend(Arc<Mutex<Rec>>);

fn apply(img: &mut Vec<u8>, ev: &Ev, torn: Option<usize>) {
    match ev {
        Ev::Write(off, data) => {
            let data = &data[..torn.unwrap_or(data.len()).min(data.len())];
            let end = *off as usize + data.len();
            if img.len() < end {
                img.resize(end, 0);
            }
            img[*off as usize..end].copy_from_slice(data);
        }
        Ev::SetLen(n) => img.resize(*n as usize, 0),
        Ev::Sync => {}
    }
}

impl Backend {
    fn from_image(img: Vec<u8>) -> Self {
        Self(Arc::new(Mutex::new(Rec {
            data: img,
            log: Vec::new(),
        })))
    }

    fn log(&self) -> Vec<Ev> {
        self.0.lock().unwrap().log.clone()
    }

    fn log_len(&self) -> usize {
        self.0.lock().unwrap().log.len()
    }
}

impl StorageBackend for Backend {
    fn len(&self) -> Result<u64, io::Error> {
        Ok(self.0.lock().unwrap().data.len() as u64)
    }

    fn read(&self, offset: u64, out: &mut [u8]) -> Result<(), io::Error> {
        let r = self.0.lock().unwrap();
        let end = offset as usize + out.len();
        if end > r.data.len() {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        out.copy_from_slice(&r.data[offset as usize..end]);
        Ok(())
    }

    fn set_len(&self, len: u64) -> Result<(), io::Error> {
        let mut r = self.0.lock().unwrap();
        let ev = Ev::SetLen(len);
        apply(&mut r.data, &ev, None);
        r.log.push(ev);
        Ok(())
    }

    fn sync_data(&self) -> Result<(), io::Error> {
        self.0.lock().unwrap().log.push(Ev::Sync);
        Ok(())
    }

    fn write(&self, offset: u64, data: &[u8]) -> Result<(), io::Error> {
        let mut r = self.0.lock().unwrap();
        let ev = Ev::Write(offset, data.to_vec());
        apply(&mut r.data, &ev, None);
        r.log.push(ev);
        Ok(())
    }
}

type Fingerprint = Vec<(u64, String, [u8; 32])>;

fn fingerprint(m: &Meta) -> Fingerprint {
    m.durable_snapshots()
        .unwrap()
        .into_iter()
        .map(|i| (i.id.0, i.name, *i.root.as_bytes()))
        .collect()
}

struct Mark {
    end: usize,
    durable: bool,
    state: Fingerprint,
}

fn opts() -> Options {
    Options {
        node_size: 512,
        sync_every_ops: 4,
        sync_interval: Duration::from_secs(3600),
        ..Options::default()
    }
}

fn record(seed: u64, ops: usize) -> (Vec<Ev>, Vec<Mark>) {
    let backend = Backend::default();
    let m = Meta::open_with_backend(backend.clone(), opts()).unwrap();
    let mut marks = vec![Mark {
        end: backend.log_len(),
        durable: true,
        state: fingerprint(&m),
    }];
    let mut prev = backend.log_len();
    m.new_snapshot("s0").unwrap();
    let after = |m: &Meta, marks: &mut Vec<Mark>, prev: &mut usize| {
        let log = backend.log();
        marks.push(Mark {
            end: log.len(),
            durable: log[*prev..].iter().any(|e| matches!(e, Ev::Sync)),
            state: fingerprint(m),
        });
        *prev = log.len();
    };
    after(&m, &mut marks, &mut prev);
    let (mut rng, mut st) = (Rng(seed), WState::new());
    for _ in 0..ops {
        step(&m, &mut rng, &mut st);
        after(&m, &mut marks, &mut prev);
    }
    let log = backend.log();
    (log, marks)
}

/// Which recorded states a reopened image may equal for a crash during event `p`.
fn allowed(marks: &[Mark], p: usize) -> Vec<&Fingerprint> {
    let done = marks.iter().rposition(|m| m.end <= p && m.durable);
    let mut out = Vec::new();
    if let Some(i) = done {
        out.push(&marks[i].state);
    }
    if let Some(m) = marks.iter().find(|m| m.end > p) {
        if m.durable {
            out.push(&m.state);
        }
    }
    out
}

fn reopen_and_verify(img: Vec<u8>, p: usize, marks: &[Mark], what: &str) -> Result<(), String> {
    let creating = p < marks[0].end;
    let m = match Meta::open_with_backend(Backend::from_image(img), opts()) {
        Ok(m) => m,
        Err(_) if creating => return Ok(()),
        Err(e) => return Err(format!("{what}: open failed: {e}")),
    };
    m.check()
        .map_err(|e| format!("{what}: check failed: {e}"))?;
    let got = fingerprint(&m);
    let ok = if creating && got.is_empty() {
        true
    } else {
        allowed(marks, p).iter().any(|s| **s == got)
    };
    if !ok {
        let matching: Vec<usize> = (0..marks.len())
            .filter(|&i| marks[i].state == got)
            .collect();
        let info: Vec<(usize, usize, bool)> = marks
            .iter()
            .enumerate()
            .filter(|(_, m)| m.end + 40 > p && m.end < p + 200)
            .map(|(i, m)| (i, m.end, m.durable))
            .collect();
        return Err(format!(
            "{what}: state after reopen is not an allowed transaction boundary; matches marks {matching:?}; nearby marks (idx,end,durable) {info:?}"
        ));
    }
    if let Some(s) = m.snapshots().unwrap().first() {
        let s = m.snapshot(&s.name).unwrap();
        let _ = s.create(cowfs_meta::ROOT_INO, b"after-crash", 0o644);
        m.check()
            .map_err(|e| format!("{what}: check after a post-crash write failed: {e}"))?;
    }
    Ok(())
}

fn crash_points(log: &[Ev], want: usize, rng: &mut Rng) -> Vec<usize> {
    let mut pts: Vec<usize> = (0..want).map(|_| rng.below(log.len() + 1)).collect();
    for (i, e) in log.iter().enumerate() {
        if matches!(e, Ev::Sync) {
            pts.push(i);
            pts.push(i + 1);
        }
    }
    pts.sort_unstable();
    pts.dedup();
    pts
}

fn run(seed: u64, ops: usize, points: usize) -> (usize, usize, Vec<String>) {
    let (log, marks) = record(seed, ops);
    let mut rng = Rng(seed ^ 0x9E37_79B9_7F4A_7C15);
    let pts = crash_points(&log, points, &mut rng);
    let (mut running, mut synced) = (Vec::new(), Vec::new());
    let mut pending: Vec<usize> = Vec::new();
    let mut fails = Vec::new();
    let mut opened = 0;
    let mut next = 0;
    for p in 0..=log.len() {
        while next < pts.len() && pts[next] == p {
            next += 1;
            let torn_len = match log.get(p) {
                Some(Ev::Write(_, d)) => Some(if rng.below(3) == 0 {
                    d.len()
                } else {
                    rng.below(d.len() + 1) / 512 * 512 + rng.below(2) * rng.below(512)
                }),
                _ => None,
            };
            let mut prefix = running.clone();
            if let Some(ev) = log.get(p) {
                apply(&mut prefix, ev, torn_len);
            }
            let mut mixed = synced.clone();
            for &i in &pending {
                if rng.below(2) == 0 || matches!(log[i], Ev::SetLen(_)) {
                    let t = match &log[i] {
                        Ev::Write(_, d) if rng.below(4) == 0 => Some(rng.below(d.len() + 1)),
                        _ => None,
                    };
                    apply(&mut mixed, &log[i], t);
                }
            }
            if let Some(ev) = log.get(p) {
                apply(&mut mixed, ev, torn_len);
            }
            let cases = [
                ("prefix", prefix),
                ("synced only", synced.clone()),
                ("synced plus random unsynced", mixed),
            ];
            for (name, img) in cases {
                opened += 1;
                if let Err(e) =
                    reopen_and_verify(img, p, &marks, &format!("seed {seed} point {p} {name}"))
                {
                    fails.push(e);
                }
            }
        }
        if let Some(ev) = log.get(p) {
            apply(&mut running, ev, None);
            if matches!(ev, Ev::Sync) {
                for &i in &pending {
                    apply(&mut synced, &log[i], None);
                }
                pending.clear();
            } else {
                pending.push(p);
            }
        }
    }
    (pts.len(), opened, fails)
}

#[test]
fn crash_images_reopen_at_a_transaction_boundary() {
    let (points, opened, fails) = run(1, 60, 220);
    eprintln!(
        "crash points {points}, images reopened {opened}, failures {}",
        fails.len()
    );
    assert!(
        fails.is_empty(),
        "{}",
        fails[..fails.len().min(5)].join("\n")
    );
    assert!(points >= 100);
}

#[test]
#[ignore = "heavy: cargo test -p cowfs-meta --release --test crash -- --ignored --nocapture"]
fn crash_images_many_seeds() {
    let (mut points, mut opened, mut all) = (0, 0, Vec::new());
    for seed in 2..12 {
        let (p, o, f) = run(seed, 120, 400);
        points += p;
        opened += o;
        all.extend(f);
    }
    eprintln!(
        "crash points {points}, images reopened {opened}, failures {}",
        all.len()
    );
    assert!(all.is_empty(), "{}", all[..all.len().min(5)].join("\n"));
}

#[test]
fn durable_commits_bound_the_loss() {
    let (log, marks) = record(7, 60);
    let last_durable = marks.iter().rposition(|m| m.durable).unwrap();
    let mut img = Vec::new();
    for e in &log[..marks[last_durable].end] {
        apply(&mut img, e, None);
    }
    let m = Meta::open_with_backend(Backend::from_image(img), opts()).unwrap();
    m.check().unwrap();
    assert!(marks[last_durable].state == fingerprint(&m));
    let lost = marks[last_durable..]
        .windows(2)
        .filter(|w| w[0].state != w[1].state)
        .count();
    assert!(lost < 4, "lost {lost} transactions with sync_every_ops = 4");
}
