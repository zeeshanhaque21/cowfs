# Verification: real-project acceptance for mode (b) treehouse slots

Status: **acceptance NOT met (NONACCEPTED).** The warm-base step that mode (b) rests on cannot run
on the core backend at the commit this lane was assigned, and two further defects sit behind it.
Everything below was measured on real binaries, a real `cowfs-core` daemon, a real NFS mount and a
real project. Nothing is asserted that was not executed, and no timing is claimed.

- Harness: `crates/cowfs-treehouse/tests/real_project_acceptance.rs`
- Commit these receipts belong to: `1b1f2e1c4d1ef43f8dfa0e321774a60e058f67d2`
- Raw evidence, gitignored and private to this lane: `bench/out/ready-real-project/acceptance.jsonl`
  plus one log per run in the same directory

## Which run this document records

Every number below is from one coherent single-threaded run at `1b1f2e1`, whose receipt file is
`acceptance.jsonl` and whose log is `run-full-suite-repair.log`:

```
cargo test -p cowfs-treehouse --test real_project_acceptance -- --test-threads=1
  10 passed; 0 failed; 1 ignored
```

Two earlier runs exist and are **historical, not the record for this head**. They measured a
different tree, because the harness is itself the sample project, so every edit to the harness
changes the corpus:

| run | head | sample tracked | import files | exported entries | result |
| --- | --- | --- | --- | --- | --- |
| earlier 1 | `46b0f26` | 6360 KiB | 1837 | 2215 | 8 passed, 1 ignored |
| earlier 2 | `1ca8242` | 6436 KiB | 1909 | 2287 | 8 passed, 1 ignored |
| **this run** | **`1b1f2e1`** | see receipts | **2021** | **2399** | **10 passed, 1 ignored** |

The earlier two used an unbounded command runner and are the runs whose teardown could strand a
mount. They are kept only to show the numbers moved, and none of them is cited as acceptance.

## Identity of what ran

Recorded in the receipt, not inferred:

```
workspace head          1b1f2e1c4d1ef43f8dfa0e321774a60e058f67d2
sample commit           1b1f2e1c4d1ef43f8dfa0e321774a60e058f67d2
rustc                   rustc 1.99.0 (b940084d7 2026-09-28)
cargo                   cargo 1.99.0 (5f94df478 2026-08-27)
git                     git version 2.56.0
host                    macOS 26.6.2 (25G83), Apple M3 Max
sha256 cowfs-daemon     b6b9970944b487249d3041f40ed78d6c8755116e3df19b7d7d953cf14b4eeb51
sha256 cowfs            59b88405ab1e3ad00ed242938684ae01f56fe2254bfa18e34a6fe7c445bb9f3c
sha256 cowfs-treehouse  65a42acb158c37e46b4a454057cd0d56bec7d90d8c7c5ef51dc4f1f0b15fab6e
daemon backend          core
mount adapter           nfs, read back from the native mount table
```

The three binary digests are identical to the ones the independent reviewer recorded. That is
useful and it is not a build record: those binaries were built once in this lease and reused, and
they are byte-identical to the reviewer's copy because both came from the same target directory.
Because this lane changes no production source, the daemon binary is also production-identical
between `46b0f26` and `1b1f2e1`. That argument stops holding the moment production source moves, which
is why the digests are now in every receipt.

## Sample project

This repository at the commit under test. A real project with real dependencies and real tests,
small enough that nothing copies a corpus.

```
tracked files / bytes   2021 / 12,295,644   (from the daemon's own verified import report)
import verified         true
source_root_hash        d18d6396b7aef917c02cbf779f332d06e0528e2f0277f83a2054f9635a6137aa
imported_root_hash      d18d6396b7aef917c02cbf779f332d06e0528e2f0277f83a2054f9635a6137aa
```

## Executed and skipped, as counts

```
default tests in the file                     11
  executed and asserted for real              10
  executed as a capability skip                0
  failed                                        0
ignored, never executed                         1   warm_base_acceptance_over_a_real_core
records appended and flushed in this run       29   across 5 private daemons
```

Not measured by this document, and declared as such: mode (a) against a real pool, warm-base
publication, the two-slots-from-a-published-base flow, and anything about speed, dedup,
last-writer-wins or crash durability.

## Result per gate

| Gate | Result | Receipt |
| --- | --- | --- |
| Native control, same project/commit/deps/compiler | PASS | build exit 0, test exit 0 |
| Warm base published from a git ref on the core | **FAIL, chain link 1** | companion exit 1, `unsupported` |
| `git worktree add` path parse | **FAIL, chain link 2** | stdout carries no path on git 2.56.0 |
| Companion calls `mount_snapshot` | **FAIL, companion-side** | companion exit 1, cites gap 1 |
| Published base discoverable with provenance | **BLOCKED by link 1**, gate written and run | `base_commit` empty, `fresh` false |
| Verified ingest of the real project | PASS | `verified: true`, both roots equal |
| Fork id distinct from base, parented by it | PASS | `sr-slot-1`, parent `sr-base` |
| Real export at a treehouse-shaped slot path | PASS | mount table lists it, fstype `nfs` |
| Export equals the source, both directions | PASS | 2399 vs 2399, 0 only-in-export, 0 only-in-source |
| Export is writable | PASS | write succeeded |
| Real `cargo build` inside the export | PASS | exit 0, after mount identity readback |
| Real `cargo test` inside the export | PASS | exit 0 |
| Reset returns the slot to an untouched base | PASS | 2399 both sides, identical digest, write gone |
| Cache hook installed and read back | PASS | asserted `Added` then `AlreadyThere` |
| Mount readback is tri-state and scoped | PASS | 5 synthetic cases, no mount involved |
| Receipt states what was measured | PASS | 18 records read at that point, 0 capability skips |

## Native control, and the same project inside the core

Same commit, same `Cargo.lock`, same toolchain. The exit codes are the deliverable.

```
native control (real filesystem)
  cargo build -p cowfs-ctl      exit 0
  cargo test  -p cowfs-ctl      exit 0
  libcowfs_ctl.rlib            sha256 20a447cd45224b7f28bd66cd8b95457026337ceee95340c0c1d672b39eaf856f
  target size                  277,596 KiB

inside a real core-backed NFS export, forked from a base
  mount point                  <private>/th/.treehouse/p/1/sample
  mount source                 localhost:/cowfs-6288e6fe03f73e3d1c561cf401173816
  mount fstype                 nfs
  answering store              <private>/store, read back from the daemon
  cargo build -p cowfs-ctl      exit 0
  cargo test  -p cowfs-ctl      exit 0
  libcowfs_ctl.rlib            sha256 2d1144d12cf4c85e94c22955219d277856aaf766c7958fbacd0a2441014b409e
  export size                  287,461 KiB
```

The mount identity is read from the native table, not from the API's own `mounted: true`, and the
answering store is read back from the daemon. That is what stops this row from being an ordinary
native build that never touched cowfs.

The two rlib digests are **not** equal and no byte identity is claimed: the tree's absolute path is
baked into debug info, and each run clones the sample into a different temporary path. Equality of
artifacts would have been the wrong assertion. The exit codes and the readback are the right ones.
No duration is claimed for either run.

Reset returned the slot to the base, entry for entry and byte for byte:

```
base entries                     2399
slot entries after reset         2399
base  Cargo.toml sha256          cf256f30883b4f63464177405223a9d791c63ac2c704b9f5a8525f75b537f92c
slot  Cargo.toml sha256          cf256f30883b4f63464177405223a9d791c63ac2c704b9f5a8525f75b537f92c
slot write survived reset        false
```

## The blocker chain, in the order it has to be broken

### Link 1: `base_refresh` does not exist on the core

```
command      cowfs-treehouse --socket <private> --json base refresh --repo <sample> --ref HEAD
exit code    1
stderr       cowfs-treehouse: cowfs: unsupported: this backend stores snapshots as trees,
             not as directories: copy the source into the mount path instead
snapshot list after   {"snapshots":[]}
git worktrees        unchanged
```

Deliberate and documented in the source, not an accident:

- `crates/cowfs-daemon/src/handler.rs`: `base_refresh` calls `can_ingest()?` before anything else.
- `can_ingest` answers `unsupported` unless `backend.ingests_directories()`.
- `CoreBackend::ingests_directories()` is `false`, and its own doc comment says why: `base_refresh`
  still copies a git worktree into the store directory, which means nothing for a backend whose
  snapshots are trees.
- `import` does not come through that flag; it goes through `Backend::ingest`, which is why a real
  ingest on the core works while `base_refresh` cannot.

**This is the blocking dependency.** Provenance work cannot make `base_refresh` publish anything,
because the call is refused before any persistence is attempted.

### Link 2: #97, independent of link 1

`crates/cowfs-daemon/src/import.rs` found the checkout by reading the last non-empty line of
`git worktree add --detach <commit>` stdout and treating it as a path. On this host's git no line of
that stdout is a path:

```
git version 2.56.0

git -C <repo> worktree add --detach <sha>
  exit    0
  stdout  HEAD is now at 1b1f2e1 test(treehouse): make the acceptance harness safe, honest and rustfmt-clean
  the parsed value is a directory:  false
  worktree left behind:             <repo>/1b1f2e1c4d1ef43f8dfa0e321774a60e058f67d2

git -C <repo> worktree add --detach -q <sha>
  exit    0
  stdout  (empty)
  the parsed value is a directory:  false
  worktree left behind:             <repo>/1b1f2e1c4d1ef43f8dfa0e321774a60e058f67d2
```

Both invocations **succeed** and both leak a worktree, so this is not a git failure. With the
message the value is prose; with `-q` the value is absent. The parse has no valid input on this git.

A failed `base_refresh` therefore leaves a worktree inside the user's repository, named after the
commit, because the early return happens before the compensating `git worktree remove`. The harness
measures that leak on both forms and cleans both up; the readback is 0 entries after cleanup.

**Attribution, corrected.** The repair is `b59bc3c` "fix(daemon): give base_refresh an explicit
worktree path, stop parsing stdout (#97)", which touches `import.rs` and
`crates/cowfs-treehouse/tests/canonical.rs`. `e243cb1` is not the repair: it is
"docs(namespaces): retract the through-the-wiring PASS", one documentation file, no code. `b59bc3c`
is not an ancestor of this lane's head and was not re-measured here. `import.rs` is byte-identical
between `46b0f26` and the `main` this PR is based on, so the unfixed code path is present there too
at the source level; the reproduction here was measured only at this lane's head.

This link is independent of link 1: repairing link 1 alone would expose it on whichever backend does
permit the call.

### Link 3: the companion never calls the `mount_snapshot` the daemon provides

`crates/cowfs-treehouse/src/mode_b.rs` refuses to materialise a slot and says the control protocol
has no `mount_snapshot` method. It does: `crates/cowfs-daemon/src/exports.rs` implements it,
`crates/cowfs-daemon/src/handler.rs` routes it, the daemon exposes `--export-root`, and
`docs/v1-control-api.md` lists it. This harness drives it directly and successfully above.

The unsupported transition is exactly one, and it is companion-side: `CowfsMaterialiser` never calls
`mount_snapshot`. Against a real daemon, with a real treehouse-shaped slot and a real base behind it:

```
pool id            sample-e18c1e
base snapshot      sample-e18c1e-base
slot snapshot      sample-e18c1e-1        (distinct from the base)
command            cowfs-treehouse --socket <private> --json provision --slot <pool>/1/sample
exit code          1
stderr             this daemon cannot make snapshot "sample-e18c1e-1" appear at <slot>:
                   the control protocol has no mount_snapshot method
                   (docs/v1-treehouse.md, gap 1)
```

Everything up to that call worked for real: the pool id was derived and matched, the base was found
by its derived name, the slot's `.git` resolved, and the slot snapshot name was derived and distinct.

### Link 4: provenance, which is what acceptance requires once a base exists

Acceptance needs `base status` to report the repository, the ref, the commit and `fresh`, and
`find_base` to discover the base. Because link 1 means no base is ever published here, the run records
that instead:

```
refresh exit        1
status exit         1
base snapshot       sample-e18c1e-base      (the name status looks for)
base_commit         (empty)
head_commit         1b1f2e1c4d1ef43f8dfa0e321774a60e058f67d2
fresh               false
reason              no warm base sample-e18c1e-base for this repository
snapshot list       {"snapshots":[]}
```

The gate is written and runs on every head, not only on a fixed one. On a head where `base_refresh`
succeeds it demands the provenance and fails, which is the correct behaviour until that seam is
fixed. It is `a_published_warm_base_must_be_discoverable_with_its_provenance`.

## The acceptance gate, and why it stays unaccepted

`warm_base_acceptance_over_a_real_core` is `#[ignore]`d with the reason naming the chain. It is not a
blanket waiver: it is the only ignored test, and it is the acceptance.

```
COWFS_ACCEPTANCE_REQUIRED=1 cargo test -p cowfs-treehouse \
  --test real_project_acceptance -- --ignored warm_base_acceptance_over_a_real_core \
  --test-threads=1 --nocapture
```

It asserts, with real exit codes: a warm base published from a real git ref with discoverable
provenance; two fresh slots that each clone **that published base**, not an imported artifact and not
the empty tree; a real `cargo build` and `cargo test` inside each, with the mount identity read back
before the build; a reset after each that returns the slot to a byte-identical untouched base; and
the base itself still intact after both builds.

It has never been executed to a pass, so #15 and #16 remain open and unaccepted. Nothing in this
document, and no test in this file, asserts otherwise.

## Ownership, so nothing here is duplicated

| Seam | Owner |
| --- | --- |
| core `base_refresh` publication and the `can_ingest` gate | needs an issue that scopes it; the publication seam sits with the provenance lane |
| provenance persistence and `BaseMeta` reconstruction | provenance lane |
| `git worktree add` path resolution in `crates/cowfs-daemon/src/import.rs` | #97 repair `b59bc3c`, unmerged; **not** touched here |
| `CowfsMaterialiser` calling the real `mount_snapshot` | companion lane; **not** touched here, and it needs `Provision`'s borrow shape revisited because the materialiser would need the daemon it already holds |
| child-open-FD refusal on a private slot | holder lane; **not** duplicated here |

No production source was changed in this lane.

## Harness honesty and safety, as built

- A missing sibling binary is always a hard failure. Verified by a negative control that runs the
  compiled test binary alone in an empty directory: it exits 101 with a message naming the fix,
  instead of reporting `ok` for a gate that ran nothing.
- A missing mount capability is recorded as `skipped-capability`, announced as not measured, and
  checked by a receipt test against the receipt file. `COWFS_ACCEPTANCE_REQUIRED=1` turns it into a
  failure, and is the only mode that can produce acceptance. This run had zero capability skips.
- Every teardown command runs under one absolute deadline taken before the first spawn, so a phase
  cannot extend itself. Every command has its own bound on top of that, and its own child is killed
  on expiry, never a group.
- The native mount table is read as a tri-state. An unreadable or unparsable table quarantines and
  unmounts nothing, rather than recording an empty list that reads as a clean readback. A synthetic
  control covers that with no mount and no daemon, including the case of a sibling directory that
  shares a name prefix.
- `Drop` never panics. A panic there during an unwinding failure is a double panic, which aborts the
  process and destroys the failure message.
- The temporary tree is no longer left to a `TempDir` drop, whose `remove_dir_all` walks a live mount
  point when teardown failed. It is removed only when the table is known and nothing of ours is
  mounted under it; otherwise it is left in place and named for inspection.
- A signal is sent only after the registered pid, argv, store and socket are re-read from the process
  table and matched. No process group, no `pkill`, no `abort`. The shared `Watchdog` is not used,
  because its abort path runs no destructors and would leave a live daemon and a live mount.
- All five private daemons in this run exited on their own after a control-plane shutdown, none was
  signalled, and every teardown recorded an empty `mounts_left_listed` and an empty quarantine.

Defects this harness had, found and fixed during this repair, recorded because they are the kind
that damage a shared machine:

1. The teardown killed the daemon before unmounting its exports, leaving a live mount with no server,
   and the tree under it could not be removed. The run hung until its timeout.
2. The mount-table reader discarded everything when the `mount` command failed or its shape changed,
   so the teardown recorded a clean readback while knowing nothing. That is failing open on the
   harness's own safety check.
3. The `shutdown` request was sent to the wrong binary, so every teardown fell through to the
   fallback path.
4. The bounded runner drained its pipes only after the child exited, which deadlocked any command
   whose output fills the pipe buffer, and every importing `cowfs` call does that with its progress
   frames.

## Independent evidence carried honestly

An independent reviewer validated the small real Core chain separately: a verified import, a promote,
an O(1) fork, a write into the export, and a reset, over one real daemon and one real NFS mount, on
this host with the same three binary digests recorded above. Their sample was
`git archive 1ca8242` of `crates/cowfs-treehouse`, 18 files, 306,821 bytes, with the export holding
the imported tree entry for entry and byte for byte and matching a native do-nothing baseline
digest for digest.

That is **source-bound historical evidence for `1ca8242`**, not for this head and not an acceptance
of mode (b). It is cited because it independently confirms the data-plane half of this lane's
findings on a tree small enough to walk by hand. It does not establish warm-base publication, which
neither run could reach.

### A measured limit: a post-reset readback through a still-mounted export can lag

Independently observed by the reviewer, and reproduced here only in the sense that this harness
avoids it. Reading a still-mounted export immediately after `snapshot reset` showed the pre-reset
tree while `reset` had returned exit 0; a fresh daemon reading the same store saw the correct state
immediately; and re-reading the same mounted path converged as follows:

```
0.00s after reset   24 entries   marker present   != base
2.00s after reset   23 entries   marker absent    == base
6.04s after reset   23 entries   marker absent    == base
12.02s after reset  23 entries   marker absent    == base
```

So the store is correct and the live export view converges within about two seconds. The cause was
not determined and is not claimed here; it is consistent with a cache-propagation delay. This is a
**measured limit of that observation, not a claim that reads are eventually coherent in general.**

Why it matters for mode (b): a slot reset is a routine operation on a slot an agent already has
mounted. This harness therefore always unmounts and re-exports before reading back after a reset,
which is the right way to avoid the race and also means this harness does not characterise it.

## Honest limits of this evidence

- Path-backend readback is not core `fsck` and says nothing about crash durability.
- A surviving artifact proves nothing about publication, which is why the provenance gate exists.
- No speed or overhead claim is made. The machine is shared with eleven other workers. The
  build-overhead success criterion needs a quiet host and is still open.
- Mode (a), unmodified treehouse against the mount, is not measured here. The companion's mode (a)
  checks and `return` path are covered by the crate's existing suite.
- The blocker reproductions are for this lane's head. The #97 repair and any provenance repair are
  later, unmerged heads and were not re-measured here.
- The rlib digests differ between the native control and the in-slot build, and between runs. That is
  expected and is not a defect; no artifact byte identity is claimed.