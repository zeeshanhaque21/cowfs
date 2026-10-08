# PR136 clock observation: clarification of the 9-microsecond early durable ctime

Reviewed head, unchanged from the prior review: `cde59305fe05f4551468965cd648888eb98e9dbf`, branch `fix/deferred-operation-time-42`.
Prior report, immutable: `docs/reviews/pr136-meta42-operation-time-final.md`, sha256 `c4999d45af0ea0d78344ab311be378102ec0856931939bd4f96468f0965a8644`, verified unmodified before and after this task.
Scope: reconcile the prior report's `PASS` with the one early-durable observation it recorded, and nothing else.
This is issue #42's clock obligation only.
Requests 1, 3 and 4 are untouched. Whole #42 is not closed by this and must not be.

No checkout, branch change, source edit, reset, stash, commit, push, merge, lease action or issue edit was performed.
No production fix was written and none is proposed as landed.
No new issue was filed.

## Verdict

**The prior `PASS` stands, and the reason is now measured rather than assumed: the early durable `ctime` was not a replay failure.**
In both historical runs the durable value equalled the stable pre-flush public `ctime` exactly, and the assertion that failed compared it against the transient peak of a quarter of a million racing per-write samples.
That is a cache-layer observation, not a `flush`/replay observation.

Separately, and this is a real defect, **the cached `ctime` is not monotonic under concurrent writers**, and I demonstrated it deterministically rather than statistically.
A writer that reads the clock before taking the node write lock can apply after a writer that read a later clock, so `ctime` moves backwards after a client has already observed a later value, while the file's content linearizes by application order and so disagrees with `ctime` about which write happened last.
The durable value after flush and reopen is faithful to the final stable cached value.
That is the branch the brief anticipated: a cache-layer race, an existing #42 timestamp obligation for a minimal responsible-layer follow-up, and not a PR136 replay contract failure.

| claim | result |
| --- | --- |
| `durable == stable pre-flush ctime` in the two historical runs | **true, exact**, read from the raw logs |
| `durable >= peak of racing samples` in those runs | **false**, by 7 us and 9 us; that was the failing assertion |
| stable-epoch sample, exact equality after flush and reopen, content included | **5 of 5 attempts pass**, 977,507 samples, peak never exceeded stable |
| deterministic forcing of the older-clock-writer-after-newer-writer interleaving | **pass**, 1 run, cache `ctime` regressed 3 us, content linearized to the older-clock writer |
| durable value equals the final stable cached value in that forced case | **exact**, and the content is the older-clock writer's bytes |
| `crates/cowfs-core/src/io.rs` changed by PR136 | **no**, byte-identical between base and head |
| the shipped tree at `cde5930` changed by this task | **no** |
| production fix written | **none**, deliberately |

## The two historical raw logs, read first

Both are in the prior lane at `bench/out/meta42-operation-time-final-critic/logs/`, unmodified.
Each test binary is the same hash, `zz_critic_concurrent-7faccce26253a4d1`.

`r-head-6.log`, verbatim values:

```
PROBE concurrent writes=255388 max_seen=Timestamp { secs: 1791233532, nanos: 487109000 }
  last_reported=Timestamp { secs: 1791233532, nanos: 487102000 }
  durable=Timestamp { secs: 1791233532, nanos: 487102000 }
the durable ctime Timestamp { secs: 1791233532, nanos: 487102000 }
  is earlier than a ctime already reported Timestamp { secs: 1791233532, nanos: 487109000 }
```

`probe-conc-run2.log`, verbatim values:

```
PROBE concurrent writes=292257 max_seen=Timestamp { secs: 1791232858, nanos: 457181000 }
  last_reported=Timestamp { secs: 1791232858, nanos: 457172000 }
  durable=Timestamp { secs: 1791232858, nanos: 457172000 }
the durable ctime Timestamp { secs: 1791232858, nanos: 457172000 }
  is earlier than a ctime already reported Timestamp { secs: 1791232858, nanos: 457181000 }
```

Two facts are in those lines and they settle the question.

`durable == last_reported`, digit for digit, in both runs.
`last_reported` is read at line 53 of the probe, after `stop.store(true)`, after `handles` are joined and after `flusher` is joined, so it is a quiesced public value with no writer still between its clock reading and its node lock, and it is read before the final `c.flush()`.
`max_seen` is the maximum over ~255,000 samples, each taken by a writer thread immediately after its own `write` returned, so it is a peak across many racing epochs.
The gap is `max_seen - last_reported`, 7,000 ns and 9,000 ns.
The assertion that failed was `durable >= max_seen`.

So the comparison was between two different visible epochs: a durable value taken from the quiesced epoch against a peak taken from an epoch that had already ended.
The durable value did not differ from the public stable pre-flush `ctime`; it differed from a transient peak.

That is also why the second assertion, `assert_eq!(durable, last_reported)`, never fired.
It is after the first in the source, so the run aborted before reaching it, which is why the logs record the first failure and not the second.
Both runs passed my re-read of the durable value against `last_reported` in the twenty clean head runs the prior review recorded, and in the five attempts recorded below.

## One representative sample, epoch-matched

`probe/zz_critic_epoch.rs`, run against a fresh archive of the exact head, 5 attempts, no soak.

The shape is the prior probe's, with the epoch pinned:

1. two writer threads write one file and `getattr` it after each write, with a background flusher on a 2 ms interval;
2. after 400 ms, `stop` is set, then **both writer handles are joined and the flusher handle is joined**, so no writer remains between its clock reading and its node lock;
3. the stable pre-flush `ctime` **and the full 8-byte content** are read from the public `Vfs` at that quiescence point;
4. one explicit `c.flush()`, then every handle is dropped and the same directory is reopened by a brand new `Core`;
5. the reopened `ctime` and content are compared to the pre-flush values by **exact equality**.

| attempt | samples | peak minus stable | reopened minus stable | pre-flush bytes | reopened bytes | exit |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | 124,318 | 0 ns | 0 ns | `abcdefgh` | `abcdefgh` | 0 |
| 2 | 173,980 | 0 ns | 0 ns | `abcdefgh` | `abcdefgh` | 0 |
| 3 | 278,492 | 0 ns | 0 ns | `abcdefgh` | `abcdefgh` | 0 |
| 4 | 252,343 | 0 ns | 0 ns | `abcdefgh` | `abcdefgh` | 0 |
| 5 | 148,374 | 0 ns | 0 ns | `abcdefgh` | `abcdefgh` | 0 |

977,507 samples across five runs.
In every run the durable `ctime` after flush and reopen equalled the stable pre-flush `ctime` exactly, and the durable content equalled the pre-flush content exactly.
In every run the peak never exceeded the stable value, so the historical condition did not reproduce at this boundary.

Per the brief: a clean sample does not resolve the historical failure, so it stays **unreproduced**, and the cause below stays **unconfirmed by that sample**.
It does establish that the flush-and-reopen path is exact whenever the epoch is quiesced.

## The deterministic demonstration

`probe/zz_critic_barrier.rs`, run against a private archive copy of the code head whose only delta is a test-only rendezvous at the existing `op_write` boundary.
The shipped tree is untouched: `git diff 93cfef9 cde5930 -- crates/cowfs-core/src/io.rs` is empty, so this boundary is pre-existing and is not part of PR136.

The boundary, from the head's own `crates/cowfs-core/src/io.rs`:

```
 88        let now = Timestamp::now();
 89        {
 90            let mut st = node.st.wr();
```

The clock is read at line 88 and the node write lock is taken at line 90, with nothing in between.
The rendezvous parks the first armed writer at exactly that point, holding the real `Timestamp::now()` it has already read, and lets the test thread perform a complete write first.
No clock value is injected, replaced, mocked or synthesised; both readings come from the same host clock, microseconds apart, and the ordering is imposed by the rendezvous, not by any clock step.
A backwards host clock step is therefore not needed or implied, and I make no claim about one.

Measured, 1 run, exit 0:

```
PROBE barrier before=Timestamp { secs: 1791234737, nanos: 316526000 }
  after_later=Timestamp { secs: 1791234737, nanos: 316665000 }
  after_older_applied=Timestamp { secs: 1791234737, nanos: 316662000 }
  final_cached=Timestamp { secs: 1791234737, nanos: 316662000 }
  final_bytes="AAAAAAAA"
PROBE barrier reopened_ctime=Timestamp { secs: 1791234737, nanos: 316662000 }
  reopened_bytes="AAAAAAAA"
PROBE barrier VERDICT ctime_regressed_while_content_linearized=true
  durable_equals_final_cache=true durable_earlier_than_observed=true
```

Reading it in order.
`after_later` is the cached `ctime` after the test thread's write applied, 316665000 ns.
`after_older_applied` is what writer A's own `getattr` returned after A applied with the earlier reading it had been holding, 316662000 ns.
The cache moved **backwards by 3,000 ns** after a value had already been returned to a client.
`final_bytes` is `AAAAAAAA`, writer A's payload, so content linearized by application order: A applied last, and A is the writer whose clock reading was older.
`ctime` and content therefore disagree about which write happened last.
`reopened_ctime` equals `final_cached` exactly and `reopened_bytes` equals A's bytes, so the replay carried the final stable cached value faithfully and did not invent a time.

This is the mechanism behind the historical 7 us and 9 us gaps, established by construction instead of by luck.
A writer holding an older clock reading can be the last to apply, so the cached value it installs is the earliest of the recent ones, and the peak the earlier probe compared against was the transient window before that writer applied.

It is a cache-layer defect.
It is pre-existing in the sense that `io.rs` is byte-identical at the base and at the head, and the prior review measured the same cached regression at the base, 5 to 79 occurrences per 300,000 writes.
Pre-existing is a fact about where the code came from, not a reason to leave it: the user's standing rule is that a bug seen is a bug to fix, at the layer that owns it.

## Boundaries before any author code fix

The responsible layer is the cached write path, not the metadata replay.

- `crates/cowfs-core/src/io.rs:88` versus `:90` is the measured site: take the node write lock first, then read the clock, so a writer's `ctime` reflects its own application point.
- `crates/cowfs-core/src/inner.rs`, `Inner::op_times` and the final attribute loop, are **not** the fix site and **not** at fault here. They are faithful to the cached value, which is what the replay contract asks for.
- `crates/cowfs-meta/src/tx.rs`, `Tx::set_now`, is untouched by this finding and needs no change.
- The same read-then-lock shape appears at `io.rs:168` in `op_setattr` and `io.rs:436` in `op_removexattr`, and at `ns.rs:176` before the parent lock at `ns.rs:206`.
  I did not demonstrate any of those, so they are recorded as the same shape to check, not as measured defects.
- Ownership boundary, flagged not resolved: READY6 declares a `Core` hole-IO surface, so a change in `crates/cowfs-core/src/io.rs` may sit on that lane's boundary.
  I did not inspect READY6's files and I make no claim about what it touches.
  A fix here must be coordinated with that lane rather than landed blind.
  READY5 owns `cowfs-meta`'s database for atomic rename, which this finding does not touch.
  READY1 is doc-only, covering the `set_now` comment and errata, also untouched by this finding.
- This is one fix attempt's worth of diagnosis, not a third theory, so no spike is warranted yet.
  If a fix at `io.rs:88` fails to remove the regression in the forced-interleaving probe, that would be the second attempt and the point at which a falsifiable spike is the right instrument.

## Limitations, stated plainly

- The historical failure is **unreproduced**, not refuted.
  5 attempts and 977,507 samples did not produce it, and one occurrence in the prior review's 32 runs is too small a sample to give a rate.
  I state no frequency.
- The forced interleaving uses a rendezvous, so it proves the boundary can produce the inversion, not how often the boundary is hit in production.
- The two concurrent probes from the prior review are immutable and were not re-run; this task re-read their raw logs and added one epoch-matched sample and one deterministic case.
- No performance, timing or soak measurement, and no acceptance threshold.
- No `SIGKILL` or power-loss claim.
- `cargo test --workspace` was not run, and neither were the daemon, `PathVfs`, the FUSE or NFS adapters.
  They are outside this clarification's scope and outside the two arms measured here.
- `no-mistakes` is **not initialized** in this repository, `.no-mistakes` and `.claude` are both absent, so that pipeline was not run and no claim is made about it.
- No browser step; `chromium` is not installed, so any browser work would be **UNVERIFIED** here, and this change has no browser surface.
- `codebase-memory-mcp` graph tools were not used for this task; the source was read directly from the extracted archives, which is the binding the prior review established and the one this task required.
- MisakaNet was available only as a local stdio server and was not consulted, because no failure-recall need arose; no remote MisakaNet call was made.

## One deviation to disclose

The shared lane script hardcodes its log path, and I copied it into this lane unchanged, so my single acquisition appended **five lines** to the prior lane's `logs/lane.log` at 14:12:07 to 14:12:36, lines 153 to 157, instead of writing this lane's own log.
Verified before writing anything: that file is the only file in the prior lane modified in that window, the previous session's last entry is line 152 at 13:59:27, nothing was removed or overwritten, and no probe log, script, exit file or source tree in that lane was touched.
The five lines are reproduced verbatim in this lane's `logs/lane.log` with that explanation.
The lane discipline itself held: one foreground acquisition of `/Users/zeeshanhaque/Projects/cowfs/.treehouse-ready-wave/mac-heavy.lock`, acquired in 0 s, released in 29 s, cap 8 GiB and floor 20 GiB respected at 3,518,504 KiB used and 311,024,088 KiB free.
No other lane was disturbed and no lease operation was performed.

## Evidence

Under `bench/out/meta42-clock-observation-clarification/` in the assigned worktree, gitignored:

| what | where |
| --- | --- |
| extraction bound to the exact head, 645 of 645 and 643 of 643, 0 mismatched | `scripts/extract.sh`, `archives/mismatch-*.txt` |
| the private probe arm, its only delta being the rendezvous | `scripts/mutate-barrier.py` |
| the lane, one bounded acquisition | `scripts/lane.sh`, `logs/lane.log` |
| the deterministic demonstration, 1 run | `logs/barrier.log` |
| the one representative sample, 5 attempts | `logs/epoch-1.log` through `logs/epoch-5.log` |
| my probe sources | `probe/zz_critic_epoch.rs`, `probe/zz_critic_barrier.rs` |

Every arm ran from an extracted archive proved byte-identical to its commit, in its own `CARGO_TARGET_DIR` and its own `TMPDIR`; `archives/probe-barrier` is a private copy and the shipped tree at `cde5930` is unmodified.

The two immutable inputs were verified before and after: `pr136-meta42-operation-time-final.md` at `c4999d45…`, `meta42-operation-time.md` at `ed9609e2…`, `meta42-residual-verification.md` at `fbc6a078…`.
The assigned worktree still sits at `016769e7f4076a5c0fc712a65932c546048052f7` with zero tracked-file changes.

## Where this leaves the prior review

`PASS` on PR136 stands, with one addition to its record.
The prior report already carried the observation as "a real observation, a plausible mechanism, an unconfirmed cause, and a rate I cannot state".
That is now stronger and more specific: the mechanism is identified and demonstrated, the assertion that fired is identified as an epoch mismatch against a racing peak, the flush-and-reopen path is shown exact on a quiesced epoch across five attempts with content as well as time, and the open obligation is the cached write path's clock-before-lock ordering in `crates/cowfs-core/src/io.rs`, owned by the cache layer and coordinated with READY6.

Nothing here blocks PR136, and nothing here closes anything in issue #42.