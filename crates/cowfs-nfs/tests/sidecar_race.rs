//! Adversarial cases for the sidecar translator and the reference accounting, from the round 2
//! review: concurrent writes, concurrent unlinks of the last two names, inode reuse behind a
//! sidecar handle, and many attributes on one file.
mod common;

use std::sync::{Arc, Barrier};

use common::reuse::ReusingVfs;
use common::*;
use cowfs_nfs::{Adapter, AdapterOptions, AppleDoubleMode, MountOptions, Sidecar};
use cowfs_vfs::{Vfs, XattrFlags, ROOT_INO};
use nfsserve::nfs::sattr3;

fn translated() -> MountOptions {
    MountOptions {
        appledouble: AppleDoubleMode::Translate,
        ..MountOptions::default()
    }
}

fn adapter() -> Arc<Adapter> {
    Arc::new(
        Adapter::new(
            memfs(),
            AdapterOptions {
                appledouble: AppleDoubleMode::Translate,
                ..AdapterOptions::default()
            },
        )
        .unwrap(),
    )
}

/// A sidecar holding `n` attributes of 100 bytes each.
fn sidecar_with(n: usize) -> Vec<u8> {
    let mut s = Sidecar::default();
    for i in 0..n {
        s.attrs
            .insert(format!("user.k{i:03}").into_bytes(), vec![i as u8; 100]);
    }
    s.encode()
}

#[test]
fn concurrent_chunk_writers_lose_nothing() {
    let mut lost = 0;
    let rounds: usize = std::env::var("COWFS_SIDECAR_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(60);
    for r in 0..rounds {
        let a = adapter();
        a.create(ROOT_INO, b"f", &sattr3::default(), true).unwrap();
        let (sid, _) = a
            .create(ROOT_INO, b"._f", &sattr3::default(), true)
            .unwrap();
        let img = sidecar_with(30);
        let chunk = 1024;
        let mut hs = vec![];
        for (i, c) in img.chunks(chunk).enumerate() {
            let a = a.clone();
            let c = c.to_vec();
            hs.push(std::thread::spawn(move || {
                a.write(sid, (i * chunk) as u64, &c).unwrap();
            }));
        }
        for h in hs {
            h.join().unwrap();
        }
        let (back, _) = a.read(sid, 0, 1 << 20).unwrap();
        if back != img {
            lost += 1;
            if lost == 1 {
                eprintln!(
                    "round {r}: image differs, {} vs {} bytes",
                    back.len(),
                    img.len()
                );
            }
        }
    }
    eprintln!("LOST rounds {lost}/{rounds}");
    assert_eq!(lost, 0, "concurrent partial writes were lost");
}

#[test]
fn concurrent_unlinks_of_the_last_two_links_bury_the_inode() {
    let rounds: usize = std::env::var("COWFS_SIDECAR_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(300);
    let mut missed = 0;
    for _ in 0..rounds {
        let a = Arc::new(
            Adapter::new(
                ReusingVfs::new(),
                AdapterOptions {
                    appledouble: AppleDoubleMode::Translate,
                    ..AdapterOptions::default()
                },
            )
            .unwrap(),
        );
        let (ino, _) = a.create(ROOT_INO, b"a", &sattr3::default(), true).unwrap();
        a.link(ino, ROOT_INO, b"l").unwrap();
        let h = a.handle(ino);
        let bar = Arc::new(Barrier::new(2));
        let ts: Vec<_> = [b"a".to_vec(), b"l".to_vec()]
            .into_iter()
            .map(|n| {
                let (a, bar) = (a.clone(), bar.clone());
                std::thread::spawn(move || {
                    bar.wait();
                    a.remove(ROOT_INO, &n).unwrap();
                })
            })
            .collect();
        for t in ts {
            t.join().unwrap();
        }
        if a.resolve(&h).is_ok() {
            missed += 1;
        }
    }
    eprintln!("RACE inode not buried after both links removed: {missed}/{rounds}");
    assert_eq!(missed, 0, "a handle to a removed file stayed valid");
}

#[test]
fn a_sidecar_handle_of_a_reused_inode_is_stale() {
    let vfs = ReusingVfs::new();
    let (_s, mut c) = serve(vfs.clone(), translated());
    let root = c.root.clone();
    let a = c.create_file(&root, "a");
    let side = c.create_file(&root, "._a");
    assert_eq!(c.remove(&root, "a"), OK);
    let b = c.create_file(&root, "b");
    assert_eq!(
        a.data[8..16],
        b.data[8..16],
        "the backend reused the number"
    );
    let ino_b = c.attrs(&b).fileid;
    vfs.setxattr(ino_b, b"user.secret", b"hunter2", XattrFlags::default())
        .unwrap();

    let (st, data, _) = c.read(&side, 0, 65536);
    let leaked = data.windows(7).any(|w| w == b"hunter2");
    eprintln!("SIDE-HANDLE read: status {st}, leaked={leaked}");
    assert_eq!(st, STALE, "the old sidecar handle is stale");

    let mut evil = Sidecar::default();
    evil.attrs.insert(b"user.planted".to_vec(), b"x".to_vec());
    let wst = c.write(&side, 0, &evil.encode(), 2).0;
    let planted = vfs.getxattr(ino_b, b"user.planted").is_ok();
    eprintln!("SIDE-HANDLE write: status {wst}, planted on the new file={planted}");
    assert!(
        !planted,
        "a write through the old handle planted an attribute"
    );
    assert_eq!(c.getattr(&side).0, STALE);
}

#[test]
fn many_attributes_stay_visible_through_the_mount_protocol() {
    let vfs = memfs();
    let (_s, mut c) = serve(vfs.clone(), translated());
    let root = c.root.clone();
    c.create_file(&root, "many");
    let side = c.create_file(&root, "._many");
    for n in [0usize, 1, 150, 200, 255] {
        let st = c.write(&side, 0, &sidecar_with(n), 2).0;
        assert_eq!(st, OK, "{n} attributes must be accepted");
        let (st, back, _) = c.read(&side, 0, 1 << 20);
        assert_eq!(st, OK);
        let decoded = Sidecar::decode(&back).expect("what we wrote must parse back");
        assert_eq!(decoded.attrs.len(), n, "{n} attributes must come back");
        let ino = vfs.lookup(ROOT_INO, b"many").unwrap().ino;
        assert_eq!(vfs.listxattr(ino).unwrap().len(), n);
    }
}

#[test]
fn hostile_bytes_do_not_panic_or_allocate_wildly() {
    let mut seed = 0x1234_5678_9abc_def0_u64;
    let base = sidecar_with(5);
    let t = std::time::Instant::now();
    for _ in 0..200_000 {
        let mut b = base.clone();
        for _ in 0..6 {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let i = (seed >> 33) as usize % 200;
            b[i] = (seed >> 20) as u8;
        }
        let _ = Sidecar::decode(&b);
    }
    eprintln!("200k mutated decodes in {:?}", t.elapsed());
    for hdr in [0xffff_ffff_u32, 0x7fff_ffff, 0x8000_0000, 0] {
        let mut b = base.clone();
        for at in [30, 34, 38, 42, 46, 92, 96, 100, 120, 124] {
            b[at..at + 4].copy_from_slice(&hdr.to_be_bytes());
            let _ = Sidecar::decode(&b);
        }
    }
}
