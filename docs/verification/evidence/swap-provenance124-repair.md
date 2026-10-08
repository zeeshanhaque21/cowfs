# swap-provenance124-repair: option 1, a swap invalidates the base record instead of keeping it

Lane: `.treehouse-ready-wave/.treehouse/cowfs-7c1bf8/5/cowfs`, READY5.
Base: merged `main` at `93cfef94457a989d031cb6b0a475ac4edbdb85ef`.
Code commit `b9422768d5938e3242c5bb8ebb70b18ac44f47aa`, documentation commit `cbac228`.
Branch `fix/swap-provenance-124`, pushed over HTTPS.
Host: Apple M3 Max, macOS 26 aarch64, `rustc 1.99.0` / `cargo 1.99.0`.
Reproduction this answers: `swap-provenance124-reproduction.md`, sha256 `3fde9b054fea530288e1fe3d4577bdbe97a626fad2f6f26cf92611b8d3a029a1`, committed on this branch byte for byte.

## Verdict

Option 1 implemented and proven by a regression that fails first-hand on the old code and passes on the new.

A successful public swap onto a promoted base now leaves that base promoted with no `repo`, no `git_ref` and no `commit`.
The record can no longer describe a tree that is not there.
One test file, one production file, no new public API, no change to `cowfs-core`, the VFS, the control protocol or any dependency.

## What changed and what deliberately did not

Production, `crates/cowfs-daemon/src/backend.rs` only:

- `CoreSnapshots::swap` and `PathSnapshots::swap` now run inside `self.bases.exclusive`, the section `create`, `remove`, `rename` and `promote` already use, so the tree change and the record change are one step for a reader.
- Two new private helpers, `invalidate_base_record` and `restore_base_record`, sit next to the existing `rollback_base` seam.
- The record is invalidated to `Record::promoted_unknown()`, the exact value `promote` already writes when a base has no known provenance.

Tests, `crates/cowfs-daemon/tests/swap_provenance_124.rs`, new, 9 tests.

Deliberately not done, each with a reason:

- **The source's provenance is not adopted.** A swap installs a clone, and nothing in `snapshot_reset {name, from}` says which build that clone came from. Adopting the source's record would replace one unproven claim with another. A test asserts the discard explicitly so a future reader does not mistake it for an oversight.
- **A name that was never promoted does not acquire a record.** A plain snapshot is not a base and the swap does not promote it. A test covers both backends.
- **No new API, no source-provenance feature, no core or VFS change.** The instruction to stop before widening production core scope was not reached, because the existing staged swap turned out to need nothing: see the rollback note below.
- **`base_meta.rs` is unchanged**, byte-identical at `4a8eac20294e6d5785a0fb34776731cfab077cdb40685e6bd80255813c1b4b92`. `write_locked` and `promoted_unknown` were already the right primitives, so the helper seam needed no extension there.

## Why the record goes first, and the rollback boundary

This is the part worth reviewing carefully, because it decides whether a failure can be honest.

`cowfs-core`'s staged swap (`crates/cowfs-core/src/swap.rs`) rolls forward after its point of no return: past the victim removal it records the failure in `last_error` and the call still succeeds. The only `Err` after that point is a failure of the roll-forward itself, which leaves the intent file for the next `Core::open`.

The consequence is a hard ordering constraint.
If the record were invalidated *after* the tree swap, a record publication that failed could not be undone: the old tree would already be gone, so putting the old record back would describe a tree that no longer exists, and leaving it cleared would mean reporting a failure for work that was actually done.

So the order is: invalidate the record, then swap the tree, and if the tree swap returns an error, put the record back.

That is safe for the specific reason the core rolls forward.
A returned `Err` from `promote_base` means the swap rolled back and the old tree is still there, which is exactly the state the restored record describes.
When the swap succeeded, including every roll-forward case, the record stays invalidated, which is the correct end state.
There is no window in which the record describes a tree that is not under the name, and no window in which a successful swap leaves a stale commit.

`restore_base_record` reuses the same shape and the same error-reporting convention as the existing `rollback_base`: if the record cannot be put back either, both failures are reported in one message rather than losing the first.

The record publication happens before any tree operation, so a store whose record cannot be written fails with the old tree and the old record both intact.
That is the case the `bases` write-permission fault proves, and it is asserted on both backends with a reopen in the assertion.

## Old fails, new passes

The regression was written first and run against unmodified production.
`backend.rs` at that moment was sha256 `53892a6ecd60528527e96ba8df9b05e1da5757ea71198be836a63b0a5064a976`, the same blob as `2c219a1` and `93cfef9`, so the old run was against the shipped code and not against a hand-edited variant.

Old result, `9` tests, exit `101`, `4 passed; 5 failed`:

```text
test a_base_keeps_its_own_provenance_when_nothing_is_swapped ... ok
test a_core_swap_clears_the_base_record_it_inherited ... FAILED
test a_path_swap_clears_the_base_record_it_inherited ... FAILED
test a_swap_does_not_adopt_the_source_provenance ... FAILED
test a_swap_does_not_promote_its_target ... ok
test a_swap_from_the_same_source_still_clears_the_record ... FAILED
test a_swap_into_a_name_that_was_never_promoted_invents_no_base ... ok
test a_swap_that_cannot_publish_the_record_fails_and_keeps_the_old_base ... FAILED
test the_existing_swap_refusals_keep_their_record ... ok
test result: FAILED. 4 passed; 5 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.15s
```

The failures name the defect rather than a symptom, for example:

```text
assertion `left == right` failed: core, immediate response: a stale commit survived:
SnapshotInfo { name: "base", parent: Some("srcB"),
  base: Some(BaseMeta { repo: Some("/repoA"), git_ref: Some("refs/heads/main"),
                        commit: Some("commit-AAA") }), created_unix_ms: 1791225981355 }
```

and, for the record-publication case:

```text
core: a swap that cannot publish its record must not report success
```

New result, same 9 tests, `backend.rs` at sha256 `5e8d640b3891731e53f8f90837a5a8614d396ecff31ecfbaacb3af37116a` as committed:

```text
test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.29s
```

## The nine tests and what each binds

Every one of them drives the public `Snapshots` and `Backend` traits against a real private store, binds actual snapshots, and reads actual tree content through `Backend::snapshot`.

1. `a_core_swap_clears_the_base_record_it_inherited`: the primary regression. Asserts the tree really changed and is the source's, the source survives intact, the immediate response and a fresh `create_meta` are both cleared, and a reopened `CoreBackend` agrees.
2. `a_path_swap_clears_the_base_record_it_inherited`: the same on the path backend, with a reopen.
3. `a_swap_does_not_adopt_the_source_provenance`: a source that is itself a published base with `commit-BBB` and `/repoB`; after the swap the target is unknown and the source keeps its own record.
4. `a_base_keeps_its_own_provenance_when_nothing_is_swapped`: the do-nothing control. With no swap the base keeps `commit-AAA`, `/repoA` and `refs/heads/main`, and the two trees are asserted distinct so the control cannot pass vacuously.
5. `a_swap_into_a_name_that_was_never_promoted_invents_no_base`: both backends. The tree is replaced and no record appears.
6. `a_swap_that_cannot_publish_the_record_fails_and_keeps_the_old_base`: both backends. See the fault note below.
7. `the_existing_swap_refusals_keep_their_record`: missing source, same-name, and missing target all keep their existing error kinds, and the base keeps both its record and its tree.
8. `a_swap_does_not_promote_its_target`: swapping from a base into a plain snapshot neither promotes the target nor disturbs the source base's record.
9. `a_swap_from_the_same_source_still_clears_the_record`: resetting a target from the source it already came from still clears rather than reviving the old commit.

## The record-save failure is a real fault, not a mock

Test 6 uses the fault seam that already exists in this crate.
The `base_meta` tests make a base's own record directory read-only to stop a record write, and the probe file here asserts the injection actually took effect before relying on it:

```rust
readonly(&record_dir, 0o500);
let injected = std::fs::write(record_dir.join("probe"), b"x").is_err();
readonly(&record_dir, 0o700);
assert!(injected, "the record directory was never read-only");
```

So a passing run cannot come from a store that was never actually read-only.
No new seam was invented and nothing was guessed: `write_locked` needs write permission on the record directory, which is the existing mechanism.

What the test then proves, on both backends:

- the swap returns `Err`, so it does not report success,
- the old tree is still there, byte for byte,
- the full old record survives: `commit-AAA`, `/repoA` and `refs/heads/main`,
- the source snapshot is untouched,
- a reopened backend reports the same `commit-AAA`, so the live view and the on-disk view agree.

That covers the required "record-publication failure must not return success with stale metadata, and must not destroy the old tree".

## Scoped results at the code commit

One 600 second foreground `mac-heavy.lock` hold, one flock retry budget, isolated `CARGO_TARGET_DIR` and a project-local `TMPDIR`, receipts printed before the batch.
No daemon, socket or mount was needed: the public backend trait is the acceptance surface and no global filesystem claim is made.

| Check | Result | Exit |
| --- | --- | --- |
| `--test swap_provenance_124`, repeat 1 | 9 passed, 0 failed, 2.24s | 0 |
| `--test swap_provenance_124`, repeat 2 | 9 passed, 0 failed, 2.26s | 0 |
| `cargo test -p cowfs-daemon --locked --lib` | 100 passed, 0 failed, 9.39s | 0 |
| `--test namespace_durability` | 3 ignored, 0 failed | 0 |
| `--test snapname_drift` | 4 passed, 0 failed | 0 |
| `cargo fmt -p cowfs-daemon -- --check` | clean | 0 |
| `cargo clippy -p cowfs-daemon --all-targets --locked -- -D warnings` | zero warning or error lines | 0 |

The 100 existing daemon unit tests, which include the base-provenance tests and the namespace atomicity tests, pass against the change.
The 3 ignored `namespace_durability` tests carry the same `#[ignore]` attributes as the base; they are reported as ignored, not as passing.

Three intermediate compile failures during development are part of the honest record and were all in this lane's own code:

- `?` on `cowfs_vfs::Error` into `io::Error` in the throwaway reproduction probe, which is why the probes unwrap instead,
- `&dyn Snapshots` is not itself a `Snapshots`, so the helpers take `&dyn Backend` and reach the namespace through it,
- `Ok(())` inside `bases.exclusive` needed `Ok::<(), io::Error>(())` because several crates in the graph implement `From<io::Error>`.

Clippy also caught two real problems in the new test file, an unused `Snapshots` import and three `drop(s)` calls on a reference, both fixed rather than allowed.

## Source receipts and owned paths

| Path | State | sha256 |
| --- | --- | --- |
| `crates/cowfs-daemon/src/backend.rs` | modified | `5e8d640b3891731e53f8f90837a5a8614d396ecff31ecfbaacb3af37116a` |
| `crates/cowfs-daemon/tests/swap_provenance_124.rs` | added | `db58ba7658b2f4759df0805a9ca3b7a27b44b81a697516937bce4762d3e367c2` |
| `crates/cowfs-daemon/src/base_meta.rs` | unchanged | `4a8eac20294e6d5785a0fb34776731cfab077cdb40685e6bd80255813c1b4b92` |

The reproduction proved the defect on `2c219a1`.
`93cfef9` was verified to carry byte-identical daemon sources before the fix was carried over: `backend.rs` blob `669c8e0d119f5372979f3adfe36c5fa4269e674a`, `base_meta.rs` blob `6c020ae303b7ac3213efe87a0c205629acac12d5`, `import.rs` blob `2021842397473df72bc8aab0562da02e2517c958`, all unchanged between the two commits.
So the defect and the fix are on the same source, and PR #131's merge did not move the ground.

Owned-path diff: `crates/cowfs-daemon/src/backend.rs` 104 insertions, 21 deletions, plus the new test file.
Nothing outside those paths was touched.

Raw artifacts under `bench/out/swap-provenance124-repair/`: `old-run.log` (the failing run), `new-run.log`, `gate-run.log`, `gate.sh`.
The prior reproduction's artifacts under `bench/out/swap-provenance124/` are untouched.

## Limitations

- `swap` was exercised through the public `Snapshots` trait, which is what the handler at `handler.rs:285` and the wire path at `server.rs:983` call. No live socket round trip was driven, so the claim is about the backend contract, not about a socket session.
- The record-save fault is injected by filesystem permissions on the record directory, which is the existing seam. A fault in the in-memory map or a torn write inside `write_locked` is not covered, and `write_locked` is unchanged by this repair.
- Lock ordering is not newly established by this change: the new section nests the record lock outside the core lock, which is the order `create`, `remove` and `rename` already used. No lock-ordering change is claimed and no deadlock is claimed.
- A base that is reset now reports stale rather than fresh until the next `base_refresh`. That is the intended honest state, but it is a behaviour change a caller may notice.
- The three ignored `namespace_durability` tests were not run.

## What needs to happen before this merges

A fresh independent review of the ordering argument in "Why the record goes first", and a real CI run.
CI on this branch was pending, not green, at the time of writing, and no CI result is claimed beyond the table above, which is what actually ran locally.
Issue #124 stays open and no merge is done here: that is the coordinator's call.