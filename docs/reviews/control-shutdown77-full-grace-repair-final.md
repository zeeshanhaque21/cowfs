# PR #79 head `da9870fd`: the repair is right, and CI is red on the repair's own new test

Reviewer: independent critic, treehouse lease 4, original native build train.
Head under review: `da9870fd6ee43eb33eaa39996f721e7595c2484a`.
Previous head I BLOCKed: `0f4cca97f3499e7aeaca02e4ccb59fb9607210e8`.
Base: `46b0f269d5bef4a2c204c25f5b3015da601d3beb`, verified an ancestor of the head.
CI run read once: `37253782129`, `head_sha` `da9870fd...`, `completed` / **failure**.
One read-only API call plus one log read. No dispatch, no rerun, no polling, no runner changes.

## Verdict

**BLOCK**, and it is a different block than last round.

The source repair is correct and I can prove it three ways. The blocker is that the repair's own
new committed regression test is failing on hosted CI, on both Linux and macOS, in two different
ways, and one of those two ways means the test does not park its fixture on Linux at all.

Last round I BLOCKed a contract change. This round the contract change is undone and verified
undone. What is left is a test that is red where it counts and fragile where it passes.

## What the repair does, confirmed

`crates/cowfs-ctl/src/server.rs`, one line at 289, with the surrounding comment rewritten:

```rust
-            let release_start = grace_end - opts.drain_deadline / 2;
+            let release_start = grace_end;
```

Total source delta for the whole head: 11 insertions, 4 deletions in `server.rs`, one of which is
the behaviour and the rest are comments. `grace_end` at line 278 is still the single
`Instant::now() + opts.drain_deadline`, reused by all three waits. No deadline reset, no second
grace, no new clock. I read lines 270 to 372 looking for exactly that and there is none.

Budget clock, every instant in the branch: `grace_end` computed once at 278; `release_start` is now
an alias for it at 289; the two `left(Instant::now(), release_start)` calls at 321 and 328 read the
same end; the join loop at 351 and the release loop at 366 both test `Instant::now() < grace_end`.

### Old FAIL / new PASS, both mutation gates, published-head tests

Both gates bind the **published head test blobs**, extracted with `git archive` of the exact head SHA,
with only `server.rs` swapped. I did not copy tests across heads.

Test file byte-identical in both scratch trees and in the worktree: `progress_shutdown.rs`
sha256 `0d9d8fd2d6d48c5b...`. Server sources byte-verified against `git show` before use:

| source | sha256 | identity |
|---|---|---|
| `da9870fd` published head | `5eb3f6bcd092ec2a...` | matches `git show da9870fd:...`, matches worktree |
| `0f4cca9` split head-1 | `25f41e561e2e7750d...` | matches `git show 0f4cca9:...` |
| `2de8697` three graces | `0556aaf817ca49c84...` | matches `git show 2de8697:...` |

Gate 1, the budget regression. `2de8697` source plus head tests:

```
shutdown_budget_is_the_deadline_plus_one_grace_with_a_parked_writer_and_a_cpu_handler ... FAILED
wait() took 1.31470775s, past the deadline plus one grace (1.05s)
```

Gate 2, the contract regression this head exists to fix. `0f4cca9` source plus head tests:

```
PROGRESS77 late 200/250@340 rep=0 resume_ms=340 elapsed_ms=452 whole_terminal=false peer_closed=true
200/250@340 rep 0: a client that resumed 340 ms inside a 450ms grace got no whole terminal frame
```

Head source plus head tests: that test passes, 20 of 20.

I also ran the new late-frame test against `2de8697` source: it **passes**, which is correct. The
three-grace code also delivered the full window. That test distinguishes the split head from both
the old three-grace code and the repair, which is what a good regression should do.

### Probe, three sources, one binary each, linkage proven first

Same probe source compiled against each of the three server versions. Before running anything I
proved which `server.rs` each binary actually contains, by inspecting the binary's own path strings.
This is the check I got wrong last round, when a regenerated manifest silently linked the new source
into the "old" probe and made the two look identical.

| probe binary | contains |
|---|---|
| `ctl77probe` | `crates/cowfs-ctl/src/server.rs` (head) |
| `probeold3g` | `old3g/src/server.rs` |
| `probesplit` | `split/src/server.rs` |

Mixed parked-plus-CPU, deadline 300 / drain 500, budget 800:

| shape | `2de8697` three graces | `0f4cca9` split | `da9870fd` head |
|---|---|---|---|
| 1 parked + 1 CPU | 1311 ms | 807 ms | 807 ms |
| 6 parked + 1 CPU | 1316 ms | 800 ms | 813 ms |
| 1 parked only (control) | 890 ms | 695 ms | 811 ms |
| 1 CPU only (control) | 803 ms | 808 ms | 805 ms |

One absolute grace, confirmed on the repair. Excess over budget is 5 to 13 ms. Peers see EOF after
the return in every case on all three sources.

Full delivery window, five reps per point, three sources. `a4`'s own geometry, window 450 ms, the
resume point that the split head failed:

| resume | `2de8697` | `0f4cca9` split | `da9870fd` head |
|---|---|---|---|
| 320 ms | whole 5/5 | whole 5/5 | whole 5/5 |
| 340 ms | whole 5/5 | **NoFrame 5/5** | **whole 5/5** |
| 400 ms | whole 5/5 | **NoFrame 5/5** | **whole 5/5** |
| 440 ms | whole 5/5 | **NoFrame 5/5** | **whole 5/5** |

Finite parked handler, deadline 300 / drain 500, window 800 ms: head whole at 250, 400, 500 and
600 ms, 5/5 each; split `Partial` from 400 ms onward, 5/5 each. At drain 1000, window 1300 ms, head
whole at 700, 800, 900 and 1000 ms, 5/5 each, and split `Partial` at every point from 700 ms.

Straddling the bound on the head, resume at 400, 415, 425, 435, 442, 446 and 449 ms inside a 450 ms
window: whole frame 7 of 7. The head really does deliver to the last millisecond of the window, not
merely to the middle of it.

Socket closed at the return, measured the instant `wait()` returns with a non-blocking read that
drains queued bytes first, so buffered data cannot be mistaken for an open socket:

| shape | `2de8697` | `da9870fd` head |
|---|---|---|
| mixed, drain 250 / 500 / 1000, 4 reps | EOF 20/20 | EOF 20/20 |
| parked only, drain 250 / 500 / 1000, deadlines 300 and 600 | EOF 16/16, handler already dropped | EOF 16/16, handler already dropped |

So the close-at-return property holds on the repair without any reserve, which is what the builder
now claims and what I could not make the old code violate last round either. The withdrawn claim is
correctly withdrawn.

## The blocker: CI is red, in the new test, two ways

Run `37253782129`, head `da9870fd`, conclusion **failure**. `check (macos-latest)` failure,
`check (ubuntu-latest)` failure, `linux-fuse` success. Every job's `head_sha` matches the head, so
this is the head and not a stale run. Both failures are the same step, `Run cargo test --workspace`,
and both are the same test, the one this PR adds.

**macOS, the frame did not land whole.** Log, verbatim:

```
PROGRESS77 late 200/250@340 rep=0 resume_ms=340 elapsed_ms=466 whole_terminal=true peer_closed=true
PROGRESS77 late 200/250@340 rep=1 resume_ms=340 elapsed_ms=467 whole_terminal=true peer_closed=true
PROGRESS77 late 200/250@340 rep=2 resume_ms=340 elapsed_ms=514 whole_terminal=true peer_closed=true
PROGRESS77 late 200/250@340 rep=3 resume_ms=340 elapsed_ms=465 whole_terminal=true peer_closed=true
PROGRESS77 late 200/250@340 rep=4 resume_ms=340 elapsed_ms=453 whole_terminal=true peer_closed=true
PROGRESS77 late 200/250@400 rep=0 resume_ms=400 elapsed_ms=463 whole_terminal=true peer_closed=true
PROGRESS77 late 200/250@400 rep=1 resume_ms=400 elapsed_ms=547 whole_terminal=false peer_closed=true
200/250@400 rep 1: a client that resumed 400 ms inside a 450ms grace got no whole terminal frame
test result: FAILED. 12 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out
```

**Linux, the fixture never parked.** Log, verbatim:

```
assertion `left == right` failed: still-window 0 advanced 37 -> 50; the progress write was not parked
```

That is `assert_parked` at `progress_shutdown.rs:96`, the fixture precondition, before any assertion
about frames. On the ubuntu runner the handler was still advancing its step counter 13 times per
150 ms window, so the progress write was never blocked on the socket.

### Why the Linux failure is a fixture defect, not a flake

The new test's handler, `FiniteFlood`, emits 400 progress events of 2000 bytes: 781 KiB total, with
`write_timeout` 6 s. The other tests in the same file park a **1 MiB** payload per event. 781 KiB is
below what a Linux AF_UNIX send buffer will absorb before the writer blocks. Linux autotunes
`wmem`, and the observed 37 to 50 progression says the backlog was being absorbed rather than
blocking. The test then asserts on a precondition that never held, so the assertion that was
supposed to prove the delivery window was never reached.

The comparison is direct: the sibling tests that use 1 MiB events park reliably, including on the
same runners, and this new fixture is the only one in the file using 2000-byte events.

### Why the macOS failure is a real margin problem, not noise

`elapsed_ms` in the log is the tell. Passing reps land at 453 to 467 ms. The failing rep shows 547 ms.
The window is 450 ms, so at 400 ms of resume the delivery has 50 ms to cover the write unwind, the
handler return and the terminal write. On my machine the head passes that point 8 of 8 and the
committed test passes 6 of 6 consecutive full runs, but on the hosted macOS runner one rep in six
missed. That is a margin that is too thin for a hosted runner, and asserting a hard `whole` with no
tolerance at a point 50 ms from the bound is asking for exactly this.

Note what this is not. It is not a regression to three graces: the budget gate is green and 5 to
13 ms over. It is not the split: the split fails 5 of 5 at that point, this fails 1 in 6. It is the
repair being correct in principle and under-asserted in margin on slower hardware.

## Tests, toolchain, CI, per binary rather than by sum

Local, head source, `cargo test -p cowfs-ctl`, exit 0 in 145 s. Per test binary, so overlap is
visible instead of added up:

| binary | passed |
|---|---|
| `unittests src/lib.rs` | 10 |
| `admission.rs` | 15 |
| `flood.rs` | 1 |
| `progress_shutdown.rs` | 13 |
| `regress.rs` | 19 |
| `server.rs` | 31 |
| `wire.rs` | 20 |
| doc-tests | 0 |
| **total** | **109** |

109 matches the PR body. These are seven distinct binaries, so the figure is a sum of disjoint sets,
not the 20 or 15 the body also quotes. The body's "half-close 20 of 20" and "a5 20 of 20" are
per-test repetition counts inside `regress.rs` and `admission.rs` respectively, already inside those
totals. Counting them again would double-count. `a4` 15 of 15 admission: confirmed locally, three
runs, `ending=Response` within grace and `ending=NoFrame` past it, `wait_ms` 454 to 461.

Child-exit frames: not re-run in full this round, it takes 72 s per run and is unchanged code.
The PR body claims 100 of 100. I am not restating that as my own measurement.

Toolchain, exit status read from the command itself, not a pipeline:

- `cargo fmt -p cowfs-ctl -- --check` → rc 0.
- `cargo clippy -p cowfs-ctl --all-targets -- -D warnings` → rc 0.

CI: `37253782129` failure, described above. The previously cited green run `37244238927` was on the
**old** head `0f4cca9` and says nothing about this one. That distinction is the whole reason this
review is a BLOCK.

## The two changed assertions

Authorized scope was exactly the two `handler_alive` assertions. Confirmed exactly two removals in
the whole test diff, both of them that:

```
-        assert!(
-            !handler_alive_at_return,
-            "{label}: the server still owns the abandoned handler when wait() returns"
-        );
-    assert!(!alive, "the parked handler was still alive at the return");
```

What replaced them, in `abandoned_blocked_connection_is_released_before_wait_returns` and
`shutdown_budget_with_only_a_parked_writer`, is a bounded 5 s poll for the detached handler to end,
asserting it does end, with the message "the detached handler never finished; the test would leak a
thread per case". `handler_alive_at_return` is still computed and printed in both. That is a
test-hygiene assertion, not a contract promise, and the reasoning is stated in the code.

I checked this is not a way of making a broken server look green. The user-visible requirements are
still asserted and I verified each independently:

- whole terminal frame inside the full window: yes, the new test plus `a4`, and my own probe at
  8/8 including the last millisecond.
- connection closed before `wait()` returns: yes, `peer_closed` in both tests, and my at-return probe
  at 20/20 and 16/16.
- `wait()` inside the budget: yes, the mixed-budget assertion, and my probe 5 to 13 ms over an 800 ms
  budget against 511 ms over for the old three-grace source.

`docs/v1-control-api.md:403-407` says abandoned handlers "get `shutting_down`, their connections are
closed and the server returns" and "the handler threads are detached and die with the process".
Requiring the handler to be unwound at the return is stricter than that, so the change is a
relaxation toward the documented contract, not a weakening of a user guarantee. Approved.

### Backend lifetime, and what that check does and does not prove

The new assertion in the mixed test samples `cpu_ticks` twice after the return and requires
`cpu_after > cpu_before`, to show the detached worker keeps running and its borrowed state stays
valid. I measured `cpu_ticks 10775 -> 10823 while detached after return` locally; the PR body
records `9142 -> 9177`. Same shape, both advancing, so the property holds on this source.

Scoping that claim honestly, as the coordinator asked. This is one public `Arc<AtomicU64>` in a test
fixture. It demonstrates that a detached handler outliving `wait()` still runs and still touches
state the server also owns. It does **not** demonstrate the absence of a use-after-borrow or a
lifetime race anywhere in the real handler path: the counter is an independent atomic, so it would
keep advancing even if some other borrow were invalidated, and Rust's ownership rules, not this
assertion, are what make that safe. The check is worth having. It is not a soundness proof and the
PR body does not claim it is one.

No universal daemon-shutdown figure is claimed anywhere in this head. The 120 s `GC_STOP_PATIENCE` in
`crates/cowfs-daemon/src/backend.rs:180` belongs to the daemon's own close path, which I did not
execute. I did not run a real daemon against a live Core, so I make no claim about that path.

## Docs: two claims correctly withdrawn, `a4` correctly restored

`docs/control-progress-shutdown.md` diff removes the split paragraph, including the
`connection_closed_at_return=false` sentence, and adds an explicit withdrawal: "did not happen at the
return. Measured across mixed, parked-only, three grace values and two deadlines, read EOF was
observed at the return in every shape on the unsplit source." That matches what I measured
independently, on the old source, at 20/20 and 16/16.

`a4` is back on the 250 ms default grace, with a comment saying the 500 ms widening "silently
removed the guarantee for clients resuming between 325 ms and 450 ms". That is the correct
characterisation and my table above is the evidence for it. The prose describing the geometry, at
`admission.rs:384-397` and the new comment at 403-410, now matches the code: 200 ms deadline, 250 ms
grace, closes at 450 ms, resume at 400 ms.

The "operator full-window finite-frame schedule" claim is scoped correctly. The docs say a client
unreadable past the bound may see a truncated frame then EOF, and that handlers may detach. That
matches the beyond-grace test, `ending=NoFrame`, and the split geometry's own `Partial` outcomes
past the bound. No claim that every reader inside the window is served unconditionally forever, and
none that any peer past the bound is.

One number in the docs is worth a second look, not a defect. The new test cases are labelled
"200/1000@900" and "200/1000@950" and the PR body calls them "90 percent" and "95 percent". Against
a 1000 ms grace, 900 ms and 950 ms are 90 and 95 percent of the grace but only 70 and 75 percent of
the 1200 ms total window from shutdown. The coordinator flagged exactly this arithmetic. The label
is defensible since the resume is measured after shutdown and the grace is what is being divided,
but "90 percent" next to a 1200 ms window invites the wrong reading. Worth one word of clarification
in the case labels so a maintainer does not later think the test covers 90 percent of the window.

No `/2` or `/3` fraction rationale survives anywhere in the head. The only mention of the old split is
the historical explanation of what was removed and why.

## Minimal fix

Two changes, both in the new test, no source change. The source repair stands as it is.

**1. Make the fixture park on Linux.** `FiniteFlood` at `progress_shutdown.rs:1175` uses
`"x".repeat(2000)` with `steps: 400`. Every other parking fixture in this file uses 1 MiB per event.
Raise the per-event payload to `1 << 20` and lower `steps` so the total still exceeds any plausible
socket buffer with room to spare, for example `steps: 8` of 1 MiB is 8 MiB. Keep
`assert_parked` exactly as it is. Do not weaken `assert_parked` to accommodate the smaller fixture;
that assertion is the only thing standing between this test and a vacuous pass.

**2. Give the two late cases margin, or move them off the bound.** The failing case resumes 400 ms
into a 450 ms window, leaving 50 ms for unwind plus handler return plus terminal write, and asserts
a hard `whole` with no tolerance. Either resume earlier in that geometry, for example 340 ms is
already covered by the first case, or widen that case's grace so the resume sits further from the
bound, or accept a documented tolerance. What must not happen is leaving a hard assertion 50 ms from
the edge on a hosted runner, because it will keep failing intermittently and it will be blamed on
something else next time.

Verify by re-running the whole `progress_shutdown` binary at least five times locally and confirming
`PROGRESS77 late` shows `whole_terminal=true` on every case, then let CI on the new head decide. I am
not asking for a rerun of the failing run; a new push supersedes it.

## Remaining limits of this review

- The hosted macOS margin failure is characterised from the log, not reproduced locally. My box
  passes that point 8 of 8 and the committed test 6 of 6. I could not make it fail here, so I report
  the margin analysis and the log, not a reproduction.
- The Linux parked failure is diagnosed from the log line and the fixture arithmetic. I have no
  Linux runner in this review, so "781 KiB is absorbed by a Linux send buffer" is inference from
  37 to 50 plus the socket buffer defaults, not a measurement. The fix in item 1 is robust to that
  inference being somewhat off, because it makes the fixture an order of magnitude larger.
- No real daemon against a live Core. Daemon and Core shutdown ordering was read only.
- Spawn-failure seam: read, not independently reproduced, no production hook added. It calls
  `best_effort_abandon(left(now, release_start))` then `kill()`, so with `release_start == grace_end`
  it inherits the single budget and is bounded. Existing narrow tests unchanged.
- `terminal_frame_survives_process_exit_after_wait` not re-run in full; unchanged code, and the PR
  body's 100 of 100 is the builder's measurement, not mine.
- Two scratch-archive runs in my previous round failed on a missing example binary. Same class of
  artefact, recorded there. Not repeated this round: the example was built before the archived runs
  here.

## State and provenance

Leased worktree `4`, `.treehouse-build-train/.treehouse/cowfs-7c1bf8/4/cowfs`, fast-forwarded with
`git merge --ff-only` to `da9870fd`. No reset, no stash, no source or test edits, no commit, no push,
no merge, no lease return, no CI dispatch. Shared PID 15263, the shared daemon store, mounts,
sockets, runners and devices untouched, no signals, no process groups. All leases protected, no path
overlap with the active #96, #98, #100, #102, #104, #106, #111 and #113 work.

Probe sources, control crates, mutation archives and runtime scratch trees are under the ignored
`bench/out/control77-full-grace-critic/**`, 552 KiB remaining after build trees were removed. Probe
binaries deleted after use, zero stray processes, `$TMPDIR` clean of fixture directories.

The seven earlier reports in this lease are preserved untouched:
`gc-space-accounting-final.md`, `gc-space-accounting-repair-final.md`,
`gc-space-accounting-wire-final.md`, `gc-post-unlink-accounting.md`, `gc-space-unlink-final.md`,
`control-shutdown77-bounded-final.md`, `control-shutdown77-budget-ordering.md`. My previous report,
`control-shutdown77-one-grace-final.md`, is in the primary checkout and unmodified.

On issue #77 and the PR: the PR is a draft, `state open`, and this review does not merge, close or
dispatch anything. Closing #77 is a coordinator decision after merge, not a review action.