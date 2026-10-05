# PR 136 erratum: current claims that supersede the wrong ones in the evidence record

Refs #42.

The independent review passed the change and recorded four findings, none blocking.
Two of them are wrong claims in the delivery's own evidence record, and one is a false doc comment in
production source.
All three are corrected here rather than by editing the immutable files, so what was claimed and when
stays readable.

| file | status | sha256 |
| --- | --- | --- |
| `docs/verification/evidence/meta42-operation-time.md` | immutable, unchanged, historical | `ed9609e23a389fde20917a3aa87e168a0bcea24848fbc9c656df1b6c84d9762b` |
| `docs/verification/evidence/meta42-residual-verification.md` | immutable, unchanged, historical | `fbc6a078137b0fab370638d27dcaf64ff3ad283de37d8e7e970e4b10faac53ba` |
| `docs/reviews/pr136-meta42-operation-time-final.md` | independent review, unchanged | `c4999d45af0ea0d78344ab311be378102ec0856931939bd4f96468f0965a8644` |

| | |
| --- | --- |
| branch | `fix/deferred-operation-time-42` |
| reviewed head | `cde59305fe05f4551468965cd648888eb98e9dbf` |
| code head | `2a06ea916a670be69ffad55ae9af2c26ab6377e3`, the direct parent of the reviewed head |
| base | `93cfef94457a989d031cb6b0a475ac4edbdb85ef`, which was `main` at review time |
| production change here | `crates/cowfs-meta/src/tx.rs`, doc comment only |
| test change here | `crates/cowfs-meta/tests/operation_time.rs`, one added test and two message strings |
| untouched | `inner.rs`, `io.rs`, `ns.rs`, `db.rs`, `types.rs`, the store crate, `swap.rs`, `rename`, `docs/v1-core.md`, `Cargo.toml`, `Cargo.lock` |

## Erratum 1: the false doc comment, corrected in source

`Tx::set_now`'s doc comment at the reviewed head said:

> Only `ctime` follows this value: `atime` and `mtime` are whatever the caller asked for, and an
> explicit time in a `setattr` is never replaced.

The reviewer measured that this is false at the metadata seam, and the source confirms it.
`Tx::new_rec` at `crates/cowfs-meta/src/tx.rs:87-89` builds a new inode's `atime`, `mtime` and `ctime`
all from `self.now`, so a `create` under `set_now(T)` yields `atime == mtime == ctime == T`.

The corrected comment says what the code does:

A newly created inode takes all three of its times from this value.
An inode that already exists takes only its `ctime` from it, and its `atime` and `mtime` are whatever
the caller asked for.
A time given explicitly in a `setattr` is never replaced, including on a create.

The commit body of the delivery carried the same false sentence and remains historical.

### No user-visible time is wrong, and the reviewer proved it

Through `cowfs-core` the create path's now-derived `atime` and `mtime` are invisible, because the final
attribute loop rewrites both from the cached values, and the create's now-derived values are the same
clock reading the cache already holds.
The reviewer verified a created file's durable `atime` and `mtime` equal the reported ones, and that an
explicit `SetTime::At` for both survives a rename and a batch.
So this is a documentation defect with no behavioural consequence, and no caller can be misled by the
old sentence once the comment states the seam behaviour.

## Erratum 2: the line citations that do not resolve

Every citation below was re-verified by reading the reviewed head, not by trusting the review's numbers.

| the record cites | actually at the reviewed head | what is there |
| --- | --- | --- |
| `inner.rs:880-911` for `op_times` | `inner.rs:879-908` | `fn op_times` starts at 879, ends at 908 |
| `inner.rs:862-868` for `restore_states` | `inner.rs:863` | `fn restore_states` starts at 863 |
| `tx.rs:407` for `rec.ctime = self.now` | `tx.rs:418` | 407 is `rec.mtime = self.now;` |
| `docs/v1-core.md:556` for request 2 | `docs/v1-core.md:559` | 556 is the staged-swap workaround belonging to request 1 |
| `docs/v1-core.md:238` for the cached-`ctime` workaround | `docs/v1-core.md:561` | 238 is "`Data (`write`) is buffered per file.`" |

The `tx.rs:407` entry needs one clarification rather than a correction.
At the reviewed head `407` is `rec.mtime = self.now;`, and the doc comment fix in this change adds
three lines above it, so after this commit the same statement sits at `421`.
The reviewer described 407 as a closing brace, which is what it would be at a different revision; the
substantive point stands either way, which is that the cited line was not the `rec.ctime` assignment.

The quotations themselves are verbatim and correct, so the contract is not misrepresented by any of
these.
Only the numbers were wrong, and they were wrong in the direction of pointing at less code than the
text describes.

Every other citation the reviewer checked resolves exactly and needs no change:
`tx.rs:28`, `db.rs:714`, `:735`, `:740`, `inner.rs:915`, `:1013`, all of `ns.rs:195`, `:206-207`,
`:286`, `:292`, `:336`, `:341`, `:411`, `:416`, `:548-549`, `:551`, `:555`, and `io.rs:103`, `:202`,
`:399`, `:411`.

## Erratum 3: the mechanism description overstates what the per-operation stamps contribute

`meta42-operation-time.md` line 65 presents `Inner::op_times` as the mechanism that makes the contract
hold.
The reviewer built two mutants and measured which stamps actually carry it:

| mutant | result |
| --- | --- |
| all five per-operation replay stamps removed, final-loop stamp kept | passes the delivery's 4 core fixtures, the 5 metadata fixtures, the 6-case probe and the 3-case replay probe, all exit 0 |
| the final-loop stamp removed, per-operation stamps kept | fails 4 of 6 probes and all 4 delivery core fixtures |

So the invariant rests on the single stamp at `inner.rs:1015`, `stamp(tx, *ino)` in the final attribute
loop, which runs once per touched inode.
That is not a defect: every inode an operation writes is pushed into the batch's `touched` list by the
queueing code, because the `ns.rs` push calls pass the same nodes as both `structural` and `touch`, so
the final loop already covers all of them with a per-inode value.

**Current claim:** the final-loop stamp is what carries the contract, and the five per-operation stamps
are not measurably necessary for it.
The per-operation stamps are **kept**.
Removing production code to tidy a description is not justified by a mutant that happens to pass, and
the review did not recommend it.
Their necessity is not established either way: the measurement shows they are not required by the
fixtures and probes that exist, which is not the same as showing they do nothing.

## Erratum 4: the absent-from-the-map fallback, stated correctly

`meta42-operation-time.md` line 78 says:

> Its operations keep the transaction's wall-clock opening time, which is exactly what they had before
> this change, so an eviction cannot make a timestamp worse than the old behaviour.

That describes the intent, not what the code would do on that branch, and the reviewer says so
explicitly as a source reading rather than a measured counterexample.

The branch is not reachable through the public surface.
`maybe_evict_node` returns early unless `node.seq <= sc.flushed()` and `node.ns_seq <= sc.flushed()` and
`dirty_bytes() == 0` and `nlink > 0`, and `shrink_nodes` requires the same `seq` and `ns_seq` conditions
plus `strong_count == 1`.
An inode with a queued operation has `seq` above `flushed`, so neither eviction path can drop it.
The reviewer probed this with a node cache of 8 entries, 200 queued unlinks, 40 setattr-only touches and
40 writes to 40 files, and every subject stayed stamped correctly.

**Current claim:** the fallback is unreachable through the current eviction guards, so its behaviour is
not part of the contract.
If it were ever reached, an unstamped operation would inherit the *previous* operation's `set_now`,
because `stamp` is a closure that calls `set_now` only when the map holds the inode and `Tx::now` is
shared by the whole transaction.
It would not read the wall clock.
No runtime counterexample was produced and none is claimed.

## The `set_now` surface is unchanged in kind

The reviewer's fourth finding is a note, not a defect, and it is recorded here so the surface is stated
rather than left implicit.
`set_now` is public on `Tx`, and `Tx` is handed out only inside `Snapshot::batch`, so the seam is
reachable by any external caller of `cowfs-meta`.
`Snapshot::batch_at` was not added, because issue #42 offers either and `set_now` satisfies the request
as written.

This exposes a new value for a field those callers could already not reach.
`cowfs-meta` already hands out `Tx` operations that mutate a tree, so no new capability class appears.
There is no privacy or security surface change, and no new clock abstraction, dependency, `Op` timestamp
field, or on-disk format change.

## The unconfirmed concurrent observation, carried as the reviewer left it

The reviewer recorded one direction this change moves, and one observation that does not close.

Under two writer threads against one file with a background flusher, the base's durable `ctime` is
always later than the last one reported, which is the defect being fixed: 6 runs, 6 of 6 late, by 5.4 ms
to 12.9 ms.
The head's is equal in 20 of 20 runs.

In an earlier, longer pass at the head the reviewer saw the opposite once in 12 runs: a durable `ctime`
9 microseconds earlier than one already reported.
Two follow-ups bear on it and neither closes it.
A probe watching the cached value directly showed the cache itself moving backwards, 5 to 79 times per
300,000 writes, at the head and at the base alike, so the cache layer is not monotonic under concurrent
writers.
`Inner::load_node` rebuilds a node's attributes from metadata, which holds the last durable value, so a
node evicted and reloaded reports an older `ctime`; both eviction guards require `seq <= flushed`, which
a node under active writes fails, so the reload is not the path, and the path was not identified.
A critic mutant moving the clock reading inside the node write lock, so two writers cannot install
out-of-order values, then ran 12 and 20 more times without reproducing it, against 20 clean runs of the
unmodified head in the same batch.

**Current claim:** this is a real observation with a plausible mechanism, an unconfirmed cause, and a
rate that cannot be stated, because the reviewer records one occurrence and the report's later counts are
ambiguous about the denominator.
It is a pre-existing cache-level non-monotonicity that this change makes visible in the stored value
rather than causes; at the base the same regression was overwritten by the later batch time.

No frequency, rate, or rate-of-occurrence is claimed, and no causality is claimed.
This change does not attempt to fix it, and no production race fix is in these owned paths.
One further epoch-bound clarification sample with a raw log audit is being taken separately and is not
part of this erratum, so no number here should be read as final.

## Actual coverage, derived from the review rather than guessed

| scope | result the reviewer recorded |
| --- | --- |
| `cowfs-meta`, all suites | 12 suites, 87 passed, 0 failed, 2 ignored, exit 0 |
| `cowfs-core`, the delivery's named targets | 18 targets, 126 passed, 0 failed, 1 ignored, all exit 0 |
| `cowfs-core`, the targets it does not name | 167 passed, 0 failed, 8 ignored, all exit 0, `conformance` 132 of them |
| lock audit row | `critic2b` 27 pass exit 0 with the row, 26 pass 1 fail exit 101 without it |
| CI at the reviewed head | 3 check runs, all completed and successful |
| merge into current `main` | clean, tree `ffdfbb5f…`, no conflict |

The reviewer's private probe cases, which is where the coverage beyond the delivery's own fixtures comes
from, and the counts taken from its report rather than inferred:
a 6-case mixed-batch probe, a 3-case rename and replay probe, a 3-case eviction probe, a 3-case
concurrency probe, a 4-case `set_now` scope probe, and a 2-case created-fields probe.
That is 21 private cases in total, 18 of them failing at the base and passing at the head, with the
concurrency probe's base comparison recorded separately as the 6-of-6 late direction.

The author recorded 15 independent probes for the contract plus the 4 delivery fixtures.
The review recorded the contract verified on 15 independent probes plus the 4 delivery fixtures, with
its own additional cases on top.
These are different sets and neither number supersedes the other.

Nothing here is a workspace sweep.
No full-workspace run was performed by this change, and the 27 `cowfs-core` targets and 12 `cowfs-meta`
suites above are the reviewer's scoped numbers under pre-existing coverage, not a claim that the whole
repository is covered.

## What this change ran

Documentation and one test, on a fresh private archive of the reviewed head with an isolated
`CARGO_TARGET_DIR` and a private `TMPDIR`.
643 of 645 tracked files were compared by blob against `cde5930` and are byte-identical; the only two
that differ are the two this change edits.

| check | result |
| --- | --- |
| the added create-path test, `--exact`, one test | 1 passed, 5 filtered out, exit 0 |
| `cargo test --locked -p cowfs-meta --test operation_time` | 6 passed, 0 failed, exit 0 |
| `cargo test --locked -p cowfs-core --test operation_time` | 4 passed, 0 failed, exit 0 |
| `cargo test --locked -p cowfs-meta --lib` | 16 passed, 0 failed, exit 0 |
| `cargo fmt --all -- --check` | exit 0 |
| `cargo clippy --locked -p cowfs-meta --all-targets -- -D warnings` | exit 0, zero diagnostics |

No full workspace run, no performance or timing acceptance, no stress suite.

The production change is a doc comment.
It moves no clock value and changes no behaviour, and the existing fixtures are what show the
behaviour is unchanged.
The new test exists because the corrected comment makes a claim the five existing tests did not assert:
that a created inode takes `atime`, `mtime` and `ctime` from the stamp.

## Merge gate

The merge of this branch is held pending one epoch-bound clarification sample with a raw log audit of
the concurrent observation above, even though CI was green at the reviewed head.
Green CI at `cde5930` does not carry to a new head, so a fresh exact-head snapshot is required after
this change and the three conclusions must be re-read.

#42 stays open.
The rename half, the hole flag and the reservation work are separate and are not addressed by this
erratum.
Nothing here closes any part of #42.