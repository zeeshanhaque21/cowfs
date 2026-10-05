# Verification: ready task #19, NFS server requirements

Refs #19. That issue is a requirement list, not one defect, and it stays open.
This is the harness that checks those requirements plus a statement of what is still open.

Lease: `.treehouse-ready-wave/.treehouse/cowfs-7c1bf8/1/cowfs` (slot 1).
Portability delta reviewed against head `395c2361b0c2bfcae41aa8d949fae53b9eb830eb`.
Head carrying this document: `4dc63ff7ce1b795d63265fc40a99b6434f8bc65d`.
Raw logs: `bench/out/ready-19/**`, ignored. Portability evidence:
`docs/verification/evidence/server-requirements19-portability.md`.

## What backend these figures are about

Every number here is `cowfs_vfs_path::PathVfs` over a scratch directory on the host, reached
through a real in-process NFSv3 server over a real TCP socket, or through a real kernel NFS client
mounting that server.

- Not `MemVfs`. `common::serve` starts a real `Server` and the client opens a real connection and
  performs a real MNT.
- **Not the Core backend.** These are Path-backend cases. Nothing here is real-Core acceptance for
  issue #15 or for any pjdfstest gate.
- `common::serve` sets `opts.check_peer_uid = false`, a test-only relaxation, acceptable for a
  one-shot server on 127.0.0.1 behind a secret export name.

## Hosts

| | macOS | Linux |
| --- | --- | --- |
| what | Apple M3 Max, macOS 26, APFS | `moonscapenas`, Debian aarch64, 6.12.109 |
| rustc | 1.99.0 | 1.95.0 |
| how reached | directly | `git archive` of the exact head into `/home/moonscape/cowfs-ready-wave/task-19/src-4dc63ff` |

The Linux source is an archive of the commit, not a working tree of it. 531 files extracted.
Archive sha256 matched on both ends, `3bfabf87673eb785…`.
`Cargo.lock` sha256 `706645b958fb89e4b90f56a4c31f9c04e68816e0a652137aec7960479628de63` was identical
before and after every cargo command on both hosts, and identical to the value the review recorded.
`cargo metadata --locked` exited 0 on Linux and rewrote nothing.

## Test inventory, measured

macOS discovers 10 tests, 7 default and 3 ignored.
Linux discovers 10 tests, 7 default and 3 ignored.
The difference is which symlink-mode test exists: `setattr_gives_a_symlink_its_own_mode_where_the_host_stores_one`
is macOS-only, `setattr_refuses_a_symlink_mode_where_the_host_cannot_store_one` is Linux-only.
Both hosts run the other six.

Default battery, one run each, `--test-threads=1`:

| | passed | failed | ignored | time |
| --- | --- | --- | --- | --- |
| macOS | 7 | 0 | 3 | 27.91s |
| Linux | 7 | 0 | 3 | 27.97s |

## What the portability delta changed

CI run `37253685951` failed 2 of 8 tests on `ubuntu-latest`, both asserting macOS symlink semantics
with no host gate. Both were real test defects, in a suite whose whole point is a syscall.

`a_native_chmod_through_a_symlink_lands_on_the_target` compared a symlink's own mode against a
literal `0o755`. macOS stores a mode on a symlink and reports 755, Linux stores none and reports
777. The control's purpose is that a followed `chmod` moved the target, so the link's mode is now
read before the `chmod` and compared after, and the target's move is asserted to be a real change
rather than a no-op that passes twice.

Measured, both hosts, from the run logs:

```
macOS  NATIVE_CHMOD link 755 -> 755, target 644 -> 600
Linux  NATIVE_CHMOD link 777 -> 777, target 644 -> 600
```

`setattr_gives_a_symlink_its_own_times_and_mode_over_the_real_filesystem` asked one call to do both,
which cannot hold on a host with no `chmod` for a symlink. It is split into three.

**Times, both hosts.** This is the portable half and the one #19's `rsync` failure was about. It
checks that the link takes its own times, that a dangling link does too, and that the target's mtime,
its bytes and the link's target string are all unchanged, read natively.

**Mode where the host stores one, macOS only.** Behind a capability check that reads the host's own
symlink mode instead of assuming it, so a host that stopped storing one records that rather than
failing on a Mac-only expectation. Measured:

```
SYMLINK_MODE_CAPABILITY host=macos native_link_mode=755
SYMLINK_MODE host=macos status=0 link 755 -> 600 target=644
```

**Mode where the host cannot store one, Linux only.** This is an open limitation, recorded, not
fixed, because fixing it means changing production code this lane does not own. Linux has no
`chmod` for a symlink: there is no `fchmodat` that takes `AT_SYMLINK_NOFOLLOW`, and
`cowfs-vfs-path` opens the link `O_PATH | O_NOFOLLOW`, on which `fchmod` is `EBADF`
(`crates/cowfs-vfs-path/src/sys.rs`). Measured on Linux:

```
SYMLINK_MODE host=linux backend_setattr=Some("i/o error: errno 9")
SYMLINK_MODE host=linux nfs_status=5 link=777 target=644 open_issue=19
SYMLINK_MODE_COMBINED host=linux status=5 times_applied_before_the_error=true target_mode=644
```

Three things follow from those three lines.

- The backend asked directly refuses too, so the refusal belongs to the host and the backend, not
  to the protocol layer inventing an error.
- `nfs_status=5` is `NFS3ERR_IO`, which is what the errno mapping produces today. The test names
  the status it saw and calls it an artefact of that mapping, **not a contract**: a later fix may
  legitimately answer `NOTSUPP`, or succeed on a host that grows the capability. The test asserts
  only that the server does not report success and that the target is untouched.
- A combined times-and-mode call applies the times and then fails on the mode.
  `times_applied_before_the_error=true` is the measured order on this host. No atomicity is claimed
  for that call in either direction, and none is asserted.

## Requirement matrix

| #19 requirement | State | Where it is answered | Evidence |
| --- | --- | --- | --- |
| `NFSPROC3_LINK` implemented | landed before this lane | `nfs_handlers.rs:708`, `vfs.rs:114` | `protocol.rs` LINK cases; `hardlinked_names_in_one_directory_are_listed_once_each` |
| Retransmission tracker lock | landed, tracker replaced by a reply cache | `reply_cache.rs` | `PATCHES.md`; `dupcache.rs` |
| readdir page cookie is a position | landed before this lane | `vfs.rs:140`, `nfs_handlers.rs:733` | `hardlinked_names_in_one_directory_are_listed_once_each`; `mount.rs` `hardlink_pair_readdir` |
| Hardlink readdir test in the battery | landed before this lane | `mount.rs:349`, `protocol.rs:339` | both present |
| `-o locallocks` required | landed before this lane | `mount.rs:129` | `option_string_has_the_required_options` |
| SETATTR never follows a symlink, times | implemented, untested at the real backend until this lane | `cowfs-vfs-path/src/sys.rs` | `setattr_gives_a_symlink_its_own_times_over_the_real_filesystem`, both hosts |
| SETATTR never follows a symlink, mode | implemented on macOS, **unavailable on Linux** | `cowfs-vfs-path/src/lib.rs`, `sys.rs` | macOS: `setattr_gives_a_symlink_its_own_mode_where_the_host_stores_one`. Linux: refusal recorded, open |
| AppleDouble `._*` policy | out of this lane | `appledouble.rs`, `sidecar.rs` | slot 7, issue #43 |
| Fresh `nlink` in readdir replies | implemented, never measured through a kernel client | `table.rs:204` | `readdir_replies_carry_the_link_count_of_the_moment`; `find_links_counts_the_same_through_the_mount_as_natively` |
| Server resident memory | bounded in one bounded run per host, cause undiagnosed | adapter `parents`, `PathVfs` table | `namespace_churn_over_the_real_filesystem_stays_bounded` |
| readdir cost per page | closed by the listing cache, measured not gated | `table.rs:169` | `a_large_directory_is_read_once_per_listing` |
| Path map never shrinks | closed | adapter `reap`, `handle.rs` `bury` | the churn test; `hardening.rs` staleness cases |
| Upstream contribution or maintained fork | open, needs the maintainer | `crates/nfsserve/PATCHES.md` | see "Still open" |

## The mounted cases, and why they are manual

Two tests mount a real filesystem, and no CI job runs them. `.github/workflows/ci.yml` runs
`cargo test --workspace`, which does not pass `--ignored`; the only job that runs ignored tests
names `-p cowfs-vfs-path --test native` and `-p cowfs-fuse`. So for `-p cowfs-nfs` these two are
manual-only, and a green default run says nothing about them.

They are also the only coverage of two things only a kernel client can get wrong: its attribute
cache, and a `touch` of a symlink becoming SETATTR on the link's own handle.

Both used to `return` when the mount was unavailable, which made a green run of those two names
mean nothing. Now:

- every successful mount prints a receipt with the mount line and the device ids of the mountpoint
  and of the backing directory, and asserts the two differ, so a green run cannot be a plain read
  of the backing directory;
- a mount that did not happen is an error when `COWFS_REQUIRE_MOUNT=1` is set, carrying
  `UNMEASURABLE`, so a manual acceptance run cannot pass without a mount;
- without that variable it stays a labelled `SKIP label=no-mount-capability`, which is what a CI
  host off macOS needs;
- a unit test covers that decision with no filesystem involved, so the guard cannot rot.

macOS, `COWFS_REQUIRE_MOUNT=1`, one run:

```
RECEIPT label=nfs-mount mount_dev=436209696 backing_dev=16777234 line=localhost:/cowfs-95ddcd… on /…/mnt (nfs, nodev, nosuid, mounted by zeeshanhaque)
LINKS native=151 mount_first=151 mount_cached=151
RSYNC ok=true out=""
RECEIPT label=nfs-teardown … (per test)
```

`find -links +1` on a fixture of 200 files where 60 carry a second name and 30 of those a third:
native 151, mount cold 151, mount cached 151. The cached pass is the one the kernel answers from
its own attribute cache, which is the shape #19 reported as undercounting.
`rsync -a` of a tree holding a valid symlink and a dangling one, in a `node_modules/.bin` layout,
onto the mount: exit 0, where #19 recorded exit 23 on exactly that shape.

The same run on Linux, with `COWFS_REQUIRE_MOUNT=1` and no `mount_nfs` on the host, is the negative
proof that the guard is real rather than a claim:

```
UNMEASURABLE: mount_nfs is not available on this host; this run asked for a mount and did not get one, so it established nothing
test result: FAILED. 0 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out
```

That exit is 101 and it is correct. It is not a defect in this lane.

## Resident memory: one bounded observation, not a leak result

`namespace_churn_over_the_real_filesystem_stays_bounded` churns 6,000 namespace cycles over the real
filesystem and reads this process back with `ps`.

| | warm at iteration 600 | at 6,000 | growth |
| --- | --- | --- | --- |
| macOS | 9 MiB | 11 MiB | 2 MiB |
| Linux | 8 MiB | 10 MiB | 1 MiB |

The assertion is `growth < 64 MiB`, against an observed 1 to 2 MiB.

That is a single bounded run per host on a shared machine. It is **not** a proof that nothing leaks,
and it does not diagnose anything. The adapter `parents` map plus the `PathVfs` table, named as the
suspect, is a hypothesis. The spike's own memory growth was never diagnosed and no soak longer than
about a minute was ever run there. Root cause stays open.

## readdir paging: measured, not a budget

12,000 entries in one directory, 64 entries requested per page, 204 pages, two author runs on a
shared machine:

| Figure | run 1 | run 2 |
| --- | --- | --- |
| first page | 40.64 ms | 28.97 ms |
| mean of pages two onwards | 1.61 ms | 1.05 ms |
| native read of the same directory | 7.81 ms | 7.00 ms |
| native read plus one `lstat` per entry | 57.03 ms | 43.12 ms |

#19 recorded 23 to 71 ms per page on a 10,000 to 15,000 entry directory.

The single asserted bound is `mean of pages two onwards < one native directory read`. That
comparison separates a cached listing from a re-read one on its own: a page from a cache pays for
its own entries, and a page that re-reads pays at least one whole read. It is deliberately loose and
it is **not** a performance budget. No quiet-machine claim is made anywhere: the wave has other
workers running, two runs is not a distribution, and the 1.5x build-overhead criterion in
`AGENTS.md` is not touched by any of this. The 23 to 71 ms figure is the old spike reading from a
different machine and different code.

## Regressions, and two failures that are not this lane's

At `4dc63ff`, exit codes taken from the command itself:

| | macOS | Linux |
| --- | --- | --- |
| `cargo test -p cowfs-nfs --test requirements19` | 0 | 0 |
| `cargo test -p cowfs-nfs --test requirements19 -- --ignored find_links touching` | 0 | 101, the required-mount guard |
| `cargo test -p cowfs-nfs --test protocol` | 0 | 0 |
| `cargo test -p cowfs-vfs-path --lib` | 0 | 101, see below |
| `cargo clippy -p cowfs-nfs --all-targets -- -D warnings` | 0 | 101, see below |
| `cargo clippy -p cowfs-nfs --test requirements19 -- -D warnings` | n/a | 0 |
| `cargo fmt --all -- --check` | 0 | 0 |

Both Linux failures are in files this lane does not touch. `git diff --name-only 395c236 4dc63ff` is
exactly one path, `crates/cowfs-nfs/tests/requirements19.rs`.

- `cowfs-vfs-path/src/tests.rs:328`, `readdir_sees_a_name_created_outside_the_vfs_while_a_listing_is_paged`,
  fails on this particular host. The suite passes in CI on `ubuntu-latest`, so this is a property of
  this machine's filesystem, not a code change. Not investigated further here: it is another lane's
  file and the assertion is not this lane's to change.
- `cargo clippy` on Linux fails in `crates/cowfs-nfs/tests/contract.rs:233` on
  `assert!(post.is_none() || true)`, a tautology that rustc 1.95's clippy flags and 1.99's does not.
  It is outside this lane's diff and outside its ownership, so it is proposed here rather than
  rewritten: `contract.rs` wants a real assertion, and the barrier work in the original build train
  owns that file.

## Still open on #19

1. **The fork or upstream contribution decision** for vendored `nfsserve`. It is a fork of
   huggingface/nfsserve 0.11.0 carrying the LINK procedure, the cookie fix and the reply cache.
   `PATCHES.md` records every patch. Sending a subset upstream is a maintainer decision with a
   maintenance cost attached.
2. **AppleDouble `._*` policy.** `AppleDoubleMode::Hide` is the default and `Translate` is opt-in.
   Slot 7 owns the review issue #43 asks for. No default may move on this evidence.
3. **Symlink `chmod` on Linux.** `fchmod` on the `O_PATH | O_NOFOLLOW` descriptor is `EBADF` and
   the server answers `NFS3ERR_IO`. #19's requirement is unmet on Linux. Closing it needs a
   production decision in `cowfs-vfs-path`, which is not this lane's surface.
4. **Server resident-memory root cause.** Undiagnosed, as above.
5. **A store-side growth question**, referred to in review as a Store leak. No issue in the open list
   tracks it and nothing in `docs/` records it. If it is real it needs filing; issue #10 is the
   nearest subject.
6. **#107 typed `create` for FIFO and socket**, **#108 `pathconf`**, **#109 `rmdir` of a trailing
   `..` plus `nlink` after unlinking an open file**, **#110 `chown` to another uid**. New gate
   findings, assigned to the requirement lane, untouched here.

## What this lane owns

The #19 requirement list and narrow vendored-server fixes.

Not owned and not touched: AppleDouble `Translate`, authorization and the dead-server sweep (slot 7,
issue #43); the namespace barriers and NFS durability adapter sites (original build train);
`crates/cowfs-daemon/src/import.rs` and import/refresh #97 (original build train).

The only shared-file change in the whole PR is one dev-dependency,
`cowfs-vfs-path` in `crates/cowfs-nfs/Cargo.toml`, so an NFS test can run against a real filesystem.
It is additive and changes no production build. The portability delta adds no manifest or lock edit
at all.

Shared daemon PID 15263, its store, socket and mount at `~/.cowfs/mnt` were never signalled, read,
mounted or used as a fixture. The mounted tests run their own server on their own port with their own
mountpoint and unmount only that, verified against the native mount table.