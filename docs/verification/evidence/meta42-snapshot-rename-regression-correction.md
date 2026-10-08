# meta42-snapshot-rename-regression-correction: the review's three findings are accepted, the dirty fixture is corrected, and the runtime is blocked

Lane: `.treehouse-ready-wave/.treehouse/cowfs-7c1bf8/5/cowfs`, READY5.
Base: merged `main` at `93cfef94457a989d031cb6b0a475ac4edbdb85ef`.
Blocked head under review: `924c16bd14c17b0716b19cdf4a0e02e913144a39`.
This head is a **test-only** correction on top of it. `crates/cowfs-meta/src/db.rs` is unchanged.
Blocking review answered: `docs/reviews/pr137-meta42-snapshot-rename-final.md`, sha256 `471a07310e793016763a1f77e4e195bfeca89dcf1613d84fe50b238a6355b0ec`.
Prior receipt: `docs/verification/evidence/meta42-snapshot-rename.md`, sha256 `93c4b1317b7d9445ced3f7d1316e159b3cca472cbee6172c0a81e2b9a8883c0b`, **preserved unedited**.

## Verdict on the review: all three findings accepted

I re-derived each one from the source before changing anything, and each holds.

**Finding 1, blocking, confirmed in the source and not yet reproduced at runtime.**
The flush loop at `db.rs:531-546` writes the new root for every dirty snapshot, including the rename target.
The rename arm at `db.rs:600-610` then reads the **session's** copy of the row:

```rust
let e = s.snaps.get(id).ok_or(Error::NoSuchSnapshot)?;   // s is the Session
let mut info = e.info.clone();                          // e.info.root is the pre-flush root
info.name = (*name).to_string();
snaps.insert(id.0, encode_snap(&info).as_slice())?;     // second insert on the same key
```

The session's `info.root` is only brought forward at `db.rs:630-635`, after `wtx.commit()` at `db.rs:620`, so the second insert overwrites the flushed root with the pre-flush one.
`Extra::Add` at `db.rs:551-553` resolves the root through `new_roots` for exactly this reason and only falls back to the session copy, which is the asymmetry the review points at.

This is stated as a source chain with line numbers, and that is all it is.
**I have not reproduced it, and I am not claiming a runtime counterexample.**

**Finding 2, blocking, confirmed.** The dirty-snapshot test used `opts()` with `sync_every_ops: 1`, so one `s.create` is one applied op, `1 >= 1` satisfies the inline commit, and the tree is already clean when the rename runs. The test passed on a clean tree and the property it was named for was never exercised.

**Finding 3, blocking, confirmed.** `tests/snapshot_rename.rs:215` bound `inos` without using it, so `cargo clippy --workspace --all-targets -- -D warnings` failed at Ubuntu and macOS before any test target ran. There is no `test result:` line in either CI log, so no test in this branch has ever run on CI.

## Retractions of my prior receipt, explicitly

The prior receipt `93c4b131` is preserved byte for byte and is **not** rewritten. These are its claims, withdrawn:

- **"the same transaction still writes the roots of any dirty snapshot"** is not supported at head `924c16b`. The transaction does write the root and then the rename arm overwrites it.
- **The claim that `a_rename_of_a_dirty_snapshot_keeps_its_uncommitted_writes` covers that property** is withdrawn. It never exercised a dirty tree.
- **`fmt 0` and `clippy 0`** are withdrawn. Clippy is demonstrably non-zero at that head; CI failed on it at both platforms.
- **Every count in that receipt**, namely eight rename tests passing twice, lib 16, health 7, recovery40 four, critic 12, posix 16, model 2 and crash 2 with one ignored, is withdrawn as a statement about `924c16b`.
  Those were results of my local working tree, where the clippy lint had already been avoided by the time I ran them, and they were reported as local results. They are not results of the committed head, and the reviewer's reading is that they are not shown to run at all at that head. Both readings agree they cannot stand as head results.

What is **not** withdrawn: the API shape, the one-transaction property, the refusal semantics, the name-validation equality with `new_snapshot`, the refusal-before-mutation ordering, the session mirror being applied only after the commit, the untouched reservation, reap queue and `next_snapshot`, and the absence of any schema, dependency or lock change.
The review confirmed each of those independently in the source, and the rename arm writes only `SNAPSHOTS` and `SNAP_NAMES`.

## What this commit changes, and only this

Test file only. No production file, no dependency, no lock line.

**1. The clippy lint.** `a_name_held_by_another_snapshot_is_refused` no longer binds the unused `inos`.

**2. The dirty fixture, corrected so it is genuinely dirty.** The old test was replaced by `a_rename_commits_a_pending_tree_change_and_leaves_the_row_readable`, which uses its own options:

```rust
fn opts_pending() -> Options {
    Options {
        node_size: 512,
        background: false,
        ..Options::default()   // sync_every_ops stays at the default 256, ack is Ack::Applied
    }
}
```

`256` is the default and is what `cowfs_core::inner` opens meta with, so one applied operation does not reach the commit threshold and stays pending.
The test then does, in order:

1. create a snapshot, write three files, `m.sync()`, and read the durable root: `old_root`,
2. one `s.create` that is **not** synced, so the tree is dirty and the rename is the next durable commit,
3. assert the pending entry is already visible in the cached tree, which is the public evidence that the tree really is pending at that point and not asserted from the inside,
4. the rename,
5. drop everything, reopen, and read.

The discriminator after the reopen is deliberately a **read**, not a row comparison:

```rust
assert_ne!(infos[0].root.as_bytes(), &old_root,
    "the committed row must carry the flushed root, not the pre-flush one");
let reopened = again.snapshot("renamed").expect("the renamed snapshot must reopen with a readable root");
let f = reopened.lookup(ROOT_INO, b"pending")
    .expect("the rename's commit must have made the pending create durable");
```

The lookup walks the tree from the row's root, so a row left pointing at the root that the same transaction freed fails there, on a read of real metadata rather than on a self-consistent assertion.
`again.check()` is asserted too, since it walks every snapshot root against the reference counts.
The three original files are asserted still present, and the pending entry is asserted to carry the inode number it was handed.

The same fixture stays in the tree so the corrected production source can be run against exactly this test later.

## Runtime is blocked, and no red is claimed

**I did not run this test. It is not compiled. No TDD red is established, and none is claimed from theory.**

The reasons, all measured rather than assumed:

- `bench/out` in this lane measures **20.862 GiB** physical, against the 8 GiB cap. The reviewer measured 35.884 GiB at the same tree earlier. Either way the cap is exceeded and the instruction is to stop heavy work.
- The only prebuilt `snapshot_rename` binary in the artifact tree is `snapshot_rename-9e58373b250cb1bc`, built from the **old** fixture with `sync_every_ops: 1`. Running it would execute the old test, not this one, and reporting that as this fixture running would be false.
- `Options` has no runtime configuration: no environment variable, no argument parsing in `crates/cowfs-meta/src/db.rs`. The binary cannot be re-pointed at `sync_every_ops: 256`.
- So a bounded run of the corrected fixture needs a compile, and a compile is what the budget gate forbids.

Consequences stated plainly: the compiled status of this change is **none**, locally; the clippy fix is verified by source reading and by the fact that no binding is left unused, not by a clippy run; and the corrected fixture is a **candidate** that should be expected to fail on the current production source if Finding 1 is real, which is a prediction and not a measurement.

The cap violation is not waived by free space.
Free disk is 294.4 GiB against a 20 GiB floor, and neither of those is the gate; the gate is `bench/out`.

## What I did not do

- No fix to `db.rs`, and no production edit of any kind. The two-line change the review describes, resolving the row's root through `new_roots` as `Extra::Add` does, is **not** applied here, because the brief requires a runtime old-fail first and there is none.
- No compile, no fmt, no clippy, no archive, no new target directory, no cache pruning, no deletion, no move, no offloading.
- No run of the old prebuilt binary presented as this fixture.
- No new issue, no matrix, no consumer change, no schema change, no name-policy expansion.
- No workflow edit, no dispatch, no rerun, no runner change, no artificial trigger commit.
- Nothing deleted: every prior report, log, fixture, artifact tree and other lane's cache is intact, and `bench/out` is larger than when I started.

## Limitations, carried forward

- **Real store-bytes readback is still owed.** Reading actual `cowfs-store` bytes through the refs before the rename, after the rename and after a reopen is the probe that would catch Finding 1 without trusting the row, and it is not written. The review is right that it is the right test and that it belongs with the root fix.
- The forked-from case is untested. If `refs[R_old]` is above 1 the old tree is not freed, so the same defect becomes silent staleness instead of a dangling root.
- The exact error a caller sees on the first read after reopen is derived from the source, not executed.
- Meta's name rule remains looser than Core's, as it already was for `new_snapshot`, and unifying them is request 2 of #42.
- The API still has no consumer; `cowfs-core` still stages its own rename. Nothing outside `cowfs-meta` changes behaviour.
- Browser unverified. `no-mistakes` is uninitialized in this lane and was not initialized. Misakanet is local-only here and was not consulted.

## What has to happen next, in order

1. A budget decision from the coordinator, because nothing here can be executed until `bench/out` is under the cap. That is a gate, not a code task.
2. With budget available, run this exact fixture against unmodified production to establish the runtime old-fail. Only then is a root fix in scope.
3. Apply the root fix, adding the real store-bytes probe alongside it.
4. Fresh independent review of the root fix, and green CI.

Nothing is merged. PR #137 stays a draft and is recorded as BLOCKED, issue #42 stays open, and no new task was created.