use std::sync::Arc;

use cowfs_vfs::Vfs;
use cowfs_vfs_test::conformance::{all_checks, run_all, Options};
use cowfs_vfs_test::{conformance_tests, MemVfs};

conformance_tests!(|| -> Arc<dyn Vfs> { Arc::new(MemVfs::new()) });

#[test]
fn every_check_has_a_unique_name() {
    let checks = all_checks();
    let mut names: Vec<_> = checks.iter().map(|c| c.name).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), checks.len());
    assert!(checks.len() >= 100, "only {} checks", checks.len());
}

#[test]
fn run_all_prints_a_table_and_passes() {
    let factory = || -> Arc<dyn Vfs> { Arc::new(MemVfs::new()) };
    let report = run_all(&factory, &Options::default());
    println!("{}", report.table());
    assert!(report.passed(), "{}", report.table());
}
