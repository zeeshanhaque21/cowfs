# PR 91 round 6: doc-only corrections

Reviewer: critic, original native build-train lease 12, exclusive. Target head `373e5bb6c79b6ad6a6174e2e22635c37f9e13619` against my accepted `7d9380aa345d682f52de45afb1013386af71785d`.

Nothing was built, mounted, sampled or cleaned this round. No Rust build, no cargo, no daemon, no mount table write, no crash sample, no 29-case matrix, no signals, no fixture deletion. The doc-only directive was followed and I did not repeat the builder's three rebuilds or add any of my own.

All five earlier reports are byte-identical after the fast-forward, and every round-5 lab fixture is untouched, mtimes still 16:41 to 16:50.

| axis | verdict |
|---|---|
| DOC review, complete delta | **PASS with 1 minor precision defect** |
| SOURCE and PROOF | **carried on byte identity**, zero code delta |
| g6 and success criterion 3 | **BLOCKED**, unchanged |
| merge, ready for coordinator | **not yet**: CI for this exact head was still running at my read |

## The delta is exactly one file, 98 insertions, 32 deletions

| check | result |
|---|---|
| files changed `7d9380a` -> `373e5bb` | 1, `docs/verification/daemon-crash-acceptance.md` |
| numstat | 98 / 32 |
| any path outside that file | none |
| `.rs`, `Cargo.toml`, `Cargo.lock` delta | none |
| tracked file count | 532 at both heads |
| `scripts/verify-daemon-crash.py` | blob `f918270b5c` both heads, sha256 `1c400c5edd34208e` |
| `bench/test_daemon_crash.py` | blob `c48b029e7d` both heads, sha256 `d7720122c3` |
| `bench/test_gates.py` | blob `e325f2047d` both heads, sha256 `df7e2d4873` |
| bench delta at this revision | none |

All three digests are exactly the ones the brief expected. The harness, the harness tests and the gate tests are byte-identical to the head where I ran them, so every SOURCE and PROOF result from round 5 carries without a rerun: 44 of 44 parser controls, 48 of 52 byte-identical bodies, the scoped 2-execution integrated run with 1 pass and 1 fail, and the 154-test discovery at `OK (skipped=1)` exit 0.

I read all 13 hunks, not the two sentences the round-5 report disputed. The hunks land in the Provenance section, the source-inputs section, the re-measurement section and the CI section. The verdict table at line 13, the receipt-model section, the teardown policy at lines 450 and 469, the power-loss disclaimer at 509 to 511 and the gc note at 538 are all outside the delta and unchanged.

## P1 correction: verified against tracked source, not artifacts

The inference is gone. The section is now titled "The binary's source inputs changed after that run", and the argument is two tracked blobs.

| tracked file | at `90c9a8f` | at main `46b0f26` | commit that changed it |
|---|---|---|---|
| `crates/cowfs-core/src/ns.rs` | `84088323fb5798c129d1f3454bce88d60ca0bfbc` | `5a1024c115d51806131c6f6cc9ff99f5f20e6588` | `2d00743`, #94 |
| `crates/nfsserve/src/tcp.rs` | `c92d50e07fd4532fa423fd89cd17b2e018d13542` | `97e3ecd7363995d6b178509dbefd417ec7524184` | `af1d113`, #93 |

Both blob pairs read exactly as written in the document. Path-scoped `git log 90c9a8f..46b0f26` attributes `ns.rs` to `2d00743 fix(core): keep the dentry of an elided unlink dirty until its queued removal commits (#94)` and `tcp.rs` to `af1d113 fix(nfsserve): EMFILE from accept is recoverable, not fatal`, so the issue labels are not swapped and neither is misattributed. `crates/cowfs-nfs/src/mount.rs` also moved under `af1d113`.

The count of five is right: `git diff --name-only 90c9a8f 46b0f26 -- '*.rs'` returns exactly five, `ns.rs`, `tcp.rs`, `elide_dentry.rs`, `mount.rs` and `resource_bounds.rs`, and the document names all five. I did not take that on trust and I did not let it stand on the two-file table alone.

Substantive descriptions check out against the diffs. The `tcp.rs` change adds `EMFILE = 24` and returns true from `is_transient_accept_error`, which is the recoverable-not-fatal behaviour the title claims. The `ns.rs` change replaces `self.dents.put(parent, name, None, 0)` with the seq returned by `q.touch(&pn)`, so a negative dentry is no longer recorded clean while its removal is uncommitted, in two places.

Provenance is now split the way the evidence splits. Reproducible source identity is `git rev` plus the harness digest, and the document's four values are all correct: sha256 of the harness file is `047cf9750bcfa98d` at both `90c9a8f` and `1a2b4f5`, `d328d411b70e6304` at `2db2f7f`, `1c400c5edd34208e` at `7d9380a`. Build-artifact identity is labelled non-reproducible, and the reason is given as measured rather than asserted. My own five-build counterexample is quoted, with the honest part kept: a reviewer could not reproduce `eb48f336` or `e0cc963b` from that tree at all. The cache consequence is stated correctly as fail-safe over-rejection, not as proof of source identity.

I independently confirmed the "no absolute build path embedded" claim on my own build of the same tree: `strings -a` finds zero occurrences of the worktree prefix in either binary. My five digests stand as recorded in my round-5 report and are not restated here as new evidence.

One wording note that is not a defect: the document says three rebuilds produced three distinct digests where I measured five. Both observations support the same claim, and neither number is load-bearing.

## P2 correction: 154 is the discovery total, and the split is right

| claim | verified |
|---|---|
| `discover -s bench` is 154 at this revision | yes, 118 plus 36 |
| `bench/test_daemon_crash.py` has 118 | yes, 118 `def test_` methods |
| `bench/test_gates.py` has 36 | yes, 36 methods |
| 1 skip, `no /proc on this platform`, at `bench/test_daemon_crash.py:1362` | yes, line 1362 is that `skipTest`, and it is the only `skipTest` in the file, none in `test_gates.py` |
| the 36 belong to a file this work did not add | correct, `test_gates.py` is untouched by PR 91 at this revision |

The count structure was checked by reading the code, not by re-running anything. The executed result carries from the identical blobs: `Ran 154 tests`, `OK (skipped=1)`, exit 0. The document also states the distinction that matters, 154 to re-run and 118 attributable to this harness.

## Defect 1, P3: the dentry change is attributed to the wrong operation

The document says an elided create now marks the entry dirty with the queued `seq` instead of `0`, and repeats it later as "#94 changes the dentry cache's dirty marking on an elided create".

Both changed sites are in `op_unlink` and `op_rmdir`, not in a create path. `try_elide(&cn)` returning true there means the create was elided earlier; the dentry being marked is the negative entry the unlink or rmdir records, and the `seq` is the one that removal reached. The code's own comment says "eliding the create says nothing about an earlier queued unlink", which is the trigger, not the actor.

This is a prose precision defect in the paragraph added to answer my P1, and it does not touch the load-bearing claim. I read the full `ns.rs` diff: neither hunk is near the `fsync`-after-`rename` COMMIT path, and the same is true of the `tcp.rs` diff, so the statement that #93 and #95 do not fix #90 holds. One clause to reword.

## No weakened or new claims anywhere in the document

| check | result |
|---|---|
| verdict table line 13, g6 and criterion 3 | still **BLOCKED**, outside the delta |
| power loss | still disclaimed at 509 to 511, `SIGKILL` is not a kernel crash |
| gc | still recorded as reclaiming nothing at 538 |
| performance, throughput, benchmark claims | none in the document |
| 29-case matrix | labelled `90c9a8f`'s and pre-merge, restated as not established on merged source |
| 2-execution re-measurement | scoped to the failing boundary and its passing neighbour |
| receipts | nothing downgraded, the document says so and the ledger still shows `durable@nfs_commit` |
| teardown policy | `unknown` still "blocks cleanup and runs no `umount`", unchanged, no promise that anything is removed |
| images, screenshots | none |
| ignored evidence paths | both marked gitignored and not presented as shared or public |

The unparseable-entry and escape restrictions are described exactly as at `7d9380a`: the four documented escapes only, one pass, no rescan, exact-target comparison, and `unknown` for empty, non-zero, missing-binary and unreadable cases. No cleanup guarantee was invented.

## PR body: rstrip-normalized equality, not byte equality

Measured on the live body, one read.

| | |
|---|---|
| raw bytes as returned by the API | 15723 |
| trailing bytes | two newlines, `... this PR.\n\n` |
| length with one trailing newline removed | 15722 |
| length with all trailing whitespace removed | 15721 |
| sha256 raw / rstripped | `a995146c2ac77091` / `f5f44ff5c9fe20c2` |

So the recorded pair 15722 against 15723 is exactly a one-trailing-newline difference, which means the equality that was checked was on a rstrip-normalised form. The raw bodies were not byte-identical. Stating it precisely: the check proved the bodies agree after trailing whitespace is dropped, not that they agree byte for byte.

One structural point the check cannot cover. The PR body is GitHub metadata, not a tracked file, so it does not appear in this commit's delta. The only file in the delta is the acceptance document. Body preservation across this revision therefore rests entirely on the API comparison above and on nothing in git.

## Current CI for this exact head: still running, not green

Run `37245744603`, workflow `ci`, branch `verify/full-stack-crash-88`, event `pull_request`, `head_sha=373e5bb6c79b6ad6a6174e2e22635c37f9e13619`. The sha matches the reviewed head exactly.

Three reads, all within about two minutes, no polling loop, no dispatch, no rerun, no runner changes:

| read | observed |
|---|---|
| run and jobs | `in_progress`, no conclusion; `linux-fuse` completed success, `check (ubuntu-latest)` and `check (macos-latest)` in progress |
| ubuntu job steps | steps 1 to 7 success including `cargo fmt`, `cargo clippy -D warnings` and `cargo test --workspace`; step 8 `Bench harness unit tests` **in_progress** |
| `linux-fuse` job steps | completed success, 13 steps, its own native controls green |

The harness step had not yet passed on ubuntu when I looked. `mergeable_state` is `unstable` at this head, which is what a pending required check looks like.

The green run `37244249112` belongs to `7d9380a` and is not this head's result. Nothing in this round establishes CI green for `373e5bb`.

## Main moved, and the 154 is scoped correctly

Primary main is now `724f81c1731bf409497f7062574462df2ca0045b`, PR 89 comparator coverage.

| check | result |
|---|---|
| PR 89 paths over `46b0f26` | `bench/compare.py`, `bench/test_compare_coverage.py`, `bench/test_gates.py`, `docs/benchmark-coverage.md` |
| new tests added | `bench/test_compare_coverage.py` has 16 |
| main-only discovery total | 52, since `bench/test_daemon_crash.py` does not exist on main |
| post-merge projection | 118 plus 36 plus 16 equals 170 |
| `724f81c` inside the PR 91 head | no, not an ancestor |
| `46b0f26` inside the PR 91 head | yes, ancestor |
| `bench/test_gates.py` touched by PR 91 | no, and its blob differs between the branch and main, `e325f204` against `f9d00e07` |

So the document's "154 at this revision" is correct at its own head and will read 170 once main advances and the branch merges. The document scopes the number to its revision and makes no claim about latest main. The four PR 89 paths are disjoint from PR 91's, except that PR 89 also edits `bench/test_gates.py`, which PR 91 has never modified, so there is no shared-file conflict to resolve from this side. I did not merge anything and make no recommendation on how the two combine.

## PR and issue state, read once

PR 91: head `373e5bb6c79b6ad6a6174e2e22635c37f9e13619`, base `46b0f269d5bef4a2c204c25f5b3015da601d3beb`, `state=open`, `draft=false`, 13 commits, `mergeable_state=unstable` pending CI. GraphQL `closingIssuesReferences.nodes` is `[]`, so there is no acceptance auto-close. Issues 88 and 90 are both open. The body opens "Related to #88" and disclaims g6 in its own words.

## g6 and success criterion 3: still BLOCKED

Nothing this round moves it. The only failing boundary was re-measured on merged source in the previous head's run and still loses the promised name after a caller `fsync` that returned success, with fsck clean. That is #90's defect, tracked by open issue #96, not fixed in this branch. All-green unit tests are not g6 acceptance and are not reported as such anywhere in this document.

Still unproven and deliberately not attempted: real uninterruptible `umount` cwd pinning, power loss, a crash mid-`gc`, reclamation, a live Linux mount table, and the 29-case matrix on merged source.

## Isolation

| | |
|---|---|
| shared pid 15263 | alive, same start time, about 20h22m elapsed, read via `ps` only |
| Rust builds, cargo, mounts, daemons, crash samples, matrix runs | none this round |
| signals, device work, private fixture cleanup | none |
| reports 1 to 5 | byte-identical after the fast-forward |
| round-5 lab fixtures | present, mtimes unchanged, nothing deleted |
| new files this round | this document only; no lab directory was created |
| commits, pushes, merges, lease returns, workflow dispatch or rerun | none |
| HEAD | `373e5bb6c79b6ad6a6174e2e22635c37f9e13619` |

## Coordinator note

Source and proof are clean on this head and the doc-only delta is honest about what it does and does not establish. Two things stand between this head and a pinned merge: the P3 rewording of the dentry attribution, and CI run `37245744603` actually finishing green, which at my read it had not. When it lands, the green run for this sha is the thing to pin, not `37244249112`.