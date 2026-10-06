# The cache-write clock ordering now has a permanent regression test

Scope: the gap the independent review of PR #139 recorded at
`docs/reviews/pr139-meta42-cache-write-clock-final.md`, sha256
`72d37cc4d209ca2c3bda04e969e7889d16b846f7dc622c18b0ffb5e8ca5c6fa7`.
Nothing else. No production behaviour changed by this work.

| what | value |
|---|---|
| branch | `fix/cache-write-clock-42` |
| reviewed head this starts from | `ad6dca8b7b5bd331e8e553e7874c0852458a1c15` |
| review read, sha256 verified | `docs/reviews/pr139-meta42-cache-write-clock-final.md`, `72d37cc4d209ca2c3bda04e969e7889d16b846f7dc622c18b0ffb5e8ca5c6fa7` |
| the review's verdict acted on | independent **PASS**, CI three green at `ad6dca8b` |
| files changed | `crates/cowfs-core/src/io.rs` only |
| lease | `.treehouse-ready-wave/.treehouse/cowfs-7c1bf8/6/cowfs` |

## The gap, in the review's words

> **The shipped tree carries no test that exercises this fix.**
> The rendezvous exists only in private archive copies, so CI cannot regress this ordering: a future
> change that moved `Timestamp::now()` back above the lock would leave every test in the repository
> green, exactly as it was green before the fix.

The review then established that a minimal seam was possible without inventing anything:
`cowfs-core` already carries `#[cfg(test)]` test-only seams on `Core`, both `#[doc(hidden)]`, and
`#[cfg(test)] mod gc_barrier_window` is the established place to drive one.

## What was added

Two insertions and zero deletions in `crates/cowfs-core/src/io.rs`, verified line by line below.

**The seam**, two lines at the boundary `op_write` already had, immediately before the node write
lock is taken:

```rust
        #[cfg(test)]
        park_if_armed();
```

**The gate and the test**, 175 lines at the end of the same file: `ClockGate`, a `thread_local!`
holding the gate armed for one thread, `park_if_armed`, and `mod clock_order_tests` with one test.

Nothing outside `io.rs` changed. No new public API: every added item is `pub(crate)` and `cfg(test)`,
so it does not exist in a production build at all. No new dependency, no Cargo feature, no new file.

## Why the gate is thread-scoped and not a static

A process-global gate would be a mutable global that two tests running in parallel could both arm, and
one test's writer could park inside the other test's window.
`ClockGate::arm` is called **on the writer's own thread** and stores the `Arc` in a `thread_local!`.
A writer that never armed a gate finds `None` and returns immediately, so the only writer that can ever
park is the one that asked to.
What the test and that writer share is the `Arc`, which is not mutable global state.

`taken` keeps a second writer on the same thread from queueing behind the first.

## Why it cannot deadlock

The park point is before `node.st.wr()`, so the parked writer holds **no node lock**.
The second writer, on the thread that armed no gate, proceeds and finishes while the first is parked.
That is the whole point of the boundary, and it is the same boundary the review's private probe used.

## Why the waits fail instead of hanging

Both directions are bounded at 60 seconds with a message, not a hang.

- The parked writer asserts `Instant::now() < deadline` on every spin and panics with
  "the parked op_write writer was never released" if the test never releases it.
- The test asserts `Instant::now() < deadline` while waiting to be told a writer arrived, and panics
  with "no writer reached the op_write boundary, so this run proves nothing" if the boundary was
  never reached.

A run that proves nothing fails rather than passing quietly, and a stuck run fails rather than
occupying the lane.

## The clock is real, and nothing claims it is monotonic

Both `Timestamp::now()` readings come from the host wall clock, microseconds apart.
No value is injected, replaced, mocked or synthesised, and there is no injected clock anywhere in the
tree.
The ordering between the two writers is imposed by the rendezvous, not by a clock step, so a backwards
host clock step is neither needed nor implied.

The assertions are about ordering relative to what a client already observed, never about absolute
time: `final_cached >= after_second`, and `reopened.ctime == final_cached`.
**No global monotonicity is claimed.** The host wall clock can step backwards, and this test says
nothing about that.

## Old fail, new pass, one fixture, the mutant is only the clock position

The two arms are extractions of the same tree, bound blob for blob to `ad6dca8b`.
651 tracked files each, exactly one file differing from the base, and **no extra files**, so there is
no injected fixture: the test lives in `src/io.rs` where it ships.

The only difference between the arms is where the clock reading sits:

```
-        let now = Timestamp::now();                     <- old arm: above the lock
         #[cfg(test)]
         park_if_armed();
         {
             let mut st = node.st.wr();
             if let Some(e) = node.poisoned() {
                 return Err(e);
             }
+            // read under the node lock: a writer that parks here applies after, so its clock must be
+            // its own application point rather than a reading taken before the wait
+            let now = Timestamp::now();                 <- new arm: under the lock
```

That is the entire mutant, printed as the complete unified diff of `io.rs` between the two arms.
No feature was disabled and no hook was switched off: the gate, the park point and the test are
byte-identical in both arms.

The sequence the test imposes: the first writer reaches the boundary and parks there holding no node
lock, the test writes as the second writer and reads `ctime`, then the parked writer is released and
applies last.

| build | `io::clock_order_tests` | exit |
|---|---|---|
| old arm, clock read above the lock | **0 passed, 1 failed** | 101 |
| new arm, clock read under the lock | **1 passed, 0 failed** | 0 |

Old arm, verbatim from `logs/old-sample.log`:

```
the cached ctime moved backwards: Timestamp { secs: 1791243681, nanos: 427413000 }
  is earlier than the Timestamp { secs: 1791243681, nanos: 427416000 } a client had already observed
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 29 filtered out
```

3,000 ns of regression, on a value the test had already read back through `getattr`, and the failure
is the assertion the fix is about.

New arm, verbatim from `logs/new-sample.log`:

```
test io::clock_order_tests::ctime_does_not_move_backwards_when_the_first_writer_applies_last ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 29 filtered out
```

The test also pins what the review's private probe pinned: content is the parked writer's bytes,
because it applies last, and the durable `ctime` after `flush`, `drop` and a fresh `Core::open` equals
the final cached value exactly.

## Source and executable bindings

| what | identity |
|---|---|
| base commit | `ad6dca8b7b5bd331e8e553e7874c0852458a1c15` |
| `archives-old` | 651 tracked, **0 mismatched outside `io.rs`**, **0 extra files** |
| `archives-new` | 651 tracked, **0 mismatched outside `io.rs`**, **0 extra files** |
| `io.rs` sha256, lease tree and `archives-new` | `0351206c229225600cfe071f1c4d3df9fe4ba0e4bf7fcacdfdc449ce90af67a7`, identical |
| `io.rs` sha256, `archives-old` | `23cea07a03ef663b61cbf642140c66beeb8f1d4b0ef5e2fefb1af7b3a8ac732c` |
| old test binary | `cowfs_core-04b1a37622c17b3e`, sha256 `c676f57ec21b5c77b753e40a234e8de2…` |
| new test binary | `cowfs_core-04b1a37622c17b3e`, sha256 `d96313055c3bac649eec549c891d7510…` |

The two test binaries share a name and differ by hash, which is the arm binding: same crate, different
source, and the difference is only the clock position.
Each arm has its own `CARGO_TARGET_DIR` and its own `TMPDIR`.
No target directory was reused across arms and no hot rebuild of the wrong source was measured.

## The production build carries none of it

`#[cfg(test)]` items do not exist in a production build, and that is checked rather than assumed:

| check | result |
|---|---|
| `cargo build -p cowfs-core` | exit 0 |
| `ClockGate`, `park_if_armed` or `ARMED` symbols in `libcowfs_core.rlib` | **none** |
| `park_if_armed` symbols in the test binary | 14, as expected |

So the normal write path takes **zero** gate atomics, zero probes and zero extra branches.
The `#[cfg(test)] park_if_armed();` call site compiles out entirely.

## Line-level diff, mechanically checked

A raw-line `difflib` comparison of `io.rs` against `ad6dca8b`, comment lines included:

```
insert: base[87:87] -> new[87:89]
    + '        #[cfg(test)]'
    + '        park_if_armed();'
insert: base[482:482] -> new[484:659]
    + 175 lines: the gate, the park helper and the test module
deleted base lines: none
```

Two insertions, **zero deletions**.
Every line of the reviewed clock fix is byte-identical: the reading is still under the node lock, after
the poison check, in the same position relative to `poisoned`, the `Stale` returns, the dirty-byte
accounting, the queue touch and the in-lock `try_enter`.

Added items and their visibility: `pub(crate) struct ClockGate`, `pub(crate) fn arm`, `pub(crate) fn
parked`, `pub(crate) fn release`, all `#[cfg(test)]`.
No `pub` item was added, so the crate's public API surface is unchanged.

## Scoped checks, exit codes read directly

| command | result | exit |
|---|---|---|
| `cargo test -p cowfs-core --lib clock_order_tests`, old arm | **0 passed, 1 failed** | 101 |
| `cargo test -p cowfs-core --lib clock_order_tests`, new arm | **1 passed, 0 failed** | 0 |
| `cargo test -p cowfs-core --lib` | **30 passed, 0 failed**, 0 ignored | 0 |
| `cargo test -p cowfs-core --test operation_time` | 4 passed, 0 failed | 0 |
| `cargo test -p cowfs-core --test locks` | 2 passed, 0 failed | 0 |
| `cargo test -p cowfs-core --test caches` | 2 passed, 0 failed | 0 |
| `cargo fmt --all -- --check` | clean | 0 |
| `cargo clippy -p cowfs-core --all-targets -- -D warnings` | clean | 0 |
| `rustfmt --edition 2021 --check crates/cowfs-core/src/io.rs`, standalone | clean | 0 |

The whole lib test binary is run, not only the new module, so the gate is exercised alongside the
30 other in-crate unit tests: that is the check that the thread-scoped gate does not disturb its
neighbours.

`operation_time` is PR #139's deferred-operation `ctime` fixture and still passes at 4 of 4.
`locks` is the lock-order target and passes at 2 of 2, which is the property that matters for reading a
clock under a lock.
`caches` covers the surrounding write and flush surface.

No ignored tests in any of these targets, so nothing is reported as ignored.
The full multi-crate suite was deliberately not run: the brief scopes this to the sample plus these
targets, and other workers are active.

## What this deliberately does not claim

- **No rate.** One forced interleaving per arm proves the boundary can produce the inversion, not how
  often it is hit.
- **No global monotonicity.** `Timestamp::now()` is the host wall clock and can step backwards. The test
  orders one reading relative to the node lock and asserts nothing about absolute time.
- **No fix at the other read-then-lock sites.** `io.rs:168`, `io.rs:436` and `ns.rs:176` still have the
  same shape. None was reproduced, and fixing an unreproduced path is out of scope.
- **The historical `7 us` and `9 us` gaps stay unreproduced,** per the clarification.
- **No performance or timing acceptance,** and no acceptance threshold.
- **No `SIGKILL` or power-loss claim.**
- **`cargo test --workspace` was not run,** and neither were the daemon, `PathVfs`, the FUSE or NFS
  adapters.
- **The PR #139 author's earlier 811-pass figure is not re-executed here.**
- **`no-mistakes` is not initialized** in this repository, `.no-mistakes` and `.claude` are both absent,
  so that pipeline did not run and no claim is made about it.
- **No browser step.** `chromium` is not installed, so any browser work would be **UNVERIFIED** here, and
  this change has no browser surface.
- **`codebase-memory-mcp` graph tools were not used.** The source read was this one file plus the
  crate's existing test seams, read directly, which is the binding this task required.
- **MisakaNet was local-only and was not consulted.** No remote call was made.

## Budget and lane discipline

Measured before any archive or build:

| measure | value |
|---|---|
| `bench/out` before | 5,748,212 KiB, 5.48 GiB |
| cap | 8,388,608 KiB, 8.00 GiB |
| headroom | 2,640,396 KiB, 2.52 GiB |
| proven single `cowfs-core` compile in this same lane | 463,868 KiB, 0.44 GiB |
| plan, two fresh target directories | 927,736 KiB, 0.88 GiB |
| projected total | 6,675,948 KiB, 6.37 GiB, **within cap** |
| free at the tightest reading | 294,004,628 KiB against a 20,971,520 KiB floor |

Four bounded 600 s foreground acquisitions of `/Users/zeeshanhaque/Projects/cowfs/.treehouse-ready-wave/mac-heavy.lock`,
acquired at 0 s each time, released between 1 s and 83 s.
Logs append and flush per phase under this lane's own log path.
No cleanup, deletion, pruning, moving, offloading or cap waiver was performed: that is READY5's approved
scope, not this lane's, and no approval for it exists here.
No signal, no shared resource, no mount walk, no install, no sudo, no lease action.

## Two build failures of my own, recorded rather than dropped

The first two attempts at compiling this fixture failed on my own mistakes, both before any test ran,
and both are in the logs rather than hidden:

1. `error[E0433]: cannot find module or crate 'clock_gate'`.
   The call site said `clock_gate::park_if_armed()` while the definition is a plain same-module
   function.
   Fixed to a plain call.
2. `error[E0599]: no method named 'arm' found for struct 'Arc<ClockGate>'`, then
   `error[E0308]: mismatched types` on `a.join()`. I had written `arm` as an associated function
   returning a fresh `Arc`, so the writer armed a gate the test was not holding, and the thread returned
   a tuple the join destructured after I had already simplified it. Fixed by making `arm` take
   `self: &Arc<Self>` and arm the shared gate, and by returning a bare `Timestamp`.

No third theory was needed and no spike was warranted: each failure named its own cause and its own fix.

## Evidence

Under `bench/out/meta42-cache-write-clock-permanent/` in the lease, gitignored:

| file | what |
|---|---|
| `lane.sh` | this lane's own script and log path |
| `logs/lane.log` | every acquisition, exit, free and used reading |
| `logs/old-sample.log` | the old arm: 1 failure with the nanoseconds |
| `logs/new-sample.log` | the new arm: 1 pass |
| `logs/scoped.log` | fmt, clippy, `operation_time`, `locks`, `caches`, the full lib binary |
| `logs/bindings.log` | base blob checks, `io.rs` sha256 per arm, test binary hashes |
| `archives-old/`, `archives-new/` | the two extractions, bound to `ad6dca8b` |
| `old-target/`, `new-target/`, `old-tmp/`, `new-tmp/` | the isolated target and temp directories |

Prior records untouched and immutable: `a8bc5337…` the cache-write-clock record, `72d37cc4…` the
independent review, `b1f1940a…` the hole-flag finalization delta, `b63eb260…` the hole-flag review.
