# PR 140 required-proof failure repair: T10 floor ordering and T11 output visibility

Lane: READY5 follow-on repair for PR #140, branch `fix/meta-inode-reservation-42`.
Repair head: `d8080553bf906db7f739c240b7607f55185aa1f5`.
Failed head: `72f19f961fcb0fc8c4b780a1dc43e6799639846f`, mirrored at `37c197b57934b42612081fa45232d0372d28b695`.
Review it answers: `docs/reviews/pr140-required-proof-wbuddy-review.md`, SHA-256
`d38f6b2548d7751ee87a0b3fb75993e44034d669c53b4ef1d9d598cfdf656e80`.

## Immutability

This receipt supersedes nothing and rewrites nothing.
The earlier receipts, in order, stay byte-for-byte as they were:

- `meta42-large-inode-reservation.md`
- `meta42-large-inode-reservation-ci-repair.md`
- `meta42-inode-reservation-correction.md`
- `meta42-residual-verification.md`
- `meta42-reservation-required-proof.md` (SHA-256 `a49b23f331572a51150a0f23e7f08f0fe9845168d12953515bb8ea8da0cdd6a4`)
- the review `pr140-required-proof-wbuddy-review.md` (SHA-256 `d38f6b25...`)

One claim in the receipt above this one was wrong and is corrected here rather than edited.
`meta42-reservation-required-proof.md` says execution is "expected on normal CI".
The bounded completed run `37516750404` for `72f19f9` shows `check (ubuntu-latest)` and
`check (macos-latest)` both FAILED on the new T10 test.
That receipt's expected-green was optimistic and is not true; this receipt records the actual
outcome and the repair.
This is the explicit immutable correction the assignment requires; the old receipt is untouched.

## What actually ran and what it showed

The run at `72f19f9` (identical test code to `37c197b`) is the one to read, and it is complete.
`gh run view 37516750404 --repo zeeshanhaque21/cowfs --log-failed`:

```
test db::tests::open_recover_keeps_a_reservation_bound_across_a_real_rollback ... FAILED
thread 'db::tests::open_recover_keeps_a_reservation_bound_across_a_real_rollback'
  (14974) panicked at crates/cowfs-meta/src/db.rs:2444:9:
assertion `left == right` failed: and so is the recovered floor
  left: 1000406
 right: 1000402
```

T9, T9b and T11 PASSED in the same run.
The macOS job failed on the same test with the same numbers.
The only failing test is T10, and its failure is the one line above.

## Root cause: a test-ordering bug, not a production bug

Line 2444 was `assert_eq!(again.health().ino_floor, floor, "and so is the recovered floor")`,
where `floor` was captured at recovery time from `rec.ino_floor`.

The test body did this, in order:

1. `open_recover` returned `rec.ino_floor = 1000402`, which equals the reserved range end.
2. The assertion `m.health().ino_floor == floor` right after recovery PASSED, so the store agreed
   with the recovery object at that point.
3. The test then called `m.reserve_inodes(4)`. A reservation advances the durable floor to the end
   of the new range, so `ino_reserved` became `1000402 + 4 = 1000406`.
4. `m.check()`, `drop(m)`, reopen.
5. The reopened `health().ino_floor` read the durable `ino_reserved`, which is `1000406`.
6. The assert compared `1000406` (reopened) against `1000402` (the pre-reservation recovery floor).

The left value is exactly `right + 4`, which is the reservation the test itself made between
recovery and reopen.
The test advanced the floor and then demanded it had not moved.
The production behaviour is correct: the floor rose by exactly the numbers reserved, and both the
recovery floor and the post-reservation floor are past the reserved range end.

So this is a wrong assertion in the test, not a defect in the reservation or recovery code.
There is no production bug and no production change here.

## The repair

Two changes, both inside the test, both tests-only.

### T10: correct ordering, and pin the lost commit

The reopened check is now monotonic instead of an equality against a stale floor:

```
m.check().unwrap();
// This reservation raised the durable floor; the reopened floor must be at or above it,
// never below the recovered value.
let raised = next.end().0;
drop(m);

let again = Meta::open(&path, opts()).unwrap();
assert_eq!(again.health().recoveries, 1, "the count is durable");
assert!(
    again.health().ino_floor >= raised,
    "the reopened floor ({}) must not fall below the floor after recovery ({raised})",
    again.health().ino_floor
);
```

The load-bearing invariants are untouched:

- `floor >= original.end().0`, the reservation is never reissued,
- no reuse on the handle or after a plain reopen,
- `rolled_back` and `recoveries == 1` are the recovery signal,
- survivors and `check()` still pass.

The review also asked which commit was lost, and to not assume newest-only.
The repair now pins it:

```
assert_eq!(
    floor,
    original.end().0,
    "the rollback lost exactly the floor move: the recovered floor lands on the bound"
);
```

This is a discriminating observable, not a guess.
`reserve_inodes` writes the bound first (`ino_reserved_intent = end`) and then, in the next commit,
moves the floor and spends the bound.
`record_recovery` prefers the bound when one is present.
So:

- a rollback of only the floor move leaves the bound and yields `floor == end`,
- a rollback that also lost the bound falls back to `old_floor + block`, which is strictly below
  the end, and this assert fires.

Equality therefore fails loudly if the damage ever selects a deeper rollback than the newest
commit, instead of quietly asserting a weaker `>=`.
The earlier receipt's claim that a one-commit rollback lands on the bound is now the assertion
that proves it, per run.

### T11: make the timing visible in normal CI

T11 passed but printed nothing, because libtest captures the Rust-level `std::io::stdout` and
shows it only on failure.
`println!` in a passing test produces no CI output.

The repair writes to the process's real stdout instead, with a fallback:

```
fn emit(line: &str) {
    match std::fs::File::options().write(true).open("/dev/stdout") {
        Ok(mut f) => {
            let _ = f.write_all(line.as_bytes());
            let _ = f.write_all(b"\n");
        }
        Err(_) => println!("{line}"),
    }
}
```

`/dev/stdout` resolves to fd 1 on both Ubuntu and macOS, which is the descriptor CI collects.
It is standard library only, no new dependency, no `libc`, no unsafe.
If the device cannot be opened the test still passes and falls back to `println!`, so the
measurement never fails for want of a sink.

No timing assert was added, no threshold, no workflow or CI edit, and the 1.5x filesystem
acceptance gate is untouched.
The printing is confined to the timing test; no other test emits anything.

## Status of each proof, source versus execution

| proof | source at `d808055` | execution |
| --- | --- | --- |
| 1, real `open_recover` rollback | repaired, committed | T10 failed at `72f19f9`; repaired source not yet run |
| 2, `INO_LIMIT` boundary | unchanged | PASSED at `72f19f9` on ubuntu and macos |
| 3, measured latency | emit repaired, committed | PASSED at `72f19f9`; values not visible in that log |

Source proof and execution proof are distinct.
Proofs 2 and 3 have a completed green run on the failed head; the T10 repair does not yet.
No CI run, rerun, dispatch, trigger, workflow or runner change was made for this receipt.
The repair push starts a normal CI run on `d808055`; this receipt records the source and the
observed failure, and does not claim the repaired head is green until its own run says so.

## What is proved, and what is still open

Proved on the observed run at `72f19f9`:

- the new tests compile inside `#[cfg(test)]` in an ordinary workspace build,
- T9 and T9b pass, so the `INO_LIMIT` ceiling is exercised, not merely asserted in arithmetic,
- T11 passes and completes, so the timing harness runs the full reservation plus close/reopen and
  the bounded comparison without a flaky assert,
- T10 reaches real `open_recover` on a damaged file and rolls back, since it failed at a point
  after `rolled_back`, `recoveries == 1`, and the immediate post-recovery `health().ino_floor`
  equality had already passed, which means `rec.ino_floor == the reserved range end == 1000402`
  was observed on real Ubuntu and macOS runs.

Still open at this head:

- the repaired T10 has not run; its own CI run must confirm the monotonic reopen check and the new
  equality-to-end after recovery,
- the T11 timing values have not yet appeared in a log; the next run must show the
  `inode reservation timing:` lines before any number is claimed,
- a published timing figure is not the 1.5x filesystem acceptance result; it is one measured
  data point at unknown load and is not a global speed pass.

No execution claim, no timing number and no green-CI claim is made for `d808055` in this receipt.
They are awaited from its normal CI run.

## Commit and remote

```
$ git log --oneline -3
d808055 test(meta): repair T10 floor ordering and expose T11 timing to CI
37c197b docs(meta): mirror the PR 140 required-proof receipt onto the branch (#42)
72f19f9 test(meta): prove the reservation bound across a real open_recover rollback
$ git ls-remote https://github.com/zeeshanhaque21/cowfs.git refs/heads/fix/meta-inode-reservation-42
d8080553bf906db7f739c240b7607f55185aa1f5  refs/heads/fix/meta-inode-reservation-42
```

One file changed, tests only: `crates/cowfs-meta/src/db.rs`.
No production, Core, Vfs, store, NFS, ctl, daemon, CI, manifest or dependency change.
