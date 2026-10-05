# Issue #42 cache-write clock: read a write's clock after it takes the node lock

Scope: the cache-layer `ctime` obligation the PR #136 clock observation left open, at
`crates/cowfs-core/src/io.rs:88`.
This is not a replay fix and touches no replay code.

| what | value |
|---|---|
| branch | `fix/cache-write-clock-42`, cut with `git switch -c` from the verified #136 head |
| base commit | `e7ee215878cc0102ce52c7611dddc81f064105f6`, tip of `fix/deferred-operation-time-42` |
| implementation head | `e9dc1066a5601bde3bcca439d8df71e03c1972ea` |
| lease | `.treehouse-ready-wave/.treehouse/cowfs-7c1bf8/6/cowfs` |
| clarification read, sha256 verified | `docs/reviews/pr136-clock-observation-clarification.md`, `965d9c732882aed380df8c5dc4d4365e35a71997d58a621d1be765065f0ba59d` |
| PR | opened after this record was written; see the pull request link |
| prior hole-flag branch preserved | `fix/meta-hole-flag-42` at `99bf7a5efd28a80bc024f040efa2ae6fe0ca6c67`, PR #138, untouched |
| `cde5930` preserved | unchanged |

## The defect, as measured before this change

The clarification named the site and the shape, with its own deterministic demonstration, and I
re-derived both rather than trusting the prose.

`op_write` read the clock and then took the node write lock, with nothing in between:

```
 88        let now = Timestamp::now();
 89        {
 90            let mut st = node.st.wr();
```

A writer between those two statements holds a real `Timestamp::now()` reading and no lock.
Two writers can therefore apply in the opposite order to the one in which they read their clocks.
Content linearizes by application order, so the last write's bytes win.
`ctime` follows the order the clocks were read, so the cached value can move **backwards** after a
client has already observed a later one, and content and `ctime` disagree about which write
happened last.

The clarification recorded three facts I relied on and did not re-litigate:

- the durable value after flush and reopen equalled the final stable cached value exactly, so the
  replay was faithful and `PASS` on #136 stands;
- the `7 us` and `9 us` historical gaps were an epoch mismatch against a transient racing peak, not
  a replay failure;
- the same read-then-lock shape also appears at `io.rs:168`, `io.rs:436` and `ns.rs:176`, recorded
  as the same shape to check and **not** as measured defects.

## The change

One statement moved, `3 insertions, 1 deletion` in one file:

```rust
         {
             let mut st = node.st.wr();
             if let Some(e) = node.poisoned() {
                 return Err(e);
             }
+            // read under the node lock: a writer that parks here applies after, so its clock must be
+            // its own application point rather than a reading taken before the wait
+            let now = Timestamp::now();
             let NodeState { attr, file, .. } = &mut *st;
```

The reading now happens at the writer's own application point, so a writer that arrives at the
boundary first and applies last gets the later reading.

Everything else in `op_write` keeps its order and its position: the `MAX_FILE` and zero-length
checks, `ensure_file`, the partial-chunk verify loop, `poisoned`, the `Stale` returns, the
dirty-byte accounting, the queue touch under `sc.q.lk()`, and the in-lock `try_enter` whose
existing comment already states that a node lock is held there.
`Timestamp` is still imported from the same place, and `io.rs` is the only file changed.

No monotonic clock, no clamp, no atomic clock and no new time framework.
`Inner::op_times`, the final per-inode attribute loop and `Tx::set_now` are untouched, because the
replay was never the defect.

## The proof, and its instrumentation, stated honestly

### The probe

A private archive copy of each tree receives exactly one delta: a test-only rendezvous parked at
the boundary between "has finished its pre-lock work" and "takes the node write lock".

- The parked writer holds **no node lock** while it waits, so the second writer is never blocked
  behind it. The clarification's probe parked at the same functional boundary; I moved the
  injection point one statement earlier in the new arm because the clock now lives inside the lock.
- No clock value is injected, replaced, mocked or synthesised. Both readings come from the same
  host clock, microseconds apart. The ordering is imposed by the rendezvous, not by a clock step,
  so a backwards host clock step is neither needed nor implied, and none is claimed.
- The probe arms are private archive copies. **The shipped tree carries no probe code and no
  permanent test.**

Writer A reaches the boundary first and parks; the test thread performs a complete write with its
own later real reading; A is released and applies last.

### Old fail, new pass

| build | `zz_cache_clock` | exit |
|---|---|---|
| `e7ee215` plus the probe arm | **0 passed, 1 failed** | 101 |
| this head plus the probe arm | **1 passed, 0 failed** | 0 |

Old arm, verbatim from `logs/old-probe.log`:

```
PROBE clock before=Timestamp { secs: 1791241401, nanos: 648408000 }
  after_second=Timestamp { secs: 1791241401, nanos: 648545000 }
  writer_A_reported=Timestamp { secs: 1791241401, nanos: 648541000 }
  final_cached=Timestamp { secs: 1791241401, nanos: 648541000 } final_bytes="BBBBBBBB"
the cache ctime moved backwards: Timestamp { secs: 1791241401, nanos: 648541000 }
  is earlier than the Timestamp { secs: 1791241401, nanos: 648545000 } already observed by a client
```

The regression is 4,000 ns, and the value that regressed was one a client had already read back
through `getattr`.

New arm, verbatim from `logs/new-probe.log`:

```
PROBE clock before=Timestamp { secs: 1791241503, nanos: 785279000 }
  after_second=Timestamp { secs: 1791241503, nanos: 785372000 }
  writer_A_reported=Timestamp { secs: 1791241503, nanos: 785378000 }
  final_cached=Timestamp { secs: 1791241503, nanos: 785378000 } final_bytes="BBBBBBBB"
PROBE clock reopened_ctime=Timestamp { secs: 1791241503, nanos: 785378000 }
  reopened_bytes="BBBBBBBB" (final cached was Timestamp { secs: 1791241503, nanos: 785378000 })
PROBE clock VERDICT ctime_monotonic_across_forced_interleaving=true durable_equals_final_cache=true
  bytes_linearized=true
```

Reading it: writer A's clock reading is now 6,000 ns **later** than the second writer's, because A
read it after applying.
Content is `BBBBBBBB`, writer A's bytes, in both arms, because A applies last.
The reopened `ctime` equals the final cached value exactly, so the replay carried the value
faithfully, which is the property the clarification already established and this run re-confirms.

The probe also carries a bounded wait, so a build where the boundary is never reached fails with
"this run proves nothing" rather than hanging, and it parks no lock, so the second writer cannot
deadlock behind it.

### Source binding

Both arms ran from a `git archive` extraction proved byte-identical to its commit, each with its
own `CARGO_TARGET_DIR` and its own `TMPDIR`.

| archive | base | tracked | mismatched | delta from base |
|---|---|---|---|---|
| `old-src` | `e7ee215` | 650 | **0** | the probe file only |
| `new-src` | `e7ee215` | 650 | **0** | `crates/cowfs-core/src/io.rs` modified, the probe file added |

The old arm's injection anchor, `let now = Timestamp::now();` followed by `{` then
`let mut st = node.st.wr();`, appears exactly once in the old tree, checked before the arm was
built, and the mutator refuses to build on any other count.
The new arm's anchor appears exactly once too.

No stale target directory was reused and no hot rebuild of the wrong source was measured.
The old arm's target dir was built from the old archive only.

## Scoped checks, exit codes read directly

| command | result | exit |
|---|---|---|
| `cargo test -p cowfs-core --test zz_cache_clock`, `e7ee215` + probe | 0 passed, 1 failed | 101 |
| `cargo test -p cowfs-core --test zz_cache_clock`, this head + probe | 1 passed, 0 failed | 0 |
| `cargo test -p cowfs-core --test operation_time` | 4 passed, 0 failed | 0 |
| `cargo test -p cowfs-core --test caches` | 2 passed, 0 failed | 0 |
| `cargo test -p cowfs-core --test critic2b` | 27 passed, 0 failed, 1 ignored | 0 |
| `cargo test -p cowfs-core --test locks` | 2 passed, 0 failed | 0 |
| `cargo test -p cowfs-core --test chunks --test flush_boundary --test poison --test durability` | 15 passed, 0 failed | 0 |
| `cargo test -p cowfs-core --test invariant --test core` | 24 passed, 0 failed | 0 |
| `cargo fmt --all -- --check` | clean | 0 |
| `cargo clippy -p cowfs-core --all-targets -- -D warnings` | clean | 0 |

Why these and not more.

`locks` is the lock-order target, and it is the one that matters most for this change, because
reading a clock under a lock is only safe if the lock order is unchanged.
It passes at 2 of 2.
`operation_time` is PR #136's deferred-operation `ctime` fixture and still passes at 4 of 4, so
the fix did not disturb the replay contract that fixture pins.
`caches`, `chunks`, `flush_boundary`, `poison`, `durability`, `invariant` and `core` cover the
surrounding write, flush and invariant surface.

The 1 ignored in `critic2b` is reported as ignored, not as passed.
The full 91-suite matrix was deliberately not run: the brief scopes this to the small
`operation_time` and core `io` set, and other workers are active.

## What this deliberately does not claim

- **No permanent concurrency regression test.** The rendezvous is test-only instrumentation in a
  private archive copy. Reproducing the interleaving deterministically needs a production seam that
  does not exist, and the brief says to propose rather than invent one, so the shipped tree is one
  line moved and nothing else. A coordinator decision on whether a seam is worth it is open.
- **No rate.** The forced interleaving proves the boundary could produce the inversion, not how
  often it is hit in production.
- **`io.rs:168`, `io.rs:436` and `ns.rs:176` are not fixed.** They have the same read-then-lock
  shape. None was reproduced here, and the brief forbids fixing an unreproduced path.
- **The historical `7 us` and `9 us` gaps stay unreproduced.** Five epoch-matched attempts with
  977,507 samples did not produce them, per the clarification, and this change did not attempt to.
- **No performance, timing or soak measurement,** and no acceptance threshold.
- **No `SIGKILL` or power-loss claim.**
- **`cargo test --workspace` was not run,** and neither were the daemon, `PathVfs`, the FUSE or NFS
  adapters.
- **`no-mistakes` is not initialized** in this repository, `.no-mistakes` and `.claude` are both
  absent, so that pipeline was not run and no claim is made about it.
- **No browser step.** `chromium` is not installed, so any browser work would be **UNVERIFIED** here,
  and this change has no browser surface.
- **`codebase-memory-mcp` graph tools were not used.** The binding this task required was the
  extracted archive, compared to the commit with `git hash-object` per file.
- **MisakaNet was available only as a local stdio server** and was not consulted; no failure-recall
  need arose and no remote call was made.

## Boundaries respected

- `crates/cowfs-core/src/io.rs` `op_write` is the only file edited.
- `crates/cowfs-core/src/inner.rs`, `Inner::op_times`, the final replay attribute loop, `Tx::set_now`
  and `cowfs-meta`'s `db.rs` are untouched, so PR #136's clock work and its review are unaffected.
- `Core`'s `lib.rs`, `cowfs-store`, the hole flag, the daemon, `PathVfs`, swap, rename and the inode
  policy are untouched.
- The #42 request-3 branch `fix/meta-hole-flag-42` and its immutable archives were read only, and
  its `99bf7a5` head is preserved.
- Whole #42 stays **open**. Requests 1, 2, 3, 4 and the remaining clock work are not claimed by this
  change.

## Evidence

Under `bench/out/meta42-cache-write-clock/` in the lease, gitignored:

| file | what |
|---|---|
| `lane.sh` | this lane's own script, with its own log path |
| `logs/lane.log` | every acquisition, every exit, every free and used reading |
| `scripts/mutate.py` | the probe-arm builder, one delta per arm, refusing on any unexpected anchor count |
| `probe/zz_cache_clock.rs` | the probe source, one test |
| `logs/old-probe.log` | the old arm, 1 failure with the regression and its nanoseconds |
| `logs/new-probe.log` | the new arm, 1 pass with the durable equality |
| `logs/scoped.log` | fmt, `operation_time`, clippy |
| `logs/io-scoped.log` | `locks`, `chunks`, `flush_boundary`, `poison`, `durability`, `invariant`, `core` |
| `logs/caches-critic2b.log` | `caches` and `critic2b` |
| `archives/old-src/`, `archives/new-src/` | the extracted trees the two arms ran from |
| `old-target/`, `new-target/`, `old-tmp/`, `new-tmp/` | the isolated target and temp dirs |

Lane discipline: one bounded 600 s foreground acquisition per batch of the shared
`mac-heavy.lock`, acquired at 0 s every time this lane ran, released between 1 s and 174 s.
The 8 GiB cap is on `bench/out` as a whole, not only this lane's directory.
Measured before any archive or build: `bench/out` at 4,802,676 KiB, 4.58 GiB, with 3.42 GiB of
headroom under the cap, and a comparable previous lane at 0.91 GiB, projecting 5.5 GiB total.
Free floor: 297,061,440 KiB free at the tightest reading, against a 20 GiB floor.
No cleanup, pruning, moving, offloading or cap waiver was performed here: that is READY5's
approved scope, not this lane's.
