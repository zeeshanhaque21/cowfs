# ready-45: FUSE read/write coherence, native reproduction and the invariant that replaces the skip

Task: issue #45, "FUSE conformance: intermittent torn read in concurrent readers/writers on hosted
Linux".
Base: `46b0f269d5bef4a2c204c25f5b3015da601d3beb`.
Branch: `fix/fuse-torn-read-45`.
Compact tracked proof: [`evidence/coherence45-repair.md`](evidence/coherence45-repair.md).

## Verdict, scoped

The tear reported by CI is reproducible on Linux with **no `cowfs` code in the data path**.
In the arms tested here, `cowfs` did not tear: neither a real `Core` called directly nor a real
`Core` under a real FUSE mount showed a tear.

That is a scoped result, not a blanket one.
It is not a claim that no `cowfs` production defect exists anywhere, and it is not a claim about
macOS, `ext4` durability, or any other ready-wave lane.
What it establishes is narrower and sufficient: the reported mixed-generation tear is native Linux
page-cache behaviour, so there is no reason to change production code for it.
No `cowfs` source file is modified by this PR.

## Why the existing skip is correct

`crates/cowfs-vfs-test/src/lib.rs:117-121` classifies
`concurrent_readers_and_writers_of_one_file` as a `Cowfs`-only check, because POSIX calls a read
atomic against a concurrent write and Linux buffered I/O does not provide that.
`crates/cowfs-fuse/tests/conformance.rs` skips it through the mount on that basis.
That skip was introduced by commit `fd5ec1f`, before this branch's base, and `conformance.rs` is
byte-identical between base and head (sha256
`7c0c72fc3cd7e9b9299e0c4063474ad8cdb2ca67d98f60bbe4d06a7679b92213`).
This PR adds no skip, no retry, no serialization, and weakens no invariant.

The oracle is not wrong and must not be relaxed.
A read that mixes two writes while writers are running is legal POSIX behaviour on Linux.

## What was missing, and what this adds

The existing `cowfs-core` conformance arm builds its factory with `Options { background: false, .. }`
and the default `file_flush_bytes`, which `crates/cowfs-core/src/inner.rs:63` sets to `4 << 20`.
The check's file is `PAGES * PG` = 64 KiB.
So `io.rs:120`'s threshold `after >= self.opts.file_flush_bytes` is never reached and
`FileData::flush` is never called: that arm exercises the in-memory overlay alone and never crosses
the boundary where the chunk list is republished and the overlay is cleared.

So the invariant the skip depends on, that the `Core` is coherent where it is directly observable,
was not tested across a flush.
`crates/cowfs-fuse/tests/coherence.rs` closes that on the path production runs: a real `Core` under a
real FUSE mount, reached through the mount's own syscalls, with flushes forced.

### Why the `Core` is coherent, from the source

Every content write takes the node write lock and mutates `FileData` under it (`io.rs:90-139`).
Every read snapshots `chunks` and the dirty overlay under the same read lock before reading any
block (`io.rs:44-58`).
`FileData::write` (`file.rs:136`) merges into one `BTreeMap` of disjoint runs, and
`FileData::flush` (`file.rs:209`) publishes the new chunk list and clears the overlay in one step
under that lock.
The background flusher reaches the same code through `flush_node`, which takes `node.st.wr()`
(`inner.rs:664-667`).
A reader therefore sees the whole overlay or none of it and cannot straddle a flush.

## The invariant, stated so no interleaving can excuse a failure

For each 4 KiB block, once all writers have stopped:

1. the block reads back **uniform**, every one of its 4096 bytes equal;
2. the block's value is one that **some acknowledged write actually wrote**, recorded per block as a
   receipt after the write returned to its caller.

Both are illegal to violate once the writers have joined, and neither is a concurrency artifact.

**What this oracle does not establish.** It is a membership test, not last-write-wins.
If a block's final acknowledged write were lost and an *older* acknowledged value survived, the
block would be uniform and its value would be in the receipt set, so this test would pass.
Excluding `PREFILL` from the writer value space catches a block that received no write at all; it
does not catch a lost later write.
Accordingly **no zero-data-loss, no-lost-write, or durability claim is made here.**
Crash and power-loss durability remain owned by #96 and by `docs/design.md`'s separate criterion.

## Oracle validation: the assertion is load-bearing

A passing assertion proves nothing unless it can fail.
Each writer's single 4 KiB block write was replaced with two half-block writes carrying two
different values from that writer's own sequence, then rebuilt and run:

```
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 3 filtered out
block 0 is torn at rest with no writer running: it holds 63 and [101, 101, 101, 101]
```

The oracle fired, named the block and the mixed bytes.
Source was restored from the pristine bytes, rebuilt, and re-run: 4 passed, and the restored source
hash equals the pre-mutation hash `88577bfdf0ba5c917c9ca3ea64e3f91170d1b908577cf1065d489bb3730dd213`.

## F1: the branch now builds under `--locked`

Adding `cowfs-core` to `crates/cowfs-fuse`'s dev-dependencies required a `Cargo.lock` refresh that
the first commit omitted, so `--locked` consumers failed.
Reproduced before the fix, with direct exit codes:

```
cargo metadata --locked                              -> rc 101
cargo check -p cowfs-fuse --all-targets --locked     -> rc 101
error: cannot update the lock file ... because --locked was passed to prevent this
```

The lock was then regenerated with a standard cargo command, never hand-edited.
The whole diff is one added line inside the `cowfs-fuse` package's dependency list, and no version
anywhere else moves:

```
@@ -295,6 +295,7 @@ dependencies = [
 name = "cowfs-fuse"
 version = "0.0.0"
 dependencies = [
+ "cowfs-core",
```

After the fix:

```
cargo metadata --locked                              -> rc 0
cargo check -p cowfs-fuse --all-targets --locked     -> rc 0
```

Confirmed identically on the Linux host under the shared lane.

## F4: source identity is the tracked revision, not a build side effect

The earlier revision of this document recorded `Cargo.lock` sha256 `0b47cb02...` as "source identity".
That number was the lock **after cargo rewrote it during a build**, so it changed on every build and
was not a property of any committed state.
It is now a tracked artifact and legitimately part of the source identity.

- tracked `Cargo.lock` at this head: `0b47cb02a7fe7f6bdf0447286e512d43fae04201df8f3a7e93e97833d45696b9`
- `coherence.rs`: `88577bfdf0ba5c917c9ca3ea64e3f91170d1b908577cf1065d489bb3730dd213`

Binary digests are build-specific and are not reproducible proof of a source revision; each is
labelled with the build it came from in the evidence file.

## F2: the reproduce command, corrected

The previous document, and the test file's own doc comment, told the reader to pass `--ignored`.
No test in `coherence.rs` is `#[ignore]`d, so that flag filtered all four out and still exited 0.
Reproduced with direct exit codes:

```
argv: coherence-a198afafd2e72431 --ignored --nocapture --test-threads=1
running 0 tests
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 4 filtered out
OLD_ignored_rc=0
VERDICT=FAIL  (green, but zero tests executed)

argv: coherence-a198afafd2e72431 --nocapture --test-threads=1
running 4 tests
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 41.59s
NEW_exact_rc=0
VERDICT=PASS  (rc=0, 4 tests executed and passed)
```

The old "4 passed" result in the earlier document therefore could not have come from the command it
printed. The flag is removed from the doc comment, and the correct invocation asserts test
*discovery*, not just a green exit code.

## F8: the conformance harness's exit code lies, so results are parsed from its output

`cowfs-vfs-test`'s conformance runner reports a failed check by printing a `FAIL cowfs ...` line while
the Rust test itself exits 0. Measured on native `ext4`, six reps of the repository's own check body,
unskipped:

```
rep=1 rc=0 FAIL_lines=0   rep=4 rc=0 FAIL_lines=0
rep=2 rc=0 FAIL_lines=0   rep=5 rc=0 FAIL_lines=1
rep=3 rc=0 FAIL_lines=1   rep=6 rc=0 FAIL_lines=1
```

Three of six reps printed a real tear while every exit code was 0, and the offset 12288 in two of
them matches the offset class in the original CI report:

```
cowfs concurrency concurrent_readers_and_writers_of_one_file 100.02ms torn read at offset 12288: block mixes two writes
cowfs concurrency concurrent_readers_and_writers_of_one_file  99.37ms torn read at offset 4096: block mixes two writes
```

Any conclusion drawn from that harness's exit code alone is wrong.
Every native figure below is derived from parsed `FAIL` lines, and every counting-harness figure
comes from the test's own printed counters.

## Measured arms

Host `moonscape` at `192.168.68.119`: `Linux 6.12.109+rpt-rpi-2712 aarch64`, 4 cores, cargo 1.95.0,
rustc 1.95.0, `/home` is `ext4`, `/dev/shm` is `tmpfs`, 58 GiB free.
Remote artifacts under `/home/moonscape/cowfs-ready-wave/task-45-repair/`.
Heavy work ran serially as one foreground child through
`/home/moonscape/cowfs-ready-wave/linux-heavy.lock`; a lock wait failure exits 75 and was never
worked around by running unlocked.
Contention is fixture-owned and bounded: one spinner per two cores, scoped to a single arm and torn
down by a trap, so the g3, g4 and g5 lanes keep their share of the host.
No timing or quiet-performance claim is made.

### Native arms, reported separately because they are different experiments

| experiment | filesystem | source of the number | reps | result |
| --- | --- | --- | --- | --- |
| repository's own check body, unskipped, builder | tmpfs | `FAIL` line count, no read counter exists | 40 | 11 reps tore, 7 distinct offsets |
| repository's own check body, unskipped, builder | tmpfs | same | 40 | reported alongside the above |
| repository's own check body, unskipped, independent reviewer | ext4 | same | 40 | 17 reps tore, 11 distinct offsets |
| repository's own check body, unskipped, this revision | ext4 | `FAIL` line count | 6 | 3 reps tore, offsets 4096 and 12288 |
| `coherence.rs` native arm, counting harness | tmpfs | the test's own read counter | 20 | 2 tears in 120000 reads |
| `coherence.rs` native arm, counting harness, independent reviewer | tmpfs | the test's own read counter | 20 | 17 tears in 120000 reads |

**Rate claim withdrawn.**
The repository's own check carries no read counter and its per-rep read count is time-bounded by an
internal cap, so a rep count from it is a rep count and never a rate.
The earlier "about 1 in 3000 reads" was an inference presented in the same voice as measured counts,
and it is withdrawn.

What can be said, with each figure attributed to its own experiment:
the two counting-harness `tmpfs` experiments, each 20 reps and 120000 reads, measured 2 tears and 17
tears respectively, about 1 in 60000 and about 1 in 7059.
These are separate samples of a low-rate probabilistic event on a shared 4-core host, they disagree,
and neither is a general rate.
The repository-check experiments are rep-level counts only: 11 of 40 and 17 of 40 on tmpfs and ext4
respectively.
Different filesystems, different harnesses and different samples; the numbers are not combined.

### cowfs arms

| arm | what runs under the readers and writers | reps | torn under concurrency | torn at rest |
| --- | --- | --- | --- | --- |
| real `Core` direct | `file_flush_bytes` forced to 4096, background flusher on | 3 | 0 | 0 |
| real `Core` under a real mount | reached through the mount's own syscalls | 8 started, 7 completed | 0 | 0 |
| four-test invocation | all tests, restored source | 1 | 0 | 0 |

No `cowfs` arm tore in any run recorded in this document or in the evidence file.
Absence of a tear in these samples is not proof of absence in general.

## Interruption accounting, preserved

One mount rep, rep 7, is recorded `rc=143`: I sent `SIGTERM` to my own rep loop while it was running,
so it was killed and contributes no data.
It is not counted as a pass.
The row is preserved in the evidence rather than deleted.

The consequence is also preserved, not cleaned up.
Pid `987929` remains in uninterruptible sleep with `wchan request_wait_answer`.
Cause: the `SIGTERM` went to the rep loop's parent, which was the FUSE session serving a request from
that same process, so the kernel waits on a reply whose session thread had already exited.
It holds no mount and cannot be killed until the request resolves.
I did not signal it again, did not attempt to clear it, and did not walk or repair anything around it.
My own mount was unmounted by exact path; no mount under any `task-45` path appears in the mount
table.
The g4, g5 and g4-review mounts and daemons were verified intact throughout and never touched.

Counting stays separated:
the native and `Core` oracle arms, the mount acceptance reps, and the four-test invocation are three
different denominators and are not folded together.

## Ownership and lane boundaries

- Adds `cowfs-core` to `crates/cowfs-fuse`'s dev-dependencies. Every other FUSE test in the crate runs
  over `MemVfs`, so no test anywhere mounted a real `Core`.
- `crates/cowfs-meta/src/tx.rs:314` raises `clippy::collapsible_match` under
  `cargo clippy --workspace --all-targets -- -D warnings` on rustc/clippy 1.95.0.
  It is unmodified at base and belongs to the #42 lane, which owns the other Core/meta API residuals.
  It is a toolchain-version finding, not a repo breakage: main's CI is green.
  I did not edit it.
- Under `-D warnings` restricted to my own surface, `cargo clippy -p cowfs-fuse --all-targets`
  exits 0 and emits no warning from `cowfs-fuse` or `coherence.rs`.
  The workspace-wide `-D warnings` failure is entirely that one dependency file.
  Reporting the workspace default as if it were `-D warnings` would be a false pass, so both numbers
  are given.
- The Core flush, gate and barrier methods are the namespace #96 builder's lane.
  This test reads `Options::file_flush_bytes` and exercises the flush path; it changes nothing in
  `io.rs`, `inner.rs`, `gate.rs` or `file.rs`.
- The pre-existing `statfs_free_after_unlink` skip is a different finding and is untouched.

## Honest limits

- One 4-core host, and the native controls are `tmpfs` and `ext4` only.
  No APFS, btrfs or macOS measurement here.
- Probabilistic results with disagreeing samples; see the rate section.
- The at-rest oracle is membership in acknowledged writes, not last-write-wins, so no zero-data-loss
  or durability claim follows from it.
- No crash, power-loss or `fsync` durability behaviour was exercised.
- The mount arm ran 8 reps, 7 completed.
- Behaviour when `/dev/shm` is not writable is not verified: the native control falls back to
  `std::env::temp_dir()`, which may not be tmpfs.
- The reviewer-owned probes used to confirm the flush threshold, the mount-versus-`Core` byte
  equality, and the drop-and-reopen path are the reviewer's evidence, not mine, and are cited as such.

## Reproducing

```sh
# the four invariant tests on the production path. No --ignored: it would run zero tests and exit 0.
cargo test -p cowfs-fuse --test coherence -- --nocapture --test-threads=1

# native control, the repository's own check body, unskipped.
# The exit code is 0 even when the check tears: parse the FAIL lines.
COWFS_CONFORMANCE_FILTER=concurrent_readers \
COWFS_PATHVFS_NATIVE_DIR=/path/on/ext4 \
  cargo test -p cowfs-vfs-path --test native -- --ignored --nocapture --test-threads=1

# the real Core, the same check body, unskipped
cargo test -p cowfs-core --test conformance -- \
  --exact concurrent_readers_and_writers_of_one_file --nocapture --test-threads=1
```

`bench/out/ready-45/run_arm.sh` is the bounded repetition wrapper used for the tables.
It appends and flushes one row per rep, stops the arm at the first failure, records the child's real
exit code rather than a pipeline's, and owns the contention fixture for the life of the arm only.
It is gitignored and local; the tracked numbers are in
[`evidence/coherence45-repair.md`](evidence/coherence45-repair.md).

## Issue #45 closure wording, for the coordinator after merge

Not "defect found and fixed": there was no `cowfs` defect and no production change.
Accurate scope: the reported mixed-generation tear is native Linux page-cache behaviour, measured with
no `cowfs` in the data path; the pre-existing mount skip is justified and unchanged; and the
coherence invariant that skip depends on is now held by `coherence.rs` across a forced flush.
Keep #45 open until merge.