# Open issues triage, 2026-10-09

Audited against origin/main `fb64023efd6beaa53692a33019e24b7dc587f9be` (merge of PR #233).
Method: a scratch clone at that SHA was read, nothing was built or run, no issue was closed, edited or commented on.
39 issues were open at audit start.
Issues #167, #170 and #178 are already CLOSED (PR #231) and were only spot-checked: `crates/cowfs-daemon/tests/base_refresh_hardening.rs` holds their tests (plain-snapshot refusal :105, concurrent refresh :175, long name refused at the first call :229).
Tags: (V) verified by reading code or tests at the cited path, (I) inferred from comments, docs or the issue text, not confirmed in code.
Statuses: DONE, PARTIAL, OPEN, DECISION (needs Zee).
Other agents were changing main concurrently, so later SHAs may differ.

## Summary table

| # | Issue | Verdict | Recommendation |
|---|---|---|---|
| 1 | spike: dedup ratio | DONE | close |
| 2 | spike: mmap and locking over NFS | DONE | close |
| 3 | spike: build and git status overhead | DONE as a spike, real-backend numbers live in gates g1/g3 | close, file a gate issue |
| 4 | spike: .git inside the filesystem | DONE as a spike, one DECISION left | close, file a measurement issue |
| 5 | spike: treehouse process detection | DONE | close |
| 6 | spike: artifact byte-identity | DONE | close |
| 7 | v1: chunking, hashing, block store | DONE | close |
| 8 | v1: Merkle tree and O(1) snapshots | DONE except the seeding measurement row | close, split the seeding row |
| 9 | v1: redb metadata, crash consistency | DONE | close (g6 stays on #173) |
| 10 | v1: garbage collection | PARTIAL, no scheduled gc | keep open, one remaining item |
| 11 | v1: Linux FUSE mount | DONE | close |
| 12 | v1: macOS NFS loopback mount | DONE | close |
| 13 | v1: CLI and control API | DONE | close |
| 15 | v1: treehouse mode (a) | PARTIAL | keep open, 4 items |
| 16 | v1: treehouse mode (b) | PARTIAL | keep open, 3 items, tied to #123 |
| 19 | v1: NFS server requirements | DONE | close |
| 20 | v1: open-fd holders | DONE except upstream filing | close, upstream filing is a DECISION |
| 21 | v1: data integrity | PARTIAL | keep open, narrowed to the soak |
| 26 | v1: cowfs-core | DONE except the perf row | close, perf row belongs to gates |
| 29 | v1: Vfs, MemVfs, conformance suite | DONE | close |
| 34 | v1: cowfs-vfs-path | DONE | close |
| 37 | post-v1: other macOS mount route | OPEN by design | keep open, deferred |
| 42 | meta and ctl requests from core | 3 of 5 DONE, 2 PARTIAL | close and split into 2 issues |
| 43 | NFS adapter follow-ups | PARTIAL | keep open, 4 items |
| 45 | FUSE torn read | DONE (cause found, gated, covered) | close |
| 80 | bench partial coverage | DONE | close |
| 101 | g5 xfstests blocked | PARTIAL, the title is false on main | retitle, keep open |
| 103 | fuse fallocate ENOTSUP | DONE | close |
| 109 | rmdir .. and nlink | DONE | close |
| 121 | control shutdown terminal frame | OPEN, unreproduced | keep open until bytes-logged failure |
| 123 | tree-native warm-base publication | PARTIAL | split into 2, keep 123 as tracker |
| 128 | slow-client wait vs bound | DONE in code, one geometry not re-run | close or one re-run (DECISION) |
| 171 | namespaces follow-ups | DONE except older kernels | close |
| 173 | fault-injection seam | 2 of 3 gaps DONE | keep open, slice 3 power loss |
| 176 | orphan staging sweep | OPEN | keep open (PR #220 in flight) |
| 177 | roll forward pending intent | OPEN | keep open (PR #220 in flight) |
| 204 | fifo open EACCES | OPEN, cause unknown | keep open, DECISION on the gate class |
| 211 | PathVfs mknod chmod | PARTIAL | keep open, 5 items |
| 232 | g2 spread bound | OPEN | keep open |

Top closures: #1-#6, #7, #9, #11, #12, #13, #19, #20, #26, #29, #34, #45, #80, #103, #109, #128, #171, and #42 (after splitting).
That is 24 closures with evidence below.

## Per-issue detail

### #1 spike: dedup ratio on the real worktree corpus
Row: ingest worktree pools plus one Node project, report ratio with do-nothing baseline and sample sizes. DONE. The comment gives 45 slots, 843,889 files, 14.04x with FastCDC plus zstd against 1.00x, and the write-up is `docs/spikes/1-dedup-corpus.md` on main. (V for the file, I for the numbers)
Recommendation: close. Residual caveat (97.5 percent of raw is cargo `target/`) is recorded in the spike.

### #2 spike: mmap and locking over the NFS loopback
Row: rustc, linkers and git work over an in-process NFS loopback. DONE, 24 of 24 checks, `docs/spikes/2-nfs-loopback.md`. (V file, I numbers)
The "FUSE-T fallback" is moot because the loopback is viable, and `crates/cowfs-nfs/src/mount.rs:120` carries the required `locallocks`. (V)
Recommendation: close.

### #3 spike: build and git status overhead
Row: measure cargo build and git status on FUSE and NFS against native, budget 1.5x. DONE for passthrough servers on both platforms (`docs/spikes/3-linux-fuse.md`). (V file)
Row: not measured then: real cowfs backend, large workspace, large tracked tree. OPEN. These are exactly gates g1 (clean build), g2 (edit and rebuild) and g3 (git status) in `bench/gates.py:67`, and `docs/verification/g1-g2-readiness-20261009.md:4` states "No g1 or g2 gate number exists in this document". (V)
Success criterion 2 of `docs/design.md:107-118` therefore has no gate result on the real backend.
Recommendation: close the spike, and file one issue per unproduced gate result (see the missing-issues list).

### #4 spike: .git inside the filesystem
Row: check git-heavy workloads meet the budget with .git in cowfs, fall back to a passthrough overlay for .git only. DONE as a spike (`docs/spikes/4-git-inside-the-filesystem.md`). Result: budget missed on writes (add -A 3.9x, commit 3.9x) with .git inside, overlay fixes about half. (V file)
The spike's own conclusion: adopt the overlay only if write latency stays over budget on the real backend. `docs/design.md:122` still lists the overlay as a fallback. No overlay exists in the code (no match in crates). (V by absence)
Row: real-backend git add and commit latency. OPEN, no issue tracks it.
Recommendation: close the spike, file a "measure .git write cost on the real backend, decide overlay" issue. DECISION D4.

### #5 spike: treehouse process detection
Row: confirm `treehouse return` detects lingering processes on the mount. DONE, `docs/spikes/5-treehouse-process-detection.md`; the gap it found is #20. (V file)
Recommendation: close.

### #6 spike: artifact byte-identity across slots
Row: measure byte-identity with and without path remapping. DONE, `docs/spikes/6-artifact-byte-identity.md`, debug slot 2 costs 12.8 and 28.0 percent without remap, 0.55 and 0.74 percent with remap plus `CARGO_INCREMENTAL=0`. (V file)
The follow-up decision (cargo is covered) is implemented in `crates/cowfs-treehouse/src/mode_b.rs:638-646`. (V)
Recommendation: close.

### #7 chunking, hashing, block store
Body rows: FastCDC, BLAKE3, zstd, append-only packs, hash-on-read verification. DONE.
FastCDC `crates/cowfs-store/src/chunk.rs:1,47` (16/64/256 KiB); BLAKE3 `crates/cowfs-store/src/lib.rs:40`; zstd `record.rs:8`; packs `pack.rs`; hash verified on `get` `store.rs:1224` raising `HashMismatch` at `store.rs:1281`; `fsck` `store.rs:1420`. (V)
Crash and corruption tests: `crates/cowfs-store/tests/{crash,integrity,durability,round5}.rs`. (V by listing)
Recommendation: close.

### #8 Merkle tree and O(1) writable snapshots
Row: directory tree as a Merkle structure. DONE, `crates/cowfs-meta/src/db.rs:2039` (Merkle root), `node.rs:9`. (V)
Row: snapshot is a metadata copy. DONE, `db.rs:2046` `fork` "costs one row and one counter plus the commit, independent of tree size". (V)
Row (added comment): measure seeding on the real store, snapshot time and first `cargo build --frozen` Fresh at a new path, n=5, with hardlink count preserved (322). OPEN. No `--frozen` appears anywhere in `bench`, `crates`, `scripts` or `docs/*.md`, and `real_project_acceptance.rs:19` says "what is still not proven is a warm `target/` in the base". (V by absence)
Recommendation: close and move the seeding row into the g1/g2 gate issue or the #123 remainder A issue.

### #9 redb metadata with crash consistency
Row: transactional metadata with crash-injection tests, zero torn trees. DONE. redb `crates/cowfs-meta/Cargo.toml:13`; crash tests `crates/cowfs-meta/tests/{crash,kill9,recovery40,m2_pending_window40}.rs`, core `tests/{crash,kill9}.rs`. (V)
Recommendation: close. Gate g6 (daemon-level zero data loss) is tracked by #173.

### #10 garbage collection
Row: mark-and-sweep from snapshot roots. DONE, `crates/cowfs-gc/src/lib.rs`, tests `core_end_to_end.rs`, `core_reclaim.rs`, `race.rs`. (V)
Row: incremental marking over unchanged subtrees. DONE, `lib.rs:6-9` (shared `Marker`, persistent marked-root set). (V)
Row: batched last-accessed hints as a sweep-candidate hint only. DONE, `lib.rs:21`, `Hints` and `flush_hints` `lib.rs:232`, coldest-first ordering `lib.rs:446`. (V)
Row (design.md:40): "on demand and on a schedule". OPEN. A grep for schedule, interval and periodic across `crates/cowfs-daemon/src`, `cowfs-gc/src` and `cowfs-cli/src` finds no scheduler. (V by absence)
Recommendation: keep open with one item: scheduled gc in the daemon (or decide on demand only and amend design.md).

### #11 Linux FUSE mount
Row: POSIX-complete FUSE adapter. DONE. `crates/cowfs-fuse/src/fs.rs` implements mknod (:618), setxattr (:931), lseek (:998), fallocate (:1017), copy_file_range (:1032). (V)
CI job `linux-fuse` (`.github/workflows/ci.yml:150`) runs the FUSE conformance suite and is in the `check` aggregate (:105). (V)
Gated skips: `concurrent_readers_and_writers_of_one_file` and `statfs_free_after_unlink` at `crates/cowfs-fuse/tests/conformance.rs:77-95`, both with a stated cause. (V)
No `getlk` or `setlk` handler in `fs.rs`, so POSIX locks are kernel-local on a mount. Locking passes the suite, so this is acceptable for a single mount. (I)
Recommendation: close.

### #12 macOS NFS loopback mount
Row: in-process NFS adapter, FUSE-T fallback. DONE, `crates/cowfs-nfs`, vendored server `crates/nfsserve`. The fallback was not needed (spike #2 viable). (V)
Remaining NFS follow-ups are #43, #204 and #37.
Recommendation: close.

### #13 CLI and Unix-socket control API
Row: snapshot create and rm, gc, fsck, import, base refresh. DONE, `crates/cowfs-cli/src/cli.rs:58-157` (Snapshot create/rm/reset/promote, Gc, Fsck, Import, Base refresh), control API `crates/cowfs-ctl`. (V)
Recommendation: close.

### #15 treehouse mode (a), transparent integration
Row: treehouse root on the mount, unmodified treehouse. DONE as a live trial (`docs/live-treehouse.md`, `docs/live-trial-metrics.md`, comment on the issue). (V docs exist)
Row: main checkouts on the mount. OPEN. The live-trial comment says main checkouts and shared git object databases "remain native unless separately cloned or imported". (I)
Row: unexplained `npm ci` ENOENT and `TAR_ENTRY_ERROR` warnings on the mount. OPEN. `docs/nanomuse-monitor.md:27-28` says "The cause is not established" and "No full install or build success has been verified". (V)
Row: measured acceptance. OPEN, gated by g1/g2/g3.
CONTRADICTION: `docs/live-treehouse.md:26` tells users to run `scripts/treehouse-cowfs.sh`. That file is not on origin/main. It exists only as an untracked file in the primary checkout. (V)
Recommendation: keep open. Remaining: commit the launcher, an `npm ci` reproduction with a native control, main-checkout migration, measured acceptance.

### #16 treehouse mode (b), snapshot-native slots and warm base
Row: slot create and reset as O(1) clone of a warm base. DONE for a source-only base (`warm_base_acceptance_over_a_real_core`, `crates/cowfs-treehouse/tests/real_project_acceptance.rs:2968`, ignored for time, about 17 minutes). (V)
Row: warm base contains build artifacts. OPEN. `BaseRefresh::run` builds in a leased slot and then calls `base_refresh`, which checks out a fresh worktree, so the built tree is never published. (I from the decision memo, `docs/warm-base-123-decision-20261009.md`)
Row: companion calls `mount_snapshot`. OPEN. `CowfsMaterialiser::materialise` always returns `Unsupported` and `available()` is false, `crates/cowfs-treehouse/src/mode_b.rs:29-45`. The error text claims the protocol has no `mount_snapshot` method, which is stale because `Handler::mount_snapshot` exists (`crates/cowfs-daemon/src/handler.rs:~492`). (V)
Row: upstream provisioner extension if hooks are insufficient. PARTIAL, `docs/upstream-treehouse-proposal.md` is written and says "Nothing here is merged". (V)
Recommendation: keep open, tracked through the two #123 remainders.

### #19 NFS server requirements and nfsserve fixes
All boxes are unchecked in the body, nearly all are done.
- LINK implemented: `crates/nfsserve/src/nfs_handlers.rs:34,138`, `PATCHES.md:8`. (V)
- TransactionTracker scan removed, replaced by `reply_cache.rs`: `PATCHES.md:9`. (V)
- Readdir cookie fix and hardlink test: `crates/cowfs-nfs/tests/requirements19.rs:358` `hardlinked_names_in_one_directory_are_listed_once_each`, `readdir_cookie21.rs:89`. (V)
- `locallocks`: `crates/cowfs-nfs/src/mount.rs:120,130`. (V)
- SETATTR on symlinks: `requirements19.rs:85,123,195,244`. (V)
- Stale nlink in readdir replies: `requirements19.rs:324`. (V)
- AppleDouble policy: `AppleDoubleMode::Hide` default, `Translate` opt-in, `crates/cowfs-nfs/src/lib.rs:9-16`. (V)
- Large directory read once per listing: `requirements19.rs:396`; bounded path map and memory: `requirements19.rs:459`, `resource_bounds.rs`. (V)
- Upstream versus fork: settled in practice as a vendored fork with `PATCHES.md`; `crates/nfsserve/README.md:62` still carries the upstream "Seeking Contributors" text. (V)
Recommendation: close.

### #20 open-fd holders not detected by treehouse return
- Propose upstream (fd-aware detection): PARTIAL. The proposal is written (`docs/upstream-treehouse-proposal.md`) but not sent. DECISION D5. (V file)
- cowfs-side control API listing holders: DONE, `crates/cowfs-daemon/src/holders.rs` (1037 lines, process-table scan, lsof and /proc, bounded, refuses a short answer: header :3-25, `/proc/locks` :152-181, lsof exit-1 handling :276 and :466). (V)
- Handle `.nfs*` entries: DONE for mode (a), `crates/cowfs-treehouse/src/holders.rs:9-48` (`nfs_entries`, `wait_for_nfs_clear`) and `docs/v1-treehouse.md:224`. (V)
- Verify when the `.nfs` file disappears: DONE, `docs/v1-treehouse.md:450-452`. (V)
- Linux and FUSE re-check: DONE, commit 20d0e64 "Linux run on moonscape, 49 passed"; tests `crates/cowfs-treehouse/tests/issue20.rs`. (I for the run, V for the file)
The BLOCK from the PR #112 review (lsof exit 1 treated as empty, `/proc/locks` index error) was repaired in 3226b76; the cited code shows the process-table form and an explicit locks parser. (V)
Recommendation: close, and file the upstream proposal as its own decision item.

### #21 data integrity (spike 4 findings)
- Write race on read-only-mode files: DONE in the spike server, regression check in the spike battery (issue text). The Core stores modes as metadata. (I)
- Unexplained corrupt `.idx`: OPEN. `docs/verification/ready-21.md:11` "NOT REPRODUCED in the declared window", `:72` "Not a soak"; the review `docs/reviews/git-integrity21-final.md:399` says the 805 MiB soak "remains outstanding". (V)
- Verify the fix on the large (805 MiB) repo: OPEN, same soak.
- Readdir cookie position-based: DONE on the Core path. `crates/cowfs-nfs/tests/readdir_cookie21.rs:89,93` pin stability across edits between pages; the PR #104 probes found 0 remaining entries on both arms. (V file, I probes)
- AppleDouble sidecars piling in a backing store: moot on the Core (no backing directory), handled by Hide/Translate. (I)
- Add delete-while-listing and repack plus fsck loop to the main battery: PARTIAL. The repack and fsck loop lives in `scripts/verify-git-index-integrity.py`, a manual harness, not a CI gate. (V listing)
- Owner can write a file created read-only: no direct test found. (I)
Recommendation: keep open, retitle to "bounded soak: repeated gc, repack -adf and fsck --full on a large repo through the Core mount". DECISION D3 on whether a soak is required or the issue closes as not reproducible.

### #26 cowfs-core
- Reads and verify, write-back and re-chunking, flush and fsync, sparse and truncate: DONE (`crates/cowfs-core/src/{io,file,queue,blocks}.rs`, `vfs_impl.rs:92-104`, tests `core.rs`, `chunks.rs`, `flush_boundary.rs`). (V)
- Open-handle and forget tracking: DONE (`vfs_impl.rs:17,88`, `io.rs:582`, tests `alias.rs`, `poison.rs`). (V)
- Synthetic mount root of snapshots with unique inodes: DONE (`ns.rs:8,53,62`, `ino.rs`, `names_ino.rs`). (V)
- Passes `cowfs-vfs-test` and crash tests: DONE (`tests/conformance.rs`, `crash.rs`, `kill9.rs`). (V)
- Durability batching: DONE, `flush_interval` default 500 ms `inner.rs:34,63`. (V)
- Performance on seeded build and git status: OPEN, no gate result (see #3).
Recommendation: close, performance is tracked by the gate issues.

### #29 cowfs-vfs and conformance suite
Rows: `Vfs` trait (`crates/cowfs-vfs`), `MemVfs`, generic suite with 156 listed checks (`crates/cowfs-vfs-test/src/conformance/list.rs`), covering names, links, rename, xattrs, readdir cookies, special files, readonly modes. DONE. (V)
Recommendation: close.

### #34 cowfs-vfs-path
Rows: PathVfs over std::fs and libc (`crates/cowfs-vfs-path`, `sys.rs:239` fchmodat), suite run on native filesystems as control and through FUSE and NFS. DONE. CI `linux-fuse` runs the native control (`ci.yml:171-176`), results recorded in the issue comment from PR #35. (V)
Known client-side limits are recorded (silly rename, `crates/cowfs-nfs/tests/known_limits.rs`). (V)
Recommendation: close.

### #37 evaluate another macOS mount route (post-v1)
Rows: FUSE-T, macFUSE, other routes, workloads, compare to budget, quiet-machine retighten. All OPEN by design; no measurement of any alternative exists in the repo (only `docs/design.md:59,116` mention macFUSE). (V by absence)
Precondition "after v1 is complete" is not met, because gates g1/g2 have no result.
Recommendation: keep open, deferred. DECISION D6.

### #42 meta and ctl requests from core
Audit method: an earlier comment maps 5 requests at 460e1ae; re-checked at fb64023.
1. Atomic snapshot rename: PARTIAL. `Meta::rename_snapshot` `crates/cowfs-meta/src/db.rs:1865`, `Core::rename_snapshot` uses it, test `crates/cowfs-core/tests/core_atomic_rename.rs`. But `promote_base` still goes through `swap_snapshot`, which forks twice (`crates/cowfs-core/src/swap.rs:240` and `:271`, entry `lib.rs:388-390`), so ids and inode numbers still change on promote. (V)
2. Shared snapshot-name crate: DONE, `crates/cowfs-snapname` with wrappers in core and ctl, drift test `crates/cowfs-daemon/tests/snapname_drift.rs`. (V)
3. Hole flag in `ChunkRef`: DONE, `cowfs-meta/tests/hole_flag.rs`, `cowfs-core/tests/hole_walk.rs`. (V file listing, I line detail from the audit comment)
4. Inode reservation: PARTIAL. `Meta::reserve_inodes` `db.rs:1910` and Core consumption are done; Core still has `virt.ino` and the alias table (`crates/cowfs-core/src/ino.rs:13-14,191-198`). (V)
5. Operation time: PARTIAL. `Tx::set_now` exists and Core stamps per operation (PR #136, `cowfs-meta/tests/operation_time.rs`); `batch_at` was never added (`grep batch_at crates` is empty). The goal is met by `set_now`. (V)
Recommendation: close #42 and file 2 issues: (a) end the double fork in `promote_base` (needs an atomic replace-by-rename in meta, DECISION D13), (b) retire `virt.ino` and the alias table now that reserved inodes exist.

### #43 NFS adapter follow-ups
- Round-3 critic of Translate code (sidecar, xattr encoding, locking, generations): OPEN. No such review exists in `docs/reviews` (only `nfs-contract117-final.md`, `pr143-...`). (V by absence)
- Security review of MNT export, root handle, MAC, xid eviction, second uid: OPEN, per the issue comments and the build-train note the hardening builder is still working; two suspected BLOCK findings are "reasoned only". (I)
- Dead-server hang, wire `install_signal_cleanup` and `sweep_stale_mounts` into the daemon: DONE, `crates/cowfs-daemon/src/mounts.rs:94,101,111-118`, `daemon.rs:254-270`. (V)
- Store-mode leaves `._` files after checkout: PARTIAL, `crates/cowfs-nfs/tests/store_checkout43.rs` exists, assertion strength not checked. (I)
- Conformance through the NFS mount with the raw client: PARTIAL (raw-protocol tests exist, `protocol.rs`, `translate.rs`; no full-suite raw run found). (I)
- Warm-build edit-and-rebuild vs macOS budget with the real core: OPEN, gate g2 has no result.
Recommendation: keep open with these four items; do not flip the Translate default (D10).

### #45 FUSE conformance: torn read
Cause: Linux page cache, not an adapter bug. Same test body tore 181 of 200 through FUSE, 72 of 200 on native btrfs, 29 of 200 on tmpfs (issue comment). (I)
Gated, not hidden: both skips carry reasons at `crates/cowfs-fuse/tests/conformance.rs:77-95`, and the check is documented in `crates/cowfs-vfs-test/src/lib.rs:120`. (V)
Replacement coverage: `crates/cowfs-fuse/tests/coherence.rs:262,277,293` (4 KiB atomicity in the Core view, acknowledged-write visibility mount vs native, uniform blocks). (V)
Reviewer wording (`docs/reviews/fuse-coherence45-repair-final.md:251`): if closed, record only the scoped native page-cache cause.
Recommendation: close, noting that the skipped statfs-after-forget check has no mount-level equivalent.

### #80 bench partial gate coverage
All six acceptance rows DONE: coverage reported per skipped gate and arm (`bench/compare.py:156-175`), scope printed (`:486-493`), docstring `:34-40`, one-sided gates, multiple native files, noise-floor file and refusal cases tested in `bench/test_compare_coverage.py:109-333`. (V)
Recommendation: close.

### #101 g5 xfstests blocked
CONTRADICTION: the title and body say the host cannot build `ltp/fsstress` and `ltp/fsx` unprivileged. `docs/reviews/g5-status-20261009.md` records an unprivileged `make -k -j4` at the pinned tree on the cachyos box producing both, and says "The #101 build blocker is removed". (V)
Rows now: build blocker DONE; differential harness DONE (`bench/g5_diff.py`, `bench/g5_root.sh`, `bench/test_g5_diff.py`, PR #200); acceptance receipt OPEN. `docs/g5-harness-redesign.md:173-192` says the wide run is diagnostic, generic/127 fails twice on cowfs, generic/247 unmounts `TEST_DIR`, and the 6-id sample and wide list "must be re-run on the final script" with the daemon sha pinned. (V)
Also open: widening the 6-id allowlist, 511 scratch-device cases needing root or a CAP_SYS_ADMIN container.
Recommendation: retitle to "g5: produce the xfstests-generic differential receipt", keep open. DECISION D7.

### #103 fuse fallocate ENOTSUP
Rows: modes allocate, keep-size, punch-hole, zero-range now answered. DONE. `crates/cowfs-fuse/src/convert.rs:250-266` maps modes 0, 1, 3, 0x10, 0x11 and refuses the rest with ENOTSUP; Core `vfs_impl.rs:136`, `view.rs:188`; PathVfs `lib.rs:532`; tests `crates/cowfs-core/tests/fallocate.rs`, `crates/cowfs-vfs-path/src/tests.rs:826`. (V)
Collapse, insert and unshare range stay ENOTSUP by design (not in the issue's POSIX list).
Recommendation: close. Follow-ups from the critics are listed under missing issues.

### #109 rmdir .. and nlink after unlinking an open file
- Symptom 1, `rmdir a/b/..` EINVAL: DONE, commit ecc7022, test `crates/cowfs-nfs/tests/rmdir_dotdot109.rs`. (V)
- Symptom 2, nlink 1 after unlink of an open file: macOS client silly-rename, not fixable server side. Pinned in `crates/cowfs-nfs/tests/unlink_open_nlink109.rs`, and the g3 accepted divergence landed in `bench/pjdfstest-accepted-divergences.json:12-17` (unlink/14.t #4, issue #109). (V)
- Symptom 3, truncate timestamp checks: the comment says it does not reproduce. (I)
The last open condition in the PR #215 comment ("until the accepted-divergence entry lands") is met.
Recommendation: close.

### #121 control shutdown terminal-frame test intermittently fails
The original failure log is gone. Not reproduced in 200 loaded runs (PR #230 comment), 30 of 30 in `docs/reviews/ctl-issues-status-20261009.md`. A related race (kill during a terminal write) was fixed by PR #194 (#127). (I)
One CI sample returned wait_ms=1687 with no frame and bytes were not logged. The test now logs nothing new, so the evidence the issue waits for cannot arrive until a failure happens. (I)
Recommendation: keep open as a watch item, or close with a rule "reopen on any CI recurrence" (D9).

### #123 tree-native Core warm-base publication
- Tree-native publication via existing seams: DONE (`Backend::ingest_replacing` `crates/cowfs-daemon/src/backend.rs:119,616`, `import::replace_tree`, `Handler::base_refresh` without `can_ingest` `handler.rs:483`, doc `docs/warm-base-publication-123.md`). (V)
- Provenance reachable by caller: DONE (gate `the_core_daemon_publishes_base_refresh_and_leaves_no_worktree`, `real_project_acceptance.rs:1353`). (V)
- Two fresh slots, isolation, reset, fresh-daemon readback: DONE for a source-only base (ignored acceptance `:2968`). (V)
- Slots include build artifacts: OPEN (remainder A).
- `mount_snapshot` invoked by the companion: OPEN (remainder B, `mode_b.rs:29-45`). (V)
- #97 repair merged: DONE (decision memo). (I)
Recommendation: split into A (publish a built tree, needs a wire field, D2) and B (companion mounts and unmounts), close #98 if still open, keep #123 as tracker or close it after the split. The lead's memo `docs/warm-base-123-decision-20261009.md` has the options.

### #128 reconcile slow-client wait measurement
Production fix: one absolute `grace_end` (`crates/cowfs-ctl/src/server.rs:116,137,157-166,298`), commit 2257796. Tests: `a_slow_reading_client_does_not_stretch_wait_past_the_deadline_plus_one_grace` (`progress_shutdown.rs:466`), `shutdown_budget_*` (`:991,1134,1187`). (V)
The 2322 ms vs 1800 ms sample was taken before the fix, on blob 5eb3f6bc, and `progress_shutdown.rs:464` states the new test "guards the bound but does not reproduce the 2322 ms of case B". Case B geometry was not re-run on main. (V)
Recommendation: close after one re-run of case B on macOS, or close on the strength of the fix and test (D9).

### #171 namespaces follow-ups
- CI runs isolation tests: DONE, job `linux-namespaces` lifts the AppArmor sysctl and enforces 9 isolation tests ran (`.github/workflows/ci.yml:121-147`), in the `check` aggregate. (V)
- Cargo covered by the byte-identical promise: DONE, canonical route sets `CARGO_INCREMENTAL=0` (`crates/cowfs-treehouse/src/mode_b.rs:638-646`, test `canonical.rs:640`); measured 43 of 43 files identical on btrfs, ext4, XFS (`docs/linux-namespaces.md:158-173`). (V)
- Core backend `base_refresh`: DONE, `handler.rs:483`. (V)
- Real treehouse lease on Linux: DONE on the cachyos box with treehouse v3.1.2 (`docs/linux-namespaces.md:161-169`). (V file, I run)
- btrfs, XFS: DONE. Older kernel than 6.12: OPEN (`docs/linux-namespaces.md:171` "Not covered: any kernel older than 6.12"). (V)
- Also not covered: leased slot on the FUSE mount, release builds, registry dependencies.
Recommendation: close, optionally file "older-kernel run" as low priority.

### #173 fault-injection seam
- Gap 1 mid-gc crash: DONE, slice 2 (PR #189) kills the daemon during a real sweep, n=1..18 all pass (`docs/crash-injection-173.md:114`). (V)
- Gap 2 internal orderings: DONE at process-exit level for the store boundaries, slice 1 (PR #183, `crates/cowfs-gc/tests/crash_inject.rs`, feature `fault-injection` in `crates/cowfs-store/src/fsio.rs`). (V)
- Constraint "compiled out of release": DONE and gated, `scripts/check-fault-seam-absent.sh` with a positive control, CI job `fault-seam` (`ci.yml:88`). (V)
- Gap 3 power loss: OPEN. `docs/crash-injection-173.md:134,145-149` says process exit keeps the page cache, "no slice so far makes a power-loss claim", real power loss needs a VM harness. (V)
Recommendation: keep open for slice 3 only. DECISION D8.

### #176 Core::open does not remove orphan staging snapshots
OPEN on main. `crates/cowfs-core/src/swap.rs:113` `recover` walks only intents, and the comment at `:172` says "`Core::open` does not sweep orphans either (issue 176)". (V)
PR #220 is open to fix this and #177; per the build-train note it was blocked by a critic for two data-loss paths. (I)
Recommendation: keep open.

### #177 roll forward a pending swap intent before a same-name retry
OPEN on main. `swap.rs:169-175` unregisters a leftover staging snapshot first even when an intent names it, which the comment calls "the only copy of the new tree". `replace_with_staged` (`:212`) has no `fault()` seam and `ingest_replacing` has no failure-at-every-step test. (V)
Recommendation: keep open, same PR #220.

### #204 macOS NFS: open() of a fifo gives EACCES
OPEN, cause not determined. Documented in the crate docs, `docs/special-files-107.md`, `crates/cowfs-nfs/tests/known_limits.rs:1-5`, and listed as the one established regression in `bench/pjdfstest-accepted-divergences.json:5-10`. (V)
Untried leads from the issue: `mount_nfs` option variations, a second NFSv3 server as control, Linux NFS client behaviour.
Effect: g3 on macOS stays FAIL until this is fixed or accepted (`docs/reviews/g3-status-20261009b.md`).
Recommendation: keep open. DECISION D1.

### #211 PathVfs::mknod chmod follows symlinks and coverage gaps
- chmod by name following symlinks: DONE for the first window, `crates/cowfs-vfs-path/src/sys.rs:239-248` uses `AT_SYMLINK_NOFOLLOW` (PR #214). The residual swap-to-another-special-node race between `mknodat` and `fchmodat` (`lib.rs:305-306`) remains, fix needs fd-based chmod with a dev and inode check. (V)
- Device fallback depends on the backend's own PermissionDenied: not verified fixed. (I)
- Core model `Entry` has no rdev: OPEN, `crates/cowfs-core/tests/model.rs:353-362` has kind, mode, nlink, size, digest, target, xattrs, group only. (V)
- `devices_need_privilege` decided by the owner of `/proc/self`: OPEN, `crates/cowfs-fuse/tests/mount.rs:28`, and `grep CAP_MKNOD crates` is empty. (V)
- Root branch of `special_files_through_mknod` never runs in CI: OPEN (no root runner in `ci.yml`). (I)
- Doc run count `docs/special-files-107.md:279`: not checked.
Recommendation: keep open with 5 items.

### #232 g2 spread bound
OPEN. `bench/compare.py:89` `G2_SPREAD_MAX = 2.0` and `:378-380` apply a ratio, not an absolute bound against the 1.0 s budget. `G2_MIN_UNITS = 3` in `bench/gates.py:79` and `compare.py:250`, against about 127 units. (V)
Exit-code masking and `cargo build --tests` under the mount: not checked. (I)
Recommendation: keep open. DECISION D12 on the spread rule.

## Issues needing a decision from Zee

- D1 (#204): fix the NFS fifo open, or add an "accepted established regression" class to g3 with sign-off, or keep g3 red. Options: try `mount_nfs` variations and a second NFSv3 server (cheap, finds the cause); accept it as a documented client limit like #109; leave it failing.
- D2 (#123 remainder A): how a built tree reaches the base. Options from the memo: (1) optional `from_slot` or `from_path` on `base_refresh` with a daemon check that the directory is a worktree at the resolved commit (recommended, wire change); (2) `import` plus `promote` with no provenance (reintroduces #98); (3) keep source-only and warm lazily.
- D2b (#123 remainder B): companion mounts after create and unmounts in its return hook (recommended), or the daemon auto-mounts on `snapshot_create --at` (wire change), or leave mounting to the caller.
- D3 (#21): require the 805 MiB soak before closing, or close as not reproducible on the Core after the bounded 42-operation run.
- D4 (#4): when to measure real-backend `git add` and `commit` and adopt the .git passthrough overlay (threshold: over budget on the real backend).
- D5 (#20): send `docs/upstream-treehouse-proposal.md` to treehouse, or keep it parked.
- D6 (#37): keep deferred until g1/g2 produce results, or start FUSE-T now (needs admin to install).
- D7 (#101): acceptance box and allowlist growth for g5, and whether a root or CAP_SYS_ADMIN container is acceptable for the 511 device cases.
- D8 (#173 slice 3): power-loss mechanism, write-ordering simulation at the store boundary or a VM power-cut harness (`crates/cowfs-store/tests/qa4-vm.sh` shows the shape).
- D9 (#121, #128): close both with a reopen-on-recurrence rule, or keep #121 until a failure with bytes-received logged.
- D10 (#43): flip the Translate default only after the round-3 critic and the security review pass.
- D11 (gates): approve quiet-host runs of g1/g2 on macOS and cachyos, and the Linux FUSE arm of g3.
- D12 (#232): bound spread by seconds against the budget (spread seconds <= budget/2) or a relative-to-budget rule.
- D13 (#42 split): add a Meta API that replaces a snapshot's tree by rename, so `promote_base` stops forking twice, or accept id changes on promote.
- D14 (#15): migrate the main checkouts onto the mount, or declare mode (a) limited to new pools.

## Work with no issue (candidates to file)

Source: `docs/build-train-ideas-and-concerns-20261009.md` plus findings above.
- N1: gates g1, g2 (macOS and cachyos quiet-host runs), g3 Linux FUSE arm, g4 ext4 arm have no tracking issue, only `progress/plan.json`. One issue per unproduced gate result. (V no open issue; docs/verification/g1-g2-readiness-20261009.md:4)
- N2: fsx matrix does not compare `st_blocks` (zero-range drops cowfs from 128 to 64 while btrfs stays 128). Source: train doc line 19.
- N3: PR #205 follow-ups: the `fallocate_modes_reach_the_kernel` SKIP passes silently on CI (should panic when CI is set), len and range checks should run before `open_rw`, no unit test for the symlink and macOS fallocate branches (`crates/cowfs-vfs-path/src/tests.rs:826`).
- N4: PR #208 follow-ups: the g2 corpus file drifts by one comment line per rep, `compare.py` does not check `cowfs_ctl` is in `rebuilt_units`.
- N5: `scripts/treehouse-cowfs.sh` is untracked but referenced by `docs/live-treehouse.md:26`. Commit it or fix the doc. (V)
- N6: `CowfsMaterialiser` error text says the protocol has no `mount_snapshot`, which is stale (`mode_b.rs:35`). Folded into #123 remainder B. (V)
- N7: same-name `promote` racing `base_refresh` is not prevented (they take different locks), from reading code, untested. Source: train doc line 119.
- N8: NFS MOUNT EXPORT procedure leaks the export path and the one-shot mount gate re-admits a later MNT (reasoned only, to be reproduced). Belongs to #43 or its own issue once reproduced.
- N9: GC on a schedule is missing (see #10). (V)
- N10: seeding measurement on the real store (snapshot time plus first `cargo build --frozen` Fresh, hardlink count 322, n=5) from the #8 comment, never done. (V)
- N11: treehouse lease hygiene: leases fill the pool (16 of 16), `treehouse return` prompts without stdin, no owner or expiry. A script that audits leases against PR state. Source: train doc lines 41-49 and 91.
- N12: repo has no branch protection, the `check` aggregate is the only gate. Source: train doc line 36.
- N13: rotate `CACHY_OS_PASS`, which an old pause note still lists. Source: train doc line 94.
- N14: PR #222 changed-tests selector completeness (PATH-resolved binaries and new helper names evade the scan guard). Source: train doc line 108.
- N15: `progress/plan.json` carries a diff of about 1500 lines not on main, and review docs sit as untracked files or in a stash named `pre-main-update-2`. Hygiene issue so the decision log is not lost. Source: train doc lines 61-72.
- N16: g3 PATH_MAX overlay (PR #233) must stay restricted to the known macOS client condition or it can hide a real pathconf regression on Linux. Source: train doc line 111.
- N17: older-kernel namespace run (from #171).
- N18: collapse, insert and unshare range for fallocate are ENOTSUP (optional, from #103).
- N19: promote of a long base name (`swap-<name>` intent file over NAME_MAX): `base_refresh` refuses long names (`base_refresh_hardening.rs:229`) but `intent_path` is still `swap-<target>` (`swap.rs:48-50`). Whether `promote_base` and snapshot swap guard it was not checked. (I)

## Limits of this audit

Nothing was built or executed; every (V) is a read of code, tests or docs at fb64023.
Test names and line numbers were confirmed present, but a passing result was not observed.
The issue comments were read in full for the claims quoted; some long comments were truncated at 1,500 characters in the working copy, so a few late details (the second half of the #43 coverage audit, the last paragraphs of #15 and #21) are taken from the visible part.
Issues #167, #170, #178 were only spot-checked.
Issue #98 is referenced by the decision memo but was not in the open list.
