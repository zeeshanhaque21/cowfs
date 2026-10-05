# ready-42: Core/meta API requests from #42

Task: issue #42, "cowfs-meta and ctl requests from cowfs-core (#26): atomic snapshot rename, shared name rule, hole flag, inode reservation, batch_at".
Lease: `.treehouse-ready-wave/.treehouse/cowfs-7c1bf8/6/cowfs`, branch `followup/core-meta-integration-42`.
Base: `46b0f269d5bef4a2c204c25f5b3015da601d3beb`.
Head delivered: `301fcc226d62e4b8568497fcea1b8b8a92302ccd`.
PR: https://github.com/zeeshanhaque21/cowfs/pull/99
Date: 2026-10-05.

## What was picked, and why this one

#42 lists five requests in priority order.
Four of them change `cowfs-meta`'s on-disk contract or `cowfs-store`'s `ChunkRef`.
One of them is duplication that already exists in two crates.
The smallest item that is *complete* on its own is the shared snapshot-name rule, so that is what this
change delivers; the other four are reconciled below with runnable proof of their current state and are
not implemented here.

`docs/v1-core.md` fixes the semantics, not the code:

- "Snapshot names follow exactly the rule `cowfs-ctl` enforces (`crates/cowfs-ctl/src/validate.rs`),
  which is what the CLI's tests pin".
- "The rules and the test table are copied into one small module, `src/snapname.rs`, so the two can be
  reconciled into a shared crate when the branches land."
- request 5: "A crate for the snapshot-name rule shared by the backend and the control API."

The branches have landed, the copies had already diverged, and nothing reconciled them.

## Reconciliation of all five requests

Every row below was checked against the source at this head and, where a workaround exists, against a
test that was actually run for this task.

| # | Request | Landed? | Runnable proof at this head | Residual |
|---|---|---|---|---|
| 1 | `Meta::rename_snapshot(id, new_name)` | No | `cargo test -p cowfs-core --test swap`, **3 passed, 0 failed**: `promote_base_survives_a_failure_at_every_step`, `rename_snapshot_is_failure_safe`, `a_damaged_block_does_not_stop_a_promote` | `cowfs-meta` has `new_snapshot`, `snapshot`, `snapshot_by_id`, `snapshots`, `Snapshot::fork`, `Snapshot::info` and no rename, so `cowfs-core/src/swap.rs` still stages a fork, writes an intent file, removes the victim, forks again and rolls forward on the next open. The two forks change the snapshot id twice, so every inode number in a renamed snapshot differs from the old one's. |
| 2 | Shared crate for the snapshot-name rule | **Yes, this change** | `cargo test -p cowfs-snapname` **6 passed**; `cargo test -p cowfs-ctl --lib` **10 passed**; `cargo test -p cowfs-core --test names_ino` **4 passed**, which includes the pre-existing `snapshot_names_follow_the_cli_rules` | None. The rule has one owner and `crates/cowfs-daemon/tests/snapname_drift.rs` fails if a second copy comes back. |
| 3 | Hole flag in `ChunkRef` | No | `cargo test -p cowfs-gc --test regressions a_hole_is_never_swept` **1 passed, 0 failed** (19 filtered out); `cargo test -p cowfs-core --test chunks` **4 passed, 0 failed** | `cowfs_store::ChunkRef` is `{ id: BlockId, len: u32 }`, 6 lines, no flags. A hole is an all-zero `BlockId`, and the constant is written twice: `cowfs-core/src/file.rs:14` and `cowfs-gc/src/lib.rs:44`. The walker in meta is not safe on its own, so GC must go through `Core::live_blocks`. |
| 4 | `Meta::reserve_inodes(n)` or `Tx::create_with_ino` | No | `cargo test -p cowfs-core --test names_ino` **4 passed, 0 failed**: `virtual_inode_numbers_are_never_reused_across_a_restart`, `the_virtual_number_reservation_survives_a_crash`, `the_virtual_number_reservation_survives_a_crash_child` | Meta reserves durably inside itself, but a caller still cannot hold a number before the transaction that creates the inode, so `cowfs-core/src/ino.rs` keeps the two-copy durable high-water mark `<root>/virt.ino.a` and `virt.ino.b` plus the session alias table. |
| 5 | `Snapshot::batch_at(now)` or `Tx::set_now` | No | **None run.** There is no test in `crates/cowfs-core/tests/` that names `ctime`, and none was added here, because this change does not touch it. | `cowfs-meta/src/types.rs:71` `Timestamp::now()` is the only clock in meta and `tx.rs` has no override, so ctime of a deferred operation is the batch time, up to `flush_interval` late. atime and mtime are restored with `setattr`. This row is a source-level fact only, not a measurement. |

## The drift that was already real

Both copies existed and both were in the tree at the base commit. They had already diverged in two ways.

**1. The control API accepted a name the backend refuses.**
`cowfs-core/src/snapname.rs` refused any name containing the swap protocol's marker `.cowfs-swap`;
`cowfs-ctl/src/validate.rs` did not.

`cowfs_core::swap::staging_name(target)` is `<first 200 chars of target>.cowfs-swap0`, so
`conf_base.cowfs-swap0` is exactly the staging name of `conf_base`. The backend refused it for that
reason alone: no leading dot, no slash, no control character, 23 bytes.

**2. The same name got two different collision keys.**
`cowfs-ctl` bounded `name_key` by hashing a folded form longer than 255 bytes.
`cowfs-core` returned the folded form unbounded.
126 `U+0130` fold to 378 bytes, so for one name the control API produced `#9df1ddaff6eb...` and the
backend produced `i̇i̇i̇...`.

Both differences are shown by real output in `bench/out/ready-42/old-source-proof.log`.

**No reachable wrong outcome was found from either difference, and none is claimed.**
`Core` enforces the reserved marker twice over, in the name rule and again in
`Inner::check_new_name_except`, so nothing inside core could hold such a name.
`PathBackend` stages under `.cowfs-swap-<name>`, which the leading-dot rule already refused, and
`Core::list_snapshots` filters staging names before the control API ever lists them.
The value of the change is that the rule now has one owner and a regression that fails if a copy
returns, which is what #42 asked for.

## What changed

New crate `cowfs-snapname` owns the rule:

- `NAME_MAX`, `RESERVED` (the marker, moved here from `cowfs-core/src/swap.rs`),
- `validate_snapshot_name`, `validate_snapshot_name_bytes`, `is_reserved`, `name_key`,
- `NameError` with a `why()` for each refusal.

Every `why` string is the wording each crate already produced, so no error message changed.

Both consumers are now a forwarder, with their public signatures and error types unchanged:

- `cowfs-core/src/snapname.rs` keeps `validate_snapshot_name`, `validate_snapshot_name_bytes` and
  `name_key`, mapping `NameError` to `ControlError::InvalidName(why)`. `lib.rs:57` still re-exports them.
- `cowfs-ctl/src/validate.rs` keeps `validate_snapshot_name`, `name_key` and `MAX_NAME_BYTES`, mapping
  `NameError` to `CtlError::invalid` with the same message.

`cowfs-core/src/swap.rs` changed in five lines: `STAGING` is now `cowfs_snapname::RESERVED` and
`is_staging` calls `cowfs_snapname::is_reserved`. Nothing else in that file moved.

`unicode-normalization` is no longer a direct dependency of either crate; both depend on
`cowfs-snapname`, which carries it.

`scripts/mutants.py` mutant `n16_name_rule_allows_staging` patched
`    if name.contains(crate::swap::STAGING) {` in `cowfs-core/src/snapname.rs`, a line that no longer
exists, so the run would have reported `PATCH-FAILED count=0` and silently stopped killing that mutant.
It now patches the reserved check in the shared crate, and `src_of` lets a mutant name a path outside
`cowfs-core/src`. Its focus list is unchanged: it still has to be killed by
`--test critic2b staging_names_are_reserved_for_the_swap_protocol`, which this head passes.

## Proof

### Old fail, new pass

Both runs are the *same* fixture file, `crates/cowfs-daemon/tests/snapname_drift.rs`, against the same
target dir.
The old run is base `46b0f26` with that one file copied in and nothing else changed.

```
base_files=529 differing_from_base=0 []
fixture crates/cowfs-daemon/tests/snapname_drift.rs matches_head=True
```

| build | `snapname_drift` | exit |
|---|---|---|
| base `46b0f26` plus the fixture | **0 passed, 4 failed** | 101 |
| this head `301fcc2` | **4 passed, 0 failed** | 0 |

The four failures on the base, verbatim from the log:

```
a_real_store_holds_exactly_the_names_the_control_api_accepts
  assertion `left == right` failed: "conf_base.cowfs-swap0": the control API says ok, the backend says refused
a_staging_name_is_refused_by_the_control_api_before_it_reaches_the_backend
  must be refused: ()
the_control_api_and_the_backend_produce_the_same_collision_key
  left: "#9df1ddaff6ebdacb9a5b982c6621f10b2366e68ca30db6a79d4cf4376a21990c"
 right: "i\u{307}i\u{307}...i\u{307}"
bytes_from_a_wire_go_through_the_same_rule
  left: false  right: true   ("conf_base.cowfs-swap0")
```

The old build was produced by reverting the tree in place and restoring it from git on exit.
After the run the tree was verified back at the head:
`git diff --quiet HEAD --` reported `tree matches HEAD` and `git status --porcelain` was empty.

### What the fixture actually does

`a_real_store_holds_exactly_the_names_the_control_api_accepts` opens a real store in a tempdir with
`background: false`, then for every name in the table asks both surfaces and requires the same verdict:

```rust
let api = cowfs_ctl::validate_snapshot_name(name);
let store = core.create_snapshot(name);
assert_eq!(api.is_ok(), store.is_ok(), ...);
if api.is_ok() { assert!(core.snapshot_view(name).is_ok(), ...); }
```

It then checks the reserved names are unreachable through `snapshot_view` and `rename_snapshot`, that a
refused rename left `conf_base` in place, and that the store holds exactly the six legal names.
`cowfs-swap` is legal and is asserted to be held: the marker needs its leading dot, so it is not a
reserved name. That row caught a wrong expectation in the first version of this fixture.

**Reopen.** The store is then dropped and reopened from the same directory, and the whole reserved-name
and listing check runs again, so nothing is answered from a cache. A refused name is asserted not to have
been stored after the reopen either.

The table holds no aliasing pair on purpose: two names that fold onto one another are one name, so the
backend would answer `Exists` for a name the control API calls legal.

### Source identity before the matrix

A fresh clone of the lease was made and checked out at the pushed head, then every owned file was compared
by blob hash and by sha256 of the bytes on disk (`bench/out/ready-42/source-hashes.txt`):

```
clone_head=301fcc226d62e4b8568497fcea1b8b8a92302ccd
checkout_head=301fcc226d62e4b8568497fcea1b8b8a92302ccd
paths=10 mismatched=0
```

The same file also records the files other lanes own, each byte-identical to the base:

```
UNTOUCHED crates/cowfs-core/src/ns.rs identical_to_base=True
UNTOUCHED crates/cowfs-core/src/lib.rs identical_to_base=True
UNTOUCHED crates/cowfs-core/src/queue.rs identical_to_base=True
UNTOUCHED crates/cowfs-core/src/inner.rs identical_to_base=True
UNTOUCHED crates/cowfs-core/src/io.rs identical_to_base=True
UNTOUCHED crates/cowfs-core/src/gate.rs identical_to_base=True
UNTOUCHED crates/cowfs-core/src/view.rs identical_to_base=True
UNTOUCHED crates/cowfs-core/src/vfs_impl.rs identical_to_base=True
UNTOUCHED crates/cowfs-daemon/src/import.rs identical_to_base=True
UNTOUCHED crates/cowfs-core/src/ino.rs identical_to_base=True
```

`ns.rs` byte-identical is the #95 elide fix preserved.

### Test matrix

Every exit code read directly, no `$?` from a `head`, `tail` or `tee` pipeline.
Toolchain `rustc 1.99.0 (b940084d7 2026-09-28)`, macOS, at head `301fcc2`.

| command | result | exit |
|---|---|---|
| `cargo fmt --all -- --check` | clean | 0 |
| `cargo test -p cowfs-snapname` | 6 passed, 0 failed | 0 |
| `cargo test -p cowfs-ctl --lib` | 10 passed, 0 failed | 0 |
| `cargo test -p cowfs-daemon --lib` | 48 passed, 0 failed | 0 |
| `cargo test -p cowfs-daemon --test snapname_drift` | 4 passed, 0 failed | 0 |
| `cargo test -p cowfs-core --lib snapname` | 4 passed, 0 failed, 25 filtered out | 0 |
| `cargo test -p cowfs-core --test names_ino` | 4 passed, 0 failed | 0 |
| `cargo test -p cowfs-core --test swap` | 3 passed, 0 failed | 0 |
| `cargo test -p cowfs-core --test chunks` | 4 passed, 0 failed | 0 |
| `cargo test -p cowfs-core --test critic2b staging_names_are_reserved_for_the_swap_protocol` | 1 passed, 0 failed, 27 filtered out | 0 |
| `cargo test -p cowfs-gc --test regressions a_hole_is_never_swept` | 1 passed, 0 failed, 19 filtered out | 0 |
| `cargo clippy -p cowfs-snapname -p cowfs-ctl -p cowfs-core -p cowfs-daemon --all-targets -- -D warnings` | clean | 0 |
| `cargo test -p cowfs-daemon --test snapname_drift` on base `46b0f26` plus the fixture | **0 passed, 4 failed** | 101 |

`cargo fmt --all -- --check` was exit 1 on `3de5583`, the commit before this one, because the fixture was
written by hand.
CI runs that as its first step, so no test would have run for that head.
`301fcc2` is the fmt-only commit that fixed it, and the check above is exit 0 on this head.

### Matched control

`crates/cowfs-core/tests/critic2b.rs::staging_names_are_reserved_for_the_swap_protocol` already asserted
that `cowfs_core::validate_snapshot_name("evil.cowfs-swap0")` and `Core::create_snapshot("evil.cowfs-swap0")`
are both refused.
It passes on this head and on the base, so it is reported as a control, not as the fix.
`snapshot_names_follow_the_cli_rules` in `names_ino.rs` is the pre-existing test that asserts the
relationship this change makes structural; it passed before and after.

### Not run, and why

`cargo test --workspace` was not run.
CI runs it on the pushed head and is the authoritative full-suite check for this change.
`cargo clippy --workspace --all-targets` was not run either; clippy was scoped to the four affected
crates, per the dispatch rule against a full-workspace build.
The mutation run `scripts/mutants.py` was not run end to end; it is a local script and not a CI job.
The one mutant this change moves, `n16`, was not re-run.

A from-scratch rebuild of both trees in fresh target dirs under `bench/out/ready-42/` was blocked: the
shared lane `.treehouse-ready-wave/mac-heavy.lock` was held by another worker for the whole session.
Seven attempts through the dispatch recipe, each waiting the full 600 s, all returned exit 75
"resource lane busy; blocked".
No build was run without the lock.
The proof above therefore comes from the lease's own target dir, which is the same compiler and the same
dependency versions as the readback clone; the readback identity check is git-only and did not need the
lane.

## Overlap and ownership

Owned by this change, nothing else touched:

- `crates/cowfs-snapname/` (new)
- `crates/cowfs-core/src/snapname.rs`
- `crates/cowfs-core/src/swap.rs`, lines 38 to 44 only: `STAGING` and `is_staging`
- `crates/cowfs-ctl/src/validate.rs`, `validate_snapshot_name`, `name_key`, `MAX_NAME_BYTES`
- `crates/cowfs-core/Cargo.toml`, `crates/cowfs-ctl/Cargo.toml`, `Cargo.lock`
- `crates/cowfs-daemon/tests/snapname_drift.rs` (new)
- `scripts/mutants.py`, `src_of` and mutant `n16` only

Deliberately not touched, because another lane owns them:

- #40 metadata recovery counters and health reporting, lease slot 5
- #96 namespace barrier: `cowfs-core/src/{flush,gate,inner,io,view,vfs_impl,ns,queue}.rs`, original builder 13
- #98 warm-base provenance: the `Snapshots`/Path/Core backend repo/ref/commit setter, persistence and
  status reconstruction, original namespace builder 10
- `cowfs-daemon/src/import.rs`, #97
- the ctl shutdown server, #79

One thing learned that is worth writing down: `cowfs_ctl::handler_conformance` is the wrong home for a
backend-specific name rule.
It runs against `PathBackend` as well as the core backend, and `PathBackend` enforces no snapshot-name
rule at all, so adding a reserved-name case there fails `the_handler_passes_the_framework_conformance_suite`.
That attempt was reverted and is not in this change.

## Integration order

This change adds a workspace member and removes two dependency edges that nothing else can conflict with
textually.
It touches `cowfs-core`, so it should be integrated with the #96 namespace lane and retested on the exact
post-merge head, together with #98's provenance work and #40's counters.
Nothing here changes a rule `Core` enforced, so the order between them does not matter for correctness.

## Evidence

Raw logs, ignored by git, in `bench/out/ready-42/` of the lease:

| file | what |
|---|---|
| `old-source-proof.log` | base-source run, the four failures, and the restore verification |
| `source-hashes.txt` | fresh-clone readback, owned blobs, and the untouched other-lane files |
| `summary-incremental.txt` | per-step expected and actual with exit codes |
| `incr-*.log` | one log per step |
| `matrix.sh`, `incremental.sh`, `old-proof.sh` | the exact commands, rerunnable |