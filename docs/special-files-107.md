# Special files design (issue #107)

Status: design, written before any code.
Decision: Zee, 2026-10-09, implement special-file support so gate g3 can pass.
Evidence base: `docs/reviews/g3-status-20261009.md` and the pinned pjdfstest `85a8aea9e685999ef0540392fd80535f873d7ff7`.

## Problem

A fifo, socket or device node cannot be created on a mount.
The refusal is by design in every layer today.
NFS `MKNOD` answers `NFS3ERR_NOTSUPP` (`crates/nfsserve/src/nfs_handlers.rs`, `crates/nfsserve/PATCHES.md`).
FUSE `mknod` answers `ENOTSUP` for any non-regular type (`crates/cowfs-fuse/src/fs.rs`, `convert::mknod_is_regular`).
`cowfs-core` import refuses such a source entry (`crates/cowfs-core/src/import.rs`).
`cowfs-meta` has three file types only (`docs/v1-meta.md` line 428: "Only regular files, directories, and symlinks").
`FileKind` and `meta::FileType` have no variant for the other four types.

## What g3 needs

About 75 of the 77 established g3 regressions are fifo, socket or device creation cases.
They are `mkfifo/00.t` 22, `mknod/00.t` 22, `unlink/00.t` 30, `open/17.t` 3, plus 13 rows in the `for type in regular dir fifo block char socket symlink` loops.
The loops are in `link/10.t`, `mkdir/10.t`, `open/22.t`, `rename/13.t`, `rename/20.t`, `rmdir/06.t` and `symlink/08.t`.
A further 360 `ENOENT` cascade rows and 48 textless rows follow from a failed `mkfifo` or `bind` earlier in the same script.
The harness runs as a non-root user, so only fifo and socket creation is reachable there.

Facts read from the pinned scripts:

- `mkfifo/00.t` and `mknod/00.t` (type `f`) check `lstat` type and mode after the umask (client side), `nlink`, size, and that atime, mtime and ctime of the node and mtime and ctime of the parent move forward.
- `unlink/00.t` and `open/17.t` create a fifo with `mkfifo`, then unlink or open it.
- `open/24.t` and the `bind` rows create a unix socket with `bind(2)`.
- Every `mknod ... b` and `mknod ... c` assertion needs root (`mknod/02.t`, `03.t`, `11.t`), so the harness counts and excludes them.
- `mknod/11.t` asserts `EINVAL` for a device number that is too big.
  It is root-only and excluded from g3, so the server behaviour for out-of-range numbers is chosen here, not by g3: the adapters reject major or minor that does not fit the wire field with `EINVAL`.
- Block and character devices are therefore validated by the conformance suite and by unit tests, not by g3.

## Scope

Types supported: FIFO, socket (SOCK), character device (CHR), block device (BLK).
Protocols: NFSv3 `MKNOD` (macOS and Linux clients), FUSE `mknod` (Linux).
FUSE `mknod` for a regular file stays on the existing `create` path.
NFSv3 `CREATE`, `MKDIR` and `SYMLINK` are unchanged.

Non-goals:

- No device semantics on the server.
  A device node is a name, a type and a device number.
  The client kernel decides what opening it means.
- No permission model beyond the device-creation check below.
- No new issue and no change to `progress/plan.json`.

## Vfs layer (`cowfs-vfs`)

`FileKind` gains `Fifo`, `Socket`, `CharDevice`, `BlockDevice`.
`FileKind` is already `#[non_exhaustive]`, so adding variants is source compatible for matchers that have a wildcard arm.
`Attr` gains `pub rdev: u64`.
`rdev` is `0` for everything except `CharDevice` and `BlockDevice`.
`rdev` is cowfs canonical: `(major << 32) | minor`, with helpers `makedev(major, minor)`, `dev_major(rdev)` and `dev_minor(rdev)` in `cowfs-vfs`.
It is not a host `dev_t`.
Every adapter converts to and from its own wire encoding, so a store written on macOS is read correctly on Linux.
Every `Attr` constructor gets one added `rdev` line, found by the compiler, and nothing else on those lines is touched, to keep conflicts with the #103 change small.

A new method on `Vfs`:

```rust
fn mknod(&self, parent: Ino, name: &[u8], kind: FileKind, mode: u32, rdev: u64) -> Result<Attr> {
    Err(Error::NotSupported)
}
```

The default keeps every existing implementation compiling (`PathVfs` in `cowfs-vfs-path` is not touched).
`kind` must be one of the four special kinds.
`Regular`, `Directory`, `Symlink` and unknown kinds return `InvalidArgument`, because those have their own methods.
`rdev` must be `0` unless `kind` is a device; a non-zero `rdev` for a fifo or socket is `InvalidArgument`.
`mode` is masked with `MODE_MASK`, as for `create`.
Name validation, `Exists`, `NotDir`, `Stale` and `ReadOnly` behave exactly as for `create`.
Effects match `create`: new inode has `nlink` 1, `size` 0, `blocks` 0, atime, mtime and ctime set to now, and the parent gets mtime and ctime.
The new node is a counted reference like the result of `create`, so `forget` applies.

Operations on an existing special node:

| operation | behaviour | why |
| --- | --- | --- |
| `getattr`, `lookup`, `readdir`, `readdir_attrs` | report the real kind and `rdev` | needed for the client to build the right vnode |
| `setattr` mode, atime, mtime | work as for a regular file | `chmod` and `utimes` on a fifo are legal; NFS clients send times on close of a fifo |
| `setattr` size | `InvalidArgument` | same as a symlink; native `truncate` on a fifo is `EINVAL` |
| `read`, `write` | `InvalidArgument` | same as a symlink; the client never sends these for a fifo, socket or device |
| `open`, `release`, `flush`, `fsync` | succeed and pin the inode | `open` never fails because of intent |
| `link` | allowed (non-directory) | native allows hardlinks to fifos, and `link/10.t` needs it |
| `unlink`, `rename` | as for a regular file, replacing a special target is allowed | `unlink/00.t`, `rename/13.t`, `rename/20.t` |
| `rmdir` on a special node | `NotDir` | unchanged logic |
| xattrs | allowed | metadata only; Linux would forbid user xattrs on specials, a refinement we do not need |
| `readlink` | `InvalidArgument` | unchanged |

Permissions: the `Vfs` has no caller credentials and does not enforce permissions today, so the device check lives in the adapters.
Creating `CharDevice` or `BlockDevice` as a non-root caller is `EPERM`, like native.
Creating a fifo or socket needs no privilege.
`Error` gets no new variant for this: adapters answer `EPERM` or `NFS3ERR_PERM` themselves before calling `mknod`, in the same way adapters already map "link a directory" to `EPERM`.
That keeps this work out of the way of the parallel `FileTooBig` change.

Docs: the `Vfs` trait doc sentence "There are no special files (devices, fifos, sockets)" is replaced.
`docs/v1-meta.md` line 428 is replaced.

## Storage (`cowfs-meta`)

`FileType` gains `Fifo` (code 4), `Socket` (5), `CharDevice` (6), `BlockDevice` (7).
Codes 1 to 3 are unchanged.
`meta::Attr` gains `pub rdev: u64`.

The inode record keeps `INODE_V = 2` and its 86-byte layout for every existing kind and for fifo and socket.
A character or block device record is 94 bytes: the same 86 bytes with `rdev` (u64, little endian) appended.
`InodeRec::decode` accepts length 86 (rdev 0) and length 94, and a length 94 record is valid only for a device kind.
`InodeRec::encode` writes 94 bytes only for devices.
There is no new key kind and no extra row, so reading a device is still one get.

Format version impact: lazy bump from `2` to `3`.
A store stays at version 2 until its first special node is committed.
The commit that persists the first special node also writes `version = 3` in the same write transaction, so the version and the tree change atomically.
This build opens versions 2 and 3 (`check` too).
A build that only knows version 2 refuses a version 3 store up front with the existing `Format` error, before it reads any record.
Existing stores need no migration and a store that never holds a special node is byte compatible with older builds.
Reason for not staying at 2: `Inner::note` sets `poisoned` on any `Error::Corrupt`, and an older build reads a code 4 to 7 inode as `Corrupt("unknown file type")`, so merely looking up a fifo would poison the whole session of an old binary.
Mechanism: the session keeps a flag; `Tx::mknod` sets it (restored if the batch fails); the snapshot commit in `db.rs` inserts `version = 3` into `META` when the flag is set; open sets the flag from the stored version.
The flag is never cleared: a store that once held a special node stays at version 3 after the node is removed.

Dirent records carry a one-byte kind code, so they carry the new codes with no layout change.
`new_child` generalises to take the kind and an optional rdev; `Tx::mknod(dir, name, kind, mode, rdev)` is the new entry point.
`new_rec` sets `nlink` 1 and `size` 0 for specials.
`setattr` size on a special is `Invalid`, and `file_rec` already refuses non-files (add the new arms).
`link`, `unlink`, `rename`, `unref` and `drop_inode` need no change: they test only for `Dir` and for `File` (chunks).
`drop_inode` deletes the whole inode prefix, so an unlinked device has nothing left to leak.

Snapshot and clone semantics: a snapshot is a content-addressed tree root and a fork shares nodes, so the special node is copied with the tree and nothing special happens.
Snapshots report the same kind and `rdev` as the live tree.
A node is immutable in a read-only snapshot view like any other (`ReadOnly`).
`Ino` uniqueness across snapshots is unchanged.

GC: garbage collection works on chunks and tree nodes.
A special node owns no chunks, so GC has nothing to add and nothing to skip.
`Removed.chunks` is empty for a freed special node, as for a symlink.

fsck (`cowfs_meta::check`):

- `FileType` code to `kind_code` mapping covers the four new kinds.
- A fifo, socket, or device must have no extents, no link target, `size` 0 and `covered` 0 (same shape as a directory with no entries).
- A fifo or socket must have `rdev` 0 and a 86-byte record; a device record must be 94 bytes.
- A special inode has no directory entries (the existing "non-directory inode has directory entries" rule).
- `nlink` accounting is unchanged: the number of directory entries naming the inode.
- New unit tests corrupt each rule and expect `check` to report it.

## Core (`cowfs-core`)

`Create` gains `Special { kind, rdev }`, written to the queue like the other creates.
`ns.rs::make` builds the in-memory node with `nlink` 1, `size` 0, no `file`, no `target`, and `Attr.rdev`.
The commit path (`inner.rs`) calls `tx.mknod` and retargets the dentry cache with the real kind.
`inner::kind_of` maps the four new `FileType` values.
`node.rs::report` reports `blocks` 0 for them (the existing wildcard arm already does).
`io.rs` read, write and truncate return `InvalidArgument` for the new kinds, matching symlinks.
`vfs_impl.rs` and `view.rs` forward `mknod`; `SnapshotView::mknod` forwards to Core like `create` and `mkdir`, and `ReadOnly` comes from Core when the parent is the synthetic root.
Reopening a store restores kind and `rdev` from the inode record.
The `Create` queue (`queue.rs`, `Op::Create`) is in memory only; `swap.rs` persists snapshot names, not operations.
So `Create::Special` is not an on-disk format.
What is replayed after a crash is the meta store's last durable commit, and a special node created after it is lost like any other uncommitted create.
The reopen test covers a clean sync, drop and reopen, and a meta test injects a failing commit (the existing `commit_fault` seam) and shows that neither the node nor version 3 reaches the disk.
A kill -9 test is not added: the node is an ordinary inode record in the same redb transaction as any other create.

## Wildcard audit

`FileKind` is `#[non_exhaustive]`, so the compiler does not find the places where the new kinds fall into an existing `_` arm.
Each one is decided here.

| site | today's `_` arm | decision |
| --- | --- | --- |
| `cowfs-core/src/io.rs` `file_node` (read, write) | `InvalidArgument` | keep: specials are `InvalidArgument` |
| `cowfs-core/src/io.rs` setattr size | `InvalidArgument` | keep |
| `cowfs-core/src/node.rs` `report` | blocks 0 | keep |
| `cowfs-core/src/inner.rs` `preserve_orphan` | nothing to preserve | keep: a special orphan has no data or target |
| `cowfs-core/src/inner.rs` commit retarget (`Create` to `FileKind`) | not a wildcard, exhaustive over `Create` | add the `Special` arm; if it were missing the dentry cache would hold the wrong kind |
| `cowfs-core/src/inner.rs` `kind_of` | exhaustive over `meta::FileType` | add four arms (compile error otherwise) |
| `cowfs-core/src/ns.rs` rename onto a special destination | rename logic tests only `Directory` | confirm by test: replace allowed |
| `cowfs-core/src/import.rs` | refuses specials | slice D |
| `cowfs-nfs/src/convert.rs` `ftype` | `NFS3ERR_SERVERFAULT` | unchanged in slice A (only `rdev: 0` is added to the `Attr` constructor); four arms and MKNOD in slice B |
| `cowfs-fuse/src/fs.rs` kind to `FileType` and `dir.rs` `DT_*` | wildcard (`EIO`) | unchanged in slice A; four arms and `mknod` in slice C |
| `cowfs-vfs-path/src/lib.rs`, `table.rs` | `NotSupported` | keep: PathVfs does not model specials |
| `cowfs-vfs-test` `MemVfs` | exhaustive `Body` enum | add a `Special` body |
| `cowfs-ctl/src/treehash.rs` | `other` | keep (see Import) |

Slice A forwards `mknod` only in the wrappers that sit in front of a real backend and are used by the slice A suites: `SnapshotView` and the `Keep` macro in `cowfs-core/tests/conformance.rs`.
The default `NotSupported` would otherwise make a wrapper silently refuse.
The other wrappers keep the default `NotSupported` in slice A and forward in slice B (NFS test wrappers) and slice C (FUSE test wrappers).
Wrappers found: `cowfs-core` `SnapshotView` (`view.rs`), the `Keep` macro in `cowfs-core/tests/conformance.rs`, `Watched` (`cowfs-nfs/tests/contract.rs`, `ns_durability.rs`), `ReusingVfs` and `CountingVfs` (`cowfs-nfs/tests/common`), `Probe` (`cowfs-fuse/tests/common/mod.rs`), the test doubles in `cowfs-fuse/src/dir.rs`, and `Bad` (`cowfs-vfs-test/tests/runner.rs`).
Full list of non-forwarding wrappers for slices B to D: `Watched` (`cowfs-nfs/tests/contract.rs`, `ns_durability.rs`), `SharedBackend` (nfs and daemon `separate_adapter_namespace.rs`), `ReusingVfs`, `CountingVfs`, `WatchVfs` (`namespace_race.rs`), `Probe`, `Zeroed`, `Gappy`, `Never` (fuse), `Bad` (`cowfs-vfs-test/tests/runner.rs`), `GuardedView` (daemon test).
Test doubles that never see a special create keep the default.
The daemon `GuardedView` test wrapper is forwarded in slice D.

## Import (`cowfs-core::import`)

Today import refuses a fifo, socket or device.
That is replaced: import creates the node through `mknod`, with kind from `FileTypeExt` and `rdev` from `st_rdev` converted to canonical form with `libc::major` and `libc::minor` (so macOS and Linux sources agree).
Mode is preserved like for any entry.
The read-back verification compares kind, mode and `rdev`, never content.
`cowfs_ctl::hash_tree` is not changed: it hashes a special node as `other` on both the source and the imported side, so the two still hash the same, and import's own comparison carries kind, mode and `rdev`.
No privilege is needed to import a device node because nothing is created on the host, only metadata in the store.
An unsupported source type (for example a Solaris door) is still refused with the same message.

## NFS (`nfsserve`, `cowfs-nfs`)

MKNOD arguments: `diropargs3 where`, `mknoddata3 what`.
`what` is a discriminated union on `ftype3`: `NF3CHR` and `NF3BLK` carry `devicedata3` (a `sattr3` and `specdata3 { specdata1, specdata2 }`), `NF3SOCK` and `NF3FIFO` carry a `sattr3` only, and any other type is `NFS3ERR_BADTYPE`.
`NFSFileSystem` (in `crates/nfsserve/src/vfs.rs`) gains `async fn mknod(&self, dirid, name, ftype, attr: &sattr3, rdev: specdata3) -> Result<(fileid3, fattr3), nfsstat3>`, with a default that answers `NFS3ERR_NOTSUPP` so other implementors are unaffected.
The handler follows `nfsproc3_create`: decode up front, GARBAGE_ARGS on a short body, reply cache for retransmits, `wcc_data` for the directory, and `post_op_fh3` and `post_op_attr` for the new node.
Reply when the file system does not support it stays `NFS3ERR_NOTSUPP` with a `wcc_data`.
`PATCHES.md` is updated: MKNOD is implemented.

`cowfs-nfs` adapter:

- `convert::ftype` maps the four new kinds to `NF3FIFO`, `NF3SOCK`, `NF3CHR`, `NF3BLK`.
- `convert::fattr` fills `rdev` from `Attr.rdev` (major to `specdata1`, minor to `specdata2`).
- NFS carries major and minor as separate 32-bit numbers, so no host encoding is involved.
- The adapter calls `Vfs::mknod`, applies the `sattr3` mode, and returns the file handle exactly as `create` does.
- `NF3CHR` and `NF3BLK` with an AUTH_SYS uid other than 0 answer `NFS3ERR_PERM`.
  The credential is in `context.auth`.
  The macOS and Linux clients already refuse the call in their own kernels for non-root, so this is defence in depth for a hostile client.
- The sidecar (`sidecar.rs`) and AppleDouble handling are for regular files and are not involved.

Client behaviour (to be checked by the live run after slice B, not assumed):

- macOS NFS client: `mkfifo(2)` and `mknod(2)` for a fifo, and `bind(2)` for an AF_UNIX socket, reach the server as `MKNOD` with `NF3FIFO` or `NF3SOCK`.
  Today these fail as `EIO` in the g3 run because the server says `NFS3ERR_NOTSUPP`.
  After creation the node is a vnode of type fifo, socket or device in the client.
  Open, read, write and poll of a fifo are handled by the client kernel and generate no read or write RPC.
  The client may send `SETATTR` for times when the fifo is closed, which `setattr` supports.
  Opening a socket or a device node gives `ENXIO` or `EOPNOTSUPP` locally, never an RPC.
- Linux NFS client: same shape; it also uses `MKNOD` for sockets.
- Device numbers on macOS are 8-bit major and 24-bit minor, on Linux 12 and 20 bits in the new encoding, and the wire carries the two numbers separately.

## FUSE (`cowfs-fuse`)

`fs.rs::mknod` dispatches on `mode & S_IFMT`:

- `S_IFREG` (or 0): existing `create` path.
- `S_IFIFO`, `S_IFSOCK`: `Vfs::mknod` with `rdev` 0.
- `S_IFCHR`, `S_IFBLK`: `Vfs::mknod` with `rdev` decoded from the kernel's `u32 rdev`, only when `req.uid() == 0`, else `EPERM`.
- anything else (including `S_IFDIR`, `S_IFLNK`): `EINVAL`.

Linux passes `rdev` in the kernel "new encoding": `major = (rdev & 0xfff00) >> 8`, `minor = (rdev & 0xff) | ((rdev >> 12) & 0xfff00)`.
The reply attributes encode back the same way.
`convert.rs` maps `FileKind` to the fuser `FileType` (`NamedPipe`, `Socket`, `CharDevice`, `BlockDevice`), and `dir.rs` maps it to `DT_*`.
`mknod_is_regular` and the `ENOTSUP` branch are removed.
`umask` is applied by the kernel before the call unless `FUSE_DONT_MASK` is negotiated, which the adapter does not do today, so mode handling is unchanged.
The Linux kernel handles open of a fifo (`fifo_open`), socket and device inode locally through `init_special_inode`, so no FUSE_OPEN reaches the daemon for them.
The kernel itself requires `CAP_MKNOD` in `vfs_mknod` for devices; the uid check is a second line.
A FUSE mount made by an unprivileged user gets `nodev`, so device nodes exist but cannot be opened as devices.
That matches native semantics and needs no code.

## Conformance suite (`cowfs-vfs-test`)

New module `special`.
Levels: `mknod_fifo_attrs` is `Posix` (believed identical on ext4, btrfs and APFS, but no native run backs that label yet, because PathVfs skips every special check), all others are `Cowfs` contract decisions.
Checks (names are the contract, and match `crates/cowfs-vfs-test/src/conformance/list.rs`):

- `mknod_fifo_attrs`: kind, mode, `nlink` 1, `size` 0, `blocks` 0, `rdev` 0, one creation time, lookup and getattr agree, and the parent's mtime and ctime moved.
- `mknod_socket_attrs`, `mknod_masks_mode` (type bits are not permission bits, setuid, setgid, sticky and rwx are kept).
- `mknod_device_attrs_keep_rdev`: char and block, `rdev` round trips through `mknod`, `getattr`, `lookup` and `readdir_attrs`, including the largest major and minor.
- `mknod_existing_is_exists`, `mknod_in_file_is_not_dir`, `mknod_stale_parent`, `mknod_invalid_names`.
- `mknod_rejects_bad_arguments`: Regular, Directory and Symlink kinds, and a device number on a fifo or socket, are `InvalidArgument` and create nothing.
- `special_readdir_kinds`.
- `special_io_is_invalid`: `read`, `write`, `setattr` size and `readlink` are `InvalidArgument`.
- `special_setattr_mode_and_times`: chmod and utimes apply and bump ctime.
- `special_open_release`.
- `special_hardlink_unlink_rename`: hardlink shares inode and `nlink`, unlink of one name keeps the other, rename over a special node replaces it, rename of a special node over a directory is `IsDir`.
- `special_unlinked_with_handle_survives`: `getattr` through the inode still works until `release`.
- `special_dir_ops_error`: `rmdir` of a special node is `NotDir`, `mkdir` over one is `Exists`.

`MemVfs` (the reference implementation) implements `mknod` and the above behaviours first, so the checks are red on the old trait and green on `MemVfs`.
Conformance consumers found: `MemVfs` (`cowfs-vfs-test/tests/memvfs.rs`), Core (`cowfs-core/tests/conformance.rs`), `PathVfs` (`cowfs-vfs-path/tests/common/mod.rs`, `Options` skip list), and the FUSE mount (`cowfs-fuse/tests/conformance.rs`).
`PathVfs` does not implement `mknod`, so its options skip the new checks with the reason "PathVfs does not implement mknod".
Slice D implements `PathVfs::mknod` on Linux (`mknodat`; macOS has none, so the native macOS PathVfs run skips the checks with that reason) and the FUSE conformance run no longer skips the 16 checks: the mounted MemVfs is driven through `PathVfs`, so the checks now reach FUSE `mknod` through a real kernel.
PathVfs opens a special node with `O_PATH|O_NOFOLLOW|O_NONBLOCK` (attributes, times and unlinked-while-open survival), changes its mode by name or through `/proc/self/fd`, and answers `read`, `write` and a size change with `InvalidArgument`.
A device node needs privilege on a real kernel, so the checks first probe with one device `mknod`; a backend that answers `PermissionDenied` is checked with the fifo and the socket only, and the device cases stay with MemVfs and Core.
The expected linux-fuse run count goes from 128 to 144.
`special_files_through_mknod` in `cowfs-fuse/tests/mount.rs` has a root branch for the device case, but CI runs as a normal user, so that branch has not run anywhere yet; the device cases are only exercised by MemVfs and Core.
The ctime-equality assertion of `mknod_fifo_attrs` was dropped for every backend, because PathVfs applies the mode after creating the node.
`PathVfs::mknod` works on Linux only.
`assert_skip_names` catches a misspelt name but not a missing entry, so each consumer is checked by running it.
The model-based tests (`cowfs-core/tests/model.rs`, `cowfs-vfs-test/src/model.rs`) get a `mknod` action in slice D, after the adapters exist.
`cowfs-core` adds reopen tests: kind and `rdev` survive a clean sync, drop and reopen, and a fork keeps them; a meta test injects a failing commit and shows the node and version 3 both stay off disk.
`cowfs-meta` adds record tests: encode and decode round trip for each kind, length 94 only for devices, fsck rules.

## Slices

| slice | crates | contents | proof |
| --- | --- | --- | --- |
| design | docs | this document | CI runs (path is not docs-only) |
| A | `cowfs-vfs`, `cowfs-vfs-test`, `cowfs-core`, `cowfs-meta` | type, `Vfs::mknod`, `rdev`, `MemVfs`, meta storage and fsck, core `Create::Special`, conformance and reopen tests, adapter compile fixes (`rdev: 0` only), skip entries for the PathVfs and FUSE conformance runs | conformance red first on MemVfs, then green on MemVfs and Core; meta tests; CI both OS |
| B | `nfsserve`, `cowfs-nfs` | MKNOD handler, fattr `rdev`, device uid check, update `mknod_pathconf19.rs` | wire-level tests; live run of `mkfifo/00.t` and `mknod/00.t` on this Mac's NFS mount |
| C | `cowfs-fuse` | `mknod` dispatch, rdev encoding, `FileType` maps | unit tests for encode and decode; Linux FUSE mount test in CI |
| D | `cowfs-core::import` | import of special nodes, docs | import tests with a real fifo and socket; device only where root |

Slice A has to compile every other crate, so it includes the mechanical `rdev: 0` edit in `cowfs-nfs` and `cowfs-fuse` (no behaviour change there; the refusals stay until B and C, and a special node reached through an adapter before then answers `EIO` or `NFS3ERR_SERVERFAULT`).
Slice D may merge into A if the diff stays small; it is kept separate so a critic can read the import verification change alone.

## Failing first

The old trait does not compile against the new checks, so "red on the old trait" cannot be observed.
Slice A is therefore committed in two steps and the history shows both.
Step 1: `FileKind` variants, `Attr.rdev`, the `Vfs::mknod` default, the `special` checks and `MemVfs` still on the default `NotSupported`.
The conformance run on `MemVfs` is recorded red for the new checks by name.
Step 2: `MemVfs`, meta, Core implement `mknod`, and the same run is green on `MemVfs` and Core.

## Tests that change

`crates/cowfs-nfs/tests/mknod_pathconf19.rs` pins the refusal today.
It is updated, not deleted: the MKNOD case now asserts success for fifo and socket, device creation as root, `NFS3ERR_PERM` as non-root, `NFS3ERR_BADTYPE` for regular and directory types, and `NFS3ERR_NOTSUPP` is no longer expected.
The PATHCONF half of that file is untouched.
The `cowfs-fuse` doc comment and tests that say mknod of a non-regular file is `ENOTSUP` are updated the same way in slice C.
The import test that expects a fifo to be refused (`crates/cowfs-core/tests/import.rs`) is updated in slice D.

## Risks

- Overlap with the parallel `Vfs::fallocate` change (#103) in `cowfs-vfs`, `cowfs-core` and `cowfs-fuse`, and with a later `FileTooBig` variant.
  Mitigation: only add lines, no reformatting, rebase on origin/main right before the PR.
- `Attr` gains a field, so every constructor changes.
  Mitigation: the compiler finds all of them; the edits are mechanical.
- Verified by the slice B/C run on this Mac (2026-10-09, `bench/out/special-107/run`): `mkfifo(2)` and `mknod(2)` of a fifo reach the server as MKNOD, `bind(2)` of an AF_UNIX socket creates the node (`open/24.t` 5/5 on both arms), and `mkfifo/00.t` and `mknod/00.t` give the same 25 ok / 11 not ok on the native and the cowfs arm (the 11 are root-only assertions).
- Found by that run (#204): a fifo created on the macOS NFS mount cannot be opened. See "Known limits" below.
- Still unverified: macOS client behaviour for fifo close.
  Mitigation: the live run after slice B is a gate, and anything unexpected there is reported as a finding, not guessed.
- Downgrade: an older build refuses a version 3 store up front (see Storage).

## Known limits

- macOS NFS client: `open(2)` of a fifo on the mount fails with `EACCES` (issue #204, kept open as a known limit).
  Cause: the client's `nfs_vnop_open` returns `EACCES` for any vnode that is not a regular file, directory or symlink, so the server sees no RPC.
  No server change and no mount option helps.
  `mkfifo` and `stat` of a fifo work; rename and unlink were not exercised.
  pjdfstest `open/17.t` #2 (expects `ENXIO`, macOS NFS gives `EACCES`) is this limit.
  Evidence: `docs/verification/evidence/nfs204-fifo-open.md`.
  The Linux FUSE path is not affected.

## Open questions for the advisor and critic

1. Settled after advisor review: lazy bump to version 3 (see Storage), because an older build would poison its session on `Corrupt`.
2. `rdev` as one `u64` on `Attr` and `meta::Attr`, versus a `DevNum` struct.
3. Device-creation privilege check in adapters only, versus a `Vfs` credential parameter later.
4. xattrs on special files: allowed (this design) versus `EPERM` like Linux.
