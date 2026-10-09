# pr116-final-delivery: final delivery review of PR #116, revision 2 (issue #40)

Reviewer lane: `.treehouse-ready-wave/.treehouse/cowfs-7c1bf8/8/cowfs`, lease `cowfs-ready45`, branch `fix/fuse-torn-read-45` at `0092804`, left clean and unchanged.
Subject: PR #116 head `fab4c6490c4274262976431384d7468b2e6a2de5`, code commit `8a6e1ba`, superseding the head this lane blocked, `a1f302353bc829e30d4d09875df9536be5c1ffcd`.
Host toolchain: `rustc 1.99.0` / `cargo 1.99.0`, macOS aarch64.
No checkout, reset, stash, branch change, commit, push, merge, or lease return was performed.

## Verdict

The original blocker is closed, with first-hand proof rather than a receipt.
Ship it, with one non-blocking gap: the inline `Ack::Applied` hook-panic path is correct but has no shipping test.

## Provenance of what was reviewed

`git ls-remote origin refs/pull/116/head` and GraphQL `headRefOid` both returned `fab4c6490c4274262976431384d7468b2e6a2de5`.
Both trees were taken with `git archive` of that exact SHA, so every byte reviewed is the committed blob.
Revision 2 adds two commits on top of the blocked head: `8a6e1ba` for the code and `fab4c64` for the documentation.

The `db.rs` git blob is `b08ed7f7e204a60625124b13415f471ec82800b8` and is identical at `8a6e1ba` and at `fab4c649`, so the code pin is exact and the documentation commit changes no behaviour.
Its sha256 is `de79c2713fbb01489c7895d51b0c5c2b52229ea8da86db552d8d41532ef38da1`, which is the author's shipping receipt `de79c2713fbb0148`.
The `db.rs` of the previously blocked head `a1f3023` has sha256 `4d26d725527c24545d52b25f9c478ddcb154e67bd1e07815ba6b5b864c9f18cf`, which is the author's own `4d26d725527c2454` row for that head.
`Cargo.lock` sha256 is `2ed20c88136771f956e4170aa248e5261b3249a2ba4a0e4cb6b6d49aa8e46f77`, unchanged from this lane's previous review, so no dependency moved.

The pristine tree used for every shipping result was verified against the published tree afterwards: 532 tracked files checked, 0 mismatched, 0 extra files on disk.

## The blocker, and why it is closed

The blocker was that `record_recovery` advanced the inode floor by `opts.ino_block`, the value the caller passed at recovery time, so recovering a store built with a large block using a small one re-issued inode numbers that had already been handed out.

That is now structurally impossible, verified in the archived source:

- The block is persisted when the file is created, at `db.rs:1279`, inside the `tables.is_empty()` branch only.
- It is validated at the door, at `db.rs:1257`, with `opts.ino_block.clamp(1, INO_LIMIT)`, and a stored zero is refused as corrupt at `db.rs:1315`.
- The stored value governs the allocator, at `db.rs:1341`, as `block: stored_block.unwrap_or(ino_block)`.
- The stored value governs recovery, at `db.rs:678`, where `record_recovery` reads `self.ino_block` and the field is populated only from `stored_block` at `db.rs:1360`.
- `record_recovery` no longer takes a block argument at all. In `a1f3023` its signature was `record_recovery(&self, ino_block: u64)` and the call site passed `opts.ino_block`. Now the caller cannot influence the bound by any route.

`FORMAT_VERSION` stays at 2 and the key is written only on creation, so no existing file is silently rewritten and no format migration is invented.

Both directions are covered and both were run live.
Recovering a store created with 64 using 4, and reopening a recovered store with 4096, are `recovery40` tests 1 and 4; both passed at the head and both went red on the blocked source.

The legacy case is refused rather than guessed.
A file written before the key existed leaves `stored_block` as `None`; `record_recovery` then returns `Error::Format` with a stated reason, and `open_recover` surfaces it under the `RECOVERY_FAILED` prefix.
`recovery40.rs:232` proves the refusal, and it first removes the key with redb directly, asserts the key was present so the fixture really is legacy, and asserts the file still opens and accepts writes before damaging it.
So the normal-open behaviour of a legacy file is disclosed and pinned: legacy stores keep working for everything except recovery.
No guessed legacy reservation size appears anywhere.

## Malformed, zero, and oversized stored values

A stored zero is refused as corrupt at open, so the store fails closed rather than reserving by zero.
A caller's zero or overflow is clamped into `1..=INO_LIMIT` at open, so it cannot be injected.
A stored value larger than `INO_LIMIT` is not rejected, and that is safe rather than merely lucky.
`alloc_ino` computes `(next + block.max(1)).min(INO_LIMIT)` and `record_recovery` computes `saturating_add(block).min(INO_LIMIT)`, so an oversized stored block can only over-advance the floor to `INO_LIMIT`, never under-advance it, and `alloc_ino` then fails closed at `next >= INO_LIMIT`.
Reuse is impossible in that direction, because over-advancing wastes numbers and under-advancing is the only reuse route.
Worth one line in the doc that the bound is clamped rather than validated, but not a defect.

No configuration change can reintroduce the original failure, because the stored value governs both the allocator and recovery, so a caller cannot lower the bound after the reservations were written.

## Discrimination of the four shipping tests, measured

The four `recovery40` tests were run against both source states in isolated target directories, one per tree.

At the shipping source `de79c271`, `cargo test -p cowfs-meta --locked --test recovery40` gave exit 0 and `4 passed`.

At the blocked source `4d26d725`, the same tests gave exit 101 with 3 of 4 red:

| Test | Result on the blocked source |
| --- | --- |
| `a_smaller_block_at_recovery_does_not_re_issue_inode_numbers` | FAILED: `60 inode numbers handed out twice, first [6, 7, 8, 9, 10, 11, 12, 13]; highest pre-crash was 65` |
| `the_stored_block_governs_a_plain_reopen_with_a_different_ino_block` | FAILED: `a reopen with ino_block 4096 re-issued 60 pre-crash numbers` |
| `recovery_refuses_a_file_whose_reservation_block_is_unknown` | FAILED: `the current build must persist the block, or this fixture is not a legacy one` |
| `a_snapshot_id_lost_to_a_rollback_is_not_handed_out_again` | passed |

That reproduces the author's own figures and the author's "3 of 4" split, first-hand, on the author's fixture.
The snapshot test passing on the old source is correct, because the old code already carried the `+1` snapshot bump; it is the inode bound and the legacy refusal that the old source lacks.
The two failures that name a re-issue are the ones that carry the blocker, and they fail with the re-issue counts in the message rather than with a floor comparison.

The snapshot test is load-bearing at the head by construction.
`recovery40.rs:202-206` asserts the lost snapshot id is genuinely absent from the recovered state before the fresh id is compared, so it cannot pass vacuously if the rollback ever stopped losing that commit.

The author's mutant receipts: three of five reproduce byte-exactly, two do not.
I reconstructed the mutations from the described edit and hashed the result.
Deleting both `.saturating_add` lines from the shipping `db.rs` reproduces the `neither bump` row exactly at `816c04d8004add9b`.
Deleting only the inode line yields `9bf37ecf2b6d1035`, not the recorded `a2f62c568a395e9f`.
Deleting only the snapshot line yields `d1995f18f143393c`, not the recorded `bfae9acf6aa21591`.
So the two single-mutant rows rest on source states the document does not describe, and their receipts are not independently reproducible.
The conclusion is unaffected, because `neither bump` reproduces exactly and both single mutants are strict reductions of it, and because I reproduced the load-bearing `shipping` and `old` discrimination live instead.
I did not build those three variants, per the instruction to avoid five expensive builds when existing proof plus a bounded live sample suffices.

The distinction between the author's 60 and this lane's earlier 56 is a fixture difference, not a contradiction.
This lane's earlier spike built with `ino_block = 64`, created 60 files, and so had a highest handed-out number of 61, and it re-issued 56 numbers.
The shipping fixture creates 64 files, so its highest handed-out number is 65, and it re-issues 60.
Both are non-zero, both are correct for their own fixture, and the earlier raw bytes stand unrewritten.

## Counts, lint, format, and toolchain scope

| Check | Selection | Real exit | Real result |
| --- | --- | --- | --- |
| `cargo test -p cowfs-meta --locked` | `--test recovery40` | 0 | 4 passed, 0 failed |
| `cargo test -p cowfs-meta --locked --test recovery40` on the blocked source | `--test recovery40` | 101 | 1 passed, 3 failed |
| `cargo test -p cowfs-meta --locked` | whole crate, no filter | 0 | 82 passed, 0 failed, 2 ignored |
| `cargo fmt -p cowfs-meta -- --check` | crate | 0 | clean |
| `cargo clippy -p cowfs-meta --all-targets --locked -- -D warnings` | all targets | 0 | zero warning or error lines |

Per-suite from the whole-crate run: unit 16, `corrupt` 1, `crash` 2, `critic` 12, `health` 7, `kill9` 2, `model` 2, `posix` 16, `recovery40` 4, `review` 20, doc-tests 0, total 82 with 2 ignored.
Both ignores are the same `#[ignore]` attributes that were present at base `46b0f26`, verified in this lane's previous review and unchanged.

The clippy and format results are scoped to `-p cowfs-meta` on the host's 1.99.0 toolchain.
I read CI once for the conclusion below and make no claim about which toolchain CI ran, because I did not read its log.

CI, one snapshot, no poll and no rerun: `3 passed, 0 failed`, namely `check (ubuntu-latest)`, `check (macos-latest)` and `linux-fuse`.

## The M1 panic result still carries, with byte-level source proof

The `Job::Flush` arm of `bg_main` is byte-identical between the blocked head and this head: the arm extracted from each `db.rs` hashes to `49fdefef4ba68c18b292173d1f643c5552b7e9869bfc105d67ceff2c90a58bba` in both.
Therefore this lane's previously measured old-arm result applies unchanged at `fab4c649`: reverting only that arm to `Job::Flush => inner.timer_flush()` gave exit 101, `thread 'cowfs-meta-bg' panicked`, and `timed out after 20s waiting for the background commit after the panicking hook`.
No second mutant build was needed for that, and none was run.
The new pass is live in this review: `health.rs` is 7 passed inside the whole-crate run.

## Inline `Ack::Applied` hook panic: correct, and the one real gap

The fix at `db.rs:779-796` wraps the inline commit in `catch_unwind`, records the failure and sets `flush_err`, then calls `resume_unwind` with the original payload.
Catching there is safe for the reason the comment gives, and I confirmed the ordering in the source: `run_hook` runs before `begin_write`, so no redb write transaction can be open when the panic is caught.
The other paths that call `commit` without a catch are unchanged: `wait_durable`, `sync`, `close`, and the background job, which has its own catch.

I verified this path first-hand with a single reviewer probe, since no shipping test covers it.
`grep` for `inline` across `crates/cowfs-meta/tests/` returns nothing, and neither `health.rs` nor `recovery40.rs` asserts any of it.

Measured on the shipping source, probe exit 0:

- `P5 BEFORE create: flush_failures=0 bg_panics=0 last=None poisoned=false`, so the baseline is clean and the later values are attributable.
- `P5 CALLER_PANICKLED payload="INLINE_HOOK_BOOM"`, so the caller still receives its own panic with its own payload, and the semantics are truthful rather than replaced by a library message.
- `P5 AFTER panic: flush_failures=1 consecutive=1 bg_panics=0 poisoned=false last=Some("the before_sync hook panicked on the inline commit")`, so the store records it and `Health` is no longer falsely clean, which was this lane's earlier finding.
- `P5 RECOVERED write_accepted ino=3`, so the session `RwLock` that the unwind poisoned is recovered and the store still accepts writes; the catch is not a trap.
- `P5 AFTER later success: flush_failures=1 consecutive=0 last=Some(...)`, so a later success clears the consecutive run while the reason stays visible.
- `P5 REOPEN ok lookup_inode=3 recoveries=0 flush_failures=0 last=None`, so no transaction was left open: the file closes, reopens, and `lookup` returns the same inode number that was handed out, and `check()` passes.

`Health.poisoned` stays false, correctly, because a hook panic is not corruption.

The gap is coverage, not behaviour.
A regression that removed the `note_flush_failure` call, or moved the `catch_unwind` to sit after `begin_write`, would pass the entire shipping suite.
One test in `health.rs` on the shape of my probe would close that; it needs no new fixture machinery.

## Reap arm: code review only, explicitly not fault-injected

`db.rs:1055-1072` now handles four outcomes.
`Ok(Ok(true))` sleeps 300 microseconds and re-arms; `Ok(Ok(false))` does nothing; `Ok(Err(e))` records the reason, sleeps 300 microseconds and re-arms; `Err(_)` records a background panic, sleeps and re-arms.
The re-arm on the error path is the actual change, and the original defect was real: `matches!(.., Ok(Ok(true)))` is indeed false for `Ok(Err(e))`, so a failed reap step previously stopped reaping while reporting nothing.
The 300 microsecond pause is what keeps a persistently failing reap from spinning, so there is no hot repair loop here.
The timer interaction is sound because `bg_main` clears the reap flag before running the job, so setting it again from the cleared state is a correct re-arm.

This is reasoned from the code and labelled as such by the author, which is the correct label.
I ran no reap fault injection and make no claim that the reap failure path is proved; proving it needs a fixture whose reap step genuinely fails, which remains open.

## Documentation accuracy at this head

- `Health::consecutive_flush_failures` now says it counts, that it does not by itself refuse anything, and to read `last_flush_error` instead. That matches the code, where the refusal in `mutate` is gated on the session's own `flush_err`. This lane's earlier finding is addressed.
- The transcript line number is corrected to `268`, and `grep -n 'sync hook exploded'` on the head's `health.rs` returns exactly 268. The old document's 257 was wrong for the head it described, where the panic was at 267; the correction is right for this head.
- The damage candidate count is corrected to 153 with the `(N-1) + (N-2)` derivation, and `2N-3 = 153` gives the 78-page fixture the text names. This lane's earlier arithmetic finding is addressed exactly.
- Source and documentation commits are named separately, and the `db.rs` blob is identical across them, so counts measured at the source commit do hold at the head.
- The working-tree `docs/verification/ready-40.md` hashes to blob `6c90f8617ee5f9a654b5727c57abb58f26be171c`, identical to the committed blob at `fab4c649`. Note for whoever checks this: a plain `git diff fab4c649 -- docs/verification/ready-40.md` run from the primary prints 335 deletions, because that file is untracked relative to `main`, so the blob-hash comparison is the correct receipt and the diff is not.

## Damaged-file repair is not power loss

Everything above drives redb's own repair path by damaging pages of a closed file, which models a lost commit.
It is not a power cut, and a surviving process is not power loss.
The author states this in both the PR body and the verification document, and I concur.
`docs/design.md` line 36 records bounded loss of recent writes on a crash as acceptable, and line 117 keeps zero data loss in crash-injection tests as a success criterion, so that criterion remains unmet and is not claimed met here.

## Integration with current main

`main` is `e8b3f792c985be4895c798f4e84c60f8b062512c`, and it does not touch `crates/cowfs-meta` at all since the base `46b0f26`, a count of zero files.
`git merge-tree --write-tree e8b3f792 fab4c649` produced tree `a9970ccea9afa8277e02e1141991c059875c7fcd` with exit 0 and no conflict output.
So this is a clean merge with no overlapping hunks, and nothing here can disturb the merged 92 and 112 backend holder work or the 96 and 111 durable-namespace work, none of which touches this crate's metadata.

Core callers are compatible.
A repository-wide grep shows no caller anywhere sets `ino_block`; the only occurrences are inside `cowfs-meta` itself and the default of 16384, so every real caller uses `Options::default()`.
`crates/cowfs-core/src/lib.rs:190` and `crates/cowfs-gc/src/state.rs:446` call `Meta::open` with caller-built options, and since the stored value governs after creation their behaviour is unchanged for stores this build creates, while legacy stores behave exactly as before except that recovery refuses.
I changed no Core source.

## Closing references: neutral, and issue #40 stays open

- The PR body ends with `No whole-issue closing phrase: #40 stays open for the items above.`
- All four commit subjects carry only `(#40)`, which is not a GitHub closing keyword. No commit contains `close`, `fixes` or `resolves` applied to #40.
- GraphQL `closingIssuesReferences` for PR 116 is an empty list.
- Issue #40 is open.

So nothing will auto-close #40 on merge, and this review claims no whole-issue closure.
Whether to close anything later is the coordinator's call, not this lane's.

## Deferred, and not new blockers from this review

M2, `Health.poisoned` as a fix rather than a surfaced field, wiring `cowfs_meta::Health` into `cowfs_core::Health`, the real-`cowfs-store` crash re-run, the `docs/v1-meta.md` performance table, the mutant harness, and real power-loss acceptance all remain deferred.
They are disclosed by the author, they are out of this lane's scope, and none of them is a reason to hold this revision.

One operational consequence follows from the chosen design and is worth stating plainly, without treating it as a defect.
A store created by a build from before this key existed keeps `FORMAT_VERSION` 2 without the key permanently, so `open_recover` will refuse that file forever.
Refusing is the right call under the instruction not to guess a legacy reservation size, and the document does disclose the refusal, but an operator holding an existing store that ever needs recovery has no path with this build.
One sentence in the verification document saying the condition is permanent for pre-existing stores would close that gap in disclosure.

## Recommendation

Approve and merge.
The original blocker is closed and I reproduced both the fix and its failure mode on the exact head.
The counts, format and clippy reproduce, CI is green, and the merge into current main is clean with no overlapping hunks.
The single item worth adding before or shortly after merge is one `health.rs` test for the inline `Ack::Applied` hook-panic path, which is correct today and entirely untested.

## Resource and safety record

Free disk was 335 GiB against a 20 GiB floor, and every artifact stayed under the 8 GiB cap.
All cargo work ran inside the wave's `mac-heavy.lock`: one 600-second foreground hold covering the source receipts, both recovery-test runs, the probe, the whole-crate run, format and clippy, and the tree integrity check, then one short additional hold to finish my own probe, which had failed to compile on a partial move in my test code.
No measured step was run twice.
Each tree had its own `CARGO_TARGET_DIR`, and the source sha256 of each `db.rs` was printed before its build, so the `cp -R` mtime trap that produced a false reading in this lane's previous session cannot recur here.

My probe lived in a separate tree from the shipping source.
The pristine tree was verified byte-identical to the published tree, 532 files with zero mismatches and zero extra files, and format and clippy ran against that pristine tree.

No daemon was launched.
The shared daemon PID 15263 is not running on this host now; I did not signal, start or restart it, and I did not touch the shared store, mount or socket.
Every damaged file was a private `tempfile` copy inside this lease.
No irreversible action was taken, so there was nothing to verify before one.

Lease `cowfs-ready45`, slot 8, stays at `0092804` with a clean tree, and no other lease, slot or agent's files were written.