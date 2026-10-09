# PR143 RPC-outcome and valid-channel fixture: final independent review

Independent, read-only review of PR143 head `7afe72264e1564b471e94aeaace8a370384514e0` on `fix/nfs-translate-namespace-race-43`.
Scope: the third fixture revision in `crates/cowfs-nfs/tests/namespace_race.rs` and the adapter lock patch it exercises, against existing issue #43.
This review supersedes nothing; `docs/reviews/pr143-original-race-final-wbuddy-review.md` (SHA256 `99a65885d5a0f1f146286921504d48af1978278869d343111640200493b4fa17`) stays as written.

No source, build, test, probe, cleanup, offload, lease, signal, SSH, install, or workflow change was made.
No rerun, dispatch, or artifact download.

## Pins

| object | value |
| --- | --- |
| reviewed head | `7afe72264e1564b471e94aeaace8a370384514e0` |
| head tree | `362a2bb312c9d194a3c46d07766e5d6fc11e8992` |
| head parent | `8c3f81683276ae7857530ea2e9e9d0420642a3ea` |
| remote branch tip | `refs/heads/fix/nfs-translate-namespace-race-43` = `7afe7226...` |
| remote PR head | `refs/pull/143/head` = `7afe7226...` |
| fixture blob at head | `1b5634e75562634e351815e40ae16a739fe77a4b` |
| fixture blob at prior head | `fdb683059372ac1179eaae81bab1993c2dca35a6` |
| adapter blob (both heads) | `1987ece5d8a354693b6af88ec9b3160968e8b8f7` |
| sidecar blob at head | `5f5e7d54...` (unchanged) |
| prior-red main | `89353e17e5085000711dc428e834f9cc41840a1f` |
| CI merge base | `e488a17bb67b30b31be6f3f19f6a9e0e6aa8c94e` = current remote `main` (PR140 merge) |
| CI merge commit | `6ab4b2e` = `Merge 7afe72264e1564b471e94aeaace8a370384514e0 into e488a17...` |

`git diff --name-only 8c3f8168... 7afe7226...` returns exactly one path, `crates/cowfs-nfs/tests/namespace_race.rs`, at `+135/-23`.
The adapter lock patch is byte-identical between the two heads.
Scope increase since the prior review: none in source; test-only, exactly as the correction receipt states.

## Verdict

**Source verdict: the adapter lock patch remains source-correct.**
**Fixture verdict: the three gaps the prior review named are closed in source, and the fixture is now a genuine RED-on-main / GREEN-on-head regression for the adapter-reachable half of #43.**
**Remaining gap: the root-handle-versus-`snapshot_view` integration requirement is still not covered by any permanent test.**
**#43 status: not a whole-issue close. One of six checklist obligations is evidenced; five remain open.**

## What changed since the prior review

The prior review (`99a65885...`) blocked acceptance on three concrete problems.
All three are corrected in the fixture at `7afe`.

1. **Dropped `create(doc)` result (false-green path).**
   The prior raw test bound `create(doc)` to a discarded name.
   If that create failed, `doc_present_at_mutation` stayed false, the shadow assertion was skipped, and the test passed while the defect was live.
   At `7afe` the raw test asserts `created_st == OK`, a raw `lookup("doc") == OK`, and `mkdir_st == OK` **before** the outcome branch (fixture lines 337-342).
   A failed create now fails the run on its own.
   Closed.

2. **Conditional shadow skip / channel acceptance.**
   The prior test ended with `if real { assert_ne!(ch, OK) }`, a status-only check that skipped the byte path.
   At `7afe` both legal outcomes assert unconditionally:
   - `doc_present_at_mutation == true` branch (lines 355-370): `assert!(!real)`, channel `create(._doc) == OK`, `write(blob) == OK`, `read == OK`, and `got == blob` byte equality, where `blob` is a real `Sidecar::from_xattrs(...).encode()` payload.
   - `doc_present_at_mutation == false` branch (lines 371-385): `assert!(real)`, `doc.kind == FileKind::Regular`, and `created_fh.is_some()`.
   The conditional skip is gone.
   Closed.

3. **Channel-success ordering not covered.**
   Because a correctly serialised adapter lets `mkdir` win the lock first, the race test never reaches the "main file first" channel path.
   `a_main_file_first_channel_round_trips` (lines 464-498) drives it directly over the same raw NFS server: `create(doc) == OK`, `create(._doc) == OK`, `write(valid_sidecar) == OK`, `read == OK`, `got == blob`, `!is_real_dir`, `doc` present.
   Closed.

The `NFS3ERR` 10004 mislabel from the earlier receipt is also corrected.
`10004` is `NFS3ERR_NOTSUPP` (`crates/nfsserve/src/nfs.rs:171`), not `NFS3ERR_IO` (`:109`).
`implausible_sidecar_bytes_are_refused` (lines 504-538) pins this as a deliberate safe control: `[0u8; 4096]` write returns `NOTSUPP`, leaves no real `._doc`, and a valid payload still round-trips afterward.
No product defect is invented.

## Byte-round-trip proof is real, not status-only

The valid payload is `cowfs_nfs::Sidecar::from_xattrs([(b"user.k".to_vec(), vec![7u8; 100])]).encode()` (fixture lines 261-263).
`Sidecar` and `from_xattrs` are public; `encode` (via `encode_capped`, `crates/cowfs-nfs/src/appledouble.rs:207`) writes `MAGIC`/`VERSION` at offsets 0/4, count 2 at 24, and a Finder-info entry at `ENTRIES_AT` pointing to `FINFO_AT`.
`is_plausible_prefix` (`appledouble.rs:90`) accepts exactly that shape, so the payload genuinely reaches the write path; the assertion is a reassembled-length-and-bytes check (`assert_eq!(got, blob)`), not a status check.
Confirmed by source, both in the in-race branch and in the standalone channel test.

## Deterministic branch meaning

`doc_present_at_mutation = inner.lookup(ROOT_INO, MAIN).is_ok()` is sampled after the hold is reached and before `release` (fixture lines 321-322).
It reads the same `Vfs` beneath the adapter, not a below-`Vfs` injection.
The hold sits after the backend has answered the guard read (`WatchVfs::lookup`, lines 156-164), so the sampled fact is exactly "did the second connection's main name land while the guard's stale answer was held".
This is a real read-then-write window on the real request flow, not a timing guess.
Confirmed by source.

## Executed evidence (CI, derived from actual logs)

CI run `37526812136` (`ci`, `pull_request`) is `completed` / `success`.
- `112485793019` `check (ubuntu-latest)`: success
- `112485792780` `check (macos-latest)`: success
- `112485792500` `linux-fuse`: success

The Ubuntu job's own log records the checkout as `HEAD is now at 6ab4b2e Merge 7afe72264e1564b471e94aeaace8a370384514e0 into e488a17bb67b30b31be6f3f19f6a9e0e6aa8c94e`.
`e488a17` is the current remote `main`, so CI exercised a clean merge of the reviewed head onto real main, not a stale base.

Per-test lines read directly from the completed job logs:

macOS (`check (macos-latest)`):
```
Running tests/namespace_race.rs
test a_refused_directory_leaves_the_name_free ... ok
test a_main_file_first_channel_round_trips ... ok
test implausible_sidecar_bytes_are_refused ... ok
test the_guard_and_its_mutation_are_one_step ... ok
test a_sidecar_name_never_becomes_a_real_object_under_raw_nfs ... ok
test result: ok. 5 passed; 0 failed
```

Ubuntu (`check (ubuntu-latest)`) runs the same five by name and result.

`cargo fmt --all --check` and `cargo clippy --workspace --all-targets -- -D warnings` ran in both `check` jobs and the jobs succeeded, so both passed.

The `linux-fuse` job passed.
This lane is not the FUSE-mount end-to-end; it is not treated as FUSE coverage here.

Per-test fixture output (the `eprintln!` lines) is not present in the aggregate job logs and was not downloaded; it is logged as missing coverage, not as passed.

## Adapter lock patch: source re-check

The patch is unchanged since the prior review (same blob `1987ece5`).
`Adapter::with_names` (`crates/cowfs-nfs/src/adapter.rs:640`) takes the `PerIno` map lock, resolves a per-`Ino` `Arc<Mutex<()>>`, then holds that mutex across the whole closure, including the `Vfs` calls.
`with_two_names` (`:650`) takes two per-`Ino` locks in a fixed ascending order, so two renames cannot deadlock.
`PerIno::of` (`crates/cowfs-nfs/src/sidecar.rs:111`) stores `Weak` refs and GCs dead entries past 4096.
`mkdir`, `create`, `create_exclusive`, unlink, rename paths all route through `with_names`/`with_two_names`.
Same-directory mixed operations serialise; different-directory operations do not.
Source-correct.

One doc nit, not a blocker: the module header (`adapter.rs:1`) still says "No lock is held across a `Vfs` call", while `with_names` now does hold the directory lock across the `Vfs` read-then-write pair by design.
The comment describes a stale invariant.

## Remaining gap: root-handle versus `snapshot_view`

The prior review required an actual permanent integration test where the root handle and a `snapshot_view` handle (or two separate `Adapter` instances) race on one directory.
No such test exists.
`grep` over `crates/cowfs-nfs/tests/` and `src/` for `snapshot_view` returns nothing.
Every adapter-level race test in the fixture (`the_guard_and_its_mutation_are_one_step`, lines 393-431) does `let a = a.clone()`, cloning the **same** `Arc<Adapter>`, so both threads share one `PerIno` lock map; it cannot represent two independent adapters over one backend.

The retraction claim ("same-snapshot serialisation holds") therefore still rests on source trace, not a test:
`Core::snapshot_view` (`crates/cowfs-core/src/lib.rs:315`) and the root `Core` clone (`:154`) share one `Inner`, and every namespace op takes `SnapCtx.ns` from the same `by_id`/`snapctx` path (`inner.rs:165`, `:240`), with `make` returning `ReadOnly` for `ROOT_INO` (`ns.rs:163-175`).
The trace is sound but untested.
This is a **missing permanent test**, not a demonstrated failure, and it is carried forward as the open integration obligation.

## Remaining existing #43 obligations

Issue #43 lists six unchecked obligations.
This fixture and lock evidence exactly one: per-inode sidecar locking and "`._name` stored as a real file when there is no main file".
The other five are untouched by this PR and remain open:

1. Round-3 critic review of the post-round-2 code (`._name` as a real file with no main file, whole-file sidecar-write refusal, 200+ xattr encoding, per-inode locking, sidecar handle generations).
2. Critic review of the security code (export-path answering, one-shot root handle, keyed BLAKE3 handle MAC, per-connection high-water xids, oldest-silent eviction); not tested with a second uid.
3. Dead-server hang: `install_signal_cleanup` + `sweep_stale_mounts` wired into the daemon (#13).
4. `Store` mode leaving untracked `._` files after checkout; nothing asserts on it.
5. Re-run the conformance suite through the NFS mount with the raw-protocol client.
6. Warm-build edit-and-rebuild measurement against the amended macOS budget (#37).

Therefore #43 must **not** be recorded as passed by this PR.
A race-only merge of the adapter lock can proceed on its own merits while #43 stays open.

## Separate defect candidate (out of this PR's scope)

The `NFS3ERR_NOTSUPP` on an implausible whole-file write over a live `._doc` view is deliberate and proven non-destructive here, so it is not a product bug in this revision.
Whether a live view should accept a whole-file write at all (versus the xattr path) is a separate design question, not raised or changed by this PR.

## Constraints: unknown or violated

**Violated (operational, not technical).**
The lane was assigned no disposable worktrees and no `/tmp` artifacts.
Observed:
- `/tmp/race43_oldfail2.log` (created Oct 6 13:23) and `/tmp/race43_residual_red.log` (Oct 6 12:39) both exist.
- A throwaway main worktree (`mainwt2`) was reported created and removed.

These are **operational-claim violations**, labelled as such.
They do **not** invalidate the CI evidence: CI ran on GitHub infrastructure against the pushed head, independent of the local process.
Technical acceptance and operational-conduct correction are distinct here; do not treat the clean CI result as retroactive permission for the local violations.

**Unknown.**
- `bench/out/ready43-race/mainwt` and `target/ready43-main` are absent with no deletion receipt. Observed state is consistent with the removal claim; provenance of the cleanup is **unknown**. `target/ready43` is still present. Do not delete or move any of these.
- No resource-preflight receipt (8 GiB artifact cap, 20 GiB free-space floor, projected compile peak) was found for this lane. The cap and floor exist in `docs/ready-wave-dispatch.md:45,48`, but no lane-specific receipt was located. **Preflight provenance: unknown.**

## Immutability

Unchanged, verified by hash after this review:
- `docs/reviews/pr143-original-race-final-wbuddy-review.md` = `99a65885d5a0f1f146286921504d48af1978278869d343111640200493b4fa17`
- `docs/verification/evidence/translate43-original-race-regression.md` = `faace84a19d34dda311b8997354fb6addb3308ba43b67ce9d32d5ac25d07461b`
- `docs/verification/evidence/translate43-existing-race-repair.md` = `0f4cc6a898539c83198ca084fb7c59be3285eee394634b83bc5cda5114ecb80b`
- `docs/reviews/pr143-nfs-namespace-race-wbuddy-review.md` = `69d5688541f86b02ace1234ca26071949917c02b1d712189ecfe312784fda739`
- author correction receipt `docs/verification/evidence/translate43-rpc-outcome-and-valid-channel-correction.md` = `40b123c91ac5f5dc4de52df9592d61e2fbae22e9e5f66736e94c04f17c2c3b9a`

## Findings

| id | severity | finding |
| --- | --- | --- |
| F1 | resolved | `create(doc)` result now asserted before the branch; false-green path closed (fixture 337-342). |
| F2 | resolved | Conditional shadow skip removed; both legal outcomes asserted with real byte round-trip (fixture 355-385). |
| F3 | resolved | Channel-success path covered by a dedicated raw-NFS test (fixture 464-498). |
| F4 | resolved | `10004` correctly labelled `NFS3ERR_NOTSUPP`; implausible-bytes refusal pinned as a deliberate safe control (fixture 504-538). |
| F5 | low | Root-handle versus `snapshot_view` / two-separate-`Adapter` atomicity still has no permanent test; rests on source trace. Carried forward. |
| F6 | low | Stale module doc comment in `adapter.rs:1` ("No lock is held across a `Vfs` call") contradicts the new `with_names` design. |
| F7 | process | Operational violations: `/tmp/race43_*.log` retained; throwaway `mainwt2` created and removed. CI evidence unaffected. |
| F8 | unknown | `mainwt`/`target/ready43-main` deletion and resource preflight have no receipt. Provenance unknown. |

## Bottom line

The corrected fixture is a genuine regression for the adapter-reachable half of #43: it now fails on main inside the named shadow assertion with `create(doc) == OK`, and passes on the fixed head across both OSes in a real merge against current main.
The adapter lock patch is source-correct and unchanged.
The three prior-review gaps are closed.

Still missing before #43 can be called done:
1. A permanent root-handle-versus-`snapshot_view` (or two-adapter) atomicity test, or an explicit decision to accept the source trace.
2. Evidence for the other five #43 checklist items.
3. Recording of the operational violations and the unknown-provenance items.

A race-only merge of the adapter lock can proceed separately.
#43 must not be recorded as passed by this PR.

