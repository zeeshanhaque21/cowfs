//! The suite must fail for a backend that is wrong. Each `Fault` is one deliberate defect in
//! `MemVfs`; the suite has to notice every one, and name at least one of the checks that
//! are meant to catch it.

use std::sync::Arc;

use cowfs_vfs::Vfs;
use cowfs_vfs_test::conformance::{run_all, Options};
use cowfs_vfs_test::{Fault, MemVfs};

const EXPECTED: &[(Fault, &[&str])] = &[
    (
        Fault::InodeCookies,
        &[
            "hardlink_pairs_8000_listed_once_and_removed",
            "readdir_delete_returned_entries_between_pages",
        ],
    ),
    (
        Fault::PositionCookies,
        &[
            "readdir_delete_returned_entries_between_pages",
            "readdir_resume_from_removed_entry_cookie",
        ],
    ),
    (Fault::EofOffByOne, &["readdir_eof_flag_is_exact"]),
    (Fault::DotEntries, &["readdir_never_lists_dot_entries"]),
    (Fault::NoHardlinkNlink, &["hardlink_nlink_counts_names"]),
    (Fault::RenameNoReplace, &["rename_file_over_file"]),
    (
        Fault::LinkReplaces,
        &["hardlink_over_existing_name_is_exists"],
    ),
    (Fault::NoCtimeUpdate, &["setattr_bumps_ctime"]),
    (Fault::WriteNoMtime, &["write_updates_mtime_and_ctime"]),
    (Fault::UnlinkFreesOpen, &["unlink_while_open_keeps_data"]),
    (
        Fault::StaleNeverReclaims,
        &["unlink_while_open_reclaimed_after_release_and_forget"],
    ),
    (
        Fault::TruncateNoZeroFill,
        &["truncate_shrink_then_grow_zero_fills"],
    ),
    (
        Fault::ModeNotMasked,
        &["setattr_mode_is_masked", "create_masks_mode"],
    ),
    (Fault::ReadPadsEof, &["read_past_eof_is_empty"]),
    (
        Fault::SymlinkSetattrFollows,
        &["setattr_times_on_symlink_never_touch_target"],
    ),
];

#[test]
fn every_fault_is_caught() {
    std::thread::scope(|s| {
        let handles: Vec<_> = EXPECTED
            .iter()
            .map(|&(fault, expect)| {
                s.spawn(move || {
                    let factory = move || -> Arc<dyn Vfs> { Arc::new(MemVfs::with_fault(fault)) };
                    let report = run_all(&factory, &Options::default());
                    let failed: Vec<&str> =
                        report.failures().iter().map(|r| r.check.name).collect();
                    println!("{fault:?}: {} checks fail: {failed:?}", failed.len());
                    (
                        fault,
                        expect,
                        failed.iter().map(|s| (*s).to_string()).collect::<Vec<_>>(),
                    )
                })
            })
            .collect();
        for h in handles {
            let (fault, expect, failed) = h.join().expect("mutation run panicked");
            assert!(
                !failed.is_empty(),
                "{fault:?}: the suite did not notice the defect"
            );
            assert!(
                expect.iter().any(|e| failed.iter().any(|f| f == e)),
                "{fault:?}: none of {expect:?} failed, failing checks: {failed:?}"
            );
        }
    });
}

#[test]
fn the_reference_has_no_fault() {
    let factory = || -> Arc<dyn Vfs> { Arc::new(MemVfs::new()) };
    assert!(run_all(&factory, &Options::default()).passed());
}
