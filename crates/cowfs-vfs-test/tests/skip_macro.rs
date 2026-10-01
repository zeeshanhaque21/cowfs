//! `conformance_tests!` with a skip list: the skipped check becomes an ignored test with the
//! reason, the skip-name test passes, and everything else still runs.

use std::sync::Arc;

use cowfs_vfs::Vfs;
use cowfs_vfs_test::{conformance_tests, MemVfs};

conformance_tests!(
    || -> Arc<dyn Vfs> { Arc::new(MemVfs::new()) },
    skip = {
        hardlink_pairs_8000_listed_once_and_removed => "exercises the skip list",
        readonly_mode_0444_writes_always_succeed => "exercises the skip list",
        readonly_mode_0400_writes_always_succeed => "exercises the skip list",
        hardlink_limit_reports_too_many_links => "exercises the skip list",
        concurrent_rename_unlink_lookup => "exercises the skip list",
        concurrent_readers_and_writers_of_one_file => "exercises the skip list",
        readdir_5000_entries => "exercises the skip list",
        roundtrip_sizes => "exercises the skip list"
    }
);
