# meta-health40-ci-rollback: the macOS `check` failure on PR #131 is UNREPRODUCED, with the layout evidence

Lane: `.treehouse-ready-wave/.treehouse/cowfs-7c1bf8/5/cowfs`, branch `followup/metadata-health-40` at `fab4c6490c4274262976431384d7468b2e6a2de5`.
Host: Apple M3 Max, macOS 26 aarch64, `rustc 1.99.0` / `cargo 1.99.0`, 16 cores.
Main checkout used for this document: `/Users/zeeshanhaque/Projects/cowfs` at `48c06f9cf9d323bb626d902cd0e7b2247beaf055`.
No patch, no branch, no commit, no push, no PR, and no lease operation was performed. The lease is left clean at `fab4c64`.

## Verdict

UNREPRODUCED.

The failure did not reproduce on this host in six bounded runs, and current main is green on the same input.
This is explicitly not a pass completion and not a claim that the code is correct.
No patch is proposed, because there is no failing input to prove a fix against, and inventing one without proof is exactly what the lane rules forbid.

## The failing CI identity, as read from the GitHub API

One read-only call per fact, no polling and no rerun.

| Fact | Value |
| --- | --- |
| Job id | `111880928624` |
| Job name | `check (macos-latest)` |
| Job conclusion | `failure` |
| Job head sha | `2b4cf66d5284ec14f56fd89944fd6b524303c082` |
| Run id | `37344908792`, attempt 1, event `pull_request`, branch `test/pathvfs-stamp-precondition-118` |
| Started / completed | `2026-10-05T16:58:31Z` / `2026-10-05T17:05:22Z` |

Raw log bytes were retrieved and preserved, not paraphrased.
`gh-axi api ... --full` refused with `the response contains terminal escape sequences`, which is the documented limitation, so the justified read-only fallback was used.

- File: `bench/out/meta-health40-ci/raw/job-111880928624.log`
- Bytes: `234589`, matching the size in the task brief
- sha256: `7873236f66e0bcf57227b84586d5c5fdd3a09bf36262eb3801b4952e41c3bd80`, matching the brief exactly and computed from the bytes on disk

## Source carry: PR #131 did not touch the failing crate

`git diff --stat` over `crates/cowfs-meta` is empty for all three pairings, so the failing test source is identical at current main, at the PR #131 head, and at the #40 lane head.

| Pairing | Files changed in `crates/cowfs-meta` |
| --- | --- |
| `48c06f9` .. `2b4cf66` | 0 |
| `fab4c64` .. `2b4cf66` | 0 |
| `48c06f9` .. `fab4c64` | 0 |

Receipts, identical in every tree:

- `crates/cowfs-meta/tests/health.rs` sha256 `e083506ca98a9b88a1ce6bb5c80e9910b2f63e3e4f55eeb5f4155a0c1b5a11f5`, git blob `be0a908c2a43fa4ed89da10c7462b9adeececcb1`
- `crates/cowfs-meta/src/db.rs` sha256 `de79c2713fbb01489c7895d51b0c5c2b52229ea8da86db552d8d41532ef38da1`, git blob `b08ed7f7e204a60625124b13415f471ec82800b8`

The `db.rs` sha256 is the same shipping receipt `de79c2713fbb0148` recorded in `docs/reviews/pr116-final-delivery.md`, so the code under test is the reviewed, merged #40 code and nothing since.
`redb` is pinned at `4.3.0` with checksum `fb338a6c67830a61bed824b78c2bb034ab1ff401a20616bd7957524f4fdc2f22`, the same version CI compiled.

So the trigger is not a source difference introduced by PR #131.
The remaining candidate is environment and layout, which is what the rest of this document measures.

## What actually failed, read out of the raw log

The redb `unreachable` panics are not the failure.
`crates/cowfs-meta/src/error.rs:137` defines `guard()`, whose own doc comment says redb 4.3 can panic on some damaged pages instead of returning an error, and it turns that panic into `Error::Corrupt`.
There is a unit test for exactly that, `guard_turns_a_panic_into_corrupt`.
The same panics appear in this lane's passing runs, so their presence is designed-for behaviour, and treating a caught panic as a passing outcome would be the wrong fix.

The real failure is the test's own assertion, at `crates/cowfs-meta/tests/health.rs:230`:

```text
panicked at crates/cowfs-meta/tests/health.rs:230:5:
no 558-page fixture yielded RolledBack; tried [(1, 1, Unrepairable), (2, 1, Unrepairable), ... (557, 1, Opened), (1, 2, Unrepairable), ... (556, 2, Opened)]
```

The binary reported `test result: FAILED. 6 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 11.98s` and the job exited `101`.
All `1113` candidates enumerated, and every single one came back either `Opened` or `Unrepairable`.
Not one returned `RolledBack`.

The `1113` is a check on the reading rather than a count taken on trust.
`docs/reviews/metadata-health40-final.md:120` already corrected this search's arithmetic, noting that it enumerates `(N-1) + (N-2)` and that a 78 page fixture therefore gives `153` candidates, not the `155` first claimed.
Applying the same derivation to CI's 558 page fixture gives `557 + 556 = 1113`, which matches the enumerated candidate list exactly, so the log was read completely and no candidate was skipped.

`damage()` searches windows of 1 and then 2 consecutive 4096-byte pages over the whole file and panics if no candidate reaches the wanted verdict.
So the assertion that failed is that a reachable rollback exists within a 1 or 2 page damage window on that particular fixture.

## Which round failed, and why that matters

`repeated_rollbacks_keep_counting_and_keep_moving_the_floors` calls `damage_newest_commit` twice, once per round, and round 2 rebuilds the fixture on top of the round 1 recovered file.

Measured page counts on this host, with one temporary one-line instrument in `damage()` that printed the total and the hit, since a successful search prints nothing:

| Fixture | Pages measured on this host |
| --- | --- |
| `a_rollback_never_hands_out...` (`build`, 24 files, 2 snapshots) | 80 |
| `a_failed_recovery...` (`build`, 12 files, 1 snapshot) | 80 |
| `repeated_rollbacks` round 1 (`build`, 20 files, 1 snapshot) | 80 |
| `repeated_rollbacks` round 2 | 633, 617, 633, 633, 633 across five runs |

Round 1 is stable at 80 pages.
Round 2 is not stable, varying by 16 pages on identical source, an identical binary and an identical host.
The CI failure's fixture was 558 pages, which is the size class of a round 2 fixture, not a round 1 one, so the CI failure occurred in round 2.

The instrument also shows where the rollback is found, and that is the sharpest difference of all.

| Host | Verdict for damage at page 1 | Round 2 outcome |
| --- | --- | --- |
| this host, 5 runs | `RolledBack` | reached at page 1, first candidate |
| CI `macos-latest` | `Unrepairable` | no candidate in 1113 reached `RolledBack` |

The same first candidate that succeeds immediately here is the first candidate that fails there.

So the test's reachability of `RolledBack` depends on redb's page sharing for the specific bytes in the file, and those bytes differ between this host and the CI runner, and are not even stable between runs of the same fixture on one host.
Nothing in `cowfs-meta` chooses that sharing: it is redb's allocator, and `cowfs-meta` cannot make a page exclusive to the newest commit.
That is the reason the assertion is layout-sensitive rather than a statement about the product.

## The bounded reproduction attempts, all of which passed

Every run used a private `tempfile` fixture, an isolated `CARGO_TARGET_DIR` and `TMPDIR` inside this lease, and the shared `mac-heavy.lock` under one 600 second foreground hold per batch.
Source receipts were printed before each command and no step was run twice for a receipt.

| Run | Selection | Result | Exit |
| --- | --- | --- | --- |
| 1 | exact failing test alone, `--exact` | 1 passed, 0 failed, 2.72s | 0 |
| 2 | whole `health` suite, default threads | 7 passed, 0 failed, 6.13s | 0 |
| 3 | whole suite, repeat 1, instrumented | 7 passed, 0 failed, 6.09s | 0 |
| 4 | whole suite, repeat 2, instrumented | 7 passed, 0 failed, 6.12s | 0 |
| 5 | whole suite, repeat 3, instrumented | 7 passed, 0 failed, 5.93s | 0 |
| 6 | whole suite, `--test-threads=1` | 7 passed, 0 failed, 8.15s | 0 |
| 7 | whole suite, `--test-threads=3` | 7 passed, 0 failed, 7.74s | 0 |

Thread count was probed because the CI `macos-latest` runner has 3 to 4 cores, so 3 to 4 of the 7 tests run at once there, while this host defaults to 16.
Runs 6 and 7 cover 1 and 3, and both pass, so parallelism is not the trigger.

The temporary instrument was reverted immediately afterwards and the tree returned to the reviewed blob, verified by `git hash-object` giving `be0a908c2a43fa4ed89da10c7462b9adeececcb1` and by `git status --porcelain` being empty.
No instrument is being proposed for landing.

## The current-main do-nothing control

The control is a pristine `git archive` of current main, not the lease working tree, so it is the committed bytes and nothing else.

- Tree: `bench/out/meta-health40-ci/main-48c06f9`
- `git ls-tree -r` count `636`, files on disk `636`, so no file is missing and none is extra
- `health.rs` and `db.rs` sha256 identical to the values above, which independently confirms the source-carry table

| Check | Selection | Result | Exit |
| --- | --- | --- | --- |
| `cargo test -p cowfs-meta --locked --test health -- --exact repeated_rollbacks_keep_counting_and_keep_moving_the_floors` | the exact failing test | 1 passed, 0 failed, 3.06s | 0 |
| `cargo test -p cowfs-meta --locked --test health` | whole suite | 7 passed, 0 failed, 5.96s | 0 |

Both are the do-nothing baseline for this report: the same test, the same pinned redb, the same host toolchain, on current main's committed bytes, and it passes.

## The rest of the scoped gate at the #40 lane head

| Check | Selection | Result | Exit |
| --- | --- | --- | --- |
| `cargo test -p cowfs-meta --locked --test health` | whole suite, uninstrumented, final | 7 passed, 0 failed, 6.02s | 0 |
| `cargo test -p cowfs-meta --locked --test recovery40` | the four #40 controls | 4 passed, 0 failed, 4.26s | 0 |
| `cargo fmt -p cowfs-meta -- --check` | crate | clean | 0 |
| `cargo clippy -p cowfs-meta --all-targets --locked -- -D warnings` | all targets | zero warning or error lines | 0 |

The four `recovery40` controls all pass individually, so the #40 invariant work is intact and is not implicated:

- `a_smaller_block_at_recovery_does_not_re_issue_inode_numbers`
- `the_stored_block_governs_a_plain_reopen_with_a_different_ino_block`
- `recovery_refuses_a_file_whose_reservation_block_is_unknown`
- `a_snapshot_id_lost_to_a_rollback_is_not_handed_out_again`

The stored-block proof from `pr116-final-delivery.md` still holds unchanged: `db.rs` is the same blob, no migration was invented, `FORMAT_VERSION` is untouched, the legacy refusal is intact, and no test was weakened, skipped or deleted.

## What this does and does not establish

Established, first-hand:

- PR #131 did not change the failing crate, so it is not the trigger
- The failure is `health.rs:230`, the damage search finding no reachable rollback in a 558 page round 2 fixture
- The redb panics are designed-for `guard()` behaviour, not the failure
- Reachability of `RolledBack` depends on redb page sharing, and the round 2 fixture is not byte-stable across runs here, 617 to 633 pages
- Current main is green on the same input, both for the exact test and for the whole suite

Not established, and not claimed:

- That the code is correct on the CI runner
- Why redb's page sharing differs between this host and the CI runner
- Why the round 2 fixture varies between runs on one host
- Any root cause in `cowfs-meta`

No dependency upgrade was guessed, no redb vendor fork was attempted, and no `cargo` version or `redb` pin was touched.
A dependency change is precisely the guess this lane was told not to make.

## Recommendation, and the decision that is not mine

The only observation that could carry a change is that the test reaches `RolledBack` through a brute-force search over 1 and 2 page damage windows, and that search is layout-sensitive.
Widening the window, or making the fixture construction deterministic, would be a real candidate, but neither can be justified here because I cannot produce the failing input that a fix would have to make pass.
So no change is proposed, and this lane stops rather than guessing.

Whether to change the test's damage search, whether to treat a layout-dependent search as an acceptable CI flake, and whether to rerun the `macos-latest` job are the coordinator's calls, not this lane's.

## Resource and safety record

- Free disk was `327.9 GiB` before the runs and `327.1 GiB` after, against a `20 GiB` floor
- Artifacts under this lane are `754.4 MiB`, against an `8 GiB` cap, of which `736 MiB` is the two isolated cargo target dirs and `229 KiB` is the preserved raw CI log together with the 120 byte `gh-axi` refusal that forced the fallback
- Every cargo run used its own `CARGO_TARGET_DIR` and its own `TMPDIR`, both inside this lease, so no shared target, mtime copy or cache was involved
- All cargo work ran under the shared `.treehouse-ready-wave/mac-heavy.lock` using the same non-blocking `flock` protocol the peer lane at PID 10420 was using, and that holder was never signalled
- No daemon was started, no mount was made or walked, no store, socket or Linux host was touched, and no signal was sent to anything
- Every damaged database was a private `tempfile` fixture inside this lease
- No irreversible action was taken, so there was nothing to verify before one, and no buffered generator exists to protect
- `git status --porcelain` is empty, and no process from this lane is still running
- No other lease, slot, agent's file, CI workflow, runner or mount was written or touched, and no lease was returned, reset, stashed, rebased or deleted
- No issue was created or edited