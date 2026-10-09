# PR #134 final delta: the daemon regression is shipped, the refusal order is restored, and both are pinned against the unsafe code

Reviewer lane: `.treehouse-build-train/.treehouse/cowfs-7c1bf8/6/cowfs`, held slot 6.
Lease verified before any write: branch `review/gc-root-mark-retention-82`, HEAD `b4b55ab`, working tree showing only the six pre-existing untracked `docs/reviews/*.md` files from other lanes, no process of mine running.
Matched the expected state, so the lane was used.
A mismatch would have stopped the review; none was found.

Head reviewed: `6350468f049ad1e9087a72c62fe4231e2d832fe7`.
Base reviewed: `93cfef94457a989d031cb6b0a475ac4edbdb85ef`.
Prior scoped-pass head this is measured against: `8264ca73238eb5fb7d567946f8699fd188b77a60`.
Prior report this supersedes in exactly one respect: `docs/reviews/pr134-swap-provenance124-recovery-final.md`, sha256 `ccc5eafccea4cb32606e49aec4eab4a7c8a124be4afdd22069c54276e6c34e50`, preserved immutable and committed byte for byte on this head.

Review date: 2026-10-05.
Artifacts: `bench/out/swap-provenance124-final-delta-critic/**` inside the lease.
This document: canonical PRIMARY copy, `docs/reviews/pr134-swap-provenance124-final-delta.md`.

## Verdict

PASS, for issue #124 only.

Both items the scoped pass left open are now done in the repository, not in a reviewer's probe.
The third item, the refusal ordering I reported as a finding, is corrected and pinned by a test on both backends.
I re-derived the central claim against the unsafe code myself before trusting the new head.
The author does not assert it; I measured it.

No provenance restore was reintroduced.
The contract is not weakened anywhere in this delta.

## What the delta contains, and nothing else

`git diff --name-only 8264ca7 6350468` is exactly five paths.

| Path | Change |
| --- | --- |
| `crates/cowfs-daemon/src/backend.rs` | the three tests, two test helpers, and the refusal reorder |
| `crates/cowfs-daemon/Cargo.toml` | one line: `cowfs-meta` under `[dev-dependencies]` |
| `Cargo.lock` | one added dependency line, no version change |
| `docs/reviews/pr134-swap-provenance124-recovery-final.md` | my prior report, committed unmodified |
| `docs/verification/evidence/swap-provenance124-final-regression.md` | the new canonical receipt |

Typed source carry, blob for blob, against `8264ca7`:

| Path | Blob at `6350468` | Blob at `8264ca7` |
| --- | --- | --- |
| `crates/cowfs-core/src/lib.rs` | `efc502cae4b04a811e4e6d4260115f8d748da0d4` | identical |
| `crates/cowfs-core/src/swap.rs` | `4cbc3f289b68e8d626321a481525ced5f5562af0` | identical |
| `crates/cowfs-daemon/src/base_meta.rs` | `6c020ae303b7ac3213efe87a0c205629acac12d5` | identical |
| `crates/cowfs-meta/src/db.rs` | `b08ed7f7e204a60625124b13415f471ec82800b8` | identical |
| `crates/cowfs-daemon/tests/swap_provenance_124.rs` | `bfc2336fd2f97831dbd4e47d4b5e1b69eb69e23b` | identical |

The core, the record primitives, the metadata hook plumbing and the nine integration tests are untouched.
`Cargo.lock` gains `"cowfs-meta"` under the `cowfs-daemon` dependency list and nothing else, so no dependency version moved.

`backend.rs` at the head under test: git blob `9e26f48`, sha256 `23149848688e104d336fcbfd7ab0f2d89dfbecf7fb234e18720e3ec39d0974c5`.

## The refusal order is restored, and my finding is closed

The scoped pass found that `backend.rs:728` checked `name == from` before the existence loop, so the core backend answered a self-swap of a nonexistent name with `InvalidInput` where it used to answer `NotFound`, and disagreed with the path backend.

`crates/cowfs-daemon/src/backend.rs` at this head, `CoreSnapshots::swap`:

- `self.bases.exclusive(...)` opens at line 720.
- The existence loop for `[name, from]` runs at lines 733 to 742, inside that section.
- The same-name refusal runs at lines 743 to 748, after it.
- Invalidation and `promote_base` follow.

Existence first, same as `Core::promote_base` at `crates/cowfs-core/src/lib.rs:359`, which looks the source up before anything else.
Both backends now answer the same input the same way, and the refusal still leaves the full old tree and the full old record, because the check and the swap are inside one critical section that every namespace mutation also takes.

The comment at lines 729 to 732 states the reason, including that `promote_base` would refuse these too but only after the record was cleared.
That is the point I raised, recorded in the code.

## The daemon regression is real, and I proved it rejects the unsafe code

The claim to test is the one my scoped pass could only demonstrate in a private probe: an admitted swap that fails past the point of no return must leave the record unknown.

The author claims the new shipped test fails on `4646f51`.
I did not take that on trust.
I extracted the three tests and their two helpers verbatim from the head's `backend.rs`, lines 1201 to 1418, verified the block is present byte for byte in my copy, grafted them into a private archive of `4646f51`, and added only the one `cowfs-meta` dev-dependency line that tree lacks.
No production source on either head was edited.

Result on the unsafe source, `crates/cowfs-daemon/src/backend.rs` sha256 `5e8d640b3891731e57c0fd13f90837a5a8614d396ecff31ecfbaacb3af37116a`, exit `101`:

```text
test backend::tests::a_swap_of_a_nonexistent_name_with_itself_is_not_found_on_both_backends ... ok
test backend::tests::a_refused_swap_keeps_the_full_old_tree_and_record_on_both_backends ... ok
test backend::tests::a_core_swap_that_fails_past_the_point_of_no_return_never_restores_the_old_commit ... FAILED
panicked at crates/cowfs-daemon/src/backend.rs:1293:9:
assertion `left == right` failed: the record still names a commit that did not produce this tree:
  SnapshotInfo { name: "base", parent: None,
    base: Some(BaseMeta { repo: Some("/repoA"), git_ref: Some("refs/heads/main"), commit: Some("commit-AAA") }),
    created_unix_ms: 1791232920537 }
  left: Some("commit-AAA")
 right: None
test result: FAILED. 2 passed; 1 failed; 0 ignored; 0 measured; 100 filtered out
```

That is #124's defect, reproduced by the shipped test, on the code that contains it.
The assertion did not fire vacuously: the test asserts the new tree is in place and the intent file is gone before it checks the record, so the `commit-AAA` above is a real stale claim over a real rolled-forward tree.

The two refusal tests pass on the unsafe source too.
That is what makes them regression pins rather than assertions that only the new code can satisfy.

On this head, the same three tests pass, twice, exit `0` both times.

## The hook wraps the durability hook, and I measured it rather than reading it

This was the part of the claim most likely to be wrong, so I checked it twice, by source and at runtime.

By source, `crates/cowfs-core/src/lib.rs:198` to `207`: `open_with_meta` builds `mopts`, sets `mopts.before_sync = Some(store_sync_hook(&store))`, and only then calls `make_meta(dir, mopts)`.
So the real store sync hook is already present in the options the closure receives.
The test at `backend.rs:1222` takes it with `o.before_sync.take()`, wraps it, and reinstalls it.
Nothing is replaced.

At runtime, in a private copy where I counted the inner hook's invocations, one locked hold, exit `0`:

```text
critic-order: production hook present in options at make_meta time: true
critic-order: FAILING k=3 AFTER the real hook ran; inner_runs=3
```

`inner_runs=3` means the store sync had already happened three times, including on the failing commit, before the injected error returned.
So the failing commit is a real lost commit after durable data, not a commit whose store sync was skipped.
There is no early return ahead of the counter or ahead of the inner call: the increment is the first statement and the inner call precedes the fault return.

Counter stability: `background: false` on the injected `Core` at `backend.rs:1221`, so no background flusher can call the hook and shift the index.
The test also asserts the third commit is the one that fails, and asserts the intent file is pending, so a silent shift in which commit is targeted would fail rather than pass quietly.
Two consecutive runs produced identical results.

## Test hygiene, checked rather than assumed

- No shared store. Each test opens its own `tempfile::tempdir()`. The two multi-backend tests use `dir/corestore` and `dir/pathstore`, siblings, never nested, and the comment at `backend.rs:1333` explains the reason: a core store holds a lock, so a second open of the same store is refused outright.
- Seeding happens before either backend is reopened, in a scoped block at `backend.rs:1276` to `1280`, so the seed core is dropped and its lock released before the fault-injecting core opens.
- Drop before reopen. `drop(s)` at `backend.rs:1290` precedes the `CoreBackend::open` at `backend.rs:1298`, so the reopen is not contending with a live core.
- No vacuous commit check. The test asserts tree content `BBBB-from-srcB` at `backend.rs:1303`, asserts the intent file is gone at `backend.rs:1308`, then asserts `commit` is `None` at `backend.rs:1311`, `repo` is `None` at `backend.rs:1316` and `git_ref` is `None` at `backend.rs:1321`.
  A record that dropped only the commit would still fail.
- Private to the crate, as it must be. The tests live in `backend.rs`'s own `mod tests`, because `CoreSnapshots`' two fields are private and `CoreBackend` exposes no meta-options hook. Nothing was added to `tests/`, no public API changed, no fault framework was introduced.

## No restore, and the contract is not weakened

`rg -c restore_base_record` over the head's `backend.rs` returns `0`.
`rg -c 'rolled back'` returns `0`.
`invalidate_base_record` still returns `io::Result<()>` and still clears before any tree operation.
The rollback path that caused the original block is absent, and this delta does not touch it.

The refused-swap cost the receipt discloses is unchanged and still accepted by me: a swap that genuinely rolled back loses still-true provenance and reports stale.
That is the correct floor for code that cannot distinguish rollback from a failed roll-forward, and this delta does not weaken the contract to paper over it.

## Scoped results at this head

One foreground `mac-heavy.lock` hold for the batch, 600s acquisition, isolated `CARGO_TARGET_DIR`, project-local `TMPDIR`, exit codes taken directly from the command, never through a pipeline.
One further hold for the unsafe-source graft, one for the hook-order probe.

| Check | Result | Exit |
| --- | --- | --- |
| three new lib tests, run 1 | 3 passed, 0 failed | 0 |
| three new lib tests, run 2 | 3 passed, 0 failed | 0 |
| `cargo test -p cowfs-daemon --locked --lib` | **103 passed**, 0 failed, 9.82s | 0 |
| `--test swap_provenance_124` | 9 passed, 0 failed | 0 |
| `--test snapname_drift` | 4 passed, 0 failed | 0 |
| `cargo test -p cowfs-core --locked --test swap` | 3 passed, 0 failed | 0 |
| `--test namespace_durability` | 0 passed, **3 ignored**, 0 failed | 0 |
| `cargo fmt -p cowfs-daemon -- --check` | clean, empty log | 0 |
| `cargo clippy -p cowfs-daemon --all-targets --locked -- -D warnings` | zero `warning` or `error` lines | 0 |
| three new tests grafted onto `4646f51` | 2 passed, **1 failed** with `commit-AAA` | 101 |
| hook-order probe, counting inner runs | wrap confirmed, `inner_runs=3` | 0 |

`103` is exactly the previous `100` plus the three new tests, which is the arithmetic I expected and the reason to believe nothing else in the library changed.
The three `namespace_durability` tests are reported as ignored, not as passing. They mount a filesystem and kill a daemon, and were not run.

## CI, one snapshot, on this head

One `statusCheckRollup` read for the last commit of PR #134, exact head `6350468`.
Nothing was dispatched, rerun, or reconfigured, and nothing was polled.

| Check | Status | Conclusion | Started |
| --- | --- | --- | --- |
| `check (ubuntu-latest)` | QUEUED | null | 2026-10-05T20:31:35Z |
| `check (macos-latest)` | IN_PROGRESS | null | 2026-10-05T20:36:47Z |
| `linux-fuse` | IN_PROGRESS | null | 2026-10-05T20:41:55Z |

CI on this head is **not green**. None of the three has completed.
`mergeStateStatus` is `UNSTABLE`.
The three green checks recorded for `4646f51` in the earlier receipt say nothing about this head, and this delta adds two commits on top of `8264ca7`, whose own two checks were still running when I reviewed it.
CI must be read again before merge.

Other PR state, same snapshot: `headRefOid` `6350468f049ad1e9087a72c62fe4231e2d832fe7`, `baseRefOid` `93cfef94457a989d031cb6b0a475ac4edbdb85ef`, `state` OPEN, `isDraft` true, `mergeable` MERGEABLE, `reviewDecision` null.
`closingIssuesReferences` is empty and issue #124 is open, title "Backend swap can retain base provenance from the replaced tree".

Current main candidate: `origin/main` is `93cfef94457a989d031cb6b0a475ac4edbdb85ef`, which is the merge base and the base, so main has not moved.
`6350468` is not an ancestor of `origin/main`, so nothing has been merged.
Seven commits from base to head: `b942276`, `cbac228`, `4646f51`, `264eec1`, `8264ca7`, `363268b`, `6350468`.

## Six committed documents, all byte-for-byte

Verified with `git cat-file` against the blobs inside `6350468`, and separately against the MAIN primary checkout.

| Path | sha256 |
| --- | --- |
| `docs/reviews/pr134-swap-provenance124-final.md` | `803ea5c5e049225369ab72b50829a9f6483b2ac96ff1d06a9b16405d58c17e7e` |
| `docs/reviews/pr134-swap-provenance124-recovery-final.md` | `ccc5eafccea4cb32606e49aec4eab4a7c8a124be4afdd22069c54276e6c34e50` |
| `docs/verification/evidence/swap-provenance124-reproduction.md` | `3fde9b054fea530288e1fe3d4577bdbe97a626fad2f6f26cf92611b8d3a029a1` |
| `docs/verification/evidence/swap-provenance124-repair.md` | `0dcb6a695a8b933528a348209c46c13c216d5a933a396daa7a33ff187e4f80f9` |
| `docs/verification/evidence/swap-provenance124-recovery-repair.md` | `cfeffa7b35644feeef07296b223224f9b7e20db782ae4795f94b37a048e3bf2a` |
| `docs/verification/evidence/swap-provenance124-final-regression.md` | `bf80569e58346c691e956cdce5c05a6d34bfab0a3f48e4b034fcc046a84c8f26` |

The original blocked report `803ea5c5` and my scoped pass `ccc5eafc` are both unchanged.
The receipt that carried the false claim, `0dcb6a69`, is still in the tree with its original hash and was never rewritten; the retraction lives beside it.

This document supersedes exactly one disclosure in `ccc5eafc`, quoted there in its limitations section:

> The daemon harness is a `#[cfg(test)]` module inside a private copy, not a shipped test.

That is no longer true and is now false in the branch's favour: the same test ships in `backend.rs`, and it rejects the unsafe code, as measured above.
The rest of that report, the PASS verdict on the recovery head, the source bindings, the path backend deletion-site analysis, and every other limitation, stands unchanged and is not reopened here.

## Erratum in my own prior report

`docs/reviews/pr134-swap-provenance124-recovery-final.md:231` says:

> The harness replaces the store-durability hook.

That is wrong, and I am correcting it rather than letting it stand.
My probe did not replace the hook.
It took the hook `open_with_meta` had already installed, called it, and only then returned the injected error, exactly as the shipped test does.
I re-read my own probe crate at `bench/out/swap-provenance124-recovery-final-critic/probe-daemon/crates/cowfs-daemon/src/backend.rs` and confirmed it: `let inner = o.before_sync.take();` then `if let Some(h) = inner.clone() { h()?; }` before the `fail_at` check.

The consequence for that report: none of its results move. The wrap was present in my probe, so the old-unsafe versus new-safe comparison it reported was already measuring a wrapped hook, and the verdict and every measurement in it stand.
Only the prose was wrong.

## Limitations, stated plainly

- The path backend retired-tree behaviour is still source-level only. No `std::fs::rename` fault seam exists in this repository. This delta changes nothing on that path and makes no new claim about it.
- The fault class is the third durable meta commit. That is one real failure after the point of no return. It does not claim to cover every post-victim failure, and the test says so.
- The injected hook measures which commit fails, not durability ordering, and makes no durability claim of its own.
- The graft onto `4646f51` proves the test detects the unsafe code at that specific commit index. A hypothetical unsafe change that kept the record correct at commit three but broke it elsewhere would not be caught by this test alone.
- No live socket round trip, no mid-GC, power-loss or whole-filesystem acceptance, no performance or core blame claim. Browser unverified.
- `no-mistakes` is uninitialized in this lane and was not initialized.
- CI on this head has not finished, so this review cannot say the branch is green.

## Scope discipline

Issue #124 only.
No new issue, no new feature, no audit matrix, no new task.
Issues #125, #127 and #128 are parked and untouched.
No source edit on any head: the graft lives in `probe-old`, the hook-order probe in `probe-order`, and `head/` was verified byte-identical to `6350468` with `diff -rq` afterwards.
Both prior critic reports and both prior artifact trees are untouched.
No checkout, branch change or commit in the lease.
No lease acquired, returned, reset, stashed, pruned or destroyed.
No signal, restart, sudo, install, unmount, or store operation.
The shared daemon PID 15263 with start time `Sat Oct 3 20:44:29 2026`, the Linux host, and every store, mount and job were never contacted, and no mount was traversed.
Concurrent lanes untouched: the #136 critic on the metadata and `Core` inner clock, and the #135 author blocked on hosted mac capacity.
One disk reading: 298 GiB free against the 20 GiB floor; artifacts total 2.9 GiB against the 8 GiB cap.

## What remains before merge

1. CI green on `6350468`. Nothing has completed yet, and this review cannot stand in for that read.
2. Nothing else. The contract is sound, the regression ships, and the refusal ordering is pinned on both backends.

Nothing was fixed, merged, marked ready or closed.
PR #134 stays a draft, issue #124 stays open, and no new task was created.