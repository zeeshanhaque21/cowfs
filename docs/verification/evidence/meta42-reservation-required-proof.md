# PR 140 required proof: the reservation bound across a real rollback, the inode ceiling, and measured latency

Lane: READY5 follow-on proof for PR #140, branch `fix/meta-inode-reservation-42`.
Head proved: `72f19f961fcb0fc8c4b780a1dc43e6799639846f`.
Parent: `573b02f5e069f1e52bc32a11f2da4ce4ec8083c4`.
This receipt supersedes nothing.
The older receipts `meta42-large-inode-reservation.md`, `meta42-large-inode-reservation-ci-repair.md`,
`meta42-inode-reservation-correction.md` and `meta42-residual-verification.md` stay immutable.
The owed items are the three "still owed" claims in
`docs/verification/evidence/meta42-large-inode-reservation-ci-repair.md` and the review
`docs/reviews/pr140-repaired-reservation-final-wbuddy-review.md` (lines 167-175).
This receipt answers exactly those three and no more.

## What this lane may run, and what it actually ran

Two different constraints met on this lane and only one of them is real.

The older receipts cite an "8 GiB cap" over `bench/out`.
That is not a disk cap and it is not enforced in this repository.
There is no `[lints]`, `Cargo.toml` or `.cargo/config.toml` rule for it, and free space is 224 GiB:

```
$ df -h /Users/zeeshanhaque | tail -1
/dev/disk3s5   1.8Ti   1.6Ti   224Gi    88%  /System/Volumes/Data
$ du -sh bench/out target
 18G  bench/out
851M  target
```

The real constraint is a **shared heavy-command lane** on this Mac.
`/Users/zeeshanhaque/Projects/cowfs/.treehouse-ready-wave/mac-heavy.lock` is held by the READY7
worker for a `cargo test -p cowfs-nfs` run, and the assignment forbids taking shared resources:

```
$ ls -la /Users/zeeshanhaque/Projects/cowfs/.treehouse-ready-wave/mac-heavy.lock
-rw-r--r--@ 1 zeeshanhaque  staff  0 Oct  5 17:42 mac-heavy.lock
$ pgrep -fl 'cargo|rustc'
93622 /Users/zeeshanhaque/.rustup/toolchains/stable-aarch64-apple-darwin/bin/cargo test -p cowfs-nfs
```

So on this lane: **no `cargo build`, `cargo test`, `cargo clippy`, no `git archive`, no probe binary,
no new target directory, and no deletion, move, offload or waiver.**

The only executable check available is a standalone `rustfmt`, already installed.

```
$ rustfmt --edition 2021 --check crates/cowfs-meta/src/db.rs
rustfmt exit=0
```

`rustfmt 1.10.0-stable (b940084d7e 2026-09-28)`, workspace edition 2021, no `rustfmt.toml`.
The single owned file is clean.

**That is a formatter result and nothing more.**
It says nothing about compilation, tests or lints, and no such claim is made anywhere below.
Execution status of the three proofs is **unexecuted** on this lane.
They are ordinary workspace tests and run under the existing `cargo test --workspace` step of the
normal CI at the head above; this receipt records expected behaviour, not a green run.

## Scope

One file changed, tests only: `crates/cowfs-meta/src/db.rs`, `+302` lines, all inside the private
`#[cfg(test)] mod tests` and one test-only helper there.
No production behaviour, schema, or fault-API change.
No change to `crates/cowfs-meta/tests/health.rs`, `crates/cowfs-meta/tests/inode_reservation.rs`,
Core, ctl, daemon, store, NFS, CI, or any manifest.

```
$ git diff --stat cf67e8a..72f19f9
 crates/cowfs-meta/src/db.rs | 302 ++++++++++++++++++++++++++++++++++++++++++++
 1 file changed, 302 insertions(+)
```

The helper writes the existing `ino_reserved` meta key directly, which is the same write the
production reservation path performs. It exists only because reaching `INO_LIMIT` through
`reserve_inodes` alone would mean reserving well over a terabyte of numbers. No production seam is
added.

## Proof 1: the bound survives a real `open_recover` rollback (T10)

Test `open_recover_keeps_a_reservation_bound_across_a_real_rollback`.

It drives the on-disk repair. A store is built with a large final batch, then a
`reserve_inodes(1_000_000)` whose bound is durable one commit ahead of the floor move.
The file is copied before any drop, so the copy ends on the floor move.
The test then searches for single- or two-page damage that makes `Meta::open` fail closed and lets
`Meta::open_recover` roll back, applies it to the real file, and asserts:

- `Meta::open` refuses the damaged file before recovery,
- `open_recover` reports `rolled_back`,
- `recoveries == 1`,
- the recovered `ino_floor` covers the reserved range end,
- `health().ino_floor` agrees,
- the bound is spent (`durable_bound == None`),
- the next reservation resumes at or above the recovered floor,
- a plain reopen reports the same `recoveries` and `ino_floor` and still refuses to reissue.

This uses the real damaged-redb path, not `record_recovery` called directly.
The order is load-bearing: `reserve_intent` commits the bound, then `reserve_durable` moves the
floor and spends the bound, so a one-commit rollback of the floor move lands on the bound and
`record_recovery` skips to it.

Reproduction of the test body of record:

```
$ sed -n '2338,2460p' crates/cowfs-meta/src/db.rs
    #[test]
    fn open_recover_keeps_a_reservation_bound_across_a_real_rollback() {
```

Full source is in the commit, not reproduced here.

## Proof 2: the `INO_LIMIT` ceiling, from both sides (T9 and T9b)

Test `the_inode_ceiling_admits_the_last_range_and_refuses_the_overflow` and
`a_range_ending_on_the_limit_is_the_largest_legal_one`.

`INO_LIMIT` is `1 << 40` at `crates/cowfs-meta/src/types.rs:75`.
The floor is seeded one below the limit and the store reopened, which is the real load path.
The last single-number range is accepted, its end lands exactly on the limit, and the floor reaches
the limit.
Every further request (`1`, `2`, `1 << 20`) is refused with `Error::LimitExceeded`, the floor does
not move, and a reopen stays refused.
T9b pins the guard `n > INO_LIMIT - next` from both sides: `11` past ten remaining is refused and
consumes nothing, exactly `10` is accepted and ends on the limit.

This exercises the real arithmetic at the boundary; it is not a lexical math claim.

## Proof 3: measured latency, large against a single number (T11)

Test `a_large_reservation_is_measured_against_a_single_one`.

The representative full case runs first: a fresh store, `reserve_inodes(1_000_000)`, the durable
floor asserted on the range end, then close, reopen and a re-reservation asserting no reserved
number is reissued.
Only then the bounded comparison runs: the same fresh-store conditions for `n = 1` and
`n = 1_000_000`.

It prints `Instant` wall-clock samples and environment, with no threshold:

```
println!("inode reservation timing: env CI={ci} reps={reps} node_size=512 ino_block=8 \
          n=1000000 -> {full:?} (floor {floor})");
println!("inode reservation timing: n={n} reps={reps} min={min:?} median={median:?} max={max:?}");
```

`reps` is 3 under CI and 5 otherwise.
There is deliberately no timing assert, so no flaky wall-clock gate is introduced, and the 1.5x
filesystem gate in the design is untouched.
Because normal CI hides per-test stdout during `cargo test --workspace`, these lines do not surface
in the default job log; the deliverable of this proof is the source and the committed measurement
harness, and the numbers are produced when the test is run with `--nocapture`.
No timing number is claimed here because none was produced on this lane.

## Status of these proofs

| proof | source | execution |
| --- | --- | --- |
| 1, real `open_recover` rollback | committed, `db.rs` | unexecuted on this lane; expected on normal CI |
| 2, `INO_LIMIT` boundary | committed, `db.rs` | unexecuted on this lane; expected on normal CI |
| 3, measured latency | committed, `db.rs` | unexecuted on this lane; prints under `--nocapture` |

Proof of source and proof of execution are distinct.
This receipt proves the first and labels the second as not run on this lane.
No CI run, rerun, dispatch, artificial trigger or runner change was made.
No issue was closed and no goal was claimed.

## Commit and remote

```
$ git log --oneline -2
72f19f9 test(meta): prove the reservation bound across a real open_recover rollback
573b02f docs(meta): record the request-4 CI repair and its green run (#42)
$ git ls-remote https://github.com/zeeshanhaque21/cowfs.git refs/heads/fix/meta-inode-reservation-42
72f19f961fcb0fc8c4b780a1dc43e6799639846f	refs/heads/fix/meta-inode-reservation-42
```

Pushed over HTTPS, matching the remote head exactly.
The PR stays a draft and issue #42 stays open.
