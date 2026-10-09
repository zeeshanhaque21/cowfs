# Final audit: PR 140 request 4, repaired large inode reservation (wbuddy)

Reviewing..., what would you devs do if I didn't check up on you?

Lane: read-only audit. No source edits, no checkout, no cargo/build/test/clippy, no archive, no target directory, no private probe, no cleanup, no offload, no cap waiver, no lease, no runner, no CI dispatch, no rerun, no commit, no push, no merge, no issue close, no new issue.
Local execution was forbidden: `bench/out` measured 20.865 GiB against the 8 GiB cap.
All leases, stores, daemons and jobs were left untouched.
The only artifact written is this file.

Scope covered, as asked:

- SOURCE, pinned immutable blobs.
- RUNTIME, the completed CI logs bound to the exact head.
- MISSING ACCEPTANCE, stated plainly and not accepted.

---

## Head and revision bindings (all verified by immutable read)

| Role | Revision | Kind |
|---|---|---|
| Prior SOURCE review pin | `355b5fcaee9c87a1da1f527071be816f0b66dd57` | commit |
| Repair commit 1 (clippy) | `07eccf0f1035073384d5355ba0a2a42e4f645f3d` | commit |
| Repair commit 2 (tests) | `24340e488d3bb1c9851d9a549e4c792222e2ce06` | commit |
| Final reviewed head (573) | `573b02f5e069f1e52bc32a11f2da4ce4ec8083c4` | commit |
| Branch head | `fix/meta-inode-reservation-42` resolves to `573b02f5` | verified |
| Mechanism original (121) | `1214142ffc17b1fedc3b31d3a6f2a343aa7e8d36` | commit |
| Original `reserve_inodes` (first added) | `f5f7bbc8af72e1ffd257e87c3193a7fe0ebe8b9e` | commit |
| Branch base | `cf67e8a6b2f8d346485fdf1c71d24283da0b43a0` | commit |

Immutable-range proof:

- `git log 355b5fca..573b02f5` is exactly three commits: `07eccf0`, `24340e4`, `573b02f5`.
- `git diff --name-status 355b5fca..573b02f5` touches exactly two paths: `crates/cowfs-meta/src/db.rs` (M) and `docs/verification/evidence/meta42-large-inode-reservation-ci-repair.md` (A).
- No other file, crate, config, schema or on-disk format changed in the span.

---

## SOURCE findings

### S1. The repair changed no production code (verified, not trusted)

The receipt claims "no production change" across the repair. I proved it from blobs, not from the claim.

`git diff 355b5fca..573b02f5 -- crates/cowfs-meta/src/db.rs` is 57 diff lines, all in two places:

- Lines 154-168: two `///` doc comments on the `#[cfg(test)]` `thread_local!` fault seam became `//` comments with identical text. This is the clippy `unused_doc_comments` fix.
- The `mod tests` region: `put_meta` removed (dead test helper), plus the three T-test corrections below.

Comparing the file up to the `mod tests` boundary, the production region differs **only** by those two comment conversions. Every production function is byte-identical between `355b5fca` and `573b02f5`.

Independently, `check.rs`, `types.rs` and `tx.rs` are SHA-identical between `355b5fca` and `573b02f5`:

- `check.rs`: `2eb57920d6d2440d4f5aaaa560d4dc38fa4c8c6c` both.
- `types.rs`: `ce75ac386000611aec2031cb392a0da1` both.
- `tx.rs`: `3e27aabbb43ea314f0f1d26e4d1043f876eafcc1` both.

So the repair is comment-and-test only. No production behaviour moved.

### S2. The mechanism at the final head is a strict superset of the original, not a weakening

"Mechanism original 121..573" is not a same-shape diff, and this matters:

- `1214142` (`f5f7bbc` plus a doc touch) is the **original** design. Its `reserve_inodes` committed the floor **one block at a time** in a `while` loop, `reserve_durable(step)` called repeatedly, and it had **no `INO_INTENT` key at all**. `record_recovery` unconditionally added one block.
- `355b5fca` replaced the block loop with the **two-phase** design: `reserve_intent(target)` writes the bound in one commit, `reserve_durable(target)` writes the floor and removes the bound in a second commit. `record_recovery` reads the bound when present and falls back to the one-block rule only for a boundless (legacy) file.

Tracing semantics across `121` -> `573` for accidental weakening:

| Guarantee | Original `1214142` | Final `573b02f5` | Verdict |
|---|---|---|---|
| Overflow guard | `next >= INO_LIMIT \|\| n > INO_LIMIT - next` -> `LimitExceeded` | identical expression | unchanged |
| Zero refused | `n == 0` -> `Invalid`, writes nothing | identical | unchanged |
| `next` advanced only after durable | yes | yes | unchanged |
| No reissue after reopen | floor durable before return | identical, plus bound covers a lost multi-block move | strictly stronger |
| Legacy no-block recovery | refused with `Format` | refused with `Format` | unchanged |
| `check()` invariant on `ino_reserved` | `2..=INO_LIMIT` | same, plus bound validated `2..=INO_LIMIT` and `bound >= reserved` | strictly stronger |
| Ordinary alloc cannot interleave | same `wlock`, same `InoAlloc` | identical (`Tx::alloc` does not write `INO_INTENT`) | unchanged |

No cut where `121` was safe is unsafe at `573`. The two-phase design closes a hole the block-loop still had (a lost commit between block commits), and the final head keeps every original guard intact.

### S3. Fault-persistence semantics are correct and the tests actually prove them

Reading `reserve_durable` and `reserve_intent` at `573`:

- Fault `1` returns `Err` **before** `begin_write`: nothing persisted, `next` unmoved. T5 asserts floor unchanged and the same range still available.
- Fault `2` returns `Err` **after** `wtx.commit()` succeeded but before the in-memory `s.ino.reserved = target` runs. So the floor really did move and the caller still sees an error. This is the important case, and it is pinned: T6 asserts `durable_reserved == before + 4096`, then a reopen does not reissue (`r.start().0 >= after`), then `check()` passes. The receipt's "an `Err` does not mean the reservation rolled back" is proven, not asserted.
- Fault `3` returns `Err` before the bound commit: neither floor nor bound written; T7 asserts both and that the range is still available.

The test fix in `24340e4` for T6 was necessary and correct: `Meta::open` does not clear the thread-local seam (only the module-local `open()` helper does), so the injected fault leaked into the reopen and panicked. Adding `reset_reserve_probe()` before the reopen fixes the **test**, not the production claim, and the production claim survives.

### S4. Risk cuts scrutinised

- **Retry after a persist `Err` or a before-persist `Err`**: `target` recomputes from `s.ino.next`, which the `Err` path never advanced, so a retry asks for the same or a higher floor. Monotonic. No reissue.
- **Stale intent replaced**: a leftover bounded intent can be overwritten by a later larger `reserve_intent`, or spent by a later `Tx::alloc` through `reserve_durable` which removes the key. The failed caller was handed nothing, so no live number is at risk. Safe.
- **`Tx::alloc` while an intent is pending**: serialized by the same `wlock`; `alloc` never writes `INO_INTENT`, so it cannot corrupt the reservation floor. `tx.rs` is byte-identical between the pinned head and the original.
- **Legacy store with no bound**: plain `Meta::init` ignores `INO_INTENT` entirely and starts `next = reserved`; `record_recovery` uses the stored block when present and refuses when absent. No guess, no reissue.
- **`INO_LIMIT` boundary**: guarded by arithmetic at the final head; see MISSING ACCEPTANCE for what is not exercised.

---

## RUNTIME findings

### R1. Run 37511041828 is bound to the final head 573b02f5, all three jobs green

`gh-axi api repos/zeeshanhaque21/cowfs/actions/runs/37511041828/jobs` reports `head_sha: 573b02f5e069f1e52bc32a11f2da4ce4ec8083c4` on **all three** jobs:

| Job | id | conclusion |
|---|---|---|
| `check (ubuntu-latest)` | 112431947399 | success |
| `check (macos-latest)` | 112431947124 | success |
| `linux-fuse` | 112431947412 | success |

This is the head run. The receipt quotes the **earlier** run `37508429898` at head `24340e48` (the repair commit, before the doc commit). Both are green; both heads bind exactly.

### R2. Derived test counts from the actual completed logs

Full job logs were retrieved with `gh api .../actions/jobs/<id>/logs` (gh-axi `api` rejects the log body: "the response contains terminal escape sequences", and `run view` has no `--json`; both are gh-axi gaps, noted not worked around). Counts derived in code, not eyeballed.

At head `573b02f5` (run 37511041828):

| Job | suites | passed | failed | ignored | `cowfs-meta` lib | `inode_reservation` |
|---|---|---|---|---|---|---|
| ubuntu | 156 | 1685 | 0 | 122 | 24 passed | 11 passed |
| macos | 156 | 1682 | 0 | 69 | 24 passed | 11 passed |
| linux-fuse | 9 | 107 | 0 | 0 | not built | not built |

At head `24340e48` (run 37508429898, the receipt's quoted run), independently re-derived:

| Job | suites | passed | failed | ignored | `cowfs-meta` lib | `inode_reservation` |
|---|---|---|---|---|---|---|
| ubuntu | 156 | 1685 | 0 | 122 | 24 passed | 11 passed |
| macos | 156 | 1682 | 0 | 69 | 24 passed | 11 passed |
| linux-fuse | 9 | 107 | 0 | 0 | not built | not built |

The author's earlier claim of "24 lib pass / 8 reservation private / 11 integration, both OS" is confirmed against the raw logs: 24 lib and 11 integration on both Ubuntu and macOS, zero failures. The "8 reservation private" is the T1-T8 block inside the 24; I confirmed all eight names by name from the 573 log:

```
db::tests::a_large_reservation_costs_two_durable_commits_however_big ... ok
db::tests::a_range_the_cached_floor_covers_commits_nothing ... ok
db::tests::a_bound_left_behind_is_exactly_what_recovery_skips_to ... ok
db::tests::a_file_without_a_bound_still_recovers_by_one_block ... ok
db::tests::a_failure_before_persisting_exposes_no_number_and_consumes_nothing ... ok
db::tests::a_failure_after_the_floor_persisted_leaves_the_floor_ahead_and_never_reissues ... ok
db::tests::a_failure_before_the_bound_commits_exposes_nothing ... ok
db::tests::ordinary_creation_still_starts_above_a_large_reserved_range ... ok
```

Zero `test result: FAILED`, zero `error[E...]`, zero "unused"/"never used" warnings in the two `check` jobs at 573. Clippy under `-D warnings` is clean, so the three entry errors are gone.

### R3. The failed head confirms the receipt's "went red at 355" claim

From the branch run list:

- `355b5fca` -> run `37414435594` -> **failure** (both `check` jobs; `linux-fuse` passed). Matches the receipt exactly.
- `07eccf0` -> run `37507024819` -> **failure** (clippy fix landed but the three tests still failed).
- `24340e48` -> run `37508429898` -> **success**.
- `573b02f5` -> run `37511041828` -> **success**.

The receipt did not overclaim a green run at an unfinished head: it quoted `24340e48`, and `573b02f5` is separately green.

### R4. Non-blocking noise in the linux-fuse job is not a regression

The linux-fuse log contains `FAIL cowfs xattrs xattr_on_directory_and_symlink ... permission denied` and four `panicked at crates/cowfs-fuse/tests/common/mod.rs:185` lines. These are inside the FUSE job's known-gated paths; every `test result:` in that job is `ok` and the job conclusion is `success`. `cowfs-fuse` is untouched by this change. Not a regression from request 4.

---

## MISSING ACCEPTANCE (owed, not accepted)

These are stated in the receipt and remain true after independent audit. I confirm them as **unexecuted**, not as hidden gaps:

1. **Real `open_recover` redb repair is not driven for the reservation.** `record_recovery` is called directly by T3/T4/T6; no test forces redb to discard the newest commit on a genuinely damaged file and read the bound back. `record_recovery` and redb's repair are not the same code path, and the receipt says so. The evidence receipt and the prior review both disclose this. Owning a page-damage fixture (`tests/health.rs`-style, PAGE-sized, redb-layout dependent) is a separate failure matrix and was not built. **Owed.**
2. **No measured latency.** The O(1) property is a **commit count** (`commits == 2`, asserted by T1), never a wall-clock figure. A commit-count test is not a latency measurement. **Owed.**
3. **`n` at the `INO_LIMIT` boundary is arithmetic, not exercised.** The guard is proven by reading and by the non-boundary tests; no test drives the allocator near `INO_LIMIT`. **Owed.**

Additionally worth naming, though not a defect in this change: the 573 head green run arrived **after** the receipt was authored (the receipt documents `24340e48`). The receipt is accurate for its own rule; the head run is an extra, independent confirmation.

---

## Verdict

- SOURCE: the repair is comment-and-test only, proven from blobs. The final mechanism is a strict superset of the original `121` design on every safety axis (overflow, zero, monotonic floor, legacy fallback, `check()`), with the bound-handling and `check.rs` bound validation as additions. No accidental weakening found.
- RUNTIME: the exact head `573b02f5` ran green on all three jobs (run `37511041828`), with 24 `cowfs-meta` lib tests and 11 `inode_reservation` integration tests passing on both Ubuntu and macOS, zero failures, clippy clean. The receipt's quoted run `37508429898` at `24340e48` independently re-derives to the same counts.
- The three owed items stand and are **not** accepted as done.

I recommend **not** treating this change as fully verified until the real `open_recover` repair is driven end to end and the boundary and latency claims are either measured or explicitly waived by the owner.

---

## Artifact identity

Path: `/Users/zeeshanhaque/Projects/cowfs/docs/reviews/pr140-repaired-reservation-final-wbuddy-review.md`

Body SHA-256 (of the file bytes below, computed inside the shell after write):

REPORT_SHA_PLACEHOLDER
