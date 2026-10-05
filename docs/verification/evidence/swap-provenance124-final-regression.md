# swap-provenance124-final-regression: the uncertain-error branch is now a shipped test, and the refusal order is back to what it was

Lane: `.treehouse-ready-wave/.treehouse/cowfs-7c1bf8/5/cowfs`, READY5.
Base: merged `main` at `93cfef94457a989d031cb6b0a475ac4edbdb85ef`.
Head `363268bad085b12a25c2f74d4b2ba28dafd1ee3e`, on top of the scoped-pass head `8264ca73238eb5fb7d567946f8699fd188b77a60`.
Branch `fix/swap-provenance-124`, pushed over HTTPS.
Host: Apple M3 Max, macOS 26 aarch64, `rustc 1.99.0` / `cargo 1.99.0`.
Scoped-pass review this answers: `docs/reviews/pr134-swap-provenance124-recovery-final.md`, sha256 `ccc5eafccea4cb32606e49aec4eab4a7c8a124be4afdd22069c54276e6c34e50`, committed on this branch byte for byte.

## Verdict

Both items the scoped pass left are done, and nothing else changed.

The daemon's uncertain-error branch now has a permanent test that lives in the repository rather than in a reviewer's private probe, so a later change to `swap` cannot reintroduce a restore without a test catching it.
The same-name refusal no longer drifts ahead of the existence check, so both backends answer the same input the way `promote_base` always answered it.

One receipt is superseded on exactly one point, and it is not rewritten.
`swap-provenance124-recovery-repair.md`, sha256 `cfeffa7b35644feeef07296b223224f9b7e20db782ae4795f94b37a048e3bf2a`, disclosed that the daemon branch was not driven end to end.
That gap is now closed, so the disclosure is stale while every other statement in it stands.

## The permanent uncertain-error regression

The critic proved this branch is reachable and, importantly, that it does not need production change.
`CoreBackend` exposes no meta-options hook, so a fault cannot be injected through it, but `CoreSnapshots`' two private fields at `backend.rs:313` and `Core::open_with_meta` at `crates/cowfs-core/src/lib.rs:198` are both reachable from inside the crate.
So the test is in the existing `#[cfg(test)] mod tests`, uses the production struct and the production `swap`, and adds no public API, no hook and no framework.

The fault is `cowfs_meta::Options::before_sync`, the existing documented hook where returning an error means the durable commit does not happen.
`open_with_meta` wires the store's own sync hook into the options it hands `make_meta`, so the test *wraps* that hook rather than replacing it, and the store's durability ordering is kept.
Only the third durable commit fails, which is the fork of the staging snapshot into the target, the one point past the staged swap's point of no return that leaves a pending intent.

The assertions, in order, each one a real production path:

- the swap returns `Err`,
- `<store>/swap-base` still exists, so the failure did land past the point of no return and the test is not vacuous,
- reopening through the production `CoreBackend::open` runs `swap::recover` and installs the new tree, asserted as `BBBB-from-srcB`,
- the intent file is gone, so the reopen really finished the swap,
- `commit`, `repo` and `git_ref` are all `None`, so the record cannot describe a tree it did not produce.

### Runnable old-fails-new-passes

The unsafe head was rebuilt as a private archive of `4646f513a02bb9b3256920d138a9f01551e35c34` into `bench/out/swap-provenance124-final-regression/old-4646`, its `backend.rs` verified still carrying `restore_base_record` three times, and the three new tests grafted into its existing `mod tests`.
The grafted file is sha256 `4406c31011b693b0ed945f09964ee935a586438e99aecee417fcd872afa85d06`; the untested unsafe production file inside it is sha256 `5e8d640b3891731e53f8f90837a5a8614d396ecff31ecfbaacb3af37116a`, the receipt the critic also measured.

Unsafe head, same test, exit `101`:

```text
panicked at crates/cowfs-daemon/src/backend.rs:1293:9:
assertion `left == right` failed: the record still names a commit that did not produce this tree:
SnapshotInfo { name: "base", parent: None,
  base: Some(BaseMeta { repo: Some("/repoA"), git_ref: Some("refs/heads/main"),
                        commit: Some("commit-AAA") }), created_unix_ms: 1791232128794 }
  left: Some("commit-AAA")
 right: None
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 102 filtered out; finished in 0.36s
```

Corrected head, same test, `backend.rs` at sha256 `23149848688e104d336fcbfd7ab0f2d89dfbecf7fb234e18720e3ec39d0974c5`, exit `0`:

```text
test backend::tests::a_core_swap_that_fails_past_the_point_of_no_return_never_restores_the_old_commit ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 102 filtered out; finished in 0.98s
```

So the discrimination is real and it is the state #124 was filed for, on both heads, from the same test and the same fault.
The representative sample was run before the expanded suite, as asked.

## The refusal order, restored rather than changed

The scoped pass found that my same-name check ran before the existence check, so `swap("nosuch", "nosuch")` answered `InvalidInput` on the core backend and `NotFound` on the path one, where `promote_base` had always answered `NotFound`.

This is previous semantics, not new behaviour, so it is restored rather than redesigned.
`crates/cowfs-core/src/lib.rs:359` looks the source up first, so existence-before-same-name is the order the core has always used, and matching it also makes the two backends agree.

The existence preflight moved above the same-name refusal, inside the same `bases.exclusive` section, with the reason written down at the call site.
No other mutation was reordered, and no provenance restoration was reintroduced.
The uncertain-error contract is unchanged: once a swap is admitted, an error leaves the provenance invalidated.

Two narrow tests cover it:

- `a_swap_of_a_nonexistent_name_with_itself_is_not_found_on_both_backends` asserts `NotFound` from both backends.
- `a_refused_swap_keeps_the_full_old_tree_and_record_on_both_backends` asserts, on both backends, that a same-name refusal and a missing-source refusal both leave `commit-AAA`, `/repoA` and `refs/heads/main` intact and the tree byte-identical.

Both of those pass on the unsafe head as well, which is the point: they pin behaviour that the refusal reordering had to preserve, not behaviour this commit introduced.

## The nine extended integration tests are unchanged

`crates/cowfs-daemon/tests/swap_provenance_124.rs` is byte-identical at `db58ba7658b2f4759df0805a9ca3b7a27b44b81a697516937bce4762d3e367c2` and still passes 9 of 9, including the do-nothing control, the deliberate non-adoption of source provenance, the never-promoted target, the pre-mutation record-save failure, and the existing refusals.
The new coverage goes in the in-crate module instead, because that is the only place the private fields are reachable.

## Scoped results at the head

One 600 second foreground `mac-heavy.lock` hold, one flock retry budget, isolated `CARGO_TARGET_DIR` and a project-local `TMPDIR`, receipts printed before the batch.
No daemon, socket or mount was needed; every store is a private `tempfile` one.

| Check | Result | Exit |
| --- | --- | --- |
| `a_core_swap_that_fails_past_the_point_of_no_return_never_restores_the_old_commit` | 1 passed, 0.36s | 0 |
| `a_swap_of_a_nonexistent_name_with_itself_is_not_found_on_both_backends` | 1 passed, 0.29s | 0 |
| `a_refused_swap_keeps_the_full_old_tree_and_record_on_both_backends` | 1 passed, 0.30s | 0 |
| same three on the unsafe head `4646f51` | 1 failed with `commit-AAA`, 2 passed | 101 |
| `cargo test -p cowfs-daemon --locked --lib` | **103 passed**, 0 failed, 9.52s | 0 |
| `--test swap_provenance_124` | 9 passed, 0 failed, 2.31s | 0 |
| `--test snapname_drift` | 4 passed, 0 failed | 0 |
| `cargo test -p cowfs-core --locked --test swap` | 3 passed, 0 failed | 0 |
| `--test namespace_durability` | 0 passed, **3 ignored**, 0 failed | 0 |
| `cargo fmt -p cowfs-daemon -- --check` | clean | 0 |
| `cargo clippy -p cowfs-daemon --all-targets --locked -- -D warnings` | zero warning or error lines | 0 |

The daemon lib suite was 100 tests before this commit and is 103 now, which is exactly the three added.
The 3 ignored `namespace_durability` tests carry the same `#[ignore]` as the base, mount a filesystem and kill a daemon, and are reported as ignored rather than passing.

Three intermediate failures during development are recorded because they are part of the honest history of this lane's own code: the `--locked` flag refusing the new dev-dependency before the lock was written, one shared store directory between two backends producing a genuine "store is open elsewhere", and a duplicated base setup colliding with the seeder. All three were in this lane's test code and are fixed, not worked around.

## Scope, receipts and the one lock line

Owned paths, three files:

| Path | State | sha256 |
| --- | --- | --- |
| `crates/cowfs-daemon/src/backend.rs` | modified | `23149848688e104d336fcbfd7ab0f2d89dfbecf7fb234e18720e3ec39d0974c5` |
| `crates/cowfs-daemon/Cargo.toml` | modified | `a5b24c11f67170bc6904ab97d58014d54ab45089f08abc3ffe08a846f6b67a4f` |
| `Cargo.lock` | modified | one added line, `"cowfs-meta"` |

`crates/cowfs-daemon/src/base_meta.rs` is unchanged at `4a8eac20294e6d5785a0fb34776731cfab077cdb40685e6bd80255813c1b4b92`, `crates/cowfs-core/src/swap.rs` and `crates/cowfs-core/src/lib.rs` are unchanged, and `crates/cowfs-daemon/tests/swap_provenance_124.rs` is unchanged.
No new metadata format, no new API, no core change, no fault framework, no version bump, no formatting churn outside the lines this change owns.

The `Cargo.lock` line is worth stating plainly because it is the only non-`backend.rs` change that touches anything.
`cowfs-meta` joins `cowfs-daemon`'s **dev**-dependencies so the in-crate test can name `cowfs_meta::SyncHook` and `cowfs_meta::Meta`.
It is already a workspace member at a pinned version, so the diff is one line adding it to that crate's dependency list and no version anywhere changes.

Raw artifacts under `bench/out/swap-provenance124-final-regression/`: `old-run.log` and the three per-test old logs, `new-run.log`, `new-run2.log`, `gate-run.log`, `gate.sh`, and the private `old-4646/` archive.
Earlier lanes' artifact directories are untouched.

## Limitations, unchanged and restated

- **The path backend's retired-tree handling is still source-level only.** No `std::fs::rename` fault seam exists in this repository, so no runtime counterexample is claimed for it. The review's analysis of the deletion sites, that `.cowfs-swap-` and `.cowfs-retired-` both begin with a dot that `validate_snapshot_name` refuses, is what backs it.
- The permanent regression covers the `before_sync` failure class at one commit index. It proves the daemon's branch is safe for a failure past the point of no return with a pending intent. It does not claim to cover every such failure.
- The test wraps the store-durability hook rather than replacing it, and makes no durability-ordering claim.
- The contract still costs provenance on a swap that genuinely rolled back, which now reports stale rather than fresh. Unchanged and disclosed.
- No live socket round trip, no mid-GC, power-loss or full-filesystem acceptance, no runtime frequency or core blame claim, no timing or performance measurement. Browser unverified. `no-mistakes` is uninitialized in this lane and was not initialized.
- The same-name refusal ordering was restored to the core's historical behaviour; I did not check whether any caller depended on either kind, so that remains unexamined.

## What happens next

The same critic lane does a final delta review on this head.
CI on this head is pending, not green, at the time of writing; the three green checks the scoped pass read for `8264ca7` say nothing about `363268b`.
Issue #124 stays open, PR #134 stays a draft, and nothing is merged or closed here.