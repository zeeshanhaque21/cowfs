# PR 92 final delivery review: `4c2d7ec9`

Reviewer: native critic, lease 14, `review/linux-namespaces-17`.
Head under review: `4c2d7ec95ce42076ff2aa814ec50b7209db26523`.
Prior point named in the brief: `78f19f86fc6530b23720bcb7a1e760c096b65657`.
Prior review of mine that must carry: `38111b4929bff46189f26e85f76e3e3c5229c44f` (`docs/reviews/base-provenance115-final.md`, sha256 `b9107d17d8c12ac9682028866e5dbe788b89c63e5631f5afe6d333d1ff3b354d`).
All gates run natively on the Mac, rustc `1.99.0 (b940084d7 2026-09-28)`, clippy `0.1.99`.
No SSH, no Linux mount, no daemon, no fixture touched, no source edited, no commit, no merge.

## Verdict

**BLOCK**, on one mechanical, one-line-fixable defect that the brief made an explicit requirement.

Everything the delta actually changes is correct and verified: the ETXTBSY test-fixture repair is sound, the injection and failure-handling properties of the new stub writer hold under direct test, all three corrected source-comment rows have receipts in the committed spike, production code is byte-unchanged so the issue-115 atomicity PASS carries, and the new Mac suite counts match the author's claims exactly.

The single blocker: **merging this PR auto-closes issue #124**, which is open, unimplemented, and explicitly not resolved here. See "Closing references" below.

## The delta is one commit, three files

`78f19f86..4c2d7ec9` is a single commit, `test(canonical): write the stub from a waited-for child, and measure it`.

```
 crates/cowfs-treehouse/tests/canonical.rs      | 241 +++++++++++---
 docs/verification/evidence/etxtbsy17-repair.md | 221 ++++++
 docs/verification/evidence/etxtbsy17-spike.md   | 289 +++++
```

Both evidence docs are new in this delta, not edited. The only source file is a test file.

## Source carry: my atomicity PASS is structurally carried, not re-argued

Byte-identity across `38111b49` -> `78f19f86` -> `4c2d7ec9`, sha256 of the blob at each point:

| file | 381 == 78 | 78 == final |
|---|---|---|
| `crates/cowfs-treehouse/src/mode_b.rs` | yes | yes |
| `crates/cowfs-daemon/src/backend.rs` | yes | yes |
| `crates/cowfs-daemon/src/base_meta.rs` | yes | yes |
| `crates/cowfs-daemon/src/import.rs` | yes | yes |

So nothing the `38111b49` review examined was touched by this delta. I still re-ran the four issue-115 arms on the final head to show the carry is behavioural and not only a file-hash argument, and they behave exactly as they did:

```
a_concurrent_published_base_survives_another_threads_create_on_core   EXIT=0  1 passed; 0 failed
a_concurrent_published_base_survives_another_threads_create_on_path   EXIT=0  1 passed; 0 failed
a_concurrent_rename_onto_a_name_keeps_the_base_on_core                EXIT=0  1 passed; 0 failed
a_concurrent_rename_onto_a_name_keeps_the_base_on_path                EXIT=0  1 passed; 0 failed
```

All four report `accepted=true, ran while parked=false`. Whole daemon lib on this head: **89 passed, 0 failed**, of which `backend` 25 and `base_meta` 15, unchanged from `38111b49`.

## Direct controls and exact test exits, in the required order

Own `CARGO_TARGET_DIR` at `bench/out/pr92-final-delivery-review/t-final`, used by exactly one tree.
Last round a shared target directory served a binary compiled from different source, so this is a correctness requirement, not bookkeeping.
I re-verified the four source hashes immediately before running anything.

`crates/cowfs-treehouse/tests/canonical.rs` at the reviewed head is `86b1b70f6c1f99b40455fc541c69171d105282459e795f651a33f7bc427ad634`.
That is byte-for-byte the hash the repair doc quotes as its measured `tree-new`.
So the tree the author measured is the published head, not a lookalike.

**1. First, the single real `write_stub` -> `run_build` sample.** Of all the tests in the file, exactly one drives both the new writer and `run_build` on a non-Linux-gated path: `the_helper_path_is_never_repeated_as_an_argument`.

```
EXIT=0   running 1 test   test result: ok. 1 passed; 0 failed; 0 ignored
```

**2. The four new writer controls, each separately, exact counts.**

```
EXIT=0   running 1 test   1 passed; 0 failed; 0 ignored   a_stub_is_byte_exact_and_executable_after_the_writer_exits
EXIT=0   running 1 test   1 passed; 0 failed; 0 ignored   a_stub_path_is_never_shell_source
EXIT=0   running 1 test   1 passed; 0 failed; 0 ignored   arbitrary_stub_bytes_survive_the_pipe
EXIT=0   running 1 test   1 passed; 0 failed; 0 ignored   a_stub_the_writer_cannot_create_is_refused
```

Every gate above ran a nonzero number of tests and I asserted the count from the log rather than from the exit code, so none of these can be a vacuous zero-test pass.

**3. The scoped canonical binary run, whole suite on this Mac.**

```
EXIT=0   running 14 tests   test result: ok. 14 passed; 0 failed; 0 ignored
```

Author claims macOS 14 passed, 0 failed, 0 ignored. Agrees exactly.
The `#[cfg(target_os = "linux")]` opt-in control `write_stub_is_safe_under_concurrent_exec` does not appear in the macOS run log at all, zero occurrences, which is why macOS carries no ignored entry.
Per the brief I did **not** run the 3200-exec stress, on either side.

**4. Formatting and lint, scoped to what this delta touched.**

```
cargo fmt --all --check                      EXIT=0
cargo clippy -p cowfs-treehouse --tests -D warnings   EXIT=0
```

## The new stub writer, reviewed against the asks

The old writer built a command by pasting the path into a shell string. The new one is:

1. spawn `/bin/sh -c 'cat > "$1"' stub-writer <path>`, with the writer's stdin piped;
2. write the body into the pipe;
3. `drop` the pipe, which is what produces the child's EOF;
4. `wait_with_output()`, so the child is reaped before the caller ever execs anything;
5. assert the child's exit status, and only then assert that the pipe write succeeded;
6. read the file back and assert byte equality with the body;
7. `chmod 0755` and assert the mode.

Path and body safety, which is what the brief asked me to confirm:

- **The script text is fixed.** It is the literal `cat > "$1"`. Nothing derived from the path or the body ever reaches the shell as source.
- **The path is positional and quoted** as `$1`, so metacharacters in the path cannot become shell source. There is a dedicated test for it, `a_stub_path_is_never_shell_source`, which uses a directory literally named `bin ' quote; * $(exit 3)` and asserts no file appears. It passes.
- **The body travels over stdin**, never as shell source, and its EOF is the dropped pipe.
- **NUL in the body.** `arbitrary_stub_bytes_survive_the_pipe` round-trips a body containing NUL bytes and does **not** exec it. That is the correct scope, and the test comment says so plainly: a shell is not required to read NUL as an ordinary byte. `cat` is an external program reading the pipe bytewise, which is why the round trip holds. Honest scoping, not a gap.

Command-failure and pipe-write-failure handling:

- If `/bin/sh` itself cannot be spawned, the `expect` on the spawn fails loudly.
- If `cat > "$1"` cannot create the file, the child exits nonzero, stderr is captured by `wait_with_output`, and the status assert fires with the shell's own message rather than a bare `Broken pipe (os error 32)`. `a_stub_the_writer_cannot_create_is_refused` asserts the panic message mentions the stub writer, so the test and the failure path agree on what the reader sees.
- **The ordering is the point and it is right.** Status is checked *before* the write result. If the writer could not create the file, the write may well also have failed as a consequence, and checking the write first would report the derived symptom and hide the cause.
- The residual case is a partial write where the child nonetheless exits zero. Then `written.expect` fires. That path is reachable but harmless: the test already failed, and the temp fixture is removed on drop, so a half-written stub is not left for a later exec.

**Bounded-test semantics.** The opt-in control is `#[ignore = "spawns thousands of processes; opt in with --ignored"]` plus `#[cfg(target_os = "linux")]`, so default CI never spawns 3200 processes. That is a deliberate, disclosed choice about a *control*, not a suppression of any real test, and the four real tests are not ignored. It does mean the 3200-exec measurement is manual-only, which is a real coverage reduction and I am recording it as one. The author states plainly that they ran it, twice at 3200.

## The three corrected source-comment rows do have receipts

The delta removes the old row `write then exec, fork + exec, 8 threads | 1 / 1600`, adds two new rows, and rewrites the prose conclusion. The conclusion narrowed from needing three things (write, concurrency, `posix_spawn`) to needing two (write-open and concurrency), because the fork+exec path now fails at a comparable rate.

All four numbers the prose and the table cite are backed by rows in the committed spike table:

| arm | what it was | execs | ETXTBSY | backs |
|---|---|---|---|---|
| `inplace-pre` | `inplace` plus a no-op `pre_exec`, so std uses fork+exec | 8 x 400 | **2** | `2 / 400 against 4 / 400` |
| `pub` | tmp + rename + chmod, then exec | 8 x 400 | **3** | `3 / 400`, rename does not help |
| `wopen` | write-open, **zero bytes written**, close, chmod | 8 x 3200 | **4** | `4 / 3200`, write-open alone is enough |
| `childwrite` | same bytes, written by a waited-for `/bin/sh` child | 8 x 3200 | **0** | `0 / 3200`, the new writer |

Two things make this table load-bearing rather than decorative, and both are stated in the spike doc:

- The `wopen` arm's remaining 3196 results are `ENOEXEC`, because a zero-byte file is not a valid executable. So the `4 / 3200` is a real ETXTBSY count off a real control, not a control that mostly failed for an unrelated reason.
- `inplace` pooled over its three runs is 97 of 6800 execs, 1.43%, so the old writer's rate is measured rather than asserted.

I am not re-auditing these numbers and I did not re-run any of them. I am recording that the claims in the source comment are traceable to measured rows rather than to nothing, which is what the brief asked for. Low wording is not a rejection ground.

## Original CI event versus the harness seam, kept distinct

The brief requires this distinction be recorded and not collapsed.

- The **original** failure, run `37244286403` of the `ci` workflow, is recorded in the spike doc as **not reproduced**. Reproducing it needs at least a dozen repeated runs of the real 13-test binary, and the author excluded that by budget. That remains true at this head. I did not attempt it.
- What *was* demonstrated is narrower and is what the numbers above measure: under a purpose-built harness that reproduces the write-then-exec shape, the old writer failed at 1.43% and the new one at 0 across two 3200-exec runs in the same session.
- The two binaries are provably different from source alone: the marker string `stub-writer` occurs **0** times in `canonical.rs` at `78f19f86` and **1** time at `4c2d7ec9`. I verified that count from the blobs. So "old versus new" is not an artefact of one build being reused.
- The author records **"0 of 6400 is not a universal zero."** That sentence is in the delivered doc and I am restating it here rather than improving on it.
- No kernel bug is claimed, and I am not reopening a kernel root cause.

One methodological cross-check worth recording: the repair doc discloses that its first old-arm attempt seeded `tree-old`'s target directory with `cp -a` of `tree-new`'s, and cargo then satisfied the fingerprint against the copied absolute source path, never rebuilt, and both binaries hashed `c8c6e32a...`. That is the identical trap that produced a false finding in my own round 7, which is why I used a private target directory here. The author caught it by hashing the binaries. Their disclosure is what let me verify the marker bound instead of taking the run counts on faith.

## CI at `4c2d7ec9`

One read of the check runs, no polling, no dispatch, no rerun:

```
linux-fuse     success
macos-latest   in_progress
ubuntu-latest  in_progress
```

**Pending, not green.** Recorded as pending. I did not wait on it and I make no claim about its eventual result.

## Closing references: the one BLOCK

The brief required that the body and all commit closing references be neutral with respect to 17, 98, 115 and 124, so that merging causes no auto-closure. **That requirement is not met.**

Commit side, all 26 commits in PR 92, scanned for a closing keyword adjacent to a reference:

```
commits scanned: 26; closing-keyword+reference pairs in commit messages: 0
```

Commit side is clean. Every subject and body uses `Refs #N`.

Body side, same adjacency test across the whole PR body:

```
closing-keyword+reference pairs in the body: 1
  line 243: Wrapping `swap` would not fix #124: that is the operation, not an interleaving.
```

GitHub's own API is authoritative here and agrees:

```
closingIssuesReferences: [{"number":124, ...}]
issue 124: OPEN, "Backend swap can retain base provenance from the replaced tree"
```

GitHub's closing-keyword parser matches `fix #124` and has no notion of the negation in "would not fix". So the sentence that exists precisely to explain why #124 is **not** resolved by this PR is the sentence that will close it.

This also directly contradicts the PR's own first line, `Refs #17, Refs #98, Refs #115, Refs #124.`, and the status line asserting the PR resolves none of them.

**Remedy, one line, mechanical.** Reword body line 243 so no closing keyword sits adjacent to the reference. For example "Wrapping `swap` would not address issue 124: ...", or "Wrapping `swap` leaves issue 124 as it is: ...". Nothing else in the body needs to change, and no source change is implied. After the reword, re-read `closingIssuesReferences` and confirm it is empty.

I am calling this a BLOCK rather than a nit because the brief made no-auto-closure an explicit acceptance condition and the condition fails mechanically, and because the failure is silent: it would land at merge time and close an issue whose own body says the defect is still live.

## Merge check, source compatibility only

```
git merge-tree --write-tree 4c2d7ec9  c07aabce   -> clean
merge tree 0e937de96ad75863b61d3906dae7f25c39fec295
main's own tree 03ba1e10aa7c9890fbd7b6666e4d492f3e9e01b9
```

`main` is `c07aabce311df4202736a50a28bcccd0377ca511`.

Three checks that the result is real and not a no-op:

- the merge tree differs from main's own tree, so something was actually merged;
- the merge tree carries `crates/cowfs-treehouse/tests/canonical.rs` as blob `4b4adf6585fb511c98c04a7c7a2c397a4b4529b4`, which is the reviewed head's version of that file;
- `git diff --stat main reviewed-head` is 67 files, so the two points have genuinely diverged and a clean merge-tree is informative rather than trivial.

**Source compatibility only.** I did not build or test the combined result and I make no claim that the merge is green, only that it does not conflict at the source level.

### A void measurement of my own, kept on record

My first merge-tree attempt in this round was **wrong and I am retracting it**. I ran
`git merge-tree --write-tree refs/tmp/main-check FETCH_HEAD` instead of naming the reviewed head
explicitly. An earlier `git fetch origin main` in the same command had already overwritten
`FETCH_HEAD` with `c07aabce`, so the comparison was **main against main**: trivially clean, and the
tree it printed, `03ba1e10…`, is simply main's own tree. Had I not checked whether the merge tree
equalled main's tree, I would have reported a clean merge that I had never actually computed.

The corrected run above uses explicit SHAs throughout. The same class of mistake is why I read every
other item in this round from a named commit rather than from `FETCH_HEAD`, and why the gate run used
a `git archive` extract taken while `FETCH_HEAD` still pointed at the reviewed head.

## Pre-existing gaps, unchanged, not re-litigated

These remain OPEN and I am not blocking this delivery with them, per the brief:

- **#124**, backend `swap` can retain base provenance from the replaced tree. Identified by reading, not forced at runtime.
- **#98**, Core warm-base publication. Untestable by design: `base_refresh` refuses directory ingest on Core.
- **#17**, Path warm-base acceptance.

Also still separate and not owned by this PR:

- The `crates/cowfs-meta/src/tx.rs:313` clippy discrepancy. Red `collapsible_match` on clippy 1.95, green on 1.99, byte-identical file, present in zero files of any delta I have reviewed. Owned by the core-metadata lane.
- CI pending on two of three jobs.

## Scope statement on the ETXTBSY claim

Unchanged from the delivered doc and restated so it is not over-read: `0` refusals across 6400 execs of the new writer is a measured result under one harness on one host. It is not a proof of absence. The original CI event has never been reproduced. Both facts belong in the merge decision.

## Recommendation

**Do not merge as-is.** One blocker, one line of body text.

1. Reword PR body line 243 so no closing keyword is adjacent to `#124`.
2. Re-read `closingIssuesReferences` and confirm it comes back empty.
3. Re-read the CI check runs once they finish and confirm all three are green.

None of those three steps needs a code change, a rebuild, or a re-run of any measurement. Once they are done the delivery is, on the evidence I gathered, a PASS: the fixture repair is correct and directly tested, the three corrected rows are receipted, production code is untouched so my issue-115 atomicity PASS carries, the Mac counts match the author's exactly, and `main` merges cleanly at the source level.

I have not merged anything and I am not merging anything.

## Evidence and method

- Runtime only under `bench/out/pr92-final-delivery-review/**`, gitignored, preserved.
  `evidence-gates.txt` is the gate transcript, `g-*.log` are the per-gate cargo logs, `new/` is the `git archive` extract of the reviewed head, `gates.sh` is the driver.
- Head read from `FETCH_HEAD` after `git fetch` of the exact SHA, then `git archive`.
  **No checkout, no branch change, no reset, no stash, no merge.** The lease `HEAD` is still `c3bafb7b86358e45aa744f53082ff32e6fe26008` and has not moved all eight rounds.
- One 600 s bounded foreground `flock` on the Mac heavy lane, exit 75 if busy. Not contended.
- Disk: 357 GiB free before, 356 GiB after. Floor of 20 GiB respected.
- Cleanup: before removing my build cache I checked the exact target path, that it carried `CACHEDIR.TAG`, and that zero processes of mine referenced it. All three guards passed before `rm -rf`. Owned path is now 6.6 MB. An earlier attempt at that guard **refused** because I passed a relative path to an absolute-path check; the guard failed closed, which is the behaviour I want, and I re-ran it with absolute paths.
- Zero processes, mounts or daemons left behind. No signal, mount walk or cleanup of any borrowed fixture. All 32 leases and every shared Mac resource left untouched.
- All 7 prior reports preserved unmodified in the lease, hashes re-verified:
  `513d9c3f`, `2a20e526`, `ce3b2e11`, `2aeeca9f`, `043f1171`, `d740a0e7`, `b9107d17`.
- Canonical docs read at the reviewed head, primary checkout hash equal to the blob hash in both cases:
  `docs/verification/evidence/etxtbsy17-repair.md` sha256 `495dd06ed130c1c66ee51a6ba63331a9184ed97f3964a8e5dfb212a73c5358ff`
  `docs/verification/evidence/etxtbsy17-spike.md` sha256 `72920d10dab70862212c1bd473ed322f0aa41aa4a359ac404e82f3b40bebf285`
- No installs, no sudo, no sysctl, no reboot, no workflow config changes, no polling. `shellcheck` was not needed and was not invoked.
- Owners of other work (final review, scan repair, recovery, fsx, CI, parked tasks) untouched. No new issues, tasks, audits or stress runs created.

## One retracted finding kept on record

`a_reused_name_does_not_inherit_a_lost_commit` does not exist. An earlier draft of my round-7 notes treated a check by that name as coverage. It was a vacuous pass and it is recorded here so it is not reintroduced.
The real coverage for that behaviour is `a_recreated_snapshot_is_never_a_base_even_when_a_record_outlived_it`.
Separately, the round-7 statement "the head fails 4 of its own tests" was caused by a shared `CARGO_TARGET_DIR` across `cp -a` trees and was retracted. The void evidence is preserved at `bench/out/provenance115-critic/evidence-final-inprocess.txt`.
