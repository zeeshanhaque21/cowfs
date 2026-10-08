# Issue #42 residual verification against current main

Scope: verification only.
No production code, no test code, no issue, no feature, no PR was written for this task.
Nothing under test was changed and nothing was rerun that the #99 delivery had already verified, apart from the single representative sample named below.

## Immutable inputs

| what | value |
|---|---|
| main commit read | `93cfef94457a989d031cb6b0a475ac4edbdb85ef` (2026-10-05 11:41:46 -0700, "Merge pull request #131 from zeeshanhaque21/test/pathvfs-stamp-precondition-118") |
| #99 delivery head, an ancestor of main | `3951d50922127450f781883c65864caf83189d07` |
| issue #42 body, retrieved twice with the same hash | sha256 `67caa764c421b308d943f7fcef0542dae56d2a5b9bc94a071f484c637e7e55af`, 36 lines |
| issue #42 state at retrieval | `open`, `labels: []`, `updated_at 2026-09-30T23:23:33Z` |
| lease | `.treehouse-ready-wave/.treehouse/cowfs-7c1bf8/6/cowfs`, branch `followup/core-meta-integration-42`, head `3951d50`, working tree clean, idle |

Method: `git cat-file -p` and `git grep` against the main commit, plus one `git archive` of main extracted to a private directory and hash-verified.
No checkout of main was performed and no file in the lease was modified.

`codebase-memory` was queried first and had been re-indexed: `index_status` reports `head_sha 93cfef94457a989d031cb6b0a475ac4edbdb85ef`, matching the commit read above.
Its symbol totals agreed with the source reading in every case, and the line-precise citations below come from `git cat-file`, not from the graph.

## Numbering correction, which changes how this must be cited

The shared-crate request is **item 2 of issue #42** and **item 5 of `docs/v1-core.md`**.
Those two documents number their lists differently:

| item | issue #42 body | `docs/v1-core.md` "Requests of store and meta", "Not landed" |
|---|---|---|
| atomic snapshot rename | 1 | 1 |
| **shared snapshot-name crate** | **2** | **5** |
| hole flag in `ChunkRef` | 3 | 3 |
| `reserve_inodes` / `create_with_ino` | 4 | 4 |
| `batch_at` / `set_now` | 5 | 2 |

`docs/verification/ready-42.md` and the #99 PR body both say "request 5 of #42".
That number is `docs/v1-core.md`'s, applied to the issue's list.
The shared crate is request 2 of #42 and request 5 of `docs/v1-core.md`, and both facts should be stated with their document.
This record numbers by the issue body throughout, because the issue is what the coordinator and the tracker key on.
`docs/verification/ready-42.md` was not edited: it is outside the paths this task owns, and correcting it is a decision for its author or the coordinator.

## Residual matrix

Status vocabulary, used strictly:

- **missing API**: the named public contract does not exist in the source at main.
- **supported invariant**: a user-level property is true and is pinned by a test that exists at main.
- **workaround only**: the named API is absent and an alternative carries the property, with or without a test.

| # | request, as the issue words it | named API at main | status | shipped seam at main | test names verified present at main |
|---|---|---|---|---|---|
| 1 | `Meta::rename_snapshot(id, new_name)`, one transaction, keeps the id | absent | missing API, workaround only | `crates/cowfs-core/src/lib.rs:350` `Core::rename_snapshot`, delegating to `crates/cowfs-core/src/swap.rs:149` `swap_snapshot` | `crates/cowfs-core/tests/swap.rs:67` `promote_base_survives_a_failure_at_every_step`, `crates/cowfs-core/tests/swap.rs:154` `rename_snapshot_is_failure_safe`, `crates/cowfs-core/tests/swap.rs:114` `a_damaged_block_does_not_stop_a_promote` |
| 2 | one crate for the snapshot-name rule, `validate.rs` depends on it | present | **delivered**, invariant verified | `crates/cowfs-snapname/src/lib.rs`, 7 public items; consumers `crates/cowfs-core/src/snapname.rs:15,20,27` and `crates/cowfs-ctl/src/validate.rs:30,87` | `crates/cowfs-snapname/src/lib.rs` 6 tests; `crates/cowfs-daemon/tests/snapname_drift.rs:61,112,138,149`; `crates/cowfs-core/tests/names_ino.rs:120` `snapshot_names_follow_the_cli_rules`; `crates/cowfs-core/tests/critic2b.rs:911` `staging_names_are_reserved_for_the_swap_protocol` |
| 3 | a hole flag in `ChunkRef`, so the walker in meta is safe | absent | missing API, workaround only | `crates/cowfs-core/src/lib.rs:583` `Core::live_blocks` filters `file::HOLE`; `crates/cowfs-meta/src/walk.rs:101` does **not** | `crates/cowfs-core/tests/caches.rs:81` `live_blocks_filters_holes_and_yields_only_stored_blocks`; `crates/cowfs-gc/tests/regressions.rs:353` `a_hole_is_never_swept`; `crates/cowfs-meta/tests/posix.rs:389` `live_blocks_skips_shared_subtrees` |
| 4 | `Meta::reserve_inodes(n)` or `Tx::create_with_ino` | absent | missing API, workaround only | durable reservation exists but is private: `crates/cowfs-meta/src/db.rs:653` `fn reserve_durable`, `crates/cowfs-meta/src/tx.rs:12` `pub(crate) struct InoAlloc`, `crates/cowfs-meta/src/tx.rs:27` `reserve: &dyn Fn(u64)`; core keeps `crates/cowfs-core/src/ino.rs:12,13,14,206` | `crates/cowfs-core/tests/names_ino.rs:13` `virtual_inode_numbers_are_never_reused_across_a_restart`, `:56` `the_virtual_number_reservation_survives_a_crash`; `crates/cowfs-core/tests/alias.rs:106` `alias_bytes_at_500k_creates`, `#[ignore]`d at line 105 |
| 5 | `Snapshot::batch_at(now)` or `Tx::set_now` | absent | missing API, workaround only, **invariant untested** | `crates/cowfs-meta/src/db.rs:740` `now: Timestamp::now()` once per batch; `crates/cowfs-meta/src/tx.rs` writes `self.now` into `ctime` at 12 sites (89, 110, 123, 165, 245, 355, 362, 407, 467, 535, 550, 563) | **none.** no test in any `crates/*/tests/` names `ctime`, at main |

## Per-request detail, with the measured source facts

### 1. Atomic snapshot rename

`cowfs-meta` exposes, at main: `new_snapshot` (`crates/cowfs-meta/src/db.rs:1412`), `snapshot_by_id` (`:1426`), `snapshots` (`:1436`), `Snapshot::id` (`:1585`), `Snapshot::info` (`:1591`), `Snapshot::fork` (`:1606`).
A tree-wide search of main for `fn rename_snapshot` returns exactly two hits, `crates/cowfs-core/src/lib.rs:350` and `crates/cowfs-core/tests/swap.rs:154`; none in `cowfs-meta`.
The `rename` at `crates/cowfs-meta/src/db.rs:1570` is `Tx::rename(from_dir, from_name, to_dir, to_name)`, a directory-entry rename inside one tree, and is not a snapshot rename.

The staged swap is what ships: `crates/cowfs-core/src/swap.rs:36` `SWAP_PREFIX = "swap-"`, `:52` `staging_name`, `:111` `recover`, `:149` `swap_snapshot`.
`Core::rename_snapshot` states the cost in its own doc comment at `crates/cowfs-core/src/lib.rs:347-349`: "Its snapshot id and every inode number in it change."
`swap.rs:147-148` says the same of the two forks.

**Presence versus invariant.** The requested property, that a rename keeps the snapshot id, is **false** today and no test can assert it, because the API that would express it does not exist.
The tests that do exist assert a different property: that a failure at each of the five steps leaves the mount safe, which `promote_base_survives_a_failure_at_every_step` covers by injecting the fault seam `Core::set_swap_fault` at each step, checking content, reopening, and checking recovery or the old base.
`rename_snapshot_is_failure_safe` covers the `Exists` and `InvalidName` refusals plus the crash-safe ordering.
Neither asserts id stability, because stability is not offered.

### 2. Shared snapshot-name crate

Present and uniform at main, established from the source rather than from prose.

`crates/cowfs-snapname/src/lib.rs` is the only implementation: `NAME_MAX:17`, `RESERVED:21`, `NameError:25`, `NameError::why:44`, `is_reserved:58`, `validate_snapshot_name:63`, `validate_snapshot_name_bytes:86`, `name_key:98`.
Exactly two crates declare a dependency on it, `crates/cowfs-core/Cargo.toml` and `crates/cowfs-ctl/Cargo.toml`; `cowfs-meta`, `cowfs-store`, `cowfs-daemon`, `cowfs-gc`, `cowfs-fuse`, `cowfs-nfs` and `cowfs-treehouse` do not.

There is no second copy of the rule anywhere at main: the whole-tree call-site list for `cowfs_snapname::` is nine lines, four in the core adapter and forwarders plus the swap marker, and five in the ctl forwarders.
In `crates/cowfs-ctl/src/validate.rs` the `starts_with` and `is_control` that survive at lines 14, 57 and 60 belong to `escape_control` and `validate_abs_path`, not to the snapshot-name rule; `validate_snapshot_name` at line 29 is a two-line forwarder.
The reserved marker has one definition: `crates/cowfs-core/src/swap.rs:39` `pub(crate) const STAGING: &str = cowfs_snapname::RESERVED;` and `swap.rs:43` `is_reserved` delegating.

**Presence versus invariant.** Both, and the invariant is the stronger of the two: `crates/cowfs-daemon/tests/snapname_drift.rs` opens a real store, asks `cowfs_ctl::validate_snapshot_name` and `Core::create_snapshot` about every name in a table, requires identical verdicts, reopens the store and repeats, and separately requires the two `name_key` functions to agree, including for a name whose folded form exceeds `NAME_MAX`.
Its measured old-fail and new-pass record and its green exact-head CI run are in `docs/verification/ready-42.md` and were not rerun for this task.

### 3. Hole flag in `ChunkRef`

`crates/cowfs-store/src/lib.rs:68-73` at main is `pub struct ChunkRef { pub id: BlockId, pub len: u32 }`.
There is no flags field, no bitfield and no `is_hole` member; the struct is 6 lines including its doc comment.
A hole is therefore still an all-zero `BlockId`, and that literal is written in two crates: `crates/cowfs-core/src/file.rs:14` `pub(crate) const HOLE` and `crates/cowfs-gc/src/lib.rs:44` `pub const HOLE`.

The residual is directly observable in the walker, not merely asserted.
`crates/cowfs-meta/src/walk.rs:100-103`:

```
if k.get(8) == Some(&K_CHUNK) {
    let refs = decode_chunks(top.node.val(i))?;
    self.queue.extend(refs.into_iter().map(|c| c.id));
}
```

Every `c.id` from every chunk list is queued, with no filter, so `Snapshot::live_blocks` (`crates/cowfs-meta/src/db.rs:1685`) and `live_blocks_with_root` (`:1696`) yield the all-zero id of a sparse file's hole.
The compensating filter is one level up, at `crates/cowfs-core/src/lib.rs:594`: `if id != file::HOLE`.
That function's own doc comment at lines 577-581 states the residual in the tree's own words: "`cowfs-meta` yields the all-zero hole ref of a sparse file, which is not a block; this filters it, so every id here is in the store. GC (#10) should use this, not the walker directly, until `ChunkRef` has a hole flag (see `docs/v1-core.md`)."

**Presence versus invariant.** The requested property, that the walker in meta is safe, is **false** at main, measured from those five lines.
The compensating property, that `Core::live_blocks` filters hole refs, is a supported invariant and is the one sample run recorded below.
`crates/cowfs-gc/tests/regressions.rs:353` `a_hole_is_never_swept` is a GC-side property: a hole ref is not swept.
It is not a walker property and is not offered as one.
`crates/cowfs-meta/tests/posix.rs:389` `live_blocks_skips_shared_subtrees` covers incremental subtree skipping, not hole filtering.

### 4. Inode reservation before the creating transaction

No `pub fn` in `cowfs-meta` whose name contains `reserve` or `ino` exists at main.
The durable reservation the issue describes is present but private:

- `crates/cowfs-meta/src/db.rs:653` `fn reserve_durable(&self, new: u64)`, reached only through the closure built at `db.rs:733` `let reserve = |n: u64| self.reserve_durable(n);`
- `crates/cowfs-meta/src/tx.rs:12` `pub(crate) struct InoAlloc { next, reserved, block }`
- `crates/cowfs-meta/src/tx.rs:66-79` `Tx::alloc`, which extends the mark itself at line 73 when `a.next >= a.reserved`, blocks of `db.rs:100`
- `crates/cowfs-meta/src/db.rs:1280` seeds `"ino_reserved"` with `2`; `check.rs:53-57` range-checks it

Core therefore still carries the two-file durable mark: `crates/cowfs-core/src/ino.rs:13` `VIRT_A = "virt.ino.a"`, `:14` `VIRT_B = "virt.ino.b"`, `:12` `VIRT_BLOCK = 1 << 20`, `:206` `SAFETY = 1 << 32`, with the damage-tolerance ladder at `:177-198`.

**Presence versus invariant.** The requested property, that a caller can hold a number before the transaction that creates the inode, is **not offered**; `reserve_durable` is not public and `Tx::alloc` is not callable from outside the crate, so there is no seam to expose without changing visibility.
What the existing tests pin is the workaround's durability, not the API: `virtual_inode_numbers_are_never_reused_across_a_restart` and `the_virtual_number_reservation_survives_a_crash` assert that a virtual number is not handed out twice and that the mark survives a crash.
The "130 aliases after 500,000 creates" figure in `docs/v1-core.md` is not pinned by a default test; `crates/cowfs-core/tests/alias.rs:105` marks `alias_bytes_at_500k_creates` `#[ignore]` and gates it behind `ALIAS_FILES=500000 ... --release`, so it does not run in CI and was not run for this task.

### 5. Explicit batch timestamp

No `batch_at` and no `set_now` exists at main, by whole-tree search.
`Tx` carries `pub(crate) now: Timestamp` (`crates/cowfs-meta/src/tx.rs:28`), and it is assigned exactly once, at `crates/cowfs-meta/src/db.rs:740` `now: Timestamp::now()`, inside the `Tx { .. }` literal built at `db.rs:735` in `Meta::mutate` (`db.rs:714`).
So one wall-clock reading is taken per batch, at batch start, and `tx.rs` writes that value into `ctime` at 12 sites: 89, 110, 123, 165, 245, 355, 362, 407, 467, 535, 550, 563.
A queued operation's own time, taken in `cowfs-core`, for example `crates/cowfs-core/src/ns.rs:176` and `crates/cowfs-core/src/io.rs:88`, is therefore not what lands in meta's `ctime`.
The gap between the two is bounded by the batch trigger, `Options::flush_interval`, which `docs/v1-core.md:233-236` gives as 500 ms by default, with `max_pending_ops` 4096 and `max_dirty_bytes` 128 MiB as the other two triggers.

**Presence versus invariant.** The API is missing and the workaround is **unverified**: there is no test at main in any `crates/*/tests/` directory that names `ctime`, so nothing asserts the batch-time behaviour, the bound, or the claim in `docs/v1-core.md` that a cached ctime is exact while the node is cached.
That claim is a design statement, not a measurement, and it is recorded here as such.

## The one representative sample

Chosen because it is the only residual row whose compensating invariant is both load-bearing and cheap to pin, and because it is not a rerun of the #99 suite: `caches.rs` was not among the test targets that delivery ran.

Bound to main by extraction, not by the working tree: `git archive 93cfef94457a989d031cb6b0a475ac4edbdb85ef` into `bench/out/meta42-residual-verification/main-src`, then every tracked file compared with `git hash-object` against `git rev-parse <main>:<path>`: **641 tracked files, 641 extracted, 0 mismatched**.
sha256 of the files this row rests on: `crates/cowfs-meta/src/walk.rs` `8bff4277599f2bc6ce1bd089e00c1e9612fc14e3c3c63f862d5ac8bd44641868`, `crates/cowfs-core/src/lib.rs` `d7418cb07d481f4e41f3680a30e37f368201d554bd94a161dfea1c7d8c7f39b8`, `crates/cowfs-store/src/lib.rs` `1110e7ff400ee7939b1840538dc0833df574748c705b9515f2a22191a6effb23`.

Run in its own target dir `bench/out/meta42-residual-verification/main-target`, under one bounded 600 s acquisition of the shared lane, floor and cap respected:

```
cargo test -p cowfs-core --test caches live_blocks_filters_holes_and_yields_only_stored_blocks
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out
SAMPLE_EXIT=0
```

Free disk before and after: 318 GiB and 318 GiB.
Nothing was built without the lane, and no daemon, mount or socket was touched.

## Minimal work still needed, per request

No implementation was performed and none is proposed beyond naming the smallest honest step.

**1, atomic rename.** Add `Meta::rename_snapshot(id, new_name)`: one transaction that rewrites the snapshot row's name, keeps `SnapshotId`, and either refuses an existing name or removes it inside the same commit.
Then decide what `Core` does with `swap.rs`: either route `rename_snapshot` through it and drop the double fork, or keep the intent file for `promote_base`, which replaces a snapshot that may have holders.
A decision is needed because `promote_base` and `rename_snapshot` share `swap_snapshot` today.
Adding the meta API alone changes no user-visible behaviour while `Core` keeps forking twice, so the API and the `Core` route land together or the residual is unchanged.
Blocked on the concurrent post-error `Core` and `Path` swap-recovery work, which owns fault injection and recovery in that same module; see dependencies.

**2, shared crate.** Nothing.
Recorded as delivered and verified; the only outstanding work is keeping it that way, which the drift fixture enforces.

**3, hole flag.** Add a flag to `cowfs_store::ChunkRef` and make `cowfs-meta`'s walker filter on it, then delete the filter at `crates/cowfs-core/src/lib.rs:594` and the `Core::live_blocks` doc's "until `ChunkRef` has a hole flag" sentence.
`ChunkRef` is encoded in `cowfs-meta` (`crates/cowfs-meta/src/types.rs:353` decode path, `check.rs:383` `K_CHUNK`), so this is an on-disk format change for existing stores and needs a format version decision, not just a struct change.
`HOLE` is defined twice, in `cowfs-core` and `cowfs-gc`, and both would collapse onto the store's definition.
Until then `Core::live_blocks` must stay the only entry point a collector uses, which `cowfs-gc` already documents and `a_hole_is_never_swept` already covers.

**4, inode reservation.** Expose `reserve_durable` as a public `Meta::reserve_inodes(n: u64)` and add `Tx::create_with_ino(ino)` for the create-in-a-named-slot case, then retire `crates/cowfs-core/src/ino.rs`'s `virt.ino.a`/`.b` mark and the alias table.
This is the largest of the four and the one with a durable-format consequence: the mark is read at every open and has a damage-tolerance ladder and a `SAFETY` floor, and `docs/v1-core.md` records a lead decision already taken to keep virtual numbers with an alias table instead of asking meta for a reservation.
That decision has to be revisited on evidence before this is built, not after.

**5, batch timestamp.** Add `Tx::set_now(t)` and thread the operation's own timestamp from the core queue into the `Tx` built at `crates/cowfs-meta/src/db.rs:735`, or add `Snapshot::batch_at(now, f)` and have `Core::mutate` pass the queued op's time.
Add a test that names `ctime` and asserts a deferred operation's stored `ctime` equals the operation time, not the batch time, because nothing asserts this today.
The smaller half of this is making the current behaviour explicit: a test that pins ctime as batch time and bounds it by `flush_interval`, so the invariant stops being prose.

## Follow-on dependencies and ownership boundaries

- The concurrent post-error `Core` and `Path` swap-recovery work (BUILD TRAIN 6) owns fault injection and recovery in the swap path. Request 1's `Core` route lands inside that ownership. This record duplicates none of it and touches none of its files.
- READY 1 (#131) is done and is what produced the main tip commit.
- The open daemon issues #125, #127 and #128 are in the daemon, not in `cowfs-meta`, `cowfs-store` or the name rule; none is a dependency of any row here.
- #40 metadata recovery counters and health reporting: its tests are health-signal tests and are **not** evidence for any row here, in particular not for the hole walker.
- #96 namespace barrier and #98 warm-base provenance are owned elsewhere and touch `cowfs-core`; integrating any row above means retesting on the exact post-merge head.
- #97 import/refresh is out of scope and was not read for this record.

## What was not done, stated plainly

- No implementation of requests 1, 3, 4 or 5. Request 2 was already delivered and only verified.
- Issue #42 stays open. Five of five original requests are now either delivered (1 of them) or still named as not landed by `docs/v1-core.md` itself at main.
- No closure is claimed for the umbrella.
- No part of `docs/v1-core.md`, `docs/design.md`, `progress/` or any other document was edited; the numbering correction above is a finding, not an edit.
- The only test executed is the single sample named above. The #99 suite was not rerun; its evidence stands on `docs/verification/ready-42.md` and its exact-head CI run.
- Heavy gates and CI on other branches are not claimed as evidence for this record.

## Evidence

All paths relative to the lease `.treehouse-ready-wave/.treehouse/cowfs-7c1bf8/6/cowfs`, under `bench/out/meta42-residual-verification/` (gitignored):

| file | what |
|---|---|
| `issue42-body.b64`, `issue42-body.md` | the full untruncated issue body, two retrievals, sha256 `67caa764…` |
| `main-archive-identity.txt` | the main SHA and the 641 of 641 extraction check |
| `main-src/` | the extracted main tree the sample ran against |
| `main-target/` | that run's isolated target dir |
| `sample-live-blocks.log` | the one test run, its count and its exit code |
