use crate::error::{CtlResult, ErrorCode};
use crate::handler::{ControlHandler, HolderGuard};
use crate::types::SnapshotInfo;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;

/// How a backend injects a holder for the racing test, and takes the same per-snapshot lock the
/// framework hands to `swap`, so a correct backend cannot miss it.
pub type AddHolder = dyn Fn(&str) + Send + Sync;

/// Runs the contract every `ControlHandler` must satisfy before a daemon is wired in.
///
/// A backend that fails any check must not be served by `cowfs serve`. `add_holder` is called with
/// the snapshot name; it must add a holder while holding the same lock `HolderGuard::lock()`
/// returns, exactly as a real adapter must serialise its own holder bookkeeping.
pub fn handler_conformance(
    handler: Arc<dyn ControlHandler>,
    add_holder: Arc<AddHolder>,
) -> CtlResult<()> {
    let names = ["conf_base", "conf_slot"];
    let created = handler
        .snapshot_create(crate::types::SnapshotCreate {
            name: names[0].into(),
            from: None,
        })
        .map(|s: SnapshotInfo| s.name)?;
    assert_eq!(created, names[0]);
    let slot = handler
        .snapshot_create(crate::types::SnapshotCreate {
            name: names[1].into(),
            from: Some(names[0].into()),
        })
        .map(|s: SnapshotInfo| s.name)?;
    assert_eq!(slot, names[1]);

    let held: Arc<Mutex<()>> = Arc::new(Mutex::new(()));
    let under_lock = Arc::new(Mutex::new(0u32));
    let probe = Arc::clone(&under_lock);
    let held_probe = Arc::clone(&held);
    // A compliant handler holds `guard.lock()` while it checks holders, so the lock is taken when
    // `holders` runs. Counting that kills a handler that checks before it locks.
    let for_probe = Arc::clone(&handler);
    let source = move |n: &str| {
        if held_probe.try_lock().is_err() {
            *probe.lock().unwrap_or_else(PoisonError::into_inner) += 1;
        }
        for_probe.holders(n)
    };

    // A holder present before the operation is `busy` and changes nothing.
    add_holder(names[1]);
    let g = HolderGuard::new(names[1], true, Arc::clone(&held), &source);
    if g.check_holders().is_ok() {
        return Err(failure(
            "the framework holder check passed with a holder present",
        ));
    }
    *under_lock.lock().unwrap_or_else(PoisonError::into_inner) = 0;
    if handler.swap(names[1], names[0], &g).is_ok() {
        return Err(failure("swap succeeded with a holder present"));
    }
    if *under_lock.lock().unwrap_or_else(PoisonError::into_inner) == 0 {
        return Err(failure(
            "swap checked holders without holding the guard lock, so a holder can slip in",
        ));
    }
    if handler
        .remove(
            names[1],
            &HolderGuard::new(names[1], true, Arc::clone(&held), &source),
        )
        .is_ok()
    {
        return Err(failure("remove succeeded with a holder present"));
    }
    if handler.holders(names[1])?.is_empty() {
        return Err(failure("the injected holder is not reported"));
    }
    assert!(
        handler.snapshot_list()?.iter().any(|s| s.name == names[1]),
        "a busy operation must not remove the snapshot"
    );

    // The same operations succeed with expect_no_holders false, and the swap is atomic: the
    // snapshot exists before and after, with the new parent.
    handler.remove(
        names[1],
        &HolderGuard::new(names[1], false, Arc::clone(&held), &source),
    )?;
    assert!(
        handler.snapshot_list()?.iter().all(|s| s.name != names[1]),
        "remove left the snapshot behind"
    );
    handler.snapshot_create(crate::types::SnapshotCreate {
        name: names[1].into(),
        from: Some(names[0].into()),
    })?;
    let swapped = handler.swap(
        names[1],
        names[0],
        &HolderGuard::new(names[1], false, Arc::clone(&held), &source),
    )?;
    assert_eq!(swapped.name, names[1]);
    assert_eq!(swapped.parent.as_deref(), Some(names[0]));
    let listed: Vec<_> = handler
        .snapshot_list()?
        .into_iter()
        .filter(|s| s.name == names[1])
        .collect();
    assert_eq!(
        listed.len(),
        1,
        "swap must leave exactly one snapshot under that name"
    );

    // A holder that appears after one swap and before the next must make the next one `busy`.
    add_holder(names[1]);
    if handler
        .swap(
            names[1],
            names[0],
            &HolderGuard::new(names[1], true, Arc::clone(&held), &source),
        )
        .is_ok()
    {
        return Err(failure("swap succeeded after a holder appeared"));
    }

    // Concurrent changes of one snapshot are serialised by the framework's per-snapshot lock, so
    // the snapshot set never shows a half-applied change and never loses the name.
    handler.snapshot_create(crate::types::SnapshotCreate {
        name: "conf_race".into(),
        from: Some(names[0].into()),
    })?;
    let h2 = Arc::clone(&handler);
    let h3 = Arc::clone(&handler);
    let held2 = Arc::clone(&held);
    let held3 = Arc::clone(&held);
    let t1 = thread::spawn(move || {
        let src = |n: &str| h2.holders(n);
        h2.swap(
            "conf_race",
            "conf_base",
            &HolderGuard::new("conf_race", true, held2, &src),
        )
    });
    let t2 = thread::spawn(move || {
        let src = |n: &str| h3.holders(n);
        h3.swap(
            "conf_race",
            "conf_base",
            &HolderGuard::new("conf_race", true, held3, &src),
        )
    });
    let _ = (t1.join().unwrap(), t2.join().unwrap());
    let racers: Vec<_> = handler
        .snapshot_list()?
        .into_iter()
        .filter(|s| s.name == "conf_race")
        .collect();
    assert!(
        racers.len() == 1,
        "concurrent swaps left {} snapshots named conf_race",
        racers.len()
    );
    handler.remove(
        "conf_race",
        &HolderGuard::new("conf_race", false, Arc::clone(&held), &source),
    )?;
    handler.remove(
        names[1],
        &HolderGuard::new(names[1], false, Arc::clone(&held), &source),
    )?;
    handler.remove(
        names[0],
        &HolderGuard::new(names[0], false, Arc::clone(&held), &source),
    )?;
    Ok(())
}

fn failure(why: &str) -> crate::error::CtlError {
    crate::error::CtlError::new(ErrorCode::Internal, format!("conformance: {why}"))
}
