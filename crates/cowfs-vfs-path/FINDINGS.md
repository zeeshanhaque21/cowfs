# cowfs-vfs-path findings (#34)

`PathVfs` runs the conformance suite (114 checks, heavy ones included) on native filesystems as a control and through the mounted FUSE and NFS adapters.
Nothing here is a claim from reading code: every cell below is from a run whose log was read.
Suite and adapter sources were not touched.

## Environments

| name | what | adapter source |
|---|---|---|
| APFS | Mac data volume (macOS 26), `$TMPDIR` | - |
| APFS-cs | 4 GiB sparse image, `Case-sensitive APFS`, detached afterwards | - |
| btrfs | OrbStack VM `cowfs-spike3`, VM root | - |
| ext4 | same VM, 3 GiB loop-mounted ext4 image | - |
| FUSE | same VM, `cowfs-fuse` (origin/v1/11-fuse 5a42c12) serving a `MemVfs`, `PathVfs` rooted in the mount | v1/11-fuse |
| NFS | Mac, `cowfs-nfs` (origin/v1/12-nfs e93a3d4) serving a `MemVfs`, default `actimeo=120` | v1/12-nfs |
| NFS-a0 | same with `actimeo=0` | v1/12-nfs |

Both adapters were built in scratch copies under `spikes/nfs-loopback/out/pathvfs/`, with the current `cowfs-vfs-test` copied over their older one.
No adapter branch was modified.
Single run per environment, except the torn-read check (10 repeated runs, see below).

## Matrix

Only checks that fail somewhere are listed.
75 checks pass everywhere.
Class codes: S = the suite assumes something the filesystem does not do, Q = real quirk of that filesystem, N = behaviour of the macOS NFS client as seen by any program on the mount, PV = PathVfs bug (fixed, none open).

| check | APFS | APFS-cs | btrfs | ext4 | FUSE | NFS | NFS-a0 | class and evidence |
|---|---|---|---|---|---|---|---|---|
| `basic::root_is_directory` | ok | ok | FAIL | ok | ok | ok | ok | S: suite pins directory nlink = 2 + subdirs; btrfs reports 1 for every directory |
| `basic::new_file_attrs` | ok | ok | FAIL | ok | ok | ok | ok | S: same directory nlink assumption (btrfs) |
| `basic::dir_nlink_counts_subdirs` | FAIL | FAIL | FAIL | ok | ok | ok | ok | S: APFS counts every child in a directory nlink (got 5, want 3); btrfs always 1; ext4 matches the suite |
| `basic::statfs_sane` | ok | ok | FAIL | ok | ok | FAIL | FAIL | btrfs: S (statfs reports files=0, files_free cannot drop). NFS: UNEXPLAINED - fails in both full runs, passes alone with actimeo=0 |
| `io::sparse_write_far_past_eof` | ok | ok | ok | ok | ok | FAIL | FAIL | NFS: N, plausibly client (st_blocks = ceil(size/512) for sparse files, probe2); adapter sends used=blocks*512 (convert.rs:37) so NOT confirmed |
| `io::write_updates_mtime_and_ctime` | ok | ok | ok | ok | ok | FAIL | FAIL | NFS: N (write-back: mtime unchanged after pwrite until fsync/close, probe3) |
| `attrs::setattr_bumps_ctime` | FAIL | FAIL | ok | ok | ok | ok | ok | Q: APFS does not bump ctime when only atime is set (utimensat, mtime omitted) |
| `attrs::namespace_ops_update_times` | ok | ok | ok | ok | ok | FAIL | ok | NFS: N (client attribute cache, passes with actimeo=0) |
| `names::non_utf8_names` | FAIL | FAIL | ok | ok | ok | ok | ok | Q: APFS rejects non-UTF-8 names (EILSEQ) |
| `names::names_are_exact_bytes` | FAIL | FAIL | ok | ok | ok | ok | ok | Q: APFS treats NFC and NFD names as one name (EEXIST); also on the case-sensitive volume |
| `dirs::mkdir_rmdir_errors` | ok | ok | ok | ok | ok | FAIL | FAIL | NFS: N silly rename (an unlinked file that is still open stays as .nfs* and blocks rmdir) |
| `dirs::rmdir_updates_parent` | FAIL | FAIL | FAIL | ok | ok | FAIL | FAIL | APFS/btrfs: S (directory nlink). NFS: N (fstat of a removed directory through a held fd fails, probe8) |
| `dirs::deeply_nested_directories` | FAIL | FAIL | FAIL | ok | ok | ok | ok | S: directory nlink (APFS, btrfs) |
| `readdir::readdir_delete_returned_entries_between_pages` | ok | ok | ok | ok | ok | FAIL | FAIL | NFS: N silly rename leaves .nfs* entries that are listed and never removed by the suite |
| `readdir::readdir_delete_upcoming_entries_between_pages` | ok | ok | ok | ok | ok | FAIL | FAIL | NFS: N silly rename (.nfs* entries in the listing) |
| `readdir::readdir_delete_everything_between_pages` | ok | ok | ok | ok | ok | FAIL | FAIL | NFS: N silly rename (.nfs* entries in the listing) |
| `rename::rename_file_over_file` | ok | ok | ok | ok | ok | FAIL | FAIL | NFS: N silly rename of the replaced file (nlink stays 1) |
| `rename::rename_dir_over_empty_dir` | FAIL | FAIL | FAIL | ok | ok | FAIL | FAIL | APFS/btrfs: S (directory nlink). NFS: N (replaced directory has no attributes any more: Stale, or a cached nlink 2) |
| `rename::rename_dir_cross_directory_fixes_nlink` | FAIL | FAIL | FAIL | ok | ok | ok | ok | S: directory nlink (APFS, btrfs) |
| `rename::rename_no_replace` | ok | ok | ok | ok | ok | FAIL | FAIL | NFS: N (macOS NFS client returns ENOTSUP for renamex_np RENAME_EXCL even onto a missing name, probe7b) |
| `rename::rename_open_file_keeps_handle_working` | ok | ok | ok | ok | ok | FAIL | FAIL | NFS: N silly rename |
| `rename::rename_updates_times` | ok | ok | ok | ok | ok | FAIL | ok | NFS: N (client attribute cache, passes with actimeo=0) |
| `links::hardlink_nlink_counts_names` | ok | ok | ok | ok | ok | FAIL | FAIL | NFS: N silly rename |
| `links::hardlink_unlink_one_other_survives` | ok | ok | ok | ok | ok | FAIL | FAIL | NFS: N silly rename |
| `links::hardlink_across_directories` | FAIL | FAIL | FAIL | ok | ok | FAIL | FAIL | APFS/btrfs: S (directory nlink). NFS: N silly rename (rmdir of a directory holding .nfs*) |
| `links::hardlink_to_directory_is_denied` | ok | ok | FAIL | ok | ok | ok | ok | btrfs: S (directory nlink) |
| `links::hardlink_pairs_8000_listed_once_and_removed` | ok | ok | ok | ok | ok | FAIL | FAIL | NFS: N silly rename (24000 entries removed, 16000 expected); with actimeo=0 it exceeds the 300 s runner timeout instead |
| `lifecycle::unlink_while_open_keeps_data` | ok | ok | ok | ok | ok | FAIL | FAIL | NFS: N silly rename (nlink 1 instead of 0) |
| `lifecycle::unlink_while_open_reclaimed_after_release_and_forget` | ok | ok | ok | ok | ok | FAIL | FAIL | NFS: N silly rename |
| `lifecycle::forget_keeps_inode_with_handle` | ok | ok | ok | ok | ok | FAIL | FAIL | NFS: N silly rename |
| `lifecycle::rmdir_reclaimed_after_forget` | FAIL | FAIL | ok | ok | ok | FAIL | FAIL | APFS: Q (removed directory keeps nlink 2). NFS: N (no attributes for a removed directory) |
| `symlinks::symlink_size_is_target_length` | FAIL | FAIL | ok | ok | ok | FAIL | FAIL | S: suite uses a 4095 byte target, macOS caps symlink targets at 1024 (APFS, NFS adapter SYMLINK_TARGET_MAX = 1024, adapter.rs:24) |
| `xattrs::xattr_set_get_list_remove` | FAIL | FAIL | ok | ok | ok | FAIL | FAIL | Q: macOS adds com.apple.provenance to new files (bytes decode to that name), APFS and NFS |
| `xattrs::xattr_empty_and_large_values` | FAIL | FAIL | FAIL | FAIL | ok | FAIL | FAIL | macOS: Q com.apple.provenance in listxattr. btrfs/ext4: S (a 60,000 byte value gives ENOSPC; ext4 keeps xattrs in one block) |
| `xattrs::xattr_list_order_is_stable` | FAIL | FAIL | ok | ok | ok | FAIL | FAIL | Q: com.apple.provenance (got 11 names, want 10) |
| `xattrs::xattr_on_directory_and_symlink` | ok | ok | FAIL | FAIL | FAIL | ok | ok | S: Linux refuses user.* xattrs on symlinks (EPERM); FUSE fails in the kernel before the adapter sees it |
| `xattrs::xattr_shared_by_hardlinks` | ok | ok | ok | ok | ok | ok | FAIL | NFS-a0 only: UNEXPLAINED (NoAttr through the second name); suspect per-name AppleDouble sidecars, NOT checked |
| `concurrency::concurrent_readers_and_writers_of_one_file` | ok | ok | FAIL | ok | FAIL | ok | ok | S: Linux buffered read and write are not atomic per 4 KiB block; native btrfs and ext4 fail 6 of 10 repeated runs |
| `concurrency::concurrent_rename_unlink_lookup` | FAIL | FAIL | FAIL | ok | ok | ok | ok | S: directory nlink (APFS, btrfs) |

|  | APFS | APFS-cs | btrfs | ext4 | FUSE | NFS | NFS-a0 |
|---|---|---|---|---|---|---|---|
| passed | 99 | 99 | 100 | 112 | 112 | 88 | 89 |
| failed | 15 | 15 | 14 | 2 | 2 | 26 | 25 |

## Adapter bugs

None confirmed.

FUSE: every check that fails through the mount also fails on native Linux (`xattr_on_directory_and_symlink` on all three, `concurrent_readers_and_writers_of_one_file` on btrfs and, intermittently, ext4).
FUSE passes `xattr_empty_and_large_values` and the directory nlink checks that btrfs fails, because `MemVfs` is more permissive than btrfs.
That is a property of the backing `MemVfs`, not a defect.

NFS: 18 checks pass on APFS and fail through the mount.
For 16 of them (12 silly rename, 1 RENAME_EXCL, 1 write mtime, 2 attribute cache) a plain program with no `PathVfs` sees the same behaviour on the mount (probe scripts and logs `nfs-probe*.log`, `apfs-probe*.log` in the out directory).
The other 2 are listed as unexplained below.
The probes show the mount's behaviour, not whether the adapter or the client causes it; for silly rename and the attribute cache that is well known NFS client behaviour, for the rest it is not separated.
- Silly rename: an open file that is unlinked stays in its directory as `.nfs.<id>.<n>` until closed, then vanishes; `rmdir` of a directory holding one is ENOTEMPTY.
  `PathVfs` must hold a descriptor across an unlink to keep the inode usable, as the trait requires, so every unlink-while-referenced check trips it.
  Checks: `mkdir_rmdir_errors`, `readdir_delete_*` (3), `rename_file_over_file`, `rename_open_file_keeps_handle_working`, `hardlink_nlink_counts_names`, `hardlink_unlink_one_other_survives`, `hardlink_across_directories`, `hardlink_pairs_8000_...`, `unlink_while_open_*` (2), `forget_keeps_inode_with_handle`.
- `renamex_np(RENAME_EXCL)` returns ENOTSUP on the mount even onto a missing name, and EEXIST onto an existing one (`rename_no_replace`).
- `pwrite` without close does not move mtime until `fsync` (`write_updates_mtime_and_ctime`).
- A directory that was removed while a descriptor is held answers ENOENT to `fstat` (`rmdir_updates_parent`, `rmdir_reclaimed_after_forget`, `rename_dir_over_empty_dir`).
- Attribute cache: `namespace_ops_update_times` and `rename_updates_times` pass with `actimeo=0`.

Consequence: through NFS with `PathVfs` as the client, the whole "unlink while open / forget / rmdir while referenced" area cannot be judged.
Those checks say nothing about the adapter until a client that does not hold descriptors (a raw NFSv3 RPC client, as in `cowfs-nfs/tests/common`) runs them.
That was not done here.

Not explained, so not classified:
- `statfs_sane` on NFS: fails in both full runs, passes alone with `actimeo=0` (`nfs-iso-statfs_sane.log`).
  Suspect: deferred deletion of the previous check's files on the mount changes free-inode counts mid-check.
  Not verified.
- `sparse_write_far_past_eof` on NFS: `st_blocks` is `ceil(size/512)` even for a hole (`nfs-probe2.log`: 2049 blocks for a 1 MiB sparse file, 8 on APFS), and it fails in isolation too.
  `convert.rs:37` sends `used = blocks * 512`, so the client may ignore `used`, or the adapter may not send it.
  Only a raw GETATTR on the wire settles it.
- `xattr_shared_by_hardlinks` on NFS-a0 only: NoAttr through the second name.
  Passes on the default NFS run, so it may be flaky.
- `rename_dir_over_empty_dir` on NFS: the failure is `nlink 2, want 0` on the default run and `Stale` on NFS-a0, so it depends on attribute caching.

Watch item for the NFS adapter: `.nfs.*` entries appear in listings.
The adapter hides `._*` sidecars but not these.
Whether that matters to users is the NFS agent's call.

## Suite bugs and wrong assumptions

None of these is fixed here (the suite is not ours to edit).

1. Directory `nlink`.
   The suite pins `2 + subdirectories`.
   ext4 matches.
   btrfs reports 1 for every directory.
   APFS reports 2 + all children (a directory with one subdirectory, one file and one symlink: got 5, want 3).
   POSIX does not define directory `nlink`.
   Affects 10 checks on btrfs and 8 on APFS.
   Suggest a suite option to skip directory nlink, or check only that it is at least 2 on ext4-like backends.
2. `xattr_on_directory_and_symlink`: Linux forbids `user.*` xattrs on symlinks (EPERM).
3. `xattr_empty_and_large_values`: a 60,000 byte value gives ENOSPC on ext4 and btrfs.
   The suite says larger values are backend defined, but 60,000 is inside what it requires.
4. `concurrent_readers_and_writers_of_one_file`: Linux buffered I/O does not make a read atomic against a write of the same page.
   Repeated 10 times on native ext4: 4 pass, 6 fail; native btrfs: 4 pass, 6 fail (`vm-torn.sh`).
   XFS-style atomicity is not something Linux promises in general.
5. `symlink_size_is_target_length`: a 4095 byte target is over the macOS limit of 1024.
6. `statfs_sane`: assumes `files_free` drops when a file is created; btrfs reports `files = 0`.
7. macOS: `com.apple.provenance` is added to new files, so "a new file has no attributes" and exact `listxattr` comparisons fail on any macOS filesystem.
8. `setattr_bumps_ctime`: APFS does not bump ctime for an atime-only `utimensat`.
9. The runner drops a check's `Vfs` on its own thread after the result is out, so a backend whose drop deletes files races the next check's `statfs`.
   Handled in the test harness here by waiting for cleanup.

APFS-cs: the 15 failures are identical to the default volume.
No failing check is explained by case sensitivity.
`names_are_exact_bytes` fails on APFS-cs because NFC and NFD names alias (probe).
On the default (case-insensitive) volume it may fail earlier on `a`/`A`; the log message is the same ("file exists"), so which name aliased there was not checked.

## PathVfs bugs the runs found (all fixed, all with tests where a filesystem is not needed)

- `readdir` rebuilt the sorted snapshot on every page, so `hardlink_pairs_8000` timed out at 300 s on APFS. Now a per-directory listing is cached and dropped on every change to that directory.
- `read` at an offset above `i64::MAX` returned EINVAL on Linux; the trait wants an empty read.
- `statfs` on macOS used `fstatvfs`, whose 32-bit block counts wrap on a large volume (NFS run: "blocks is 0"); it uses `fstatfs` now.
- Linux build warning: unreachable pattern `ENOTSUP | EOPNOTSUPP`.
- Test harness: the next check started while the previous check's tree was still being deleted.

## Known gaps in PathVfs

- Unlinking the last known name of a file that cannot be reopened (mode 000) while a reference remains makes the inode `Stale` early.
- `RENAME_EXCL` is not emulated where the filesystem lacks it (NFS client).
- Symlink xattrs on Linux go through `/proc/self/fd`.
- One lock guards the inode table.
- Unit tests: 16, in the crate.
  The suite runs are `#[ignore]`d and need the environments above.

## Reproducing

Native, from the repo root (add `COWFS_CONFORMANCE_HEAVY=1` for the 50,000 entry and 8 MiB checks):

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
