# cowfs-vfs-path findings (#34)

`PathVfs` runs the conformance suite on native filesystems as a control and through the mounted FUSE and NFS adapters.
Round 3 uses the suite merged from `main` (133 checks: 56 Posix, 24 Portable, 53 Cowfs).
The native Posix runs and the 18 mutants were repeated after the round 3 fixes.
Every number below is from a run whose log was read.
Suite and adapter sources were not touched.
Single run per environment, except where a repeat count is given.

## Environments

| name | what | source |
|---|---|---|
| APFS | Mac data volume, default case-insensitive | - |
| APFS-cs | 4 GiB sparse image, `Case-sensitive APFS` (detached afterwards) | - |
| btrfs | OrbStack VM `cowfs-spike3`, VM root | - |
| ext4 | same VM, 3 GiB loop-mounted ext4 image | - |
| FUSE | same VM, `cowfs-fuse` (origin/v1/11-fuse 5a42c12) over a `MemVfs`, `PathVfs` rooted in the mount | scratch copy |
| NFS mount | Mac, `cowfs-nfs` (origin/v1/12-nfs 4fa0086) over a `MemVfs`, default options, `PathVfs` rooted in the mount | scratch copy |
| NFS raw | same server, no mount: the suite drives it through the raw NFSv3 RPC client of `cowfs-nfs/tests` wrapped as a `Vfs` (`NfsVfs`, scratch test, no xattr procedures because NFSv3 has none) | scratch copy |

The scratch copies live under `spikes/nfs-loopback/out/pathvfs/` with the current `cowfs-vfs-test` copied over their older one.
No adapter branch was modified.

## Posix level (56 checks)

Round 3, one run each with the current tree, heavy checks on, per-check timeout 300 s
(the harness sets it, `COWFS_CONFORMANCE_TIMEOUT_SECS` overrides):

| environment | ran | failed |
|---|---|---|
| APFS | 56 | 0 |
| btrfs | 56 | 0 |
| ext4 (loop image, unmounted afterwards) | 56 | 0 |

Round 2, older suite revision, one run each:

| environment | ran | failed |
|---|---|---|
| APFS-cs | 56 | 0 |
| FUSE mount | 56 | 0 |
| NFS mount | 56 | 10 |
| NFS raw, default (`AppleDoubleMode::Translate`) | 56 | 1 |
| NFS raw, `AppleDoubleMode::Store` | 56 | 0 |

No check tagged Posix fails on a native filesystem, in round 2 or round 3.
`hardlink_pairs_8000_listed_once_and_removed` is the one that used to flip: it is 39 to 60 s on
this loaded machine, against a 60 s default timeout, and the control harness now gives every
check 300 s (`tests/common/mod.rs`).
Root cause of the cost is the machine, not the crate: `std::fs::hard_link` costs about 900 us per
call in a 16,000 entry directory here, and `PathVfs` costs 850 us, so the check is at
filesystem speed. Paging a 16,000 entry directory is 0.16 s at page size 1 and 0.7 s at page
1000, after the per-page listing rebuild was removed.

NFS mount, the 10 Posix failures, all macOS client behaviour and none an adapter defect on the evidence here:
- Silly rename (an unlinked file that is still open stays as `.nfs.<id>` until closed, and `rmdir` of its directory is ENOTEMPTY): `mkdir_rmdir_errors`, `readdir_delete_returned_entries_between_pages`, `readdir_delete_upcoming_entries_between_pages`, `readdir_delete_everything_between_pages`, `rename_file_over_file`, `lookup_nlink_is_fresh`, `hardlink_unlink_one_other_survives`, `hardlink_across_directories`, `hardlink_pairs_8000_listed_once_and_removed`.
  The same 9 pass over the raw protocol, which holds no descriptors.
- `appledouble_names_are_ordinary`: the adapter's default `Translate` mode makes `._x` names invisible on purpose.
  It fails over the raw protocol in that mode and passes in `Store` mode.
  This is a documented adapter mode that contradicts a Posix check, so either the check needs a Cowfs tag or the default is wrong for a generic tree.
  That is the suite and NFS owners' call.

## Full level (127 checks, heavy included)

Counts are from the full runs, taken before the two `PathVfs` fixes of round 2.
After them `readdir_max_zero_is_invalid` passes on APFS, btrfs, ext4, FUSE and NFS mount (re-run, one check each), which removes one failure from each of those.
`xattr_name_validation` still fails on all of them except FUSE.
APFS-cs and NFS raw were not re-run after the fixes.

| environment | passed | failed |
|---|---|---|
| APFS | 109 | 18 |
| APFS-cs | 109 | 18 |
| btrfs | 110 | 17 |
| ext4 | 122 | 5 |
| FUSE | 124 | 3 |
| NFS mount | 95 | 32 |
| NFS raw | 105 | 22 |

Failing checks only.
The 50 rows below leave out `readdir_max_zero_is_invalid`, fixed in `PathVfs` and re-run green where it failed.
Class codes: S = the suite assumes something the filesystem does not do, Q = a real quirk of that filesystem, N = behaviour of the macOS NFS client, PV = `PathVfs` bug (none open).

| level | check | APFS | APFS-cs | btrfs | ext4 | FUSE | NFS mount | NFS raw | class and evidence |
|---|---|---|---|---|---|---|---|---|---|
| cowfs | `basic::root_is_directory` | ok | ok | FAIL | ok | ok | ok | ok | S: directory nlink formula (btrfs = 1) |
| cowfs | `basic::new_file_attrs` | ok | ok | FAIL | ok | ok | ok | ok | S: directory nlink formula (btrfs) |
| cowfs | `basic::dir_nlink_counts_subdirs` | FAIL | FAIL | FAIL | ok | ok | ok | ok | S: directory nlink (APFS = 2 + all children, btrfs = 1) |
| portable | `basic::statfs_sane` | ok | ok | FAIL | ok | ok | FAIL | ok | btrfs: S, `files` is 0. NFS mount: N, client caches statfs (probe4); NFS raw passes |
| cowfs | `basic::statfs_free_after_unlink` | FAIL | FAIL | FAIL | ok | ok | FAIL | ok | S: APFS and btrfs free counts do not move on unlink at once. NFS mount: N (client statfs cache) |
| portable | `io::sparse_write_far_past_eof` | ok | ok | ok | ok | ok | FAIL | ok | NFS mount: N. Raw GETATTR `used` is 4096 for a 1 GiB hole, so the adapter is right and the macOS client reports st_blocks = ceil(size/512) |
| portable | `io::write_updates_mtime_and_ctime` | ok | ok | ok | ok | ok | FAIL | ok | NFS mount: N, client write-back leaves mtime until fsync or close |
| portable | `attrs::setattr_bumps_ctime` | FAIL | FAIL | ok | ok | ok | ok | ok | Q: APFS does not bump ctime for an atime-only utimensat |
| portable | `attrs::namespace_ops_update_times` | ok | ok | ok | ok | ok | FAIL | ok | NFS mount: N, client attribute cache (passes with actimeo=0) |
| cowfs | `names::validate_name_cases` | ok | ok | ok | ok | ok | ok | FAIL | NFS raw: my NfsVfs maps NFS3ERR_EXIST for "." to Exists; the adapter does that on purpose (adapter.rs new_name). Not a bug |
| cowfs | `names::non_utf8_names` | FAIL | FAIL | ok | ok | ok | ok | ok | Q: APFS rejects invalid UTF-8 (EILSEQ) |
| cowfs | `names::names_are_exact_bytes` | FAIL | FAIL | ok | ok | ok | ok | ok | Q: APFS treats NFC and NFD as one name |
| cowfs | `names::invalid_names_rejected_by_creating_ops` | ok | ok | ok | ok | ok | ok | FAIL | NFS raw: same as validate_name_cases |
| posix | `names::appledouble_names_are_ordinary` | ok | ok | ok | ok | ok | FAIL | FAIL | NFS: adapter default `AppleDoubleMode::Translate` makes `._x` names invisible. Passes in Store mode (raw run, 56/56 Posix). Documented adapter behaviour that contradicts a Posix check |
| posix | `dirs::mkdir_rmdir_errors` | ok | ok | ok | ok | ok | FAIL | ok | NFS mount: N silly rename (`.nfs*` blocks rmdir) |
| cowfs | `dirs::rmdir_updates_parent` | FAIL | FAIL | FAIL | ok | ok | FAIL | FAIL | S: directory nlink (APFS, btrfs). NFS: getattr of a removed directory is STALE/ENOENT by protocol |
| cowfs | `dirs::deeply_nested_directories` | FAIL | FAIL | FAIL | ok | ok | ok | ok | S: directory nlink (APFS, btrfs) |
| cowfs | `dirs::create_in_removed_directory_fails` | ok | ok | ok | ok | ok | ok | FAIL | NFS raw: server answers STALE for a removed directory handle (RFC 1813), suite wants NotFound. Protocol-correct |
| posix | `readdir::readdir_delete_returned_entries_between_pages` | ok | ok | ok | ok | ok | FAIL | ok | NFS mount: N silly rename |
| posix | `readdir::readdir_delete_upcoming_entries_between_pages` | ok | ok | ok | ok | ok | FAIL | ok | NFS mount: N silly rename |
| posix | `readdir::readdir_delete_everything_between_pages` | ok | ok | ok | ok | ok | FAIL | ok | NFS mount: N silly rename |
| posix | `rename::rename_file_over_file` | ok | ok | ok | ok | ok | FAIL | ok | NFS mount: N silly rename of the replaced file |
| cowfs | `rename::rename_dir_over_empty_dir` | FAIL | FAIL | FAIL | ok | ok | FAIL | FAIL | S: directory nlink (APFS, btrfs). NFS: replaced directory is STALE or cached |
| cowfs | `rename::rename_dir_cross_directory_fixes_nlink` | FAIL | FAIL | FAIL | ok | ok | ok | ok | S: directory nlink (APFS, btrfs) |
| cowfs | `rename::rename_dir_across_parents_keeps_parent_links` | ok | ok | FAIL | ok | ok | ok | ok | S: directory nlink (btrfs) |
| portable | `rename::rename_no_replace` | ok | ok | ok | ok | ok | FAIL | ok | NFS mount: N, macOS client returns ENOTSUP for RENAME_EXCL onto a missing name |
| cowfs | `rename::rename_open_file_keeps_handle_working` | ok | ok | ok | ok | ok | FAIL | FAIL | NFS: needs an open handle, which NFSv3 does not have (mount: silly rename, raw: STALE) |
| portable | `rename::rename_updates_times` | ok | ok | ok | ok | ok | FAIL | ok | NFS mount: N, client attribute cache |
| cowfs | `links::hardlink_nlink_counts_names` | ok | ok | ok | ok | ok | FAIL | FAIL | NFS: needs unlink-while-open (see rename_open_file...) |
| posix | `links::lookup_nlink_is_fresh` | ok | ok | ok | ok | ok | FAIL | ok | NFS mount: N silly rename keeps a name |
| posix | `links::hardlink_unlink_one_other_survives` | ok | ok | ok | ok | ok | FAIL | ok | NFS mount: N silly rename |
| posix | `links::hardlink_across_directories` | ok | ok | ok | ok | ok | FAIL | ok | APFS/btrfs pass now; NFS mount: N silly rename |
| posix | `links::hardlink_pairs_8000_listed_once_and_removed` | ok | ok | ok | ok | ok | FAIL | ok | NFS mount: N silly rename (24000 removals, 16000 expected) |
| cowfs | `links::hardlink_limit_reports_too_many_links` | FAIL | FAIL | FAIL | FAIL | FAIL | FAIL | ok | S: native limits (ext4 65000 links) cannot be reached inside the 60 s timeout, leaked thread on every native run. NFS mount: EMLINK reached after 540 s |
| cowfs | `lifecycle::unlink_while_open_keeps_data` | ok | ok | ok | ok | ok | FAIL | FAIL | NFS: needs unlink-while-open (mount: silly rename, raw: STALE, NFSv3 is stateless) |
| cowfs | `lifecycle::unlink_while_open_reclaimed_after_release_and_forget` | ok | ok | ok | ok | ok | FAIL | FAIL | NFS: same |
| cowfs | `lifecycle::forget_keeps_inode_with_handle` | ok | ok | ok | ok | ok | FAIL | FAIL | NFS: same |
| cowfs | `lifecycle::rmdir_reclaimed_after_forget` | FAIL | FAIL | ok | ok | ok | FAIL | FAIL | APFS: Q, removed directory keeps nlink 2. NFS: no attributes for a removed directory (STALE/ENOENT) |
| cowfs | `lifecycle::two_handles_pin_until_last_release` | ok | ok | ok | ok | ok | ok | FAIL | NFS raw: no handles in NFSv3, `open` cannot pin |
| portable | `symlinks::symlink_size_is_target_length` | FAIL | FAIL | ok | ok | ok | FAIL | FAIL | S: 4095 byte target, macOS limit 1024 (adapter SYMLINK_TARGET_MAX = 1024 too) |
| portable | `xattrs::xattr_set_get_list_remove` | FAIL | FAIL | ok | ok | ok | FAIL | FAIL | Q: macOS adds com.apple.provenance to new files. NFS raw: my NfsVfs has no xattr procedures |
| portable | `xattrs::xattr_create_and_replace_flags` | ok | ok | ok | ok | ok | ok | FAIL | NFS raw: no xattr procedures in NFSv3 |
| portable | `xattrs::xattr_missing_is_no_attr` | ok | ok | ok | ok | ok | ok | FAIL | NFS raw: no xattr procedures in NFSv3 |
| cowfs | `xattrs::xattr_empty_and_large_values` | FAIL | FAIL | FAIL | FAIL | ok | FAIL | FAIL | S: 60,000 bytes is ENOSPC on ext4 and btrfs; macOS: com.apple.provenance in the list |
| portable | `xattrs::xattr_list_order_is_stable` | FAIL | FAIL | ok | ok | ok | FAIL | FAIL | Q: com.apple.provenance (11 names, want 10) |
| cowfs | `xattrs::xattr_on_directory_and_symlink` | ok | ok | FAIL | FAIL | FAIL | ok | FAIL | S: Linux refuses user.* xattrs on symlinks (EPERM); FUSE fails in the kernel |
| portable | `xattrs::xattr_shared_by_hardlinks` | ok | ok | ok | ok | ok | ok | FAIL | NFS raw: no xattr procedures in NFSv3 |
| cowfs | `xattrs::xattr_name_validation` | FAIL | FAIL | FAIL | FAIL | ok | FAIL | FAIL | S: a 255 byte name without a namespace prefix is ENOTSUP on Linux (probable cause, the failing step is the max-length setxattr; not isolated); macOS: com.apple.provenance in the list after the rejected calls |
| portable | `concurrency::concurrent_readers_and_writers_of_one_file` | ok | ok | FAIL | ok | ok | ok | ok | S: torn reads on native Linux at 512 and 4096 bytes (torn-probe.log), never on APFS |
| cowfs | `concurrency::concurrent_rename_unlink_lookup` | FAIL | FAIL | FAIL | ok | ok | ok | ok | S: directory nlink (APFS, btrfs) |

## Suite levels to change (the list for the suite builder)

No Posix-tagged check fails natively.
These non-Posix checks fail natively and their level is doubtful:

1. `concurrency::concurrent_readers_and_writers_of_one_file` is Portable.
   It fails on native ext4 and btrfs (6 of 10 repeated runs each) and never on APFS.
   The check uses 4096 byte, 4096 aligned reads and writes, so cross-page tearing is not the cause.
   `torn-probe.log` (3 writers, 3 readers, one file, 3 s per size, page size 4096):
   ext4 torn 0 of 1.1M reads at 8 bytes, 1 of 1.1M at 64, 191 of 818k at 512, 139 of 280k at 4096.
   btrfs 0, 2 of 1.2M, 297 of 816k, 96 of 317k.
   tmpfs 0, 0, 8 of 898k, 25 of 315k.
   APFS 0 at every size (about 300k to 470k reads each).
   The honest guarantee: POSIX text says a read is atomic against a concurrent write, Linux buffered I/O does not provide that for reads that overlap a write of 512 bytes or more, even within one page.
   Only very small (8 to 64 byte) accesses were not torn in about a million reads each.
   Proposed level: Cowfs.
2. `basic::statfs_sane` is Portable and fails on btrfs, which reports `files` 0 so `files_free` cannot drop.
   Proposed: tolerate `files == 0`, or Cowfs.
3. `xattrs::xattr_set_get_list_remove` and `xattr_list_order_is_stable` are Portable and fail on macOS because the OS adds `com.apple.provenance` to new files.
   Proposed: ignore `com.apple.*` names in those two assertions, or Cowfs.
4. `attrs::setattr_bumps_ctime` is Portable and fails on APFS, which does not bump ctime for an atime-only `utimensat`.
   Proposed: Cowfs, or assert the bump only when mtime or mode changes.
5. `symlinks::symlink_size_is_target_length` is Portable and fails on APFS with a 4095 byte target (limit 1024).
   The level text already says so, so it is properly leveled, but a variant with targets up to 1000 bytes would be Posix.
6. `xattrs::xattr_name_validation` is Cowfs and fails natively: the last step sets a 255 byte name without a namespace prefix.
   Probe (`xattr-name-probe.log`) on ext4 and btrfs: 255 bytes with no prefix is ENOTSUP, `user.` plus 250 bytes (255 total) works, `user.` plus 251 bytes is ERANGE.
   The cowfs contract says 1 to `NAME_MAX` bytes; a native filesystem also needs a namespace.
   Level is right, the check will only pass on backends that accept bare names.
7. `links::hardlink_limit_reports_too_many_links` is Cowfs and hangs on every native run past the 60 s timeout (a leaked thread in each of four runs).
   On ext4 the limit is 65000 links.
   Through the NFS mount it reached EMLINK after 540 s.
   Level is right, but the timeout and the run cost need a look.
8. `basic::statfs_free_after_unlink` is Cowfs and fails on APFS and btrfs, whose free counts do not move at once.
   Level is right.

Properly leveled Cowfs, confirmed failing natively for the known reasons: every directory `nlink` check, `xattr_on_directory_and_symlink` (Linux refuses `user.*` on symlinks), `xattr_empty_and_large_values` (60,000 bytes is ENOSPC on ext4 and btrfs), `non_utf8_names` and `names_are_exact_bytes` (APFS), `rmdir_reclaimed_after_forget` (APFS keeps nlink 2 on a removed directory).

## Adapter findings

### Bug: the duplicate request cache ignores the connection

Reproducer (scratch test `drc_two_connections.rs`, raw protocol, no mount, output read):
1. Connection A sends CREATE `x` with xid 5, then REMOVE `x` with xid 6.
2. Connection B, same client IP, sends the identical CREATE `x` bytes with xid 5.
3. B is told status 0 (success) and `x` does not exist: LOOKUP `x` returns NOENT.

The cache in `nfsserve/src/reply_cache.rs` is keyed by client address, xid and a hash of the call, not by connection, so a second connection replays the first one's cached reply and the call is never executed.
Real clients pick xids per connection, so collisions are unlikely with the macOS client, but two clients on one host (or a client that restarts its xid counter) are exposed, and a non-idempotent call silently does nothing.
Evidence at suite level: with every pooled connection starting at xid 1 the concurrent suite checks (`concurrent_creates_in_one_directory`, `concurrent_rename_unlink_lookup`) hang and panic; with distinct xid bases they pass (12.5 s and 2.6 s).
Suspected cause: the key should include the connection (peer port or a connection id).

### Protocol level: the checks silly rename made unjudgeable

Over the raw NFSv3 protocol, with no client holding a descriptor, all 9 silly-rename Posix failures pass: `mkdir_rmdir_errors`, the three `readdir_delete_*`, `rename_file_over_file`, `lookup_nlink_is_fresh`, `hardlink_unlink_one_other_survives`, `hardlink_across_directories`, `hardlink_pairs_8000_listed_once_and_removed`.
So the adapter's unlink, rename, rmdir, readdir cookie and nlink behaviour is correct at the protocol level for those.
The Cowfs `lifecycle` checks that need an open handle (`unlink_while_open_keeps_data`, `unlink_while_open_reclaimed_after_release_and_forget`, `forget_keeps_inode_with_handle`, `two_handles_pin_until_last_release`, `rename_open_file_keeps_handle_working`, `hardlink_nlink_counts_names`) fail raw with `Stale` after the last name is removed.
That is expected: NFSv3 is stateless, a handle of a removed file is STALE (RFC 1813), and `NfsVfs::open` cannot pin anything.
They cannot be judged over NFSv3 at all, so they say nothing about the adapter.
`create_in_removed_directory_fails` and `rmdir_updates_parent` fail raw for the same reason (STALE where the suite wants NotFound or attributes).
`validate_name_cases` and `invalid_names_rejected_by_creating_ops` fail raw because the adapter answers EXIST for `.` on purpose (`adapter.rs new_name`); my `NfsVfs` maps that to `Exists`.
The xattr and `symlink_size_is_target_length` failures are NFSv3 limits (no xattr procedures) and the adapter's 1024 byte symlink cap.

### Sparse-file blocks (client, not adapter)

Raw GETATTR (`nfs-raw-getattr.log`): `used` is 4096 for a 1 MiB and for a 1 GiB sparse file, 1048576 for a dense 1 MiB file, 0 for an empty one.
So the adapter reports the right allocation.
Through the mount `st_blocks` is `ceil(size/512)` (2049 for the 1 MiB hole, 2097153 for the 1 GiB hole), so the macOS client ignores `used`.
`sparse_write_far_past_eof` on the NFS mount is client behaviour (class N).

### statfs flake (client cache)

`statfs_sane` and `statfs_free_after_unlink` fail on the NFS mount and pass over the raw protocol.
`probe4` on the mount: a statfs read directly after a 1 MiB write, with no delay, shows no change in free blocks or free inodes.
After 0.2 s or more it does.
The macOS client caches `statfs` for a short time, so a check that measures right after the write sees the old numbers.
Earlier round-1 runs passed `statfs_sane` alone and failed it in full runs, which fits a timing race with the previous check's deferred cleanup rather than an adapter fault.
Class N.

### Not an adapter fault, listed for completeness

- `rename_no_replace` on the mount: the macOS client returns ENOTSUP for `RENAME_EXCL` even onto a missing name; the raw protocol passes.
- `write_updates_mtime_and_ctime`, `namespace_ops_update_times`, `rename_updates_times`: client write-back and attribute cache (the last two pass with `actimeo=0`; the first does not, because the mtime stays until `fsync` or close).

### Left unexplained

- `xattr_shared_by_hardlinks` failed once in a round-1 run with `actimeo=0` (NoAttr through the second name) and passed in every other run, including round 2. Not reproduced, cause unknown.
- `rename_dir_over_empty_dir` on the NFS mount failed with `nlink 2, want 0` in one run and `Stale` in another, so it depends on attribute caching; not pinned down further.

## Round 3: the review of PR #35 (failing-before evidence from the pre-fix tree)

Every regression test below was run against the pre-fix tree; the messages are what it printed.

| finding | fixed | test | failing before |
|---|---|---|---|
| F1 leaked node and descriptor for a file with an untracked hardlink | yes | `a_hardlink_made_outside_the_vfs_does_not_keep_the_node_forever` | `nodes left after forgetting every file: left: 301, right: 1` (critic measured 300 of 300 live and +445 descriptors) |
| F1 an `Ino` could be reused for a different file | yes | `an_inode_number_the_backing_filesystem_reuses_gets_a_new_ino`, `an_inode_number_is_never_handed_out_twice` | mutant `ino_reused`: KILLED by the first |
| F8 `Some(EACCES \| EPERM \| EROFS)` is `Some(31)` | yes | `a_pinned_descriptor_lets_a_read_only_file_be_written_after_the_cache_is_cold`, `readonly_file_is_writable_after_the_descriptor_cache_is_cold` | `write cold: PermissionDenied` |
| F9 cached listing missed changes made outside the Vfs | yes | `readdir_sees_a_name_created_outside_the_vfs_while_a_listing_is_paged` | `left: [[97],[98],[99],[100]], right: [[97],[98],[99],[100],[101]]` (the externally created name never appeared) |
| F3 reopen after a symlink was swapped into the name | yes | `reopening_a_file_never_follows_a_symlink_swapped_into_its_name` | `read: Io("Too many levels of symbolic links (os error 62)")` |
| F3 `fchmodat` could follow a swapped symlink | yes | `mode_never_lands_on_a_symlink_target` | mode now goes through an `O_NOFOLLOW` descriptor; `fchmodat` is gone |
| F4 `hardlink_pairs_8000` marginal against the timeout | yes | (timing, `tests/common/mod.rs` and the per-page rebuild) | 140 s, 242 s and 313 s in three runs before the rebuild; 39 to 60 s after, with a 300 s timeout |
| F5 one lock over the whole table | partly | (measured) | critic: a `getattr` waited 1.78 s behind one `readdir` of a 50,000 entry directory; now 3.3 ms worst case at page size 1 and 3 us at page 1000 |
| F7 unbounded `listxattr` retry, `ENOSYS` unmapped, `write` offset guard | yes | (bounded retry, mapping, guard) | - |
| trait: `#[non_exhaustive]` `Error` and `FileKind`, `readdir_attrs` | yes | - | - |

Two more fixes came out of round 3:
- `read` above 64 MiB no longer looks like the end of the file: the cap is a per-syscall chunk
  and the loop fills the caller's buffer (`read_above_the_cap_is_not_mistaken_for_the_end_of_the_file`).
- xattr names are validated on all four entry points, not just `setxattr`.

## Mutation run: can this control fail?

18 mutants, each a one-line patch in its own copy of the tree, its own `CARGO_TARGET_DIR`,
its own native directory, unit tests then the Posix level under a hard timeout
(`spikes/nfs-loopback/out/pathvfs/mut/run_mutations.py`). Killed means the crate's unit tests
or the Posix-level run failed.

| mutant | killed by |
|---|---|
| `hardlink_by_path` | unit: hardlink identity, reclaim |
| `ino_per_name` | unit |
| `write_ignores_offset` | unit: two tests |
| `cookie_unstable` | unit: cookie tests |
| `setattr_follows_symlink` | Posix: `symlink_dangling_ok`, symlink times |
| `path_based_open` | SURVIVED |
| `mode_unmasked` | SURVIVED |
| `forget_drops_nothing` | unit: three lifecycle tests |
| `unlink_closes_fd` | unit: three lifecycle tests |
| `rename_no_table_update` | unit: rename reachability |
| `create_skips_validate` | unit: name validation on 9 entry points |
| `no_reclaim` | unit: three lifecycle tests |
| `xattr_follows` | SURVIVED |
| `readdir_max_zero_ok` | unit |
| `external_change_blind` | unit |
| `ino_reused` | unit |
| `no_pinned_rw` | unit: read-only write after cold cache |
| `reopen_without_identity` | unit: inode identity after a name is taken over |

15 of 18 killed. The critic's run of the same 13 mutants killed 5.
The three survivors, and why:
- `path_based_open` (drops `O_NOFOLLOW` from every reopen): two independent defences catch it,
  the identity re-verification on the reopened descriptor and the scan for the file by identity
  in its parents. Killing it needs a race between the identity check and the open, which is not
  a deterministic test.
- `mode_unmasked` (no `MODE_MASK` on setattr): unobservable on any POSIX filesystem, because
  `chmod` ignores the file-type bits itself. `setattr_masks_the_mode_on_the_backing_filesystem_too`
  asserts it anyway.
- `xattr_follows` (the Linux symlink branch of `with_xattr`): on macOS the code is compiled out,
  and on Linux `lgetxattr`/`lsetxattr` do not follow a final symlink and `fgetxattr` on an
  `O_PATH` descriptor cannot. Not expressible as a mutation.

## PathVfs bugs the runs found (all fixed, all with tests where a filesystem is not needed)

- `readdir` rebuilt the sorted snapshot on every page, so `hardlink_pairs_8000` timed out at 300 s on APFS. Now a per-directory listing is cached and dropped on every change to that directory.
- `read` at an offset above `i64::MAX` returned EINVAL on Linux; the trait wants an empty read.
- `statfs` on macOS used `fstatvfs`, whose 32-bit block counts wrap on a large volume (NFS run: "blocks is 0"); it uses `fstatfs` now.
- Linux build warning: unreachable pattern `ENOTSUP | EOPNOTSUPP`.
- Test harness: the next check started while the previous check's tree was still being deleted.
- Round 2: `readdir` with `max` 0 succeeded (contract: `InvalidArgument`), fixed and re-run green on APFS, btrfs, ext4, FUSE and NFS mount.
- Round 2: `setxattr` did not validate names (empty, NUL, over 255 bytes) before the filesystem saw them. Fixed. `xattr_name_validation` still fails natively for another reason, see the matrix.

## Known gaps in PathVfs

- Unlinking the last known name of a file that cannot be reopened (mode 000) while a reference remains makes the inode `Stale` early.
- `RENAME_EXCL` is not emulated where the filesystem lacks it (NFS client).
- `setattr` is not atomic: `size`, then times, then mode, each applied in turn.
- A node whose mode has no owner write bit holds a writable descriptor for as long as it is
  referenced, because nothing can reopen such a file for writing later; the cost is one
  descriptor per deliberately read-only file that is still referenced.
- The inode table is still one lock, so one `readdir` holds it for one `list_dir` plus one
  `fstatat` per returned entry (3.3 ms for a 50,000 entry directory at page size 1).
- Symlink xattrs on Linux go through `/proc/self/fd`.
- One lock guards the inode table.
- Unit tests: 16, in the crate.
  The suite runs are `#[ignore]`d and need the environments above.

## Reproducing

Native, from the repo root (add `COWFS_CONFORMANCE_HEAVY=1` for the 50,000 entry and 8 MiB checks, `COWFS_CONFORMANCE_LEVEL=posix|portable|cowfs` to select a level):

```text
COWFS_PATHVFS_NATIVE_DIR=/some/dir cargo test -p cowfs-vfs-path --test native -j4 -- --ignored --nocapture
```

Mounted: mount any adapter over a `MemVfs`, then

```text
COWFS_PATHVFS_MOUNT=/path/to/mount cargo test -p cowfs-vfs-path --test mount -j4 -- --ignored --nocapture
```

The drivers used here are a few lines each, kept outside the adapter crates.
Each mounts a `MemVfs`, runs the test binary with `COWFS_PATHVFS_MOUNT` set, and unmounts.

FUSE (Linux, a scratch copy of origin/v1/11-fuse with a `pathvfs-driver` crate added):

```rust
let vfs: Arc<dyn Vfs> = Arc::new(cowfs_vfs_test::MemVfs::new());
let mount = cowfs_fuse::Mount::new(vfs, mp, "".parse().unwrap()).unwrap();
let st = Command::new(&cmd).args(&args).env("COWFS_PATHVFS_MOUNT", mp).status().unwrap();
mount.unmount().unwrap();
```

NFS (macOS, a scratch copy of origin/v1/12-nfs):

```rust
let vfs: Arc<dyn Vfs> = Arc::new(cowfs_vfs_test::MemVfs::new());
let mut opts = cowfs_nfs::MountOptions::default(); // opts.actimeo = 0 for NFS-a0
let mount = cowfs_nfs::Mount::new(vfs, Path::new(mp), opts).unwrap();
let st = Command::new(&cmd).args(&args).env("COWFS_PATHVFS_MOUNT", mount.mountpoint()).status().unwrap();
mount.unmount().unwrap();
```

Case-sensitive APFS: `hdiutil create -type SPARSE -fs 'Case-sensitive APFS' -size 4g -volname cowfscs cs.sparseimage`, `hdiutil attach -mountpoint <dir> -nobrowse`, run native with `COWFS_PATHVFS_NATIVE_DIR=<dir>/run`, then `hdiutil detach`.
Linux ext4: `truncate -s 3G ext4.img; /sbin/mkfs.ext4 -q -F ext4.img; sudo mount -o loop ext4.img <dir>`.
Raw logs and scripts: `spikes/nfs-loopback/out/pathvfs/` (not committed).
