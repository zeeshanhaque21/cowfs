# PR #139 guard-gap delta review: the typed clock read closes the coverage hole in the source, CI has not finished, and four receipt claims need tightening

Reviewer lane: `.treehouse-build-train/.treehouse/cowfs-7c1bf8/6/cowfs`, held slot 6, distinct from the READY6 author.
Lease verified before any write: branch `review/gc-root-mark-retention-82`, HEAD `b4b55ab`, working tree showing only the six pre-existing untracked `docs/reviews/*.md` files from other lanes, no process of mine running.

Head reviewed: `04d2fb554563df467b3f14a712f994c5d363ca19`.
Prior head this delta sits on: `cd1d5afa1f6cab57e479f911e1e202ce29fa7e49`.
PR base per the API: `b486d4541bc47b273a5bbd222b95c24fed05c36d`.
Current main, fetched into the local object store: `cf67e8a6b2f8d346485fdf1c71d24283da0b43a0`, the merge commit of PR #137.

Inputs I read as data, not as a standing verdict:
`docs/reviews/pr139-meta42-cache-write-clock-permanent-final.md`, sha256 `75a2310b5b74018171fba9116faec5b73850b5e712fa90b1555889ebe5d97920`, the READY1 report whose single finding this delta answers, and the author's new record `docs/verification/evidence/meta42-cache-write-clock-gap-correction.md`, sha256 `508698d027c3667fb258e3b4db36f78713789d04478077eb5a044022d90521c3`.

Review date: 2026-10-06.
Artifacts: `bench/out/pr139-gap-final-critic/logs/**` inside the lease, one CI log, 94,602 bytes.
This document: canonical PRIMARY copy, `docs/reviews/pr139-meta42-cache-write-clock-gap-final.md`.

## Verdict

SCOPED SOURCE PASS on the guard-gap delta, with the runtime gate explicitly PENDING.

The gap the `75a2310b` report found is closed at the source level, and it is closed in a way that survives inspection rather than by assertion.
`op_write` no longer calls `Timestamp::now()` directly.
It calls a helper whose signature requires a live `RwLockWriteGuard<NodeState>`, so a reading moved back above the lock or into the park-to-lock space cannot compile through that path, and the companion test-side counter makes the same move fail at runtime with the existing fixture.

I verified the ordering myself in `op_write`: guard, poison check, clock, `Stale`, dirty bytes, write, accounting, touch.
I verified the counter cannot be inflated by an unrelated read, because `clock_read` has exactly one call site.
I verified the stamp that reaches `attr.mtime` and `attr.ctime` is that helper's return and nothing else.
I verified the merge into current main is clean and that main's hole-flag `caches` variant, including its two-walks-agree assertion, survives the merge intact.

I did not compile, build, archive, probe or run anything.
CI on the exact head had not completed when I took my one snapshot, so the thirty-lib count, the three-arm mutant results and the lint results are the author's local runs, not mine and not yet CI's.
The runtime gate stays open.

Four claims in the new record overreach their evidence.
None of them is a code defect and none blocks the delta, but two of them are the kind of statement that gets quoted later as measured fact, so they should be corrected before the record is relied on.

## Budget: read-only, and nothing was run

The lane's `bench/out` measured 35.882 GiB against the 8 GiB cap before I started and 35.882 GiB when I finished, the difference being one 94,602-byte log.
No cargo invocation, no build, no archive, no target directory, no private probe, no heavy job, no deletion, pruning, move or offload, and no cap waiver.
Free space was 273.0 GiB and is not the gate; the gate is `bench/out`.
I did not use the MAIN checkout's `bench/out`, or any other lane's artifact tree, to work around that.

## The delta is exactly what it claims to be

`cd1d5af` to `04d2fb5` is three paths: `crates/cowfs-core/src/io.rs` at 50 insertions and 3 deletions, plus this delta's two documents.
Nothing else in any crate, no manifest, no lock line, no feature, no dependency, no `cowfs-store` or `cowfs-vfs` change.

The physical fix was already in `cd1d5af`, and this delta does not move it.
I measured the line index of each landmark in both heads: `park_if_armed()` at 88, `node.st.wr()` at 90, and the clock reading at 96 in `cd1d5af` and at 97 in `04d2fb5`, the one-line shift being the inserted helper block below.
The reading is inside the guard's scope in both.
What this delta adds is the typed helper and the counter assertion, which is exactly the gap-filling work and not a second fix.

## Ordering inside `op_write`, read rather than trusted

At `04d2fb5`, `op_write` is 61 to 133 or so, and the sequence is:

| Step | Line | Under the guard |
| --- | --- | --- |
| `len` conversion, `InvalidArgument` | 62 | no |
| `file_node` | 63 | no |
| `MAX_FILE` bound, `NoSpace` | 64 to 66 | no |
| zero-length early return | 67 to 69 | no |
| `ensure_file` | 70 | no |
| partial-chunk verify loop, `Stale` returns, `verify_partial` | 73 to 87 | read locks only |
| `park_if_armed()`, test-only | 88 to 89 | no lock held |
| `node.st.wr()`, the write guard | 91 | acquired here |
| `poisoned`, early `Err` | 92 to 94 | yes |
| `clock_read(&st)` | 97 | yes |
| `Stale` return if `file` is `None` | 99 to 101 | yes |
| `dirty_bytes` before, size, write, `dirty_bytes` after | 102 to 105 | yes |
| `mtime` and `ctime` assignment from `now` | 106 to 107 | yes |
| `self.dirty_bytes` accounting | 108 to 112 | yes |
| queue touch under `sc.q.lk()`, `try_enter` | 113 on | yes |

Two properties of this order are worth stating because they are the properties the fix exists to hold.
A poisoned node returns before the clock is read, so a refused write touches no timestamp and costs no clock call.
A `Stale` node returns after the clock is read, so that path reads the clock and changes nothing; that is a wasted read, not a wrong one, and it does not touch the counter invariant because the fixture's write succeeds.

The in-lock `try_enter` and the queue touch are unchanged, which is what keeps the lock order identical to before this delta.
`locks` is the target that would catch a lock-order change, and it is one of the author's local results I did not re-run.

## The typed half, and exactly what it does and does not prove

```rust
#[cfg(not(test))]
#[inline]
fn clock_read(_guard: &std::sync::RwLockWriteGuard<'_, NodeState>) -> Timestamp {
    Timestamp::now()
}
```

The signature is the mechanism: a reading routed through `clock_read` cannot be taken without holding the node write guard, because the borrow has nowhere to come from.
That is a compile-time property and the record labels it as one, which is the right labelling.

What it does not prevent is a developer writing `Timestamp::now()` directly at an earlier line, which compiles perfectly well.
That is precisely why the counter exists, and the record says so.

## The counter half, and the three ways it could have lied

```rust
#[cfg(test)]
#[inline]
fn clock_read(_guard: &std::sync::RwLockWriteGuard<'_, NodeState>) -> Timestamp {
    let now = Timestamp::now();
    READS_UNDER_GUARD.with(|c| c.set(c.get() + 1));
    now
}
```

I checked each of the ways this could produce a false pass.

**An unrelated read inflating the count.** It cannot.
`clock_read` has exactly one call site in the crate, at `io.rs:97` inside `op_write`.
The other `Timestamp::now()` sites in the file are `io.rs:172` in `op_setattr`, `io.rs:391` in `op_setxattr` and `io.rs:440` in `op_removexattr`, plus the two twins of the helper itself at 560 and 573.
None of them goes through the counter.

**The counter being carried in from an earlier operation.**
It cannot be, and the mechanism is worth naming precisely because the record describes it as "reset" and there is no explicit reset anywhere.
`READS_UNDER_GUARD` is a `thread_local!` `Cell<usize>` initialised by `const { Cell::new(0) }`.
The parked writer runs on a thread created by `std::spawn` at `io.rs:633`, so its slot starts at zero by construction, and the count is read inside that same thread's closure at `io.rs:640`.
The main thread's count, which the fixture's own second write at `io.rs:653` does increment, is never read.
Per-thread scoping is what makes the isolation work here, and no global reset is needed or present.

**The count passing while the wrong stamp is written.**
It could not pass for the mutant under discussion, and the reason is structural: `now` at `io.rs:97` is the only value assigned to `attr.mtime` and `attr.ctime` at `io.rs:106` and `107`.
A mutant that bypasses the helper with a bare `Timestamp::now()` and assigns that instead leaves the count at 0 and fails the assertion at `io.rs:662`.
What the counter cannot rule out on its own is a mutant that keeps the guarded call *and* adds a second, earlier direct clock read and assigns that one.
That would need an edit the receipts' own diffs do not show, and the behavioural half is what covers it: `final_cached >= after_second` at 673 and `a_reported >= after_second` at 678, plus the reopen equality at 694.
The two halves are complementary, and the record says so at 97 to 98 and 148 to 149.

The assertion order also matters and is right: the counter assertion at `io.rs:662` precedes the behavioural ones at 669 onward, so on a wrong-path mutant the counter fires first.

## The test fixture, unchanged where it should be

The delta does not touch `ClockGate`, `park_if_armed`, the test module's imports, `opts()`, the `SECOND` and `LAST` constants, the 60-second bounded wait, or any pre-existing assertion.
The diff hunks land at the call site, at the new helper block inserted below `park_if_armed`, at the join return, and at the new assertion.
The thread-local parallel isolation the fixture relies on is intact: the gate is armed by the parked writer's own thread at `io.rs:634` with the comment at 628 to 629 explaining that this keeps a second test's writers out of it.
The bounded wait at `io.rs:643` to 650 asserts inside the loop rather than only timing out, so a gate that never engages fails instead of hanging.
The flush, drop and reopen at `io.rs:683` to 703 are unchanged.

## Production cost: structural evidence only, and two claims go past it

The record's own framing at line 174 is right: the cost was measured at the object level, not asserted.
The five rows it reports, a production rlib with zero `ClockGate`, `park_if_armed` and `ARMED` symbols, zero `READS_UNDER_GUARD`, `reads_under_guard` and `clock_order_tests` symbols, exactly one undefined symbol the production object needs, namely `cowfs_vfs::types::Timestamp::now`, zero TLS or thread-local references in that object, and zero `__atomic` or `__sync` libcall references, are structural facts about symbols.
From the source I can confirm the same structure holds by construction: both the counter helper and the thread-local are `#[cfg(test)]`, the counter is a `Cell` and not an atomic, and `park_if_armed` is called only under `#[cfg(test)]`.
So no thread-local, no atomic and no gate machinery is compiled into a normal build, which is the load-bearing part of the cost argument.

Two sentences in the record go past that evidence and should be corrected.

Line 186 says the reading is "the same vDSO clock call it always was".
Nothing in the reported evidence establishes a vDSO route.
The evidence is a symbol name, `cowfs_vfs::types::Timestamp::now`, and a symbol name says nothing about which clock source the platform implementation reaches, least of all on macOS where the route and its inlining differ from Linux.
The defensible statement is that the production build makes the same single call to `Timestamp::now` that it made before, and no more.

Line 186 also says it is "reached through one inlined function".
`#[inline]` is a request to the optimiser, not a guarantee, and the record's own evidence is symbol-level, which says nothing about whether a call instruction survives.
A debug or incremental build may keep the call.
The defensible statement is that the helper adds one private call that the optimiser is free to inline and that carries no other effect, since the guard argument is borrowed and never read.

Neither overclaim affects the safety property, the ordering, the refcounts or the contract.
They are wording, and I am flagging them as wording rather than inventing a benchmark matrix to settle them, which the assignment rules out and which this change does not need.

## Three more accuracy notes in the same record

**The arm table contradicts itself about the `old` arm.**
Line 112 says the `old` mutant "is byte-identical to" the `gap` mutant.
Line 116 says the `old` arm's slide is "above the park" while the `gap` arm's is "into the park-to-lock space", which are different positions.
Lines 166 and 167 give the two arms different `io.rs` sha256 values, `3a791b9a…` for `gap` and `1bfbb733…` for `old`, and byte-identical files cannot have different digests.
At most one of those three statements is true.
Lines 116 and 167 agree with each other, so the reading that fits two of the three is that `old` is the above-park revert and line 112's "byte-identical" is the error.
That does not change the substantive result, because both wrong positions fail and the counter refuses both, but the arm table cannot be taken at face value and the reviewer cannot tell from the record which mutant `old` was.

**The line citations for the untouched clock sites do not resolve at this head.**
Line 277 names `io.rs:168`, `io.rs:436` and `ns.rs:176`.
At `04d2fb5` the untouched `Timestamp::now()` sites in `io.rs` are 172 in `op_setattr`, 391 in `op_setxattr` and 440 in `op_removexattr`.
Neither 168 nor 436 resolves to a clock read, and `op_setxattr` is missing from the list, so a reviewer following the citation lands on the wrong lines and on an incomplete set.

**The compile-error sentence is looser than the paragraph under it.**
Lines 66 to 68 say a reading moved back above the lock "is therefore a compile error".
That holds for a reading routed through `clock_read`; a bare `Timestamp::now()` at an earlier line still compiles, which is the whole reason the counter is there.
Line 92 states this correctly, so the headline sentence and the body disagree in strength.

The merge-tree difference is not an inconsistency and I record it so nobody reads it as one.
The record quotes `44e7bbcec61e0ddcb905c8d72cfb4cedabace146`; my run of the committed head gives `5cf4e415e464f07d3a164c2205bf4786a8e8ae4b`.
The author ran `merge-tree --write-tree origin/main HEAD` while the record itself was still an uncommitted working-tree file, so their input tree differed from the committed head by that one document.
Mine is the authoritative one for `04d2fb5`.

One check of mine came back a false negative and I am recording it rather than quietly dropping it.
I grepped the merged `tests/caches.rs` for a literal two-walks-agree phrase and got zero matches, which would have contradicted the record.
The assertion is real and the record is right: it reads

```rust
assert_eq!(
    raw, got,
    "the metadata walk and Core::live_blocks must agree now that the flag filters holes"
);
```

so my regex was wrong, not the receipt.

## CI on the exact head: one snapshot, not green

Run `37393266584`, read through the API: `head_sha` `04d2fb554563df467b3f14a712f994c5d363ca19`, `head_branch` `fix/cache-write-clock-42`, `event` `pull_request`, created `2026-10-06T00:18:02Z`, `status` `in_progress`, `conclusion` null.

| Job | Id | Status | Conclusion |
| --- | --- | --- | --- |
| `linux-fuse` | `112043107167` | completed | success |
| `check (macos-latest)` | `112043107530` | in_progress | null |
| `check (ubuntu-latest)` | `112043107562` | in_progress | null |

The PR reports `mergeStateStatus` UNSTABLE, `isDraft` false, `state` OPEN, `closingIssuesReferences` empty.
Nothing was polled, rerun, dispatched or reconfigured, and no workflow, runner or job setting was touched.

I read the one completed job's log in full and it is bound to the head, carrying `04d2fb554563df467b3f14a712f994c5d363ca19` literally.
It shows nine `test result:` lines, all ok, with zero `test result: FAILED`, zero lines starting `error` or `warning` and zero `##[error]`.

That job does not, however, test this change.
`linux-fuse` runs `cowfs-vfs-path --test native` and `cowfs-fuse`, whose targets are the unit binary, `coherence`, `conformance`, `mount` and `regress`.
It contains zero occurrences of `clock_order`, zero of `caches`, zero of `operation_time` and no `cowfs-core` test binary at all.
So the one green job is the FUSE surface, which this delta does not touch.

The evidence that would corroborate the guard test is in the two `check` jobs, which had not completed.
Consequently these remain the author's local results, not CI's and not mine: the three-arm table, the thirty-lib count, `locks` 2, `caches` 2, `operation_time` 4, the `fmt` and `clippy` clean results, and the object-level symbol table.
I do not restate them as verified.
The runtime gate is PENDING and a later read of this run is required.

## Integration against current main, read-only

`origin/main` was fetched into the tracked remote-tracking ref rather than left in `FETCH_HEAD`.
It resolves to `cf67e8a6b2f8d346485fdf1c71d24283da0b43a0`, the object is present locally as a commit, and the API agrees.
`HEAD` and the checked-out branch are unchanged: branch `main`, HEAD `93cfef94457a989d031cb6b0a475ac4edbdb85ef`.

`git merge-tree --write-tree cf67e8a6b2f8d346485fdf1c71d24283da0b43a0 04d2fb554563df467b3f14a712f994c5d363ca19` exits 0 with a single tree and no conflict list, and I read that tree rather than trusting the exit:

| Property of the merged tree | Result |
| --- | --- |
| `clock_read` definitions present | 2, the `not(test)` and `test` twins |
| `park_if_armed();` call present | 1 |
| `READS_UNDER_GUARD` occurrences present | 3 |
| `crates/cowfs-core/src/io.rs` identical to the head's | yes, main has not touched it |
| `crates/cowfs-core/tests/locks.rs` identical to the head's | yes |
| `crates/cowfs-core/tests/operation_time.rs` identical to the head's | yes |
| `crates/cowfs-core/tests/caches.rs` identical to **main's** | yes |
| hole test `live_blocks_filters_holes_and_yields_only_stored_blocks` present | yes |
| its two-walks-agree assertion present | yes, quoted above |
| `Cargo.lock` identical to the head's | yes |

So the merged tree keeps main's hole-flag `caches` variant whole, with the metadata-walk agreement assertion intact, and carries this delta's `io.rs` unchanged.
That is a statement about a tree, not a compile: no build of the integrated tree was run by me, and the record is explicit that it could not run one either, with 337,780 KiB of headroom against a 463,868 KiB proven `cowfs-core` compile need.
I make no claim that the integrated tree compiles or that its tests pass.

## Closing forms, including the negated one

The API's `closingIssuesReferences` is empty, which is the authoritative signal for the body as it stands, so nothing closes today.
Issue #42 is `open` with `state_reason` `reopened`, confirmed, and the body says "**#42 stays open.**" explicitly.

One line in the body is a negated closing form and is worth recording as a residual hazard rather than a finding:

```
This is not a replay fix and touches no replay code. **#42 stays open.**
```

`fix` and `#42` share a line, separated by a sentence boundary and a negation that GitHub's parser does not interpret.
The author's intent is unambiguous and the API agrees nothing closed, but the shape is exactly the one a scanner would flag, and a future edit that drops the period or shortens the sentence would turn it into a live reference.
The other `fix` occurrences in the body sit in prose with no reference on the line, and `fix/deferred-operation-time-42` is a branch name.

In the commit range `b486d454..04d2fb5`, seven commits:

- `e9dc106` `fix(core): read a write's clock after it takes the node lock (issue #42 cache-write-clock)` is the same shape as the hazard I flagged on PR #137: a `fix` scope prefix with `(#42 …)` trailing. Weaker than `fix #42`, and the API agrees nothing closed.
- `8b90129` `test(core): require the write guard at the op_write clock reading, closing the park-to-lock gap` contains the word `closing` with **no** issue reference on the line. That is a false positive, and I record it as cleared rather than as a hazard: the word is being used about a gap, not an issue.
- The remaining five commits contain no closing keyword at all, or contain one only in qualifying prose such as "the production fix itself is untouched".

Nothing here needs a rewrite, and rewriting published history would be the wrong repair.
The dependency on #136 is already merged history and is not touched.

## What this does not claim

- **No local execution.** I compiled nothing, built nothing, archived nothing, probed nothing and ran no test.
- **CI is not green and the runtime gate is open.** Two of three jobs had not completed; the third does not exercise this change.
- The thirty-lib count, the three-arm mutant results, the `locks`, `caches` and `operation_time` counts and the `fmt`, `clippy` and symbol-table results are the author's local runs. I verified that the record reports them and that the source structure is consistent with them. I did not reproduce any of them.
- The typed-helper half is a compile refusal, not a runtime observation, and the counter is not a lock-liveness or ownership probe.
- No global clock monotonicity is claimed and none is enforced; the ordering is imposed by the rendezvous, with two host-clock readings microseconds apart.
- No performance, wall-time, zero-overhead, power-loss or whole-workspace acceptance claim, and no benchmark matrix was invented to settle the two wording overclaims.
- `io.rs:172`, `io.rs:391`, `io.rs:440` and `ns.rs:176` are untouched and remain hypotheses; none was reproduced here.
- The three archive bindings, the per-arm target directories and the `old`-arm restore sha are the author's artifact verification. I read the receipt's account of them and did not traverse that lane's artifact tree.
- `no-mistakes` is uninitialized in this lane and was not initialized. Browser UNVERIFIED, and `chromium` is not installed on this host.
- Misakanet is local-only here and was not consulted; no local memory store was reachable in this lane.

## Scope discipline

Issue #42, the cache-layer `ctime` obligation, this guard-gap delta only.
No new issue, no new feature, no audit matrix, no new task.
Issues #125, #127 and #128 remain parked and untouched.
No production edit, no test edit, no fixture edit, no commit, no push.
No checkout, no reset, no stash, no branch change.
The one repository-state mutation is the explicit fetch of `origin/main` into `refs/remotes/origin/main`, which was authorised and moved no branch and no `HEAD`.
No lease acquired, returned, reset, stashed, pruned or destroyed, and no lease action of any kind.
No signal, restart, sudo, install, unmount or store operation, and no deletion of any kind.
The shared daemon PID 15263 with start time `Sat Oct 3 20:44:29 2026`, the Linux host `9879298996041209860`, and every store, mount and job were never contacted, and no mount was traversed.
Concurrent lanes untouched: READY1's PR #140 metadata critic, READY3's Core atomic-rename builder working in `lib`, `inner`, `swap` and tests, READY5's PR #140 author at `f5`, READY6 idle at `04d2fb5`.
`crates/cowfs-core/src/io.rs` is READY3's live file and I read it only, through `git cat-file` at the committed head, never from a working tree.
Preserved immutable and re-verified unchanged: `75a2310b` the READY1 report, `508698d0` the new record, and my own `a552af90`, `9341cc4f`, `471a0731`, `803ea5c5`, `ccc5eafc` and `4c3450f5`.
Files I own for this review: this document and `bench/out/pr139-gap-final-critic/logs/**` in the lease, which `.gitignore` excludes.

## What remains

1. A read of run `37393266584` after `check (ubuntu-latest)` and `check (macos-latest)` complete, showing `cowfs-core --lib` at thirty passing with `ctime_does_not_move_backwards_when_the_first_writer_applies_last` among them, and clippy and fmt clean under `-D warnings`.
2. Four wording corrections in `508698d0`: drop the vDSO and always-inlined claims, fix the `old`-arm contradiction and its digests, repoint the untouched-clock-site citations to 172, 391 and 440, and tighten the compile-error sentence to say it holds for a reading routed through `clock_read`.
3. The `bench/out` budget decision, which is still over cap and still blocks every lane that wants to execute.
4. Whoever merges must not paste `e9dc106`'s subject into the merge message.

Nothing was fixed, merged, marked ready or closed.
PR #139 stays open, issue #42 stays open, and no new task was created.