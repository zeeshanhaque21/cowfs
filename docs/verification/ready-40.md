# ready-40: metadata health and recovery accounting (issue #40)

Lease: `.treehouse-ready-wave/.treehouse/cowfs-7c1bf8/5/cowfs`, branch `followup/metadata-health-40`.
Base: `46b0f269d5bef4a2c204c25f5b3015da601d3beb`.
PR: https://github.com/zeeshanhaque21/cowfs/pull/116

Revision 2, after the independent review in `docs/reviews/metadata-health40-final.md`
(sha256 `0807d7f54a007665c641537122e708c5c3d4d71677ce0957b3da823fa45c863a`) requested changes.
The blocker was real and is fixed; the coverage gap is closed.
Source commit `8a6e1ba22cf879b6d7797b8ae2a338ba29fe32b5`, pushed over authenticated HTTPS.
That is the tree every count below was measured at.
The previous PR head was `a1f302353bc829e30d4d09875df9536be5c1ffcd`, whose source commit was
`4366cbbfda32fc4a2003aa36d0872c085aa12c56`; that one is quoted only where a before/after needs it,
and is not the tree under test.

Lane: slot 5, "Metadata recovery counters and health reporting".
Owned source: `crates/cowfs-meta/src/db.rs`, `crates/cowfs-meta/src/lib.rs`,
`crates/cowfs-meta/tests/health.rs`, `crates/cowfs-meta/tests/recovery40.rs`.
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
With a one-write final commit, a 78-page fixture gave **no rollback at all**: all 153 single- and two-page candidates came back either "still opens" or "unrepairable".
The search enumerates `(N-1) + (N-2)` candidates, so 78 pages gives `2N-3 = 153`.
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

Head under test: the source commit below. Counts are from a full `cargo test -p cowfs-meta` at that exact tree, real exit code 0.

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
| `tests/recovery40.rs` | 4 | 0 | 0 |
| `tests/review.rs` | 20 | 0 | 0 |
| doc-tests | 0 | 0 | 0 |
| total | 82 | 0 | 2 |

The two ignored tests were already ignored at the base commit; not new skips.
`cargo clippy -p cowfs-meta --all-targets -- -D warnings` real exit 0.
`cargo fmt -p cowfs-meta -- --check` real exit 0.
All three ran through the wave lock as foreground invocations, each with a private `CARGO_TARGET_DIR`.

### The review blocker, reproduced before it was fixed

Reproduced on the reviewed PR head `a1f3023` source, pristine, no mutant.
Fixture: build with `ino_block = 64`, recover with `ino_block = 4`.

```
SPIKE built=64 recovered=4 ino_floor=6 max_handed=65
SPIKE reused_count=60 reused=[6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17]
health=Health { last_flush_error: None, flush_failures: 0, ... recoveries: 1, poisoned: false }
```

60 inode numbers handed out twice, `Health` reporting a healthy store, no error and no warning.
The spike file is deleted; the same shape is now a permanent test.

### The fix

`ino_block` is persisted in the `meta` table when the file is created, validated on open
(`clamp(1, INO_LIMIT)`, and a stored zero is `corrupt`), and the **stored** value governs the
allocator and `record_recovery`, exactly as the stored `node_size` already did.
A caller's `ino_block` is now ignored on an existing file.

A file written before the key existed has no provable bound, so `record_recovery` refuses with a
reason instead of guessing.
That is the `node_size` precedent again: validate, persist, and let the stored value govern.
Refusing to recover is recoverable; re-issuing a number is not.

### Load-bearing coverage: four isolated builds

Every variant ran in its own `CARGO_TARGET_DIR`, with the source sha256 printed before the build so
a stale binary cannot be mistaken for a result.
`cp -R` preserving mtimes is a known trap here; the receipts below are what makes the reading safe.

| Build | `db.rs` sha256 (first 16) | persisted key | `+block` | `+1` | Result |
| --- | --- | --- | --- | --- | --- |
| shipping | `de79c2713fbb0148` | yes | yes | yes | **0**, 4 passed |
| PR head `a1f3023` | `4d26d725527c2454` | no | yes | yes | 101 |
| no inode bump | `a2f62c568a395e9f` | yes | **no** | yes | 101 |
| no snapshot bump | `bfae9acf6aa21591` | yes | yes | **no** | 101 |
| neither bump | `816c04d8004add9b` | yes | **no** | **no** | 101 |

Which test killed which, read from the output rather than inferred:

| Build | Killed by | Message |
| --- | --- | --- |
| PR head | 3 of 4 | `re-issued 60 pre-crash numbers`, plus `the current build must persist the block` |
| no inode bump | 2 of 4 | `re-issued 64 pre-crash numbers` |
| no snapshot bump | 1 of 4 | `snapshot id 2 was handed out twice` |
| neither bump | 3 of 4 | both messages above |

The old source fails these tests for the right reason: the same fixture that passes on the shipping
source re-issues 60 numbers on it.

### Why the old tests passed the mutants

The shipped `health.rs` rollback fixture lost a commit that never moved either counter: healthy
`ino_floor` was 450 and the value stored after the rollback was still 450; healthy `snapshot_floor`
was 3 and the stored value was still 3.
Its assertions were satisfied by `Tx::alloc_ino`'s own headroom, because `alloc_ino` reserves before
handing out, so `reserved` is always at least one above the highest number handed out.
The bump and the `+1` were untested.

The replacement fixture makes the newest durable commit **a reservation**: arm the hook to fail
after the snapshot exists, so every main commit fails while every `reserve_durable` still commits,
since it runs no hook. One batch keeps `pending_ops` at 1, under the backlog cap.
One rollback then undoes exactly that reservation, which is the only shape in which the bound is
observable.

Assertions are on handed-out sets and read-back state, never on floor headroom:
the actual inode numbers before and after, disjointness, a fresh reopen, and `lookup` through a new
handle so the file is what is checked.

### The other review findings

- **Inline `Ack::Applied` hook panic.** It escaped to the caller and left `Health` at zeros. Now the
  panic is recorded, `flush_err` is set, and then it is re-raised, so the caller still sees it.
  Safe to catch here for the same reason the background job may catch it: `run_hook` runs before
  `begin_write`, so no transaction is left open.
  This is a scoped panic contract on that one boundary, not a general policy of treating a panic
  like a background failure.
- **`consecutive_flush_failures` doc overstated the behaviour.** The refusal in `mutate` is gated on
  the session's `flush_err`, which the panic path never set. The doc now says the counter counts and
  does not by itself refuse anything, and points at `last_flush_error`.
- **Reap `Err` arm.** Previously it recorded the failure but did not re-arm, so reaping still
  stopped, only more visibly. It now re-arms on `Ok(Err(e))` as well, with the same 300 us pause that
  bounds a persistently failing reap. The silent stop is gone in both the panic and error paths.
  Still **not** proved by a fault-injection test; labelled as reasoned from the code.
- **Doc transcript line number** corrected from `health.rs:257` to the actual `268`, and the damage
  candidate count from 155 to 153 with the `2N-3` derivation.
- **Provenance.** Source commit and doc commit are named separately below; the counts above were
  measured at the source commit, not inferred from the earlier PR head.

## Residual criteria, and who owns them

- **M2**, one transient corrupt read poisons the whole handle and the pending window is discarded on drop. `Inner::note` sets `poisoned` on any `Error::Corrupt`; `finish_on_drop` discards pending changes if the hook fails. Not reproduced, not touched, because a partial fix would be speculative. Exact owner: `Inner::note`, `Inner::check_writable`, `Inner::finish_on_drop` in `crates/cowfs-meta/src/db.rs`. Same file this lane owns, so the natural next change here once reproduced.
- **`Health.poisoned`** is surfaced, not fixed. `Inner::poisoned` had no test in `cowfs-meta` before this change; `rg -n 'poison' crates/cowfs-meta/tests/` matched nothing outside the new file. The control test only proves it reads `false` on a healthy store. Proving it needs a fixture whose read returns `Error::Corrupt` without failing the whole open.
- **The reap arm of `bg_main`** now re-arms on `Ok(Err(e))` as well as recording it, so reaping no longer stops silently in either the panic or the error path. Still **not** proved by a fault-injection test: found by reading the adjacent arm, not reproduced, so it is not claimed as proved. Proving it needs a fixture whose reap step genuinely fails.
- **Real power-loss behaviour is not established.** Everything here is redb's own repair path
  driven by page damage on a closed file. That models a lost commit; it is not a power cut, and a
  surviving process is not power loss. The crash harness question from the issue is still open.
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
