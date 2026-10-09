# Review: PR 92 Core duplicate-create repair at c3bafb7b, against 6929e63

Reviewer: native critic 14, worktree `.treehouse/cowfs-7c1bf8/14/cowfs`, branch `review/linux-namespaces-17`, lease `0f3d1e5092d0bbec0544cd33ba163168`.

Subject: `c3bafb7b86358e45aa744f53082ff32e6fe26008` (`docs(verification): the create repair, its old fail, and its public proof`), code commit `5b23655` (`fix(daemon): a refused duplicate create must cost the caller nothing`).
PR 92's head is exactly this SHA, state OPEN, `closingIssuesReferences` empty.

This is the canonical copy, in the main repository's primary checkout.
A byte-identical mirror sits in the leased worktree.

Preserved, with the hashes I checked on arrival:
`linux-namespaces17-metadata-repair-final.md` `043f1171385db9bfdbe7fccad4e7c236e6cf74136ee8c17d8dcede4d47a168c8`, in the lease and byte-identical in primary.
`linux-namespaces17-publication-final.md` `2aeeca9ff657db950853e57c7e608716c0d96fdd55b49b0b36a4db9ddb12de8b`, in both.
`linux-namespaces17-refresh-repair.md` `ce3b2e1178ae57c143447cb2ff0eed50b638bf54760aa1b682cc5f8d177eac7b`, `linux-namespaces17-integration-final.md` `2a20e526978e74a905e9d6ddffddb80b5bfdc153b5923c6be01cd274b754d9e3`, and `linux-namespaces17-helper-final.md` `513d9c3f3d7d0db6f1ed2fa35b9450fddbc41fdae29792d827aa4ff032a5b430`, in the lease.
Remote private areas `cowfs-ns17crit`, `cowfs-ns17-int`, `cowfs-pub` and `cowfs-p98` are all verified present and unchanged where they should be, and the round-1 artifact is still `82f5af53d1dd8f569284f481c3f29b5ee3972d3fc422e737518015dea48db2d4`.

Remote placement: my own root `/home/moonscape/cowfs-p98`, in a new immutable attempt `a1005T021209-1187050`.
Not the ready-wave pool, which this critic is forbidden from writing to, and not the builder's `int5` area, which I read only if at all.
The ready-wave pool was never written to by me; it now holds 7.1 GiB from other workers.
Its `task-g4` and `task-g5` daemons, the host's two cowfs mounts and three FUSE connections were left untouched.
Every heavy step ran as one foreground command through `/home/moonscape/cowfs-ready-wave/linux-heavy.lock` with one 600-second bounded wait.

## Verdicts

**The Core duplicate-create defect: FIXED, and verified on a real daemon with a real FUSE mount on both backends.**

A refused duplicate create now costs the caller nothing: the record's bytes, the snapshot's bytes and the listing are all unchanged, live and again from a daemon that never saw the refusal, with every provenance field populated.
This was the only merge-blocking defect in my previous report.

**The orphan guard: still intact on both backends, with a complete record.**

A record carrying a real repository path, ref and commit, with no snapshot behind it, is cleared by the next create of that name and never adopted.

**The symlinked-record-directory refusal: now explicit, and it does not touch the foreign directory.**

`remove_locked` calls `no_symlinked_dir` before `remove_dir_all`, so the delete path says what it will not do instead of relying on `remove_dir_all`'s own symlink behaviour.

**The socket guard now measures bytes.**

`printf %s "$sock" | wc -c` instead of `${#sock}`, with a comment saying why, and a pipe rather than a here-string.

**The removal-durability wording is now truthful, and no durability scope was taken on.**

The doc comment no longer says "durably", and states plainly that the path fsyncs nothing after the unlink, that a crash can leave a record on disk that the live map has dropped, and that nothing depends on a removal surviving a power loss.

**One residual finding, disclosed rather than hidden, which I am not re-blocking: the pre-check is not atomic with the record clear.**

Nothing serialises `name_taken` against `bases.remove`, so a concurrent interleaving can still delete a live base's record.
It is reachable by construction, it is low severity, and the source and the evidence document both say so plainly.
Details and my judgement are in Part 4.

**ETXTBSY: still a separate merge BLOCK, carried.**

Not re-run, not retried, not ignored, not serialised, no new spike, no kernel cause claimed.

Source verdict and merge verdict stay separate: the create repair is correct and I would merge it as it stands, and the branch is not merge-ready while the flake and the red workspace clippy gate remain.

## Binding

`git archive` of the exact head into my private attempt, five files matched:

| File | sha256 |
|---|---|
| `crates/cowfs-daemon/src/backend.rs` | `a35dd3ced5e47a9fa10d181b49806eda83170e04f0b5898a3526338c64f5abf2` |
| `crates/cowfs-daemon/src/base_meta.rs` | `cb8b99b559637a1e347073a003f1e64ef643c46049f19ec4498b27dfa464548f` |
| `scripts/namespaces17-treehouse-linux.sh` | `123b3a0f275607bea89440e72281ffbc4a6adce81b2dd67c92cd8369cf04b552` |
| `docs/verification/base-provenance98.md` | `f3e84866f84ee0ae2cdff6ec042573f3f96016c3d7ce5f49774dbcd0013185f1` |
| `docs/verification/evidence/base-provenance98-create-repair.md` | `a973b68b09d3fe9a677febca8c106acfe19feea0d07171f42304956f7626f513` |

The canonical `docs/verification/base-provenance98.md` in the main checkout hashes to `f3e84866f84ee0ae2cdff6ec042573f3f96016c3d7ce5f49774dbcd0013185f1`, exactly the blob at this head, so the published document and the reviewed document are the same bytes.

The delta is five files, and only two are production source:

```
crates/cowfs-daemon/src/backend.rs                 (production)
crates/cowfs-daemon/src/base_meta.rs               (production)
docs/verification/base-provenance98.md             (documentation)
docs/verification/evidence/base-provenance98-create-repair.md  (new, documentation)
scripts/namespaces17-treehouse-linux.sh            (test harness)
```

`cowfs-meta` appears in zero of them, which matters for Part 6.

Host: `moonscape`, `moonscape@192.168.68.119`, Debian aarch64, kernel `6.12.109+rpt-rpi-2712`, uid 1000, git 2.39.5, rustc 1.95.0, cargo 1.95.0, clippy 0.1.95.
The builder measured on macOS with rustc 1.99.0 and clippy 0.1.99.
Both are stated wherever the difference matters and neither is adjusted to the other.

I read `docs/design.md` from the main checkout. Nothing in it contradicts this change: it settles redb for metadata, crash-consistent metadata transactions, atomic `rename`, `snapshot create/rm` and `base refresh` as the command surface, and mode (b) as snapshot-native.
The one relevant line is that a snapshot is "a writable, O(1) metadata copy", which is why a Core snapshot is not a directory in the store, which is the mistake I made twice and which my own assertions caught.

## Part 1: the refused duplicate create, both backends

`CoreSnapshots::create`, `backend.rs:591-614`, now opens with the same check the Path backend has:

```rust
if self.with(|c| Ok(CoreSnapshots::names(c)?.iter().any(|e| e.name == name)))? {
    return Err(name_taken());
}
// ... only then:
self.bases.remove(name)?;
```

Both backends now build the refusal from one `name_taken()` helper, so a duplicate reads identically whichever one serves the store.

Measured, Core backend, real daemon, real FUSE mount:

```
snapshot create warm EXIT=0
snapshot promote warm EXIT=0 (warm[base])
main.rs through the mount: f354d5025d513f43851c3d9d143a02803099151e0cf5095afbf9e0b4204ddf38
warm IS in the listing, asked of the API
and the mount really serves bytes
```

Then the daemon was stopped and the complete record planted, all four fields populated:

```
{
  "repo": "/home/moonscape/cowfs-p98/attempts/fixture-repo",
  "git_ref": "main",
  "commit": "5b2365509abcdef01234567890abcdef01234567",
  "promoted": true
}
record sha256 BEFORE: 7c1c4e7897b2c0527e0dd73a15d8d5eec318812ba8759058cfb32bd6ad750d76
every field populated: 4 of 4
```

Setup honesty, stated plainly: there is no public verb that publishes a commit on either backend, so the provenance was written straight into the store while the daemon was stopped.
Every call after that is a public API call.
I am not claiming public Core ingest, and `base_refresh` still refuses directory ingest on Core by design.

Then, on a fresh daemon, public API only:

```
snapshot create warm EXIT=1
  cowfs: cannot create snapshot "warm": name is taken
```

| Check | Core | Path |
|---|---|---|
| record bytes, sha256, no normalisation | unchanged `7c1c4e78…` | unchanged `7c1c4e78…` |
| snapshot bytes, read through the mount | unchanged | unchanged |
| listing, `created_unix_ms` removed | unchanged | unchanged |
| commit still reported by the API | yes | yes |
| after a daemon that never saw the refusal: record | unchanged | unchanged |
| after a daemon that never saw the refusal: tree | unchanged | unchanged |
| after a daemon that never saw the refusal: listing | identical | identical |
| existence asked of the API, never of a directory | `"name":"warm"` | `"name":"warm"` |

The Core and Path arms planted the same fixture file, so their record hashes are identical at `7c1c4e7897b2c0527e0dd73a15d8d5eec318812ba8759058cfb32bd6ad750d76`.
That is a fixture cross-check rather than a product result, and it is the evidence that both arms tested the same input rather than two different records.

On the listing normalisation, which I checked rather than accepted: the path backend does not persist a snapshot's creation time and stamps it from the current clock on every read, so that one field cannot be stable for an unchanged snapshot.
Removing only `created_unix_ms` is the minimum needed, and it is a no-op on Core where the timestamp is stable.
The record file and the snapshot's bytes were compared exactly, with no normalisation at all.

## Part 2: the orphan guard, with a complete record

A record for `ghost` carrying the same complete provenance, written into the store with no snapshot behind it, then `snapshot create ghost` through the public API:

```
before the create, is ghost in the listing? 0
snapshot create ghost EXIT=0 (ghost)
ghost IS in the listing, so the create really ran
the record file now: CLEARED
ORPHAN GUARD HELD: the record was cleared by the create, so no commit was adopted
and ghost reports no base either way: 0 occurrences of the planted commit in the listing
```

Identical on Core and on Path, live and after a reopen.
The commit that was never built from anything is not adopted, on either backend.
This is the failure the whole design exists to prevent, and the new `name_taken` pre-check does not weaken it, because an orphan has no snapshot and so passes the pre-check and reaches the clear.

The related new test `a_create_that_cannot_clear_a_stale_record_creates_no_snapshot` covers the other side: when the clear fails, no snapshot is created.
Its own doc comment is careful about what it does not assert, namely that the record survives, because with a read-only metadata root the unlink of the file succeeds and only the directory removal fails, so the store really does lose the record.
That is the branch-A/branch-B split from my previous round, and the comment defers to the two `base_meta` tests that own it rather than pretending otherwise.

## Part 3: the symlinked record directory, now refused explicitly

A record directory replaced with a symlink pointing outside the store, holding a sentinel file and a second file:

```
snapshot rm EXIT=1
  cowfs: cannot remove "warm": .../.cowfs-base-meta/warm is a symbolic link
the FOREIGN directory still intact: YES
its sentinel content: SENTINEL-MUST-SURVIVE
its other file: present
is the symlink still a symlink: yes
```

So the refusal is explicit, names the path and the reason, and the foreign directory and the link are both untouched.
This closes the caveat I recorded last round, where the delete path relied on `remove_dir_all` not traversing symlinks instead of refusing.

One scoping note so I do not overclaim: in order to plant a symlink at that path I had to remove the real record first, so this gate proves the foreign directory and the link survive a refused delete.
It does not prove the record's own content is unchanged, because at that moment there is no record file behind the link.
The "refused before anything is touched" property is what the sentinel demonstrates.

## Part 4: the pre-check window, my judgement

**What is disclosed, accurately.** The source comment says the check "is a pre-check, not a transaction", that it "narrows the window in which a concurrent create of the same free name could clear a record another request has just published", that it "does not serialise", and that "two concurrent creates of one free name can both pass here, and the loser is rejected by the core afterwards".
The evidence document repeats it and adds that per-name locking at runtime is not claimed and that the conformance-suite lock is a test wrapper.
Nothing is hidden, and the test-wrapper point matches exactly what I found last round.

**The trace, and where the disclosed case ends.** The harmless case is two concurrent creates of one free name: both pass the pre-check, one wins, the loser is rejected by the core, and the record the loser cleared belonged to a name with no snapshot. Nothing of value is lost.

The case that is not the same is this:

1. Thread A's pre-check passes, because no snapshot has the name yet.
2. Thread B creates the name, and promotes it, publishing a live, complete record.
3. Thread A's `bases.remove` runs and deletes B's live record.
4. Thread A's create is refused by the core, and A returns an error.

The end state is a live snapshot that has silently stopped being a base, produced by a call the API correctly refused.
Nothing holds a lock across the pre-check and `bases.remove`; `BaseMetaStore`'s mutex serialises record operations among themselves but is not held across the snapshot-namespace decision, and the production handler takes no per-name lock for `snapshot_create`.
So the window is reachable by construction, and it is the interleaving, not the outcome, that distinguishes it from the disclosed case.

**Why the source's safety argument does not cover it.** The comment reasons that "every failure after the pre-check is safe because of what the pre-check proved", and that the record which went "belonged to a name with no snapshot behind it".
That is sound only if no other thread created the name in between, which is precisely the case that does not hold.
The argument is conditional on the absence of the race it is being used to excuse, so I am not treating it as a proof, and I am explicitly not accepting "all records this step removes are orphans" as one.

**My judgement: low severity, disclosed, not a re-block.** It needs two concurrent operations on one snapshot name; the product is single-user with a per-name command surface; the direction is safe, since a lost record reports "no warm base" and never forges a fresh base; and the two honest disclosures mean nobody downstream is misled about it.
What it would take to close it is per-name serialisation of namespace operations, which is a broader change than this repair and which the brief rules out for me to make.
I would raise it as a follow-up issue rather than hold this head for it.

**I tried to observe it and did not, and I am not claiming that as evidence either way.**
I wrote a bounded probe, 40 iterations of a three-way concurrent create-and-promote of one name, path backend.
It wedged on iteration 1, held the shared resource lane for 36 minutes, and blocked another worker's `cargo test -p cowfs-daemon --lib` that was queued behind it.
I verified pid, argv, start time, store, socket and mount for each of the three processes, shut the daemon down through the `cowfs` CLI, and reaped the script and its runner; a second daemon that the script started in the gap between those steps needed the same treatment.
Nothing of mine was left running or mounted, and the lane was released.
I did not retry, because trading host contention for a sample that could not distinguish "window never fires" from "window too narrow for my probe" is a bad trade.
Reachability here is settled by the absence of a lock, which is certain; the frequency is not settled and I do not pretend otherwise.

## Part 5: the socket guard, and what the two units actually do

`scripts/namespaces17-treehouse-linux.sh:120`:

```sh
sock_len=$(printf %s "$sock" | wc -c | tr -d ' ')
```

`printf %s` emits no newline, so `wc -c` counts the path's bytes exactly, with no off-by-one from a terminator.
The comparison threshold of 100 against the cited 107 stays conservative.
Measured on this host:

```
a path like the one this host would build:  103 bytes
a path with a multibyte character:            48 bytes, where ${#sock} would have counted 3 fewer
```

So the old character count would have undercounted exactly as I said it would, and the new form does not.

## Part 6: counts, toolchain, and the clippy discrepancy

**Linux, rustc 1.95.0, measured, reported as mine.**

| Suite | Result | Exit |
|---|---|---|
| `cargo test -p cowfs-daemon`, all targets | 84 lib passed, 0 main, 5 ignored in `end_to_end`, 0 doc | 0 |
| the four repair tests, 3 iterations each | 12 runs, 12 pass, 0 fail | 0 |
| `cargo test -p cowfs-daemon --lib base_meta::`, 3 iterations | 15 passed, 69 filtered out, every iteration | 0 |
| `cargo test -p cowfs-daemon --lib backend::`, 3 iterations | 21 passed, 63 filtered out, every iteration | 0 |
| `cargo test -p cowfs-treehouse --test canonical`, Linux | `running 13 tests`, 13 passed | 0 |
| `python3 -m unittest discover -s bench -p test_namespaces.py`, Linux | `Ran 17 tests`, `OK (skipped=1)` | 0 |
| `cargo fmt --all --check` | clean | 0 |
| `cargo clippy -p cowfs-treehouse --all-targets -- -D warnings` | clean | 0 |
| `cargo clippy -p cowfs-daemon --all-targets -- -D warnings` | fails in a dependency | 101 |
| `cargo clippy --workspace --all-targets -- -D warnings` | one `collapsible_match` | 101 |

`cowfs-daemon` lib by module on this host, counted from the executed names: `backend` 21, `base_meta` 15, `import` 14, `handler` 14, `exports` 13, `mounts` 3, `holders` 2, `daemon` 2, totalling 84, with no test outside those eight modules.

Against the builder's macOS figure of 85 by module (`backend` 20, `base_meta` 15, `import` 14, `handler` 14, `exports` 13, `mounts` 3, `holders` 3, `daemon` 2, plus one store probe), the totals differ by one and three individual modules differ: `backend` is one higher on Linux, `holders` one lower, and the store probe is absent on Linux.
I did not reconcile those and I am not going to guess; a platform-conditional test in `backend` or `holders` is the obvious candidate and it stays a hypothesis until someone names it.
What is not in dispute is the arithmetic: the builder's per-module figures sum to 84 plus the one store probe, which is where their 85 comes from, and mine sum to exactly 84 with no such category.

The change itself adds three tests on this platform: Linux went from 81 at `6929e63` to 84 here, and `backend` went from 18 to 21, which is the two refused-duplicate tests plus the cannot-clear-stale one.
`base_meta` stayed at 15.
That matches the diff.

**The clippy discrepancy is unresolved and version-dependent, and both observations stand.**
On clippy 0.1.95 here, `--workspace` exits 101 with `collapsible_match` at `crates/cowfs-meta/src/tx.rs:314:21`.
On clippy 0.1.99.0 on the builder's machine, the same command exits 0.
The file is byte-identical at `6929e63` and at this head, sha256 `5cafb0cc2e10f1e2cde879258ba96c6a41308bd8faf06b402298824aeba18688`, `cowfs-meta` is in zero files of this diff, and the shape the lint points at is still present at `tx.rs:313-316`.
So this is a lint that fires on one clippy and not the other, on a file nobody in this branch changed.
That file belongs to the ready-wave worker on the core-metadata lane, and I neither edited it nor will.
It needs one clippy version run over one commit to settle, which is a short job for its owner.

## Part 7: CI, and the state of the branch

One read of the exact head, no polling, no dispatch, no rerun.

```
c3bafb7b check-runs: check (macos-latest) success, check (ubuntu-latest) success, linux-fuse success
total: 3
```

The brief noted the builder saw these pending; by the time I read them once they were all completed and green.
Green proves nothing about ETXTBSY, and I am not inferring a private-Linux isolation proof from a green matrix: my Linux evidence in this report is direct measurement on the real host.
PR 92's head is `c3bafb7b`, the SHA I reviewed, state OPEN, with `closingIssuesReferences` empty, so nothing in the branch proposes closing an issue.
Issue 17 stays open, and Refs 17, 97 and 98 are referenced rather than closed.
I found no negated closing phrasing such as "closes" or "fixes" attached to an issue this change does not complete.

## ETXTBSY, carried

Not re-run, not retried, not ignored, not serialised, no wider bound, no new spike, and no kernel cause claimed.
My last measured figures stand and are too small to conclude from: 1 failure in 12 full-binary runs at 8 threads, 0 in 6 at one thread.
The characterisation in the repository is better than mine and I do not dispute it.

I keep one correction on the record, because my previous report contained a misleading literal: there is no explicit `posix_spawn` call in `crates/cowfs-treehouse/src/mode_b.rs`, which builds the helper with `std::process::Command`.
On Linux `Command::spawn` uses `posix_spawn(3)` when it can avoid a fork, so tests and production share a spawn path, but that is a statement about the standard library and not about that source.
The evidence document now makes the same correction.

Status: merge BLOCK, root cause unconfirmed, independent of everything above.

## What is not proven at this head

- No Core warm-base publication. `base_refresh` refuses directory ingest on Core by design; on Core I exercised create, promote, duplicate-create and reopen over a real daemon and a real mount, nothing more.
- No directory-ingest acceptance on Core, not the 15/16 gate, and no mode (b) acceptance.
- No Path publication acceptance re-run. The brief is right that the narrow delta does not need two more full Path builds, and the Path warm-base and two-fresh-slot proof stands from `6929e63` against blobs this change does not touch.
- No transaction safety, and no per-name serialisation, both disclosed rather than claimed.
- The pre-check window's frequency is not measured. Reachability is settled by the absence of a lock; whether it fires in practice is not, and my only probe attempt wedged.
- No rollback-failure measurement, no 200-iteration concurrency batch, no ETXTBSY spike.
- No crash injection and no power-loss measurement. Removal is explicitly not crash-durable and nothing claims it is.
- No `fsck`, no deduplication, no build-time benefit, nothing about btrfs, XFS, other kernels or other architectures.
- No macOS run by me. Every Linux figure here is Linux on rustc 1.95.0; every macOS figure is the builder's, attributed as such.
- shellcheck is not installed on moonscape and I was not permitted to install it, so no verdict from it. `sh -n` and `dash -n` pass on all four of my probe scripts and on both project scripts.
- No browser or rendering validation of any kind, so no claim about any UI. Any document claim I checked was checked structurally, against the source and the blob hashes.

## Minimal defects, no fixes applied

No source or test edits, no commit, no push, no merge, no rebase, no force, no lease return.

1. **The pre-check window**, `backend.rs:591-614`. Worth a follow-up issue: serialise namespace operations per name, or make `create` re-check after clearing. Not a hold on this head, for the reasons in Part 4.
2. **The clippy version discrepancy**, `crates/cowfs-meta/src/tx.rs:313`. Owned by the core-metadata lane, not this branch, not edited, not waived. Settle it with one clippy version over one commit.
3. **ETXTBSY**, not fixed here by instruction. Still a merge BLOCK on its own.
4. **Keep issue 17 open.** The create repair is real and verified, and it is not the mode (b) warm-base acceptance: Core publication, the flake, and the red workspace gate all remain.
5. Nothing in the canonical `base-provenance98.md` or the create-repair evidence document is contradicted by my measurements. Both are honest about what they did not run, the pre-check is not a transaction, the conformance lock is a test wrapper, the removal is not crash-durable, the `posix_spawn` claim is corrected, and the first failing run of the public proof is kept as evidence rather than deleted.

## Reproduction

Mac, leased worktree:

```sh
cd /Users/zeeshanhaque/Projects/cowfs/.treehouse-build-train/.treehouse/cowfs-7c1bf8/14/cowfs
git rev-parse HEAD                                   # c3bafb7b86358e45aa744f53082ff32e6fe26008
git diff --stat 6929e63..HEAD
```

Linux, my own attempt, every heavy step through the lane:

```sh
git archive --format=tar c3bafb7b > src-c3bafb7b.tar
scp src-c3bafb7b.tar moonscape@192.168.68.119:/home/moonscape/cowfs-p98/
ssh moonscape@192.168.68.119 'A=/home/moonscape/cowfs-p98/attempts/a<stamp>-<pid>
  mkdir -p "$A/repo" && cd "$A/repo" && tar xf /home/moonscape/cowfs-p98/src-c3bafb7b.tar
  sha256sum crates/cowfs-daemon/src/backend.rs crates/cowfs-daemon/src/base_meta.rs
  cargo build -p cowfs-daemon -p cowfs-cli        # the CLI is cowfs-cli, not cowfs-daemon
  # create and promote through the public API, write bytes through the mount,
  # stop the daemon, plant the complete record, restart, then duplicate-create'
```

Expected, and what I measured: `cannot create snapshot "warm": name is taken`, exit 1, with the record's sha256, the snapshot's bytes and the listing all unchanged, on both backends, and again from a daemon that never saw the refusal.

My probes and evidence, all under the lease at `bench/out/p98-create-critic/`, which is gitignored:

```
src-c3bafb7b.tar     the exact tree I reviewed
lockrun.sh           one bounded 600s foreground wait on the shared lane, then exec
g1-g3.sh             G1 both backends, G2 the orphan guard, G3 the symlinked record dir
g1c-only.sh          G1 on Core with the snapshot's bytes read through the mount
g1c-g4.sh            the version that also carried the concurrency probe, kept for the record
final.sh             the four repair tests, module counts, fmt, clippy, the socket guard

evidence-g1-g3.txt            G1 core aborted for the right reason, G1 path full pass,
                              G2 both backends, G3 the refusal with the foreign dir intact
evidence-g1c.txt              G1 core full pass, plus the identical-record cross-check
evidence-final.txt            84 lib by module, the four tests 3x, fmt 0, clippy 101, wc -c
evidence-DISCARDED-vacuous-cli127.txt   my discarded first run, kept because it is the
                              evidence that an "unchanged" assertion passes on nothing
```

Cleanup: no process of mine and no mount of mine remain.
Both the attempt's 1.9 GiB cargo target and the concurrency probe's leftover stores were removed after showing their contents, case-guarded to my own attempt path.
The attempt is preserved at 7.5 MiB with every log, every store and both fingerprint files from round 4 intact.
All four older private areas are verified present: `cowfs-pub` 208 MiB, `cowfs-ns17-int` 91 MiB, `cowfs-ns17crit` 31 MiB, `cowfs-p98` 104 MiB, with the round-1 artifact still `82f5af53…`.
No borrowed store, mount or build cache was deleted, and the builder's `int5` area was never written to.
Mac-side owned growth is dominated by the source archive at a few MiB, far inside the 8 GiB allowance.

Five mistakes of my own this round, reported rather than buried.

I created the attempt directory before the existence guard that was supposed to refuse a collision, so the guard fired on my own directory.
I then ran the whole gate suite with a `cowfs` binary that did not exist, because I built `-p cowfs-daemon` and `cowfs` is built by `-p cowfs-cli`.
Every assertion in that script was an "unchanged" comparison, and an unchanged comparison is vacuously true when nothing ran, so it produced thirteen false passes and would have let me report a repair I had not tested.
I caught it because the listings were empty, and I threw the run away rather than reading the favourable lines, kept it under the name `evidence-DISCARDED-vacuous-cli127.txt`, and added a fail-fast binary precondition plus a positive existence assertion to every gate so the failure cannot recur.
I then read the Core snapshot's bytes through `$STORE/warm/main.rs`, which is meaningless on Core because a Core snapshot is not a directory in the store.
That is the same class of error I made two rounds ago, and this time my own assertion caught it and aborted the arm instead of letting it pass.
The concurrency probe wedged, held the shared lane for 36 minutes and blocked another worker's queued `cargo test`, which is a direct cost to someone else and not just my own lost time.
And one remote script called `git` inside a `git archive` extraction that has no `.git`, so its diff section printed usage text; I re-measured those facts locally where git exists.

Every signal I sent was to a single pid whose argv, start time, store, socket and mount I had just verified, never to a group and never with `pkill`, and every daemon was stopped through the `cowfs` CLI's `shutdown` subcommand rather than a flag on the daemon binary.