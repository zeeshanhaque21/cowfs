# Portability evidence: ready task #19, the Linux CI delta

Refs #19, which stays open. This is the evidence for the delta that made
`crates/cowfs-nfs/tests/requirements19.rs` portable, and for the limit it records rather than fixes.

Reviewed head: `395c2361b0c2bfcae41aa8d949fae53b9eb830eb`.
Head carrying this evidence: `4dc63ff7ce1b795d63265fc40a99b6434f8bc65d`.
Review that asked for it: `docs/reviews/server-requirements19-final.md`, sha256 prefix `7d975c80`.

## The two failures being repaired

CI run `37253685951` on `ubuntu-latest`, at the reviewed head:

```
test a_native_chmod_through_a_symlink_lands_on_the_target ... FAILED
test setattr_gives_a_symlink_its_own_times_and_mode_over_the_real_filesystem ... FAILED
test result: FAILED. 3 passed; 2 failed; 3 ignored
Process completed with exit code 101
```

- The native control expected `(493, 384)` = `(0o755, 0o600)` and got `(511, 384)` = `(0o777, 0o600)`.
  Linux does not store a mode on a symlink, so the link's own mode is not `0o755` there. The target
  did take `0o600`, which is the part the control exists to prove.
- The combined test asserted `OK` for a symlink `chmod` and got `5`, a non-OK `nfsstat3`.

## Cause, from the source this lane does not change

`crates/cowfs-vfs-path/src/lib.rs` sets a mode with `sys::fchmod` on whatever `open_kind` returned.

- macOS: `OPEN_SYMLINK = O_SYMLINK | O_RDONLY`, and `fchmod` on that descriptor changes the link.
  Probed directly on this machine before writing anything: it succeeds and the link's mode moves.
- Linux: `OPEN_SYMLINK = O_PATH | O_NOFOLLOW`, and `fchmod` on an `O_PATH` descriptor is `EBADF`.
  Linux has no `chmod` for a symlink at all.

The times half is portable: `sys::utimens_fd` carries an explicit Linux branch using `AT_EMPTY_PATH`.
The mode half has no such branch, so there is nothing portable to assert.

## What the delta does

One file changed. `git diff --name-only 395c236 4dc63ff` is
`crates/cowfs-nfs/tests/requirements19.rs`, 309 insertions and 50 deletions. No production source, no
manifest, no lock.

The native control now reads the link's own mode before the `chmod` and compares it after, so it
carries no host literal, and it asserts the target's move was a real change so the control cannot pass
by being a no-op.

The combined test became three:

| test | hosts | what it asserts |
| --- | --- | --- |
| `setattr_gives_a_symlink_its_own_times_over_the_real_filesystem` | both | link and dangling link take their own times; the target's mtime, its bytes and the link's target string are unchanged, read natively |
| `setattr_gives_a_symlink_its_own_mode_where_the_host_stores_one` | macOS | behind a capability read from the host; the link's mode moves and the target's does not |
| `setattr_refuses_a_symlink_mode_where_the_host_cannot_store_one` | Linux | the backend refuses when asked directly, the server does not report success, the target is untouched, and the status is named as an errno-mapping artefact rather than a contract |

The Linux test also runs a combined times-and-mode call and records whether the times landed before
the error. It asserts nothing about atomicity in either direction.

## Linux proof

Host: `moonscapenas`, Debian aarch64, kernel 6.12.109, rustc 1.95.0, 4 cores.
Source: `git archive` of `4dc63ff` into `/home/moonscape/cowfs-ready-wave/task-19/src-4dc63ff`,
531 files, archive sha256 prefix `3bfabf87673eb785` matching on both ends.
Remote logs: `/home/moonscape/cowfs-ready-wave/task-19/logs/`.

Identity and the locked-manifest guard, before any cargo command:

```
uname=Linux 6.12.109+rpt-rpi-2712 aarch64  rustc=rustc 1.95.0 (59807616e 2026-04-14)
706645b958fb89e4b90f56a4c31f9c04e68816e0a652137aec7960479628de63  Cargo.lock
cargo metadata --locked  ->  METADATA_EXIT=0
Cargo.lock after metadata, after the test runs and after clippy: unchanged, same sha256
```

That sha256 is the value the review recorded as the pristine lock, so the lock edge the PR carries
was already resolved and needed no rewrite.

Discovered on Linux: 10 tests, 7 default, 3 ignored. The macOS-only mode test is absent and the
Linux-only refusal test is present.

Default battery, the whole host-portable suite, one run:

```
LINUX_DEFAULT_EXIT=0
test result: ok. 7 passed; 0 failed; 3 ignored; 0 measured; 0 filtered out; finished in 27.97s

NATIVE_CHMOD link 777 -> 777, target 644 -> 600
SYMLINK_MODE host=linux backend_setattr=Some("i/o error: errno 9")
SYMLINK_MODE host=linux nfs_status=5 link=777 target=644 open_issue=19
SYMLINK_MODE_COMBINED host=linux status=5 times_applied_before_the_error=true target_mode=644
CHURN iterations=6000 rss_warm=8 MiB rss_end=10 MiB growth=1 MiB
```

`errno 9` is `EBADF`, which is the predicted cause, measured rather than quoted.
`nfs_status=5` is `NFS3ERR_IO`.

The required-mount guard, on a host that has no `mount_nfs`:

```
COWFS_REQUIRE_MOUNT=1 ... find_links touching
LINUX_MOUNT_REQUIRED_EXIT=101
UNMEASURABLE: mount_nfs is not available on this host; this run asked for a mount and did not get one, so it established nothing
test result: FAILED. 0 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out
```

This is the negative proof that a requested mount cannot turn into a green pass. Without the
variable the same two tests print `SKIP label=no-mount-capability` and pass, which is what a CI host
off macOS needs.

## macOS proof at the same head

Apple M3 Max, macOS 26, rustc 1.99.0.

```
RPC_EXIT=0        7 passed, 0 failed, 3 ignored, 27.91s
MOUNT_EXIT=0      2 passed, 0 failed, 8 filtered out, with COWFS_REQUIRE_MOUNT=1
PROTOCOL_EXIT=0   23 passed
VFSPATH_EXIT=0    33 passed
CLIPPY_EXIT=0     -D warnings, --all-targets
FMT_EXIT=0
```

From the mount run, with the receipts the delta added:

```
RECEIPT label=nfs-mount mount_dev=436209696 backing_dev=16777234 line=localhost:/cowfs-95ddcd5da5f1890a0242ebf2cdc88284 on /…/cowfs-nfs-ready19-1kY4t4/mnt (nfs, nodev, nosuid, mounted by zeeshanhaque)
LINKS native=151 mount_first=151 mount_cached=151
RECEIPT label=nfs-mount mount_dev=436209697 backing_dev=16777234 line=localhost:/cowfs-849133a5013a4ef121a8b6a6f61963a6 on /…/cowfs-nfs-ready19-qnYhHh/mnt (nfs, nodev, nosuid, mounted by zeeshanhaque)
RSYNC ok=true out=""
RECEIPT label=nfs-teardown … per test
```

Both device ids differ from the backing directory's `16777234`, which is what the new assertion
checks, and both mountpoints are private temporary directories of this run.
After the run, `mount | grep -c cowfs-nfs-ready19` is 0.

## Two Linux failures that are not this lane's

Both are outside `git diff --name-only 395c236 4dc63ff`, which is one file.

- `cowfs-vfs-path/src/tests.rs:328`, `readdir_sees_a_name_created_outside_the_vfs_while_a_listing_is_paged`,
  32 passed and 1 failed on this host. The same suite passes in CI on `ubuntu-latest`, so it is a
  property of this machine's filesystem. Not changed: another lane owns that file and that assertion.
- `cargo clippy -p cowfs-nfs --all-targets -- -D warnings` fails on Linux in
  `crates/cowfs-nfs/tests/contract.rs:233` at `assert!(post.is_none() || true)`. rustc 1.95's clippy
  flags it; 1.99's does not.
  Scoped to this lane's own target, `cargo clippy -p cowfs-nfs --test requirements19 -- -D warnings`,
  exits 0 on Linux.
  Not rewritten here. `contract.rs` asserts on the root-handle commit barrier and belongs to the
  barrier work in the original build train. The narrow follow-up is: replace the tautology with the
  assertion it was standing in for, in whichever lane owns that file.

## Not claimed

- No fix for Linux symlink `chmod`. The requirement is unmet there and #19 carries it as open.
- No performance claim. The readdir and resident-memory figures in `ready-19.md` are two bounded
  author runs and one bounded run per host on a shared machine. Neither is a budget, and the
  resident-memory bound is not a leak result.
- No claim that these Path-backend figures are real-Core acceptance for #15 or any pjdfstest gate.
- The mount cases are manual-only. No CI job passes `--ignored` for `-p cowfs-nfs`.
- The delta was not re-run against the combined head of PR #111 or the Translate guard. That
  combination is still owed.

## Residue

- Shared daemon PID 15263, its store, socket and mount untouched on the local Mac.
- Linux artifacts under `/home/moonscape/cowfs-ready-wave/task-19/` only: three source archives, one
  shared target directory, the log set, and the three run scripts. No other worker's directory was
  written, and no lease, mount or device was touched on either host.