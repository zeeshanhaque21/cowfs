# PR 139 final independent review: the cache-write clock ordering

Reviewed head: `ad6dca8b7b5bd331e8e553e7874c0852458a1c15`, branch `fix/cache-write-clock-42`.
Base of the change: `e7ee215878cc0102ce52c7611dddc81f064105f6`, the #136 tip.
Current `main` at review time: `b486d4541bc47b273a5bbd222b95c24fed05c36d`, which now contains #136.
Author: READY6. Reviewer: READY1, doc-only, disjoint from the author's files.

**Verdict: PASS on the contract, with one permanent-regression risk that is not the author's to close
alone.** Merge was not performed here.

This review edited no source, no test and no manifest, created no issue, and took no lease action.

## The digest, verified before anything was trusted

| item | sha256 | how verified |
| --- | --- | --- |
| `docs/verification/evidence/meta42-cache-write-clock.md` | `a8bc5337cbf5bb9ac28c1d2801377cce552681432fcd2b5b18fad35bfad42451` | canonical PRIMARY copy, and the blob committed at `ad6dca8b` |

The digest the brief carried is correct, and it is the same in the canonical primary copy and in the
committed blob, so the committed document is the canonical document and not a local variant.
I checked rather than assumed, because a previous digest in this lane was wrong.

## The change, read directly

One statement moved, `3 insertions, 1 deletion`, in one file.
`crates/cowfs-core/src/io.rs` at `ad6dca8b` is `373f5c47283b`; at `e7ee215` it is `9628e675258d`.
`inner.rs` `09219472a797`, `tx.rs` `7372c24c3129` and `cowfs-core/src/lib.rs` `efc502cae4b0` are
byte-identical across the change.

`Timestamp::now()` moves from immediately before the lock block to inside it, after the poison check
and before the `NodeState` destructure.

I read the whole of `op_write` at the reviewed head and checked the order of every step rather than
trusting the prose. It is unchanged and in the same position: the `u32::try_from` length conversion
returning `InvalidArgument`, `file_node`, the `MAX_FILE` bound returning `NoSpace`, the zero-length
early return, `ensure_file`, the eight-iteration partial-chunk verify loop with its two `Stale`
returns, the node write lock, the poison check, the `Stale` return for a vanished file, the
`dirty_bytes` accounting, the `q.lk()` dirty-file touch, the in-lock `try_enter`, `flush_locked` with
its poison and degrade arms, and `relieve`.

`now` is used only at `attr.mtime` and `attr.ctime`, both inside the same block, so the move is
self-contained and needs no other change.

## What the injection arrangement does and does not do

I built my own probe arm rather than reusing the author's, so this is my own assessment.

The delta in each private arm is a spin on an atomic, plus the statics and the test file.
Anchor counts were checked before building and each anchor appears exactly once, and the builder
refuses on any other count.
The delta in `io.rs` is seven added lines and nothing removed: an atomic load, a `fetch_add`, and a
`yield_now` loop.

- It injects no clock value, replaces no reading, mocks nothing and synthesises nothing.
- It holds no node lock while parked, so the second writer cannot deadlock behind it.
- It returns no value into the functional path, and every expression it evaluates is pure.
- It does not skip, reorder or weaken any validated input: it sits after the partial-chunk verify loop
  and before the lock, and the arm's own asserts on durable equality and content still ran.
- For a normal build `PROBE_ARM` is `false`, so the only added cost on the shipped path is one atomic
  load per write.

The differential is the right one.
In the old arm the parked writer has already read a real clock value and holds no lock; in the new arm
it has read nothing and holds no lock, because the reading is inside the lock.
Both arms therefore impose the same functional interleaving, writer A applying last, and differ only
in where the clock is read.
No measurement is taken between the two racing epochs; the probe waits on the atomic and then reads
the two times after both writes have completed.

## Independent old-fail, new-pass

My own probe, one test, one sample per arm, run under one bounded foreground acquisition of the shared
`mac-heavy.lock` each time.

| arm | source | exit | outcome |
| --- | --- | ---: | --- |
| old | `e7ee215` plus the probe arm | **101** | cached `ctime` regressed below an observed value, 1 failed |
| new | `ad6dca8b` plus the probe arm | **0** | 1 passed |

Old arm, my log, verbatim:

```
PROBE clock after_second=Timestamp { secs: 1791242472, nanos: 801621000 } final_cached=Timestamp { secs: 1791242472, nanos: 801069000 } final_bytes="AAAAAAAA"
PROBE clock reopened_ctime=Timestamp { secs: 1791242472, nanos: 801069000 } reopened_bytes="AAAAAAAA"
PROBE clock VERDICT ctime_regressed_below_observed=true durable_equals_final_cache=true bytes_linearized=true
panicked: the cached ctime Timestamp { secs: 1791242472, nanos: 801069000 } moved backwards below
  Timestamp { secs: 1791242472, nanos: 801621000 }, which a client had already been told
```

New arm, my log, verbatim:

```
PROBE clock after_second=Timestamp { secs: 1791242495, nanos: 907194000 } final_cached=Timestamp { secs: 1791242495, nanos: 907218000 } final_bytes="AAAAAAAA"
PROBE clock reopened_ctime=Timestamp { secs: 1791242495, nanos: 907218000 } reopened_bytes="AAAAAAAA"
PROBE clock VERDICT ctime_regressed_below_observed=false durable_equals_final_cache=true bytes_linearized=true
PROBE clock PASS no regression across the forced interleaving
```

My measured regression is 552,000 ns where the author's record reports 4,000 ns.
Both are non-zero and both are the same defect; the magnitude differs because the gap depends on how
long the parked writer waits, and neither number is a rate.
The direction is what the test asserts, and it flips between the two arms.

Content is `AAAAAAAA` in both arms, the last writer's bytes, and the durable value after flush and
reopen equals the final stable cached value exactly in both arms, so the replay is faithful and this is
a cache-layer defect rather than a replay contract failure.
That property is the one the #136 clarification established, and this run re-confirms it independently.

## Three failures of my own, recorded rather than dropped

I am recording these because two of them produced a result that could have been reported as a verdict
and would have been wrong.

**A false FAIL on the wrong assertion.** My first working run failed with `reopen getattr: Stale`,
because I reused an inode number from the previous `Core` in the reopened one.
The regression was visible in the printed data, but the test failed for an infrastructure reason, not
the property under test.
Reporting that exit as the old-arm verdict would have been a false FAIL.

**A miswritten content assertion.** With the reopen fixed, the run failed on `reopened_bytes == B`.
Writer A is the last applier, so the correct expectation is `A`; my assertion had the polarity backwards
while the printed line was right.
Again a wrong-reason failure.

**A deleted TMPDIR.** One rebuild removed the private temp directory and the next run failed with
`PathError NotFound` before reaching any assertion.

After fixing all three, the old arm fails on the regression assertion itself and the new arm passes.
The three failures are why the final result is trustworthy rather than merely favourable.

## Scoped checks, on the shipped tree with no probe present

Extracted from `ad6dca8b` and checked for probe residue before running: `PROBE` appears 0 times in
`io.rs` and 0 times in `lib.rs`, and no probe test file is present.

| check | result | exit |
| --- | --- | ---: |
| the new-arm probe, `--exact` | 1 passed | 0 |
| `cargo test --locked -p cowfs-core --test operation_time` | 4 passed, 0 failed | 0 |
| `cargo test --locked -p cowfs-core --test locks` | 2 passed, 0 failed | 0 |
| `cargo test --locked -p cowfs-core --test caches` | 2 passed, 0 failed | 0 |
| `cargo fmt --all -- --check` | clean | 0 |
| `cargo clippy --locked -p cowfs-core --all-targets -- -D warnings` | clean, zero diagnostics | 0 |

`locks` matters most for this change, because reading a clock while holding the node write lock is only
safe if the lock order is unchanged.
It passes at 2 of 2.
`operation_time` is #136's deferred-`ctime` fixture and still passes at 4 of 4, so the change did not
disturb the replay contract it pins.

The author's wider scoped set, `critic2b`, `chunks`, `flush_boundary`, `poison`, `durability`,
`invariant` and `core`, was not rerun here and is carried as the author's.
No workspace run, no 91-suite matrix, no rate, no performance or timing measurement, and no power-loss
claim.

## Budget

Measured before any archive or build: `bench/out` at 0.92 GiB of the 8 GiB cap with 297,446,736 KiB
free against the 20 GiB floor.
After three compiles and three probe arms it stands at 2.47 GiB, leaving 5.53 GiB of headroom.
No cleanup, pruning, moving, offloading or cap waiver was performed; that is READY5's approved scope,
not this lane's.
No signal, no shared resource, no mount and no lease action.

## The permanent-regression risk, stated precisely

**The shipped tree carries no test that exercises this fix.**
The rendezvous exists only in private archive copies, so CI cannot regress this ordering: a future
change that moved `Timestamp::now()` back above the lock would leave every test in the repository
green, exactly as it was green before the fix.
That is the precise risk, and it is a real one rather than a formality.

The change also introduces a new lock-hold region.
The node write lock is now held across a `Timestamp::now()` call, which is a single clock read served
by the vDSO on this platform.
That is the mechanism of the fix and the cost of it, and it lengthens the critical section by that
one read.
It is small, but it is a new lock-hold region in the hottest write path and it belongs on the record
rather than only in a code comment.

A second, smaller behaviour change: a poisoned node no longer reads the clock at all, because the
reading now happens after the poison check.
The returned error is identical, so this is strictly less work on an error path, but it is a difference.

### Whether a minimal test-only seam is possible, without inventing anything

Yes, and it does not require a new framework, because this crate already has the pattern.

`cowfs-core` already ships two test-only seams on `Core` that follow a house style, both
`#[cfg(test)]` and `#[doc(hidden)]`: `set_gate_fault`, documented as built only for the crate's own
tests so a production build cannot turn a fail-open barrier on, and `gate_waiters`.
There is also a `#[cfg(test)] mod gc_barrier_window`, so a crate-internal test module is the established
place for driving one.

`io.rs` currently contains zero `cfg(test)` occurrences, so using this pattern would add `cfg(test)`
code to the file this change already owns, and nothing outside it.
A rendezvous parked at the same boundary, driven by a `#[cfg(test)]` accessor in the crate's own test
module, would let a permanent test reproduce the interleaving deterministically without a
`#[cfg(feature)]` gate, a new public API, or a new dependency.

I did not write it.
The brief forbids adding hooks, source fixes or tests, and a permanent concurrency test is a design
decision with a cost, so it is a coordinator call, not mine.
My assessment is that it is feasible at low cost and that leaving the fix untested in CI is the weaker
position.

### What this change does not claim

- **Not global monotonicity.** `Timestamp::now()` is the host wall clock, and the host wall clock can
  step backwards.
  This change orders the reading relative to the node lock, so a writer's `ctime` reflects its own
  application point.
  It does not make time monotonic, and no claim of absolute or global monotonicity is made or implied.
- **No rate.** One forced interleaving per arm proves the boundary could produce the inversion, not how
  often it is hit.
- **The scope of the proof is this interleaving on this host clock**, with the ordering imposed by a
  test-only rendezvous.
- **`io.rs:168`, `io.rs:436` and `ns.rs:176` are not fixed and not reproduced here.**
  They have the same read-then-lock shape; the clarification recorded them as shapes to check, not as
  measured defects, and the brief forbids fixing an unreproduced path.
  They remain hypotheses.
- The historical `7 us` and `9 us` gaps stay unreproduced, per the clarification.

## Integration, read-only

`git merge-tree --write-tree origin/main ad6dca8b` against the current `main` `b486d454`:
tree `db06313fd43800f101439d5914e30d4c300af607`, zero conflict lines, exit 0.
Merge base is `e7ee215`.
The integration is clean and this is a read-only computation, not a merge.

## CI at the exact head

One snapshot, not polled, not rerun, not dispatched, no workflow or runner change:

```
linux-fuse             completed/success
check (ubuntu-latest)  completed/success
check (macos-latest)   completed/success
total_check_runs=3
```

All three green at `ad6dca8b`.
`mergeStateStatus` is empty in the REST payload, which is the field GitHub does not populate for every
state; `mergeable` is `true`.
No claim is made about any other head, any other PR, or the repository's overall status.

## Closing-reference hazard, and #42

`closingIssuesReferences` is empty for this PR, and the audit went further than that field because the
field is not trustworthy on its own.
Every commit message PR 139 would newly bring was scanned for a closing verb bound to an issue
reference: `e9dc106`, `8670d36` and `ad6dca8b` carry none.
`e9dc106`'s subject contains `fix(core):`, which is a conventional-commit prefix, not a closing form,
and its body says "They are not fixed here", which is a negation with no issue reference bound to a
closing verb.

The dependency commits from #136 are already on `main`, and scanning them surfaced the hazard directly:
**commit `57232166`, which is my own commit from an earlier task, contains the line "Not merged here
and nothing here closes #42."**
That is a negated closing form bound to an issue reference, which GitHub does not honour.
It is what auto-closed #42 when #136 was merged, and it is my defect, not the author's.

`#42` is currently `open` with `state_reason: reopened`, which I verified directly.
Whole #42 stays open: requests 1, 3 and 4, the hole flag, the reservation work and the `Core` integration
are untouched by this change, and this change does not close any part of it.
History is not rewritten to remove the offending line.

PR 139 is **not a draft**, `draft=false`, `state=open`, 3 commits, 2 changed files.
It was reported as expected to be a draft; it is not, and I did not change that.
It is not merged here.

## Tools

`no-mistakes` is **not initialized** in this repository, `.no-mistakes` and `.claude` are both absent,
so that pipeline was not run and no claim is made about it.
No browser step was taken; `chromium` is not installed, so any browser surface is **UNVERIFIED**.
`codebase-memory-mcp` graph tools were not used; the binding this review required was the extracted
archive compared per file with `git hash-object`.
MisakaNet was available only as a local stdio server and was not consulted; no failure-recall need
arose and no remote call was made.

## Evidence

Under `bench/out/meta42-cache-write-clock-final-critic/` in the assigned worktree, gitignored:

| file | what |
| --- | --- |
| `scripts/build-arm.py` | this review's own arm builder, refusing on any anchor count other than 1 |
| `archives/old.tar.gz`, `archives/new.tar.gz` | the two source archives, `e7ee215` and `ad6dca8b` |
| `archives/old-src/`, `archives/new-src/` | the two probe arms |
| `archives/ship-src/` | the clean shipped tree the scoped checks ran on, no probe present |
| `logs/old-probe.log` | old arm, the regression and its nanoseconds, exit 101 |
| `logs/new-probe.log` | new arm, the pass and the durable equality, exit 0 |
| `logs/lane.log` | every lock acquisition with its UTC time |
| `logs/fmt.log`, `logs/clippy.log` | fmt and scoped clippy |

The author's lane at `.treehouse-ready-wave/.treehouse/cowfs-7c1bf8/6/cowfs/bench/out/meta42-cache-write-clock`
was read only, and nothing in it was modified.
The READY3 hole-flag archives, the READY5 rename work, and every prior #118, #120, #135 and #136 report,
log and binary are preserved untouched.