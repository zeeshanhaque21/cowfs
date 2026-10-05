# Issue #98: warm-base provenance, published durably and never lost

Task: #98 on top of the issue #17 namespace PR (#92), after the review of `f1529cc`.

## What #98 was

`base refresh` exited 0 and returned a `SnapshotInfo` carrying `repo`, `git_ref` and `commit`, and
persisted none of it. Both backends rebuilt `BaseMeta` from an in-memory set of promoted names with all
three fields `None`, so `find_base` could never match and a caller that reconnected, or a daemon that
restarted, was told the base had no provenance.

The record now lives on disk, one directory per base at `<store>/.cowfs-base-meta/<name>/base.json`,
outside every snapshot subtree so no snapshot's contents, hash or Merkle root change and a clone never
copies it. Written to a temporary file, fsynced, renamed, with the containing directory fsynced.
`Snapshots` gained `set_base_meta`; `base_refresh` promotes, records, then re-reads through `create_meta`
and reports what was stored, so the response can only say published if a later reader would agree. Scope
is `Snapshots` and its two implementations, the publication call, and `base_meta.rs`. No change to
`cowfs-meta`, `cowfs-core`, the store format, the queue, the control server or any atomic-rename path.

## The five defects the review found in that record, and what each one does now

All five are in `crates/cowfs-daemon/src/base_meta.rs` and the callers in `backend.rs`. Each was
measured before and after on the same code, with the same signatures, so the difference is the defect
and not the API.

### 1. A removal that could not happen was reported as a removal that did

`remove` dropped the in-memory entry unconditionally and discarded the error, and returned `()`, so no
caller could learn the deletion failed. With the record's directory un-removable, the live daemon
answered `no warm base` while a daemon reopening the byte-identical store answered with the recorded
commit: one store, two answers.

It reports the failure now, and drops the entry only when the store agrees. `remove_dir_all` unlinks the
record before removing its directory, so a failure can leave either state, and that decides what the map
may claim: file still there means the record is intact and stays; file gone means the store has no
record and the map stops claiming one. A reopened process reads exactly that file, so the two views now
answer the same question the same way.

- `a_removal_that_cannot_unlink_the_record_reports_failure_and_keeps_one_answer`: the base's own
  directory is made read-only, so nothing is removed. Asserts the error names what did not happen, the
  record is still on disk, and the live and reopened views agree.
- `a_removal_that_lost_the_record_does_not_keep_claiming_it`: the other branch. The record file is
  removed and the directory cannot be removed. Asserts the map stops claiming a record the store lost,
  and never invents a fresh one.
- `a_removal_whose_record_cannot_be_deleted_is_reported_and_loses_no_commit`, at the namespace level:
  the same, plus that a new snapshot of the same name does not inherit the surviving commit.

Before: `a_removal_that_cannot_unlink_the_record_reports_failure_and_keeps_one_answer` failed with
`unwrap_err()` on `Ok(())`, and so did `a_removal_whose_record_cannot_be_deleted_is_reported_and_loses_no_commit`.

### 2. A rename deleted the source before writing the destination

`rename` removed the source first, so any failure writing the destination lost the provenance: gone from
memory by the removal, gone from disk if the removal succeeded. The snapshot survived, so this was not
data loss, but the base silently dropped the commit that made it a base.

The destination is written and fsynced first, so the record is never absent. If the source then cannot
be removed, both records exist and the map says so, which is what a reopened process reads. A destination
already holding a *different* record is refused rather than overwritten, because overwriting destroys a
record this call did not write.

- `a_rename_that_cannot_write_the_destination_keeps_the_source_byte_identical`: the metadata root is made
  read-only so the destination cannot be created. Asserts the refusal names `warmer`, the source record is
  byte-identical, the live and reopened views agree, and no destination record was left behind.
- `a_rename_never_overwrites_a_different_destination_record`.
- `a_rename_whose_record_cannot_be_moved_reports_failure_and_changes_nothing`, at the namespace level:
  no data loss (the tree and its contents are where they were), no dangling destination in the store or
  the records, and the source record byte-identical.
- At the namespace level the record moves *before* the snapshot, and a snapshot rename that fails puts
  the record back, reporting it if even that fails.

Before: `a_rename_that_cannot_write_the_destination_keeps_the_source_byte_identical` failed with
`unwrap_err()` on `Ok(())`, and `provenance_follows_a_rename_and_is_forgotten_by_a_remove` failed with
"a rename must not lose where the base came from".

### 3. The temporary file name was per process, so concurrent publication collided

`base.json.tmp<pid>` is the same path for every thread in one daemon. Two threads publishing one base
wrote the same file and renamed from it, and `File::create` truncated what the other was writing.

The name is unique per call now, created with `create_new` so even a wrapped counter cannot truncate a
temporary file another call is writing, and the store's own lock is held from reading the map through
publishing the file and updating the map, so a read, a write and a cache update cannot interleave.

- `concurrent_writes_to_one_base_publish_only_complete_records`: 8 threads, 25 publications each, with a
  repository path large enough that a write is not instantaneous, and a reader thread. Asserts no observed
  record ever mixes two publications, the surviving record is one whole publication and survives a
  reopen, and no temporary file is left behind.
- `concurrent_writes_to_different_bases_do_not_interfere`: 8 bases in parallel, each surviving a reopen.

Before: **10 failures in 10 runs**, every one of them a publication that failed outright with
`NotFound`, because another thread had already renamed the shared temporary path away. Captured in
`bench/out/namespaces17-p98-repair/old-semantics-concurrency.log`.

### 4 and 5. The documentation claimed a path and a property the code did not deliver

The module comment said the record is at `<store>/.cowfs-base-meta/<name>.json`. It is at
`<name>/base.json`. It also claimed the metadata root is "invisible to snapshot listing and to the mount
root on both backends". The listing half is true and load-bearing: a dot prefix is not a valid snapshot
name, so `snapshot list` cannot return it and it cannot collide with a snapshot. The mount-root half was
false: on the path backend a client that lists the mount root sees `.cowfs-base-meta` and can read
through it.

Corrected to say what is true. The visibility is not fixed, on purpose: the store is single-user with no
encryption, and the contents are a repository path, a ref and a commit, all of which the working tree
already shows. No hidden-filter feature was added.

### 7. `remove` and `rename` accepted a name that could leave the metadata root

`set` validated; `remove` and `rename` did not. They now reject an empty name, `.`, `..`, a name
containing a separator or a NUL, and a record directory that is a symbolic link, so nothing outside the
store can be written or read through the metadata root. `remove` cannot use the snapshot-name validator,
because `import` also removes and renames its private `.cowfs-import-<name>` staging snapshots, which are
not valid snapshot names; `rename` validates its destination with it, since a destination record is a
base name.

Covered by `remove_refuses_a_name_that_could_leave_the_metadata_root`, and
`an_invalid_snapshot_name_is_refused` now also asserts `set` refuses a staging name.

### Also: a snapshot never inherits a record that outlived it

An interrupted operation can leave a record behind with no snapshot. `create` now clears any such record
before the new snapshot exists, on both backends, because adopting it would report a tree as a base built
from a commit it has nothing to do with. That is the one failure this must never produce, and
`a_recreated_snapshot_is_never_a_base_even_when_a_record_outlived_it` asserts it for both backends using a
record written straight to the store with no snapshot behind it.

## The Linux acceptance is now re-runnable in place

Every run takes an immutable attempt directory, `REPO_ROOT/bench/out/ns17/a<mmddThhmmss>-<pid>`, holding
its own store, mount point, control socket, daemon, treehouse HOME, canonical directory, fixture
repository and logs. Nothing outside it is created or removed except the shared cargo build cache, and an
earlier attempt is never read, written or removed. Previously the script recreated the fixture repository
while the store persisted, so the second run in a checkout failed on the seed import collision.

The path components are short on purpose. A Unix socket address is at most 107 bytes, the first version of
this pushed the control socket to 132 and both runs died with `path must be shorter than SUN_LEN`, and the
length is now checked before a daemon starts. Those two failures are preserved as attempt directories
rather than deleted.

Two runs, back to back, in the same checkout, real Linux, path backend, real FUSE mount:

```
S1 EXIT=0  VERDICT: PASS  attempt=a1005T010623-1099994
S2 EXIT=0  VERDICT: PASS  attempt=a1005T010817-1105679
```

Each run published its own warm base at its own commit, and each was re-read by a daemon that never saw
the refresh in memory:

| | S1 | S2 |
|---|---|---|
| published commit | `ff2ad7c1b81758c331bf206c07f8e82e31b32726` | `9de040d8c8b71131118946d82ef3fd9ad5799b8f` |
| same commit after reopen | yes | yes |
| base snapshot | `repo-608477-base` | `repo-dd570c-base` |
| worktrees after refresh | 1, clean tree, HEAD unchanged | 1, clean tree, HEAD unchanged |
| canonical builds | byte-identical, native controls differ | same |

S1's own files still hash and timestamp exactly as they did when S1 finished, and both attempts'
directories are on disk afterwards. Raw logs in `bench/out/namespaces17-p98-repair/{s1,s2}/` and
`rerun-twice.log`.

## Counts, measured on this machine

Toolchain `rustc 1.99.0 (b940084d7 2026-09-28)`, `clippy 0.1.99`. Each exit captured directly, no
pipeline in a status position.

| Suite | Result | Exit |
|---|---|---|
| `cargo test -p cowfs-daemon --lib` | `running 82 tests`, 82 passed | 0 |
| `cargo test -p cowfs-daemon` | 82 lib, 0 main, 5 ignored in `end_to_end`, 0 doc | 0 |
| `cargo test -p cowfs-treehouse --test canonical`, macOS | `running 10 tests`, 10 passed | 0 |
| `python3 -m unittest discover -s bench -p test_namespaces.py`, macOS | `Ran 17 tests`, `OK (skipped=10)` | 0 |
| `cargo fmt --all --check` | clean | 0 |
| `cargo clippy -p cowfs-daemon --all-targets -- -D warnings` | clean | 0 |
| `cargo clippy -p cowfs-treehouse --all-targets -- -D warnings` | clean | 0 |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean | 0 |

`cowfs-daemon` lib tests, by module: `backend` 17, `base_meta` 15, `import` 14, `handler` 14, `exports`
13, `mounts` 3, `holders` 3, `daemon` 2, plus 1 store probe, 82 in total.

The baseline is **55** at `e243cb1`, measured by checking those three files out at that commit and running
the same command, so this change adds 27. The review reported 54 at `e243cb1` and 70 at `f1529cc`; its
delta of 16 for the same interval matches my 55 to 71, so the difference is one test in the baseline and
it is in the baseline, not in this change. I did not reconcile which one, and I did not touch the
pre-existing tests.

## The workspace clippy gate

The review reported this red, in `crates/cowfs-meta/src/tx.rs:314`, a `collapsible_match` on an `if`
nested in a `match` arm. I could not reproduce it: `cargo clippy --workspace --all-targets -- -D warnings`
exits 0 here, and so does `cargo clippy -p cowfs-meta --all-targets -- -D warnings`. The code it points at
does still contain that shape (`crates/cowfs-meta/src/tx.rs:312`), so the lint most likely does not fire
on clippy 1.99.0. That file is not mine and is not in this diff, and I did not edit it. Reported as a
discrepancy to resolve, not as a fixed gate and not as a claim that the review was wrong.

## ETXTBSY: still a merge BLOCK, still not worked around

Unchanged by any of this. The characterisation from the previous round stands, corrected: on Linux
6.12.109+rpt-rpi-2712 aarch64, 1600 execs per row, write-then-exec over `posix_spawn` fails 40 times on
tmpfs and 73 on ext4, the same loop over `fork` + `exec` fails once, and a loop that writes nothing at all
fails zero times at eight threads and zero at one thread. It needs a write, concurrency, and the
`posix_spawn` path.

What is deliberately not claimed: that `crates/cowfs-treehouse/src/mode_b.rs` contains a `posix_spawn`
call. It does not; it builds the helper process with `std::process::Command`, and there is no explicit
`posix_spawn` in that source. On Linux Rust's `Command::spawn` uses `posix_spawn(3)` when it can avoid a
fork, so the tests and production share a path, but that is a statement about the standard library, not
about this source. Nor is any kernel bug claimed: the mechanism is not established, and the review's own
sample is 1 in 12 with 0 of 6 at one thread, which is too few to conclude that serialising helps.

Not retried, not ignored, not given a longer timeout, not serialised. Status: BLOCK, root cause
unconfirmed. It stays independent of everything above.

## Not claimed

- The core backend under a real mount. `base_refresh` refuses directory ingest on the core by design, so
  the core path here is the runtime reopen tests, on a real `CoreBackend` over a temp store.
- `fsck`, crash durability or power-loss behaviour. The record is fsynced and renamed, which is a
  mechanism, not a measurement. No crash injection was run.
- Deduplication or warm-base build-time benefit. Nothing here measures any.
- btrfs, XFS, kernels older than this host's, or GitHub Actions. The measurement is one kernel, one host.
- That `origin/main` at `724f81c` is merged here. It is not, and this branch was not rebased or merged.
- The ETXTBSY flake.

## Evidence

All gitignored, under `bench/out/namespaces17-p98-repair/`:

```
s1/, s2/                  both runs' logs, commits, hashes, embedded paths, readback, worktree listing
rerun-twice.log           S1 EXIT=0 PASS then S2 EXIT=0 PASS in one checkout
old-semantics.log         the five defects' tests failing against the old behaviour: 67 passed, 15 failed
old-semantics-concurrency.log   the shared-temporary-path failure, 10 runs in 10
run-twice.sh              the two-run driver, so the rerun is reproducible
```