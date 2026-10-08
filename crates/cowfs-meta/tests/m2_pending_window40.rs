//! M2 (#40): a detected corrupt read poisons the handle, and the policy holds through drop.

mod common;

use common::backend::Be;
use cowfs_meta::*;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::Ordering::SeqCst;
use std::time::Duration;

fn quiet() -> Options {
    Options {
        background: false,
        sync_interval: Duration::from_secs(3600),
        ..Options::default()
    }
}

fn flaky_opts() -> Options {
    Options {
        node_size: 512,
        node_cache: 0,
        cache_size: 1 << 16,
        ..quiet()
    }
}

#[test]
fn corruption_poisons_the_handle_and_only_the_pending_window_is_lost() {
    let be = Be {
        flip_every: 3,
        ..Be::default()
    };
    let m = Meta::open_with_backend(be.clone(), flaky_opts()).unwrap();
    let s = m.new_snapshot("s").unwrap();
    let durable: Vec<String> = (0..300).map(|i| format!("f{i}")).collect();
    s.batch(|tx| {
        for name in &durable {
            tx.create(ROOT_INO, name.as_bytes(), 0o644)?;
        }
        Ok(())
    })
    .unwrap();
    m.sync().unwrap();

    s.create(ROOT_INO, b"pending", 0o644).unwrap();
    assert!(
        s.lookup(ROOT_INO, b"pending").is_ok(),
        "the unsynced create is not visible in the session"
    );

    be.flaky.store(true, SeqCst);
    let mut detected = false;
    for name in &durable {
        let r = catch_unwind(AssertUnwindSafe(|| s.lookup(ROOT_INO, name.as_bytes())));
        if matches!(r, Err(_) | Ok(Err(Error::Corrupt(_)))) {
            detected = true;
            break;
        }
    }
    be.flaky.store(false, SeqCst);
    assert!(detected, "no flipped read was detected");

    assert!(
        m.health().poisoned,
        "a detected corrupt read did not poison the handle"
    );
    assert!(
        matches!(s.create(ROOT_INO, b"after", 0o644), Err(Error::Corrupt(_))),
        "a poisoned handle accepted a write"
    );

    drop(s);
    drop(m);
    // Bounded loss by policy: a corrupt-seeing handle persists nothing, so "pending" is absent.
    let reopened = Meta::open_with_backend(Be::from_image(be.image()), quiet()).unwrap();
    reopened.check().unwrap();
    let page = reopened
        .snapshot("s")
        .unwrap()
        .readdir(ROOT_INO, 0, 1000)
        .unwrap();
    let mut got: Vec<Vec<u8>> = page.entries.into_iter().map(|e| e.name).collect();
    got.sort();
    let mut want: Vec<Vec<u8>> = durable.iter().map(|n| n.as_bytes().to_vec()).collect();
    want.sort();
    assert_eq!(
        got, want,
        "durable files changed, or the unsynced pending file survived"
    );
}
