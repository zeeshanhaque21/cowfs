# Closing the coverage gap the final review found in the permanent clock test

Scope: the one finding in `docs/reviews/pr139-meta42-cache-write-clock-permanent-final.md`, sha256
`75a2310b5b74018171fba9116faec5b73850b5e712fa90b1555889ebe5d97920`.
A coverage gap in the test, not a production defect, and not new scope.
The production fix is untouched.

| what | value |
|---|---|
| branch | `fix/cache-write-clock-42` |
| head this starts from | `cd1d5afa1f6cab57e479f911e1e202ce29fa7e49` |
| review read, sha256 verified | `docs/reviews/pr139-meta42-cache-write-clock-permanent-final.md`, `75a2310b5b74018171fba9116faec5b73850b5e712fa90b1555889ebe5d97920` |
| files changed | `crates/cowfs-core/src/io.rs` only |
| lease | `.treehouse-ready-wave/.treehouse/cowfs-7c1bf8/6/cowfs` |

## The gap, as the review measured it

The review built a third arm, `gap`, that places the reading between the park and the lock:

```rust
        #[cfg(test)]
        park_if_armed();
        let now = Timestamp::now();          // <- gap arm: guard not yet taken
        {
            let mut st = node.st.wr();
```

That reading is still taken before the node write lock, so the production defect survives completely.
**The permanent test passed on it**, in 0.20 s, which the review confirmed was a real run and not a
gate that never engaged.

The cause is the gate's position relative to the reading.
The gate sits before the reading, so a parked writer has not read its clock when it parks; on release
it reads a *later* value and there is nothing to regress.
The test therefore pinned "the reading did not move above `park_if_armed()`", which is a proxy for "the
reading is under the lock" but is not the same property.
The review was fair about the width of it: the exact historical revert is caught, a slide past the park
is caught, a slide into the park-to-lock space is not.

## The repair: make the guard a parameter, not a comment

`op_write` no longer calls `Timestamp::now()` directly.
It calls a helper that cannot be called without holding the node write guard:

```rust
            let mut st = node.st.wr();
            if let Some(e) = node.poisoned() {
                return Err(e);
            }
            // read under the node lock: a writer that parks here applies after, so its clock must be
            // its own application point rather than a reading taken before the wait
            let now = clock_read(&st);
```

and the helper, in a production build, is the clock call and nothing else:

```rust
#[cfg(not(test))]
#[inline]
fn clock_read(_guard: &std::sync::RwLockWriteGuard<'_, NodeState>) -> Timestamp {
    Timestamp::now()
}
```

The guard parameter is the whole mechanism.
There is exactly one call site for `clock_read` in the crate, and it is inside the guard's scope.
A reading moved back above the lock, or into the park-to-lock space, is therefore a **compile error**,
not a silent regression that CI cannot see.
That is a compile-refusal property and it is labelled as one here; it is not claimed as a runtime
observation.

The test's half is a counter, so the same fixture also refuses the gap at runtime rather than only at
compile time:

```rust
#[cfg(test)]
#[inline]
fn clock_read(_guard: &std::sync::RwLockWriteGuard<'_, NodeState>) -> Timestamp {
    let now = Timestamp::now();
    READS_UNDER_GUARD.with(|c| c.set(c.get() + 1));
    now
}
```

`READS_UNDER_GUARD` is a `thread_local!` `Cell<usize>`, so it is per thread like the existing gate slot,
and the parked writer reports its own count back through the join.
The test asserts that count is exactly 1.

**The counter is not a self-fulfilling marker and is not placed after the clock.**
It is incremented inside the one function the reading must pass through, and that function is
unreachable from any site that does not hold a write guard.
A mutant that bypasses it with a bare `Timestamp::now()` leaves the count at 0 and the test fails.

What the counter is **not**: a lock-liveness probe.
It does not inspect the guard, does not ask the lock who owns it, and proves nothing about ownership.
Its evidence is narrower and stated as such: a reading happened through the guard-requiring path.
The stronger half is the compile-time requirement, which is a different kind of claim and is not
conflated with this one.

## Three arms, one fixture, each mutant a single reading moved

Both mutants are the same one-line slide out of the guarded call, nothing else.
Printed as the complete `io.rs` diff against the shipped arm, the `gap` mutant is:

```
+        let now = Timestamp::now();
-            // read under the node lock: a writer that parks here applies after, so its clock must be
-            // its own application point rather than a reading taken before the wait
-            let now = clock_read(&st);
```

and the `old` mutant is byte-identical to it.
The `park_if_armed` call, the `ClockGate`, the `thread_local!` slots, the test body and every assertion
are unchanged in all three arms: no feature disabled, no hook switched off, no marker removed.

The `old` arm's slide above the park is the historical revert the previous delta already measured.
The `gap` arm's slide into the park-to-lock space is the review's finding, and it is the arm this
correction exists for.

| arm | where the single reading sits | `io::clock_order_tests` | exit |
|---|---|---|---|
| `new` | under the lock, through `clock_read(&st)` | **1 passed, 0 failed** | 0 |
| `gap` | between the park and the lock | **0 passed, 1 failed** | 101 |
| `old` | above the park, the historical revert | **0 passed, 1 failed** | 101 |

Both mutants fail on the same assertion, verbatim from `logs/three-arms.log`:

```
assertion `left == right` failed: the writer's clock reading must be taken while it holds the node
  write guard
```

against the expected 1.
Before the correction the same `gap` arm passed, which is the gap the review reported.

## The old arm still fails for its own reason too

The counter assertion sits before the behavioural one, so on the `old` arm it fires first.
To confirm the behavioural half is independently sound rather than shadowed, I reordered only those two
assertion blocks in the `old` arm so the behavioural one runs first, changing nothing else, and re-ran:

```
the cached ctime moved backwards: Timestamp { secs: 1791245573, nanos: 981054000 }
  is earlier than the Timestamp { secs: 1791245573, nanos: 981059000 } a client had already observed
```

exit 101, from the same fixture on the same arm.
So both halves of the test are independently load-bearing: the counter refuses a reading that took the
wrong path, and the `ctime` comparison refuses the regression itself, 5,000 ns of it in this run.
The `old` archive was then restored to its measured state, verified by sha256 equal to the value from
the three-arm run.

## Source and executable bindings

| archive | base | tracked | mismatched | extra files |
|---|---|---|---|---|
| `archives-new` | `cd1d5af` | 652 | 1 file, `crates/cowfs-core/src/io.rs` | 0 |
| `archives-gap` | `cd1d5af` | 652 | 1 file, `crates/cowfs-core/src/io.rs` | 0 |
| `archives-old` | `cd1d5af` | 652 | 1 file, `crates/cowfs-core/src/io.rs` | 0 |

`io.rs` sha256 per arm:

| arm | sha256 |
|---|---|
| `new` | `41b5a38ecdc6a64f19ccb16f3f9cfdd618b7fe34ad65eb335c8539b80bea1961` |
| `gap` | `3a791b9a11f7d20c7475cba636a7c0824c7bb325ec093a68af04dabf94c68cb1` |
| `old` | `1bfbb7330e3f26a2ce26b575a2df6d93f794f5d895e71913fd83a710fb5fe63a` |

Each arm has its own `CARGO_TARGET_DIR` and its own `TMPDIR`.
No target directory was reused across arms and no hot rebuild of the wrong source was measured.

## Production cost, measured rather than argued

`clock_read` exists in production, unlike the gate machinery.
Its cost was measured at the object level, not asserted:

| check | result |
|---|---|
| `ClockGate`, `park_if_armed`, `ARMED` symbols in `libcowfs_core.rlib` | 0 each |
| `READS_UNDER_GUARD`, `reads_under_guard`, `clock_order_tests` in the production rlib | 0 each |
| undefined symbols the production object containing `clock_read` needs | exactly one: `cowfs_vfs::types::Timestamp::now` |
| TLS or thread-local symbol references in that object | 0 |
| `__atomic` or `__sync` libcall references in that object | 0 |

So in a production build the reading is the same vDSO clock call it always was, reached through one
inlined function whose only other effect is passing a borrow the compiler discards.
The guard is taken before the reading and released after, exactly as before this correction; only the
call's shape changed, and no production clock policy, clamp or extra lock was introduced.

## Scoped checks

| command | result | exit |
|---|---|---|
| `cargo test -p cowfs-core --lib clock_order_tests`, `new` arm | **1 passed, 0 failed** | 0 |
| `cargo test -p cowfs-core --lib clock_order_tests`, `gap` arm | **0 passed, 1 failed** | 101 |
| `cargo test -p cowfs-core --lib clock_order_tests`, `old` arm | **0 passed, 1 failed** | 101 |
| `cargo test -p cowfs-core --lib clock_order_tests`, `old` arm, assertions reordered | 0 passed, 1 failed | 101 |
| `cargo test -p cowfs-core --lib` | **30 passed, 0 failed**, 0 ignored | 0 |
| `cargo test -p cowfs-core --test locks` | 2 passed, 0 failed | 0 |
| `cargo test -p cowfs-core --test operation_time` | 4 passed, 0 failed | 0 |
| `cargo test -p cowfs-core --test caches` | 2 passed, 0 failed | 0 |
| `cargo fmt --all -- --check` | clean | 0 |
| `rustfmt --edition 2021 --check crates/cowfs-core/src/io.rs`, standalone | clean | 0 |
| `cargo clippy -p cowfs-core --all-targets -- -D warnings` | clean | 0 |

The whole lib test binary runs, so the gate and the counter are exercised alongside the 29 other
in-crate unit tests, which is the empirical check that neither disturbs its neighbours.
`locks` is the lock-order target and is the one that matters most here, because reading a clock under a
lock is only safe if the lock order is unchanged.
`operation_time` is PR #139's deferred-operation `ctime` fixture, and `caches` is the neighbouring write
and flush surface.

No ignored tests in any of these targets, so nothing is reported as ignored.

## Read-only integration with current `main`

The review noted that `main` has moved since it was written; at the time of this check `main` is
`cf67e8a6b2f8d346485fdf1c71d24283da0b43a0`, the #137 rename merge.

No checkout, no merge, no branch switch, no source written, and no primary-checkout edit:

```
git merge-tree --write-tree origin/main HEAD
44e7bbcec61e0ddcb905c8d72cfb4cedabace146
conflict lines: 0
exit 0
```

`main` has not touched `crates/cowfs-core/src/io.rs`, `tests/locks.rs` or `tests/operation_time.rs`
relative to this branch's base, so those merge trivially.
`main` **has** changed `tests/caches.rs` relative to the base, through the hole-flag work, and the
merged tree keeps `main`'s version of that file exactly, with
`live_blocks_filters_holes_and_yields_only_stored_blocks` and the two-walks-agree assertion intact.
The merged tree's `io.rs` carries both `clock_read` and the gate call.

**A compile of the integrated tree was not run and is not claimed.**
After the three arms, `bench/out` stood at 8,050,828 KiB against the 8,388,608 KiB cap, leaving
337,780 KiB of headroom, and one proven `cowfs-core` compile in this lane needs 463,868 KiB.
An integration build would have breached the cap, and no cleanup, pruning, offloading or cap waiver is
authorised here.
That is the honest limit: integration is verified as a clean, conflict-free tree and not as a green run.

## Budget and lane discipline

Measured before any archive or build:

| measure | value |
|---|---|
| `bench/out` before | 6,698,320 KiB, 6.39 GiB |
| cap | 8,388,608 KiB, 8.00 GiB |
| headroom at that point | 1.61 GiB |
| three fresh target directories, at the proven 0.44 GiB each | 1.33 GiB |
| projected | 8,089,924 KiB, 7.72 GiB, **within cap** |
| free at the tightest reading | 287,303,728 KiB against a 20,971,520 KiB floor |

Five bounded 600 s foreground acquisitions of `/Users/zeeshanhaque/Projects/cowfs/.treehouse-ready-wave/mac-heavy.lock`.
The first three acquired at 0 s and released in 1 s and 34 s.
The fourth waited 191 s behind another lane and then ran, which is the lane working as intended rather
than a timeout.
The fifth was the integration read-only check, which needed no build.
Logs append and flush per phase under this lane's own log path.

No cleanup, deletion, pruning, moving, offloading or cap waiver was performed: that is READY5's approved
scope and no approval for it exists here.
No signal, no shared resource, no mount walk, no install, no sudo, no lease action.

## What this deliberately does not claim

- **The compile-time half is a compile refusal, not a runtime observation**, and is labelled that way
  above. What the runtime evidence shows is that the counter refuses the `gap` arm with the same fixture.
- **The counter is not a lock-liveness or ownership probe.** It records that a reading went through the
  guard-requiring path and nothing more.
- **No global monotonicity.** `Timestamp::now()` is the host wall clock and can step backwards. Both
  readings are microseconds apart on the host clock, nothing is injected or mocked, and the ordering is
  imposed by the rendezvous rather than by a clock step.
- **No rate.** Three forced runs, one per arm.
- **`io.rs:168`, `io.rs:436` and `ns.rs:176` are untouched** and remain hypotheses; none was reproduced.
- **The historical `7 us` and `9 us` gaps stay unreproduced,** per the clarification.
- **No performance or timing acceptance,** no acceptance threshold, no `SIGKILL` or power-loss claim.
- **`cargo test --workspace` was not run,** and neither were the daemon, `PathVfs`, the FUSE or NFS
  adapters.
- **The PR #139 author's earlier 811-pass figure is not re-executed here.**
- **`no-mistakes` is not initialized** in this repository, `.no-mistakes` and `.claude` are both absent,
  so that pipeline did not run and no claim is made about it.
- **No browser step.** `chromium` is not installed, so any browser work would be **UNVERIFIED** here, and
  this change has no browser surface.
- **`codebase-memory-mcp` graph tools were not used.** The source read was this one file plus the
  crate's existing test seams, read directly.
- **MisakaNet was local-only and was not consulted.** No remote call was made.

## One build failure of my own, recorded rather than dropped

The first attempt to construct the three arms asserted on a `gap` anchor that no longer existed, because
the anchor was written against `park_if_armed();` followed by the lock with the reading already inside
it, and the arm generator was matching text that the fix had moved.
The error was `AssertionError: gap anchor 0`, it happened before any compile, and the fix was to anchor on
the parked call and the guarded call separately rather than on their concatenation.
The mutant generation was then also simplified: an earlier version introduced an extra unguarded twin and
edited the test-side helper, which would have meant the arms differed by more than the clock position.
The final generator moves exactly one line, which is why the diffs above are a single line each.

## Evidence

Under `bench/out/meta42-cache-write-clock-gap-correction/` in the lease, gitignored:

| file | what |
|---|---|
| `lane.sh` | this lane's own script and log path |
| `logs/lane.log` | every acquisition, wait, exit, free and used reading |
| `logs/three-arms.log` | `new` pass, `gap` fail, `old` fail, with the per-arm exit codes and the running `bench/out` total |
| `logs/old-reordered.log` | the `old` arm with the two assertion blocks swapped, failing on the `ctime` comparison |
| `archives-new/`, `archives-gap/`, `archives-old/` | the three extractions, bound to `cd1d5af` |
| `new-target/`, `gap-target/`, `old-target/` and the matching `-tmp` directories | one isolated target and temp directory per arm |

Prior records untouched and immutable: `b5e1f19c…` the permanent-regression record, `72d37cc4…` and
`75a2310b…` the two reviews, `a8bc5337…` the original one-line fix's record, `b1f1940a…` and `b63eb260…`
the hole-flag records.
