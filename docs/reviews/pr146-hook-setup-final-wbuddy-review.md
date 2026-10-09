# PR146 hook-setup fix: final review (wbuddy, READONLY)

Scope: #40 follower-durable-ack ordering kill only; not whole-#40. This file's SHA256 = report SHA. Evidence = real blobs + live refs, never the stale primary `9874afa` graph. Prior review WRONG: claimed "leader established" while actual old-head CI (`e915330`) FAILED both real jobs.

BLOCKERS (first)
- HOLD/REQUEST CHANGES. Runtime UNEXECUTED: READY5 19G > 8G cap; no local cargo/test/build/mutation. Named-test green + executed mutant red on `efb125c` PENDING: run `37543390611` `in_progress` (pending -> stop; no poll/wait/rerun/dispatch). Mutant NOT EXECUTED (red below is analytic).
- Merge vs main `01fa855` is a clean `merge-tree` (mergeable), NOT a verified checkout of the merged tree, so tested-tree = merged-tree is unproven.

SOURCE (real blobs, not the stale primary)
- Head (live refs/pull/146/head) `efb125c8a129df69c9333de4c15d7096500eaa8f`, tree `811f1bc36239df2a845a529cb30e1a3a8fb5b20f`, parent `e915330`, `db.rs` blob `d92102840c329efd12ba41ca3fba3bb166cfd75f`; worktree HEAD == remote head, clean.
- Real main `01fa855fc3521c519e8a93fe0b867dc56ebfdc5d` NOT an ancestor; base `1580e69b`; `merge-tree(1580e69, efb125c, 01fa855)` exit 0, 0 conflicts. Since base, main touched neither `db.rs` nor `follower_durability.rs` (adds are PR144, elsewhere). Primary `9874afa` is behind.
- test-only: +130 `db.rs` cfg(test), +299 `tests/follower_durability.rs`, +34 receipt; production semantics unchanged.

CAUSE + FIX (traced real lines)
- `new_snapshot("s")` -> `add_snapshot` (db.rs:1058, `force_hook=true`) -> `run_hook` (555) fires the hook; `entered_tx.send(())` sits in buffered mpsc and `entered_rx.recv_timeout` (2619) reads it before any leader exists -> old `assert!(*led)` panic in run `37540375872`. Real, from the log.
- Fix: open hook-free (`warm.ack=Durable`, 2583-2588), snapshot, `close` (1029; `force_hook=true` but `before_sync=None` => `run_hook` Ok, 468-473), drop, reopen WITH hook (2603-2606). `open`/`init` runs no commit/`run_hook`, so reopen emits nothing; each `entered_tx` is leader's.

DETERMINISM (vs production control flow)
- Leader `create` under `Ack::Durable`: `mutate` (895) sets `wait_for=Some` (983), no inline commit (`Ack::Applied` only, 949), calls `wait_durable` (987); `*led` false -> sets `*led=true` (1004), `commit` (1012); `run_hook` (555) fires while `*led==true` and before `durable_seq.store` (735). So `assert!(*led)` (2628) and `durable_seq<target` (2633) hold at entry: forced, not incidental.
- Follower calls the REAL private `wait_durable` (2643), reaches `if *led` (1000), blocks on `gc_cv.wait_timeout`; only wake is `notify_all` (737) after the leader commit. No ack before the leader's actual durable_seq at target/reopen; identity bytes untouched.
- 150ms probe (2652) fires only on early return; no sleep is the pass condition. Residual: an unscheduled follower could false-green in 150ms, but that only weakens the kill; the mutant returns immediately, so the intended kill survives it. Bounds: 30s entry 2620, 30s release 2600.

MUTATION (prediction only, NOT executed)
- Canonical mutant at `wait_durable` (1000): replace the `if *led { .. }` body with `return Ok(());` (guard `gc_cv.wait_timeout` under `if false`). Follower (2643) returns at once; 150ms `recv_timeout` (2652) fires; panic (2653) -> RED. Unmutated blocks until release, returns `durable_seq>=target` -> GREEN. Coherent but NOT run: not an executed kill.

REOPEN / INTEGRATION
- Reopen uses `opts()` (2677); `follower_durability.rs` uses per-thread `s{t}`/distinct doc names, disjoint from the unit test's `"s"`/`b"lead"`, non-blocking `yielding_hook`: e2e port, not the gate.

CI (bounded read; no poll/wait/rerun/dispatch)
- Old head `e915330`, run `37540375872`: FAILURE - macos FAIL, ubuntu FAIL, linux-fuse SUCCESS. Fix head `efb125c`, run `37543390611`: in_progress, no conclusion -> stop; fmt/clippy unknown.

VERDICT
- Do not merge/clear #40 on source alone: need (a) named unit test green on `efb125c`; (b) canonical mutant executed red with unmutated green; (c) test the actual merged tree (`merge-base efb125c 01fa855` + both tips), not the tip; (d) fmt/clippy green. Source fix + test construction correct and real; branch genuinely forced. Runtime + mutation separate/unproven; #40 controls open.

Finite, verified; source / runtime / mutation separate.
