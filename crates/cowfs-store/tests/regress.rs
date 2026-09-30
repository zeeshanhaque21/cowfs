mod common;

use std::fs::{self, OpenOptions};
use std::os::unix::fs::FileExt;
use std::time::{Duration, Instant};

use common::{index_bytes, index_path, opts, pack_path, random, record};
use cowfs_store::{BlockId, Options, Store};

#[test]
fn f1_put_never_acks_a_block_that_is_unreadable() {
    let dir = tempfile::tempdir().unwrap();
    let d = random(1, 5000);
    let id;
    {
        let s = Store::open(dir.path(), opts()).unwrap();
        id = s.put(&d).unwrap();
        s.put(&random(2, 5000)).unwrap();
        s.sync().unwrap();
        s.checkpoint().unwrap();
    }
    let f = OpenOptions::new()
        .write(true)
        .open(pack_path(dir.path(), 0))
        .unwrap();
    f.write_all_at(&[0xff], 16 + 52 + 100).unwrap();
    drop(f);
    let s = Store::open(dir.path(), opts()).unwrap();
    if s.put(&d).is_ok() {
        assert_eq!(s.get(id).unwrap(), d, "put acked but block unreadable");
    }
}

#[test]
fn f2_forged_inner_record_does_not_poison_an_id() {
    let dir = tempfile::tempdir().unwrap();
    let secret = random(77, 3000);
    let target = BlockId::of(&secret);
    let junk = random(78, 200);
    let inner = record(0, junk.len() as u32, *target.as_bytes(), &junk);
    let mut outer = random(79, 100);
    outer.extend_from_slice(&inner);
    outer.extend_from_slice(&random(80, 400));
    {
        let s = Store::open(dir.path(), opts()).unwrap();
        s.put(&outer).unwrap();
        s.sync().unwrap();
    }
    let p = pack_path(dir.path(), 0);
    let bytes = fs::read(&p).unwrap();
    let at = bytes
        .windows(4)
        .skip(16 + 52)
        .position(|w| w == b"CWRB")
        .unwrap()
        + 16
        + 52;
    fs::write(&p, &bytes[..at + inner.len() + 5]).unwrap();
    let s = Store::open(dir.path(), opts()).unwrap();
    assert!(!s.contains(target), "forged record was indexed");
    assert_eq!(s.put(&secret).unwrap(), target);
    assert_eq!(s.get(target).unwrap(), secret);
}

#[test]
fn f3_fake_header_flood_opens_quickly() {
    let dir = tempfile::tempdir().unwrap();
    {
        let s = Store::open(dir.path(), opts()).unwrap();
        s.put(b"x").unwrap();
        s.sync().unwrap();
    }
    let p = pack_path(dir.path(), 0);
    let mut hdr = b"CWRB".to_vec();
    hdr.extend_from_slice(&[0, 0, 0, 0]);
    hdr.extend_from_slice(&262143u32.to_le_bytes());
    hdr.extend_from_slice(&262143u32.to_le_bytes());
    hdr.extend_from_slice(&[0u8; 36]);
    let mut bytes = fs::read(&p).unwrap();
    for _ in 0..(4 << 20) / 52 {
        bytes.extend_from_slice(&hdr);
    }
    fs::write(&p, &bytes).unwrap();
    let t = Instant::now();
    let s = Store::open(dir.path(), opts()).unwrap();
    assert!(
        t.elapsed() < Duration::from_millis(1500),
        "open took {:?}",
        t.elapsed()
    );
    assert!(s.contains(BlockId::of(b"x")));
}

#[test]
fn f4_bit_flip_in_a_synced_record_never_deletes_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (random(1, 4000), random(2, 4000));
    let ia;
    {
        let s = Store::open(dir.path(), opts()).unwrap();
        ia = s.put(&a).unwrap();
        s.put(&b).unwrap();
        s.sync().unwrap();
    }
    let p = pack_path(dir.path(), 0);
    let before = fs::metadata(&p).unwrap().len();
    let f = OpenOptions::new().write(true).open(&p).unwrap();
    f.write_all_at(&[0xff], before - 10).unwrap();
    drop(f);
    let s = Store::open(dir.path(), opts()).unwrap();
    assert_eq!(
        fs::metadata(&p).unwrap().len(),
        before,
        "synced bytes were truncated"
    );
    assert_eq!(s.get(ia).unwrap(), a);
}

#[test]
fn f8_index_claiming_a_huge_record_is_rejected_before_allocation() {
    let dir = tempfile::tempdir().unwrap();
    let a = random(1, 4000);
    let ia;
    {
        let s = Store::open(dir.path(), opts()).unwrap();
        ia = s.put(&a).unwrap();
        s.sync().unwrap();
    }
    let p = pack_path(dir.path(), 0);
    let big: u64 = 1 << 30;
    OpenOptions::new()
        .write(true)
        .open(&p)
        .unwrap()
        .set_len(big)
        .unwrap();
    let slen = (big - 16 - 52) as u32;
    fs::write(
        index_path(dir.path()),
        index_bytes(&[(0, big)], &[(ia, [0, 16, slen, slen])]),
    )
    .unwrap();
    let s = Store::open(dir.path(), opts()).unwrap();
    assert!(s.stats().uncompressed_bytes < 1 << 20, "{:?}", s.stats());
    let t = Instant::now();
    let _ = s.get(ia);
    assert!(t.elapsed() < Duration::from_millis(50), "{:?}", t.elapsed());
}

const CHILD_ENV: &str = "COWFS_FD_CHILD_DIR";

#[test]
fn f5_many_packs_under_a_tiny_fd_limit() {
    let dir = tempfile::tempdir().unwrap();
    let exe = std::env::current_exe().unwrap();
    let out = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!(
            "ulimit -n 64 && exec '{}' --exact fd_child --ignored --nocapture",
            exe.display()
        ))
        .env(CHILD_ENV, dir.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
#[ignore]
fn fd_child() {
    let Some(dir) = std::env::var_os(CHILD_ENV) else {
        return;
    };
    let o = Options {
        max_pack_size: 1,
        checkpoint_on_drop: false,
        ..Options::default()
    };
    let mut ids = Vec::new();
    {
        let s = Store::open(&dir, o).unwrap();
        for i in 0..1500u64 {
            let d = random(i, 100);
            ids.push((s.put(&d).unwrap(), d));
        }
        s.sync().unwrap();
        for (id, d) in ids.iter().step_by(7) {
            assert_eq!(&s.get(*id).unwrap(), d);
        }
    }
    let s = Store::open(&dir, o).unwrap();
    assert_eq!(s.stats().packs, 1501);
    for (id, d) in &ids {
        assert_eq!(&s.get(*id).unwrap(), d);
    }
    assert!(s.fsck().unwrap().is_clean());
}
