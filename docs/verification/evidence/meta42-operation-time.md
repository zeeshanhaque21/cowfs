# Issue #42 request 5: ctime is the time of the change, not the time of the batch

Scope: request 5 of issue #42, which is request 2 of the `docs/v1-core.md` "Requests of store and meta" list.
Requests 1, 3 and 4 are untouched and are not started.

| what | value |
|---|---|
| branch | `fix/deferred-operation-time-42`, cut with `git switch -c` from the verified main commit |
| main commit it is based on | `93cfef94457a989d031cb6b0a475ac4edbdb85ef` |
| implementation head | `2a06ea916a670be69ffad55ae9af2c26ab6377e3` |
| PR | https://github.com/zeeshanhaque21/cowfs/pull/136 |
| lease | `.treehouse-ready-wave/.treehouse/cowfs-7c1bf8/6/cowfs` |
| prior branch preserved | `followup/core-meta-integration-42` still at `3951d50922127450f781883c65864caf83189d07` |
| accepted source audit, immutable | `docs/verification/evidence/meta42-residual-verification.md`, sha256 `fbc6a078137b0fab370638d27dcaf64ff3ad283de37d8e7e970e4b10faac53ba` |
| toolchain | `rustc 1.99.0 (b940084d7 2026-09-28)`, macOS |

## The contract, from the two source documents

Issue #42 request 5:

> `Snapshot::batch_at(now)` or `Tx::set_now`. ctime of a deferred operation is the batch time, up to `flush_interval` late. Not blocking anything; recorded because ctime is part of the contract.

`docs/v1-core.md:556`, request 2 of its own list:

> `Snapshot::batch_at(now: Timestamp, f)` or `Tx::set_now`, so that ctime (and creation times) of deferred operations are the times the operations happened.

`docs/v1-core.md:238` records why this happens: `cowfs-meta` reads the clock once, when the transaction opens.
The reading is at `crates/cowfs-meta/src/db.rs:740`, in the `Tx` literal built at `db.rs:735` inside `Meta::mutate` (`db.rs:714`), and `Tx::now` is what all twelve ctime writes read.

`docs/v1-core.md` also states the existing workaround, which this change does not remove:

> Cached ctime is exact while the node is cached.

That is the invariant this makes durable: the cached value the mount already reports becomes the stored value.

## What was built, and what was deliberately not

`Tx::set_now(now)`, one assignment to the existing `pub(crate) now: Timestamp` (`crates/cowfs-meta/src/tx.rs:28`), with the doc comment at `tx.rs:218-227`.
Every ctime write already read that field, so nothing else in meta changed.
`Snapshot::batch` keeps its signature and its wall-clock default, so every existing caller behaves as before.

`Snapshot::batch_at` was not added.
The issue offers it as an alternative, not a requirement, and one of the two is enough.
Adding both would be a second way to do the same thing with no caller.

No timestamp field was added to any `Op` variant.
No clock abstraction, no injected time source, no fake-time test seam, no source-format change, no other public API.

## Where the operation times come from

The queued operations carry no timestamps, and none were added.
The times were already in the layer: every operation that reaches meta stamps two or three inodes from **one** `Timestamp::now()` reading, and the code that queues the operation writes that same value into each cached node as it goes.

| operation | the single reading, written to |
|---|---|
| create | `crates/cowfs-core/src/ns.rs:195` new inode, `ns.rs:206-207` parent |
| link | `ns.rs:286` target inode, `ns.rs:292` parent |
| unlink | `ns.rs:336` child, `ns.rs:341` parent |
| rmdir | `ns.rs:411` child, `ns.rs:416` parent |
| rename | `ns.rs:548-549` both parents, `ns.rs:551` source, `ns.rs:555` replaced inode |
| write | `crates/cowfs-core/src/io.rs:103` |
| setattr | `io.rs:202` |
| setxattr, removexattr | `io.rs:399`, `io.rs:411` |

Because the reading is shared, any one of those inodes carries the operation's time, which is why `Inner::op_times` (`crates/cowfs-core/src/inner.rs:880-911`) keys on exactly one inode per operation:

| operation | the inode keyed on, and why |
|---|---|
| `Op::Create` | `child`, the new inode, `ns.rs:195` |
| `Op::Link`, `Op::Content` | `ino`, the affected inode, `ns.rs:286` and `io.rs:103` |
| `Op::Unlink`, `Op::Rmdir` | `parent`, because the child is named by the entry and `tx.unlink` resolves it itself, `ns.rs:341` and `ns.rs:416` |
| `Op::Rename` | `from`, the source parent, `ns.rs:548`; both parents and the source share the reading |

`op_times` is read before the commit opens meta's writer lock, the same way and for the same lock-order reason as the existing `Inner::restore_states` (`inner.rs:862-868`), whose doc comment already says a node lock taken inside the commit closure would invert the order.
The stamp is applied in `Inner::commit` (`inner.rs:915-921`) before each operation, and again before each `setattr` in the final attribute loop (`inner.rs:1013-1015`).

An inode whose node has already been evicted is absent from the map.
Its operations keep the transaction's wall-clock opening time, which is exactly what they had before this change, so an eviction cannot make a timestamp worse than the old behaviour.

### The final attribute replay, which is the worst case of the defect

`Inner::commit` runs one `tx.setattr` per touched inode after the operations, and `Tx::setattr` writes `rec.ctime = self.now` (`tx.rs:407`).
Unstamped, that loop restamps **every inode the batch carries**, touched or not by the last operation, with the transaction's opening time.
It is stamped from the same map, and `a_deferred_write_keeps_its_own_ctime_and_does_not_move_an_older_file` is exactly that case: a file nobody touched after its create must keep its create time while a sibling in the same batch is written.

## Proof

### Source binding

Both fixtures were run from an extracted archive of a specific commit, in an isolated target dir, under one bounded 600 s acquisition of the shared lane.
Every tracked file was compared with `git hash-object` against `git rev-parse <rev>:<path>`.

| archive | commit | tracked | extracted | mismatched |
|---|---|---|---|---|
| `old-src` | `93cfef9` (main) | 641 | 641 | **0** |
| `new-src` | `2a06ea9` (this head) | 643 | 643 | **0** |

sha256 of the files the result rests on:

| file | old-src | new-src |
|---|---|---|
| `crates/cowfs-meta/src/tx.rs` | `5cafb0cc2e10f1e2…` | `a92c3d76c573fb77…` |
| `crates/cowfs-core/src/inner.rs` | `ba3036909c6d5fe8…` | `b94fe02d1474c51b…` |
| `crates/cowfs-core/tests/operation_time.rs` | absent | `4139220cb220b041…` |
| `crates/cowfs-meta/tests/operation_time.rs` | absent | `10bb5b61bef78a2d…` |

The old archive was given both new test files and nothing else, so the only difference between the two runs is the two production files.

### Old fail, new pass

| build | `cowfs-core/tests/operation_time.rs` | exit |
|---|---|---|
| `93cfef9` plus the fixture only | **0 passed, 4 failed** | 101 |
| `2a06ea9` | **4 passed, 0 failed** | 0 |

| build | `cowfs-meta/tests/operation_time.rs` | exit |
|---|---|---|
| `93cfef9` plus the fixture only | does not compile: `no method named 'set_now' found for mutable reference '&mut Tx<'_>'`, 10 sites | 101 |
| `2a06ea9` | **5 passed, 0 failed** | 0 |

The metadata fixture cannot compile against the old source because the seam it exercises does not exist there.
That is reported as what it is, not as a passing test.

### The four measured divergences, from the old run

Every one is the batch time where the operation time belongs.
The gaps are tens of microseconds, which is why an interval-shaped assertion would have hidden the defect instead of catching it.

```
a_deferred_create_keeps_its_own_ctime_across_a_flush_and_a_reopen
  the durable ctime is not the time the create happened
  left:  Timestamp { secs: 1791229358, nanos: 550806000 }   <- stored, the flush time
  right: Timestamp { secs: 1791229358, nanos: 550773000 }   <- when create reported it

a_deferred_write_keeps_its_own_ctime_and_does_not_move_an_older_file
  the durable ctime is not the time the write happened
  left:  Timestamp { secs: 1791229358, nanos: 555938000 }
  right: Timestamp { secs: 1791229358, nanos: 550799000 }   <- 5.1 ms earlier: the write, not the batch

a_deferred_setattr_and_a_mutation_to_the_same_inode_keep_their_own_times
  the durable ctime is not the time of the last change to this inode
  left:  Timestamp { secs: 1791229358, nanos: 555949000 }
  right: Timestamp { secs: 1791229358, nanos: 555932000 }

a_namespace_change_keeps_its_own_ctime_on_the_parent_and_the_child
  the parent directory ctime is not the time of its last entry change
  left:  Timestamp { secs: 1791229358, nanos: 550815000 }
  right: Timestamp { secs: 1791229358, nanos: 550791000 }
```

### How the public sample avoids a sleep

No fixture sleeps, and none can pass on the old behaviour by accident.

Each test first performs several operations and asserts they produced **distinct** times, so a single later stamp for the whole batch cannot equal an earlier one.
The assert is on the public surface: `v.getattr(ino).ctime` immediately after the operation.
Then it flushes, drops the `Core`, reopens the same directory, and requires exact equality against the time the mount reported earlier.

```rust
let f1 = v.create(d, b"f1", 0o644).unwrap().ino;
let t1 = v.getattr(f1).unwrap().ctime;
let f2 = v.create(d, b"f1-sibling", 0o644).unwrap().ino;
let t2 = v.getattr(f2).unwrap().ctime;
assert!(t2 > t1, "the two creates must be distinct times: {t1:?} then {t2:?}");
c.flush().unwrap();
drop(v);
drop(c);
assert_eq!(reopened_ctime(dir.path(), b"d", b"f1"), t1, ...);
```

`Options { background: false }` keeps the batch under the test's control, so the flush point is the explicit `flush()` and not a timer.
The reopen is a real `Core::open` on the same directory, reading through the public `Vfs`, so nothing is answered from a cache.

No clock is injected, no metadata is mocked, no private field is read, and no assertion is of the form "within `flush_interval`".
Every comparison is exact timestamp equality.

### What each of the four core tests pins

| test | pins |
|---|---|
| `a_deferred_create_keeps_its_own_ctime_across_a_flush_and_a_reopen` | a deferred create's ctime survives the flush and the reopen; a flush does not move a ctime the mount already reported |
| `a_deferred_write_keeps_its_own_ctime_and_does_not_move_an_older_file` | the write's time wins for the written file, and a sibling nobody touched after its create is not restamped |
| `a_deferred_setattr_and_a_mutation_to_the_same_inode_keep_their_own_times` | the last change's time wins for that inode, and an explicit `SetTime::At` mtime survives the flush unchanged |
| `a_namespace_change_keeps_its_own_ctime_on_the_parent_and_the_child` | the parent directory takes the time of its last entry change, and a file inside it is not restamped when the directory changes |

The five metadata cases pin the seam directly with stated times: the wall-clock default when `set_now` is not called, the new inode and its parent taking the stated time, a mixed transaction of three operations at three times, `atime`/`mtime` surviving `set_now`, and a later transaction not moving an earlier ctime.

### The matched control

`Cowfs` atime/mtime user semantics are pinned by `set_now_does_not_replace_an_explicit_atime_or_mtime` and by the third core test, which sets an explicit `SetTime::At` mtime and requires it back unchanged after the flush and reopen.
Before this change `Tx::setattr` never replaced an explicit time either, so this is a control that the change had to keep passing rather than a new capability.

### Test matrix, every exit code read directly

| command | result | exit |
|---|---|---|
| `cargo test -p cowfs-meta` | 12 suites, **87 passed, 0 failed**, 2 ignored | 0 |
| `cargo test -p cowfs-core`, 18 scoped targets | 18 suites, **126 passed, 0 failed**, 1 ignored | 0 |
| `cargo test -p cowfs-core --test operation_time`, this head | 4 passed, 0 failed | 0 |
| `cargo test -p cowfs-meta --test operation_time`, this head | 5 passed, 0 failed | 0 |
| `cargo test -p cowfs-core --test operation_time`, `93cfef9` + fixture | **0 passed, 4 failed** | 101 |
| `cargo test -p cowfs-meta --test operation_time`, `93cfef9` + fixture | does not compile, no `set_now` | 101 |
| `cargo fmt --all -- --check` | clean | 0 |
| `cargo clippy -p cowfs-meta -p cowfs-core --all-targets -- -D warnings` | clean | 0 |

The 18 core targets are `lib`, `core`, `names_ino`, `swap`, `invariant`, `durability`, `critic2b`, `model`, `chunks`, `caches`, `fsck`, `locks`, `alias_session`, `operation_time`, `kill9`, `stress`, `poison`, `crash`.
This includes the lock audit (`critic2b`), the crash-injection suites, and the model comparison against `MemVfs`.

No full-workspace build, no stress or gate variant run, no performance or mount run.

## The one source addition outside the two crates

`docs/v1-core.md` gains exactly one row in the lock audit table:

```
| `inner::op_times` | st.rd, nodes | 2 |
```

`critic2b::every_lock_site_is_in_the_audit_table` fails on any lock-taking function that is not listed there.
It was **exit 1** with `Inner::op_times` present and unlisted, and **exit 0** with the row, both measured.
The function reads `st.rd` before the commit, order 2, exactly beside the existing `Inner::restore_state` row.

No other line of `docs/v1-core.md`, no `docs/design.md`, no `CHANGELOG.md`, no workflow, no runner configuration.
The request list in `docs/v1-core.md` and the body of issue #42 are left as they are; marking request 2 or 5 as landed is the coordinator's call, not this branch's.

## Untouched, deliberately

Requests 1, 3 and 4 of #42 are not implemented and not started:

- **1, atomic snapshot rename.** `Meta::rename_snapshot` stays absent; `Core::rename_snapshot` and the staged swap are untouched. The swap path belongs to the concurrent post-error `Core` and `Path` recovery work.
- **3, hole flag in `ChunkRef`.** `ChunkRef` stays `{ id, len }`. This is an on-disk format change and needs a lead decision.
- **4, `reserve_inodes` / `create_with_ino`.** No reservation API is added. The public reservation question and the recorded lead decision in `docs/v1-core.md` both need the lead, not a lane.

Also untouched: `crates/cowfs-core/src/swap.rs`, `promote_base`, `rename_snapshot`, fork and recovery; `crates/cowfs-daemon/**`; `crates/cowfs-vfs-path/**` and `PathVfs`; base metadata; the inode alias table; `Options::flush_interval` and the other flush triggers; generation and durability ordering; `finish_sync`; the health and recovery counters.

The only edits to existing source are `Tx::set_now` and the `Inner` replay that calls it.
`restore_state`, `restore_states`, `commit_batch` ordering and `finish_sync` are unchanged apart from the one extra pre-commit read.

## Limitations, stated plainly

- **`Snapshot::batch_at` is not implemented.** `set_now` satisfies the request as written, which offers either. A caller that wants to stamp a whole transaction from outside `Core` can now do it with `set_now` inside its own `batch` closure.
- **A directory rename goes through `rename_dir`, which is not queued.** It writes to meta directly after a barrier (`crates/cowfs-core/src/ns.rs:608-629`), so its ctime is the time of the meta write, which is immediately after the barrier. That was true before and is unchanged; this change does not make it worse and does not improve it.
- **The stamp is the cached ctime, so it is the time of the last change to that inode, not of a specific operation on it.** For an inode changed twice before one flush, the stored ctime is the second time. That is the correct POSIX meaning of `ctime` and it is what the mount already reports, so this is alignment rather than new information.
- **`xattr` writes are not in the queue.** `op_setxattr` (`io.rs:369-411`) either updates the cache or writes to meta directly, so no stamp was needed and none was added.
- **No `SIGKILL` or power-loss claim is made.** The crash-injection suites passed, which is evidence that nothing regressed, not evidence of power-loss behaviour.
- **CI on this head is reported as one snapshot when read.** It was not dispatched, not rerun, and no runner configuration was touched.
- **`cargo test --workspace` was not run.** The scoped `cowfs-meta` and `cowfs-core` suites and CI cover it.
- **A worker, hardware or scheduler difference is not excluded** from the microsecond gaps; the assertions are exact equality, so the old build fails deterministically on this property regardless of timing.

## Evidence

Under `bench/out/meta42-operation-time/` in the lease, gitignored, with unique `CARGO_TARGET_DIR` and `TMPDIR` per source tree:

| file | what |
|---|---|
| `OLD-fixtures-on-old-source.log` | both fixtures on `93cfef9`: 4 failures with the measured gaps, and the compile error |
| `NEW-fixtures-on-new-source.log` | both fixtures on `2a06ea9`: 4 passed and 5 passed |
| `OLD-new-test.log` | the first old-source run, before the fixture's own directory fix |
| `NEW-sample.log`, `NEW-meta.log` | the fixture runs in the lease tree |
| `meta-suite.log` | `cargo test -p cowfs-meta`, 12 suites |
| `core-suite.log` | the 18 scoped `cowfs-core` targets |
| `lock-audit.log` | the audit table check after the row was added |
| `fmt.log`, `clippy.log` | the two static checks |
| `old-src/`, `new-src/` | the extracted archives, 0 mismatched each |
| `old-target/`, `new-target/` | the isolated target dirs |
