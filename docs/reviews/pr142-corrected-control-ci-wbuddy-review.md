# PR142 corrected-control CI audit (wbuddy, independent)

Reviewing 10c4a0f9e5f49189db9d19027723f1e65595fdbd, what would you devs do if I didn't check up on you?

Scope: independent, read-only review of the corrected control in PR142 head `10c4a0f9`, and of the exact CI run over that head.
This review is source and CI-log reads only.
No local cargo, build, test, or archive was run, and no production or test file was changed.
The original `e7d` review (`pr142-core-inode-regression-wbuddy-review.md`) and the coordinator receipts are left immutable.

## Verdict

SOURCE PASS on the corrected control.
The correction is genuinely control-only, the two positive bodies are byte-identical between `399153a` and `10c4a0f9`, and the exact-head CI run proves the corrected control passes on both OS legs that run the target while the two positives are RED.
One accuracy finding on the PR body, stated below.
One factual correction to the task premise: the third CI job (`linux-fuse`) does not run this target at all, so its green conclusion is not evidence about the fixture.

## Pins (all verified this pass)

| What | Value | How verified |
| --- | --- | --- |
| PR142 head | `10c4a0f9e5f49189db9d19027723f1e65595fdbd` | `git ls-remote origin refs/pull/142/head` == local `cat-file -t commit`; head is a commit |
| Head parent | `399153a6e69a3d2a292a9ccb42291221f81b6dfd` | `git rev-parse 10c4a0f9^` |
| Corrected test blob | `583032cfd4164e789785dd89fd3e3bc2dc4a4ea5482cd83e4013e5aa041402a2` | `git show 10c4a0f9:crates/cowfs-core/tests/reserved_inode_identity.rs | shasum -a 256`; matches receipt |
| Prior test blob (`399153a`) | `e5ded873aa9c1b43c9d87a977aaab05a3a504a6a58118d7cf8c07e2ba124ce40` | `git show 399153a6:...` |
| Control-correction receipt | `6cff927be4e418cc6510a88dc3a35124e5a53314ebcc1d125232b37a4ff9b0ff` (94 lines) | worktree `shasum` == head blob `shasum`, both match the task's stated SHA |
| Run | `37511812607`, run_number 565, event `pull_request`, conclusion `failure` | `gh-axi run view`; `gh-axi api .../actions/runs/37511812607` `head_sha: 10c4a0f9...` |

Run-to-head binding is exact: the run's `head_sha` equals PR142's remote head.
So the CI verdict below is over the corrected control, not the old fixture.

## CI result, by job, derived from the full job logs

Run has three jobs.
Two run `cargo test --workspace`; the third does not.

| Job | Name | Conclusion | Runs the target? |
| --- | --- | --- | --- |
| 112434598621 | check (ubuntu-latest) | failure | yes |
| 112434597959 | check (macos-latest) | failure | yes |
| 112434598444 | linux-fuse | success | no |

Binding evidence for the "does not run the target" claim: `linux-fuse` step list is `cargo test -p cowfs-vfs-path --test native ...` and `cargo test -p cowfs-fuse ...`.
It never builds `cowfs-core`, and its log contains zero references to `reserved_inode_identity`.
Its `success` is therefore unrelated to this fixture, and must not be reported as fixture evidence.

### ubuntu (job 112434598621), suite `tests/reserved_inode_identity.rs`

```
running 3 tests
test a_created_number_is_never_the_virtual_alias_shape_or_the_root ... FAILED
test a_virtual_alias_number_is_stale_after_a_reopen ... ok
test a_created_file_keeps_one_durable_identity_across_a_flush_and_reopen ... FAILED
test result: FAILED. 1 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.03s
```

Failure windows:

```
---- a_created_number_is_never_the_virtual_alias_shape_or_the_root stdout ----
panicked at crates/cowfs-core/tests/reserved_inode_identity.rs:146:5:
assertion `left == right` failed: a created file number 0x8000010000000001 carries the virtual alias bit
  left: 9223372036854775808
 right: 0

---- a_created_file_keeps_one_durable_identity_across_a_flush_and_reopen stdout ----
panicked at crates/cowfs-core/tests/reserved_inode_identity.rs:102:5:
assertion `left == right` failed: created file got a session-local virtual number:
created(in-session)=0x8000010000000001 created_meta_before_flush=None
created_meta_after_flush=Some(2) reopen=(ino=0x10000000002, meta=Some(2))
  left: 9223372036854775808
 right: 0
```

### macos (job 112434597959), same target

```
running 3 tests
test a_created_number_is_never_the_virtual_alias_shape_or_the_root ... FAILED
test a_created_file_keeps_one_durable_identity_across_a_flush_and_reopen ... FAILED
test a_virtual_alias_number_is_stale_after_a_reopen ... ok
test result: FAILED. 1 passed; 2 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.15s
```

Same two failure sites (`:146:5`, `:102:5`) and the same observed values.
Only the test-completion order and the interleave differ between legs.

Both failure sites are `assert_eq!(ino & VIRT, 0)`.
`VIRT = 1 << 63 = 9223372036854775808` (confirmed in `crates/cowfs-core/src/ino.rs:10`), and the observed inode `0x8000010000000001` has that bit set.
So both REDs are the genuine contract assertion, not a compile break and not infrastructure.

### Misattribution hazard, confirmed present

The task warning is real and I reproduced it.
In the macos log the line `right: 0` trails the `error: test failed, to rerun pass ...` line rather than the assertion line.
A naive "grep the line after `error:`" or "last `Running` line" parser would bind the wrong OS/step to the wrong assertion.
I derived every window from the actual suite header (`Running tests/reserved_inode_identity.rs`) and the suite's own `test result:` line, never from adjacent stdout.

Also confirmed: earlier suites in the same job print `test result: ok. 3 passed` / `running 3 tests` for other targets.
Taking the first `3 passed` in the stream attributes another suite's result to this one.
I gated on the `reserved_inode_identity.rs` header to avoid exactly that.

## Diff is control-only

`git diff 399153a6 10c4a0f9 -- crates/cowfs-core/tests/reserved_inode_identity.rs`: +22 / -9, entirely inside the third test.
The third test is renamed `a_number_from_a_closed_session_is_stale_after_a_reopen` to `a_virtual_alias_number_is_stale_after_a_reopen`, and its saved number changes from `a.ino` to `a.ino | VIRT`, with a new `assert_ne!(virt_alias & VIRT, 0, ...)`.

The two positive function bodies, hashed as extraction between `fn` and its closing brace, are identical across the two commits:

| Positive test | `399153a` body digest | `10c4a0f9` body digest | identical |
| --- | --- | --- | --- |
| `a_created_file_keeps_one_durable_identity_across_a_flush_and_reopen` | `c558e35ad5170d79` | `c558e35ad5170d79` | yes |
| `a_created_number_is_never_the_virtual_alias_shape_or_the_root` | `2643e7f86fed9a38` | `2643e7f86fed9a38` | yes |

`10c4a0f9 --stat` touches only the test file and the correction receipt; no production file.

## Semantic counterexample, as asked

The corrected control is honest for the intended reason.

- `classify` in `crates/cowfs-core/src/ino.rs:33` returns `Id::Root` for `1`, `Id::Virt` iff `ino & VIRT != 0`, else `Id::Meta`.
- A real meta-derived number is `pack(snap, m)` with `snap < MAX_SNAP` and `m` in the low 40 bits, so its bit 63 is clear by construction.
- Therefore `a.ino | VIRT` on a real created number yields a number that no admitted path hands out.
- In `load_node`, the `Id::Virt` arm resolves via the alias table and returns `Error::Stale` when there is no alias entry, so a fresh session returns `Stale` for it deterministically.

Counterexample shape, concretely: had the control kept asserting `Stale` on the un-tagged created number `a.ino`, then on the NEW design - where the positive case requires `created == reopened` and a fresh `getattr` on the created number to succeed - a real (non-virtual) created id would make that control pass its own getattr while being exactly the admitted id the positive case requires to succeed.
The old control therefore tested the wrong object (a created number), and passed on OLD only because OLD minted a session-local virtual number.
Tagging with `VIRT` moves the control onto the alias shape, which no real reservation-backed number can occupy, so the two tests no longer contradict.

Note I only endorse the semantic claim above.
The tagged number being `a.ino | VIRT` is a legacy-shape probe, not a proof about migrating an actual OLD store, and the receipt is careful to say so.

## Findings

### F1 - PR body "checks: 1 passed, 2 failed, 3 total" is job-level and collides with the 1-passed-2-failed suite reading (low, accuracy)

The PR body line reports the run's job tally, which is `1 passed, 2 failed` (linux-fuse success; ubuntu and macos failure), while the same body's runtime section says the corrected head is UNEXECUTED locally.
A reader can easily take `1 passed, 2 failed` as the correct control passing and the two positives failing (the test-level reading), because that is literally also true in the logs.
Both readings are simultaneously true but mean different things, and the body does not separate them.
Suggested fix: state the two plainly, e.g. "Test level: 1 passed; 2 failed (corrected control ok, both positives RED). Job level: 1 success; 2 failure (linux-fuse green but does not run this target; ubuntu and macos red)."

### F2 - body asserts corrected head UNEXECUTED while CI executed it (low, accuracy)

The body says the corrected head is UNEXECUTED because a local capacity block forbids cargo.
That is true for the local reviewer, but this audit's run `37511812607` demonstrably executed the corrected control on ubuntu and macos.
The body should distinguish "UNEXECUTED locally" from "executed in CI at head `10c4a0f9`", or it under-credits the evidence it actually has.

### F3 - CI green on `linux-fuse` is not fixture evidence (informational)

Already covered above; recorded so no one counts it as a third passing leg for this target.

## What this review does not claim

- No acceptance claim for #42 request 4.
  The positive contract is still RED on both legs; the NEW producer/consumer implementation is still absent.
- No claim about any other PR, issue, or file.
- No proof about migrating a real OLD store.
- No runtime claim from this reviewer.
  Every verdict above is a source read or a CI-log read; nothing was compiled or run by me.

## Reproduce (read-only)

```sh
git ls-remote origin refs/pull/142/head
git show 10c4a0f9:crates/cowfs-core/tests/reserved_inode_identity.rs | shasum -a 256
git diff 399153a6 10c4a0f9 -- crates/cowfs-core/tests/reserved_inode_identity.rs
gh-axi run view 37511812607 --job 112434598621 --log
gh-axi run view 37511812607 --job 112434597959 --log
gh-axi run view 37511812607 --job 112434598444 --log
```

## Provenance

- Head under review: `10c4a0f9e5f49189db9d19027723f1e65595fdbd`, branch `fix/core-reserved-inode-consumer-42`.
- Run: `37511812607` (run_number 565, event pull_request, conclusion failure), head_sha `10c4a0f9`.
- Test file at head: `crates/cowfs-core/tests/reserved_inode_identity.rs`, SHA-256 `583032cf...`.
- Control-correction receipt: `docs/verification/evidence/meta42-core-reserved-inode-control-correction.md`, SHA-256 `6cff927b...`.
- Prior receipts and the `e7d` review are immutable and unchanged by this pass.
- Reviewer execution: source and CI-log reads only (UNEXECUTED locally).
