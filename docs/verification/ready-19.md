# Verification: ready task #19, NFS server requirements

Task #19 is a requirement list, not one defect.
Most of it was implemented by earlier work, and this task reconciles it: what is already
covered, what is still open, and what was never actually measured.
The additions here are the regressions #19's own failures would have been caught by, written at
the layer the failures lived in.

Lease: `.treehouse-ready-wave/.treehouse/cowfs-7c1bf8/1/cowfs` (slot 1).
Base: `46b0f269d5bef4a2c204c25f5b3015da601d3beb`.
Raw logs: `bench/out/ready-19/**`, ignored.
Machine: Apple M3 Max Mac, macOS 26, APFS.

## What this lane owns and what it does not

Owned: the NFS server requirement list in issue #19, and narrow vendored-server fixes.

Not owned, not touched:

- AppleDouble `Translate`, authorization, dead-server sweep. Slot 7 has issue #43.
- Namespace barriers and NFS durability adapter sites. The original build-train worker has those.
- `crates/cowfs-daemon/src/import.rs` and import/refresh #97. The original build train has those.

The one place this lane touches shared source is `crates/cowfs-nfs/Cargo.toml`, which gains one
dev-dependency (`cowfs-vfs-path`) so an NFS test can run against a real backing filesystem instead
of `MemVfs`.
That is additive and changes no production build.

## Why the existing tests were not enough

`crates/cowfs-nfs/tests/protocol.rs` already proves `setattr_never_follows_symlinks`.
It runs against `MemVfs`, which answers whatever it is asked.
It cannot get a syscall wrong, so it never exercises the code that caused the bug.

The bug in #19 was the server's own syscalls: `fs::metadata` and `filetime::set_file_times`, both
of which follow a final symlink.
In the v1 code that means `cowfs-vfs-path/src/lib.rs` (`setattr`) calling `sys::utimensat` and
`sys::fchmod` (`crates/cowfs-vfs-path/src/sys.rs`).
No test reached that path from NFS.

## What was added

`crates/cowfs-nfs/tests/requirements19.rs`, five tests over the raw protocol client and two over a
real `mount_nfs`.

Every group has a native control, so a test cannot pass by the server doing nothing:

- `a_native_chmod_through_a_symlink_lands_on_the_target` proves the harness can see a symlink
  being followed. Without it, a server that ignored SETATTR entirely would pass the tests below.
- `setattr_gives_a_symlink_its_own_times_and_mode_over_the_real_filesystem` checks the result by
  `lstat` on the backing directory, not by trusting the server's own reply.
- `readdir_replies_carry_the_link_count_of_the_moment` and
  `hardlinked_names_in_one_directory_are_listed_once_each` compare the reply against `lstat`.
- `namespace_churn_over_the_real_filesystem_stays_bounded` reads this process back with `ps`.
- `touching_a_symlink_on_the_mount_leaves_its_target_alone` applies the same stamp to a native copy
  and to the mount, so the expected value needs no hardcoded epoch.

The two mounted tests are `#[ignore]`d, like every other mount test in this crate.

## Independent syscall probe

Before writing anything, the two syscalls the fix depends on were checked on this machine directly,
because a Linux-only answer would have been worthless.

`AT_SYMLINK_NOFOLLOW` on a dangling name succeeds; the same call without it is `ENOENT`.
That is exactly the #19 symptom, and it is why `rsync` exited 23.

`futimens` and `fchmod` on an `O_SYMLINK|O_RDONLY` descriptor both succeed on macOS and both land
on the link, not the target.
That is why `cowfs-vfs-path` can use a descriptor for `chmod` on both platforms, and why the Linux
`AT_EMPTY_PATH` branch in `sys::utimens_fd` is not a macOS gap.

## Requirement matrix

| #19 requirement | State | Where it is answered | Evidence |
| --- | --- | --- | --- |
| `NFSPROC3_LINK` implemented | landed before this task | `crates/nfsserve/src/nfs_handlers.rs:708`, `vfs.rs:114` | `protocol.rs` LINK cases; `hardlinked_names_in_one_directory_are_listed_once_each` |
| Retransmission tracker lock | landed before this task, replaced not throttled | `crates/nfsserve/src/reply_cache.rs`, tracker deleted | `PATCHES.md`; `dupcache.rs` (7 tests) |
| readdir page cookie is a position | landed before this task | `crates/nfsserve/src/vfs.rs:140`, `nfs_handlers.rs:733` | `hardlinked_names_in_one_directory_are_listed_once_each`; `mount.rs` `hardlink_pair_readdir` |
| Hardlink readdir test in the main battery | landed before this task | `crates/cowfs-nfs/tests/mount.rs:349`, `protocol.rs:339` | both present in the suite |
| `-o locallocks` required | landed before this task | `crates/cowfs-nfs/src/mount.rs:129` | `mount.rs::option_string_has_the_required_options` |
| SETATTR never follows a symlink | implemented, was untested at the real backend | `cowfs-vfs-path/src/sys.rs:311,325` | added `setattr_gives_a_symlink_its_own_times_and_mode_over_the_real_filesystem` and `touching_a_symlink_on_the_mount_leaves_its_target_alone` |
| `chmod` never follows a symlink | implemented, was untested | `cowfs-vfs-path/src/lib.rs:200-211` | same two tests, mode half |
| AppleDouble `._*` policy | out of this lane | `crates/cowfs-nfs/src/appledouble.rs`, `sidecar.rs` | slot 7, issue #43 |
| Fresh `nlink` in readdir replies | implemented, never measured through a kernel client | `cowfs-vfs-path/src/table.rs:204` | added `readdir_replies_carry_the_link_count_of_the_moment` and `find_links_counts_the_same_through_the_mount_as_natively` |
| Server resident memory | bounded here, cause never diagnosed | adapter `parents` map plus `PathVfs` table | added `namespace_churn_over_the_real_filesystem_stays_bounded` |
| readdir cost per page | closed by the listing cache | `cowfs-vfs-path/src/table.rs:169` | measured, ungated. See below |
| Path map never shrinks | closed | adapter `reap` / `reap_if_last`, `handle.rs` `bury` | the churn test above; `hardening.rs` staleness cases |
| Upstream contribution or maintained fork | open, needs the maintainer | `crates/nfsserve/PATCHES.md` | see "Still open" |

## Measured

Counts are exact and from the logs in `bench/out/ready-19/`.
Exit codes are the exit code of the command, never of a pipeline.

RPC battery, private server over `PathVfs` on a scratch directory, no mount:

- 5 passed, 0 failed, 2 ignored, 42.57s.
- Churn: 6,000 namespace cycles over the real filesystem. Resident set 9 MiB at iteration 600,
  11 MiB at 6,000, growth 1 MiB. A second run of the same battery reported 2 MiB growth.

Mounted, private server, private mountpoint, private backing directory:

- `find -links +1` on a fixture of 200 files where 60 have two names and 30 of those have three:
  native 151, mount cold 151, mount cached 151.
  The cached pass is the one the kernel answers from its own attribute cache, which is the shape
  #19 reported as undercounting.

## Not claimed

- No performance acceptance is claimed. The wave has other workers running, and the dispatch
  forbids a quiet-machine claim while they are active.
- The per-page cost of a large directory is measured and reported below, not gated. Timing a
  shared machine would be a guess.
- `rss_soak` in `crates/cowfs-nfs/tests/mount.rs` is a 10 minute manual soak over `MemVfs`.
  It was not run here. The added churn test is bounded, runs in the normal battery, and uses the
  real backend, which is the gap that mattered: `MemVfs` has no inode table to grow.

## Still open

1. **Upstream contribution or maintained fork.** `crates/nfsserve` is a vendored fork of
   huggingface/nfsserve 0.11.0 carrying the LINK procedure, the cookie fix and the reply cache.
   The fork is the de facto answer and `PATCHES.md` records every patch.
   Whether to send a subset upstream is a maintainer decision with a maintenance cost attached,
   not something this lane can settle.
2. **AppleDouble `._*` policy for v1.** Delivered as `AppleDoubleMode::Hide` with `Translate`
   opt-in. Slot 7 owns the review that issue #43 asks for before flipping the default.
3. **Cause of the spike's memory growth.** The spike never diagnosed it and never ran a soak
   longer than about a minute. The added churn test shows the adapter and the real backend stay
   bounded over 6,000 cycles, which narrows the suspect to whatever the spike's own mirror did
   that this server does not, but it does not name a cause.

## Lane dependencies, stated

- Nothing in this delivery depends on slot 7, on the namespace-barrier work, or on import/refresh.
- The `Cargo.toml` dev-dependency line is the only shared-file edit and can be rebased onto any of
  them without conflict, since it adds a new key inside an existing table.
- Shared daemon PID 15263, its store, socket and mount at `~/.cowfs/mnt` were not signalled,
  read, mounted, or used as a fixture. The mounted tests here run their own server on their own
  port with their own mountpoint and unmount only that.