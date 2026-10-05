# meta42-snapshot-rename: `Meta::rename_snapshot` moves a name in one transaction and keeps the id

Lane: `.treehouse-ready-wave/.treehouse/cowfs-7c1bf8/5/cowfs`, READY5.
Base: merged `main` at `93cfef94457a989d031cb6b0a475ac4edbdb85ef`.
Head `bf37fdf6d0a8512137bb551ae00c8b2580ac9619`, branch `fix/meta-snapshot-rename-42`, pushed over HTTPS.
Host: Apple M3 Max, macOS 26 aarch64, `rustc 1.99.0` / `cargo 1.99.0`.

## Verdict, and what it is not

Request 1 of issue #42 is delivered as a staged metadata API.

This is **not** completion of #42 and not a consumer integration.
`cowfs-core` still stages its own rename through `crates/cowfs-core/src/swap.rs`, so nothing outside `cowfs-meta` changes behaviour.
The core integration remains an explicit follow-on inside the same existing request, and no new issue was created for it.
Requests 2 to 5 of #42, the shared name-rule crate, the hole flag, inode reservation and `batch_at`, are untouched.

## The API, exactly as it stands

```rust
pub fn rename_snapshot(&self, id: SnapshotId, new_name: &str) -> Result<()>
```

`SnapshotId` and `Error` are already re-exported from `cowfs_meta`, so no signature change, no new re-export and no `lib.rs` edit was needed.

What it guarantees, and what each guarantee is pinned by below:

- the `SnapshotId` does not move, so the tree, its Merkle root and every inode number in it are the same object before and after,
- a `Snapshot` handle taken before the rename stays usable and reports the new name,
- the old name stops resolving and the new one resolves to the same id,
- the name is moved in both the snapshot table and the name table inside one write transaction,
- a name held by a **different** snapshot is refused with `Error::SnapshotExists`, and neither snapshot is replaced, removed or renamed,
- a `SnapshotId` that is not present is `Error::NoSuchSnapshot`,
- an empty name, or one longer than `u16::MAX` bytes, is `Error::Invalid`,
- renaming to the name the snapshot already has is a successful no-op that writes nothing.

Explicitly preserved and asserted: the `SnapshotId`, the Merkle root, every inode number, the creation time and parent, the inode reservation high-water mark, the reap queue and the `next_snapshot` counter.

## How it reuses the existing seam

No new transaction framework. The change is one arm on the existing `Extra` enum plus one `Inner` method and one public method.

- `Extra::Rename { id, name }` is validated in `commit`'s existing `match &extra` block, in the same place `Extra::Add` and `Extra::Remove` are, so it gets the same refusals, the same `check_writable`, the same `run_hook` and the same `guard`.
- Inside the existing single `begin_write` transaction, the rename rewrites the snapshot row with the new name, removes the old key from the name table and inserts the new one. That is the same table pair `Extra::Add` and `Extra::Remove` already touch.
- After `wtx.commit()` returns, and only then, the session mirrors move: `s.names` and the entry's `info.name`. This mirrors `Extra::Add` and `Extra::Remove`, which also update the session after the transaction rather than inside it.
- The same transaction still writes the roots of any dirty snapshot, because a rename is a commit like any other. A snapshot with uncommitted work therefore has that work flushed rather than dropped, which is a test below.

Nothing else was touched: no schema change, no on-disk version bump, no index, no `tx.rs`, no `types.rs`, no `walk.rs`, no `check.rs`, no `store`, no `Core`, no daemon, no `base_meta`, no dependency and no lock change.

## Meta's name rule versus Core's, stated rather than papered over

This reuses meta's own rule, the one `Inner::add_snapshot` already applies: empty, or longer than `u16::MAX` bytes, is `Error::Invalid`.

Meta has no `name_key`, no Unicode normalisation and no case folding. Neither does its existing `new_snapshot`. That is deliberate here: a rename must not be able to produce a name that `new_snapshot` would have refused, so it uses exactly the same predicate.

`cowfs-core` has a different and stricter rule in `crates/cowfs-core/src/snapname.rs`, with NFC-lowercase-NFC collision keys, a leading-dot refusal and control-character refusal, and it is currently a **copy** of `cowfs-ctl`'s validator. Request 2 of #42 is the one that unifies those.

So the difference is real and it is unchanged by this commit: a rename in `cowfs-meta` accepts names that `cowfs-core` would refuse.
That is not a new security bypass and not a new policy, because nothing in `cowfs-meta` ever applied Core's rule, including `new_snapshot`.
It does mean that until request 2 lands, a name admitted here is not guaranteed admissible by a Core consumer, and that is a property of the staged delivery rather than a defect in it.

No new normalised index and no new read-side consumer policy was invented.

## Old versus new, and what the old comparison can and cannot show

The same test file was compiled against unmodified `main` in a private archive at `bench/out/meta42-snapshot-rename/old-main`, whose `db.rs` is sha256 `de79c2713fbb01489c7895d51b0c5c2b52229ea8da86db552d8d41532ef38da1`, the same blob `main` carries.

The old result is exit `101` and it is a **compile** failure, not a runtime one:

```text
error[E0599]: no method named `rename_snapshot` found for struct `Meta` in the current scope
help: there is a method `snapshot` with a similar name, but with different arguments
```

That is the honest form of the old result, and it is stated as such: the API did not exist, so no runtime assertion could have failed and none is claimed.

Against `Core`, the source-known fact is that `Core::rename_snapshot` forks twice through the staged swap and therefore changes the snapshot id, and so every inode number in it, twice. That is read from `crates/cowfs-core/src/swap.rs` and from `docs/v1-core.md`, which already records it.
It was **not** reproduced here as a comparison, because doing so would need a Core consumer of the new API, which is exactly the follow-on this branch deliberately does not contain.

## The fault scope, specified rather than generalised

The `before_sync` test uses the hook that already exists at `crates/cowfs-meta/src/db.rs:89`, where returning an error means the durable commit does not happen.

The fault point is stated, not assumed: the hook is armed with an atomic flag only *after* the snapshot has been created, populated and synced, so nothing in the setup can trip it, and the next durable commit is the rename's own transaction.

What that proves: at that fault point the rename reports the failure, and the rows, the open handle, the live namespace and the reopened store are all exactly as they were. The hook fired is asserted, so the test cannot pass without the fault having been injected.

What that does **not** prove, stated plainly: that every error after a commit is universally rolled back.
No such claim is made, and no power-loss or crash-injection matrix was run.

## The eight tests

`crates/cowfs-meta/tests/snapshot_rename.rs`, new, 8 tests, all driving the public `Meta` API on private `tempfile` stores.

1. `a_rename_keeps_the_id_the_root_the_numbers_and_an_open_handle`: the primary contract. Asserts the id, the root and the content identity are unchanged on the open handle, that the old name stops resolving, that the new one gives the same id, that exactly one snapshot exists afterwards, and that after dropping everything and reopening the store the new name, id, root and content identity are all still there.
2. `a_rename_does_not_move_the_next_id_or_the_inode_floor`: a snapshot created after the rename gets a fresh id rather than reusing the renamed one, and a later create still gets an inode above everything handed out before the rename.
3. `renaming_to_the_current_name_is_a_no_op`: twice, still one snapshot, same id, same root, same content, and the name survives a reopen.
4. `a_name_held_by_another_snapshot_is_refused`: `SnapshotExists`, both snapshots keep their own names, ids and roots, and both survive a reopen.
5. `a_missing_id_and_an_invalid_name_are_refused`: `NoSuchSnapshot` for an absent id, `Invalid` for empty and for over-long, with the whole `snapshots()` vector byte-equal before and after, live and reopened.
6. `a_rename_of_a_dirty_snapshot_keeps_its_uncommitted_writes`: an uncommitted create is still there after the rename, on the open handle and after a reopen, with the inode number it was handed.
7. `a_before_sync_failure_at_the_rename_commit_changes_nothing`: as described above.
8. `a_rename_frees_the_old_name_and_keeps_the_new_one_busy`: the old name becomes usable for a new create with a fresh id, while the new name stays held.

Content identity is compared through `chunks` and `content_version`, which is the content identity meta actually exposes on the public API.
`Snapshot` has no read method, so comparing file bytes was not available and was not faked; equal chunk lists and equal content versions are the same bytes.

## Scoped results at the head

One 600 second foreground `mac-heavy.lock` hold, one flock retry budget, isolated `CARGO_TARGET_DIR` and a project-local `TMPDIR`, receipts printed before the batch.
The representative public sample was run before the expanded suite.

| Check | Result | Exit |
| --- | --- | --- |
| `--test snapshot_rename`, representative single test first | 1 passed, 0.16s | 0 |
| same file against unmodified `main` | compile failure, API absent | 101 |
| `--test snapshot_rename`, repeat 1 | 8 passed, 0 failed, 1.00s | 0 |
| `--test snapshot_rename`, repeat 2 | 8 passed, 0 failed, 1.02s | 0 |
| `cargo test -p cowfs-meta --locked --lib` | 16 passed, 0 failed | 0 |
| `--test health` | 7 passed, 0 failed | 0 |
| `--test recovery40` | 4 passed, 0 failed | 0 |
| `--test critic` | 12 passed, 0 failed | 0 |
| `--test posix` | 16 passed, 0 failed | 0 |
| `--test model` | 2 passed, 0 failed | 0 |
| `--test crash` | 2 passed, **1 ignored** | 0 |
| `cargo fmt -p cowfs-meta -- --check` | clean | 0 |
| `cargo clippy -p cowfs-meta --all-targets --locked -- -D warnings` | zero warning or error lines | 0 |

The `health` and `recovery40` suites are included because they are the #40 suites that read snapshot rows, the reserved inode block and the recovery floors, and this change moves a row between two tables.
They pass unmodified, which is the evidence that the existing row, health and recovery semantics are intact.
The 1 ignored `crash` test carries the same `#[ignore]` as the base and is reported as ignored, not passing.

Three intermediate failures during development are recorded because they are this lane's own code, all in the test file and all fixed rather than worked around: reading a snapshot through a `read` method meta does not have, opening two backends over one store, and keeping a `Snapshot` handle alive across a reopen.

## Scope, receipts, and resource discipline

Owned paths, two files:

| Path | State | sha256 |
| --- | --- | --- |
| `crates/cowfs-meta/src/db.rs` | modified, 86 insertions | `9f331f905361d88e619d9155d4565b53b78f090edd9c268ec31dcab36aca1026` |
| `crates/cowfs-meta/tests/snapshot_rename.rs` | added, 8 tests | `39bc1792aa072b0ce4da4c8a606106a13959e3146b010f5bbfdaa9eb07d9b639` |

Unchanged and verified: `tx.rs` `5cafb0cc2e10f1e2cde879258ba96c6a41308bd8faf06b402298824aeba18688`, `types.rs` `154bc7951a41eb63641ef5ebdfd5a3fac5aa6a37f26876ae4c495343c00ef5a9`, `lib.rs` `568b373358d6e155722f3e914f655643b7cc49facfb04c53b16fd1f35eb5b6d0`.
`git diff` over `Cargo.lock` and every crate manifest is empty: no dependency and no lock change.

Artifacts under `bench/out/meta42-snapshot-rename/`: `rep-run.log`, `rename-run.log`, `old-api-absent.log`, `gate-run.log`, `check.log`, `gate.sh`, and the private `old-main/` archive.
The earlier lanes' artifact directories were not touched and nothing was deleted.

Artifact budget was a live constraint: this lane's `bench/out` already held about 8.2 GiB from the #40 and #124 work, which is at the 8 GiB cap, so this task created exactly one cargo target directory and no second full archive of the workspace.
The new directory is about 725 MiB, free disk went from 303.9 GiB to 300.0 GiB against a 20 GiB floor, and no prior artifact was removed to make room.

## Limitations

- No consumer is wired to this API, and `cowfs-core` still stages its own rename. This is a staged API delivery, not request-1 completion and not #42 completion.
- The Core comparison is source-read, not reproduced: `Core::rename_snapshot` changes the id because it forks twice, per `src/swap.rs` and `docs/v1-core.md`.
- Meta's name rule is looser than Core's, as it already was for `new_snapshot`, and that difference is described above rather than fixed here.
- The fault test covers one fault point in one failure class. No universal rollback claim, no power-loss test, no crash-injection matrix, no timing or performance measurement.
- A renamed snapshot is a new name with the same id, so any external consumer holding the old name string by value must be told the new one; that is a consumer concern this branch does not touch.
- No live socket round trip, no daemon, no mount. Browser unverified. `no-mistakes` is uninitialized in this lane and was not initialized.

## What has to happen next

A fresh independent review of this head, and a real CI run.
CI on this head is pending, not green, at the time of writing.
PR is a draft, issue #42 stays open, and the Core consumer integration is the next step inside the same request, owned by whoever holds `crates/cowfs-core/src/lib.rs`.