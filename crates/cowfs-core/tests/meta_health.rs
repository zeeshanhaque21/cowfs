use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use cowfs_core::{Core, Options};

#[test]
fn metadata_sync_failures_are_visible_through_core_health_and_remain_after_retry() {
    let dir = tempfile::tempdir().unwrap();
    let armed = Arc::new(AtomicBool::new(false));
    let flag = armed.clone();
    let c = Core::open_with_meta(
        dir.path(),
        Options {
            background: false,
            ..Options::default()
        },
        move |d, mut o| {
            o.background = false;
            let store_sync = o.before_sync.take().unwrap();
            o.before_sync = Some(Arc::new(move || {
                if flag.load(Ordering::SeqCst) {
                    return Err(std::io::Error::other("metadata health sentinel"));
                }
                store_sync()
            }));
            cowfs_meta::Meta::open(d.join("meta.redb"), o)
        },
    )
    .unwrap();
    c.create_snapshot("s").unwrap();
    c.sync().unwrap();
    assert!(c.health().last_error.is_none());
    assert!(c.last_flush_error().is_none());

    armed.store(true, Ordering::SeqCst);
    let first = c.meta().sync();
    let second = c.meta().sync();
    armed.store(false, Ordering::SeqCst);
    assert!(first.is_err());
    assert!(second.is_err());
    let metadata = c.meta().health();
    assert_eq!(metadata.flush_failures, 2);
    assert_eq!(metadata.consecutive_flush_failures, 2);
    let expected = metadata.last_flush_error.unwrap();
    assert!(expected.contains("metadata health sentinel"));
    assert_eq!(c.health().last_error.as_deref(), Some(expected.as_str()));
    assert_eq!(c.last_flush_error().as_deref(), Some(expected.as_str()));

    c.meta().sync().unwrap();
    assert_eq!(c.meta().health().consecutive_flush_failures, 0);
    assert_eq!(c.health().last_error.as_deref(), Some(expected.as_str()));
    assert_eq!(c.last_flush_error().as_deref(), Some(expected.as_str()));
    c.check().unwrap();
}
