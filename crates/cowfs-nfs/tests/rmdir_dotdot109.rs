//! pjdfstest `rmdir/12.t` over raw NFSv3 (#109).
mod common;

use common::*;
use cowfs_nfs::MountOptions;

/// POSIX rmdir: "." is EINVAL, ".." is ENOTEMPTY or EEXIST. Nothing is removed either way.
#[test]
fn rmdir_of_dot_is_inval_and_of_dotdot_is_notempty_or_exist() {
    let (_s, mut c) = serve(memfs(), MountOptions::default());
    let root = c.root.clone();
    let a = c.mkdir(&root, "a").1.unwrap();
    let b = c.mkdir(&a, "b").1.unwrap();
    assert_eq!(c.rmdir(&b, "."), INVAL, "rmdir b/.");
    let st = c.rmdir(&b, "..");
    assert!(st == NOTEMPTY || st == EXIST, "rmdir b/.. answered {st}");
    assert_eq!(c.lookup(&a, "b").0, OK, "b must survive");
}
