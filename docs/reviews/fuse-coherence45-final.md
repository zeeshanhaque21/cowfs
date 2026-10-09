# Independent review: PR #105, FUSE read/write coherence, issue #45

Reviewer: ready-wave slot 13, lease `748aec3c71201afdaeb690847dfbab10`, branch `review/fuse-coherence-45`.
Reviewed head: `45fcebff5eabdbe97766659cb0c929571f08b17f`, verified exact before and after all work.
Base: `46b0f269d5bef4a2c204c25f5b3015da601d3beb`.
PR: #105. Issue: #45, still open.

Verdict: **BLOCK**, on two small mechanical defects plus two evidence-integrity defects.
The technical conclusion is sound and I reproduced its central claim independently.
No `cowfs` production defect was found, and no production change was needed, so the "no source fix" call is correct.
Nothing here disputes the differential finding.
What blocks merge is that the PR does not build under `--locked`, and that its documented reproduce command executes zero tests while returning success.

## 1. Exact identities reviewed

| item | value |
| --- | --- |
| head | `45fcebff5eabdbe97766659cb0c929571f08b17f` |
| branch | `review/fuse-coherence-45` |
| files changed | `crates/cowfs-fuse/Cargo.toml`, `crates/cowfs-fuse/tests/coherence.rs`, `docs/verification/ready-45.md` |
| `coherence.rs` sha256 | `88577bfdf0ba5c917c9ca3ea64e3f91170d1b908577cf1065d489bb3730dd213` |
| committed `Cargo.lock` sha256 | `2ed20c88136771f956e4170aa248e5261b3249a2ba4a0e4cb6b6d49aa8e46f77` |
| `Cargo.lock` after any cargo build | `0b47cb02a7fe7f6bdf0447286e512d43fae04201df8f3a7e93e97833d45696b9` |
| `conformance.rs` sha256 at base and at head | `7c0c72fc3cd7e9b9299e0c4063474ad8cdb2ca67d98f60bbe4d06a7679b92213`, byte-identical |
| my test binary | `coherence-4ae8afeea6fae250`, sha256 `cd139bd1ddc167e52cb3d35c140752bd77ad15c722f425272ad03eca9173baf4` |
| builder's test binary | `coherence-a198afafd2e72431`, sha256 `1b6629fa12c8d988079f298a5afa90df5dec1b5df875829f3209b8759a679b8a`, as claimed in the doc |

The last two rows matter.
My binary hash differs from the builder's, and so does the `Cargo.lock` each build produced.
The builder's binary is therefore **not reproducible from the reviewed head**, which is a build-provenance fact, not a defect in itself.

Host, as I verified it: `moonscape` at `192.168.68.119`, `aarch64`, 4 cores, cargo 1.95.0, rustc 1.95.0.
`/home` is `ext4` on `/dev/sda2`; `/dev/shm` is `tmpfs`.
Free disk 59 GiB before the build and 58 GiB after, both above the 20 GiB floor.
Reviewer artifacts 1.6 GiB, under the 8 GiB cap.
All heavy work ran as one foreground child through the shared lock `/home/moonscape/cowfs-ready-wave/linux-heavy.lock`, with a 600 s bounded wait that exits 75 rather than running unlocked.

## 2. Findings

### F1, blocking: the committed `Cargo.lock` is stale for this PR's own manifest change

PR #105 adds `cowfs-core` to `crates/cowfs-fuse`'s dev-dependencies but does not update `Cargo.lock`.
The only difference between the committed lock and the lock cargo produces is one added line, `"cowfs-core",`.
Consequence, measured:

```
cargo metadata --locked   -> rc 101
cargo check  -p cowfs-fuse --all-targets --locked -> rc 101
error: cannot update the lock file ... because --locked was passed to prevent this
```

Any consumer building with `--locked` or `--frozen` fails on this branch.
CI does not use `--locked`, so CI cannot catch it.
Fix is one command, `cargo update -p cowfs-core` or an equivalent targeted lock refresh, committed.

### F2, blocking: the documented reproduce command runs zero tests and exits 0

Both `crates/cowfs-fuse/tests/coherence.rs:25` and `docs/verification/ready-45.md:225` tell the reader to run:

```
cargo test -p cowfs-fuse -j4 --test coherence -- --ignored --nocapture --test-threads=1
```

No test in `coherence.rs` carries `#[ignore]`.
I ran that exact command against the built binary:

```
running 0 tests
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 4 filtered out; finished in 0.00s
A_RC=0
```

A reader who follows the documented path gets a green exit code and no coverage whatsoever.
The same binary without `--ignored` runs all four tests and passes.
So the doc's own headline result, "test result: ok. 4 passed", cannot have come from the command the doc prints.
Either the invocation or the printed result is wrong.

### F3, evidence integrity: "about 1 in 3000 reads" is inferred, not measured

The doc states the native tear rate as "about 1 in 3000 reads here".
The repository's own check, `crates/cowfs-vfs-test/src/conformance/concurrency.rs:109`, returns `Ok` or an `Err` and carries **no read counter**.
Its per-rep read count is also time-bounded by `CAP`, so it varies with host load.
Forty reps of it therefore yield a rep-level pass/fail count and no denominator at all.
The doc presents an inference in the same voice as its measured counts, which is exactly the failure mode the repo's own measurement rules forbid.

I supplied the missing denominator with a fixture that does count.
Twenty reps of the `coherence.rs` native arm on `/dev/shm` tmpfs, no `cowfs` in the path:

```
N2_TOTAL reps=20 writes=120000 reads=120000 torn_concurrent=17 torn_at_rest=0 rate=1/7059
```

Per-rep torn counts ran 0, 1, 1, 2, 2 and so on, confirming the tear is real and probabilistic.

### F4, evidence integrity: the recorded "source identity" is a build side effect

The doc records `Source identity: Cargo.lock SHA-256 0b47cb02...`.
That is the hash of the lock **after cargo rewrote it**, not the hash of any committed source state.
The committed lock at the reviewed head is `2ed20c88...`.
Labelling a cargo mutation as source identity is what let F1 go unnoticed, since the number looks stable and authoritative while in fact it changes on every build.

### F5, minor: the cited raw evidence is not reachable from the PR

The doc points at `bench/out/ready-45/native-check.tsv` and `bench/out/ready-45/mount-tear-count.tsv`.
`.gitignore:10` ignores `/bench/out/`, so those files are local only and a PR reviewer cannot open them.
The tracked doc does carry the numbers inline, so this is a convenience gap, not a hidden claim.

### F6, pass: the no-production-defect conclusion is correct, and I reproduced it

The doc's central claim is that the CI tear is not a `cowfs` defect, and that the pre-existing skip is correct.
That holds up.

Forty reps of the repository's own check body, unskipped, on native `ext4` with no `cowfs` in the data path:

```
N1_reps_with_FAIL=17  N1_total_FAIL_lines=17  N1_FAIL_lines_other_checks=0
N1_distinct_offsets=11  N1_non_aligned=0
offsets: 0 4096 12288 16384 28672 32768 36864 40960 49152 57344 61440, all 4096-aligned
```

Seventeen of forty reps tore, every tear was the concurrency check, and every offset was 4 KiB aligned.
The builder reported 11 of 40 with seven distinct offsets.
Same phenomenon, same order of magnitude, different sample, as expected for a low-rate probabilistic tear.
Neither number is a rate.

My own `cowfs` runs never tore: mount arm 6000 writes, 6000 reads, 0 torn, and the direct-`Core` arm likewise.
So the differential hypothesis, "this is a `cowfs` defect", is refuted.
A native counterexample does **not** prove every `cowfs` concurrency and flush path is safe, and I did not claim it did.
It only removes the reason to change production code.

### F7, scope limit the doc should state plainly: the rest oracle is membership, not last-write-wins

`coherence.rs` records, per block, the set of values for which some write returned to its caller, then asserts the block is uniform and holds one of those values.
That is a safety property: no tear at rest, and no never-written block quietly changed.
It is **not** a last-write-wins or no-lost-write property.
If a block's final acknowledged write were lost and an older acknowledged value survived, the block would be uniform and its value would be in the receipt set, so the oracle would pass.

Excluding `PREFILL` from the writer value space detects only a block that was never written.
It does not establish that no acknowledged write was lost.
The doc's own wording, "a lost write shows as the pre-fill value", is true only for a block that received no write at all.
The doc's section heading "Honest limits" does not say this, and it should.
On the strength of this test, no zero-data-loss claim is available; `docs/design.md` reserves crash-injection zero-loss for a separate criterion, and I assert nothing there.

### F8, pre-existing, reported not fixed: conformance failures do not fail the Rust test

The conformance suite reports a failed check by printing a `FAIL cowfs ...` line, while the Rust test itself exits 0.
I hit this myself: all forty native reps exited 0, and seventeen of them contained a real tear.
Any result read from the exit code alone is wrong, and my first harness pass was wrong for exactly that reason before I re-derived it from the printed lines.
This is why `.github/workflows/ci.yml` carries `|| true` on the native page-cache control.

A second consequence, pre-existing and not this PR's doing: the `linux-fuse` job pipes `cargo test` into `tee` under GitHub's default `bash -e`, which has no `pipefail`, and its enforcement step greps only for `^FAIL`.
A Rust-level test failure prints `test ... FAILED`, not `^FAIL`, so a `coherence.rs` failure would not fail that job.
The real gate is `cargo test --workspace` in the `check` job, which has no pipeline.
I did not execute a deliberately failing PR to confirm the CI behaviour end to end, because workflow dispatch and reruns are forbidden here, so treat this as a reading of the workflow text, not a measured result.

### F9, pre-existing at base, confirmed not this PR's: `clippy::collapsible_match`

`cargo clippy --workspace --all-targets -- -D warnings` fails at `cowfs-meta/src/tx.rs:314`, rc 101.
`git show --stat 45fcebf -- crates/cowfs-meta/` is empty, and base `46b0f26` carries the identical file, so this PR neither introduced nor can fix it.
Main's CI is green, so the lint fires on rustc/clippy 1.95.0 on this host and not on CI's pinned stable.
It belongs to the #42 lane, as the builder said, and I did not edit it.
Under the workspace default, `cargo clippy -p cowfs-fuse --all-targets` exits 0 and `cowfs-fuse` plus the new test emit no warning, which I confirmed.

## 3. What I verified independently, and how

Every probe below is mine, ran in the reviewer-owned archive at `/home/moonscape/cowfs-ready-wave/task-45-review/`, and is deleted from the source tree afterwards.
No tracked test and no production file was edited.
Source identity in the archive is the `git archive` of the exact head, and `coherence.rs` there still hashes to `88577bfd...` at the end of the review.

### The flush boundary is genuinely crossed, and the threshold is what drives it

The doc's argument is that the default `file_flush_bytes` of 4 MiB is never reached by a 64 KiB file, so the default conformance arm never crosses the boundary.
I confirmed the mechanism in the source: `crates/cowfs-core/src/io.rs:96` reads `let after = f.dirty_bytes()`, and `io.rs:120` flushes when `after >= self.opts.file_flush_bytes`, under the node write lock, into production `FileData::flush`.

Reading it is not proof, so I measured it through the public `Core::stats().dirty_bytes`, which is unflushed bytes.
With `background: false`, so the only code that can clear `dirty_bytes` is the synchronous threshold branch:

```
P1 after 2048B write, threshold 4096: dirty_bytes=2048
P1 after 2nd 2048B write, crosses 4096: dirty_bytes=0
P1 flushes=0 barriers=0 batches=0 dirty_bytes=0
P1 OK threshold crossed and republished 16 times
```

Sixteen consecutive full-block writes each drove `dirty_bytes` back to zero.
The aggregate is per inode, the threshold is the trigger, and publication really happens on the write path.

### The mount arm's at-rest reads are not served by the kernel page cache

This was my main worry about the new test.
Its mount arm reads through the same mount the writers used, so a page cache could in principle satisfy those reads without the `Core` ever being consulted, which would make the arm nearly vacuous.
Two probes close it.

Read every block twice, once through the mount and once straight from the `Core` snapshot view:

```
P2 OK 16 blocks byte-identical through the mount and straight from the Core
```

Then the strongest boundary available without a crash test: write through a real mount, drop both the mount and the `Core`, reopen the same store with a fresh `Core`, and read back:

```
P3 OK 16 blocks correct after dropping both the mount and the Core, from a fresh open
```

Nothing is cached in any process at that point, so a pass means the bytes reached the store through the production flush path.
I did not add a production hook, a test-only build flag, or any new `cowfs` source to obtain this.

### The oracle is load-bearing

Baseline, pristine source:

```
coherence.rs sha256 BEFORE: 88577bfdf0ba5c917c9ca3ea64e3f91170d1b908577cf1065d489bb3730dd213
M1_BASELINE_RC=0
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

Mutated, by replacing each writer's single 4 KiB block write with two half-block writes carrying two different values from that writer's own sequence, generated mechanically from head bytes as one hunk:

```
mutant source sha256 IN: a452effaf5055446882ddbd48d56595ca28c835a01347f78ffbd936a6ae06416
M2_MUTANT_RC=101
block 0 is torn at rest with no writer running: it holds 34 and [45, 45, 45, 45]
block 0 is torn at rest with no writer running: it holds 158 and [169, 169, 169, 169]
test result: FAILED. 1 passed; 3 failed
```

The oracle fired, named the block, and reported the mixed bytes.
The direct-`Core` arm failed on the same assertion, so that arm is load-bearing too.
The mutator itself performs two distinct writes at two offsets, which is a torn write by construction.
This demonstrates the oracle can fail and says nothing about a `cowfs` defect.

Restored, from the pristine source bytes, rebuilt, re-run:

```
coherence.rs sha256 AFTER:  88577bfdf0ba5c917c9ca3ea64e3f91170d1b908577cf1065d489bb3730dd213
M3_RESTORED_RC=0
test result: ok. 4 passed; 0 failed
M3 source restored byte-identical to HEAD: YES
M3_BIN sha256=cd139bd1ddc167e52cb3d35c140752bd77ad15c722f425272ad03eca9173baf4
```

The restored binary digest equals the baseline digest exactly.
The restore is proved by source bytes, and independently corroborated by the binary digest.
Reviewer test files left in the archive source tree: 0.

## 4. Interruption and D-state accounting, preserved not cleaned

The doc records mount rep 7 as `rc=143`, killed by the builder's own signal, contributing no data and counted as no pass.
That is the correct treatment and I am not relabelling it.

I verified the consequence rather than trusting it.
Pid `987929` exists, state `D`, `wchan request_wait_answer`, command `[coherence-a198a]`, which is the builder's own coherence binary.
Elapsed time grew from 26:35 at the start of my review to 43:00 at the end, so the request has still not resolved.

I did not signal it, did not attempt to clear it, and did not try to walk or repair anything around it.
I reproduced no cleanup experiment of that shape.
Its explanation in the doc, a `SIGTERM` to the rep loop's parent while a `FUSE` request from that same process was in flight, is consistent with the observed `wchan` and with `ppid 1`.

The builder's claim that its own mount was already unmounted is corroborated: no mount under any `task-45` path appears in the mount table, and never did during my review.

Counting stays separated, as required.
Forty plus forty refers to the native and `Core` oracle arms.
Four plus one interrupted refers to the mount acceptance reps.
The flushing acceptance denominator is the four-test invocation, reported separately, and is not folded into either.

## 5. Tests, lint and CI snapshot

One READ of each, never a poll:

| check | result |
| --- | --- |
| `cargo test -p cowfs-fuse --test coherence -- --ignored ...` (the documented command) | `running 0 tests`, `ok. 0 passed`, rc 0 |
| `cargo test -p cowfs-fuse --test coherence -- --nocapture --test-threads=1` | `ok. 4 passed; 0 failed`, rc 0, 44.30 s and 43.84 s on two runs |
| my three probes | `ok. 3 passed; 0 failed`, rc 0 |
| mutant | rc 101, `FAILED. 1 passed; 3 failed` |
| restored | `ok. 4 passed`, rc 0, binary digest equal to baseline |
| `cargo metadata --locked` | rc 101, F1 |
| `cargo check -p cowfs-fuse --all-targets --locked` | rc 101, F1 |
| `cargo clippy -p cowfs-fuse --all-targets` | rc 0, no `cowfs-fuse` or test warning |
| `cargo clippy --workspace --all-targets -- -D warnings` | rc 101 at `cowfs-meta/src/tx.rs:314`, pre-existing, F9 |

The four tests do execute on Linux.
They are not `#[ignore]`d, so CI's `cargo test --workspace` in the `check` job runs them, and `linux-fuse` runs them under `--include-ignored`.
Green CI here is a real signal for these tests, not an ignored-test artefact, which is the opposite of the F2 trap.

CI at the reviewed head, one snapshot:

```
summary: "3 passed, 0 failed, 3 total"
check (ubuntu-latest)  pass
check (macos-latest)   pass
linux-fuse             pass
```

Run `37246373869`, completed success, branch `fix/fuse-torn-read-45`.
An earlier read of mine showed two of these pending; that was a stale snapshot and the completed run is green.
Main's run `37245670732` is also green, which is what makes F9 a toolchain finding rather than a repo breakage.

## 6. Issue #45, scoped resolution

Issue #45's premise was that a hosted `ubuntu-latest` run reproduced
`concurrent_readers_and_writers_of_one_file` failing, and its body states that "CI workflow will keep gating this failure, not skip it".
That premise is now refuted as a `cowfs` defect, and I confirmed the second half is no longer true either way: the skip was introduced by `fd5ec1f`, before this branch, and `conformance.rs` is byte-identical between base and head.

So #45 should **not** be closed as "defect found and fixed", because there was no defect and no production change.
The correct closure is: not a `cowfs` defect, native page-cache behaviour measured with no `cowfs` in the path, the pre-existing mount skip is justified, and the invariant the skip leans on is now held by `coherence.rs`.
Keep it open until after merge, and close with that wording.

Explicitly **not** claimed anywhere by this review:
any public, all-POSIX, performance, crash-durability or g6 acceptance result.
Any zero-data-loss claim, for the reason in F7.
Any statement about macOS, `ext4` durability, or the other in-flight ready-wave lanes.

## 7. Recommendation

Merge after these, and they are small:

1. Commit the `Cargo.lock` refresh that F1 requires. Without it the branch does not build under `--locked`.
2. Drop `--ignored` from the two documented commands, or add `#[ignore]` to the four tests and keep it. Either is fine, but the doc and the code must agree, because right now the printed command and the printed result cannot both be true.
3. Replace the inferred "about 1 in 3000 reads" with the measured `17 tears in 120,000 reads, about 1 in 7,059`, and say that the repository's own check reports no read count so the 40-rep figure is a rep count and not a rate.
4. Relabel the recorded `Cargo.lock` hash as a post-build artefact, or record the committed `2ed20c88...` as source identity.
5. Add one sentence to "Honest limits" stating that the at-rest oracle is membership in acknowledged writes, so it does not establish last-write-wins or absence of lost writes.

None of these require touching production code, and none require another reviewer.
F7 and F8 are documentation and pre-existing-workflow observations, not merge blockers.

Residual risk I could not retire: the at-rest invariant is proven on `tmpfs` and `ext4` at 4 KiB block granularity with 3 writers and 3 readers on one 4-core host, and probabilistically, so absence of a tear in my sample is not proof of absence in general.
No crash, power-loss or `fsync` durability behaviour was exercised, and none is claimed.
Durability remains owned by #96.