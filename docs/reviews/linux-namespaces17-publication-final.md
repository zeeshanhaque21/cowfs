# Review: PR 92 durable base publication at f1529cc, against e243cb1

Reviewer: native critic 14, worktree `.treehouse/cowfs-7c1bf8/14/cowfs`, branch `review/linux-namespaces-17`, lease `0f3d1e5092d0bbec0544cd33ba163168`.

Subject: `f1529cc1885b0ee6e9e5ab7eb2cbfe62cc9605cc` (`spike(treehouse): characterise the ETXTBSY flake instead of guessing again`), PR 92.
PR 92's head is exactly this SHA.

All three earlier reports are preserved unchanged with their Linux fixtures: `docs/reviews/linux-namespaces17-helper-final.md` at `375b005`, `docs/reviews/linux-namespaces17-integration-final.md` at `7faac1be`, `docs/reviews/linux-namespaces17-refresh-repair.md` at `e243cb1`.
Remote private areas `/home/moonscape/cowfs-ns17crit` and `/home/moonscape/cowfs-ns17-int` are verified untouched, artifacts included.

Coordination: `docs/ready-wave-dispatch.md` is not in my branch, so I read it from the main checkout, read-only.
It records eleven ready workers under `.treehouse-ready-wave/.treehouse/cowfs-7c1bf8/`, a different pool from my build-train lease, and I did not write to it.
It records 261.22 GiB free, an 8 GiB owned-growth allowance, a 600-second lock wait, and a stop below 20 GiB.
My Mac had 413 GiB free when I finished.
Every heavy Linux step ran as one foreground command through `/home/moonscape/cowfs-ready-wave/linux-heavy.lock` by the documented recipe.

## Verdicts

**Scoped: #98 source and persistence, and the Path-backend published-warm-base integration: PASS.**

The provenance is now written to disk before the report exists, read back rather than echoed, and survives a daemon restart.
I published a warm base myself, in my own store and my own repository, and observed `fresh: true` from a daemon I started, with the recorded commit equal to the repository's real HEAD.
Issue 98 is fixed at this head.

**The failure window, on the Path backend: PASS, fail-closed.**

With the metadata write failing after the physical promote and an existing base present, the refresh reports an error instead of claiming success, the previously recorded commit survives unchanged, the reader reports the base as stale with an accurate reason naming both commits, and no snapshot bytes are lost.

**#98 correctness beyond what I exercised: BLOCK on two specific defects, both in the new store, neither of which produces a false fresh.**

A failed on-disk removal is discarded silently and leaves the live process and a restarted process disagreeing about which bases exist.
A rename that fails after removing the source record loses the provenance entirely.
Neither is a false fresh, so neither breaks the postcondition I verified, but both break the durability the change exists to provide.

**ETXTBSY: still a merge BLOCK, independent of everything above.**

Measured 1 failure in 12 full-binary runs at 8 threads on the actual host.
Unchanged in substance since I first measured it; the new spike characterises it rather than closing it.

Source verdict and merge verdict are separate and both stand: the publication work is correct enough to merge once P-1 and P-2 are addressed, and the branch is not merge-ready while the flake stands.

## Binding

`git archive` of the exact head into a private remote root, seven files matched:

| File | sha256 |
|---|---|
| `crates/cowfs-daemon/src/base_meta.rs` | `f9e723d214740d6c58a738807e79887b22462d3256329980d82f2a45230ed1c0` |
| `crates/cowfs-daemon/src/backend.rs` | `b02c123973949ca3f19631f2655ee5381e4136350892781f868fe53cb53672bd` |
| `crates/cowfs-daemon/src/import.rs` | `a6ab89a4fbc1539ad25d7425fd3d46982f13d1f10cc332c0385c0b2d0bfca0c1` |
| `scripts/namespaces17-treehouse-linux.sh` | `c2c5b918f96f968b8ecb3d6360267c2c5c68cd51dcc281baac5252ddc4b2acdb` |
| `scripts/namespaces17-linux.sh` | `9bea82d4c0d6b494807c572b7a246b0c30692745a8c877ec04726902af093a02` |
| `crates/cowfs-treehouse/tests/canonical.rs` | `98264b70fb0434b444e5280c2a9ab562aae826045d28876a39ea62771132b0f0` |
| `docs/verification/base-provenance98.md` | `0605000d27472d54211f01db5f313e9a569416fdeb432cfd0e6b2feefd4e8393` |

Host: `moonscape`, `moonscape@192.168.68.119`, Debian aarch64, kernel `6.12.109+rpt-rpi-2712`, uid 1000, git 2.39.5, rustc and cargo 1.95.0.
Private root `/home/moonscape/cowfs-pub`, mine, not the ready-wave pool.

## Part 1: #98 is fixed, and I checked it without trusting the harness

### The acceptance script passes, and its postcondition is a real one

The script, unmodified, at this head:

```
VERDICT: PASS: warm base published with durable provenance, surviving a daemon reopen, and two
fresh slots built through cowfs-treehouse at one canonical path over a real cowfs FUSE mount
```

The lines that produced it, from my run:

```
warm base: the control API reports it published and fresh; snapshot repo-67e8ee-base
warm base: the reported commit is 54a2ff22f40ee081b9b2c967820a5363efdb2e44
warm base: one worktree, a clean tree, HEAD still 54a2ff22f40ee081b9b2c967820a5363efdb2e44
warm base: the published base holds the repository's committed source at 7fa626e8...
readback: the reopened daemon reports repo-67e8ee-base at the same commit 54a2ff22...
check: the caller's mounts are unchanged across the canonical builds
check: .../canonical is still empty outside every namespace
check: two fresh slots from one warm base, built at one canonical path, are byte-identical
check: both native controls at their own paths differ from their canonical builds
control: the same base, the same commit, and fresh=false because the repository moved
control: an unrefreshed repository has no commit and is not fresh
```

At `e243cb1` this same script exited 1 at the base postcondition, and I proved the old harness would have printed PASS on identical product behaviour.
That discipline held.

### The record is on disk, outside every snapshot tree

```
store/.cowfs-base-meta/repo-67e8ee-base/base.json
{
  "repo": ".../namespaces17-treehouse/repo",
  "git_ref": "main",
  "commit": "54a2ff22f40ee081b9b2c967820a5363efdb2e44",
  "promoted": true
}
```

It is not inside the seed snapshot, and snapshot listing does not leak it: `snapshot list` returns four snapshots, `repo-67e8ee-base`, `seed`, `slotA`, `slotB`, and no metadata entry.
The published base carries the repository's committed source, so the base is a clone of the ref and not a bag of build output.
Two slots cloned from the published base each hold that base's own `main.rs`, not the seed's.

### My own refresh, my own store, my own repository

The harness asserting its own postcondition is worth less than a driver I wrote, so I drove one.

Before any refresh:

```
base status EXIT=1
{"pool_id":"repo-d42a35","snapshot":"repo-d42a35-base","base_commit":null,
 "head_commit":"54a87343...","fresh":false,"reason":"no warm base repo-d42a35-base for this repository"}
```

The refresh, real exit:

```
refresh EXIT=0
{"pool_id":"repo-d42a35","snapshot":"repo-d42a35-base","git_ref":"main",
 "commit":"54a87343a0c0fbb232a7d6a62e8333cc5d32dea0", ...}
```

After, observed by me:

```
base status EXIT=0
{"pool_id":"repo-d42a35","snapshot":"repo-d42a35-base",
 "base_commit":"54a87343a0c0fbb232a7d6a62e8333cc5d32dea0",
 "head_commit":"54a87343a0c0fbb232a7d6a62e8333cc5d32dea0",
 "fresh":true,"reason":"the base is at 54a87343a0c0fbb232a7d6a62e8333cc5d32dea0"}
```

`recorded == HEAD`: YES, checked against `git rev-parse`, so the record is bound to the actual source and not to anything cached.

Moving the repository forward and refreshing again updated the record to the new commit and `fresh` stayed true, `match: YES`.
That is the update path, not just the create path.

### Reopen, from a daemon that never saw the refresh

Shut the daemon down through its control socket, confirmed the pid was gone, mounted the same store again, and asked again:

```
base status EXIT=0
{"snapshot":"repo-d42a35-base","base_commit":"79bf538b...","head_commit":"79bf538b...","fresh":true}
```

Same base, same commit, from a process with no memory of the refresh.
That is the property the previous head did not have, and it is the property the whole change exists for.

### A bare promote does not clobber provenance

`backend.rs:629` and `:866` both guard with `if self.bases.get(name).is_none()`.
I promoted a different snapshot, `seed`, and the base's record kept its commit:

```
promote seed EXIT=0
records now: base.json under repo-d42a35-base, and a separate one under seed
the base's record is unchanged: YES
```

So the ordering hazard the brief asked about, a late `promote` overwriting a recorded base, does not occur on this path.

## Part 2: the failure window, with an existing base

This is the case the design doc anticipates and I wanted to see rather than assume.
Base `repo-d42a35-base` published at `79bf538b`, repository moved forward to `e23daf29`, then the base's metadata directory made read-only so the temporary file could not be created, then a refresh.

```
refresh EXIT=1
cowfs-ns-run.sh: mount namespace ready (unprivileged), ...
cowfs-treehouse: cowfs: io_error: cannot promote "repo-d42a35-base": Permission denied (os error 13)
```

The refresh errors.
It does not report success, and the harness's own `refresh_ok` would have failed it too.

The record, byte for byte after the failed write:

```
unchanged: YES
still holds the OLD commit, not the new one: YES
any temp file left behind by the failed write? none
```

What the reader says, after a restart:

```
base status EXIT=1
{"base_commit":"79bf538b...","head_commit":"e23daf29...","fresh":false,
 "reason":"the base is at 79bf538b... and main is at e23daf29..., so it is stale"}
```

No false fresh.
The reason names both commits, so an operator can tell a stale base from a missing one.

And no data loss, which I checked directly on the store rather than through the mount:

```
repo-d42a35-base/main.rs -> fn main() { println!("g3"); }
```

The root advanced to commit 3 while the record still described commit 2.
That is exactly the split the doc claims: root and metadata are separate durability domains, not one transaction.
Here the reader resolves the split in the safe direction and labels it.

An earlier probe of mine printed `MISSING: main.rs` at this point.
That was my error, not a defect: I checked the mount point after stopping the daemon, so the mount was empty.
Re-reading the store on disk is what settled it.

### The other half of the window, in the reverse order

Record deleted, base snapshot left in place, which is the state a crash between promote and the write leaves:

```
base status EXIT=1
{"base_commit":null,"head_commit":"dd49967a...","fresh":false,"reason":"no warm base ..."}
```

An un-annotated survivor is never reported as a base.
Correct, and it is the property that makes the split safe.

## Part 3: Core backend

`base_refresh` is refused on Core because directory ingest is unsupported there, so this is deliberately scoped.
What I proved at runtime on `CoreBackend`, using only the public API:

```
snapshot create core-snap   EXIT=0
snapshot promote core-snap  EXIT=0
record: /store/.cowfs-base-meta/core-snap/base.json
        {"repo":null,"git_ref":null,"commit":null,"promoted":true}
snapshot list --json: name='core-snap' base={'commit':None,'git_ref':None,'repo':None}
```

Then shut down, and started a daemon that had never seen the promote:

```
snapshot list --json: name='core-snap' base={'commit':None,'git_ref':None,'repo':None}
record still on disk
```

So the Core backend loads and persists base records durably, and a promoted snapshot with unknown provenance is reported as a base with unknown fields rather than as a non-base.
`base status` correctly claims no commit on Core, since a bare promote carries none.

This is a provenance proof for the Core load and persist path.
It is not directory-ingest acceptance, it is not a warm-base publication on Core, and it is not the 15/16 gate.

## Part 4: findings

### P-1, medium: a failed record removal is discarded, and a restart then resurrects it

`base_meta.rs:190-196`:

```rust
pub(crate) fn remove(&self, name: &str) {
    self.records
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(name);
    let _ = std::fs::remove_dir_all(self.root.join(name));
}
```

The in-memory entry goes unconditionally, and the result of the on-disk removal is thrown away.
`remove` also returns `()`, so no caller can learn that the deletion failed.

Observed consequence, in the injection run: with the base's metadata directory un-removable, the live daemon answered

```
{"base_commit":null,"fresh":false,"reason":"no warm base repo-d42a35-base for this repository"}
```

and a daemon restarted on the byte-identical store answered

```
{"base_commit":"79bf538b...","fresh":false,"reason":"the base is at 79bf538b... and main is at e23daf29..., so it is stale"}
```

Two processes, one store, one record file, two different answers.
The path is `import.rs:361-365`, where `replace` calls `snaps.remove(name)` on an existing base, which reaches `bases.remove`.

Severity is medium rather than high because the divergence is in the safe direction: the live process under-reports, and it never invents a base or a fresh.
The costs are real though.
The live API can lose a base that is durably recorded, so an operator sees "no warm base" and may republish unnecessarily.
A silent `let _ =` on a durability operation is the wrong shape regardless of direction.
And it is the late-write resurrection the brief asked about, in the removal direction: a record the live process considers deleted comes back on restart.

Honest limit on this one: I observed it once, in the injection run, and located the mechanism in code, but my attempt to isolate it separately did not reproduce it.
That attempt removed the base snapshot as well, and once the snapshot is gone the orphan record has nothing to attach to, so both processes correctly answer "no warm base".
The isolation I actually have is the injection run, which is a realistic path.
I am reporting it as observed once with a code-proven mechanism, not as a reproduced-by-construction defect.

Minimal fix shape: make `remove` return `io::Result<()>`, drop the in-memory entry only when the on-disk removal succeeded, or surface the failure to the caller.

### P-2, medium: `rename` is not atomic and loses the record if the second step fails

`base_meta.rs:201-216`:

```rust
let record = self.records.lock()...get(from).cloned();
self.remove(from);
if let Some(record) = record {
    let meta = BaseMeta::from(record);
    self.set(to, &meta)?;
}
```

`remove(from)` deletes first and `set(to)` writes second.
If `set(to)` fails, for any reason, the provenance is gone: from memory by `remove`, and from disk if that removal succeeded.
The snapshot itself survives, so this is not data loss, but the base silently loses the commit that made it a base, and `base status` drops to "no warm base".

The happy path is correct and I measured it: renaming `repo-67e8ee-base` to `repo-67e8ee-renamed` left exactly one record under the new name with the commit preserved and none under the old name, and the CLI showed the provenance inline:

```
repo-67e8ee-renamed [base .../repo @ main 54a2ff22...]
```

Removing the renamed base then removed its record and left the metadata root and the other snapshots untouched.
So only the failure window is the problem, and it is the same window the change was written to close.
Minimal fix shape: write the destination first, then remove the source, so the record is never absent.

### P-3, low: the temporary file name is per process, so two concurrent writes to one base collide

`base_meta.rs:145`:

```rust
let tmp = dir.join(format!("{FILE}.tmp{}", std::process::id()));
```

Every thread in one daemon uses the same temporary path for the same base.
Two concurrent refreshes or promotes targeting one base would create and write the same file, then rename from it, which can interleave or publish a partially written record.
The file operations also happen outside the `records` lock, so the map is not serialised against them either.

I did not reproduce a corruption, and with a single-user, low-concurrency tool the window is narrow, so this is hardening rather than a live defect.
It is worth naming because the brief asked specifically about concurrent same-base operations, and because a unique temporary name is a one-line change.

### P-4, low: the module doc states the wrong path shape

`base_meta.rs:9` says the record is at `<store>/.cowfs-base-meta/<name>.json`.
The measured path is `<store>/.cowfs-base-meta/<name>/base.json`, a directory per base.
Everything else about the placement is as described: outside every snapshot tree, so no contents, hash or Merkle root change and a clone never copies it.

### P-5, low: the metadata root is visible in the mount root, contradicting the same sentence

`base_meta.rs:11-12` claims the file is "invisible to snapshot listing and to the mount root on both backends".
The listing half is true. The mount-root half is not:

```
the mount root, which must not show it either:
    .cowfs-base-meta
    repo-67e8ee-base
    seed
    slotA
    slotB
is .cowfs-base-meta reachable through the mount? YES-LEAK
```

The dot prefix keeps it out of snapshot listing, which is what prevents it colliding with snapshot names, and the design works.
But a tree that lists the cowfs root sees `.cowfs-base-meta`, and it is reachable.

No security consequence in the stated scope: single-user, no encryption, and the contents are a repository path, a ref and a commit, all of which are already visible in the working tree.
The cost is that the doc asserts a stronger property than the code delivers, which is the kind of claim that gets relied on later.

### P-6, low: the acceptance script is still not re-runnable in place

Unchanged from my previous two reports, so this is the third head it has survived.

`scripts/namespaces17-treehouse-linux.sh` clears `repo_dir` at line 188 and never clears `$out`.
The repository is therefore recreated each run, with a brand-new single-commit object store, while the store persists, so the seed import collides:

```
S1  EXIT=0   VERDICT: PASS
S2  EXIT=1   VERDICT: FAIL: the seed import failed
     import.log: cowfs: snapshot "seed" already exists
```

Not a false PASS, so it is low.
It cost me a confusing intermediate result: I tried to reset the fixture repository back to the recorded commit and got

```
fatal: Could not parse object '54a2ff22f40ee081b9b2c967820a5363efdb2e44'
```

because that object no longer existed in the recreated repository.
That also incidentally demonstrated something correct: `base status` on the path now holding a different repository reported the base as stale rather than fresh.

The brief asks for an immutable attempt directory or reuse of a validated existing seed.
An attempt directory would fix this and make the harness safe to run twice, which matters for a script whose whole job is to be run on demand.

### P-7, informational: `BaseMetaStore::remove` does not validate the name, while `set` does

`set` calls `cowfs_ctl::validate_snapshot_name`, which rejects empty, over-long, leading-dot, slash-bearing and control-character names, and its own doc comment notes this "also rules out `.`, `..`, `._*` and `.nfs*`".
`remove` and `rename`'s source name do not.

Not reachable from the control path: `crates/cowfs-ctl/src/server.rs` validates `SnapshotRename.from` and `.to`, `SnapshotPromote.name` and `Import.name` before dispatch, so `..` and any slash-bearing name is rejected upstream.
I am recording it as a hardening inconsistency, not a vulnerability.
It matters only because a future in-crate caller that skips validation would find one method defended and the other not.

### P-8, informational: a pre-fix store and a never-refreshed repository are indistinguishable

With no metadata root at all, the daemon starts and reports:

```
{"base_commit":null,"fresh":false,"reason":"no warm base repo-67e8ee-base for this repository"}
```

That is the same answer a repository that was never refreshed gets, and it is the same answer an un-annotated survivor gets.
This is correct for the reader's purpose, since all three mean "no base you can rely on", and a durable base must never be inferred from absence.
The operational cost is that an operator cannot tell "this store predates the fix" from "nothing has been published here".
The `promoted` field already distinguishes the cases internally; the status response does not surface it.

## Part 5: counts, measured

Each exit captured on its own line, no pipeline in a status position.

| Suite | Result | Exit |
|---|---|---|
| `cargo test -p cowfs-daemon`, all targets | 70 lib passed, 0 main, 5 ignored in `end_to_end`, 0 doc | 0 |
| `cargo test -p cowfs-treehouse --test canonical`, Linux | `running 13 tests`, 13 passed | 0 |
| `python3 -m unittest discover -s bench -p test_namespaces.py`, Linux | `Ran 17 tests`, `OK (skipped=1)` | 0 |
| `cargo fmt --all --check` | clean | 0 |
| `cargo clippy -p cowfs-treehouse --all-targets -- -D warnings` | clean | 0 |
| `cargo clippy -p cowfs-daemon --all-targets -- -D warnings` | fails in a dependency | 101 |
| `cargo clippy -p cowfs-meta --all-targets -- -D warnings` | one `collapsible_match` | 101 |

The clippy failure is `crates/cowfs-meta/src/tx.rs:314`, `collapsible_match` on an `if` nested in a `match` arm.
That file is not in this PR's diff; the PR changes `backend.rs`, `base_meta.rs`, `import.rs`, `lib.rs`, `canonical.rs`, `base-provenance98.md`, the ETXTBSY spike and the harness script.
So it is pre-existing in the tree and not introduced here, and I report it as a tree-level gate that is currently red rather than as a defect of this change.
Anyone reading "clippy is clean" from a scoped run on `cowfs-treehouse` alone should know the workspace-wide answer is currently exit 101.

`cowfs-daemon` lib grew from 54 at `e243cb1` to 70 at this head, so 16 new tests.
By module name I can account for 7 in `base_meta` and 2 in `import`, which went from 12 to 14.
I did not enumerate the remaining 7, so I am not claiming which modules they are in.

The reported lib count has been exactly one higher than what I measure at two consecutive heads: 55 reported against 54 measured at `e243cb1`, and 71 reported against 70 measured now.
Same direction both times, so it looks like a counting convention rather than a missing test, but I report what I measured.

`canonical.rs` cfg breakdown, computed from the source:

```
total #[test]   : 14
linux-only      : 4
not-linux-only  : 1
both platforms : 9
=> Linux runs 13, macOS runs 10
```

Unchanged from the last two reports, and my fourth reporting of the same sentence.
The counts are right; the doc's explanation, "because 3 are behind `cfg(target_os = \"linux\")`", is the net difference rather than the gate count, and four minus one is three.

## Part 6: ETXTBSY, a separate merge BLOCK

Unchanged in substance.
`canonical.rs` still writes the stub through a helper that closes the handle explicitly before the exec, and nothing about the spawn mechanism changed.

My bounded sample on the actual host, 12 full-binary runs at 8 threads:

```
RESULT: 12 runs, pass=11 fail=1
failure rate: 8.3%

run 6: thread 'the_canonical_path_is_never_pasted_into_the_command_string' panicked at
       crates/cowfs-treehouse/tests/canonical.rs:271:42:
       the stub exits 0: Io("cannot run the namespace helper
       /tmp/cowfs-canon-injection-1043068/bin/ns-stub: Text file busy (os error 26)")
```

And 6 runs at 1 thread in the same session: 0 failures.
Six runs is far too few to conclude that serialising fixes it, and the author's own table records 1/150 even fully serialised, so I do not conclude that.

My three measurements of the same code on the same host now read 5/60, 4/24 and 1/12, all under concurrency.
The rate is load-sensitive and the host has other workers on it, so these are not comparable as if they were one experiment, but every one of them is nonzero.

On the brief's question of whether this is fixture-only: `crates/cowfs-treehouse/src/mode_b.rs` builds the helper process with `std::process::Command` at lines 641, 651 and 667.
There is no explicit `posix_spawn` call in the source.
On Linux, Rust's `Command::spawn` uses `posix_spawn(3)` when it can avoid a fork, so the tests exercise the same spawn path production does.
That is not proof the production path is equally affected, and I do not claim it is, but "the fixture does something production does not" is not a supportable dismissal here.

This is associational measurement.
I have no kernel-level explanation and I do not offer one.
The new spike in this PR is the right shape of work for that, and it is correctly labelled in the tree as characterisation rather than a confirmed root cause.
A green run at this head is not evidence the flake is gone, and the brief notes a prior run at this source failed on it, which I did not independently re-read because CI verification was scoped to one read of the exact head.

## What is not proven at this head

- No Core-backend warm-base publication. `base_refresh` is refused on Core by design; only the Core load and persist path is proven.
- No real ingest acceptance on Core, and not the 15/16 gate.
- No performance measurement of any kind.
- No dedup or warm-base benefit. The path backend does not deduplicate, and `fsck` correctly refuses because there is no block store.
- No crash injection. The G2 failure window is an injected permission error, not a power loss, so the durability claim rests on fsync of the file and its directory rather than on an observed crash.
- No atomicity claim for the root-plus-metadata pair. They are separate domains by design and I measured them disagreeing safely, not atomically.
- No multi-daemon or concurrent-write test. P-3 is code-read only.
- No browser or rendering validation of any kind, so no claim about any UI.
- shellcheck is not installed on moonscape and I was not permitted to install it, so no verdict from it. `sh -n` and `dash -n` pass on all four of my probe scripts and on both project scripts.
- No leased-slot result. moonscape has no `treehouse` binary, so `--slot` is used: the same `run_build` call site with a different slot provider.
- No macOS run of the harness at this head. The companion's path backend and the daemon's `path` mode were exercised on Linux only.
- The prior CI run reported as ETXTBSY-failing was not independently read.
- I did not review anything after `f1529cc`; it is the PR head.

## Minimal defects, no fixes applied

No source or test edits, no commit, no push, no merge, no lease return.

1. **P-1**, `base_meta.rs:190-196`. Make `remove` return its error, and drop the in-memory entry only when the on-disk removal succeeded.
2. **P-2**, `base_meta.rs:201-216`. Write the destination before removing the source so a rename cannot lose the record.
3. **P-3**, `base_meta.rs:145`. Make the temporary file name unique per call rather than per process.
4. **P-4** and **P-5**, `base_meta.rs:9-12`. Correct the stated path to `<name>/base.json` and drop the claim that the metadata root is invisible in the mount root, or make it so.
5. **P-6**, `scripts/namespaces17-treehouse-linux.sh`. An immutable attempt directory, so the acceptance can be run twice.
6. **P-7**, add the same name validation to `remove` that `set` has, for consistency.
7. **ETXTBSY**, not fixed here by instruction. It stays a merge BLOCK on its own, independent of items 1 to 6.
8. The tree-level `cargo clippy -- -D warnings` gate is red in `cowfs-meta` and should be fixed by whoever owns that file, since a red workspace gate hides real regressions.
9. Keep issue 17 open. Do not treat a passing Path-backend integration as the mode (b) warm-base acceptance; Core publication and the flake both remain.
10. Issue 97's source fix is sound and can be split out by the coordinator if the ETXTBSY flake needs to land separately. That is a coordination decision and not mine.

## Reproduction

Mac, leased worktree:

```sh
cd /Users/zeeshanhaque/Projects/cowfs/.treehouse-build-train/.treehouse/cowfs-7c1bf8/14/cowfs
git rev-parse HEAD                                   # f1529cc1885b0ee6e9e5ab7eb2cbfe62cc9605cc
git diff --stat e243cb1..f1529cc
```

Linux, private root, every heavy step through the wave lock:

```sh
git archive --format=tar f1529cc > src-f1529cc.tar
scp src-f1529cc.tar moonscape@192.168.68.119:/home/moonscape/cowfs-pub/
ssh moonscape@192.168.68.119 \
  'mkdir -p ~/cowfs-pub/repo && cd ~/cowfs-pub/repo && tar xf ../src-f1529cc.tar \
   && sha256sum crates/cowfs-daemon/src/base_meta.rs crates/cowfs-daemon/src/backend.rs \
                crates/cowfs-daemon/src/import.rs scripts/namespaces17-treehouse-linux.sh \
   && ./scripts/namespaces17-treehouse-linux.sh ~/cowfs-pub/repo; echo EXIT=$?'
```

Expected, and what I measured: `EXIT=0` and `VERDICT: PASS`, with `fresh=true` and a record at `<store>/.cowfs-base-meta/<name>/base.json`.
Running it a second time in the same root gives `EXIT=1` on the seed import collision, which is P-6.

My probes and evidence, all under `bench/out/namespaces17-publication-critic/`, which is gitignored:

```
src-f1529cc.tar                 the exact tree I reviewed
s1-deliverable.sh               sample 1: the acceptance script, the on-disk record, the R-3 rerun
gates-a-f.sh                    A record-from-disk, B corrupt record, C no metadata root,
                                D rename and remove, E un-annotated survivor, F mount-root leak
parse-list.py                   the one-line JSON reader gates-a-f.sh and gate-a2-core.sh call
gate-a2-core.sh                 A2 stale-path behaviour, and the CoreBackend runtime proof
gate-g-injection.sh             G1 an independent refresh I drove, G2 the injected metadata failure
gate-h-remove-counts.sh         the remove attempt, test counts, fmt and clippy exits
followups-clippy-flake.sh       clippy scoping and the bounded ETXTBSY sample

evidence-s1.txt                 S1 EXIT=0 PASS, the record, RERUN EXIT=1
evidence-gates-a-f.txt          gate A through F, each with its own exit codes
evidence-a2-core.txt            Core create/promote/reopen, and the stale-path answer
evidence-g-injection.txt        G1 fresh=true, G2 refresh EXIT=1 and the preserved record
evidence-h-remove-counts.txt    remove attempt, 70 lib, 13 canonical, 17 python, fmt 0 clippy 101
evidence-clippy-flake.txt       clippy scoping, tx.rs:314, 1/12 at 8 threads, 0/6 serial
```

Cleanup: no process of mine and no mount of mine remain.
Both cargo `target` directories under my own private root were removed after confirming each held only `CACHEDIR.TAG debug tmp`.
My root is 208 MiB with all logs and stores kept.
My earlier private areas are verified unchanged: `cowfs-ns17crit` at 31 MiB with the round-1 artifact still `82f5af53d1dd8f569284f481c3f29b5ee3972d3fc422e737518015dea48db2d4`, and `cowfs-ns17-int` at 91 MiB.
The ready-wave pool was never written to.
Its two live daemons, on `task-g4` and `task-g5`, and the host's two cowfs mounts and three FUSE connections belong to those workers, and every one was left alone.
Mac-side owned growth this round is 5.7 MiB against an 8 GiB allowance.

Five mistakes of my own this round, reported rather than left for someone else.
I put each socket directly in a 0755 directory, and every daemon refused to start with a correct security message about socket parent permissions, so my first gates run proved nothing.
I wrote `HOME=$THHOME`, a typo for a variable that did not exist, under `set -u`, which killed a run mid-refresh and leaked a daemon; I verified its pid, argv, store and socket before reaping it, and the reaping used SIGTERM because I had the wrong shutdown path.
`shutdown` is a `cowfs` CLI subcommand, not a `cowfs-daemon` flag, so every earlier "still alive after 25s" in my probes was my own bad invocation falling through to SIGTERM rather than a slow shutdown.
My remove-divergence isolation removed the base snapshot as well, which masks the divergence, so that gate did not test what I intended and I am not claiming it as a reproduction.
And I checked a mount point for surviving bytes after stopping the daemon, which is why an early probe reported data loss that does not exist.
All signalling was single-pid, ownership-verified, never a group and never a `pkill`, so the safety property held even where my method was wrong.

No commit, no push, no merge, no lease return.