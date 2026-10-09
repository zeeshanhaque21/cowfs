# PR144 original-snapshot reopen: final independent review

Read-only review of PR144 head `a1ebd3d034c2284c189c340a8c419d59c5269154` (`test/meta-allocation-option-extremes-40`), testfix `825bd38e0b6aa62b06b4de25ec3579d572cc753f`.
Subject: issue #40 M5, `Options::ino_block` extremes. Read with `docs/design.md`, `docs/reviews/pr144-clean-close-runtime-final-wbuddy-review.md` (`0f730979...`), and receipt `docs/verification/evidence/meta40-allocator-extremes-original-snapshot-reopen-correction.md` (SHA256 `6a26a818b7daa89f64a55957c7e48572e65da730f6617a6437043de377dbb111`).

No source/checkout/local cargo/build/test, no probe/archive/cleanup/offload/cap-waiver/lease, no signals/SSH, no GitHub mutation, no commit/push. No poll/wait/dispatch/re-run. Historical receipts/reviews immutable. READY3 Core and READY7 NFS writers not overlapped. 8 GiB cap / 20 GiB floor binding; no local execution. Primary dirty `9874` tree, `docs/v1-core.md`, `progress/*` preserved.

## Pins

| object | value |
| --- | --- |
| reviewed head | `a1ebd3d034c2284c189c340a8c419d59c5269154` (tree `d090fdda...`, parent `1e09faa2...`) |
| testfix | `825bd38e0b6aa62b06b4de25ec3579d572cc753f` (parent `ef9b40a2...`) |
| remote branch + PR head | both `a1ebd3d0...` |
| current remote `main` | `1580e69b9d987f63c07b2430f8c0b4547ecd8622` |
| test blob (head = testfix) | `d472859e73076df0ef610432f68d05f3e41013e3` |
| prior head test blob (`fbdd104f`) | `bd58d08f0ac34fe0cfc0d73faa8f68a24410779e` |
| `db.rs`/`tx.rs`/`types.rs`/`check.rs`/`lib.rs` | head == remote `main`, byte-identical |
| receipt at head | `6a26a818b7daa89f64a55957c7e48572e65da730f6617a6437043de377dbb111` |

Delta `fbdd104f..head` = test file + two receipts. Delta `825bd38..head` = one receipt only. `git diff 1580e69b head -- crates/cowfs-meta/src/` is empty: no production change; scope is test-only plus receipts.

## Source verdict: correct, not weakened

- Real M5 path retained: the first ordinary `create` under `u64::MAX` at L47-49 is still the load-bearing `.expect("...must not overflow")`; the clamp discriminator is intact (unclamped `2 + u64::MAX` overflows before `.min(INO_LIMIT)` in `tx.rs:72`).
- Correct persistent-snapshot selection: `sid` is captured as `s.id()` (L71) inside the same block, and after reopen the file is read via `m.snapshot_by_id(sid)` (L92). `snapshot_by_id` is public (`lib.rs:23` re-exports `SnapshotId`; `db.rs:1624`) and opens only if `snaps.contains_key(&id)`; on open all snapshots reload from the `SNAPSHOTS` table (`db.rs:1519-1528`). So it reads the durable `s0` where `f` was created, not a fresh empty snapshot. The receipt's root-cause claim is verified.
- All handles dropped before reopen: both `m` (Meta) and `s` (Snapshot) live only inside the `{ ... }` block that closes at L72; the reopen at L78 is after both drop, and `sid` is a plain `SnapshotId` value that outlives them.
- No assertion weakened: only `floor >= created.0 + 1` became `floor > created.0` (the `int_plus_one` lint fix, semantically identical for integers); identity readback, `kind == File`, `mode == 0o644`, fresh `g.ino > created`, `g.ino < INO_LIMIT`, and all `check()` calls are unchanged or added. Zero case (L122) and default control (L160) and the extreme sweep (L202) are untouched.
- Method note: L104/L110 assert `g.ino.0 > created.0` and `< INO_LIMIT` on `g.ino.0`, which is `Ino`'s public `.0`; no private oracle is used. `FileType` is imported. Consistent, compiles.

Old failure reproduced as stated: run `37533053984` = completed/failure, 3 pass / 1 fail, MAX `lookup(ROOT_INO, b"f") -> NotFound` (the empty-`s1` fixture bug). Fixture-only, not production loss.

## Runtime verdict: PENDING, not green

Run `37535494085` (exact head `a1ebd3d`, `ci`/`pull_request`) is **`in_progress`**; all three jobs (`check (ubuntu-latest)` 112515222176, `check (macos-latest)` 112515222016, `linux-fuse` 112515221914) are `in_progress`, created ~2m before this read.
Per instruction, no poll/wait/dispatch/re-run was performed; reported PENDING and stopped.
Tested merge tree is `Merge a1ebd3d into 1580e69b` (current remote `main`), not a stale base. No named result for the four `allocation_option_extremes` tests exists yet; fmt/clippy and required checks unverified.

## Merge readiness and remaining #40 scope

Merge-ready on **source only**, **blocked on runtime** until run `37535494085` completes green with all four named tests on Ubuntu and macOS plus fmt/clippy. This is not whole-#40 closure and not a new requirement. Still open outside this PR: real-`cowfs-store` crash harness, ported mutant harness, Core `Health` wiring, and the other #40 items (M1-M4, M6).
