# Compact proof: a refused duplicate create costs the caller nothing

Companion to `docs/verification/base-provenance98.md`, for the repair of the Core `create` defect that
the review of `6929e63` found. Everything here was measured; nothing is projected.

Branch: `feat/linux-namespaces-17`. Code under test: `5b23655`.
Platform for every number below: macOS, `rustc 1.99.0 (b940084d7 2026-09-28)`, `clippy 0.1.99`, except
where a row says Linux.

## The defect

`CoreSnapshots::create` cleared the base record before the core refused a duplicate name, so
`snapshot create warm` on a base named `warm` exited 1 and left the snapshot with no base at all: the
record gone from the store and from the map, live and after a reopen. `PathSnapshots::create` refused
first and lost nothing. Neither the handler nor the control server pre-checks existence, so on the core
backend the deletion came first and the refusal second.

The order was the whole defect. It is now: refuse a taken name, then clear a record, which is safe only
because the refusal proved the name was free.

## Old fail, new pass, same signatures

`cargo test -p cowfs-daemon --lib`, exit 101 against the previous code, exit 0 at this head.

| Test | against `6929e63` | at `5b23655` |
|---|---|---|
| `a_refused_duplicate_create_keeps_a_core_snapshot_and_its_whole_record` | **FAILED**, `NotFound` reading the record | passes |
| `a_refused_duplicate_create_keeps_a_path_snapshot_and_its_whole_record` | passes (the control) | passes |
| `a_recreated_snapshot_is_never_a_base_even_when_a_record_outlived_it` | passes | passes |
| `a_create_that_cannot_clear_a_stale_record_creates_no_snapshot` | did not exist | passes |

Old-behaviour failure, verbatim:

```
thread 'backend::tests::a_refused_duplicate_create_keeps_a_core_snapshot_and_its_whole_record'
panicked at crates/cowfs-daemon/src/backend.rs:1292:76:
called `Result::unwrap()` on an `Err` value:
  Os { code: 2, kind: NotFound, message: "No such file or directory" }
test result: FAILED. 84 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out
```

84 passed and one failed: only the core case, and the path control and the orphan guard passed on the
same run. The core test asserts the snapshot's Merkle root and the whole record are unchanged, live and
after a `CoreBackend` reopened on the same store; the path test asserts the snapshot's bytes and the
record, the same way.

## Public proof: real daemon, both backends, real FUSE mount

Script `bench/out/namespaces17-p98-create/duplicate-create-proof.sh`, run on moonscape in its own
immutable attempt directory `~/cowfs-ns17/int5/attempt-1005T020146-1176887`. Everything after the
precondition goes through the public API: `cowfs snapshot create`, `cowfs snapshot promote`,
`cowfs --json snapshot list`, and the `cowfs --socket … shutdown` subcommand for every stop.

The precondition is a record with a complete commit, written straight into the store while the daemon was
stopped, because no public verb publishes a commit on this path. The subject of the run is what happens
next, not how the record got there.

| | core | path |
|---|---|---|
| duplicate create refused | exit 1 | exit 1 |
| message | `cowfs: cannot create snapshot "warm": name is taken` | identical |
| record sha256 across the refusal | `cdaeceeaf9fa6a2b0a1e41caa71f630907881d32a777e369950e6d2e60b45ef9`, unchanged | same |
| listing across the refusal | unchanged | unchanged |
| snapshot bytes across the refusal | unchanged | unchanged |
| after a daemon reopened on the same store | all three unchanged | all three unchanged |
| new snapshot over an orphaned record | did not adopt the commit | did not adopt the commit |

Both backends produce the same record hash because the fixture record is the same file; that is a
cross-check, not a coincidence.

The `ghost` case is the orphan guard on the public surface: a record for a name with no snapshot behind
it is cleared by the next create of that name instead of being adopted. After the run the metadata
directory holds `warm` and not `ghost` on either backend.

### One normalisation, and why

Listings are compared with `created_unix_ms` removed. The path backend does not persist a snapshot's
creation time: `create_meta` stamps it with the current clock on every read, so two listings of the same
unchanged snapshot differ in that one field. That is pre-existing, it is not what is under test, and the
substitution is a no-op on the core backend, whose timestamps are stable. Without it the path arm fails
on a field that cannot be stable. The record hash and the snapshot bytes are compared exactly, with no
normalisation.

### First run, and what it caught

The first run of this proof failed on the path arm at `the listing changed across a refused create`,
which was my assertion being wrong rather than the product: the path backend's read-time creation
timestamp. The core arm had already passed in that same run, including the reopen and the orphan guard.
Both runs are kept; the first is the evidence that the path arm was reached and corrected rather than
quietly dropped.

## What this repair does not claim

- **Not** Core warm-base publication: `base_refresh` refuses directory ingest on the core by design, so
  the core path here is create, promote, duplicate-create, rename, remove and reopen over a real daemon.
- **Not** mode (b) acceptance, and not a change to the Path publication acceptance, which is a separate
  run and is unaffected by this repair.
- **Not** transaction safety. The name check is a pre-check: two concurrent creates of one free name can
  both pass it and the loser is rejected afterwards. It narrows the window in which one request clears a
  record another has just published; it does not close it. Serialising namespace operations per name is a
  broader change and is not made here.
- **Not** per-name locking at runtime. The store's mutex keeps a record's bytes and its cache entry in
  agreement, and nothing more. The per-name lock in the conformance suite is a test wrapper, not the
  production path.
- **Not** crash-durable removal. `remove`'s doc comment said "durably, before this returns" and the
  removal path fsyncs nothing after the unlink, so a crash there can leave a record on disk that the live
  map has dropped. The comment now states that asymmetry. No fsync was added and no durability scope was
  taken on: it is a wording fix, and the alternative would have been a larger change than the defect.

## Counts at `5b23655`, measured

`cargo fmt --all --check` 0. `cargo clippy -p cowfs-daemon --all-targets -- -D warnings` 0.
`cargo clippy -p cowfs-treehouse --all-targets -- -D warnings` 0.
`cargo test -p cowfs-daemon --lib`: `running 85 tests`, 85 passed, exit 0. By module: `backend` 20,
`base_meta` 15, `import` 14, `handler` 14, `exports` 13, `mounts` 3, `holders` 3, `daemon` 2, plus one
store probe. `cargo test -p cowfs-treehouse --test canonical` 10 passed on macOS, exit 0.
`python3 -m unittest discover -s bench -p test_namespaces.py`: `Ran 17 tests`, `OK (skipped=10)`, exit 0.

85 is the macOS count. The review measured 81 on Linux for the previous head; the difference is
platform-gated tests and no single figure describes both, so neither was adjusted to match the other.

## Separate blocks, unchanged by this repair

- **ETXTBSY.** Still a merge BLOCK on its own. 1 in 12 on the review's bounded sample under concurrency,
  0 of 6 at one thread. Root cause unconfirmed. Not retried, not ignored, not serialised, and no new
  spike: a theory without a discriminating old-fail and new-pass is what the five rejected hypotheses
  already were. No `posix_spawn` call exists in `crates/cowfs-treehouse/src/mode_b.rs`; it builds the
  helper with `std::process::Command`, which is a statement about the standard library, not this source.
- **The clippy version discrepancy.** `crates/cowfs-meta/src/tx.rs:312` is red on Rust 1.95 with a
  `collapsible_match` and green on 1.99.0, byte-identical either way. Not this branch's file, not edited,
  not waived. Unresolved and version-dependent.

Issue 17 stays open.

## Evidence, all gitignored

```
bench/out/namespaces17-p98-create/duplicate-create-proof.sh   the proof, as run
bench/out/namespaces17-p98-create/old-core-create.log         old behaviour: 84 passed, 1 failed
bench/out/namespaces17-p98-create/evidence/public-proof-run2.log           the passing run
bench/out/namespaces17-p98-create/evidence/public-proof-run1-core-pass-path-assert-fail.log
bench/out/namespaces17-p98-create/evidence/{core,path}/        record files, duplicate logs
```

Remote, preserved rather than cleaned: `~/moonscape/cowfs-ns17/int5/attempt-*`, two attempts, the first
holding the run that caught the over-strict assertion. No process and no mount of this repair remain; the
build cache was removed after both were verified absent.