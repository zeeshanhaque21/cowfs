# meta-health40-fixture-lifetime: the round-2 health fixture was testing the wrong bytes, proven at byte level

Lane: `.treehouse-ready-wave/.treehouse/cowfs-7c1bf8/5/cowfs`, branch `fix/metadata-health40-fixture-lifetime`.
Base: current main `0d7da418dae2f304ae901174425d32902a20cbd3` (PR #132 merged, `crates/cowfs-meta` unchanged).
Head: `8efc69bcbc86c56ba6e44ca9675ad8da4c2e52dd`.
Host: Apple M3 Max, macOS 26 aarch64, `rustc 1.99.0` / `cargo 1.99.0`.
Prior #40 lane head `fab4c6490c4274262976431384d7468b2e6a2de5` is preserved on `followup/metadata-health-40`, untouched.
Scope: `crates/cowfs-meta/tests/health.rs` only. No production file, no dependency, no workflow, no redb pin.

## Verdict

Proven, and it is a real fixture defect.

The round-2 fixture restored bytes while a `Snapshot` was still alive, and that `Snapshot`'s later drop committed over the file.
The fixture therefore never held the newest batch it intended to damage.
A regression assertion now pins the invariant, the old ordering fails it on the real round-2 fixture with exit 101 and two differing hashes, and the minimal ordered drop makes it pass.

Two things are deliberately separated here, because conflating them would be a false claim:

- The fixture defect below is proven, first-hand, on the real round-2 fixture.
- CI job `111880928624` remains UNREPRODUCED. That event is not resolved by this change and no claim is made that it is.

## The source chain, read directly

The defect is a handle-lifetime error in the test fixture, not in production code.

- `db.rs:1130` `pub struct Meta { pub(crate) h: Arc<Handle> }`, `#[derive(Clone, Debug)]`
- `db.rs:1540` `pub struct Snapshot { pub(crate) h: Arc<Handle>, id: SnapshotId }`, `#[derive(Clone, Debug)]`
- `db.rs:1089` `impl Drop for Handle` calls `self.inner.stop_bg()`, joins the thread, then `catch_unwind(... self.inner.finish_on_drop())`
- `db.rs:862` `fn finish_on_drop` returns early only `if s.closed`, otherwise `self.commit(&mut s, Extra::None, true, true)`
- `db.rs:184` `impl Drop for Db` is what actually drops the redb `Database`

So `Meta` and `Snapshot` are two owners of one `Arc<Handle>`, and the work that writes to disk happens in `Handle::drop`, not in either owner's drop.
`drop(m)` therefore cannot do anything while `s` is alive.

The base fixture, at `health.rs:635-649`, was:

```rust
let m = Meta::open(&fx.path, opts()).unwrap();
let s = m.snapshots().unwrap()[0].name.clone();
let s = m.snapshot(&s).unwrap();          // s: Snapshot, holds Arc<Handle>
let fresh = write_tail_batch(&s, round as u32);
m.sync().unwrap();
let before = m.health();
std::fs::copy(&fx.path, &fx.scratch).unwrap();
drop(m);
// The store is closed now, so its newest commit is the one the copy captured.
std::fs::copy(&fx.scratch, &fx.path).unwrap();
```

The comment on the `drop(m)` line was false in two independent ways.
The store was not closed, and the `s` binding at that point is not a name but the `Snapshot` owner, which drops at the end of the block, after the restore.
The line `let s = m.snapshots()...[0].name.clone()` binds a `String`, and the next line shadows it with the `Snapshot`, which is what keeps the handle alive.

`build()` has the same shape and is not affected, for the reason the parent inspection gave.
There the copies go to `pristine` and `work`, which are different paths, so the late commit lands on `path` and never on the fixture.
Round 2 restores onto the same live backing path, which is the whole difference.

## Runtime proof, not a type-level inference

The claim was checked against real bytes and a real open attempt, on the real round-2 fixture built by the suite's own `build` and `write_tail_batch` helpers, never on a surrogate model.

The spike mirrored round 2 step for step and hashed the file at each step.

| Step | Run 1 | Run 2 | Run 3 |
| --- | --- | --- | --- |
| captured batch hash | `3947a871d91aba063757bd2bb7469171` | `7e86bdbfe6491d1612aabcf067b88106` | `76898c2d37f0132b4a92f9332f3742c5` |
| captured batch pages | 633 | 633 | 633 |
| open attempt after `drop(m)` failed | true | true | true |
| hash right after the restore | `3947a871…` (equals captured) | `7e86bdb…` (equals captured) | `76898c2d…` (equals captured) |
| hash after the late `drop(s)` | `ddbd1719265346fc3a9fe91e4c93286f` | `9a0f8e00b8ea8512f8b57466f628822b` | `f9163156f9fe3aeafed3d2b9fcd79ae6` |
| pages after the late `drop(s)` | 632 | 633 | 632 |
| restore equals captured | **true** | **true** | **true** |
| after late drop equals restored | **false** | **false** | **false** |

Three facts fall out of that table.

The restore itself was always exact.
`restore_equals_cap` is `true` three times out of three, so the bug is not a bad file copy.

The store was demonstrably still open after `drop(m)`.
A real `Meta::open` attempt on `fx.path` failed every time, which is what a live redb handle looks like, and it directly falsifies the inline comment.
This is the runtime handle-state evidence, not an inference from the `Arc` types.

The late drop is what corrupts the fixture.
`after_sdrop_equals_restore` is `false` three times out of three, and `Handle::drop` to `finish_on_drop` to `commit` is the only path that writes there.
The absolute hashes differ between runs because the fixture content is not byte-stable, which is the same run-to-run variance the prior report measured as 617, 632 and 633 pages.
The relationship, however, is invariant across all three runs.

### The do-nothing control

The same fixture with ordered closure, dropping `Snapshot` before `Meta` and restoring once nothing holds the handle:

| Control measure | Run 1 | Run 2 | Run 3 |
| --- | --- | --- | --- |
| final hash equals captured hash | **true** | **true** | **true** |
| final pages | 633 | 633 | 633 |
| reopen after ordered closure succeeded | **true** | **true** | **true** |

The intended bytes survive exactly and the file reopens, which is the discriminating comparison between the old and new orders.

## Old ordering fails, minimal fix passes

The regression assertion was added first and then run against both orderings on the real round-2 fixture.
Only the drop order differs between the two; the assertion text is byte-identical.

Old ordering, the current code shape, assertion retained, disposable tree, receipt `5a2e7052e07802e2ad909257335fb6d09959691ba33e4d0ca6989934c05c717a`:

```text
panicked at crates/cowfs-meta/tests/health.rs:653:13:
assertion `left == right` failed: round 2: the fixture must still be the saved batch, so the
newest commit owns pages of its own and a rollback is reachable
  left: [9867214073256457868, 10683353415618980513]
 right: [11174933857596365288, 9114160574931978097]
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.65s
```

Real exit `101`.
`left` is the file on disk after the late `Snapshot` drop and `right` is the saved batch, and they are different bytes, so the precondition the damage search depends on was genuinely violated.

New ordering, the committed fix, receipt `22a986413cdeeae992fc62563aea306cfea4878464cf01da55eb60a34dfc5d22`:

```text
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 18.26s
```

The assertion is placed after the round-2 block and before `damage_newest_commit`, which is the only placement that can catch this.
Inside the block it would pass on the old code too, because the clobbering drop has not happened yet.

## What changed

One test file, `crates/cowfs-meta/tests/health.rs`, 26 insertions and 12 deletions.

The round-2 block now reads the batch bytes, drops `Snapshot` and then `Meta` inside a scope, restores once no handle can commit, and records the saved bytes.
The assertion then pins that the fixture still equals the saved batch before the damage search runs.

```rust
let (saved, before) = {
    let m = Meta::open(&fx.path, opts()).unwrap();
    let s = m.snapshots().unwrap()[0].name.clone();
    let s = m.snapshot(&s).unwrap();
    let fresh = write_tail_batch(&s, round as u32);
    handed_out = handed_out.max(*fresh.iter().max().unwrap());
    m.sync().unwrap();
    let before = m.health();
    let saved = std::fs::read(&fx.path).unwrap();
    drop(s);
    drop(m);
    (saved, before)
};
std::fs::write(&fx.path, &saved).unwrap();
```

The two `std::fs::copy` calls are gone, replaced by one `read` and one `write`, because a copy in and a copy back is exactly the shape that hid the bug, and the round-2 path no longer needs the scratch file at all.
The scratch file remains in use by `probe_page` and `damage`, so nothing else changed shape.

The test count is unchanged at 7.
Nothing was skipped, ignored, deleted, weakened or made conditional, no window was widened, no `TAIL` value was raised, no search retry or sleep was added, and no panic was reinterpreted as a pass.
`guard()` still maps a redb panic to `Error::Corrupt`, and the redb `unreachable` panics still appear and are still handled by that existing path.

## The two round-2 `RolledBack` verdicts and the counters are still asserted

The test still asserts, unchanged, and all still pass:

- `rec.rolled_back` in both rounds, so two `RolledBack` outcomes are still required, not one
- `rec.recoveries == round`, so the durable count is still monotonic across both rollbacks
- `rec.ino_floor > handed_out` and the snapshot floor, then `floors[1].0 > floors[0].0` and `floors[1].1 > floors[0].1`, so both floors must still move forward on the second rollback
- `m.check().unwrap()` in each round and `assert_pristine_intact` at the end, so the pristine copy is still proven byte-identical

The assertion added here sits before all of that and is on the fixture's own bytes.
It cannot be satisfied by a clean `check()`, a green sibling test, or a caught panic, because it compares the file the damage search is about to use against the batch that was actually written.

Every store involved is a private `tempfile` fixture built, damaged and repaired in-process, and the recovery path exercised is the real `Meta::open_recover` rollback, not `check()` alone.

## A real cost, disclosed

The `health` suite got about three times slower: `5.93` to `6.13` s before, `17.63`, `18.26` and `20.57` s after.

The reason is that the old fixture was wrong in a way that made the search cheap.
The prior report measured the old round-2 fixture yielding `RolledBack` on the very first candidate, page 1, which is why a run finished in about 6 s.
With the fixture restored to the bytes it is supposed to damage, the search genuinely explores the real batch layout instead of landing on the first candidate.
That time is spent inside the suite's existing `damage` helper and is not a sleep, a budget change or a retry loop.
The correctness gain is worth the cost, but the cost is real and is stated rather than buried.

## What this does and does not establish about the CI failure

The prior report `meta-health40-ci-rollback.md`, sha256 `ea010c926382d3df5418e4f2ae77a1d161ba9201c29624596b8f389c4f83219f`, is unchanged and its UNREPRODUCED verdict on CI job `111880928624` stands.
That job's `no 558-page fixture yielded RolledBack`, across all 1113 enumerated candidates, has still not been reproduced on this host in any bounded run.

What is now established is the mechanism that would produce exactly that symptom.
The suite's own documentation at `health.rs:90-96` states that when the final commit is small, the newest slot shares every page and no rollback is reachable, and that a large final batch is what gives the newest slot pages of its own.
The late `finish_on_drop` commit is precisely a small extra commit placed on top of the batch, after the batch was meant to be the newest thing in the file.
So the fixture was, on the old ordering, reliably putting itself into the one state its own documentation says makes the search fail.

That is a strong mechanism and it is consistent with the failure, including the run-to-run page-count variance.
It is not proof that this is what the CI runner hit.
Claiming the original CI event is resolved would be a claim this evidence does not support, and a green local suite is not a reproduction of a red CI job.

Deciding whether to treat the original event as explained, or to ask for a rerun to observe it, is the coordinator's call.

## Scoped results at the fix head

Every command ran inside one 600 second foreground `mac-heavy.lock` hold with isolated `CARGO_TARGET_DIR` and a project-local `TMPDIR`, and source receipts were printed before the batch.

| Check | Result | Exit |
| --- | --- | --- |
| `cargo test -p cowfs-meta --locked --test health`, repeat 1 | 7 passed, 0 failed, 18.26s | 0 |
| `cargo test -p cowfs-meta --locked --test health`, repeat 2 | 7 passed, 0 failed, 17.63s | 0 |
| `cargo test -p cowfs-meta --locked --test health`, repeat 3 | 7 passed, 0 failed, 20.57s | 0 |
| `cargo test -p cowfs-meta --locked --test recovery40` | 4 passed, 0 failed, 4.16s | 0 |
| `cargo fmt -p cowfs-meta -- --check` | clean | 0 |
| `cargo clippy -p cowfs-meta --all-targets --locked -- -D warnings` | zero warning or error lines | 0 |

The four #40 recovery controls all pass individually, so the stored-inode-block invariant work from PR #116 is intact and untouched:

- `a_smaller_block_at_recovery_does_not_re_issue_inode_numbers`
- `the_stored_block_governs_a_plain_reopen_with_a_different_ino_block`
- `recovery_refuses_a_file_whose_reservation_block_is_unknown`
- `a_snapshot_id_lost_to_a_rollback_is_not_handed_out_again`

`crates/cowfs-meta/src/db.rs` is byte-identical before and after at sha256 `de79c2713fbb01489c7895d51b0c5c2b52229ea8da86db552d8d41532ef38da1`, which is also PR #116's recorded shipping receipt, so the #40 production proof, the `FORMAT_VERSION`, the legacy refusal and the no-migration stance are all unchanged.

## Source receipts

| Tree | File | sha256 |
| --- | --- | --- |
| main `0d7da418`, PR #116 head, PR #131 head, base of this branch | `health.rs` | `e083506ca98a9b88a1ce6bb5c80e9910b2f63e3e4f55eeb5f4155a0c1b5a11f5` |
| this branch head `8efc69b`, the fix | `health.rs` | `22a986413cdeeae992fc62563aea306cfea4878464cf01da55eb60a34dfc5d22` |
| disposable, old ordering with the assertion retained | `health.rs` | `5a2e7052e07802e2ad909257335fb6d09959691ba33e4d0ca6989934c05c717a` |
| disposable, spike appended | `health.rs` | `851fa984e6a66d84b883ad1fcbaf24f8e9adbc59511574b9263f4307aa6d915b` |
| every tree, unchanged | `db.rs` | `de79c2713fbb01489c7895d51b0c5c2b52229ea8da86db552d8d41532ef38da1` |

Base `health.rs` git blob `be0a908c2a43fa4ed89da10c7462b9adeececcb1`.
Spike tree from a fresh `git archive` of `0d7da418`, 637 tracked files and 637 on disk, so every byte tested is a committed blob.

Raw evidence under `bench/out/meta-health40-fixture-lifetime/`: `smoke1.log`, `discriminate.log`, `oldorder-fail.log`, `gate2-run.log`.
The prior lane's directory `bench/out/meta-health40-ci/` is untouched and still holds the 234589 byte CI log at sha256 `7873236f66e0bcf57227b84586d5c5fdd3a09bf36262eb3801b4952e41c3bd80`.

## Resource and safety record

- Free disk was `325.8 GiB` before the batch, against a `20 GiB` floor
- All four disposable trees had their own `CARGO_TARGET_DIR` and one project-local `TMPDIR`, so no shared target, mtime copy or cache was involved
- The spike and the old-order variant were written only into disposable trees under this lane's artifact directory; the committed `health.rs` was edited once and is byte-pinned above
- Every damaged database was a private `tempfile` fixture inside this lease, and both spike fixtures asserted their pristine copy was unchanged
- No daemon was started, no mount was made or walked, no store, socket or Linux host was touched, and no signal was sent to anything
- The shared `mac-heavy.lock` was taken with the same non-blocking `flock` protocol the peer lanes use, and a holder was never signalled
- No irreversible action was taken, so there was nothing to verify before one, and no buffered generator exists to protect
- No issue was created or edited, no other lease, slot or agent file was written, and no lease was returned, reset, stashed, rebased or deleted
- `git status --porcelain` was empty before the fix, and clean after the commit

## Independent review and merge

This needs a fresh independent review of the byte-identity proof and a real CI run before anything is merged.
CI on this branch is expected to be pending rather than green at the time of writing, and no CI result is claimed here beyond what the table above actually ran.
Merging is the coordinator's decision, not this lane's.