# Compact tracked proof: issue #45 FUSE read/write coherence repair

Curated, human-readable observation copies from preserved raw runs.
Full narrative and limits: [`../ready-45.md`](../ready-45.md).

**What is tracked here and what is not.** This file and `../ready-45.md` are tracked and reviewable on
the PR. The raw per-rep logs, per-rep stdout captures and build logs live on the Linux host under
`/home/moonscape/cowfs-ready-wave/task-45-repair/out/` and `.../logs/`, and in the builder's earlier
`task-45/out/`. Those are **not** in the repository: `.gitignore:10` ignores `/bench/out/`, so
`bench/out/ready-45/**` is local-only and not reachable from a PR. Nothing below was hand-edited to
invent a run; each block is copied from a preserved log, and every command line and exit code is
literal.

## Identities

Tracked source state at this head:

| item | sha256 |
| --- | --- |
| `Cargo.lock` (tracked, committed) | `0b47cb02a7fe7f6bdf0447286e512d43fae04201df8f3a7e93e97833d45696b9` |
| `crates/cowfs-fuse/tests/coherence.rs` | `88577bfdf0ba5c917c9ca3ea64e3f91170d1b908577cf1065d489bb3730dd213` |
| `crates/cowfs-fuse/tests/conformance.rs` | `7c0c72fc3cd7e9b9299e0c4063474ad8cdb2ca67d98f60bbe4d06a7679b92213` |

`conformance.rs` is byte-identical to base `46b0f26`, so the pre-existing `fd5ec1f` skip is unchanged
and this PR adds no skip, retry or serialization.

Build-specific digests, not reproducible proof of a source revision:

| build | binary sha256 |
| --- | --- |
| this revision, Linux | `26199e373705ff28752e0458097c2a68dfe93293947bba842496fae22151adfe` |
| first commit `45fcebf`, Linux | `1b6629fa12c8d988079f298a5afa90df5dec1b5df875829f3209b8759a679b8a` |
| independent reviewer build | `cd139bd1ddc167e52cb3d35c140752bd77ad15c722f425272ad03eca9173baf4` |

Host for every Linux run: `moonscape` `192.168.68.119`, `Linux 6.12.109+rpt-rpi-2712 aarch64`,
4 cores, cargo 1.95.0, rustc 1.95.0, `/home` `ext4`, `/dev/shm` `tmpfs`, 58 GiB free.
Heavy work ran as one foreground child through
`/home/moonscape/cowfs-ready-wave/linux-heavy.lock`; lock contention exits 75 and was never bypassed.

## F1: `--locked` before and after, literal exit codes

Reproduced on the builder's Mac checkout, then confirmed on the Linux host.

```
OLD_cargo_metadata_locked_rc=101
OLD_cargo_check_locked_rc=101
error: cannot update the lock file .../Cargo.lock because --locked was passed to prevent this
```

Regenerated with `cargo metadata --format-version 1`, never hand-edited.
Complete diff, one added line, no version moves anywhere:

```
@@ -295,6 +295,7 @@ dependencies = [
 name = "cowfs-fuse"
 version = "0.0.0"
 dependencies = [
+ "cowfs-core",
```

```
NEW_cargo_metadata_locked_rc=0
NEW_cargo_check_locked_rc=0
REMOTE_cargo_metadata_locked_rc=0
REMOTE_cargo_check_locked_rc=0
```

## F2: test discovery guard, old command versus corrected

```
argv: target/debug/deps/coherence-a198afafd2e72431 --ignored --nocapture --test-threads=1
running 0 tests
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 4 filtered out; finished in 0.00s
OLD_ignored_rc=0
VERDICT=FAIL (green, but zero tests executed)

argv: target/debug/deps/coherence-a198afafd2e72431 --nocapture --test-threads=1
running 4 tests
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 41.59s
NEW_exact_rc=0
VERDICT=PASS (rc=0, 4 tests executed and passed)
```

The old green result carried no coverage. `--ignored` is now removed from the test's doc comment,
which also states to assert `running 4 tests` before trusting a green result from this target.

## F3: rate withdrawn, each figure attributed to its own experiment

The repository's own check has no read counter and its per-rep read count is time-capped, so its
40-rep results are rep counts, not rates. The earlier "about 1 in 3000 reads" was an inference
presented like a measurement and is **withdrawn**.

Counting-harness experiments, each 20 reps, each 120000 reads, `tmpfs`, no `cowfs` in the path:

| experiment | tears | reads | implied |
| --- | --- | --- | --- |
| this revision | 2 | 120000 | about 1 in 60000 |
| independent reviewer | 17 | 120000 | about 1 in 7059 |

These are separate samples of a low-rate probabilistic event on a shared 4-core host. They disagree.
Neither is a general rate, and the two are not combined.

Per-rep detail for this revision, all 20 reps, 6000 reads each, `torn_at_rest` 0 in every rep:

```
rep=1  rc=0 writes=6000 reads=6000 torn_concurrent=0 torn_at_rest=0
rep=8  rc=0 writes=6000 reads=6000 torn_concurrent=1 torn_at_rest=0
rep=16 rc=0 writes=6000 reads=6000 torn_concurrent=1 torn_at_rest=0
(reps 2-7, 9-15, 17-20 all torn_concurrent=0 torn_at_rest=0)
```

Repository-check experiments, rep-level only, `FAIL` lines parsed:

| experiment | filesystem | reps | reps that tore | distinct offsets |
| --- | --- | --- | --- | --- |
| builder, first commit | tmpfs | 40 | 11 | 7 |
| independent reviewer | ext4 | 40 | 17 | 11 |
| this revision | ext4 | 6 | 3 | 2 |

## F8: the conformance harness exits 0 while the check tears

Six reps, native `ext4`, repository's own check body, unskipped:

```
rep=1 rc=0 FAIL_lines=0 other_checks=0
rep=2 rc=0 FAIL_lines=0 other_checks=0
rep=3 rc=0 FAIL_lines=1 other_checks=0
rep=4 rc=0 FAIL_lines=0 other_checks=0
rep=5 rc=0 FAIL_lines=1 other_checks=0
rep=6 rc=0 FAIL_lines=1 other_checks=0
```

Printed tears, including the offset class from the original CI report:

```
cowfs concurrency concurrent_readers_and_writers_of_one_file 100.02ms torn read at offset 12288: block mixes two writes
cowfs concurrency concurrent_readers_and_writers_of_one_file  97.29ms torn read at offset 12288: block mixes two writes
cowfs concurrency concurrent_readers_and_writers_of_one_file  99.37ms torn read at offset 4096: block mixes two writes
```

Every exit code was 0. Three of six reps tore. Any result read from that exit code alone is wrong.

## Oracle mutation: baseline, mutant, restored

Each writer's single 4 KiB block write replaced by two half-block writes carrying two different values
from that writer's own sequence, then rebuilt and run.

```
baseline: coherence.rs sha256 88577bfdf0ba5c917c9ca3ea64e3f91170d1b908577cf1065d489bb3730dd213
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

mutant:
block 0 is torn at rest with no writer running: it holds 63 and [101, 101, 101, 101]
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 3 filtered out

restored: coherence.rs sha256 88577bfdf0ba5c917c9ca3ea64e3f91170d1b908577cf1065d489bb3730dd213
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

The oracle fired on the injected defect, named the block and the mixed bytes.
The mutant is a torn write by construction, so this shows the oracle can fail and says nothing about
a `cowfs` defect.

Reviewer's independent probes, cited as the reviewer's evidence: `dirty_bytes` 2048 then 0 across 16
full-block writes at a 4096 threshold, confirming the flush boundary is really crossed; 16 blocks
byte-identical through the mount and straight from the `Core`; and 16 blocks correct after dropping
both the mount and the `Core` and reopening the store fresh.

## Lint

```
cargo clippy -p cowfs-fuse --all-targets                     -> rc 0, no cowfs-fuse or coherence.rs warning
cargo clippy -p cowfs-fuse --all-targets -- -D warnings       -> rc 0 on the owned surface
cargo clippy --workspace --all-targets -- -D warnings        -> rc 101, cowfs-meta/src/tx.rs:314
```

The workspace-wide `-D warnings` failure is one file, `crates/cowfs-meta/src/tx.rs:314`,
`clippy::collapsible_match`, unmodified at base and owned by the #42 lane. It fires on rustc/clippy
1.95.0 here and not on CI's pinned stable, so it is a toolchain finding.
Quoting the workspace default as though it were `-D warnings` would be a false pass, so both are
reported.

## Interruption, preserved not cleaned

Mount acceptance rep 7: `rc=143`, killed by my own `SIGTERM` to my rep loop. It contributes no data,
is not counted as a pass, and its row is kept:

```
rep=7 rc=143 torn_concurrent=? torn_at_rest=?
```

Cause of the residue: the `SIGTERM` reached the rep loop's parent, which was the FUSE session serving
an in-flight request from that same process, so the kernel waits on a reply whose thread had exited.
Pid `987929` remains in uninterruptible sleep, `wchan request_wait_answer`, no mount held, not
killable until the request resolves.
Not signalled again, not cleared, nothing around it walked or repaired.
My mount was unmounted by exact path; no mount under any `task-45` path is in the mount table.
The g4, g5 and g4-review mounts and daemons were verified intact and never touched.

## Denominators, kept separate

| measurement | count |
| --- | --- |
| native `ext4` repository-check reps | 6, of which 3 tore |
| native `tmpfs` counting-harness reps | 20, 120000 reads, 2 tears |
| real `Core` direct oracle arms | 3 reps, 0 torn |
| mount acceptance reps | 8 started, 7 completed, 1 killed, 0 torn |
| four-test invocation | 1, 4 passed |

Not folded together. The four-test invocation is a test-discovery and whole-target result, not a
concurrency sample.