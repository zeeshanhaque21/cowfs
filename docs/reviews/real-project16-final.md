# Review: PR #114, real-project acceptance for #15 and #16

Reviewer verdict: **REQUEST CHANGES.**
The lane's own conclusion is right and honestly stated: warm-base acceptance for #15 and #16 is **NOT met**.
The harness is real work and most of its receipts hold up.
Two things block merge, and neither is the acceptance itself: CI is red at step one and never ran the harness, and the suite's default self-skip path reports `ok` while doing nothing.

- PR: #114, `test(treehouse): real-project acceptance for #15/#16, failing closed on the warm base`
- head reviewed: `1ca8242a1a134ae25d43292d00f0a25c852efad6`, branch `verify/treehouse-real-project-16`
- base of the PR: `03bbec85626c26a85ee4f5791d3a413fe47725bc` (`main` at review time)
- parent of the head: `46b0f269d5bef4a2c204c25f5b3015da601d3beb`
- changed files: 2, `crates/cowfs-treehouse/tests/real_project_acceptance.rs` (+1511) and `docs/verification/ready-real-project.md` (+316)
- no production source changed by this PR
- closing references: none. `closingIssuesReferences` is empty and #15 and #16 are both `OPEN`. Verified over GraphQL with exact-number aliases, not by reading the body.
- this review changed no source, committed nothing, merged nothing, returned no lease

## 1. What I ran myself

My own complete sample, on one private Core daemon, under the wave resource lock, as a single foreground command.
Source: `bench/out/real-project-critic/critic_sample.py`, then two isolating probes.
Receipts: `bench/out/real-project-critic/critic-sample.jsonl`, 86 flushed records, all inside my own lease.

Sample: `git archive 1ca8242` of `crates/cowfs-treehouse`, 18 files, 306,821 bytes, extracted to my own path.
Binaries: copied from lease `cowfs-7c1bf8/4`, built 2026-10-04 16:12 there, digests recorded:

```
cowfs-daemon    b6b9970944b487249d3041f40ed78d6c8755116e3df19b7d7d953cf14b4eeb51
cowfs           59b88405ab1e3ad00ed242938684ae01f56fe2254bfa18e34a6fe7c445bb9f3c
cowfs-treehouse 65a42acb158c37e46b4a454057cd0d56bec7d90d8c7c5ef51dc4f1f0b15fab6e
```

Manifest caveat, stated rather than hidden: those binaries are a copy from another lane's target directory, not a build I made, because my own lease sits on a different branch and a cold workspace build is out of budget.
They are defensible here for one reason and one reason only: the PR adds a test file and a document, so `1ca8242` and `46b0f26` have identical production source.
That argument is mine and it is not a substitute for a recorded build, and the harness has no such record at all (finding F4).

Toolchain on the host, measured: `git version 2.56.0`, `rustc 1.99.0 (b940084d7 2026-09-28)`, `cargo 1.99.0 (5f94df478 2026-08-27)`, `Darwin 25.6.0`.
Free disk at the run: 423 GiB against the 20 GiB floor. No 405M cold build was run.

Executed, in order, all real:

```
import              rc 0  verified true  files 18  bytes 306821
source_root_hash    cb6c46439ce1c4bbd35ad8d6adce37ace8acef786979e1bee3cdc315146a14fb
imported_root_hash  cb6c46439ce1c4bbd35ad8d6adce37ace8acef786979e1bee3cdc315146a14fb
promote             rc 0
fork                rc 0  parent critic-base
native mount table  lists exactly my own mount path, nothing else
export readback     23 entries, tree digest 2075eb15a5acac8ec182ac0f4ffc78d9c8cb22eb583a78e76f7fd1aaebc7420d
native control      23 entries, tree digest 2075eb15a5acac8ec182ac0f4ffc78d9c8cb22eb583a78e76f7fd1aaebc7420d
```

The export holds the imported tree entry for entry and byte for byte, and it equals the do-nothing baseline over the same tree on a real filesystem.
That is the source-bound root readback the review needs: same hash on both sides, both from the daemon's own verification and from an independent walk.

My assertion tally: run 1 recorded 21 assertions, 18 held, 3 failed. Probe 2 recorded 7, all 7 held.
The 3 failures are accounted for below and none of them is a production defect.
Nothing was reported from a component smoke test: the deliverable is the whole import to reset chain over a live NFS mount, and it ran end to end.

Teardown, verified rather than assumed: `cowfs shutdown` exit 0, daemon reaped with return code 0, `mounts_left_listed` empty after the shutdown, shared daemon 15263 argv byte-identical before and after.
15263 is alive since Sat Oct 3 20:44:29 2026 on `~/.cowfs/store`, was never signalled, unmounted or traversed.
No process group was signalled, no `pkill`, and every path I touched was asserted to be under my own runtime root before use.

## 2. Lineage identities, which is where the evidence is weakest

The lane ran the suite twice. Both runs are real and both were all-green, and they measured different trees.

```
run 1  run-full-suite.log   566.85s  8 passed  0 failed  1 ignored  sample_commit 46b0f26  tracked 6360 KiB
run 2  run-final-suite.log   552.58s  8 passed  0 failed  1 ignored  sample_commit 1ca8242  tracked 6436 KiB
```

The tracked-size growth of 76 KiB is exactly the PR's own two files. The harness is the sample project, so every edit to the harness changes the corpus it measures.

Per-run receipts diverge further, and the public report only quotes one of them:

```
quantity                    run 1 (46b0f26)                 run 2 (1ca8242, the committed head)
import files                1837                            1909
import bytes                11,544,622                      11,805,821
import root hash            14637cc5747b19ff79eda7432a49a2aacb825e36a8121208edf7c9d56bef393a   4b9088aca3547edc96b1927f9c1b90d485694f9ab2f223b754c51d9dbe119ca3
exported entries            2215                            2287
slot-build rlib sha256      3555b5a0af31114380b3b22ecb66f4e21dd760ac80a564a6b1f94906ab97d011   4d68f646af7736e15c206d0318518552ee41d0fa691303977e8762db9f3de8e6
native rlib sha256          b1848bf5ca86ff91307ffbd9a3d9dc03a7070a5ca455689987abebedbdb9b27a   0b2323811eb2fb82a69994e6ceb079a48907c058333d8e33ac67229d93293bc4
export KiB                  286,615                         286,933
```

`docs/verification/ready-real-project.md` quotes every number in its left-hand column and never mentions run 2.
It labels the sample "cowfs at 46b0f269..." and prints the command that produced both runs, so a reader cannot tell that the head under review was measured at different values.
That is finding F5, and it is the reason the report cannot be cited as the acceptance record for `1ca8242` as it stands.

On the native control, the report is right and should be kept: the two rlib digests differ because the absolute path is baked into debug info, so byte identity would have been the wrong assertion, and the exit codes are the deliverable.
No timing, speed or overhead claim is made anywhere in this review or accepted from the PR.

## 3. Acceptance receipt counts, stated as counts

The suite has 9 tests: 8 default, 1 `#[ignore]`d.

```
executed and asserted for real          8 of 8   both lane runs, no SKIP line in either log
executed as a self-skip that reports ok  0 of 8   in the lane's two runs
self-skipped when the sibling binary is absent  5 of 8   reproduced by me, see F1
ignored, never executed                 1        warm_base_acceptance_over_a_real_core
```

Not measured by this PR, and correctly declared as such: mode (a) against a real pool, warm-base publication, the two-clone-from-published-base flow, and anything about speed, dedup, last-writer-wins or crash durability.

The five tests that self-skip are `a_published_warm_base_must_be_discoverable_with_its_provenance`, `the_core_daemon_refuses_base_refresh_and_publishes_nothing`, `the_companion_still_refuses_a_materialiser_the_daemon_provides`, `every_implemented_mode_b_postcondition_holds_over_the_real_core` and `a_real_project_builds_and_tests_inside_an_exported_slot_snapshot`.

## 4. Findings

### F1, blocker: the default invocation reports `ok` for five tests that did nothing

`Core::start()` returns `None` when `target/debug/cowfs-daemon` is not beside the test binary, and the five daemon tests then print one `SKIP:` line and `return`.
libtest reports that as `ok`, and `cargo test` exits 0.
The module comment calls it "a skip with a printed reason and never a pass". That is false as written.

Reproduced with the lane's own compiled test binary, copied alone into a directory with no sibling binaries, no rebuild:

```
test the_companion_still_refuses_a_materialiser_the_daemon_provides ... SKIP: no cowfs-daemon beside this test binary, or no usable mount adapter
ok
test the_core_daemon_refuses_base_refresh_and_publishes_nothing ... SKIP: no cowfs-daemon beside this test binary, or no usable mount adapter
ok
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 7 filtered out; finished in 0.00s
```

Process exit code 0, zero daemon started, zero Core operation performed.

The same run shows the suite is inconsistent with itself: `the_cache_hook_is_installed_and_read_back_from_the_real_config` uses `.expect("the companion is built beside this test")` and in the identical environment fails hard with exit 101.
So one missing binary makes the suite simultaneously green on five gates and red on one.

The lane was not bitten, because `build-bins.log` shows a full workspace bin build at 16:12 that put `cowfs-daemon`, `cowfs` and `cowfs-treehouse` in `target/debug` beside the test.
Nothing enforces that. `cargo test -p cowfs-treehouse --test real_project_acceptance` is the command the report tells a reader to run, and on a fresh target directory it is a no-op that passes.
`--test <name>` also does not build another package's binaries, so the documented command is exactly the one that does not guarantee the precondition.

Required: a missing sibling binary must be a failure, or the skip must be visible in the test result rather than in stderr.

### F2, blocker: CI is red at step one and never ran the harness

One read of the single run on this head, run `37253861884`, head `1ca8242a1a134ae25d43292d00f0a25c852efad6`:

```
linux-fuse           111586768819  success
check (macos-latest) 111586768823  failure   02:10:07 -> 02:10:31
check (ubuntu-latest) 111586768879  failure   02:10:03 -> 02:10:04
```

Both failures are `cargo fmt --all --check`, 39 diff hunks, in exactly one file: `crates/cowfs-treehouse/tests/real_project_acceptance.rs`.
Every hunk is rustfmt re-wrapping a multi-argument `assert_eq!` or `assert!`.
Because that is the first step, clippy and `cargo test --workspace` never ran on either runner.
So the count of harness tests executed by CI on this head is exactly zero, and the "8 passed" marker comes only from two local runs.

The lane ran clippy locally (`clippy.log`, clean, 0.19s) and never ran `cargo fmt --all --check`. That is the whole gap.
`rustfmt` is mechanical and belongs in this PR.

### F3, blocker: the `#[ignore]` reason and the report both misattribute the blocking dependency

The blocking dependency is not provenance. It is that `base_refresh` does not exist on the Core backend.

```
crates/cowfs-daemon/src/handler.rs   base_refresh calls can_ingest()? first
crates/cowfs-daemon/src/backend.rs    CoreBackend::ingests_directories() is false, documented as deliberate
```

The report gets this right in its own body, and my own run confirms the refusal is real and by name:

```
exit 1  cowfs-treehouse: cowfs: unsupported: this backend stores snapshots as trees,
        not as directories: copy the source into the mount path instead
```

But the ignore attribute says `blocked on #98: warm-base provenance is not published or discoverable yet`, and the report heads its section "Dependency 4: #98, provenance, which is what acceptance actually requires".
Those two statements are wrong in a way that matters: #98's path-record persistence work cannot make `base_refresh` publish anything on the Core backend, because the call is refused before any persistence happens.
Closing #98 alone leaves mode (b) with no base and leaves this acceptance unreachable.
The precise dependency chain is: `can_ingest` gate, then #97 path resolution, then #98 provenance, and the report's ownership table hands the `can_ingest` gate to the #98 lane without an issue that scopes it.

### F4, major: nothing binds the daemon binary to the source under test

The `core-daemon` receipt records argv, socket, store, mount and the adapter string. It records no binary digest, no build commit, and no `rustc` version.
The only compiler record in the whole suite is inside the native-control test.
So the receipts prove a real Core daemon answered, and do not prove which Core daemon.
In the lane's own runs the daemon binary was built at 16:12 from `46b0f26` while the harness binary and sample came from `1ca8242`.
That is defensible here because the PR changes no production source, and it is exactly the kind of thing that stops being defensible the moment it does.
My own manifest in section 1 is the minimum that should be mandatory.

### F5, major: the report's numbers are from a different commit than the head it documents

Covered in section 2. The report quotes run 1 and presents it as the record for the committed head.
The final run at `1ca8242` exists in the lane's raw evidence and is not in the document.
Two of the numbers a reader would use to compare against a later head, `1837 files` and `2215 entries`, are the values for a tree that does not contain the harness itself.

### F6, major: the in-slot build is never shown to run inside the export

`a_real_project_builds_and_tests_inside_an_exported_slot_snapshot` mounts, then runs `cargo build` and `cargo test` with `current_dir` set to the slot and `CARGO_TARGET_DIR` inside it, and asserts only the two exit codes and that an rlib exists.
It never re-reads the native mount table, never checks a device or filesystem identity, and never confirms the store that answered.
Its sibling test does assert `core.is_listed(&slot)`, so the suite knows how, and this test omits it.
Consequence: if `mount_snapshot` ever returned `MountInfo { mounted: true }` without a real mount, this test would run a perfectly ordinary native build and report PASS for a build that never touched cowfs.
The report's PASS row "Real `cargo build` of the project inside the export" is therefore wider than what was asserted.
An API's own `mounted: true` is not a mount table readback.

### F7, major: the ignored acceptance does not test what the report says it tests

`warm_base_acceptance_over_a_real_core` asserts, in order: `base refresh` exit 0 with a named snapshot and a real commit, `base status` exit 0 with `fresh` true, then for two slots a `snapshot create --from <published base>`, that the listed parent equals the published base, a `mount_snapshot`, and a real `cargo build` and `cargo test`.
It contains no reset and no untouched-base assertion.
The report says it asserts "a reset that returns the slot to an untouched base", and the test's own doc comment says the same.
Both statements are false about the code as written.
Two further gaps in that test: `mount_snapshot` is checked only through the returned `MountInfo`, same problem as F6, and it never asserts the base is still intact after the two builds.

### F8, minor: the `#[ignore]` acceptance is a real gate, and it must stay unaccepted

It is the only artifact here that would constitute acceptance for #15 and #16.
It must remain `NONACCEPTED` until it is actually executed end to end and passes, and nothing in this review changes that.
No production patch is requested against the Core ingest boundary here; that seam belongs to the #98 lane per the dispatch doc.

### F9, minor: `Added` then `AlreadyThere` is recorded, not asserted

The report's PASS row for the cache hook cites `Added`, then `AlreadyThere`, as evidence.
The test records both stdout strings and asserts neither; it asserts exit 0 twice, that the path is the one the companion claims, the presence of the table, the hook name, the sentinel, that an existing setting survived, and that the two installs are byte-identical.
The idempotency assertion is the real check and it is sound. The `Added`/`AlreadyThere` claim is an assertion in prose only.

### F10, minor: the export-versus-source check is one-directional

The comment says "Everything tracked in the sample is present in the export, and nothing else is."
The assertion only collects source entries missing from the export. Extra entries in the export are never rejected.
The reset comparison in the same test is genuinely bidirectional, `base_tree == slot_tree_after_reset`, so the gap is confined to the ingest readback.

### F11, major: the teardown can abort the run and can fail open

Three defects in `impl Drop for Core`, on a shared machine, all reproduced by reading the code rather than by triggering it.

1. `Drop` begins with `assert!(ps.contains("cowfs-daemon"))`. A panic inside `Drop` during an unwinding test failure is a double panic, which aborts the process and destroys the failure message. If the daemon exited on its own, `ps` returns empty and the assert fires.
2. `listed_under_base` returns an empty vector when the `mount` command fails or its output shape changes. `Drop` then records `mounts_left_listed` as empty, which reads as a clean readback when the truth is that nothing is known. There is no tri-state, so the harness fails open on its own safety check.
3. The `shutdown` call and every `umount` go through `sh`, which has no timeout. macOS `umount` against a server that is gone is exactly the case that already hung this harness once, and the fix recorded in the report does not bound it.

Required: bounded commands with a whole-deadline, a tri-state mount readback that treats an unreadable table as Unknown and refuses to proceed, and no panic in `Drop`.

### F12, major: the watchdog's abort path skips teardown entirely

`common::Watchdog::start` spawns a thread that calls `std::process::abort()` when the budget expires.
`abort` runs no destructors, so `Core::drop` never runs and the run leaves a live daemon and a live NFS mount behind, which is the exact state the report says wedged this machine once.
The budgets are 900s and 3600s against a ~550s suite, so it does not fire in the recorded runs. It fires the first time a build genuinely hangs.

### F13, minor: #97 is correctly scoped as old-source, and one attribution in the report is wrong

The `#97` reproduction is real: `git worktree add --detach <sha>` exits 0 and prints prose, `HEAD is now at ...`, and the `-q` form prints nothing, so the last-line-as-a-path parse has no valid input on git 2.56.0. Both forms leak a worktree named after the commit.
The leak measurement and its cleanup are correct. The cleanup enumerates `git worktree list` and removes each real path, so it uses the gitdir path rather than the parsed stdout line, and it asserts the list is empty afterwards. `plain_worktrees_after_cleanup` and `worktree_list_entries_after_cleanup` are both `0`. That part of the harness is sound.

Scope, stated precisely:
`crates/cowfs-daemon/src/import.rs` is byte-identical between `46b0f26` and `main` `03bbec8`, same blob `df2c831d`. The unfixed code path is therefore present on main as well, at the source level.
The reproduction itself was measured only at `46b0f26`; it was not re-measured on main, and this review does not claim it was.
The real repair is `b59bc3c` "fix(daemon): give base_refresh an explicit worktree path, stop parsing stdout (#97)", which touches `import.rs` and `crates/cowfs-treehouse/tests/canonical.rs`. It is not an ancestor of `46b0f26` and not an ancestor of `main`.

The report's attribution is wrong. It says "#97 is reported as fixed in PR #92 at `e243cb17971955397b3bfc67c16f6f13223d0d57`".
`e243cb1` is `docs(namespaces): retract the through-the-wiring PASS, and name what is still broken`. It changes one file, `docs/linux-namespaces.md`, +110/-28, and touches no code.
It is a commit inside the `feat/linux-namespaces-17` branch, whose PR head is `c3bafb7`. It is not the #97 fix.
No production fix is requested here, and none is needed for this PR. The report should name `b59bc3c`, and should say the boundary plainly: the finding stands for the code in this lane and for main, and the repair exists only on an unmerged branch.

### F14, minor: the materialiser gap is correctly named, and my run bounds it

The companion's `CowfsMaterialiser` still refuses and cites "the control protocol has no `mount_snapshot` method (docs/v1-treehouse.md, gap 1)".
That is false of the daemon. `crates/cowfs-daemon/src/exports.rs` implements it, gated by `--export-root`, and `docs/v1-control-api.md` lists it.

My own run bounds the remaining gap precisely, and it is narrower than "materialisation is unsupported". On one real Core daemon over one real NFS mount I completed: a verified import, a promote, an O(1) fork, a write into the export that read back byte for byte without touching the base, and a reset. All of that is materialisation.
So the actual remaining unsupported transition is exactly one: `CowfsMaterialiser` never calls `mount_snapshot`, companion-side.
No production patch from this review; that is the companion lane, and the report already says so.

### F15, informational: a post-reset readback through a still-mounted export can lag by about a second

This is my own measurement, and it is a limit of the evidence, not a defect.
In my first run I read the export immediately after `snapshot reset` and saw the pre-reset tree, 24 entries with my marker file still present, while `reset` had returned exit 0.
Isolating it: a fresh daemon reading the same store sees the correct post-reset state immediately, so the reset is durable in the store.
Reading the same still-mounted path at intervals gives the timing:

```
0.00s after reset   24 entries   marker present   != base   != native
2.00s after reset   23 entries   marker absent    == base   == native
6.04s after reset   23 entries   marker absent    == base   == native
12.02s after reset  23 entries   marker absent    == base   == native
```

So: the store is correct, and the live export view converges within about two seconds.
Cause not determined by this measurement; it is consistent with a cache-propagation delay and I am not claiming more than that.
Why it matters for this PR: the harness always unmounts and re-exports before reading back after a reset, which is the right way to avoid the race, and that means the harness never characterises it. The report's honest-limits section does not mention it either.
For mode (b) a slot reset is a routine operation on a slot an agent already has mounted, so this belongs in the report as a measured limit with its receipts.
My run 1 also contained one assertion of my own that was vacuous, a `Cargo.toml` read at the wrong relative path, which returned `absent` on both sides and so compared equal to itself. Probe 2 read the file at `crates/cowfs-treehouse/Cargo.toml`, digest `09961142a08446ab2981e724168c118024c0ad23b89352b3948216b97ba27a31`, identical on the slot and the base after reset.

### F16, informational: the PR branch is behind `main`, and the merge is mechanically clean

The PR's base of record is `03bbec8`, which is `46b0f26` plus the crash-88 series: `90c9a8f`, `2db2f7f`, `265fc3b`, `7d9380a`, `373e5bb`.
`main` moved during this review to `8255706` (PR #104 merged), which is `03bbec8` plus four added files, two scripts and two documents.
Nothing under `crates/` changed, so the harness's one dependency is unaffected: `crates/cowfs-treehouse/tests/common/mod.rs`, which the new test does `mod common;`, is byte-identical between `46b0f26` and `8255706`.
`git diff 03bbec8 1ca8242` reads as 9 files with 5,092 deletions, which is an artefact of diffing two tips and must not be reported as this PR deleting the crash harness.
The PR's real change is `git diff 46b0f26 1ca8242`: 2 files, +1,827, 0 deletions.
Tracked entries at review time: `main` 534, head 531.
No claim is made here that any later ready-lane fix is integrated into this head. The head predates all of them.

## 5. What is genuinely good and should survive the review

- The module doc states the negative result up front and names the two source seams that cause it. That is the right shape for a failing-closed gate.
- Every record is appended and flushed per item, so an interrupted run keeps what it measured. 26 records survive per run in the lane's raw evidence.
- Daemons are private: own store, socket, mount and export root, under a resolved `TMPDIR` root, with the socket directory at `0700`, and `private_tempdir` sets `0700`.
- Teardown identifies the daemon by pid and argv before signalling, asks the daemon to unmount its own exports first, and re-reads the native table before passing any path to `umount`. No `pkill`, no process-group signal, no recursive walk of a mount.
- The native control is a real do-nothing baseline at the same commit, lockfile and toolchain, and the report correctly refuses byte-identity of the rlib and explains why.
- The dependency-1 refusal test asserts a clean refusal, not merely an error: exit 1, the message names the reason, the snapshot list is empty, and `git worktree list` is unchanged.
- The two 550s runs were both single-threaded and coherent, and both left nothing mounted.

## 6. Required before merge

1. `cargo fmt --all --check` clean, F2. Mechanical, and CI is red without it.
2. A missing sibling binary must not report `ok`, F1. Either fail, or surface the skip in the test result.
3. Correct the blocking dependency in the ignore attribute and in the report's section headings, F3, and name `b59bc3c` rather than `e243cb1`, F13.
4. Report run 2's numbers for the committed head, or state plainly that the report describes `46b0f26` and not `1ca8242`, F5.
5. Add the daemon binary digest and the source commit to the `core-daemon` receipt, F4.
6. Re-read the native mount table in the in-slot build test, F6.
7. Either add the reset the ignored acceptance is documented to have, or stop saying it has one, F7.
8. Fix the three `Drop` defects and the watchdog's destructor-free abort, F11 and F12.
9. Add the post-reset readback lag to the report's honest-limits section, F15.

Items 1 and 2 are merge blockers.
The rest are required for this report to be citable as the acceptance record; the acceptance itself stays unaccepted either way.

## 7. Scope I did not cross

- No source file was edited. No commit, push, merge or lease return.
- My lease stayed at `6df8b9fcfd6f9d54a6af0c8e424f3851fc6ecc71` on `review/git-integrity-21`. All my writes are under `bench/out/real-project-critic/**`.
- PR #114 has no closing references and #15 and #16 stay `OPEN`.
- The Core ingest boundary was not patched. No #98, #20, #96, #79, #42, companion or import file was touched.
- No Linux cross-build, no 805M soak, no repeated 600s waits, no runner config, workflow dispatch, rerun or poll.
- 15263, and every other worker's daemon, socket, store and mount, were left alone and re-verified afterwards.
- No performance, dedup, last-writer-wins or crash-durability claim is made or accepted.
- The raw ignored evidence stays private to the lanes. This report cites no artifact link that does not exist.

## 8. Verdict

**REQUEST CHANGES.**
#15 and #16 warm-base acceptance: **NOT met**, and the lane says so itself, which is the most valuable thing in the PR.
The three blockers behind it are real, pinned and reproducible.
What is not acceptable yet is that CI never ran this suite, the suite's own default invocation is a silent no-op that reports green, and the document presents one commit's numbers as the record for another.