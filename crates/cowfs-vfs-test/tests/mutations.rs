//! Fast mutation subset for the default test run: each fault must make at least one of the
//! checks meant to catch it fail, and the reference must pass those same checks.
//! Every fault against the whole suite lives in `mutations_full` (feature `mutation-tests`).

use std::sync::Arc;

use cowfs_vfs::Vfs;
use cowfs_vfs_test::conformance::{run_all, run_named, Options};
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
    (Fault::MknodDropsRdev, &["mknod_device_attrs_keep_rdev"]),
    (Fault::MknodDeviceDenied, &["mknod_device_attrs_keep_rdev"]),
    (Fault::SpecialReadOk, &["special_io_is_invalid"]),
    (Fault::MknodNoParentTimes, &["mknod_fifo_attrs"]),
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

/// A kernel without `CAP_MKNOD` answers `PermissionDenied` to a device. The fallback to the fifo
/// and the socket follows only the host probe's answer (`no_device_privilege`), never who this
/// process claims to be: uid 0 in a user namespace has no `CAP_MKNOD` either (issue #211).
#[test]
fn device_permission_denied_falls_back_only_without_privilege() {
    let denied = || -> Arc<dyn Vfs> { Arc::new(MemVfs::with_fault(Fault::MknodDeviceDenied)) };
    for no_device_privilege in [true, false] {
        let opts = Options {
            filter: Some("mknod_device_attrs_keep_rdev".into()),
            no_device_privilege,
            ..Options::default()
        };
        let report = run_all(&denied, &opts);
        assert_eq!(
            report.passed(),
            no_device_privilege,
            "no_device_privilege = {no_device_privilege}:\n{}",
            report.table()
        );
    }
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
