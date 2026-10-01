mod common;
use common::*;
use cowfs_store::{oplog_start, oplog_take, BlockId, Error, LogOp, Op, Store};
use std::{
    fs,
    process::Command,
    time::{Duration, Instant},
};

#[test]
fn recovery_child() {
    let Ok(p) = std::env::var("C7D_STORE") else {
        return;
    };
    let mut o = opts();
    if std::env::var_os("C7D_SMALL").is_some() {
        o.max_pack_size = 1;
    }
    let result = Store::open(&p, o);
    if std::env::var_os("C7D_LOCKED").is_some() {
        assert!(matches!(result, Err(Error::Locked { .. })));
    } else {
        let s = result.unwrap();
        if std::env::var_os("C7D_ACK").is_some() {
            s.acknowledge_corruption().unwrap();
        }
    }
}

fn child(dir: &std::path::Path, extra: &[(&str, String)]) -> i32 {
    let mut c = Command::new(std::env::current_exe().unwrap());
    c.args(["--exact", "recovery_child"]).env("C7D_STORE", dir);
    for (key, value) in extra {
        c.env(key, value);
    }
    c.output().unwrap().status.code().unwrap()
}

fn damaged_tail() -> (tempfile::TempDir, Vec<u8>, Vec<u8>, usize) {
    let dir = tempfile::tempdir().unwrap();
    let a = random(1, 3000);
    let b = random(2, 3000);
    {
        let s = Store::open(dir.path(), opts()).unwrap();
        s.put(&a).unwrap();
        s.put(&b).unwrap();
        s.sync().unwrap();
    }
    let p = pack_path(dir.path(), 0);
    let mut bytes = fs::read(&p).unwrap();
    let records = parse_pack(&bytes);
    let at = records[1].1;
    bytes[at + REC_HDR] ^= 1;
    fs::write(&p, bytes).unwrap();
    fs::remove_file(dir.path().join("SYNCED")).unwrap();
    (dir, a, b, at)
}

#[test]
fn pending_marker_is_durable_before_cut() {
    let (dir, _, _, _) = damaged_tail();
    let trace = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let s = Store::open_traced(dir.path(), opts(), trace.clone()).unwrap();
    let ops = trace.lock().unwrap();
    let cut = ops
        .iter()
        .position(|op| matches!(op, Op::Truncate(_)))
        .unwrap();
    let sync = ops[..cut]
        .iter()
        .position(|op| matches!(op, Op::Sync(f) if f == "ACKED.tmp"))
        .expect("marker fsync missing before cut");
    let rename = ops[..cut]
        .iter()
        .position(|op| matches!(op, Op::Rename(_, f) if f == "ACKED"))
        .expect("marker rename missing before cut");
    assert!(sync < rename);
    assert!(
        ops[rename + 1..cut]
            .iter()
            .any(|op| matches!(op, Op::DirSync(_))),
        "marker directory fsync missing before cut"
    );
    drop(s);
}

#[test]
fn open_reserves_before_creating_a_pack() {
    let (dir, _, _, _) = damaged_tail();
    let mut o = opts();
    o.max_pack_size = 1;
    oplog_start();
    let s = Store::open(dir.path(), o).unwrap();
    let ops = oplog_take();
    let create = ops
        .iter()
        .position(|op| matches!(op, LogOp::Create { file } if file.ends_with("01.cpk")))
        .unwrap();
    assert!(ops[..create]
        .iter()
        .any(|op| matches!(op, LogOp::Write { file, .. } if file == "SYNCED")));
    assert!(ops[..create]
        .iter()
        .any(|op| matches!(op, LogOp::Sync { file } if file == "SYNCED")));
    drop(s);
}

#[test]
#[cfg(feature = "fault-injection")]
fn a_crashed_reservation_is_not_reused_or_reported_as_lost() {
    let dir = tempfile::tempdir().unwrap();
    let a = random(91, 3000);
    {
        let s = Store::open(dir.path(), opts()).unwrap();
        s.put(&a).unwrap();
        s.sync().unwrap();
    }
    assert_eq!(
        child(
            dir.path(),
            &[
                ("C7D_SMALL", "1".into()),
                ("C7D_EXIT_FILE", "SYNCED".into()),
                ("C7D_EXIT_LEN", "64".into()),
            ]
        ),
        77
    );
    assert!(!pack_path(dir.path(), 1).exists());
    let mut o = opts();
    o.max_pack_size = 4000;
    let s = Store::open(dir.path(), o).unwrap();
    assert_eq!(s.get(BlockId::of(&a)).unwrap(), a);
    s.put(&random(92, 3000)).unwrap();
    assert!(s.active_pack() >= 2);
    s.sync().unwrap();
    drop(s);
    let s = Store::open(dir.path(), o).unwrap();
    assert!(
        !s.recovery().has_corruption(),
        "an unused reservation became a loss: {:?}",
        s.recovery()
    );
}

#[test]
#[cfg(feature = "fault-injection")]
fn cut_crash_must_keep_pending_loss() {
    let (dir, a, b, at) = damaged_tail();
    assert_eq!(
        child(
            dir.path(),
            &[
                ("C7D_EXIT_FILE", "pack-00000000.cpk".into()),
                ("C7D_EXIT_LEN", at.to_string()),
            ]
        ),
        77
    );
    let s = Store::open(dir.path(), opts()).unwrap();
    assert_eq!(s.get(BlockId::of(&a)).unwrap(), a);
    println!(
        "lost={} corruption={}",
        s.get(BlockId::of(&b)).is_err(),
        s.recovery().has_corruption()
    );
    assert!(
        s.recovery().has_corruption(),
        "synced block lost, evidence forgotten after crash"
    );
}

#[test]
fn highwater_precedes_pack_create() {
    let dir = tempfile::tempdir().unwrap();
    let mut o = opts();
    o.max_pack_size = 4000;
    let s = Store::open(dir.path(), o).unwrap();
    s.put(&random(1, 3000)).unwrap();
    s.sync().unwrap();
    for gc in [false, true] {
        oplog_start();
        if gc {
            s.new_pack().unwrap();
        } else {
            s.put(&random(2, 3000)).unwrap();
        }
        let ops = oplog_take();
        let create = ops
            .iter()
            .position(|op| matches!(op, LogOp::Create { file } if file.ends_with(".cpk")))
            .unwrap();
        let write = ops[..create]
            .iter()
            .position(|op| matches!(op, LogOp::Write { file, .. } if file == "SYNCED"));
        let sync = ops[..create]
            .iter()
            .position(|op| matches!(op, LogOp::Sync { file } if file == "SYNCED"));
        println!("gc={gc} highwater write={write:?} sync={sync:?} create={create}");
        assert!(
            write.zip(sync).is_some_and(|(w, s)| w < s),
            "no highwater write+fsync before create"
        );
    }
}

#[test]
fn a_second_live_store_and_process_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(dir.path(), opts()).unwrap();
    let start = Instant::now();
    assert!(matches!(
        Store::open(dir.path(), opts()),
        Err(Error::Locked { .. })
    ));
    assert!(
        start.elapsed() < Duration::from_millis(350),
        "lock wait exceeded budget: {:?}",
        start.elapsed()
    );
    assert_eq!(child(dir.path(), &[("C7D_LOCKED", "1".into())]), 0);
    drop(s);
    Store::open(dir.path(), opts()).unwrap();
}

#[test]
fn rolled_back_watermark_keeps_loss_reported() {
    let dir = tempfile::tempdir().unwrap();
    let a = random(11, 3000);
    let b = random(12, 3000);
    {
        let s = Store::open(dir.path(), opts()).unwrap();
        s.put(&a).unwrap();
        s.sync().unwrap();
        s.put(&b).unwrap();
        s.sync().unwrap();
    }
    let path = dir.path().join("SYNCED");
    let mut wm = fs::read(&path).unwrap();
    let newest = (0..2)
        .max_by_key(|i| u64::from_le_bytes(wm[i * 32..i * 32 + 8].try_into().unwrap()))
        .unwrap();
    wm[newest * 32 + 24] ^= 0xff;
    fs::write(path, wm).unwrap();
    let path = pack_path(dir.path(), 0);
    let mut bytes = fs::read(&path).unwrap();
    let records = parse_pack(&bytes);
    bytes[records[1].1 + REC_HDR] ^= 1;
    fs::write(path, bytes).unwrap();
    for _ in 0..2 {
        let s = Store::open(dir.path(), opts()).unwrap();
        assert_eq!(s.get(BlockId::of(&a)).unwrap(), a);
        assert!(s.get(BlockId::of(&b)).is_err());
        assert!(
            s.recovery().has_corruption(),
            "rolled-back watermark hid a loss"
        );
    }
}

#[test]
#[cfg(feature = "fault-injection")]
fn crash_at_every_recovery_boundary() {
    let mut cases = 0;
    for mode in ["cut", "relocate", "short-header", "zero-header"] {
        let relocate = mode == "relocate";
        for key in ["C7D_EXIT_SYNC_N", "C7D_EXIT_BOUNDARY_N"] {
            let mut finished = false;
            for n in 1..=100 {
                let (dir, a, b, _) = damaged_tail();
                let c = random(3, 3000);
                if mode == "short-header" {
                    let bytes = fs::read(pack_path(dir.path(), 0)).unwrap();
                    install_wm(
                        dir.path(),
                        &[(0, &bytes[..12])],
                        None,
                        Some((0, bytes.len() as u64)),
                    );
                } else if mode == "zero-header" {
                    let path = pack_path(dir.path(), 0);
                    let mut bytes = fs::read(&path).unwrap();
                    bytes[..16].fill(0);
                    fs::write(path, bytes).unwrap();
                }
                if relocate {
                    let path = pack_path(dir.path(), 0);
                    let mut bytes = fs::read(&path).unwrap();
                    bytes.extend_from_slice(&record(
                        0,
                        c.len() as u32,
                        *BlockId::of(&c).as_bytes(),
                        &c,
                    ));
                    fs::write(path, bytes).unwrap();
                }
                let code = child(dir.path(), &[(key, n.to_string())]);
                assert!(matches!(code, 0 | 77), "child failed: {code}, {key}={n}");
                cases += 1;
                for _ in 0..2 {
                    let s = Store::open(dir.path(), opts()).unwrap();
                    if mode != "short-header" {
                        assert_eq!(s.get(BlockId::of(&a)).unwrap(), a, "{mode} {key}={n}");
                    }
                    if relocate {
                        assert_eq!(s.get(BlockId::of(&c)).unwrap(), c, "{key}={n}");
                    }
                    assert!(s.get(BlockId::of(&b)).is_err());
                    assert!(
                        s.recovery().has_corruption(),
                        "false clean: mode={mode} {key}={n}"
                    );
                }
                let s = Store::open(dir.path(), opts()).unwrap();
                s.acknowledge_corruption().unwrap();
                drop(s);
                let s = Store::open(dir.path(), opts()).unwrap();
                assert!(
                    !s.recovery().has_corruption(),
                    "acknowledgement did not survive {key}={n}"
                );
                if code == 0 {
                    finished = true;
                    break;
                }
            }
            assert!(finished, "boundary sweep exceeded limit");
        }
    }
    println!("recovery-boundary cases={cases}");
}

#[test]
#[cfg(feature = "fault-injection")]
fn crash_at_every_acknowledgement_boundary() {
    let mut finished = false;
    for n in 1..=100 {
        let (dir, a, _, _) = damaged_tail();
        drop(Store::open(dir.path(), opts()).unwrap());
        let code = child(
            dir.path(),
            &[
                ("C7D_ACK", "1".into()),
                ("C7D_EXIT_BOUNDARY_N", n.to_string()),
            ],
        );
        assert!(matches!(code, 0 | 77));
        let s = Store::open(dir.path(), opts()).unwrap();
        assert_eq!(s.get(BlockId::of(&a)).unwrap(), a);
        if !s.recovery().has_corruption() {
            let entries = fs::read(dir.path().join("ACKED")).unwrap();
            assert!(entries
                .as_chunks::<68>()
                .0
                .iter()
                .any(|e| e[8] == 1 && crc32c::crc32c(&e[..64]).to_le_bytes() == e[64..]));
        }
        if code == 0 {
            finished = true;
            println!("ack-boundary cases={n}");
            break;
        }
    }
    assert!(finished);
}
