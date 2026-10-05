# Verification: real-project acceptance for #15 and #16

Status: **acceptance NOT met.** The warm-base step that mode (b) rests on cannot run on the core
backend at the commit this lane was assigned, and two further upstream defects sit behind it.
Everything below is measured on real binaries, a real `cowfs-core` daemon, a real NFS mount and a
real project. Nothing is asserted that was not executed, and no timing is claimed.

- Lease: `/Users/zeeshanhaque/Projects/cowfs/.treehouse-ready-wave/.treehouse/cowfs-7c1bf8/4/cowfs`
- Branch: `verify/treehouse-real-project-16`
- Assigned base: `46b0f269d5bef4a2c204c25f5b3015da601d3beb`
- Harness: `crates/cowfs-treehouse/tests/real_project_acceptance.rs`
- Raw evidence: `bench/out/ready-real-project/acceptance.jsonl` (one flushed JSON record per gate) and
  `bench/out/ready-real-project/*.log` (one log per run)

## What was asked, and what the answer is

#15 is treehouse mode (a), unmodified treehouse against the mount.
#16 is mode (b), snapshot-native slots cloned from a warm base.

The acceptance for both needs a **published warm base**: a snapshot the daemon can later find, with
the repository, ref and commit it was built from.
That step does not exist on the core backend at this base.
An imported snapshot is not a warm base, and none of the records below are presented as one.

## Sample project and environment

The sample is **this repository at the commit under test**.
It is a real project with real dependencies and real tests, it is small enough that nothing is
copied from a corpus, and pinning the commit is what makes the native control and the snapshot the
same project.

```
sample project        cowfs at 46b0f269d5bef4a2c204c25f5b3015da601d3beb
tracked size          1837 files, 11,544,622 bytes (from the daemon's own import report)
duplicate             6360 KiB by `git ls-files -z | xargs -0 du -ck`, before this lane's own
                      test file and this report were added to the tree
rustc                 rustc 1.99.0 (b940084d7 2026-09-28)
cargo                 cargo 1.99.0 (5f94df478 2026-08-27)
git                   git version 2.56.0
host                  macOS 26.6.2 (25G83), Apple M3 Max
mount adapter         nfs (cowfs-nfs in-process loopback), reported by the daemon itself
daemon backend        core
```

Whole suite, one coherent run, single-threaded:

```
cargo test -p cowfs-treehouse --test real_project_acceptance -- --test-threads=1
  8 passed; 0 failed; 1 ignored (the acceptance, blocked on #98)
  no mount of this lane's left in the native mount table afterwards
```

Every daemon was private: its own store, socket, mount and `--export-root`, all under a `TMPDIR`
root, because a Unix socket path must be shorter than `SUN_LEN` and the lease path is not.
Each was identified by pid, argv, socket, store and mount path before any signal, and no process
group was signalled.
The shared daemon 15263 and every other worker's daemon and mount were left alone.
All five private daemons this run started were unmounted and stopped, and the teardown record for
each shows what it unmounted and that nothing was left listed.

## Result per gate

| Gate | Result | Real evidence |
| --- | --- | --- |
| Native control, same project/commit/deps/compiler | PASS | build exit 0, test exit 0 |
| Warm base published from a git ref on the core | **FAIL, blocked** | companion exit 1, `unsupported` |
| Published base discoverable with provenance (#98) | **BLOCKED**, gate written and run | refresh never published, `base_commit` empty, `fresh` false |
| `git worktree add` path parse (#97) | **FAIL, reproduced** | stdout carries no path on git 2.56.0 |
| Companion can materialise a slot (`mount_snapshot`) | **FAIL, stale refusal** | companion exit 1, cites gap 1 |
| Verified ingest of the real project on the core | PASS | `verified: true`, both roots equal |
| Fork id distinct from base, and parented by it | PASS | `sr-slot-1` parent `sr-base` |
| Real `mount_snapshot` export at a slot-shaped path | PASS | `mounted: true`, adapter `nfs`, listed in the native mount table |
| Export is writable | PASS | write into the export succeeded |
| Real `cargo build` of the project inside the export | PASS | exit 0 |
| Real `cargo test` of the project inside the export | PASS | exit 0 |
| Reset returns the slot to an untouched base | PASS | 2215 entries both sides, identical digest, write discarded |
| Cache hook installed and read back | PASS | `Added`, then `AlreadyThere`, exact `post_create` line |

## Native control, and the same project inside the core

Both runs use the same commit, the same `Cargo.lock` and the same toolchain.
The exit codes are the deliverable.

```
native control (real filesystem)
  cargo build -p cowfs-ctl      exit 0
  cargo test  -p cowfs-ctl      exit 0
  libcowfs_ctl.rlib            sha256 b1848bf5ca86ff91307ffbd9a3d9dc03a7070a5ca455689987abebedbdb9b27a
  target size                  276,140 KiB

inside a real core-backed NFS export, forked from a base
  cargo build -p cowfs-ctl      exit 0
  cargo test  -p cowfs-ctl      exit 0
  libcowfs_ctl.rlib            sha256 3555b5a0af31114380b3b22ecb66f4e21dd760ac80a564a6b1f94906ab97d011
  export size                  286,615 KiB
```

The two rlib digests are **not** equal, and no byte identity is claimed.
The absolute path of the tree is baked into debug info, so a build in a snapshot cannot produce the
same bytes as a build in a different directory.
The digests also differ between runs of this harness, because each run clones the sample into a
different temporary path.
Equality of artifacts would have been the wrong assertion here; the exit codes and the readback are
the right ones.
No duration is claimed for either run.

The ingest that produced the base was verified by the daemon itself, by hashing the source on disk
and the snapshot through its own `Vfs`:

```
verified            true
source_root_hash    14637cc5747b19ff79eda7432a49a2aacb825e36a8121208edf7c9d56bef393a
imported_root_hash  14637cc5747b19ff79eda7432a49a2aacb825e36a8121208edf7c9d56bef393a
files / bytes       1837 / 11,544,622
```

Reset returned the slot to the base, entry for entry and byte for byte:

```
base entries                     2215
slot entries after reset         2215
base  Cargo.toml sha256          cf256f30883b4f63464177405223a9d791c63ac2c704b9f5a8525f75b537f92c
slot  Cargo.toml sha256          cf256f30883b4f63464177405223a9d791c63ac2c704b9f5a8525f75b537f92c
slot write survived reset        false
```

## Dependency 1, and it is the one that blocks acceptance

`base_refresh` over a real core daemon does not publish anything.

```
command      cowfs-treehouse --socket <private> --json base refresh --repo <sample> --ref HEAD
exit code    1
stderr       cowfs-treehouse: cowfs: unsupported: this backend stores snapshots as trees,
             not as directories: copy the source into the mount path instead
snapshot list after   {"snapshots":[]}
git worktrees        unchanged
daemon log           serving <mount> on <mount> with the nfs adapter
```

The refusal is deliberate and documented in the source, not an accident:

- `crates/cowfs-daemon/src/handler.rs`, `base_refresh` calls `can_ingest()?` before anything else.
- `can_ingest` returns `unsupported` unless `backend.ingests_directories()`.
- `CoreBackend::ingests_directories()` is `false`, and its doc comment says why: `base_refresh` still
  copies a git worktree into the store directory, which means nothing for a backend whose snapshots
  are trees.
- `import` does not come through that flag, it goes through `Backend::ingest`, which is why a real
  ingest on the core works while `base_refresh` cannot.

So on the core, the warm-base publication that #16 depends on is unimplemented by design.
Until it exists, mode (b) has no base to clone and #15/#16 acceptance cannot be claimed.

## Dependency 2, independent of dependency 1: #97

`crates/cowfs-daemon/src/import.rs` finds the checkout by reading the last non-empty line of
`git worktree add --detach <commit>` stdout and treating it as a path.
On this host's git that line is never a path.

```
git version 2.56.0

git -C <repo> worktree add --detach <sha>
  exit    0
  stdout  HEAD is now at 46b0f26 Merge pull request #95 from zeeshanhaque21/fix/core-model-94
  stderr  Preparing worktree (detached HEAD 46b0f26)
  the parsed value is a directory:  false
  worktree left behind:             <repo>/46b0f269d5bef4a2c204c25f5b3015da601d3beb

git -C <repo> worktree add --detach -q <sha>
  exit    0
  stdout  (empty)
  the parsed value is a directory:  false
  worktree left behind:             <repo>/46b0f269d5bef4a2c204c25f5b3015da601d3beb
```

Both invocations **succeed** and both leak a worktree, so this is not a git failure.
The parse has no valid input on this git: with the message the value is prose, and with `-q` the
value is absent.
It is a second blocker because `base_refresh` is also refused on the core: fixing dependency 1 alone
would expose this one on whichever backend does permit the call.

A failed `base_refresh` therefore leaves a git worktree inside the user's repository, named after
the commit, because the early return happens before the compensating `git worktree remove`.
The harness measures that leak and cleans it up on both runs.

#97 is reported as fixed in PR #92 at `e243cb17971955397b3bfc67c16f6f13223d0d57`, which is **not** an
ancestor of this lane's base, so the reproduction above stands for the code in this lease and not
for that head.
The fix was not re-measured here, because measuring another lane's head is that lane's evidence to
produce.

## Dependency 3: the companion's materialiser refusal is stale

`crates/cowfs-treehouse/src/mode_b.rs` refuses to materialise a slot and says the control protocol
has no `mount_snapshot` method.
It does.
`crates/cowfs-daemon/src/exports.rs` implements it, `crates/cowfs-daemon/src/handler.rs` routes it,
and the daemon exposes `--export-root` to make it usable.
`docs/v1-control-api.md` lists `mount_snapshot` in the protocol, and this harness drives it directly
and successfully above.

Against a real daemon, with a real treehouse-shaped slot and a real base behind it:

```
pool id            sample-e18c1e
base snapshot      sample-e18c1e-base
slot snapshot      sample-e18c1e-1        (distinct from the base)
slot .git          gitdir: <sample>/.git/worktrees/sample
command            cowfs-treehouse --socket <private> --json provision --slot <pool>/1/sample
exit code          1
stderr             this daemon cannot make snapshot "sample-e18c1e-1" appear at <slot>:
                   the control protocol has no mount_snapshot method
                   (docs/v1-treehouse.md, gap 1)
```

Everything up to that call worked for real: the pool id was derived and matched, the base was found
by its derived name, the slot's `.git` resolved, and the slot snapshot name was derived and distinct.
The flow then stops at a refusal whose stated reason is not true of this daemon.

## Dependency 4: #98, provenance, which is what acceptance actually requires

Acceptance needs `base status` to report the repository, the ref, the commit and `fresh`, and
`find_base` to be able to discover the published base.
#98 reports that a real `base refresh` exits 0 while `base_commit` is null and `fresh` is false,
because the provenance is returned in memory and never persisted, and both backends reconstruct
`BaseMeta` from promoted-name sets with every field `None`.

At this lane's base dependency 1 means no base is ever published, so the provenance gate has nothing
to inspect and the run records exactly that:

```
refresh exit        1
status exit         1
base snapshot       sample-e18c1e-base      (the name status looks for)
base_commit         (empty)
head_commit         46b0f269d5bef4a2c204c25f5b3015da601d3beb
fresh               false
reason              no warm base sample-e18c1e-base for this repository
snapshot list       {"snapshots":[]}
```

The gate itself is written and runs on every head, not only on a fixed one.
On a base where `base_refresh` succeeds it demands the provenance and fails, which is the correct
behaviour until #98 is fixed.
It is `a_published_warm_base_must_be_discoverable_with_its_provenance`.

## What is left to close, and by whom

The full acceptance for #15/#16 is written and runnable.
It is `#[ignore]`d, because a permanently red default test teaches nobody anything, and because it
cannot pass until the base is genuinely published and discoverable.
Run it on a head that claims to close #97 and #98:

```
cargo test -p cowfs-treehouse --test real_project_acceptance -- \
  --ignored warm_base_acceptance_over_a_real_core --test-threads=1 --nocapture
```

It asserts, with real exit codes, in order: a warm base published from a real git ref with
discoverable provenance; two fresh slots that each clone **that published base**, not an imported
artifact and not the empty tree; a real `cargo build` and `cargo test` of the sample project inside a
fresh slot; and a reset that returns the slot to the untouched base.

Ownership of the seams, so nothing here is duplicated:

| Seam | Owner |
| --- | --- |
| core `base_refresh` publication, `can_ingest` gate | issue #98 lane, slot 10, `Snapshots`/Path/Core provenance seam |
| `git worktree add` path resolution in `crates/cowfs-daemon/src/import.rs` | issue #97 lane, PR #92; **not** touched here |
| `CowfsMaterialiser` calling the real `mount_snapshot` | companion lane; **not** touched here, it needs `Provision`'s borrow shape revisited because the materialiser would need the daemon it already holds |
| child-open-FD refusal on a private slot | issue #20 lane; **not** duplicated here |
| acceptance fixtures, cache-hook evidence, this report | this lane |

No production source was changed in this lane.
The blocker tests are written so that closing a dependency turns a green test red, which forces the
acceptance to be updated rather than silently bypassed.

## A teardown defect in this harness, found and fixed

Worth recording because it is the kind of thing that damages a shared machine.

The first run of the in-slot build left a live NFS mount in the table.
The harness killed the daemon before unmounting the export, so the mount had no server behind it, and
the temporary tree could not be removed.
The run then hung until its timeout.

Fixed by unmounting every export through the daemon that owns it, then a bounded clean shutdown
through the companion's `shutdown`, and only then a signal if the process is still there.
Teardown now reads the native mount table back and records anything of ours that is still listed,
rather than walking or removing a path it has not verified.

The dead mount from the earlier run was cleaned after checking that the native mount table listed it,
that no live process owned its socket or store, and that the path was under this lane's own private
temporary root.
It was the only mount touched, and the shared daemon 15263 and the other worker's daemon were not
signalled, unmounted or otherwise disturbed.

Teardown now asks the daemon to stop first, then re-reads the native mount table and unmounts
anything of ours that is still listed, re-checking each path against the table immediately before
passing it to `umount`.
The final suite run started five private daemons and recorded, for each, the paths it unmounted and an
empty list of anything left behind.

## Honest limits of this evidence

- Path-backend readback is not core `fsck` and says nothing about crash durability.
- A surviving artifact proves nothing about publication; the provenance gate exists because of that.
- No speed or overhead claim is made.
  The machine is shared with eleven other workers, and the two durations recorded here (the native
  control's wall time and cargo's own "Finished in 41.19s" inside the export) are not a benchmark.
  The build-overhead success criterion needs a quiet host and is still open.
- Mode (a), #15's unmodified-treehouse-against-the-mount half, is not measured here.
  The companion's mode (a) checks and `return` path are covered by the crate's existing suite.
- The blocker reproductions are for this lane's base.
  PR #92's #97 fix and any #98 fix are later heads and were not re-measured here.