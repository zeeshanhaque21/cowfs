# PR136 final review: ctime is the time of the change, not the time of the batch

Reviewed PR head: `cde59305fe05f4551468965cd648888eb98e9dbf`, branch `fix/deferred-operation-time-42`.
Author's code head: `2a06ea916a670be69ffad55ae9af2c26ab6377e3`, the direct parent of the reviewed head.
Base, and current `main`, remote and local: `93cfef94457a989d031cb6b0a475ac4edbdb85ef`.
Tracker: issue #42, open, `labels: []`, body sha256 `67caa764…` over 36 lines, retrieved twice with the same hash.
Scope: `Tx::set_now` in `crates/cowfs-meta/src/tx.rs`, `Inner::op_times` and the replay stamps in `crates/cowfs-core/src/inner.rs`, the two new fixtures, and one row in the `docs/v1-core.md` lock audit table.

No checkout, branch change, source edit, reset, stash, commit, push, merge, lease action or issue edit was performed.
No daemon, mount, socket or signal was touched. No performance or timing acceptance is claimed.

## Verdict

**PASS**, with four findings recorded below, none of them blocking.

The requested contract holds, and it holds under cases the delivery's own fixtures do not name: rename alone in a batch, replacing rename, unlink of one of two names, a created-then-removed directory, a name replaced across kinds, a node cache small enough to evict throughout a batch, and two writer threads against one file with a background flusher.
Every comparison in this review is exact equality against a time the public `Vfs` reported at the moment of the operation, and every source tree was extracted from a named commit and proved byte-identical to it first.

The old build fails the property and the new build satisfies it, which is the difference between a real fix and a new API.

| item | result |
| --- | --- |
| the contract: a deferred operation's stored `ctime` equals the time the mount reported | **holds**, verified on 15 independent probes plus the 4 delivery fixtures |
| old source, delivery fixture | **0 passed, 4 failed**, exit 101, every gap in the microseconds |
| new source, delivery fixture | **4 passed, 0 failed**, exit 0 |
| old source, metadata fixture | does not compile, `no method named 'set_now'`, 9 errors, exit 101 |
| new source, metadata fixture | **5 passed, 0 failed**, exit 0 |
| my 6-case probe, old source | **0 passed, 6 failed**, exit 101 |
| my 6-case probe, new source | **6 passed, 0 failed**, exit 0 |
| my 3-case replay probe, new source | **3 passed, 0 failed**, exit 0 |
| `cowfs-meta` suite | 12 suites, **87 passed, 0 failed**, 2 ignored, exit 0 |
| `cowfs-core`, the delivery's 18 targets | 18 suites, **126 passed, 0 failed**, 1 ignored, all exit 0 |
| `cowfs-core`, the 9 targets it does not name | **167 passed, 0 failed**, 8 ignored, all exit 0, `conformance` 132 of them |
| `cargo fmt --all -- --check` | clean, exit 0 |
| `cargo clippy -p cowfs-meta -p cowfs-core --all-targets -- -D warnings` | clean, exit 0 |
| the lock audit row is load-bearing, not decorative | **measured**: `critic2b` 27 pass exit 0 with it, 26 pass 1 fail exit 101 without it |
| the per-operation replay stamps are load-bearing | **not measured as such**: a mutant with all five removed still passes every fixture and probe |
| two writers, one file, background flusher | 20 of 20 exact at the head; 6 of 6 "later than reported" at the base |
| CI at the exact head | 3 check runs, all completed/success |
| PR refs | `closingIssuesReferences` null, no closing form, #42 open |
| merge into current `main` | **clean**, tree `ffdfbb5f…`, no conflict |

## Source binding

Every result below came from a tree extracted with `git archive` from a named commit, and every tracked file was compared with `git hash-object` against `git rev-parse <rev>:<path>` before anything was built.

| archive | commit | tracked | extracted | mismatched |
| --- | --- | --- | --- | --- |
| `base-93cfef9` | `93cfef9` | 641 | 641 | **0** |
| `code-2a06ea9` | `2a06ea9` | 643 | 643 | **0** |
| `head-cde5930` | `cde5930` | 645 | 645 | **0** |

The base archive was then given the two new test files and nothing else, so the only difference between the old and new runs is the two production files.
That was checked file by file after the copy, not assumed.
sha256 of the four files the result rests on, matching the delivery's own table:

| file | old | new |
| --- | --- | --- |
| `crates/cowfs-meta/src/tx.rs` | `5cafb0cc2e10f1e2…` | `a92c3d76c573fb77…` |
| `crates/cowfs-core/src/inner.rs` | `ba3036909c6d5fe8…` | `b94fe02d1474c51b…` |
| `crates/cowfs-core/tests/operation_time.rs` | absent at base, given | `4139220cb220b041…` |
| `crates/cowfs-meta/tests/operation_time.rs` | absent at base, given | `10bb5b61bef78a2d…` |

The delivery's four recorded divergences reproduce in kind and in magnitude class.
My own old-source run of `a_deferred_write_keeps_its_own_ctime_and_does_not_move_an_older_file`, first in this review, exit 101:

```
left:  Timestamp { secs: 1791231173, nanos: 279281000 }   <- the flush time
right: Timestamp { secs: 1791231173, nanos: 278953000 }   <- when the write reported it
```

The two immutable evidence files are unmodified in the primary checkout, at `ed9609e2…` and `fbc6a078…`, and their blobs are identical to the ones at the reviewed head, so the mirror is the committed document and not a local variant.

## What the probes add

The delivery's four `cowfs-core` cases cover create, write, setattr on one inode, and a namespace change on a parent and a child.
None of them names a rename, a link, an unlink, an rmdir, a second batch for one inode, or a node table small enough to evict.
I wrote those, plus a metadata-seam probe and two concurrency probes, all in the critic's own private archives.

**A mixed batch carrying every queued operation kind** (`zz_critic_ctime_probe`, 6 cases, 6 pass at the head, 6 fail at the base).
Create, write, chmod, hardlink, unlink, rmdir, cross-directory rename, and a later mkdir in the destination directory, then one flush and a reopen: the untouched sibling keeps its create time, the source directory keeps its last entry change, the destination directory keeps its own, the renamed inode keeps its own, and both removed names stay gone.
The hardlink identity check is on `nlink`, because inode numbers are session-local for virtual inodes and comparing them across a reopen would be wrong.

**Rename alone in a batch, and a replacing rename** (`zz_critic_replay`, 3 cases, 3 pass at the head, 3 fail at the base).
A batch whose only operation is a rename has no earlier stamp to inherit, so it isolates the path.
A rename that replaces another name additionally frees the destination inode, and the batch carries a create, a rename and an rmdir across two directories.

**Unlink of one of two names** (`zz_critic_replay`).
`Op::Unlink` is keyed on the parent and `Tx::unlink` writes the child's `ctime` from the same `self.now`, so the surviving inode's record is first written with the parent's time.
Only the final attribute loop, which stamps per inode, corrects it.
It does: the head's durable `ctime` for the survivor equals the survivor's own last reported time, and the base's is 3.5 ms later.

**A created-then-removed directory** (`zz_critic_replay`).
The elision path, where no `Op` reaches meta at all and only the parent is touched.
The parent's durable `ctime` is its own last reported time at the head, 22 ns later at the base.

**A node cache of 8 entries** (`zz_critic_eviction`, 3 cases, 3 pass).
Two hundred queued unlinks in one directory, forty setattr-only touches interleaved with lookups that churn the table, and forty writes to forty different files.
Every inode's durable `ctime` equals the time the mount reported for it, including the surviving parent directory.

**A concurrency probe** (`zz_critic_concurrent`, 3 cases).
Two threads writing one file with a background flusher on a 2 ms interval: 145,000 to 300,000 writes per run, and the durable `ctime` equals the last reported one every run.
Concurrent renames and rewrites across two directories with a partial flush between rounds: every name's durable `ctime` equals the last reported one.
Three threads creating, writing and unlinking in one directory with a background flusher: every unlink is durable and no survivor's durable `ctime` is earlier than one already reported.

**A metadata-seam probe** (`zz_critic_setnow_scope`, 4 cases).
`set_now` reaches every inode a `rename` names, is sticky inside one transaction until set again, and does not survive into the next one.

**A created file's durable `atime` and `mtime`** (`zz_critic_created_fields`, 2 cases).
`Tx::new_rec` builds a new inode's `atime` and `mtime` from `self.now` as well as its `ctime`, so this is where the delivery's "only `ctime` follows this value" claim is testable.
Through the public `Core` a created file's durable `atime`, `mtime` and `ctime` all equal the reported ones, and an explicit `SetTime::At` for both survives a rename and a batch.
At the metadata seam the claim is narrower than the doc comment says: a `create` under `set_now(T)` produces `atime == mtime == ctime == T`.
That is the same value `cowfs-core` would have written anyway on the create path, so no caller is misled and no user-visible time moves; see finding 3.

## The lock audit row is load-bearing

`critic2b::every_lock_site_is_in_the_audit_table` was run on the head and on a critic mutant carrying the identical source with that one table row deleted.

| tree | result | exit |
| --- | --- | --- |
| `2a06ea9` as committed | `27 passed, 0 failed, 1 ignored` | 0 |
| the same source, the `inner::op_times` row deleted | `26 passed, 1 failed, 1 ignored` | 101 |

The failure names the function:

```
1 of 111 lock-taking functions are not in the audit table of docs/v1-core.md: inner.rs::op_times
```

Run directly, outside `cargo test`, the binary exits 101 without the row and 0 with it, so the row is doing the work the delivery claims and not merely present.
`op_times` reads `n.st.rd()` before the commit, the same order as the `Inner::restore_state` row it sits beside, and the audit table's own rule is that any function taking a lock is listed.
`docs/v1-core.md` gains that one row and nothing else: no `Cargo.toml`, no `Cargo.lock`, no store-crate file, no `swap.rs`, no `ino.rs`, no `ns.rs`, no `io.rs`, no `queue.rs`, no `db.rs`, no `CHANGELOG.md`, no workflow.
No new clock abstraction, no dependency, no `Op` timestamp field, no on-disk format change, no inode-alias rule change.

## The per-operation stamps are not what carries the contract

`Inner::commit` stamps before each replayed operation and again before each `setattr`.
A critic mutant with all five operation-loop stamps removed and the final-loop stamp left in place passes everything: the delivery's 4 core fixtures, the delivery's 5 metadata fixtures, my 6-case probe, my 3-case replay probe, all exit 0.
A second mutant with the final-loop stamp removed fails 4 of 6 probes and all 4 delivery core fixtures.

So the whole invariant rests on the one stamp at `inner.rs:1015`, in the final attribute loop.
That is not wrong, and it is not a defect: every inode an operation writes is pushed into the batch's `touched` list by the queueing code (`ns.rs` push calls pass the same nodes as `structural` and `touch`), so the loop already covers all of them with a per-inode value.
It does mean the delivery's mechanism description overstates what the five replay stamps contribute, and a reader who deleted them would find every test still green.
Worth a line in the evidence record; not worth a code change.

## Findings

**1. Two line citations in the evidence record point at the wrong lines, and one is off by two lines.**
`docs/verification/evidence/meta42-operation-time.md` cites `crates/cowfs-core/src/inner.rs:880-911` for `op_times` and `tx.rs:407` for `Tx::setattr`'s `ctime` write.
At the head, `op_times` is at `inner.rs:879-908` and `rec.ctime = self.now` in `setattr` is at `tx.rs:418`; line 407 is a closing brace.
`inner.rs:862-868` for `restore_states` is also off by one, at 863.
Every other citation I checked resolves exactly: `tx.rs:28`, `db.rs:714`, `:735`, `:740`, `inner.rs:915`, `:1013`, and all of `ns.rs:195`, `:206-207`, `:286`, `:292`, `:336`, `:341`, `:411`, `:416`, `:548-549`, `:551`, `:555`, and `io.rs:103`, `:202`, `:399`, `:411`.
Correction is a source-clock document edit, inside the paths this review owns, and it is the author's to make.

**2. The `docs/v1-core.md` line citations in the same record are off by two and by three hundred and twenty-two.**
The record cites `docs/v1-core.md:556` for request 2 and `:238` for the cached-`ctime` workaround.
Request 2 is at line 558 at the base and 559 at the head; line 556 is the swap workaround that belongs to request 1.
"Cached ctime is exact while the node is cached." is at line 560 at the base and 561 at the head; line 238 is `Data (write) is buffered per file.`
The quotations themselves are verbatim and correct, so the contract is not misrepresented; the numbers are.

**3. `Tx::set_now`'s doc comment says "only `ctime` follows this value", and at the metadata seam that is not true of the create path.**
Measured at the seam: `tx.set_now(T1); tx.create(...)` yields `atime == mtime == ctime == T1`, because `Tx::new_rec` (`tx.rs:81-96`) builds all three from `self.now`.
The same command in the commit body states "Only `ctime` follows the stamp", and the fixture `set_now_does_not_replace_an_explicit_atime_or_mtime` passes because `setattr` takes `atime`/`mtime` from the caller, not from `now`.
Through `cowfs-core` this is invisible: I verified a created file's durable `atime` and `mtime` equal the reported ones, because the final attribute loop rewrites both from the cached values, and the create's now-derived values are the same clock reading the cache already holds.
So no user-visible time is wrong and no caller can be misled, and the doc comment is the thing to tighten.

**4. `set_now` is public on `Tx`, which is handed out only inside `Snapshot::batch`, so the seam is reachable by any external caller of `cowfs-meta`.**
The delivery notes that `Snapshot::batch_at` was not added and that `set_now` satisfies the request as written, which I agree with: the issue offers either.
This is a note, not a finding against the change: `cowfs-meta` already exposes `Tx` operations that mutate a tree, so `set_now` adds no new capability class, only a new value for a field callers could already not reach.
Worth stating that the surface is unchanged in kind.

Two things I looked for and did not find, recorded so the absence is deliberate rather than an omission.

**No `Op` variant carries a timestamp, and no queued operation is stamped with a time belonging to an uncommitted later operation.**
The stamp is the cached `ctime` of the subject inode, read once before the commit, and `Op::Content` replaces any earlier queued content operation for the same inode (`queue.rs:145-163`), so a batch cannot carry two content operations for one inode with two different times.
For the other kinds, a batch can carry two operations naming the same parent, and the map keeps the last write, which is that parent's latest cached `ctime`, which is the time of the last change to it.
That is the POSIX meaning of `ctime` and it is what the mount already reports.

**No eviction path removes a node that has uncommitted work, so the "absent from the map" fallback is not reachable through the public surface.**
`maybe_evict_node` returns early unless `node.seq <= sc.flushed()` and `node.ns_seq <= sc.flushed()` and `dirty_bytes() == 0` and `nlink > 0`, and `shrink_nodes` requires the same `seq`/`ns_seq` conditions plus `strong_count == 1`.
An inode with a queued operation has `seq` above `flushed`, so neither can drop it.
I probed this directly with a node cache of 8 entries and 200 queued unlinks, 40 setattr-only touches and 40 writes to 40 files: every subject stayed stamped correctly.
The consequence is worth stating precisely, because it is a source reading and not a measured counterexample: if the fallback branch ever were reached, an unstamped operation would inherit the *previous* operation's `set_now`, not the transaction's wall clock, because `stamp` is a closure that only calls `set_now` when the map holds the inode and `Tx::now` is shared by the whole transaction.
The record's sentence "its operations keep the transaction's wall-clock opening time" describes the intent, not what the code would do on that branch.
I did not produce a runtime counterexample and I am not claiming one.

## The one direction this change moves

Under two writer threads against one file with a background flusher, the base's durable `ctime` is always *later* than the last one reported, which is the defect being fixed: 6 runs, 6 of 6 late, by 5.4 ms to 12.9 ms.
The head's is equal in 20 of 20 runs.

In an earlier, longer pass at the head I saw the opposite once in 12 runs: a durable `ctime` 9 microseconds *earlier* than a `ctime` already reported.
Two follow-ups bear on it and neither closes it.
A probe that watches the cached value directly showed the cache itself moving backwards, 5 to 79 times per 300,000 writes, at the head and at the base alike, so the cache layer is not monotonic under concurrent writers.
`Inner::load_node` rebuilds a node's attributes from metadata, which holds the last durable value, so a node evicted and reloaded reports an older `ctime` than the one it had; both eviction guards require `seq <= flushed`, which a node under active writes fails, so the reload is not the path here, and I did not identify the path.
A critic mutant that moves the clock reading in `op_write` inside the node write lock, so two writers cannot install out-of-order values, then ran 12 and 20 more times without reproducing it, against 20 clean runs of the unmodified head in the same batch.
So: a real observation, a plausible mechanism, an unconfirmed cause, and a rate I cannot state because the sample is one occurrence in 32 runs.
It is a pre-existing cache-level non-monotonicity that this change makes *visible* in the stored value rather than *causing*; at the base the same regression was overwritten by the later batch time.
Not a blocker, and not something this PR should fix, but it belongs in the record rather than in silence.

## CI, PR and integration

One read-only snapshot at the exact head, and the workflow file itself:

| check | status | conclusion |
| --- | --- | --- |
| `linux-fuse` | completed | success |
| `check (ubuntu-latest)` | completed | success |
| `check (macos-latest)` | completed | success |

`total_count` is 3, so CI is **green at `cde5930`**, which is the usual merge gate and is satisfied here on its own evidence.
The combined legacy status endpoint reports `pending` only because it holds zero legacy contexts; that is not a failure and not a missing check.
`ci.yml` triggers on `push` to `main` and on `pull_request` with no branch or path filter, and declares exactly these three jobs.
No dispatch, rerun, poll or runner configuration change was made.

PR #136 is not a draft, base `main`, head `fix/deferred-operation-time-42` at `cde5930`, `mergeable_state` clean, 7 changed files, +957/-1.
`closingIssuesReferences` is null, so nothing auto-closes.
I audited the body for closing forms with the paragraph-split shapes too, keyword-then-newline-then-`#N`, issue-first-then-keyword, and the `addresses #N` and `part of #N` variants: 0 hits.
The only issue-naming sentences are "Addresses **request 5 of #42** only", "**#42 stays open**: this does not complete the umbrella", and two references to the record and the tracker.
Neither commit subject contains a closing form.
Issue #42 is `open` and stays open; requests 1, 3 and 4 are untouched, as the record states.

Integration against the **explicit** current `main` `93cfef9`, which is also remote `refs/heads/main`: `git merge-tree --write-tree 93cfef9 cde5930` wrote tree `ffdfbb5f7f0e0f8b4b631b4ca7c5c8c020502709` with no conflict.
No combined-runtime claim is made: nothing was built or measured through a merge.

## Not run, and not claimed

- No `cargo test --workspace` locally. The two crates' suites were run, all 27 `cowfs-core` targets and all 12 `cowfs-meta` result lines; CI ran the workspace.
- No performance or timing acceptance. The microsecond gaps are recorded as measurements of a defect, not as a budget.
- No `SIGKILL` or power-loss claim. The crash-injection targets `kill9`, `crash` and `durability` passed, which is evidence nothing regressed, not evidence of power-loss behaviour.
- No daemon, mount, socket, nfs mount or shared store was touched; the pid 15263 daemon, its store and its two nfs mounts were left alone.
- No signal, sudo, install, restart, reset, prune, destroy, workflow edit, dispatch or rerun.
- `rename_dir` is not queued and is documented as out of the deferred contract; I did not widen it, and my probes exercise it only as a neighbouring operation.
- `xattr` writes bypass the queue and are documented as such; likewise not widened.
- `no-mistakes` is **not initialized** in this repository (`.no-mistakes` and `.claude` are both absent), so that pipeline was not run and no claim is made about it.
- No browser or Playwright step. This is a filesystem-metadata change with no surface to drive in a browser; the `playwright` on this machine is a venv binary and `chromium` is not installed, so any browser step would be **UNVERIFIED** here.
- The full working tree was not run: `--workspace`, the daemon, `PathVfs` and the FUSE and NFS adapters are outside the scoped suites this review measured, and CI is the evidence for them.

## Recommendation

Land it.

The defect was real and reproduced end to end on the exact base, the fix is eleven lines at one assignment plus one read and one stamp at the correct seam, and the property now holds across fifteen independent probes that the delivery's four fixtures do not reach, including rename, unlink, rmdir, hardlink, multi-batch, small-cache and concurrent cases.
The audit row is load-bearing and I measured it failing without the row.
The scope is exactly what it claims: one assignment, one pre-commit read, one row, no dependency, no format change, no clock framework, no alias rule.

Before merging, three line-citation corrections in `docs/verification/evidence/meta42-operation-time.md` and one sentence in `Tx::set_now`'s doc comment.
None of them is a correctness problem, and none is worth a second review pass on its own; they are worth fixing because the record is the thing a future reader will trust.

Requests 1, 3 and 4 of issue #42 remain open, and the hole flag, the reservation API and the atomic snapshot rename are untouched by this delivery.
Whole #42 is not closed by it, and should not be.

## Where the evidence is

All paths relative to the assigned worktree `.treehouse-ready-wave/.treehouse/cowfs-7c1bf8/3/cowfs`, under `bench/out/meta42-operation-time-final-critic/` (gitignored):

| what | where |
| --- | --- |
| the lane, one bounded 600 s acquisition, 8 GiB cap, 20 GiB floor, 60 s no-progress | `scripts/lane.sh`, log `logs/lane.log` |
| extraction and the 0-mismatched check for all three commits | `scripts/extract.sh`, `archives/mismatch-*.txt` |
| the scoped gate matrix, every exit code read directly | `scripts/gates.sh`, `logs/*.exit`, `logs/gates-run.log` |
| the first actual sample, old fail then new pass | `scripts/sample.sh`, `logs/sample-*.log` |
| the delivery fixtures and my probe across four arms | `scripts/probe-arms.sh`, `scripts/probe-run-all.sh`, `logs/probe-*.log` |
| the three mutants: touched-loop stamp, replay stamps, audit row | `scripts/probe-mut*.sh`, `logs/probe-mut*.log` |
| the concurrency probes, head and base | `scripts/probe-conc.sh`, `scripts/probe-mech*.sh`, `scripts/quant-conc.sh`, `logs/probe-conc*.log`, `logs/q-*.log` |
| the clock-under-lock mutant and its 32 runs | `scripts/probe-mut-clock.sh`, `scripts/rate-mutclock.sh`, `scripts/ab-clock.sh`, `logs/ab-*.log` |
| the nine `cowfs-core` targets the delivery does not name | `scripts/extra-targets.sh`, `logs/extra-*.log` |
| my probe sources | `probe/zz_critic_*.rs`, 8 files, private copies only |

The only writes are this report in the primary checkout and this lane's own artifacts under `bench/out/meta42-operation-time-final-critic/**` in the assigned worktree.
Neither immutable evidence file was edited: both still hash to `ed9609e2…` and `fbc6a078…`.
The two immutable files were read only; `docs/v1-core.md` in the primary checkout carries one uncommitted line, the `inner::op_times` audit row, which was there before this review began and which I did not add, commit or revert.
The assigned worktree still sits at `016769e7f4076a5c0fc712a65932c546048052f7` with zero tracked-file changes.
Rebuildable cargo target caches for arms whose results are already recorded were pruned to stay under the lane's 8 GiB cap; every source tree and every log was kept.