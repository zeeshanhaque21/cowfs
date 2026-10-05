# ready-40: metadata health and recovery accounting (issue #40)

Lease: `.treehouse-ready-wave/.treehouse/cowfs-7c1bf8/5/cowfs`, branch `followup/metadata-health-40`.
Base: `46b0f269d5bef4a2c204c25f5b3015da601d3beb`.
Commit: `4366cbbfda32fc4a2003aa36d0872c085aa12c56`, pushed over authenticated HTTPS.
PR: https://github.com/zeeshanhaque21/cowfs/pull/new/followup/metadata-health-40

Lane: slot 5, "Metadata recovery counters and health reporting".
Owned source: `crates/cowfs-meta/src/db.rs`, `crates/cowfs-meta/src/lib.rs`, `crates/cowfs-meta/tests/health.rs`.
No `CHANGELOG.md` edit, no workflow or runner change, no merge, no lease return.

## What issue #40 asks, and what this lane did with it

Issue #40 is a list of round-2 critic findings on `cowfs-meta`.
This lane covers the two the dispatch table names, the health signal and the `open_recover` counters, plus the second half of M1.
The rest is reported as residual rather than touched.

| Finding | Text | This lane |
| --- | --- | --- |
| M1 | a panic in `before_sync` kills the background timer thread silently; fix with `catch_unwind` around `timer_flush` plus a `last_flush_error()` health signal | fixed and proved |
| M1b | no health signal for background failures or repeated failing flushes under `Ack::Applied` | fixed and proved |
| M3 | `open_recover` rolls back the inode and snapshot-id counters, so numbers handed out after the lost commit could be reused | fixed and proved |
| M2 | one transient corrupt read poisons the whole handle; the pending window is discarded on drop | not touched, reported |
| M4 | an empty splice at a non-boundary offset returns Ok and bumps the version | not touched, other lane |
| M5 | `ino_block = u64::MAX` panics in debug | not touched, slot 6 owns the inode allocator |
| M6 | zero-length `ChunkRef`s are accepted and silently collapse | not touched, slot 6 owns the hole flag |
| Core requests | atomic `rename_snapshot`, batch timestamps, hole flag on `ChunkRef` | not touched, slot 6 / #42 |
| mutant gaps | port the critic's mutation harness | not touched, see residual |
| crash harness | re-run against the real `cowfs-store` | not touched, `cowfs-meta` lane |
| docs | lead the performance table with durable and random-access figures | not touched, see residual |

## Reconciliation: what already existed

The instruction was to reconcile already-landed health fields and counters before implementing anything new.
This was already in the tree at the base commit:

- `Session::flush_err` already existed and already gated a write refusal: `Inner::mutate` refuses more changes once `pending_ops > sync_every_ops * 8` while `flush_err` is set.
- `flush_err` was written in exactly two places, the inline `Ack::Applied` commit in `mutate` and the background `timer_flush`.
- `Inner::poisoned` already existed and already made `check_writable` refuse writes after corruption.
- `Recovery { rolled_back, backup, snapshots }` already existed and already reported what a rollback did.
- `reserve_durable` already existed and already made `ino_reserved` a durable high-water mark below which no number is handed out.
- `InoAlloc` was already documented as "Hands out inode numbers below a durable high-water mark".

So the store had the mechanisms and none of the observability: every one of those fields was private to the crate, and no counter was reachable from a caller or a test.

## What was missing, precisely

M1, read against the code:

```rust
// before, crates/cowfs-meta/src/db.rs, bg_main
match job {
    Job::Flush => inner.timer_flush(),          // no catch_unwind
    Job::Reap => { if matches!(catch_unwind(..), Ok(Ok(true))) { .. } }
}
```

`Job::Reap` already had a `catch_unwind`. `Job::Flush` did not.
A panic inside `before_sync` therefore unwound out of `bg_main`, the `cowfs-meta-bg` thread died, and since `bg_main` was the only reader of the armed deadline, nothing was ever flushed again.
`Session::flush_err` stayed `None`, because the panic skipped the line that sets it, so `mutate`'s backlog cap never engaged either.
A caller asking whether its change was durable got silence.

The reap arm had the same defect one line over: `matches!(.., Ok(Ok(true)))` is false for `Ok(Err(e))`, so the reap flag was never re-armed, reaping stopped, a removed snapshot's space never came back, and nothing reported why.

M3, read against the code:

```rust
// before, crates/cowfs-meta/src/db.rs, open_recover, the rolled-back path
Ok((m, Recovery { rolled_back: true, backup: Some(backup), snapshots }))
```

The rollback returns the store to the previous commit, and `ino_reserved` and `next_snapshot` go back with it.
`Meta::open` had already recorded `ino_reserved` durably *before* handing out any number in that range, so the recovered value is a sound floor for the recovered state, but it is below the floor the pre-crash file had reached.
Nothing moved it forward, and nothing recorded that a rollback had happened at all.

## The fix, and why one block is the provable bound

`Inner::record_recovery` runs one durable commit after a successful rollback writing `ino_reserved`, `next_snapshot` and a new `recoveries` key.
Like `reserve_durable` it carries no chunk references, so it runs no hook.

The bound it adds is one `ino_block` and one snapshot id, provable from three facts already in the code:

1. `Tx::alloc_ino` reserves `next + block` through the `reserve` closure when `next >= reserved`, and `reserve` is `Inner::reserve_durable`, its own `begin_write` / `commit`. One commit therefore advances `ino_reserved` by at most one block.
2. A single commit hands out at most `block` numbers after that reservation, the highest being `reserved - 1`. Every number the lost commit could have handed out is below `recovered_ino_reserved + block`.
3. `Extra::Add` creates at most one snapshot per commit, so the lost commit advanced `next_snapshot` by at most one.

redb's repair falls back exactly one commit slot, which makes "one lost commit" the right unit.
The recovered floor plus those bounds clears everything the lost commit could have handed out, so a number can now be wasted but never handed out twice.
`close` lowers `ino_reserved` to the exact next unused value, which does not weaken this: a lower recovered floor is still below every number the recovered state used.

`record_recovery` failing is fatal to `open_recover`, and deliberately does not restore the backup: by then the file is already repaired, so restoring would put the corrupt bytes back.
Handing out a reused inode number is worse than refusing to open, so it fails closed and names the backup in the error.

## What was added, additively

No existing signature changed, no `Error` variant was added, and no exit contract exists to break.
`open_recover` has no caller outside `cowfs-meta`:

```
$ rg -n 'open_recover' crates/ --glob '!crates/cowfs-meta/**'
(no output)
```

New public surface:

- `Meta::health() -> Health`, infallible and pollable.
- `Health { last_flush_error, flush_failures, consecutive_flush_failures, background_panics, poisoned, recoveries, ino_floor, snapshot_floor }`.
- `Recovery` gained `recoveries`, `ino_floor: Option<u64>`, `snapshot_floor: Option<u64>`, all zero or `None` when `rolled_back` is false.
- `RECOVERY_FAILED`, the stable prefix of the `Error::Storage` message a failed `open_recover` returns.

Why no new `Error` variant: `crates/cowfs-core/src/error.rs` matches `cowfs_meta::Error` exhaustively with no wildcard arm, so a variant would break a crate this lane does not own and slot 6 may be editing.

`Health.poisoned` is surfaced from the pre-existing `Inner::poisoned` flag, not newly fixed; see residual.

### Health fields and exactly what increments them

| Field | Incremented by |
| --- | --- |
| `flush_failures` | any failed durable commit: the background flush timer, a background reap step, the inline `Ack::Applied` commit, `sync`, `close`, the durable wait |
| `consecutive_flush_failures` | the same, reset to 0 by any successful commit |
| `background_panics` | a panic caught from the background flush or the background reap |
| `last_flush_error` | the most recent of the above; sticky, never cleared by a later success |
| `recoveries` | read from the file's `recoveries` key, so it survives a reopen |
| `ino_floor`, `snapshot_floor` | in-memory `ino.reserved` and `next_snapshot`, which `record_recovery` raises |
| `poisoned` | pre-existing `Inner::poisoned`, not changed here |

`last_flush_error` is sticky on purpose: it mirrors `Core::last_flush_error`, and clearing it on the next success would hide the stall an operator is looking for.

## The tests

`crates/cowfs-meta/tests/health.rs`, seven tests.
Every fixture is a private `tempfile` store, built and closed in-process: no daemon, no live store, no mount, no signal, therefore no PID to verify and nothing to kill.
The shared daemon, its store, its sockets and the mounted pools were never touched.
Damage is applied only after the store is closed, and each test keeps a pristine copy asserted byte-identical at the end.

| Test | Proves | Baseline |
| --- | --- | --- |
| `background_flush_survives_a_panicking_sync_hook` | a panicking hook is caught, the timer re-armed, the retry makes the change durable, reason and panic count reported, store still takes writes | control below |
| `a_store_that_never_failed_reports_nothing` | a healthy background store reports `None` and all zeros | the 0 baseline |
| `repeated_flush_failures_are_counted_reported_and_eventually_refused` | 9 applies then a refusal, `flush_failures >= 8`, `consecutive == total`, the hook's own message surfaced, 0 panics | 0 baseline above |
| `a_rollback_never_hands_out_a_number_it_already_handed_out` | after a real rollback, 40 new files and a new snapshot reuse none of the pre-crash numbers, floors cleared the highest pre-crash numbers, `check()` passes, a plain reopen still reports `recoveries == 1` with floors not going backwards | control below |
| `opening_a_healthy_file_reports_no_recovery` | nothing lost means `rolled_back == false`, `recoveries == 0`, both floors `None`, no backup, floors unmoved | the 0 baseline |
| `a_failed_recovery_is_reported_and_restores_the_file_byte_for_byte` | failure marked `RECOVERY_FAILED`, file restored byte for byte, and the file still fails closed afterwards so it can never look clean | 0 baseline above |
| `repeated_rollbacks_keep_counting_and_keep_moving_the_floors` | two rollbacks give `recoveries` 1 then 2, and both floors move forward the second time | 0 baseline above |

`check()` passing is asserted but is explicitly not the proof.
The rollback tests assert `check()` and then go on to assert the floors and the numbers actually handed out, because a consistent recovered tree says nothing about whether a reused number was handed out.
That is why the failed-recovery case is its own test with its own assertions, per the requirement that a clean fsck cannot prove a failed recover was marked truthfully.

### Damage is not a fixed offset

Tail truncation is useless here: it destroys redb's region layout, and every recovery then fails with "File length does not correspond to a valid region layout", which proves nothing about a lost commit.
Measured on a real 327680-byte fixture:

| Damage | `Meta::open` | `open_recover` |
| --- | --- | --- |
| truncate 1 byte | Corrupt | recovery failed, file restored |
| truncate 512 bytes | Corrupt | recovery failed, file restored |
| a page of the newest commit | Corrupt | rolled back |
| a page of the shared superblock | Corrupt | recovery failed, file restored |

So `damage()` searches pages of a scratch copy, in a fixed order, for one producing the verdict the test needs, then writes that one page to the fixture.
The search runs entirely on scratch copies, so the fixture is written once with damage already known to work.
If a redb upgrade changes the layout so no page produces the wanted verdict, the test panics with the full list of what it tried rather than quietly passing.

### Three fixture properties, each forced by a measured failure

The final commit must be a large batch.
With a one-write final commit, a 78-page fixture gave **no rollback at all**: all 155 single- and two-page candidates came back either "still opens" or "unrepairable".
redb keeps two commit slots and shares every page the newer commit did not rewrite, so a small last commit leaves almost every page belonging to both slots and no page damageable in isolation.
A final batch of 400 files gives the newest slot pages of its own.
Consequently damage must target a **copy** taken while that batch is newest, because `close` and `drop` each commit once more and would put a small commit back on top.

The health baseline is sampled after the last sync and before any close.
`close` lowers `ino_reserved` to the exact next unused number, so an earlier baseline is not what a reopen reports.
This surfaced as a real failure first: baseline 14, reopened 11.

The reopen assertions are monotonic, not equal.
The recovered store takes 40 more writes before closing, so the floor legitimately advances and `close` lowers it only to the next unused number.
What must never happen is it going backwards, which would mean the recovery record was lost.

The durability signal is the durable snapshot **root**, not the snapshot count.
Creating a file does not add a snapshot, so the count stays at 1 while a background flush commits; a probe confirmed the root moves within one timer interval and the count never does.

## Result

`cargo test -p cowfs-meta` at `4366cbbf`, real exit code 0, 78 passed, 0 failed, 2 ignored.

| Suite | Passed | Failed | Ignored |
| --- | --- | --- | --- |
| unit (`src/lib.rs`) | 16 | 0 | 0 |
| `tests/corrupt.rs` | 1 | 0 | 0 |
| `tests/crash.rs` | 2 | 0 | 1 |
| `tests/critic.rs` | 12 | 0 | 0 |
| `tests/health.rs` | 7 | 0 | 0 |
| `tests/kill9.rs` | 2 | 0 | 0 |
| `tests/model.rs` | 2 | 0 | 0 |
| `tests/posix.rs` | 16 | 0 | 0 |
| `tests/review.rs` | 20 | 0 | 1 |
| doc-tests | 0 | 0 | 0 |
| total | 78 | 0 | 2 |

The two ignored tests were already ignored at the base commit; not new skips.
`cargo clippy -p cowfs-meta --all-targets -- -D warnings` real exit 0.
`cargo fmt -p cowfs-meta -- --check` real exit 0.
All three ran through the wave lock as foreground invocations.

### Old fail, new pass, for M1

`bench/out/ready-40/toggle_bg_panic_fix.py` reverts only the `Job::Flush` arm, runs the one M1 test, restores the arm, runs it again.

| Step | Real exit |
| --- | --- |
| pre-fix `Job::Flush => inner.timer_flush(),` | 101, `test result: FAILED. 0 passed; 1 failed` |
| fixed arm restored | 0, `test result: ok` |

The failure is the M1 symptom, not an incidental one:

```
thread 'cowfs-meta-bg' panicked at crates/cowfs-meta/tests/health.rs:257:17:
thread 'background_flush_survives_a_panicking_sync_hook' panicked at health.rs:49:5:
timed out after 20s waiting for the background commit after the panicking hook
```

The thread panicked in the hook, died, and the change was never flushed.
The 20 s bound is what separates a dead timer thread from a slow one.

### Lock contention

Heavily contended. Across the logs in `bench/out/ready-40/`, 8 attempts returned exit 75 (lane busy) and were retried, which is the documented behaviour and not a permission slip; every log records its own `real_exit`.

## Residual criteria, and who owns them

- **M2**, one transient corrupt read poisons the whole handle and the pending window is discarded on drop. `Inner::note` sets `poisoned` on any `Error::Corrupt`; `finish_on_drop` discards pending changes if the hook fails. Not reproduced, not touched, because a partial fix would be speculative. Exact owner: `Inner::note`, `Inner::check_writable`, `Inner::finish_on_drop` in `crates/cowfs-meta/src/db.rs`. Same file this lane owns, so the natural next change here once reproduced.
- **`Health.poisoned`** is surfaced, not fixed. `Inner::poisoned` had no test in `cowfs-meta` before this change; `rg -n 'poison' crates/cowfs-meta/tests/` matched nothing outside the new file. The control test only proves it reads `false` on a healthy store. Proving it needs a fixture whose read returns `Error::Corrupt` without failing the whole open.
- **The reap arm of `bg_main`** changed from silent-swallow to recorded. Same defect class as M1, found by reading the adjacent arm rather than reproducing it, so it is **not** claimed as proved. Three lines, purely additive: a failed reap commit increments the flush counters and sets the reason.
- **Mutant harness** not ported. `crates/cowfs-meta/tests/critic.rs` already has a crash harness that shreds a serialized image and drives `open_recover`; the missing piece is a harness that deletes individual implementation lines and asserts the suite goes red. That needs a stable assertion set across the whole crate, so it belongs with the builder-suite owner.
- **Real-`cowfs-store` crash re-run** is out of scope for a `cowfs-meta` lane.
- **Performance table ordering** in `docs/v1-meta.md` is a docs edit in a design document this lane did not otherwise touch.

## Overlap with other lanes, reported rather than duplicated

- Wiring `cowfs_meta::Health` into `cowfs_core::Health` is deliberately not done. `cowfs-core` has its own `Health { files, lanes, last_error }` and `last_flush_error`, consumed by `crates/cowfs-core/tests/critic2b.rs`. The seam would be `Inner::health()` in `crates/cowfs-core/src/inner.rs`, next to the existing `inner.last_error`, which is not this lane's surface and which slot 6 / #42 may be editing.
- No CLI or daemon status surface consumes `Recovery` or `Health` today, because `open_recover` has no caller outside `cowfs-meta`. So there was no exit contract to keep compatible and no existing status output to extend. Surfacing the recovery count in `cowfs-cli` would be a new consumer, not a change to an existing one.
- Issue #98's warm-base provenance seam belongs to slot 10, and #42's Core/meta API residuals to slot 6. Neither was touched.

## Resource and safety record

- Free disk before the work: 728 GiB; 421 GiB at the last check, against a 20 GiB floor. Owned artifacts stayed far under the 8 GiB cap.
- Scoped to `cargo test -p cowfs-meta`, `cargo clippy -p cowfs-meta`, `cargo fmt -p cowfs-meta`. No full-workspace build, no stress suite, no corpus copy.
- Every build and test ran as one foreground invocation through `.treehouse-ready-wave/mac-heavy.lock` with the 600 s wait from the updated dispatch doc.
- The shared daemon PID 15263, its store, its sockets and the mounted pools were never signalled, restarted, reset or garbage-collected. No daemon was launched at all.
- Only private `tempfile` fixtures were damaged, always after the store was closed, always with a pristine copy asserted unchanged afterwards.
- Raw logs are in `bench/out/ready-40/`, which `.gitignore` already excludes via `/bench/out/`.
