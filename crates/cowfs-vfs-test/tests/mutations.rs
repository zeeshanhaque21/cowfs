//! Fast mutation subset for the default test run: each fault must make at least one of the
//! checks meant to catch it fail, and the reference must pass those same checks.
//! Every fault against the whole suite lives in `mutations_full` (feature `mutation-tests`).

use std::sync::Arc;

use cowfs_vfs::Vfs;
use cowfs_vfs_test::conformance::run_named;
use cowfs_vfs_test::{Fault, MemVfs};

const FAST: &[(Fault, &[&str])] = &[
    (
        Fault::InodeCookies,
        &["readdir_delete_returned_entries_between_pages"],
    ),
    (
        Fault::PositionCookies,
        &["readdir_delete_returned_entries_between_pages"],
    ),
    (Fault::NoHardlinkNlink, &["hardlink_nlink_counts_names"]),
    (Fault::RenameNoReplace, &["rename_file_over_file"]),
    (Fault::NoCtimeUpdate, &["setattr_bumps_ctime"]),
    (
        Fault::SymlinkSetattrFollows,
        &["setattr_times_on_symlink_never_touch_target"],
    ),
    (Fault::DirParentStale, &["rename_ancestor_into_moved_dir"]),
    (Fault::StaleNlinkLookup, &["lookup_nlink_is_fresh"]),
    (Fault::UnlinkFreesOpen, &["unlink_while_open_keeps_data"]),
    (Fault::ShortRead32K, &["read_never_short_mid_file"]),
    (
        Fault::HidesDotUnderscore,
        &["appledouble_names_are_ordinary"],
    ),
    (Fault::UnlinkLeaksSpace, &["statfs_free_after_unlink"]),
    (
        Fault::PunchNoop,
        &["fallocate_punch_reads_zeros_keeps_size"],
    ),
    (
        Fault::AllocateShrinks,
        &["fallocate_allocate_and_keep_size"],
    ),
    (
        Fault::PunchChangesSize,
        &["fallocate_punch_reads_zeros_keeps_size"],
    ),
    (Fault::ZeroRangeNoExtend, &["fallocate_zero_range_modes"]),
    (Fault::FallocZeroLenOk, &["fallocate_errors"]),
    (
        Fault::FallocNoTimes,
        &["fallocate_content_change_bumps_mtime_and_ctime"],
    ),
];

#[test]
fn fast_faults_are_caught() {
    std::thread::scope(|s| {
        let hs: Vec<_> = FAST
            .iter()
            .map(|&(fault, checks)| {
                s.spawn(move || {
                    let broken = move || -> Arc<dyn Vfs> { Arc::new(MemVfs::with_fault(fault)) };
                    let good = || -> Arc<dyn Vfs> { Arc::new(MemVfs::new()) };
                    for c in checks {
                        assert!(run_named(c, &good).is_ok(), "{c} fails on the reference");
                    }
                    (fault, checks.iter().any(|c| run_named(c, &broken).is_err()))
                })
            })
            .collect();
        for h in hs {
            let (fault, caught) = h.join().expect("mutation run panicked");
            assert!(caught, "{fault:?} was not caught by any expected check");
        }
    });
}

#[test]
fn every_fault_is_listed() {
    assert!(
        Fault::ALL.len() >= 57,
        "Fault::ALL has {}",
        Fault::ALL.len()
    );
    let mut seen = std::collections::HashSet::new();
    for f in Fault::ALL {
        assert!(seen.insert(*f), "{f:?} listed twice in Fault::ALL");
    }
}
