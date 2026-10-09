# #40 follower-ordering test: runtime-failure topology spike

Subject: run 37540375872, `cargo test --workspace --lib`. Head
`e91533077baca6f8963d93594f94dbcb87a8a184` (READY5 `test/meta-follower-durability-40`, clean).
`db::tests::follower_wait_does_not_ack_before_the_leader_publishes_durable_seq` FAILED,
`28 passed; 1 failed; finished in 30.47s`. Panic `db.rs:2617:17: a leader must be running for this
to be a follower test`.

## Actual fault (real topology, from the run log, not a theory)

`new_snapshot` is a hook caller: `commit(..., force_hook = true)` (db.rs:1058) then `run_hook`
(db.rs:555). The test set `ack = Durable` and `before_sync = Some(hook)` before `new_snapshot("s")`
(old 2593-2595), so `new_snapshot` fires the hook: `entered_tx.send(())`, then blocks on
`release_rx.recv_timeout(30s)`, which nothing satisfies. `mpsc::channel` is buffered, so the stale
`()` stays queued. `new_snapshot` stalls the full 30s (observed 29.84s), then returns; the leader
thread spawns after. At db.rs:2619 `entered_rx.recv_timeout(30s)` returns instantly on the stale
signal, before the leader set `*led = true`, so db.rs:2617 asserts `*led` and panics. `entered_tx`
was never specific to the leader: the leader enters the follower branch only after `wait_durable`
sets `*led = true` (db.rs:1004), strictly after `new_snapshot` already sent. The premise
presupposed a leader that did not exist yet.

## Same input two ways (falsifier)

Broken (e915330): hook before `new_snapshot` -> 30s stall, `*led == false`, panic. Corrected
(READY5 edit): build the store with `ack = Durable` and no hook, `new_snapshot("s")`, `close()`,
drop, then open with the hook and run the existing leader/follower/assert flow unchanged. Open runs
no `commit`/`run_hook` (db.rs:1344-1350), so only the leader's commit signals; `*led` stays true
because the leader withholds the only wake until `release_tx` at db.rs:2649. Reopen with different
options is established here (db.rs:2111, 2455, 2520, 2666).

## Runtime status: UNEXECUTED on this host

READY5 is over the 8 GiB artifact cap. No local cargo/build/test/mutation was run. Red evidence is
the real CI log above. The fix commit is `efb125c` (pushed to PR146 `test/meta-follower-durability-40`);
its CI is queued. Corrected green and the canonical mutant red are PENDING that run; the named
test green first, then the mutant red. The integration port is unaffected (its `yielding_hook` does
not block). No executed-green or executed-kill claim is made.

## Mutation control (recipe, not run)

Mutant at `wait_durable` db.rs:1000: replace the `if *led {` arm body with `return Ok(());`, guard
`gc_cv.wait_timeout` under `if false`. After the fix: follower returns at once,
`done_rx.recv_timeout(150ms)` fires, db.rs:2641 panics -> red. Unmutated: follower blocks until
release, returns with `durable_seq >= target`. Mutant targets the `wait_durable` the test calls.
