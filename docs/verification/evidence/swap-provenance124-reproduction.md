# swap-provenance124: swap reproduces the stale base provenance at runtime, on both backends

Lane: `.treehouse-ready-wave/.treehouse/cowfs-7c1bf8/5/cowfs`, READY5.
Base under test: merged `main` at `2c219a1cb6284d2bb1381145b118bfcac39800b2`.
Host: Apple M3 Max, macOS 26 aarch64, `rustc 1.99.0` / `cargo 1.99.0`.
Method: a fresh `git archive` of `2c219a1` into a throwaway tree, plus two narrow temporary probe files inside that tree only.
No production file was edited, no lease was returned, acquired, reset, stashed, pruned or destroyed, and the leased branch was not checked out or changed.

## Verdict

Reproduced at runtime, on the public API, on both backends.

The `source-traced` claim in issue #124 is now a forced end-to-end reproduction.
A base whose tree is replaced by `Snapshots::swap` keeps the provenance record of the tree it replaced, and a caller that reconnects is told that stale commit.
The record survives a store reopen, so this is not an in-memory artefact.

Also established, and the more actionable half: the retention is unconditional, so it happens even when the new source snapshot carries its own correct provenance that could be copied instead, and it does not happen at all when the base never had provenance.

No production repair was implemented and no issue was created, per the assignment.
The minimal outcome contract is proposed at the end for the coordinator to scope.

## The public call path, named exactly

Everything below is a public API call.
No record file was written, read or edited by the probe, and no lower-level `base_meta` function was called.

| Step | Public call |
| --- | --- |
| open the store | `CoreBackend::open(store, cowfs_core::Options::default())`, `PathBackend::open(store)` |
| namespace operations | `cowfs_daemon::Snapshots`: `create`, `promote`, `set_base_meta`, `swap`, `create_meta`, `list` |
| real tree content | `cowfs_daemon::Backend::snapshot(name) -> Arc<dyn cowfs_vfs::Vfs>`, then `Vfs::create` / `open` / `write` / `fsync` / `release` / `read` / `lookup` |
| provenance value | `cowfs_ctl::BaseMeta { repo, git_ref, commit }` |
| readback after restart | drop the backend, reopen the same store path, `create_meta` again |

`swap(name, from)` is the trait method behind the `snapshot_reset {name, from}` wire request, reached through `cowfs_ctl::server` at `server.rs:983` and `cowfs_daemon::handler` at `handler.rs:285`.

## Core backend, the primary counterexample

Setup: two source snapshots with genuinely different content, a base cloned from the first and carrying its full provenance, then one public `swap`.

```text
PROBE CORE before: name=base parent=Some("srcA") repo=/repo ref=refs/heads/main commit=commit-AAA
PROBE CORE before_tree=AAAA-from-srcA|ino=3298534883330
PROBE CORE srcB_tree=BBBB-from-srcB|ino=9223374235878031362
PROBE CORE swap response: name=base parent=Some("srcB") repo=/repo ref=refs/heads/main commit=commit-AAA
PROBE CORE after readback: name=base parent=None repo=/repo ref=refs/heads/main commit=commit-AAA
PROBE CORE after_tree=BBBB-from-srcB|ino=5497558138883
PROBE CORE after reopen: name=base parent=None repo=/repo ref=refs/heads/main commit=commit-AAA
PROBE CORE reopen_tree=BBBB-from-srcB|ino=5497558138883
PROBE CORE VERDICT tree_changed=true tree_is_srcB=true retained_commit=Some("commit-AAA") srcA_commit=Some("commit-AAA") reopened_retained_still_AAA=true
```

Every element the issue asked for is present and is real measured output.

The tree genuinely changed: `AAAA-from-srcA` ino `3298534883330` became `BBBB-from-srcB` ino `5497558138883`.
The swap's own response already reports the stale commit, so even the immediate caller is misled.
`parent` correctly becomes `srcB`, which shows the code knows the tree came from `srcB` while the record still describes `srcA`.
After dropping and reopening the same store, the tree is still srcB's and the record is still `commit-AAA`.

The retained value is the complete old record, not just the commit: `repo=/repo` and `ref=refs/heads/main` are srcA's.

### Do-nothing control

Identical setup with no swap at all:

```text
PROBE CORE control (no swap): name=base parent=Some("srcA") repo=/repo ref=refs/heads/main commit=commit-AAA
PROBE CORE CONTROL base_tree=AAAA-from-srcA|ino=3298534883330 srcB_tree=BBBB-from-srcB|ino=9223474235878031362 distinct=true retained_commit=Some("commit-AAA")
```

`distinct=true`, so the fixture can express two different trees, and with no swap the base keeps its own commit.
The counterexample is therefore about the swap, not about a fixture that cannot tell two trees apart.

## Path backend, same defect

```text
PROBE PATH before: name=base parent=None repo=/repo ref=refs/heads/main commit=commit-AAA
PROBE PATH before_tree=AAAA-from-srcA|ino=2
PROBE PATH swap response: name=base parent=Some("srcB") repo=/repo ref=refs/heads/main commit=commit-AAA
PROBE PATH after readback: name=base parent=None repo=/repo ref=refs/heads/main commit=commit-AAA
PROBE PATH after_tree=BBBB-from-srcB|ino=2
PROBE PATH after reopen: name=base parent=None repo=/repo ref=refs/heads/main commit=commit-AAA
PROBE PATH reopen_tree=BBBB-from-srcB|ino=2
PROBE PATH VERDICT tree_changed=true tree_is_srcB=true retained_commit=Some("commit-AAA") STALE=true
```

Same result, so this is not a `cowfs-core` swap quirk.
Both `Snapshots::swap` implementations reach the tree replacement and leave the record alone.

## The two bounding cases, which narrow the contract

### The new source has its own provenance, and it is discarded

`srcB` was itself a published base with `repo=/repoB`, `ref=refs/heads/dev`, `commit=commit-BBB`:

```text
PROBE INFO srcB_before_swap commit=Some("commit-BBB") repo=Some("/repoB")
PROBE INFO VERDICT tree_is_srcB=true retained_commit=Some("commit-AAA") retained_repo=Some("/repoA") SRCB_COMMIT_DID_NOT_PROPAGATE=false SRCB_REPO_DID_NOT_PROPAGATE=false
```

The correct record was available on the same backend, under the name being swapped from, and the swap discarded it in favour of the replaced tree's record.
This matters for the contract: "copy the source's provenance when it has one" is implementable today with no new information.

### A base with no provenance is unaffected

`promote` alone, with no `set_base_meta`:

```text
PROBE NOPROV before_commit=None after_commit=None after_tree=BBBB-from-srcB
PROBE NOPROV VERDICT commit_unknown_after_swap=true (a stale commit would be Some)
```

Nothing stale is invented, because there was nothing to retain.
The defect is strictly a retention defect, never a fabrication.

### Both shapes, path backend

```text
PROBE PATH SHAPES withprov_tree=BBBB-from-srcB commit=Some("commit-AAA") noprov_tree=BBBB-from-srcB commit=None
PROBE PATH SHAPES VERDICT stale_on_withprov=true unknown_on_noprov=true
```

## Root cause, at the causal boundary

Read in `crates/cowfs-daemon/src/backend.rs` at `2c219a1`.

`CoreSnapshots::swap` at line 685 and `PathSnapshots::swap` at line 942 both end with a call to `self.info(name)`, and `info` only reads.
Neither takes the `bases.exclusive` section that the other namespace mutations take.
The tree replacement is therefore performed with no record transaction at all, and the record under `name` is simply whatever it was.

The contrast with the methods that do this correctly is in the same file and is the reason this is a defect rather than a policy choice.

- `create` at line 654 calls `crate::base_meta::remove_locked` so a new snapshot cannot inherit an orphaned record.
- `remove` at line 679 removes the record in the same section as the tree.
- `rename` at line 713 calls `rename_locked`, so the record moves with the tree.
- `promote` at line 730 writes `promoted_unknown` when no record exists, and it deliberately does not overwrite an existing record, which is correct for promote and exactly wrong for swap.

`rename` is the precedent that settles the design question: this codebase already treats the base record as something that must travel with the tree it describes.

One detail worth recording because it explains why `base_refresh` is not affected.
`base_refresh` does not call `swap`.
Its `replace` at `import.rs:336` does `remove(name)` then `rename(staging, name)`, and `remove` clears the record, so the refresh path already ends with no stale provenance before `set_base_meta` writes the fresh one.
The defect is reachable through the `swap` trait method and the `snapshot_reset` request, which is what a caller resets a slot with.

The `base_meta.rs` module doc at lines 18 to 20 states the intent directly: the commit is provenance about a build, not a claim about the bytes, and a base whose provenance is missing reports itself stale rather than fresh.
A swap that leaves the old commit in place makes a base whose provenance describes a tree it no longer contains report itself fresh.

## Minimal outcome contract proposal

Not implemented. Scoped for the coordinator.

The correct outcome is that a swap must never leave a record describing a tree that is no longer there.
Three candidate outcomes, in the order I would rank them:

1. Invalidate. After the swap, the target's record becomes `promoted_unknown`, that is `promoted: true` with no `repo`, `git_ref` or `commit`.
   This is the smallest change, it needs no new state and no new error path, and it matches the existing rule that a base with unknown provenance reports itself stale rather than fresh.
   It reuses `write_locked` with `Record::promoted_unknown()`, the exact value `promote` already writes.
   The cost is that a `snapshot_reset` into a base that was previously published loses its commit until the next `base_refresh`, so the slot looks stale after a reset.

2. Adopt the source's provenance when the source has one, and invalidate otherwise.
   The probe shows this is implementable with information already present, since `info(from)` returns `from`'s record.
   This preserves a good record instead of discarding it, and is what a caller resetting a slot from a warm base would most likely want.
   The open question is whether a snapshot may be a base at all in the intended protocol, because `promote` is what marks one, and whether the source's `repo` and `git_ref` are meaningful for the target.

3. Refuse. A swap into a name that has a base record could be refused unless the caller supplies new provenance.
   This is the strongest guarantee and the most disruptive, and it would need a new request shape because `snapshot_reset` currently carries only `{name, from}`.
   It also interacts with the existing crash-safe staged swap: refusing has to happen before the victim is removed, which the existing step ordering already supports.

My recommendation is option 1 now, with option 2 as a follow-up once the protocol question about whether a swapped-from snapshot is a base is settled.
Option 1 is the honest floor: a base that cannot describe its own contents must say so rather than claim a commit that produced different bytes.

Which public API should carry it: `Snapshots::swap` on both implementations, inside the `bases.exclusive` section that `create`, `remove`, `rename` and `promote` already use, so the record change and the tree change are one step for a reader exactly as they are for those four.
No new public API is needed for option 1.
The transaction should follow `rename`'s shape, including a rollback of the record if the tree change fails.

## Commands and real exits

All cargo work ran inside one 600 second foreground `mac-heavy.lock` hold, one flock retry budget, with an isolated `CARGO_TARGET_DIR` and a project-local `TMPDIR`.
Source receipts were printed before each command.

| Command | Selection | Result | Exit |
| --- | --- | --- | --- |
| `cargo test --manifest-path <archive>/Cargo.toml -p cowfs-daemon --locked --test swap_provenance_124_probe -- probe_core_swap_retains --nocapture --test-threads=1` | the one representative sample | 1 passed, 0.36s | 0 |
| same, no filter | all three probes in that file | 3 passed, 0 failed, 0.54s | 0 |
| `cargo test --manifest-path <archive>/Cargo.toml -p cowfs-daemon --locked --test swap_provenance_124_delta -- --nocapture --test-threads=1` | the two bounding cases | 3 passed, 0 failed, 0.72s | 0 |
| `cargo test --manifest-path <archive>/Cargo.toml -p cowfs-daemon --locked --lib` | existing daemon unit tests, untouched production | 100 passed, 0 failed, 9.44s | 0 |

Two compile failures preceded the passing runs and are recorded because they are part of the honest history of this probe.
The first was `?` on `cowfs_vfs::Error` into `io::Error`, 10 errors, because `cowfs-vfs::Error` has no `From` into `io::Error`.
The second was an unused format argument in the verdict line.
Both were in the probe only, both are fixed in the versions whose receipts are listed below, and neither touched production.

The `--lib` run matters for one reason: it proves the existing 100 daemon unit tests pass against the same archive whose two new probe files are present, so the probe files did not disturb the crate.

## Source receipts and scope containment

| Item | sha256 |
| --- | --- |
| `2c219a1` `crates/cowfs-daemon/src/backend.rs`, unchanged in the archive | `53892a6ecd60528527e96ba8df9b05e1da5757ea71198be836a63b0a5064a976` |
| `2c219a1` `crates/cowfs-daemon/src/base_meta.rs`, unchanged in the archive | `4a8eac20294e6d5785a0fb34776731cfab077cdb40685e6bd80255813c1b4b92` |
| probe 1, `tests/swap_provenance_124_probe.rs` | `bd69ae7818538e4892d343ad08099b343f93260b608b484b8adc0249165141f3` |
| probe 2, `tests/swap_provenance_124_delta.rs` | `a020dea56a33d6c4ac14311910714d8b6600f7caca5a3e24f384c1926b0fcff5` |

Containment was proved by extraction, not by assertion.
A second clean `git archive` of `2c219a1` was unpacked beside the working archive and `diff -rq` reported only the two new probe files and nothing else:

```text
Only in .../src/crates/cowfs-daemon/tests: swap_provenance_124_delta.rs
Only in .../src/crates/cowfs-daemon/tests: swap_provenance_124_probe.rs
```

The archive holds 639 tracked files on disk, matching `git ls-tree -r` for the same commit.

Both probe files and every run log are preserved under `bench/out/swap-provenance124/`: `probe-log/` holds the two probes, and `smoke-core.log`, `probe-all.log`, `probe-delta.log` and `lib.log` hold the run output.

## What is not claimed

- No root cause is claimed inside `cowfs-core`'s staged swap. That mechanism is sound and is already covered by `promote_base_survives_a_failure_at_every_step`.
- No claim about how often this happens in practice, and no frequency or timing measurement was taken.
- No claim of a deadlock. The locks are taken sequentially and no lock-ordering fault was observed, consistent with the issue's own caution.
- The namespace atomicity repair from PR #92 is separate and is neither relied on nor re-verified here.
- `snapshot_reset` was exercised through the `Snapshots::swap` trait method, which is what the handler at `handler.rs:285` and the wire path at `server.rs:983` call.
  The wire request itself was not driven over a socket in this lane, so the claim is about the backend contract, not about a live socket round trip.
- No production repair, no commit, no push and no new issue.

## Resource and safety record

- Free disk was `324.0 GiB` before the runs and `323.3 GiB` after, against a `20 GiB` floor
- Artifacts under this lane are `906.0 MiB`, against an `8 GiB` cap, of which `897.5 MiB` is the isolated cargo target and `8.5 MiB` is the archive
- The leased worktree is untouched: still `4dbbd992b912589e13e3841858de09079f1d60f6` on `fix/metadata-health40-fixture-lifetime`, with `git status --porcelain` empty
- No checkout or branch change was performed in the lease, and no existing source was modified for this diagnostic
- No daemon, mount, socket, store or Linux host was touched, no signal was sent, and no shared state was written
- Every store was a private `tempfile` directory inside this lane's artifact tree
- Nothing irreversible was done, so there was nothing to verify before one
- All 32 held leases, the shared `LOCAL` server and the Linux hosts were left alone

## Recommended next step

The coordinator scopes the repair against the contract above.
If option 1 is chosen, the regression belongs next to the existing base-provenance tests in `backend.rs`, asserting that after a swap the target reports `promoted_unknown` or the source's own record and never the replaced tree's commit, on both backends, with a reopen in the assertion so durability is covered.
That is a production change and belongs in its own lane with its own review; this lane stops here.