# ready-45: FUSE read/write coherence, Linux reproduction and the invariant that replaces the skip

Task: issue #45, "FUSE conformance: intermittent torn read in concurrent readers/writers on hosted
Linux".
Base: `46b0f269d5bef4a2c204c25f5b3015da601d3beb`.
Branch: `fix/fuse-torn-read-45`.

## Verdict

The CI failure is real, it is not a `cowfs` defect, and the skip already in
`crates/cowfs-fuse/tests/conformance.rs` is correct.
I reproduced the tear on this host with **no `cowfs` code in the data path**, and I could not
reproduce it against `cowfs` at all.
No source fix is warranted, so none was made.
What was missing was a test that holds the invariant the skip depends on, which is now
`crates/cowfs-fuse/tests/coherence.rs`.

## The oracle is not wrong

`crates/cowfs-vfs-test/src/lib.rs:117-121` classifies
`concurrent_readers_and_writers_of_one_file` as a `Cowfs` check and says why: POSIX calls a read
atomic against a concurrent write, Linux buffered I/O does not provide that, and 512 and 4096 byte
reads were measured tearing on ext4, btrfs and tmpfs.
`crates/cowfs-fuse/tests/conformance.rs` therefore skips it through the mount, with the reason
"native ext4/btrfs/tmpfs tear at this size too".

That justification had never been measured on this hardware, so I measured it with the repository's
own check body, unskipped, against native tmpfs through `cowfs-vfs-path`:

| arm | how it runs | torn | clean | reps |
| --- | --- | --- | --- | --- |
| native tmpfs, repo's own check, unskipped | `cowfs-vfs-path --test native`, no `cowfs` in the path | **11** | 29 | 40 |
| real `Core` via the `Vfs` trait, repo's own check, unskipped | `cowfs-core --test conformance` | **0** | 40 | 40 |

The 11 native failures are byte-identical in shape to the CI report, and include the reported
offset class:

```
FAIL cowfs  concurrency  concurrent_readers_and_writers_of_one_file  152.42ms  torn read at offset 8192: block mixes two writes
FAIL cowfs  concurrency  concurrent_readers_and_writers_of_one_file  103.66ms  torn read at offset 45056: block mixes two writes
FAIL cowfs  concurrency  concurrent_readers_and_writers_of_one_file   99.61ms  torn read at offset 0: block mixes two writes
```

Seven distinct offsets in 11 tears, all 4 KiB aligned, raw output in
`bench/out/ready-45/native-check.tsv`.
The tear rate is about 1 in 3000 reads here, which is the same order as the 8 to 297 per 300k reads
already recorded in `lib.rs`.

This is the conclusion the skip asserts, now measured on this host: the tear belongs to the kernel
page cache, not to `cowfs`.
It also means the oracle must not be relaxed.
Mixed-generation reads under concurrency are legal POSIX behaviour on Linux, and the test is right
to call that check `Cowfs`-only and skip it through a mount.

## What `cowfs` actually promises, and the test that holds it

The skip gives up one property: that a 4 KiB read never mixes two writes while writers run.
It does **not** have to give up anything a caller can observe, and nothing did hold that down.
The existing coverage had a specific hole:

`crates/cowfs-core/tests/conformance.rs` builds its factory with `Options { background: false, .. }`
and the default `file_flush_bytes`, which `crates/cowfs-core/src/inner.rs:63` sets to `4 << 20`.
The check's file is `PAGES * PG` = 16 * 4096 = 64 KiB.
So `io.rs:120`'s threshold `after >= self.opts.file_flush_bytes` is never reached and
`FileData::flush` is never called: that arm exercises the in-memory overlay alone and never crosses
the flush boundary where the chunk list is republished and the overlay is cleared.

So the invariant the skip leans on, "the `Core` is coherent where it is directly observable", was
never tested across a flush.
`coherence.rs` closes that.

### Why the `Core` is coherent, from the source

Every write of file content takes the node's write lock and mutates `FileData` under it
(`io.rs:90-139`), and every read snapshots `chunks` and the dirty overlay under the same read lock
before reading any block (`io.rs:44-58`).
`FileData::write` (`file.rs:136`) merges into one `BTreeMap` of disjoint runs, and
`FileData::flush` (`file.rs:209`) publishes the new chunk list and clears the overlay in one step
under that lock.
The background flusher reaches the same code through `flush_node`, which takes `node.st.wr()`
(`inner.rs:664-667`).
So a reader either sees the whole overlay or none of it, and can never straddle a flush.
`read_range` (`file.rs:372`) copies block bytes first and lays the overlay on top, and the overlay
was copied while the lock was held, so the two views agree.

## The invariant, stated so no interleaving can excuse a failure

For each 4 KiB block, after all writers have stopped, and after any acknowledged write has returned
to its caller:

1. the block reads back **uniform**: every one of its 4096 bytes equal.
2. the block's value is one that **some acknowledged write actually wrote**.

`coherence.rs` records a receipt per block, the set of values for which a write returned, and
checks both properties once the writers have joined.
Point 2 is what makes this an integrity check rather than a concurrency check: a lost write shows
as the pre-fill value `1`, which no writer ever writes, and a torn write shows as a mix.
Both are illegal once the writers have stopped, and neither is a legal interleaving artifact.

Point 2 is also why the fixture's own failing case is honest.
When the fixture writes each block as two half-block writes with different values, that is a
torn write, and the test must reject it.

## Oracle validation: the assertion is load-bearing

A passing assertion proves nothing unless it can fail.
I mutated the fixture so each writer splits its 4 KiB write into two halves carrying different
values, rebuilt, and ran the mount arm:

```
test concurrent_writers_leave_every_block_uniform_and_acknowledged ... panicked at
crates/cowfs-fuse/tests/coherence.rs:138:
block 0 is torn at rest with no writer running: it holds 63 and [101, 101, 101, 101]
test result: FAILED. 0 passed; 1 failed
```

The oracle fired on the injected defect, named the block, and reported the mixed bytes.
The source was then restored, rebuilt, and re-run: all four tests pass, and the restored binary's
SHA-256 matches the pre-mutation binary, `1b6629fa12c8d988079f298a5afa90df5dec1b5df875829f3209b8759a679b8a`.

## Measured arms

Host: `Linux 6.12.109+rpt-rpi-2712 aarch64`, 4 cores, moonscape.
Binary: `target/debug/deps/coherence-a198afafd2e72431`,
SHA-256 `1b6629fa12c8d988079f298a5afa90df5dec1b5df875829f3209b8759a679b8a`.
All arms ran through the shared lane `/home/moonscape/cowfs-ready-wave/linux-heavy.lock`, serial.
Source identity: `Cargo.lock` SHA-256
`0b47cb02a7fe7f6bdf0447286e512d43fae04201df8f3a7e93e97833d45696b9`, synced from this lease.

Contention is fixture-owned and bounded: one spinner per two cores, alive only inside an arm, so
g3, g4 and g5 keep their share of the shared host.
No timing claim is made or implied.

| arm | what runs under the readers and writers | reps | reads | torn under concurrency | torn at rest | verdict |
| --- | --- | --- | --- | --- | --- | --- |
| `core` | real `Core` called directly, `file_flush_bytes` forced to 4096, background flusher on | 3 | 6000 | 0 | 0 | pass |
| `mount` | real `Core` under a real FUSE mount, reached through the mount's own syscalls | 8 | 6000 | 0 | 0 | 7 pass, 1 killed by me |
| `native` | native tmpfs, same harness, no `cowfs` underneath | 5 | 6000 | 0 | 0 | pass, reported |
| `visibility` | mount and native, acknowledged writes with and without a barrier | 2 | 32 blocks | n/a | 0 | pass |

All four tests in the file, restored source, one invocation:

```
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 42.49s
REAL_EXIT_CODE=0
```

### Reading the table honestly

The `native` arm of this table shows 0 torn, while the native arm of the oracle table above shows 11
torn in 40.
That is not a contradiction, and the difference is the sample.
Each rep here runs 6000 reads, so 5 reps is 30k reads, and at about 1 in 3000 that is too small to
expect a hit; the native reps also finish in under a second each, which is why `run_arm.sh` adds
spinners.
The 40-rep native run is the one that carries the claim, and it used the repository's own check
body rather than this fixture.
The `mount` arm's concurrent count is reported, never asserted, for the same reason the skip exists.

`mount` rep 7 is recorded with `rc=143`: I sent `SIGTERM` to my own rep loop during it and it was
killed.
That rep contributes no data and is not counted as a pass.
Reps 1 to 6 and 8 completed with `rc=0` and 0 torn, and the row is kept in
`bench/out/ready-45/mount-tear-count.tsv` rather than deleted.

## What I did not change, and why

No `cowfs` source file is modified.
The defect the issue describes is not in `cowfs`, the measurement above shows that, and a fix would
mean weakening the `Core` to satisfy a property the kernel already denies on ext4, btrfs and tmpfs.

The one production change is a dev-dependency: `cowfs-core` added to `crates/cowfs-fuse`, so the
adapter can be tested over the backend it is deployed with.
Every other FUSE test in the crate runs over `MemVfs`, so no test anywhere mounted a real `Core`.

## Overlap and dependencies, reported rather than resolved

- `crates/cowfs-meta/src/tx.rs:314` raises `clippy::collapsible_match` under
  `cargo clippy -- -D warnings` on this toolchain.
  It is unmodified at base and is not mine to fix.
  It belongs to the #42 lane, which owns the other Core/meta API residuals.
  Under the workspace default (`clippy::all = warn`) the build is clean and `cowfs-fuse` and my
  test produce no warning at all.
- The Core flush, gate and barrier methods are the namespace #96 builder's lane.
  My test reads `Options::file_flush_bytes` and exercises the flush path, and changes nothing in
  `io.rs`, `inner.rs`, `gate.rs` or `file.rs`.
- The existing `statfs_free_after_unlink` skip in the same file is a different finding and is not
  touched.

## Honest limits of this delivery

- The measurement is on one 4-core Raspberry Pi host with one filesystem, tmpfs, as the native
  control.
  ext4 and btrfs were not measured here, though `lib.rs` records both tearing.
- The mount arm ran 8 reps, of which 7 completed.
  The tear rate through a mount is characterised by the 200-run figures already on the issue, not
  by this sample.
- No crash, power-loss or fsync-durability claim is made.
  This is a read/write coherence result on a live filesystem, nothing more.
- One process of mine, pid 987929, remains in uninterruptible sleep on the host in
  `request_wait_answer`.
  That is my own cleanup damage: I signalled my rep loop's parent while a FUSE request from that
  same process was in flight, so the kernel waits on a reply whose session thread had already
  exited.
  It holds no mount, uses no CPU while in `D` state, and cannot be killed until the request
  resolves.
  My mount was unmounted by exact path and the other workers' mounts were verified intact
  throughout.
- Not verified here: behaviour on a machine where `/dev/shm` is not writable, where the native
  control falls back to `std::env::temp_dir()` and may not be tmpfs.

## Reproducing

```sh
# native control, the repository's own check body, unskipped
COWFS_CONFORMANCE_FILTER=concurrent_readers \
COWFS_PATHVFS_NATIVE_DIR=/dev/shm/cowfs-native-check \
  cargo test -p cowfs-vfs-path --test native -- --ignored --nocapture --test-threads=1

# real Core, the same check body, unskipped
cargo test -p cowfs-core --test conformance -- \
  --exact concurrent_readers_and_writers_of_one_file --nocapture --test-threads=1

# the new invariant tests, on the production path
cargo test -p cowfs-fuse --test coherence -- --ignored --nocapture --test-threads=1
```

`bench/out/ready-45/run_arm.sh` is the bounded repetition wrapper used for the tables: it appends
and flushes one row per rep, stops the arm at the first failure, records the child's real exit code
rather than a pipeline's, and owns the contention fixture for the life of the arm only.