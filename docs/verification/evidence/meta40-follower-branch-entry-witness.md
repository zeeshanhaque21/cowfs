# #40 follower branch-entry witness (PR146, head `bdcbfcd`)

Scope: the follower-durable-ack ordering kill only.
New head `bdcbfcd6e08eaa906c0c75aa32de38b7f217b97e` (`test/meta-follower-durability-40`), parent `efb125c8`, pushed HTTPS.
One file changed: `crates/cowfs-meta/src/db.rs`, +74/-11, all additions `#[cfg(test)]`-gated.

## Witness (real branch, no sleep)
- `#[cfg(test)] thread_local! static WAIT_WITNESS: RefCell<Option<mpsc::Sender<WaitWitness>>>`, mirrors the existing `RESERVE_*` probes.
- Real `wait_durable` emits `WaitEntered` inside the `if *led` branch, before `gc_cv.wait_timeout`; emits `Returned` at the real `return Ok(())`.
- `watch_wait_durable()` installs the sender on the follower thread; the follower hands its receiver to main before calling `wait_durable` (thread-local must be set on the emitting thread).
- Test: first witness event must equal `WaitEntered`; the harness emits `Returned` on actual return, so a skip is a hard, scheduling-independent fail.
- Removed the 150ms `done_rx.recv_timeout` negative wait. Kept the `durable_seq >= target` (`saw >= target`) return check and the reopen durability assert.

## Canonical mutant prediction (UNEXECUTED)
- `if *led { .. }` body replaced by `return Ok(());` -> skips the `WaitEntered` emission; harness `Returned` is the first event -> `assert_eq!` fails.
- Analytic only. Mutant NOT executed; no runner dispatched.

## Evidence run
- Static: `rustfmt --edition 2021 --check` CLEAN. Non-test `wait_durable` byte-identical to HEAD after stripping `#[cfg(test)]` lines.
- Pattern repro (standalone `rustc`): REAL -> Ok; MUTANT -> `Err(first=Returned)`; spurious-wake -> first stays `WaitEntered`, terminal `Returned`.
- Runtime: CI on `bdcbfcd` PENDING (0 completed) at read time. No local cargo (READY5 over cap). Runtime UNEXECUTED.

## Remaining #40
- Named test green + canonical mutant red on the new head: PENDING CI.
- Other #40 items (health signal, open_recover counters) untouched.
