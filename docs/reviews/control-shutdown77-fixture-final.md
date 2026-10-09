# PR #79 head `4b210de`: fixture repair PASS, third assertion approved, #121 still BLOCK

Reviewer: independent critic, treehouse lease 4, original native build train.
Head under review: `4b210deea33f177daf2fa91950d4a3a28dcbb923`.
Previous head I BLOCKed: `da9870fd6ee43eb33eaa39996f721e7595c2484a`.
Base: `46b0f269d5bef4a2c204c25f5b3015da601d3beb`, verified an ancestor of the head.
CI run read once: `37260751913`, `head_sha` `4b210de...`, `completed` / **success**, all three jobs
green. One read of the run, one read of the full log, no dispatch, no rerun, no polling.

## Verdict

Three separable questions, three answers.

1. **Fixture repair: PASS.** The 1 MiB-per-event, two-event fixture parks on both runners and passes
   75 of 75 across five consecutive runs of the committed test. Both mutation gates still fail on the
   old sources, binding the exact published head test blob.
2. **Third `handler_alive_at_return` assertion: approved, with a scoping condition.** Same class as the
   two already authorised, the reasoning is in the code, and every externally observable gate is
   retained. I did not approve it silently as a continuation; the explicit reasoning is below.
3. **Issue #121: still a MERGE BLOCK.** I could not reproduce the flake, and I could not falsify the
   builder's own hypothesis for it either. That is not a diagnosis. One more blocker, with a narrow
   spike recipe.

Merge waits on #3 only. #1 and #2 are shippable and CI is green on the head.

## Source carry: production unchanged

`crates/cowfs-ctl/src/server.rs` sha256 `5eb3f6bcd092ec2a9fd69a3898b06060f5bd4fc8296bb4b0608afc3aa4a71664`,
byte-identical to the head I reviewed last round and to `git show 4b210de:crates/cowfs-ctl/src/server.rs`.
Confirmed three ways: the worktree file, `git show` at this head, and the builder's own recorded hash
in `docs/verification/control-shutdown77-grace-repair.md:61`.

`release_start = grace_end` at line 289, unchanged. The whole `da9870fd..4b210de` delta is
269 insertions / 19 deletions across three files: the test file, `docs/control-progress-shutdown.md`,
and the new verification doc. No production behaviour moved, so nothing in my last round's budget,
delivery-window or at-return findings needs re-litigating. They carry.

`crates/cowfs-core` untouched. I did not edit source, tests or docs in the worktree.

## Fixture repair: PASS, with the mutation gates re-proven

### The committed test, five consecutive runs

One representative complete run first, then the batch, per the batching rule.

| run | whole frames | exit |
|---|---|---|
| 1 | 15/15 | 0 |
| 2 | 15/15 | 0 |
| 3 | 15/15 | 0 |
| 4 | 15/15 | 0 |
| 5 | 15/15 | 0 |
| 6 | 15/15 | 0 |

90 of 90 across six runs, five consecutive after the first. Per case, from the preserved logs:

| case | whole | elapsed range |
|---|---|---|
| `window450/resume340` | 30/30 | 450 to 462 ms |
| `window1200/resume900` | 30/30 | 1090 to 1100 ms |
| `window1200/resume950` | 30/30 | 1086 to 1111 ms |

### Mutation gates, exact published head test blob

Both scratch trees are `git archive` of `4b210de`, with only `server.rs` swapped. The test file is
byte-identical across both scratch trees and the worktree: `progress_shutdown.rs`
sha256 `d479840b78e6ea51eb06`.

Old split head `0f4cca9`, server sha256 `25f41e561e2e7750d...`, byte-verified against `git show`:

```
PROGRESS77 late window450/resume340 rep=0 resume_ms=340 elapsed_ms=451 whole_terminal=false
window450/resume340 rep 0: a client that resumed 340 ms inside a 450ms grace got no whole terminal frame
```

Old three-grace head `2de8697`, server sha256 `0556aaf817ca49c84...`, byte-verified:

```
PROGRESS77 budget mixed wait_ms=1314 ... budget_ms=1050
wait() took 1.314856625s, past the deadline plus one grace (1.05s)
```

Both gates fail, so the committed test still discriminates the repair from both predecessors. The
repair is not a fixture that passes everything.

### The parked precondition is honest, not tuned

`assert_parked` is unchanged from the version I reviewed, and the fixture now matches the pattern
every other parking fixture in the file uses: `message: Some("x".repeat(1 << 20))` at
`progress_shutdown.rs:1190`, with `steps: 2`.

Two events, not eight, and the commit message explains why, which I checked against my own
measurements rather than taking on trust. Eight events leaves up to 7 MiB for the client to drain
inside the room; I measured this independently and the effect is real. On the committed two-event
fixture, sweeping the resume point through each geometry, three reps per point:

| window | resume | room left | whole |
|---|---|---|---|
| 450 ms | 340 ms | 110 ms | 0/3 in my probe, 30/30 in the committed test |
| 450 ms | 400 ms | 50 ms | 0/3 |
| 1200 ms | 900 ms | 300 ms | 3/3 |
| 1200 ms | 950 ms | 250 ms | 2/3 |
| 1200 ms | 1050 ms | 150 ms | 0/3 |

The 110 ms and 250 ms room figures the coordinator asked me to verify are arithmetically correct:
window minus resume, both from the start of shutdown, so 450 - 340 = 110 and 1200 - 950 = 250. The
corrected arithmetic is also right: 900 ms is 90 percent of a 1000 ms grace and 75 percent of the
1200 ms window, and the case labels now carry `window1200/resume900` rather than a bare percentage,
which removes exactly the ambiguity I flagged last round.

The divergence at `window450/resume340` is between my probe and the committed test, on identical
geometry and identical fixture, and I did not close it. Both are true on this host: the committed
test reports `whole_terminal=true` 30 of 30, my probe reports `Partial` 3 of 3 at the same point. The
committed result is the one CI gates on and it is green on both runners. My probe's disagreement is
recorded as an open measurement question in the limits section, not resolved in the PR's favour by
assertion and not turned into a blocker, because the thing being asserted is measured green by the
committed test on the same host with the same source.

What I did establish about the mechanism, which is useful and points at client drain capacity rather
than server delivery. After the resume, the client must drain roughly one 1 MiB event plus framing
before the terminal frame can be parsed. My probe's measured drain rate on this loaded host is
between 5.0 and 6.8 MiB/s, so 1 MiB costs 150 to 200 ms of client-side time. Against 110 ms of room
that does not fit, which is consistent with my `Partial`. The committed test's client drains faster,
which is consistent with its `whole`. Both clients are correct; the committed case's margin depends on
client drain speed, and that is the same class of sensitivity as the #121 flake below. I flag it
rather than resolve it: the committed test is not currently red, but its tightest committed point has
thin margin that depends on the client.

### CI at this head

`37260751913`, all three jobs success, every job's `head_sha` matching the head: `linux-fuse`,
`check (macos-latest)`, `check (ubuntu-latest)`. The two hosted failures from `da9870fd` are gone:
no `still-window ... not parked` and no `whole_terminal=false` anywhere in the log. `progress_shutdown`
ran 13 passed on both `check` jobs. That is the direct answer to the Linux parking and macOS margin
questions from last round: both fixed by this fixture.

Local `cargo test -p cowfs-ctl`, exit 0 in 137 s, per disjoint binary so the count is a sum of
non-overlapping sets and not a repetition count:

| binary | passed |
|---|---|
| lib.rs unit | 10 |
| admission | 15 |
| flood | 1 |
| progress_shutdown | 13 |
| regress | 19 |
| server | 31 |
| wire | 20 |
| doc-tests | 0 |
| **total** | **109** |

Toolchain, exit status read from each command directly: `cargo fmt -p cowfs-ctl -- --check` rc 0,
`cargo clippy -p cowfs-ctl --all-targets -- -D warnings` rc 0.

## The third `handler_alive_at_return` assertion: approved

This went beyond the two I authorised, so I evaluated it separately rather than waving it through.

What changed, in `shutdown_budget_is_the_deadline_plus_one_grace_with_a_parked_writer_and_a_cpu_handler`:

```rust
-        !handler_alive_at_return,
-        "the parked handler was still alive when wait() returned"
+    let until = Instant::now() + Duration::from_secs(5);
+    while !dropped.load(Ordering::SeqCst) && Instant::now() < until {
+        std::thread::sleep(Duration::from_millis(5));
+    }
+    assert!(
+        dropped.load(Ordering::SeqCst),
+        "the detached handler never finished; the test would leak a thread per case"
+    );
```

Exactly one third removal of that assertion class in the whole diff, in the mixed test, in addition to
the two from the previous round. `handler_alive_at_return` is still computed and printed. The
replacement is a bounded 5 s poll that the detached handler does end, framed in the code as the test
loop's own thread budget rather than a promise to any caller, which is the correct framing: without
it the loop accumulates a live thread per case.

Against the documented contract. `docs/v1-control-api.md:403-407` says abandoned handlers "get
`shutting_down`, their connections are closed and the server returns" and "the handler threads are
detached and die with the process". Requiring the handler to be unwound at the return is strictly
stronger than both halves of that sentence. With `release_start = grace_end`, `kill` runs at
`grace_end` and the parked `write_all` unwinds on its own schedule afterwards, so gating the unwind
would assert a scheduler, not a contract. The builder's inline reasoning says exactly that and it is
correct.

Externally observable gates retained in that same test, all still asserted and all independently
verified by me this round:

| gate | assertion | my verification |
|---|---|---|
| budget | `elapsed < budget` | mixed 807 and 813 ms against an 800 ms budget, 5 to 13 ms over |
| parked connection closed at return | `parked_closed_at_return` | at-return EOF 20/20 mixed, 16/16 parked-only, drain-first non-blocking read |
| CPU handler's peer closed at return | `peer_closed(&cpu.stream)` | EOF at return, same probe |
| CPU handler still alive at return | `cpu_alive_at_return` | recorded, `true`, kept so the case stays non-vacuous |
| detached handler ends | bounded 5 s poll | passes every run |
| backend lifetime observable | `cpu_after > cpu_before` | `cpu_ticks 10775 -> 10823` detached |

Nothing user-visible was traded away. The whole-frame guarantee inside the full window and the
closed-connection-before-return guarantee are both still asserted, and both are what the PR is for.

**Recommendation: keep it as changed.** The condition I attach is that it must not become a
precedent for relaxing further internal assertions without a contract citation. Three is defensible
because all three cite the same two sentences in `v1-control-api.md`; a fourth that does not would be
a different thing, and `assert_parked` is explicitly not in scope: it is the precondition that stops
this suite going vacuous, and it stays.

On the backend-lifetime check specifically, scoping it as the coordinator asked. It samples one
public `Arc<AtomicU64>` in a test fixture twice after the return and requires the counter to keep
advancing. That demonstrates a detached handler outliving `wait()` still runs and still touches state
the server also owns. It does **not** demonstrate the absence of a use-after-borrow or a lifetime race
anywhere in the real handler path: an independent atomic would keep advancing even if some other
borrow were invalidated, and Rust's ownership rules, not this assertion, are what make that safe. It
is a useful observability check. It is not a soundness proof, and the PR body does not claim one.

## Issue #121: MERGE BLOCK, undiagnosed

First, the log identity. The coordinator asked me to inspect the original failing log first. It is
**not preserved**. `rg "terminal_present=false"` over the primary checkout and this worktree finds it
only in `progress/plan.json` and in the builder's prose in
`docs/verification/control-shutdown77-grace-repair.md:90`, which records
`terminal_present=false` at 332 ms. There is no raw captured run, no `PROGRESS77` line with its
surrounding context, no job log, and no indication of which machine or which suite topology
produced it. That is a gap: the one artifact that would make this diagnosable was not kept, and the
prose summary is not a substitute.

What I ran, all on the exact published head and the exact published test:

| shape | repetitions | result |
|---|---|---|
| isolated `an_unblocked_client_still_receives_a_terminal_frame_at_shutdown` | 8 | 8/8 pass, `terminal_present=true`, 300 to 314 ms |
| full `progress_shutdown` binary, suite topology | 3 | 3/3 pass, 13/13 each, `terminal_present=true` at 302, 313, 413 ms |
| standalone probe of the published shape: unbounded 1 MiB flood, client reading, deadline 300 / drain 1500 | 12 | 12/12 `terminal_present=true`, bytes 1048771, EOF 300 to 311 ms |
| same shape, pre-shutdown drain varied 0, 1, 2, 4, 8 reads to change the interleaving | 50 | 0 failures |

So: not reproduced, four ways, including a probe built specifically to give a failure enough
information to identify itself. Per the coordinator's instruction that isolated passes are not cause
proof, I am not treating that as resolution. Equally I am not manufacturing a cause.

The builder's hypothesis, from `control-shutdown77-grace-repair.md:93-95`, is that "the test's margin
depends on a client draining 1 MiB frames faster than the flood fills the socket, which is
load-sensitive". I could not confirm it and I found a reason to doubt the framing. The failing
observation was `elapsed_ms=332`. In that test the loop runs until it sees the terminal or 2 s elapse,
so a run that ends at 332 ms ended because it saw a terminal, not because it ran out of time. A
client that merely failed to drain fast enough would have run to the 2 s bound and reported
`elapsed_ms` near 2000 with `terminal_present=false`. **332 ms with the terminal absent is not the
signature of a slow drain.** It is the signature of the read loop exiting early, and the only two
early exits in that loop are `Ok(0)`, a bare EOF, and a completed read. That points at the connection
being closed with a truncated or missing frame while the loop was still reading, which is a different
hypothesis from the one recorded.

I traced the code that could do that and did not find a proven defect, so this stays a hypothesis with
a location, not a finding. Two candidates, both worth a spike and neither asserted:

1. `drain_and_close` at `server.rs:768-786` does `shutdown(Shutdown::Both)` after a short drain. Its
   own comment says a killed connection "has had its delivery grace", but the guard is
   `while !self.dead.load()`, so a connection killed by the *connection thread's* teardown at line
   591 rather than by the accept loop's abandon worker skips the drain entirely and closes both
   directions immediately. If the request worker is mid-`write_all` on the terminal frame at that
   moment, the peer can see EOF with the frame cut. `finish` guards against this with the `finishing`
   counter and the `inflight` remove, and `drain_and_close` does not consult either.
2. `abandon_inflight` is reachable from two threads at once: the accept loop's abandon worker at line
   307 and the connection thread at line 589 when `stopping`. `finish` dedups on
   `inflight.remove(&id)`, so exactly one writer wins and the loser returns without writing. That is
   correct for one frame, but it means the winning writer's frame and the connection thread's
   subsequent `kill()` are ordered only by that dedup, not by anything that waits for the write.

Neither is confirmed. Both are in the shutdown teardown path, both are consistent with an early EOF,
and neither is exercised by a test that asserts a frame survives a concurrent connection-thread
teardown.

Why this blocks rather than waits: the failing test is named for the exact guarantee this whole PR
exists to restore, a reading client getting its terminal frame at shutdown. A failure there is a
statement about shutdown delivery reliability. The production blob is unchanged across the heads where
it passed and where it failed, so the bug, if it is in the server, predates this PR and is not
introduced by it, and it is equally not fixed by it. Merging a delivery-reliability fix while a test
asserting that delivery is open is the wrong order. I would merge this the moment #121 has a bounded
diagnosis, and this PR is what makes that diagnosis worth doing.

## Narrow spike recipe for #121, for the builder

Nothing here needs a new production hook, and nothing needs a retry, an ignore, or a relaxed
assertion. The target is one reproducible interleaving.

1. Reproduce in the published test's own shape, no fixture change: unbounded 1 MiB flood, client that
   keeps reading, `shutdown_deadline` 300 ms, `drain_deadline` 1500 ms. Add to the existing read loop,
   which already parses complete lines, three records the test does not keep today: the byte count
   read, the `Ok(0)` versus `Err` that ended the loop, and whether the buffer held an unterminated
   tail when EOF landed. My probe already collects all three and shows the shape is diagnosable; a
   failure prints `Partial`, an EOF timestamp and a tail of `x` bytes instead of a bare
   `terminal_present=false`.
2. Then force candidate 1 directly, in a test-owned topology rather than by racing: a client that
   resumes reading, plus a server whose connection thread is torn down while the request worker is
   inside the terminal `write_all`. If a frame can be cut that way, assert the current behaviour first
   so the spike is falsifiable, then fix.
3. Only if candidate 1 is refuted, test candidate 2: drive `abandon_inflight` from the connection
   thread and the accept-loop worker against one connection with the dedup winner's write delayed, and
   observe whether `kill` can precede the frame.
4. Bound every wait in the spike to a few seconds with an error-aware exit, and record a negative
   control: the same topology with the shutdown deadline removed must always deliver.
5. Prove the fix as old-fails / new-passes on the same input, and only then remove the spike's
   instrumentation.

The success criterion is a committed regression that fails on `5eb3f6bc` and passes on the fix. Not a
green suite, not a lower flake rate.

## Remaining limits

- **My probe and the committed test disagree at `window450/resume340`** on identical geometry and
  fixture on this host: committed 30/30 whole, mine 3/3 `Partial`. I did not close it. The measured
  mechanism is client drain capacity, 5.0 to 6.8 MiB/s here, against 110 ms of room and roughly 1 MiB
  to drain, which fits my result and not the committed one. Both are green or red on their own terms;
  I am reporting the disagreement rather than resolving it in either direction. The committed test is
  not currently red and CI is green on both runners, so this is a margin observation, not a blocker.
- **#121 not reproduced and not diagnosed.** Four shapes, 73 repetitions, zero failures. The failing
  log is not preserved, so my characterisation of the 332 ms signature comes from the builder's prose
  summary plus my reading of the test's loop, not from the artifact.
- **No Linux runner.** The Linux parked failure and its fix are confirmed by hosted CI at this head
  and by reading the previous head's log, not by a local Linux run.
- **Spawn-failure seam:** read, not independently reproduced, no production hook. With
  `release_start == grace_end` it calls `best_effort_abandon(left(now, grace_end))` then `kill()`, so
  it inherits the single budget and is bounded.
- **No real daemon against a live Core.** Daemon and Core shutdown ordering read only. No universal
  daemon-shutdown figure is claimed anywhere in this head.
- **Two candidate races in teardown are hypotheses with source locations, not findings.** I could not
  construct a reproduction for either.

## State and provenance

Leased worktree `4`, `.treehouse-build-train/.treehouse/cowfs-7c1bf8/4/cowfs`, fast-forwarded with
`git merge --ff-only` to `4b210de`. No reset, no stash, no source or test edit, no checkout, no
commit, no push, no merge, no lease return, no CI dispatch or rerun, no runner or workflow change, no
install, no sudo, no sysctl. Shared PID 15263 on this Mac, the shared daemon store, mounts, sockets,
`987929D`, `899604`, the `g4` PID 1209860 and `g5` untouched, no signals sent, no process groups, no
`pkill`, no mount walk. All leases protected; no path overlap with the active #96, #98, #100, #102,
#104, #106, #111, #113 work, and `951045f96` plus the #111 integration server left alone. Primary
checkout at `main` `03bbec85`, `#77` and `#121` both open, references left as they are.

Artifacts: ignored `bench/out/control77-fixture-final-critic/**`, 140 KiB after build trees and
scratch archives were removed. Preserved there: `ps.diff`, `ctl-suite.txt`, `fixture-run1..6.log`,
`suite-run1..3.log`, and `probe/` with the spike probe source. Probe binaries deleted after use, zero
stray processes, `$TMPDIR` clean.

One process correction worth recording, because it nearly became a false result. A chunk-size
experiment failed to compile and two probe runs I had already read came from the stale binary. I
checked the build, saw `error[E0596]`, rebuilt, and re-ran before drawing any conclusion from those
two. The `Partial` results above are all from the fresh binary at
`target/debug/c77fix` mtime 21:39.

The seven earlier reports in this lease are preserved untouched, and my two previous reports are in
the primary checkout unmodified: `control-shutdown77-one-grace-final.md` and
`control-shutdown77-full-grace-repair-final.md`.
`docs/verification/control-shutdown77-grace-repair.md` is byte-identical in both trees,
sha256 `06c5736c1bd6fe72e2278e32e3bbe55f51458c51052e1848c249caa33d9b0a6f`.