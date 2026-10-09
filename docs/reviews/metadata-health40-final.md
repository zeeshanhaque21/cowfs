# metadata-health-40-final: independent review of PR #116 (issue #40)

Reviewer lane: `.treehouse-ready-wave/.treehouse/cowfs-7c1bf8/8/cowfs`, lease `cowfs-ready45`, branch `fix/fuse-torn-read-45` at `0092804`, left untouched and clean.
Subject: PR #116, head `a1f302353bc829e30d4d09875df9536be5c1ffcd`, base `46b0f26`.
Toolchain actually used: `rustc 1.99.0` / `cargo 1.99.0`, macOS aarch64.
Nothing was merged, committed, pushed, checked out, reset or stashed.

## Provenance of what was reviewed

`git ls-remote origin refs/pull/116/head` returned exactly `a1f302353bc829e30d4d09875df9536be5c1ffcd`, and that object is present locally, so the reviewed tree is the published head and not a local variant.
Both trees were taken with `git archive` of the exact SHA, so every byte reviewed is the committed blob.

- `Cargo.lock` sha256 `2ed20c88136771f956e4170aa248e5261b3249a2ba4a0e4cb6b6d49aa8e46f77` in the PR tree and in the base tree, identical, and unchanged after every build and test I ran.
- The PR changes four paths: `crates/cowfs-meta/src/db.rs`, `crates/cowfs-meta/src/lib.rs` (one `pub use` line), `crates/cowfs-meta/tests/health.rs` (new), `docs/verification/ready-40.md` (new).
- It touches no `cowfs-core` file and no `tx.rs`, so there is no overlap with a `tx.rs` lint issue at any toolchain version, and nothing to reconcile with slot 6's Core surface.
- `open_recover` has zero callers outside `crates/cowfs-meta` (my own grep, 0 hits), so the additive `Recovery` fields break no external constructor.

## Counts, lint, format, CI

| Check | Real result |
| --- | --- |
| `cargo test -p cowfs-meta --locked` at head | 78 passed, 0 failed, 2 ignored, exit 0 |
| the two ignores | byte-identical `#[ignore]` attributes present at base `46b0f26`, pre-existing |
| `cargo clippy -p cowfs-meta --all-targets --locked -- -D warnings` | exit 0, zero warnings |
| `cargo fmt -p cowfs-meta -- --check` | exit 0 |
| `gh-axi pr checks 116` (read once) | 3 passed, 0 failed: check ubuntu-latest, check macos-latest, linux-fuse |

Every number in the PR body's "Proof" section reproduces at the head.
The clippy and fmt results are scoped to `-p cowfs-meta` on toolchain 1.99.0 only, and this review makes no workspace-wide or other-toolchain claim.

## M1: proved

Old-arm mutant, reverting only the `Job::Flush` arm to `Job::Flush => inner.timer_flush(),` and running the one M1 test:

| Step | Real exit | Real output |
| --- | --- | --- |
| pre-fix arm | 101 | `thread 'cowfs-meta-bg' panicked at crates/cowfs-meta/tests/health.rs:267:17`, then `timed out after 20s waiting for the background commit after the panicking hook`, `0 passed; 1 failed` |
| fix restored | 0 | `ok` |

The failure is the M1 symptom itself: the injected hook panic kills the timer thread and the idle change is never flushed.
The test uses a real background thread, the public `Meta`, a real `before_sync`, and a real durability signal, the durable snapshot root read back through `durable_snapshots()`.
It also pins the heal semantics the issue asks for: `consecutive_flush_failures` returns to 0 after the retry succeeds while `last_flush_error` stays set, so a recovered stall does not clear its own reason.
The store keeps taking writes afterwards and `check()` passes.

Code reading agrees with the test.
`run_hook` is called at `db.rs:477`, before `begin_write` at `db.rs:483`, so a hook panic never happens inside a redb write transaction and no transaction is left half applied.
`bg_main` clears `st.deadline` before running the job (`db.rs:985`), so `rearm_flush` into `arm_timer` finds `deadline.is_none()` and really re-arms.
The panic path calls `Instant::now().checked_add(sync_interval)`, so the retry rate is bounded by `sync_interval` and there is no hot repair loop.
Poison recovery is real: the `RwLockWriteGuard` held by `timer_flush` is dropped by the unwind, and every later acquisition goes through `unwrap_or_else(|e| e.into_inner())`, which the tests exercise, since the retry commit after the panic succeeds.

## M3: the fix is correct, but the shipped test does not test it

Both minimal mutants survive `a_rollback_never_hands_out_a_number_it_already_handed_out`, each re-run in its own isolated `CARGO_TARGET_DIR`:

| Build | `rec.ino_floor` | `rec.snapshot_floor` | test result |
| --- | --- | --- | --- |
| pristine head | 454 | 4 | pass |
| one-`ino_block` bump removed | 450 | 4 | pass |
| `next_snapshot + 1` removed | 454 | 3 | pass |

The test asserts `ino_floor > 449` and `snapshot_floor > 2`, and every one of those builds satisfies both.
The reason is measured, not guessed.
For the shipped fixture the rollback loses a commit that never moved either counter: healthy `ino_floor` is 450 and the value stored after the rollback is still 450, and healthy `snapshot_floor` is 3 and the value stored after the rollback is still 3.
The assertions are therefore satisfied by the allocator's pre-existing headroom, because `Tx::alloc_ino` reserves before handing out, so `reserved` is always at least one above the highest number handed out.
The bump and the `+1` are untested by this fixture.

The bound itself is real, and the bump is load-bearing in the case the fixture never reaches.
I built that case directly: with the hook armed to fail after the snapshot exists, every main commit fails while every `reserve_durable` still commits, so the newest durable commit is a reservation.
One damaged page then rolls back exactly one block, with `ino_reserved` going 62 to 58, which is behavioural confirmation of the one-commit-slot claim rather than a code reading.

| Build | `rec.ino_floor` | highest handed out pre-crash | inode numbers handed out twice |
| --- | --- | --- | --- |
| pristine head | 62 | 61 | 0 |
| one-`ino_block` bump removed | 58 | 61 | 4: `[58, 59, 60, 61]` |

So the shipped test passes for a reason unrelated to the fix, while removing the fix in the scenario the fix exists for really does re-hand-out four inode numbers.
A mutant-sensitive test needs the reservation to be the newest commit, which is reachable: page damage at page 2 of that fixture gives the rollback verdict directly.

The `alloc_ino` bound the PR body relies on does hold in the code.
`tx.rs:68-77` reserves `next + block.max(1)` through the `reserve` closure when `next >= reserved`, and `reserve` is `reserve_durable`, its own `begin_write` and `commit` at `db.rs:641-649`, so one commit advances `ino_reserved` by at most one block.
`set_two_phase_commit(true)` is present at all five `begin_write` sites: `db.rs:484`, `644`, `664`, `915`, `1219`.
`Extra::Add` is a single enum value and writes `next_snapshot` exactly once per commit at `db.rs:555`, and the only batch entry point, `Snapshot::batch`, mutates one snapshot and does not add snapshots.

## BLOCKER: the M3 bound is conditional on `ino_block`, which is neither persisted nor validated

`record_recovery` advances the floor by the caller's current `opts.ino_block` (`db.rs:661`, called at `db.rs:1181`).
If the file is recovered with a smaller `ino_block` than the one in effect when the lost reservation was written, the advance is smaller than the loss and the floor lands below numbers the store already handed out.

Measured on pristine head code, no mutant:

| Built with | Recovered with | `rec.ino_floor` | highest handed out pre-crash | inode numbers handed out twice |
| --- | --- | --- | --- | --- |
| `ino_block = 64` | `ino_block = 4` | 6 | 61 | 56: `6..=61` |

No error, no warning, no refusal, and `Health` reports a healthy store.
This is a direct violation of the never-reused `Ino` rule that issue #40 M3 exists to restore, reached through the public API.

The repository already has the precedent for the fix.
`node_size` is rejected below 256 at `db.rs:1206`, persisted at `db.rs:1229`, and on reopen the stored value governs through `db.rs:1253` rather than the caller's.
`ino_block` is caller-supplied only (`db.rs:101`, default 16384 at `db.rs:116`, copied into the allocator at `db.rs:1275`) and is now load-bearing for a correctness invariant.
Any of these closes it: persist `ino_block` and use the stored value in `record_recovery`, clamp the stored value when it is larger, or refuse to recover when the stored block is smaller than the recovered floor.

## Minor findings

**A panicking hook on the inline commit path escapes to the caller and is invisible to `Health`.**
`bg_main` wraps only its own job. The inline `Ack::Applied` commit at `db.rs:751`, the durable wait at `db.rs:790`, `sync` at `db.rs:804` and `close` at `db.rs:812` call `self.commit` with no `catch_unwind`.
Measured with `sync_every_ops: 4` and a hook that panics once: the fourth create panics the calling thread, and afterwards `Health` reports `flush_failures = 0`, `background_panics = 0`, `last_flush_error = None`.
This is loud rather than silent, so it is outside M1's scope, but a caller polling `Health` cannot detect that its own operation was aborted by a panicking hook.

**The `consecutive_flush_failures` doc overstates the behaviour.**
`db.rs:259-262` says that while this counter is non-zero the store refuses further changes once too much is pending.
The refusal in `mutate` at `db.rs:701` is gated on `Session::flush_err`, which the panic path never sets, because the unwind happens before the line that sets it.
The practical exposure is bounded, since a configuration with a reachable backlog cap also has a reachable inline commit, but the doc should say what the code does.

**`ready-40.md:220` quotes a transcript that does not reproduce.**
It reports the injected panic at `health.rs:257:17`. At the head the panic is at `health.rs:267:17`, and `health.rs` is byte-identical between `4366cbb` and `a1f3023`, because `4366cbb..a1f3023` adds only `ready-40.md`. Line 257 is the `#[test]` attribute.
My mutant run reproduces the failure with the line number at `267:17`.

**`ready-40.md:169` is off by two candidates.**
It says a 78-page fixture gave 155 single- and two-page candidates. The search enumerates `(N-1) + (N-2)`, so `2N-3 = 155` implies a 79-page fixture, and 78 pages gives 153.
The conclusion, that a small final commit leaves no page damageable in isolation, is consistent with what I measured, so this is a transcription error rather than a wrong finding.

**`ready-40.md:5` and `ready-40.md:187` name `4366cbb`, not the head.**
That is the fix commit, and the doc commit `a1f3023` is the PR head. The counts were re-measured at `a1f3023` and are unchanged, so the claim is sound with the wrong provenance line.

## What the doc gets right and I did not need to test

The reconciliation, the additive-surface argument, the refusal to touch `cowfs-core`, the residual list with its owners, and the honesty about the reap arm and `poisoned` all hold up.
I confirmed the reap-arm reasoning in the code: `matches!(.., Ok(Ok(true)))` is false for `Ok(Err(e))`, and the new arm records `Ok(Err(e))` and re-arms on `Err(_)` and on `Ok(Ok(true))`, so the silent stop is gone.
`Health.poisoned` is genuinely pre-existing: `Inner::poisoned` at `db.rs:227` is only set by `Inner::note` at `db.rs:349`.

## Integration with main

`main` is `951045f`, 46 commits ahead of the merge base `46b0f26`, and does not touch `crates/cowfs-meta` at all.
`git merge-tree --write-tree main a1f3023` produced tree `d71acdab9eea258f9b77726f40b70cb0cca2b509` with exit 0 and no conflict output, so the integration is a clean merge with no overlapping hunks, not merely a clean-looking diff.

## Delivery accounting, explicitly not whole-issue closure

Delivered and proved by this PR: M1 background-flush panic, M1b flush-failure health reporting, and the M3 counter-floor machinery.
The M3 machinery is correct in the case that matters, but it is not what the shipped test measures, and it is conditional on an unvalidated knob.

Reported rather than delivered, per the PR's own text: M2, the reap arm as a proved fix, `Health.poisoned` as a fix, the mutant harness, the real-`cowfs-store` crash re-run, the `docs/v1-meta.md` performance table, wiring `cowfs_meta::Health` into `cowfs_core::Health` (slot 6), and the Core `rename_snapshot`, batch-timestamp and hole-flag requests (slot 6 and #42).

Issue #40 stays open.
The PR body says it closes the metadata-health and recovery-counter half of #40 and contains no whole-issue closing phrase, and this review claims no whole-issue closure.
Whether GitHub's GraphQL-side auto-close fires on merge is the coordinator's guard to make, not this lane's call.

## Verdict

Request changes before merge.
The blocker is the `ino_block` dependency: the never-reused guarantee the PR body calls provable holds only for a fixed block size, and a smaller `ino_block` at recovery time re-hands-out 56 inode numbers with no signal.
Second must-fix is coverage: both minimal M3 mutants pass, because the fixture's rollback never moves either counter.
The old-arm M1 mutant reproduces the documented failure exactly, the counts, clippy and fmt all reproduce, and the merge into `main` is clean.

## Resource and safety record

Disk was 412 GiB free against a 20 GiB floor, with a private `CARGO_TARGET_DIR` per build under this lease and owned artifacts far under 8 GiB.
The shared daemon PID 15263, its store, its sockets and the mounted pools were never signalled, restarted, reset or garbage-collected, and no daemon was launched.
Damage was applied only to private `tempfile` fixtures, only after the store was closed.
No lease was returned, no branch merged, no commit created, no push.

Disclosure: I did not take `.treehouse-ready-wave/mac-heavy.lock`, which the author's lane protocol uses.
My builds were scoped to `-p cowfs-meta` with a private target directory and mutated no shared state, but I am recording this rather than claiming protocol compliance.

Methodology trap worth recording: `cp -R` preserves mtimes, so cargo treated mutant sources as older than a previously built artifact and silently reused the stale test binary.
That produced a false "mutant survives" reading on the first pass.
Every mutant result in this report was re-run in its own isolated `CARGO_TARGET_DIR`, and the two builds that differ in `record_recovery` demonstrably produce different `rec_ino_floor` values, which is the check that the mutation was actually compiled in.