# PR #133 final independent review: the round-2 health fixture lifetime repair

Reviewer lane: final critic, fresh independent context.
Verdict: SCOPED PASS, no blocking defect. Two operational notes for the coordinator.
No merge, no push, no branch or lease action was taken.

## Identity under review

| Item | Value |
| --- | --- |
| PR | #133, open, author zeeshanhaque21 |
| Head | `4dbbd992b912589e13e3841858de09079f1d60f6` |
| Base | `0d7da418dae2f304ae901174425d32902a20cbd3` |
| Merge base | `0d7da418dae2f304ae901174425d32902a20cbd3`, identical to base |
| Commits | 3: `8efc69b`, `febe4ed`, `4dbbd99` |
| Files changed | 3: `crates/cowfs-meta/tests/health.rs`, and the two evidence docs |
| `crates/cowfs-meta/src/db.rs` | blob identical at base and head, sha256 `de79c2713fbb01489c7895d51b0c5c2b52229ea8da86db552d8d41532ef38da1` |
| `meta-health40-fixture-lifetime.md` | sha256 `33d0eab389038a06eb0a2c0c7db81036dfd6dd58b7f57ad424122d7b22f98791`, matches assignment |
| `meta-health40-ci-rollback.md` | sha256 `ea010c926382d3df5418e4f2ae77a1d161ba9201c29624596b8f389c4f83219f`, matches assignment |
| `health.rs` head / base | `22a986413cdeeae992fc62563aea306cfea4878464cf01da55eb60a34dfc5d22` / `e083506ca98a9b88a1ce6bb5c80e9910b2f63a3e4f55eeb5f4155a0c1b5a11f5` |

## Lease verification before any work

The assigned idle, held review lease is `/Users/zeeshanhaque/Projects/cowfs/.treehouse-build-train/.treehouse/cowfs-7c1bf8/6/cowfs`.
Its branch is `review/gc-root-mark-retention-82`, which matches the assignment, and its tracked state is clean.
Only untracked prior reports from earlier lanes are present.
Its HEAD is `b4b55abfe9ab2d8d6f5fc42403bb1eb8b1c02d41`, which is not the PR head, so nothing was checked out.
The PR head object was read straight from the object store with `git archive` and `git cat-file`.
No checkout, branch, source edit, reset, stash, rebase, commit, push, merge, lease return or new lease was performed.

## The defect, as I read it in source

`db.rs:1130` gives `Meta` an `Arc<Handle>`, `db.rs:1540` gives `Snapshot` a second `Arc<Handle>` to the same handle, and the write to disk happens in `impl Drop for Handle` at `db.rs:1089` through `finish_on_drop` at `db.rs:862`, which commits unless already closed.
`drop(m)` therefore cannot close anything while a `Snapshot` binding is still alive.

In the base round-2 block at `health.rs:635-649`, `s` stayed alive past the restore.
The restore copied the scratch file back over `fx.path` while `s` still owned the handle.
`s` dropped at the end of the block, the handle committed, and that commit landed on top of the restored bytes.
The inline comment "The store is closed now" was false in both of its parts.

The fix drops `Snapshot` then `Meta` inside a scope, restores once no handle can commit, and pins the result with an assertion.

## Independent reproduction: the old code fails, the new code passes

I built the variants myself from the archived objects rather than trusting the committed numbers.

| Variant | Source sha256 | What it is |
| --- | --- | --- |
| `base` | `e083506ca98a9b88a1ce6bb5c80e9910b2f63a3e4f55eeb5f4155a0c1b5a11f5` | PR base, pre-fix |
| `oldguard` | `06ba2f162a0cffdccae9af40f535c5c2fade2a393518ed14363e217bb0c3c26e` | base drop order and base copy/copy restore kept verbatim, plus the new guard only |
| `smoke` | `a84e81f202475ebe41696edc3ac18842a5f11417eec2c10e9d86ce3f0fb880be` | PR head plus two byte-preserving `eprintln` probes |
| `pristine` | `22a986413cdeeae992fc62563aea306cfea4878464cf01da55eb60a34dfc5d22` | PR head, byte identical |

Through the published test case `repeated_rollbacks_keep_counting_and_keep_moving_the_floors`, one sample each:

| Run | Variant | Outcome | Exit | Log sha256 |
| --- | --- | --- | --- | --- |
| 04 | `oldguard` | FAILED at the new guard | 101 | `9c392c4cf8454351742a89a14466da6f33c07ea742183e838bfafc2fd7314314` |
| 11 | `pristine` | 1 passed, 0 failed, 6 filtered out, 14.45s | 0 | `e4006d15d9287054be89e6b5b8334e0e293475efba8388ba47c37468acf71534` |

The `oldguard` failure is the guard itself on real bytes, not an incidental panic:

```text
panicked at crates/cowfs-meta/tests/health.rs:653:13:
assertion `left == right` failed: round 2: the fixture must still be the saved batch, so the
newest commit owns pages of its own and a rollback is reachable
  left: [15561287921018235510, 9410392966900449370]
 right: [9966415775836001390, 8249877812978874044]
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 6 filtered out
```

`left` is the file as it stands after the late drop and `right` is the captured batch, so this is a byte-level proof on the private `tempfile` fixture.
There is no `Arc`-only argument, no mocked file and no surrogate model anywhere in the chain.
The three compiled test binaries have three different sizes, 8484856, 8486136 and 8485960 bytes, which independently proves the three variants were genuinely distinct compilations.

## Smoke first, on the real fixture

Run 03, `smoke`, 1 sample, exit 0, log sha256 `9d380c9a651908395921c84686f456457f3be374fc2294213a735b5711a79799`.

```text
SMOKE round=2 captured_sha=[15650537570788243074, 7083227283920107917] captured_pages=633 open_after_both_drops=true
SMOKE round=2 restore_equals_capture=true pages_after_restore=633
```

This is the required closed-handle evidence in the fixed order: a real `Meta::open` on `fx.path` after both handles are gone **succeeds**, which is the exact inverse of the old ordering where that open failed every time.
The capture is 633 pages, the restore reproduces the capture exactly, and the restored file is 633 pages.
The probe reads only, and it runs before the restore, so it cannot perturb the bytes the damage search consumes.

My captured hash is `d931e10022bcf682624cadafa969b98d`, which does not equal any of the three hashes in the committed document.
That is consistent with the document's own statement that the fixture content is not byte-stable between runs, and the page count 633 matches its runs.
I am reporting the difference rather than hiding it: I confirmed the page count and the equality relations, not the absolute hashes.

## The guard is load-bearing, and it is in the right place

- It sits at `health.rs:656-663`, after the round-2 block at 636-655 and before `damage_newest_commit` at 664.
  Inside the block it would pass on the old code too, because the clobbering drop has not happened yet.
  The `oldguard` run proves the chosen placement actually catches the late drop.
- `damage` reads its base image from `fx.path` at `health.rs:204`, and `probe_page` derives every candidate from that image.
  The guard therefore protects the exact bytes that get corrupted and then handed to `Meta::open_recover`, not a pristine unrelated file.
- The search bounds are byte identical to base: `for pages in [1usize, 2]`, `pages_total <= pages`, `for page in 1..=pages_total - pages`.
  `TAIL` is still 400, `PAGE` is still 4096, `opts()` is unchanged, and the `Fixture` struct is unchanged.

One arithmetic cross-check that supports the stated mechanism: the search enumerates `(N-1) + (N-2)`, and CI reported 1113 candidates.
`2N - 3 = 1113` gives N = 558, exactly the 558-page fixture named in the failure.
So the failing CI fixture was a round-2 size class, and round 1 stays at 80 pages on this host.

## Full scoped gate at the fix head, compiled from my archive

| Check | Result | Exit | Log sha256 |
| --- | --- | --- | --- |
| `cargo test --test health` round-2 test, 1 sample | 1 passed, 14.45s | 0 | `e4006d15d9287054be89e6b5b8334e0e293475efba8388ba47c37468acf71534` |
| `cargo test --test health`, whole suite | 7 passed, 0 failed, 20.81s | 0 | `762f66c594b779eb8ee7470ffda261db288ecde667ec28b2f297d6f61455c3d2` |
| `cargo test --test recovery40` | 4 passed, 0 failed, 4.43s | 0 | `01f907b8068e20a0d85efa26a851081031476026aac25101d5a92a94cc7cb053` |
| `cargo fmt -p cowfs-meta -- --check` | clean, zero output bytes | 0 | `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855` |
| `cargo clippy --all-targets -- -D warnings` | zero warning or error lines | 0 | `21d05b4593b5ff882b8cc48a5035f8264d1dd5f9afafa13b6cb275977ab4ba14` |

The fmt log hash is the SHA-256 of the empty string, which is independent confirmation that `--check` printed nothing.

Test count is 7 in both base and head, and all seven pass at head.
Nothing was skipped, ignored, deleted, weakened or made conditional.
A pattern sweep of the whole file shows a delta of zero for `sleep`, `#[ignore]`, `should_panic`, `return Ok`, `unwrap_or`, `catch_unwind`, `std::process::exit`, and any panic reinterpreted as `RolledBack`.
`FORMAT_VERSION` does not appear in the file at all.

## The load-bearing assertions all survive

Present in both base and head round-2: `rec.rolled_back` in both rounds, `rec.recoveries == round`, `f.0 > handed_out`, `floors[1].0 > floors[0].0`, `floors[1].1 > floors[0].1`, `m.check().unwrap()`, `assert_pristine_intact`, `m.close().unwrap()`, `damage_newest_commit(&fx)` and `Meta::open_recover(&fx.path, opts())`.
So two genuine `RolledBack` outcomes are still required, the durable count is still monotonic across both rollbacks, both floors must still advance past what was handed out and past round 1, the store is still checked per round, and the pristine copy is still proven byte identical.
The recovery path exercised is the real `Meta::open_recover` rollback on a private `tempfile` fixture, not `check()` alone.

## PR #116 is untouched

`db.rs` is the same blob at base and head, so `FORMAT_VERSION`, the legacy refusal, the no-migration stance, and the stored-inode-block reservation invariant are all unchanged by construction.
The four #40 controls in `recovery40` pass individually on my run, so the 64 to 4 and 4 to 64 block behaviour and the unknown-reservation refusal are still exercised and green.
No production repair, migration or format adaptation was introduced.

## The disclosed cost is honest, and I measured it independently

The document claims roughly three times slower and gives 5.93 to 6.13s before and 17.63 to 20.57s after.
I ran base and head on this host, in this session, with default test parallelism to match that methodology.

| Run | Variant | Suite time | Exit | Log sha256 |
| --- | --- | --- | --- | --- |
| 21 | `base` | 7 passed, 6.55s | 0 | `17494217cd8a90c4b4a23ed4ee41e7e6d956161aeb487b9b3c358fad79b62608` |
| 22 | head | 7 passed, 19.21s | 0 | `537ea16efb98a57aedf0c500ff8f34fa65da8f26cda0c3085600cae9e25927ea` |

That is 19.21 / 6.55 = 2.93x, inside the claimed band, and my base figure 6.55s sits inside the author's 5.93 to 6.13s.
Serialized, base is 8.52s against head 20.81s, and the round-2 test alone goes from 2.78s to 14.45s.
The cost is the fixture search exploring the real batch layout instead of landing on the first candidate, and it sits inside the existing `damage` helper.
This is a test-runtime cost from restoring a correct fixture. It is not a performance-criterion change, and `docs/design.md` success criterion 2 is unaffected because no `cargo build` or `git status` timing moved.

## What the documents do and do not claim

The committed documents are disciplined and I found no overclaim.

- The raw CI log hash `7873236f66e0bcf57227b84586d5c5fdd3a09bf36262eb3801b4952e41c3bd80` is recorded in both.
- The fixture bug is stated as proven at byte level, and the CI event is stated as still UNREPRODUCED, in four separate places including "That event is not resolved by this change and no claim is made that it is" and "a green local suite is not a reproduction of a red CI job".
- The cost is stated rather than buried.
- The three commits add the former report so the lifetime document's link to it resolves on GitHub instead of 404.
  I verified both documents are byte identical between the committed blobs and the copies on disk, and both hashes match the assignment exactly.

## CI truth at the exact head

Read-only, one snapshot, no poll, no rerun, no dispatch, no runner configuration touched.

| Check | Status | Conclusion | head |
| --- | --- | --- | --- |
| `check (macos-latest)` | completed | success | `4dbbd992b912` |
| `check (ubuntu-latest)` | completed | success | `4dbbd992b912` |
| `linux-fuse` | completed | success | `4dbbd992b912` |

Total count 3, on the exact head.
All three ran 2026-10-05T17:50Z to 18:04Z.
Annotations are runner and infrastructure notices only: the `actions/checkout@v4` Node.js 20 deprecation, and a macOS arm64 capacity notice. No code finding.

The PR body says CI "is expected to be pending, not green, at the time of writing, and no CI result is claimed". That was true when written and it is now overtaken by fact: the exact head is 3 of 3 green, including the `macos-latest` lane that failed on #118.
This is a favourable change of state, not a defect in the body's honesty.

## Issue 40 is safe

`closingIssuesReferences.totalCount` is 0 for PR #133.
Issue #40 is open.
The body references `#40` and `#118` as context in a References section and never as a closing keyword, and the title's `(#40)` is not a closing keyword either.
Nothing in the history or body can auto-close #40.

## Integration tree

`git merge-tree --write-tree 0d7da41 4dbbd99` returns tree `7124866d199ccebc2247df004bd4e4493f8e6baf` with no conflict output.
Base is an ancestor of head, so the branch is linear with no divergence and no history rewrite is needed for a merge.
The merged tree contains the modified `health.rs` and both evidence documents.

## Two operational notes for the coordinator, neither a code defect

1. Both evidence documents are currently **untracked** in the primary checkout at `docs/verification/evidence/`.
   `git status` reports them as `??`.
   A real `git merge` of PR #133 into main will abort with an untracked-working-tree-files-would-be-overwritten error, even though the tree merge itself is clean.
   Remove or relocate those two local copies before merging, or the merge will need a trivial manual step.
2. The PR body still reads as though CI is pending. It is now 3 of 3 green at the exact head. Updating that one paragraph would keep the body truthful at merge time, which is a nicety, not a blocker.

## What this review does not establish

- It does not reproduce the original CI event `111880928624`, run `37344908792`, at head `2b4cf66`.
  That remains UNREPRODUCED, exactly as the document says.
  The fix mechanism is consistent with that symptom, and the 1113 to 558 page arithmetic corroborates the round-2 size class, but consistency is not reproduction.
- It does not complete the whole #40 acceptance, and it does not clear PR #131.
- It is a fixture and evidence review on macOS with private `tempfile` fixtures.
  It claims nothing about power-loss behaviour, crash-injection durability, real production recovery, or any Linux runtime, and `docs/design.md` success criterion 3 is untouched by this change.
- I did not carry the author's full three-run matrix. I reproduced the bounded old-fails and new-passes pair once each, plus one independent base-versus-head timing pair, which is what the questions required.

## Method note, reported because it nearly produced a false PASS

My first two runs were wrong and I discarded them.
`shutil.copytree` preserves mtimes, so the archived `health.rs` looked older than an already built artifact, cargo's fingerprint check called it fresh, and the test binary compiled from the primary checkout ran instead of my archive.
Both runs reported a passing test and both were meaningless.
A second defect compounded it: I first placed `--manifest-path` after the `--` separator, so cargo rejected the flag, silently fell back to the working directory, and again compiled the primary checkout.
That one was caught by a hard gate I had added for exactly this purpose, which then failed the run rather than reporting a pass.

The fix was one fresh `CARGO_TARGET_DIR` per variant, an explicit manifest path ahead of the separator, a per-variant `TMPDIR` that actually exists, and a mandatory post-run gate that proves the compile came from this lane's archive and that the expected assertion string is inside the binary.
Every result reported above passed that gate.
Anyone reproducing this work should assume a green test result is unproven until the compiled source paths are shown.

## Recommendation

SCOPED PASS.
The defect is real, the mechanism is correct, the fix is minimal and correct, the new assertion fails on the old ordering and passes on the new one at byte level on the real fixture, all seven tests and the four #40 recovery controls pass, `db.rs` is byte identical so PR #116's invariants are untouched, the cost disclosure is accurate and independently confirmed at 2.93x, both documents are byte identical to the assignment hashes and claim nothing beyond what they ran, CI is 3 of 3 green at the exact head, and #40 cannot be auto-closed.
Clear the two untracked evidence files before merging, then merge is the coordinator's call.

Evidence, archives, per-variant logs and the lane scripts are under `bench/out/meta-health40-fixture-final-critic/`.