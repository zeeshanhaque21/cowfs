# Issue 173 slice 3+: power-loss mechanism for gate g6 (decision memo)

Status: proposed, design only, no code changed in the repo; reviewed once by the advisor; falsifying spike written but not run (section 7).
Date: 2026-10-09.
Refs: issue 173, `docs/crash-injection-173.md` (slices 1 and 2, merged), `docs/reviews/issue88-status-20261008.md`, `docs/v1-core.md` section "Ordering with the store".

Evidence tags on every claim:
[read] verified by reading the named file on main at 84bd305.
[spike] verified by the throwaway spike in section 7.
[reasoned] argued but not checked, so treat it as a hypothesis.

## 1. Decision

Recommend option (a): a write-ordering power-loss simulation at the two existing durability seams, the store's `Io` and redb's `StorageBackend`, run as ordinary `cargo test` in CI.
Defer option (b), a real power-cut harness, behind a named trigger (section 5.4).
Option (c), the hybrid, is the upgrade path once that trigger fires; it is not recommended now.
The claim (a) supports is narrow and must be stated in that form: "cowfs issues the right fsyncs in the right order under a modelled POSIX crash semantics".
It does not claim "a real kernel, disk or NFS stack honours them".

## 2. Coverage that exists today

### 2.1 Process crash (the page cache survives)

- Daemon SIGKILL matrix, 29 of 29 on post-#96 main, finite and sampled [read: `docs/reviews/issue88-status-20261008.md`].
- Mid-GC daemon crash at every store `Io` boundary: n=1..18 die with status 77 inside a real reclaiming gc and restart clean; the sweep crosses the source-pack unlink (present for n=1..13, gone for n=14..18) [read: `docs/crash-injection-173.md`, slice 2, run `f173-c`].
- Crate level, the same sweep over a real `Gc::collect` in `crates/cowfs-gc/tests/crash_inject.rs` [read].
- Per-crate SIGKILL loops: `cowfs-meta/tests/kill9.rs`, `cowfs-core/tests/kill9.rs`, `cowfs-gc/tests/kill9.rs` [read].
- `crates/cowfs-gc/tests/crash.rs` calls its images "power cut", but it builds them by driving compaction to a step and dropping the process, so every unsynced write is still in the image [read: its module doc and `drive_to`].
  It is process-crash fidelity, not power-loss fidelity.

### 2.2 Power loss (unsynced writes may vanish)

Three power-loss models already exist, each inside one crate:

- Store: `crates/cowfs-store/tests/crash.rs` replays the thread-local `oplog` of put and sync histories and drops unsynced writes in any subset, torn at 512 B or 4 KiB sectors, and drops newly created files when no directory fsync followed [read].
  It replays the first edge of the gap-2 chain, pack fsync before watermark advance, because `Store::sync_capture` fsyncs the pack and then writes and fsyncs `SYNCED` through the same `Io` [read: `store.rs` `sync_capture`, `wm.rs` `write_at`].
  Catch power for that edge: mutant W1 ("watermark written before data fsync") in `crates/cowfs-store/tests/mutate.py` was recorded as killed (48 of 50 killed, survivors W7 and H5) [read: `docs/v1-store.md` "Mutation testing"].
  Which test killed it was not checked, so it may be a static ordering test rather than the power-loss model [reasoned].
  The W1 pattern no longer matches current source (it expects `.advance(&self.io, Mark ...)`, the code now calls `.advance(Mark { pack: id, len })`), so a rerun today reports PATTERN NOT FOUND, not a kill [read: `mutate.py` line 24, `store.rs` `sync_capture`].
  Status: replayed, catch power on current code unverified.
- Meta: `crates/cowfs-meta/tests/crash.rs` records redb through a custom `StorageBackend` and rebuilds images under three loss policies [read].
  Wrapping redb's file ops is therefore not an open question: `Meta::open_with_backend(impl StorageBackend, ..)` is already public and used by two test suites [read: `crates/cowfs-meta/src/db.rs`].
- Core: `crates/cowfs-core/tests/crash.rs` puts meta on the recording backend and cuts the store, then walks every chunk list [read].
  It has a negative control: with meta's `before_sync` hook removed, it must find a missing live block (`the_crash_test_notices_a_missing_store_sync_before_metadata_commits`) [read].
  This covers the second edge, store sync before metadata commit.

### 2.3 Static ordering assertions

- `crates/cowfs-store/tests/compact.rs` asserts from the oplog that `finish_compaction` fsyncs the new pack between markers 9101 and 9102 and that the packs directory is fsynced between markers 9001 and 9002 (around the unlink) [read].
  These check the order in which calls are made, not what survives a cut.

## 3. What remains (exact)

R1. GC under power loss: no test drops unsynced writes during compaction or discard.
`cowfs-gc/tests/crash.rs` and `crash_inject.rs` are both process-crash fidelity [read].

R2. The unlink is invisible to the model.
`discard` calls `fs::remove_file` directly, not through `Io`, and logs only `Marker(9001)` before it [read: `compact.rs`].
`LogOp` has no `Unlink` and no `Rename`, and `LogOp::DirSync` does not say which directory [read: `fsio.rs`].
So "unlink durable only after the packs dir fsync" cannot be modelled, and mutation 2 from `docs/crash-injection-173.md` (unlink moved ahead of the durable watermark raise) is caught by nothing [read: that doc says so].

R3. The Core-level model cuts the store crudely.
`crash_store` keeps every pack but the last one whole, copies `SYNCED` exactly as it was at the point, and copies `index.cix` whole or not at all [read: `cowfs-core/tests/crash.rs`].
It therefore cannot see a store-side misordering, such as `SYNCED` written before the pack fsync, at Core level; that edge is only covered inside the store crate (2.2).
It also has no GC in its workload, so the collector's freeze `meta.sync` window and the "meta commit then discard" edge are untested under power loss [read].

R4. Durability writes outside both seams:
`cowfs-gc/src/state.rs` (gc state, own `sync_data`, `rename`, dir `sync_all`),
`cowfs-core/src/fsops.rs` (own `sync_all`; used for the snapshot-replacement intent file and `virt.ino`, per `docs/v1-core.md`),
`cowfs-daemon/src/base_meta.rs` (own write, rename, dir fsync) [read: grep of `sync_all|sync_data|fs::rename|remove_file` outside `fsio.rs`].
None of these is in any power-loss model.

R5. Real-stack fidelity: whether APFS, ext4 or btrfs plus the real disk honour the fsyncs is unmeasured, and the NFS COMMIT path is out of the store's scope [reasoned].

R6. A zero-cost violation already on main: the crash-model log is not behind the feature.
`Io::write_at` builds `LogOp::Write { data: buf.to_vec() }` before `log_data` checks whether a log is active, so every pack write in a release build allocates and copies its buffer [read: `fsio.rs` lines 143-147, no `cfg`].
`oplog_start`, `oplog_take` and `oplog_marker` are `pub` (`doc(hidden)`) in every build [read: `lib.rs` line 22].
`scripts/check-fault-seam-absent.sh` greps only for `C7D_EXIT_BOUNDARY_N`, so it does not see this [read].
That the copy is not optimised away is [reasoned]; a slice-3 bench or symbol check settles it.

## 4. Options

### 4.1 Option (a): write-ordering simulation at the seams

Mechanism.
Record every durability-relevant call (write, set_len, fsync, create, unlink, rename, dir fsync with its directory) at the store `Io` and the redb backend into one log with one global order.
Rebuild crash images at every op index under a modelled filesystem: fsynced file data survives; a create, unlink or rename survives only if its parent directory was fsynced after it, otherwise either way; unsynced writes survive in any subset and may tear at sector size; a whole-file write via tmp, fsync, rename, dir fsync is atomic.
Reopen each image with the shipped open path, then check receipts, `fsck` and a second gc.

Bug classes found [reasoned unless noted]:
- A missing fsync, or an fsync in the wrong order, anywhere the code goes through a seam (pack before watermark, new pack before index repoint, watermark raise before unlink, store sync before meta commit).
- Mutation 2 (unlink ahead of the durable watermark raise), once the unlink is a logged op [reasoned; the spike that would verify it was blocked, section 7].
- Recovery bugs on torn tails, half-written `SYNCED` slots, and resurrected or vanished pack files.
- Exhaustive over the op indexes of one recorded run, which a random power cut never is.

Bug classes missed:
- Kernel, filesystem and device bugs, and a device that lies about flush.
- Any write that bypasses the seam (R4) until it is routed through it.
- Races that only appear with real concurrency, because each test replays one recorded serial order.
- NFS server COMMIT or write-verifier semantics.

Fidelity.
ext4 (`data=ordered`) and btrfs are stricter than the model: they never expose an older file length after a newer synced one, and ext4 commits a rename with its journal [reasoned].
So the model is a superset of their behaviours for the cases it encodes: it may flag an image a real ext4 could not produce, but it should not miss one ext4 could produce.
APFS: plain `fsync(2)` on macOS does not flush the drive cache; only `F_FULLFSYNC` does [reasoned: Apple man page semantics, not re-read].
Rust's `File::sync_all` and `File::sync_data` on Apple targets both call `fcntl(fd, F_FULLFSYNC)` [read: rust-lang/rust master `library/std/src/sys/fs/unix.rs`, `os_fsync` and `os_datasync`; the installed toolchain's copy was not checked].
redb 4.3.0's file backends call `File::sync_data`, so meta commits get the same flush [read: `redb-4.3.0/src/tree_store/page_store/file_backend/optimized.rs` and `fallback.rs`].
So the "fsync is honest" rule of the model matches what the code asks APFS for; whether the drive honours `F_FULLFSYNC` is a device question outside any option here except a real cut on Mac hardware, which none of the options offers.
The model assumes a directory fsync makes entries durable, which POSIX does not promise but Linux filesystems and APFS honour in practice [reasoned].

Cost.
About 300 to 600 lines of test support plus routing three writers through `Io` [reasoned].
Runtime per test: seconds to a minute per seed at the store and gc level [reasoned from the existing `crash.rs` sizes]; the spike's run time is in section 7.

CI feasibility.
Plain `cargo test` on `ubuntu-latest` and macOS, no root, no VM, deterministic seeds.
Fits the existing `test` shards.

Compiled-out rule.
Fits, once R6 is fixed: the log, its public functions and the new `Unlink` and `Rename` ops go behind the existing `fault-injection` feature, which already reaches every test crate through dev-dependencies.
No control-protocol field, no daemon flag, no CLI.
`check-fault-seam-absent.sh` gains one more absent-symbol check.

### 4.2 Option (b): real power-cut harness

Variants.
- (b1) qemu: run a store or Core workload in a Linux guest on a virtio disk, `kill -9` the qemu process at random points, boot the image again and verify.
  `qemu-system-aarch64` is installed on this Mac [read: `which`].
  There is no power-cut harness in the repo; `crates/cowfs-store/tests/qa4-vm.sh` and `qa5-vm.sh` only run the test suite inside a Linux VM [read].
  With `cache=writeback` or `unsafe` the host keeps the guest's flushed and unflushed writes alike, so the cut must be `cache=none` or `directsync` plus guest-side loss, or it degenerates into process-crash fidelity [reasoned].
- (b2) dm-log-writes: record every bio, FLUSH and FUA of a workload on a block device, then replay to each flush mark (`replay-log` from xfstests), mount and verify.
  This is the xfstests "generic/4xx" method and is the highest fidelity available for ext4, btrfs and xfs [reasoned].
  dm-flakey can drop writes after a point but cannot enumerate points [reasoned].
  Both need root and the `dm_log_writes` and `dm_flakey` modules [reasoned].

Bug classes found: real fs and kernel behaviour for the fs under test, code that bypasses every seam (R4 at no extra cost), and the whole daemon path if the daemon runs in the guest [reasoned].
Bug classes missed: anything about APFS, since none of these runs it; device cache lies (a VM disk honours flush); points between flushes are not distinct states, so it explores fewer images than (a) for the same run, though each is real [reasoned].

Cost.
Highest: guest image, cross build or in-guest build, boot per image for qemu, and a replay tool plus a verifier script for dm-log-writes [reasoned].

CI feasibility.
The cachyos box has no root (stated constraint), so (b2) cannot run there.
GitHub `ubuntu-latest` runners do have passwordless sudo, which `ci.yml` already uses for `sysctl` and `apt-get` [read], so (b2) is possible in CI in principle.
Whether the runner's Azure kernel ships `dm_log_writes` as a module is unverified [reasoned, open].
(b1) in CI needs KVM on the runner; without it, aarch64 or x86 TCG emulation is too slow for a per-PR job [reasoned].

Compiled-out rule.
Best of the three: no production code at all, because the cut happens below the process [reasoned].

### 4.3 Option (c): hybrid

(a) as the per-PR gate, plus (b2) as a scheduled or manual calibration job that runs the same store and Core workloads on ext4 and btrfs under dm-log-writes and checks that every image (b2) produces is also accepted by the verifier, and that nothing (b2) finds is missed by (a).
It finds what (a) and (b) find together and costs both [reasoned].
Fidelity: (a)'s model on every platform, plus real ext4 and btrfs behaviour through (b2); still nothing real for APFS [reasoned].
CI feasibility: the (a) part runs per PR; the (b2) part needs a sudo runner (available on `ubuntu-latest` [read: `ci.yml`]) and the `dm_log_writes` module in the runner kernel (unverified), so it fits a scheduled or manual job, not the required `check` [reasoned].
Compiled-out rule: same as (a), since (b2) adds no production code [reasoned].
Its main value is calibrating the model against a real fs, which matters only if the model is suspected of being wrong [reasoned].

### 4.4 Comparison

| | (a) simulation | (b) real power cut | (c) hybrid |
|---|---|---|---|
| finds missing or misordered fsync in cowfs | yes, exhaustive per run | yes, sampled at flush points | yes |
| finds fs, kernel, device bugs | no | yes, for ext4/btrfs/xfs only | yes |
| covers APFS | model only | no | model only |
| covers writes outside the seam (R4) | only once routed | yes | yes |
| per-PR CI, no root | yes | no (needs root, maybe KVM) | (a) part only |
| production code | log behind feature | none | log behind feature |
| cost | low to medium | high | highest |

Every table cell restates a tagged claim from 4.1 to 4.3 and carries that claim's tag.

## 5. Recommendation and slice plan

Recommend (a).
Reason: every gap in g6 that is about cowfs (R1, R2, R3, R4, R6) is an ordering question that (a) answers exhaustively and per PR, and most of the machinery is already in the tree (2.2).
(b) mostly tests the platform, cannot run on the macOS host that is the product's main platform, and cannot run per PR without root [reasoned].

### 5.1 Slice 3: make the store model complete and truly compiled out

Scope: `crates/cowfs-store` only.
- Put `LOG`, `log_data`, `oplog_*`, `LogOp` and their call sites behind `cfg(feature = "fault-injection")`, so a build without the feature constructs no `LogOp` and copies no buffer (fixes R6).
- Add `Io::remove_file(path)` and route the unlink in `discard` and the torn-file cleanup in `store.rs` through it; it logs `LogOp::Unlink` and calls `fault_boundary("unlink")`.
  This also closes the slice-1 follow-up "add a `fault_boundary` after `remove_file`"; the hard-coded n values in `round4.rs`, `round5.rs` and `compact.rs` are re-derived in the same slice.
- Add `LogOp::Rename { from, to }` and make `LogOp::DirSync` carry its directory.
- Move the crash-image builder from `tests/crash.rs` into one shared module under the feature (for example `cowfs_store::crashmodel`), so gc and core tests reuse it instead of copying it.
- Add `tests/power_discard.rs`: power-cut images at every op index of a real `discard_pack` and `finish_compaction`, several seeds and tear modes.

Acceptance (critic-checkable):
- `cargo test -p cowfs-store` passes; the new test prints its image count and it is above 0.
- Mutations run through the existing harness (`crates/cowfs-store/tests/mutate.py`, whose patterns are first refreshed to current source, since W1 no longer matches), not ad hoc.
- Mutation 2 (unlink moved ahead of the watermark raise) is killed by the new test; show the failing output.
- W1 (watermark before pack fsync) is killed by a power-loss test by name, not only by a static ordering test.
- A mutation that drops the new-pack fsync in `finish_compaction` is killed.
- Zero cost when disabled, checked by symbol absence, because the store's self dev-dependency forces the feature on under `cargo test` and no in-crate test can observe a build without it: `scripts/check-fault-seam-absent.sh` also fails if the release `cowfs-store` rlib and the release `cowfs-daemon` built without the feature contain `oplog_` or `LogOp`, with the same positive control built with the feature.
  Once the whole log is gated, absent symbols imply no copy.
- The merged doc's sentence "Cost when disabled is zero" (in `docs/crash-injection-173.md`) is corrected to describe R6 and its fix.
- The new `fault_boundary("unlink")` shifts the boundary counts, and routing the open-path torn-file cleanup through `Io::remove_file` can add startup boundaries [reasoned].
  So the slice-2 daemon sweep (`scripts/verify-daemon-crash.py --fault-boundary 1-60`) is re-run, and the recorded counts in `docs/crash-injection-173.md` (n=1..18 die, source pack gone from n=14, "startup contributes none") are updated from that run.

### 5.2 Slice 4: GC under power loss

Scope: `crates/cowfs-gc` tests, plus routing `cowfs-gc/src/state.rs` through a store-exported durable-write helper, or a written argument that gc state is advisory and both its survive and vanish outcomes are safe (decide in the slice, with evidence).
- `tests/power_collect.rs`: the slice-1 fixture (sealed packs mixing live and dead records), record the oplog over a real `Gc::collect`, build images at every op index under every loss policy, then reopen, check every receipt byte for byte, `fsck` clean, and a second collect clean.
- `Gc::collect` and the store spawn no thread [read: no `thread::spawn` in `cowfs-gc/src` or `cowfs-store/src`].
  But the fixture's `Meta` could run a `cowfs-meta-bg` thread whose `before_sync` calls `Store::sync`, which a thread-local log would miss.
  The gc fixtures open meta with `background: false` today [read: `cowfs-gc/tests/common/mod.rs`, `crash_inject.rs`]; the test must keep that and assert it, or use the per-store log of slice 5 instead.

Acceptance:
- The image count is printed and is above 0; at least one image is missing the source pack and at least one still has it (the slice-1 fail-closed rule, applied to power loss).
- Mutations 1 (`discard(rw.to, ..)`), 2 and "no new-pack fsync" each fail the test.
- Runtime under 2 minutes in the CI test shard.

### 5.3 Slice 5: Core level, store and meta on one timeline, with a gc cycle

Scope: `crates/cowfs-core/tests/crash.rs`.
- Replace `crash_store` with images built from the store log, and merge the store log and the meta backend log into one global order.
- The thread-local log cannot serve here, because Core runs a `cowfs-flusher` thread and meta runs `cowfs-meta-bg`, and `Store::sync` runs from meta's `before_sync` hook [read: `cowfs-core/src/lib.rs` line 279, `cowfs-meta/src/db.rs` line 1768].
  A thread-local log at this level would silently miss those ops [reasoned from the above].
  A process-global log is also wrong, because parallel tests in one binary would log into each other [reasoned].
  Instead extend the per-store seam that already exists: `Io` carries `trace: Option<Trace>`, an `Arc<Mutex<Vec<Op>>>` set by `Store::open_traced` [read: `fsio.rs`, `store.rs` line 340].
  It is per instance and thread-safe, so it sees the flusher and meta threads; it gains the data payloads of `LogOp` and shares one sequence counter with the redb recording backend to give a single global order.
- Route `cowfs-core/src/fsops.rs` durable writes (intent file, `virt.ino`) through the same log.
- Add a gc cycle to the seeded workload so the freeze `meta.sync` and the discard after it are cut.

Acceptance:
- The existing hook-off negative control still fails as expected.
- No R6 regression: `Trace` and `Store::open_traced` are compiled in every build today, and `Io::log` builds its `Op` lazily [read: `fsio.rs`]; the payload-carrying trace sits behind the feature, and the slice-3 symbol-absence check is extended to cover it.
- New mutation: advance the watermark before the pack fsync in `sync_capture`; it must fail at Core level (today only the store crate sees it).
- New mutation: skip the intent-file fsync in snapshot replacement; it must fail.

Out of scope, with reason: `cowfs-daemon/src/base_meta.rs` (R4).
It records warm-base provenance (which repo, ref and commit a base was built from), not file data, so it cannot cause g6 data loss or a torn tree [read: its module doc].
It already uses tmp write, fsync, rename and dir fsync [read: lines 344-349], and it stays covered only by process-crash evidence.

### 5.4 Deferred: real power cut

Not built now.
Trigger to build (c) with dm-log-writes on a GitHub runner: any one of a field report of loss after power failure, a model rule found wrong by reading kernel or fs source, or a decision to claim real-device power-loss safety in user docs.
Before that, the g6 text must say "power loss: ordering proven under a modelled filesystem, real device not tested".

### 5.5 What the plan does not change

No control-protocol field, daemon flag or CLI option.
No change to `progress/plan.json`; the coordinator updates g6 text.

## 6. Falsifying the recommendation cheaply

The recommendation fails if the model cannot tell a correct ordering from a broken one at the unlink, which is exactly the edge slice 1 could not catch.
Spike: in a scratch copy, add an `Unlink` op, build power-cut images over a real `discard_pack`, and check that the correct code passes every image while mutation 2 fails some.
If the correct code fails, the model is too pessimistic (or the code has a bug); if mutation 2 passes, the model is too weak to justify (a).

Second falsifier, for slice 5 only: if merging the per-store trace and the redb recorder into one order needs a lock that changes the interleaving enough to hide the store-sync-before-meta-commit bug, the existing hook-off negative control stops failing; slice 5 must show it still fails.

## 7. Spike result: NOT RUN, blocked by the host

Status: no `[spike]` claim in this memo; every claim the spike was meant to settle stays `[reasoned]`.

What was built (throwaway, scratch copy of main 84bd305, nothing in the repo touched):
- `fsio.rs`: a new `LogOp::Unlink { file }` and `pub fn oplog_unlink(path)` that logs it.
- `compact.rs` `discard`: log `Unlink` right after the successful `fs::remove_file`; with env `SPIKE_MUT2` set, do the `remove_file` (and log it) before the watermark `wm.reset` block instead, which is mutation 2.
  `packs()` lists the directory with `read_dir`, so the early unlink makes it omit the pack rather than fail [read: `compact.rs` line 222], and the arm should fail on images, not on an unwrap.
- `tests/spike_power.rs`: seed a 32 KiB-pack store with 3 live and 120 dead 4 KiB noise blocks, compact the sealed pack (copy, `finish_compaction`, sync), snapshot the directory as the base image, then record the oplog over `discard_pack`.
  For every op index k and 16 seeds, build an image: a write survives if a later fsync of its file precedes k, else with probability 1/2; a whole-file write survives if it completed before k; an unlink survives if a dir fsync follows it before k, else with probability 1/2.
  Each image is reopened with `Store::open`, and the check fails on an open error, `has_corruption()`, any live block not read back byte for byte, or an unclean `fsck`.
- Expected: the correct order passes every image; `SPIKE_MUT2=1` fails images where the unlink survived and the `SYNCED` raise did not.

Why it did not run: on this Mac, at 2026-10-09 04:50 local time, every non-system executable hung at launch.
Evidence: a freshly compiled `int main(){return 0;}` and an existing, previously working cargo build script both died on a 15 s `alarm` (exit 142), while `/usr/bin/true` ran (exit 0).
Other agents' cargo build scripts (scratch dir `n43`) were also stuck for more than 12 minutes.
So cargo's build scripts never returned and no test binary could have run either.
This is a host fault, not a result about the design.

To re-run (critic or slice-3 builder): apply the three changes above to a scratch copy, then `cargo test -p cowfs-store --test spike_power -- --nocapture` and again with `SPIKE_MUT2=1`.
Pass condition for the recommendation: 0 failures on the correct order, more than 0 failures with `SPIKE_MUT2=1`, each failure message the test's own "N of M crash images failed" and not a panic in the fixture.
If the correct order fails, read the failing image before changing the model, and loosen a model rule only if a real filesystem rule justifies it.
