# PR 139 permanent-regression delta: final independent review

Reviewed head: `cd1d5afa1f6cab57e479f911e1e202ce29fa7e49`, branch `fix/cache-write-clock-42`.
Delta reviewed: `ad6dca8b7b5bd331e8e553e7874c0852458a1c15` to `cd1d5afa1f6cab57e479f911e1e202ce29fa7e49`.
Current `main` at review time: `3dffcdea3d6ff0245e6cb622001b8dcd6b7b4a58`, which contains #136 and the #138 hole-flag work.
Author: READY6. Reviewer: READY1, doc-only, disjoint from the author's files.

**Verdict: the fix passes and ships sound; the permanent test is a real regression guard, and it has one
precise coverage gap that the author should know about before merge.**
Not a blocker on the fix. Not closed by this review either.

This review edited no source, no test and no manifest, made no commit, pushed nothing, merged nothing,
and filed no issue.

## Digests, verified before anything was trusted

| document | sha256 | verified where |
| --- | --- | --- |
| `docs/verification/evidence/meta42-cache-write-clock-permanent-regression.md` | `b5e1f19cf757eb87634473b033e0307b295ccad6435b8eaba78ea1272b5683fb` | canonical PRIMARY copy, and the blob committed at `cd1d5afa` |
| `docs/verification/evidence/meta42-cache-write-clock.md` | `a8bc5337cbf5bb9ac28c1d2801377cce552681432fcd2b5b18fad35bfad42451` | unchanged from my prior review |
| `docs/reviews/pr139-meta42-cache-write-clock-final.md` | `72d37cc4d209ca2c3bda04e969e7889d16b846f7dc622c18b0ffb5e8ca5c6fa7` | my prior report, now committed on the branch byte-identically |

The committed documents are the canonical documents, not local variants.
The brief warned that a previous digest in this lane was wrong, so each was recomputed rather than trusted.

## The delta

Two commits, three files, and in the one production file **177 insertions and zero deletions**.

`crates/cowfs-core/src/io.rs` gains `#[cfg(test)] park_if_armed();` at the `op_write` boundary before
the node lock, and 175 lines at the end of the file: `ClockGate`, a `thread_local!` named `ARMED`, the
`park_if_armed` helper, and `mod clock_order_tests` with one test.
The two other files are the evidence document and my prior review mirrored onto the branch.

Every line of the reviewed clock fix is byte-identical and no base line was deleted, so the fix itself is
untouched by this delta.
`Cargo.toml`, `Cargo.lock` and `crates/cowfs-core/Cargo.toml` are byte-identical across the delta, so no
dependency and no feature was added.

## Independent old-fail, new-pass, and the mutant really is only the clock position

I built my own arms from a `git archive` of the reviewed head, with **no injected fixture**, because the
permanent test ships inside `src/io.rs` and therefore needs no test target and no manifest entry.
All three arms: **653 tracked files, 0 mismatched outside `io.rs`, 0 extra files.**

Arms, in one 600-second foreground `mac-heavy.lock` batch, one sample each:

| arm | clock placement | exit | result |
| --- | --- | ---: | --- |
| `new` | shipped: under the node lock | **0** | 1 passed |
| `old` | above `park_if_armed()`, i.e. the pre-fix placement | **101** | 1 failed |
| `gap` | **between `park_if_armed();` and the lock** | **0** | 1 passed, and it should not have |

Old arm, verbatim from my log:

```
panicked at crates/cowfs-core/src/io.rs:624:9:
the cached ctime moved backwards: Timestamp { secs: 1791244280, nanos: 47102000 }
  is earlier than the Timestamp { secs: 1791244280, nanos: 47109000 } a client had already observed
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 29 filtered out
```

7,000 ns of regression, on a value the test had already read back through `getattr`.
The author's record reports 3,000 ns on the same arm.
Both are non-zero and both are the same defect; the magnitude depends on how long the writer waits and
neither number is a rate.

### The mutant is byte-for-byte the author's own

My `old` arm's `io.rs` sha256 is `23cea07a03ef663b61cbf642140c66beeb8f1d4b0ef5e2fefb1af7b3a8ac732c`,
which is **exactly** the author's `archives-old` value in his own table.
So the arm that fails is the author's arm, reproduced independently, not a variant of my own invention.
The shipped `new` arm's `io.rs` sha256 is `0351206c229225600cfe071f1c4d3df9fe4ba0e4bf7fcacdfdc449ce90af67a7`,
also exactly the author's `0351206c…`.

The gate, its position and the test module are byte-identical across the arms: my builder refuses to
write unless the counts of `park_if_armed`, `mod clock_order_tests` and the test name are unchanged, so
no arm can pass by moving the rendezvous or by weakening an assertion.

### Executable identity, with full values

The author's binary hashes are recorded truncated with an ellipsis, so they cannot be compared; mine are
full. All three arms produce the **same file name** and three **different** contents, which is the arm
binding:

| arm | binary name | sha256 |
| --- | --- | --- |
| `new` | `cowfs_core-04b1a37622c17b3e` | `6ed51ce56c14f42bc6928a6c4f09fa0972859e580d166a34544155c43f740a58` |
| `old` | `cowfs_core-04b1a37622c17b3e` | `1a3ab66b1d4ac298e7c16c8b009234f27942e4d9e7dc5300e0a067d48d34fc55` |
| `gap` | `cowfs_core-04b1a37622c17b3e` | `806e55024db7a9f5114c62e95c72ec2e857e65ad3a949531e3e6671a6f69d755` |

Each arm had its own `CARGO_TARGET_DIR` and its own `TMPDIR`; none was reused.

## Finding: the permanent test does not pin the whole property

This is the one substantive result of this review, and it is a real gap rather than a nitpick.

The `gap` arm places `let now = Timestamp::now();` between `park_if_armed();` and the opening of the
lock block.
The reading is therefore **still taken before the node write lock is acquired**, so the production
defect survives completely: a writer can read its clock, another writer can read a later clock and
apply first, and the first writer then installs the older value.

**The permanent test passes on that arm.**
It finished in 0.20 s, which means `gate.parked()` did fire and the writer really did park, so this is
not a "the gate never engaged" artifact.

The reason is the ordering of the gate relative to the reading.
The gate sits before the reading, so a parked writer has not read its clock yet; when it is released it
reads a **later** clock value and there is nothing to regress.
The test can only observe the defect when the reading precedes the park, which is precisely the author's
`old` arm.

So the test pins "the reading did not move above `park_if_armed()`", which is a proxy for "the reading is
under the lock" but is not equivalent to it.
A one-line upward slide that stops just below the park, which a merge conflict or a careless edit could
produce, would pass CI while the bug is fully back.

### How narrow the gap is, stated fairly

- It **does** catch the exact historical revert, because that revert puts the reading above the park.
- It **does** catch any further upward slide past the park.
- It **does not** catch a slide into the space between the park and the lock.
- It is not a false-assertion problem, not a self-fulfilling gate, and not a bypass of the test by
  disabling anything: the gate, the park point and the test are byte-identical in all three arms.

### What would close it, as a proposal and not an implementation

The behavioural gate cannot see this placement, so closing it needs a second, independent mechanism
rather than a cleverer gate.
The cheapest is a `#[cfg(test)]` counter that records whether the `op_write` clock reading happened while
a node write guard was alive, asserted true in the same test.
That adds a few `cfg(test)` lines to the file already owned and no public API, no dependency and no
feature, following the `set_gate_fault` pattern already in this crate.

I did not write it.
This review is doc-only, and a second mechanism inside a hot write path is a design decision for the
author and coordinator, not something to slip into a review.

## Gate soundness: what I checked and what holds

- **Thread-scoped, not global.** `ClockGate::arm` is called on the writer's own thread and stores the
  `Arc` in a `thread_local!`. A writer that never armed finds `None` and returns immediately, so the only
  writer that can park is the one that asked to. Nothing mutable and global is introduced.
- **Parallel isolation.** Two tests running in parallel each arm their own gate on their own writer
  thread; the `thread_local` prevents cross-talk and `taken` is per-`Arc`, so no second writer on the same
  gate queues behind the first. The 30-test `--lib` run below is the empirical check that the gate does
  not disturb its 29 neighbours.
- **No lock held while parked.** The park is before `node.st.wr()`, so a parked writer blocks only itself
  and the second writer proceeds. No deadlock.
- **Bounded waits, failing rather than hanging.** Both directions assert against a 60-second deadline on
  every spin: the parked writer panics with "the parked op_write writer was never released", and the test
  panics with "no writer reached the op_write boundary, so this run proves nothing". Neither is a hang.
- **Unwind and cleanup.** If the deadline fires the writer panics and `ARMED` is never cleared, but the
  slot is thread-local and dies with the thread, so nothing leaks into another test; the panic propagates
  through the join and fails the test loudly.
- **No `RefCell` re-entrancy.** `ARMED.with(|c| c.borrow().clone())` ends its borrow when the closure
  returns, and the later `borrow_mut()` is a separate, non-overlapping borrow.
- **No production impact.** Verified two ways, because symbol-name absence alone is weak evidence.

### Production build carries none of it

Source-level first, which is the primary evidence: walking all contiguous attribute lines above every
gate-bearing item, `ClockGate`, its `impl`, `thread_local! ARMED`, `park_if_armed` and
`mod clock_order_tests` are each governed by `#[cfg(test)]`, and every reference to those names sits
inside one of those gated items.
My first parser read only one attribute line back and reported `struct ClockGate` as ungated; that was
my parser stopping at `#[derive(Default)]`, not a gap in the code, and the corrected walk clears it.

Empirical corroboration with defined symbols, not embedded strings:

| artifact | defined symbols matching the gate |
| --- | --- |
| `libcowfs_core.rlib`, production `cargo build` | **0**, out of 6787 defined symbols |
| `cowfs_core-04b1a37622c17b3e`, test binary | `ClockGate` 35, `park_if_armed` 7, `clock_order_tests` 22 |

`strings` on the production rlib does report one hit each, and it is worthless as evidence: the matches
are bare `ClockGate` and `park_if_armed` with no path, i.e. debug and panic-location text, and `strings`
on the *test* binary reports 0 for `ClockGate` while `nm` reports 35 defined symbols.
This is exactly why the source-level `cfg` walk is the primary proof and symbol names are only
corroboration.
The normal write path takes zero gate atomics and zero extra branches.

## Scoped checks, all on the shipped `new` arm

| check | result | exit |
| --- | --- | ---: |
| `cargo test --locked -p cowfs-core --lib`, whole binary | **30 passed, 0 failed**, 0 ignored | 0 |
| `cargo test --locked -p cowfs-core --test operation_time` | 4 passed, 0 failed | 0 |
| `cargo test --locked -p cowfs-core --test locks` | 2 passed, 0 failed | 0 |
| `cargo test --locked -p cowfs-core --test caches` | 2 passed, 0 failed | 0 |
| `cargo fmt --all -- --check` | clean | 0 |
| `cargo clippy --locked -p cowfs-core --all-targets -- -D warnings` | clean, 0 `error`/`warning` lines | 0 |
| `cargo build --locked -p cowfs-core`, production | exit 0, no gate symbols | 0 |

The whole `--lib` binary runs, not only the new module, so the gate is exercised alongside the other 29
in-crate unit tests; that is the check that a thread-scoped gate does not disturb its neighbours.
`locks` matters most, because reading a clock while holding the node write lock is only safe if the lock
order is unchanged.
No workspace run, no 91-suite matrix, no performance, timing, soak or crash measurement, and no
power-loss claim.

## The author's disclosed compile failures

His record discloses two of his own and states that no `PASS` is carried from the failed attempts:
`E0433` on a `clock_gate` module that did not exist, then `E0599` and `E0308` from writing `arm` as an
associated function and using `a.join()` as the ctime.
I verified those errors are disclosed rather than hidden, and I did not carry any result from them.
My own arms compiled clean on the first build, and the two early failures in this lane were my own probe
API mistakes, both fixed before any result was recorded.

## Claims the change does not make

- **Not global monotonicity.** `Timestamp::now()` is the host wall clock, and the host wall clock can
  step backwards. This orders the reading against the node lock so a writer's `ctime` is its own
  application point. No claim of absolute or global monotonicity is made or implied.
- **The clock is real.** No clock is mocked, injected, replaced or synthesised in any arm. Both readings
  come from the host clock, microseconds apart, and the ordering is imposed by the rendezvous.
- **No rate.** One forced interleaving per arm shows the boundary could produce the inversion, not how
  often it is hit.
- **Scope is this interleaving on this host clock.**
- **`io.rs:168`, `io.rs:436` and `ns.rs:176` are hypotheses only**, untouched by this change. They have
  the same read-then-lock shape; none was reproduced and none is fixed here.
- The historical `7 us` and `9 us` gaps stay unreproduced.

## Integration with the current `main`, read-only

`main` is now `3dffcdea3d6ff0245e6cb622001b8dcd6b7b4a58`, not the `b486d454` of my prior review, because
the #138 hole-flag work merged in between.

`git merge-tree --write-tree origin/main cd1d5afa`: merge base `e7ee215`, tree
`1842f5a7226b8c2749799e666442699128b53873`, **0 conflict lines, exit 0.**
That is a read-only computation, not a merge.

I checked the overlap risk the brief flagged rather than trusting the clean exit alone: `main` did not
touch `crates/cowfs-core/src/io.rs` at all since the merge base, so the new hole-flag `ChunkRef` API,
the store walk and the doc comments cannot collide with this delta.
`main` did change `file.rs`, `lib.rs`, `tests/caches.rs`, `tests/critic2b.rs` and `tests/hole_walk.rs`;
`tests/caches.rs` is shared with a check I ran, and it passes against the pre-merge `main` content, so a
post-merge run is still worth doing by whoever merges.
No integration or compile problem is reported for the owner, and none was found.

## Closing-reference audit

`closingIssuesReferences` is empty, and as in my prior review I did not rely on that field.

Every commit the branch would newly bring against current `main` was audited for a closing verb bound to
an issue reference: `e9dc106`, `8670d36`, `ad6dca8b`, `d2c5337` and `cd1d5afa` carry none.
`fix(core):` is a conventional-commit prefix, not a closing form.

The PR body contains one `Addresses`, in its first line: "Addresses the **cache-layer** `ctime`
obligation in issue #42". That is not a GitHub closing keyword, it binds no verb to an issue reference in
closing form, and `closingIssuesReferences` is 0, so nothing will close on merge.
It is recorded because the brief asked for the audit to include negated forms and not to trust the empty
field alone.

The dependency commit `57232166`, my own from an earlier task, still contains "nothing here closes #42",
which is what auto-closed #42 on the #136 merge. It is already on `main`, already superseded by the
reopen, and history is not rewritten.
`#42` is `open` with `state_reason: reopened`, verified directly.
Whole #42 stays open; the rename, hole-flag, reservation and `Core` integration work are untouched here.

PR 139 is `draft=false`, `state=open`, head `cd1d5afa`.

## CI at the exact head

One snapshot of run `37390317724`, not polled, not rerun, not dispatched, no workflow or runner change:

```
linux-fuse             completed/success
check (ubuntu-latest)  in_progress
check (macos-latest)   in_progress
run status=in_progress
```

Two of three jobs are still running, so this is not a green claim and not a "no checks configured" claim.
All three must be read again once they land.
No claim is made about any other head, any other PR, or the repository's overall status.

## Budget

`bench/out` measured before any archive or build: 2.46 GiB of the 8 GiB cap, 278.0 GiB free against the
20 GiB floor.
Peak after four target directories and three arms: 4.27 GiB, leaving 3.73 GiB.
The cap was never exceeded.
No cleanup, deletion, moving or offloading was performed, and no cap waiver was taken; that is READY5's
approved scope, not this lane's.
No signal, no restart, no install, no `sudo`, no mount walk and no lease action.
My artifacts are confined to `bench/out/meta42-cache-write-clock-permanent-final-critic/`, which is
gitignored; the leased worktree is clean at `e7ee215` with no commits and nothing pushed.
Every prior #118, #120, #135, #136 and #139 report, log, archive and binary is preserved, and the
author's lane was read only.

## Tools

`no-mistakes` is **not initialized** in this repository, `.no-mistakes` and `.claude` are both absent, so
that pipeline was not run and no claim is made about it.
No browser step was taken; `chromium` is not installed, so any browser surface is **UNVERIFIED**.
`codebase-memory-mcp` graph tools were not used; the binding this review required was the extracted
archive compared per file with `git hash-object`.
MisakaNet was available only as a local stdio server and was not consulted; no failure-recall need arose
and no remote call was made.

## Evidence

Under `bench/out/meta42-cache-write-clock-permanent-final-critic/` in the assigned worktree, gitignored:

| file | what |
| --- | --- |
| `scripts/mutate-clock.py` | this review's own arm builder, refusing on any anchor count other than 1 and on any change to the gate, the park helper or the test name |
| `archives/head.tar.gz` | the source archive of `cd1d5afa`, sha256 `c23c12140a2859612cb8ef7b9cb31133b40b7122761448fd4cbc3ff06a9036db` |
| `archives/new/`, `archives/old/`, `archives/gap/` | the three arms |
| `logs/new-sample.log` | new arm, 1 passed |
| `logs/old-sample.log` | old arm, the regression and its nanoseconds, exit 101 |
| `logs/gap-sample.log` | gap arm, 1 passed, the finding |
| `logs/fmt.log`, `logs/clippy.log`, `logs/prod-build.log` | fmt, scoped clippy, production build |
| `logs/lane.log` | every lock acquisition with its UTC time and every arm exit |