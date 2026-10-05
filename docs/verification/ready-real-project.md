# Verification: real-project acceptance for mode (b) treehouse slots

Status: **acceptance NOT met (NONACCEPTED).** The warm-base step that mode (b) rests on cannot run
on the core backend at the commit this lane was assigned, and three more links sit behind it in a
four-link chain. Everything below was measured on real binaries, a real `cowfs-core` daemon, a real
mount and a real project. Nothing is asserted that was not executed. This report carries no timing:
the per-test figures live in the evidence file, each labelled with the commit that recorded it, and
none of them is a performance claim.

- Harness: `crates/cowfs-treehouse/tests/real_project_acceptance.rs`
- Raw evidence, gitignored and private to this lane: `bench/out/ready-real-project/`
- Repair evidence: `docs/verification/evidence/real-project16-repair.md`
  First committed at `a64e1189ff5f8ac8d34b24aad5d820818f8eaf72`, corrected at
  `86554b48add2c1f2845289953c6487673f6fbd33`. **The link below points at the corrected version**;
  the earlier commit is named only so a reader knows the file was not always committed.
  https://github.com/zeeshanhaque21/cowfs/blob/86554b48add2c1f2845289953c6487673f6fbd33/docs/verification/evidence/real-project16-repair.md

## Which commit each receipt binds to

This matters more than usual here, because the harness is itself the sample project: every edit to
the harness changes the corpus it measures.

| Receipt file | `workspace_head` / `sample_commit` | What it covers |
| --- | --- | --- |
| `acceptance.jsonl` | `a64e1189ff5f8ac8d34b24aad5d820818f8eaf72` | controls, the representative Core run and the blocker gates, on the repaired tree |
| `acceptance-a64e118-pre.jsonl` | `4a70c53ae4c7062458ae7e63dbe8619cce1458a1` | the same gates, plus the two expensive builds, run before the repair was committed |

These are two different files. Their row counts are never added together and never compared as if
one contradicted the other. Their digests:

```
acceptance.jsonl              sha256 88cb8cb879c40aea37eebceecca960134196da8ca9f18059d4b5907894f96ded   12,864 bytes
acceptance-a64e118-pre.jsonl  sha256 ef9a2c3148eb2b3cee064a987a1d4211d10d98e18bcbd7cbaa412cc5302b4a12   17,468 bytes
```

The second file is named for when it was written, before `a64e118` existed as a commit; its
`workspace_head` field says `4a70c53` for the same reason. Its 2508-entry figures belong to that
file and to no other measurement here.

The two expensive builds ran while the N1 to N7 repair was still uncommitted, so their receipts
carry the last commit rather than the tree. The difference between that commit and the tree under
test is this one test file and this document; **no production source changed**, so the daemon, CLI
and companion binaries are the same artifacts. That argument is stated rather than assumed, and it
stops holding the moment production source moves, which is why the binary digests are in every
receipt.

The binary digests below are **artifact identity only**. Build provenance is **UNVERIFIED**: these
binaries were built once in this lease and reused across runs, and nothing binds them to a recorded
source build. They say which bytes answered. They say nothing about new production source, and this
branch changes none.

## Executed and skipped, as counts

Per test, each run once on the repaired tree unless stated:

```
tests in the file, from --list                   15   and 0 benchmarks
  ignored, never executed                         1   warm_base_acceptance_over_a_real_core
  executed on a full run                         14
    of those, capability skips                    0
    of those, failed                              0
the six safe controls                             6   a SUBSET of those 14, not six more
negative controls that must fail                  4   (F1, seeded boolean claim, seeded string claim, seeded undescribed row)
```

The 15 and the 14 come from `--list` on the built binary, which reports one ignored test. A full
run's libtest summary reports the same split, and that is the number to quote for executions.

The four negative controls are **not four more tests**. Each seeds a receipt row and runs a test that
otherwise passes, so it is counted inside the 14.

`acceptance.jsonl` is the file the run recorded here wrote, and nothing else wrote to it: 28 rows,
24 carrying `outcome: "measured"`, 4 carrying `outcome: "cleanup"`, and **zero rows without an
outcome**, which `the_acceptance_receipt_states_what_was_measured` asserts. Its own summary row
reads 27, because it counted before appending itself.

`acceptance-a64e118-pre.jsonl` is a different file from an earlier run: 42 rows, 37 `measured`, 5
`cleanup`, 0 without an outcome. **Neither row count is a count of tests.** Executions are counted
by libtest and by `--list`; rows are counted by parsing, and the `cleanup` rows are teardown
receipts rather than test outcomes.

## Safe controls, run before any mounted work

Each is a synthetic input or a process this lane starts. No mount, no daemon, no shared process.

| Control | Cases | Result |
| --- | --- | --- |
| `the_mount_grammar_of_both_platforms_is_decoded_exactly` | 16 | PASS: macOS `nfs`, Linux `fuse` and `fuse.cowfs`, octal escapes decoded, four malformed shapes refused |
| `the_bounded_runner_finishes_inside_its_bound_for_every_child_shape` | 4 | PASS, measured below |
| `a_recycled_pid_with_the_same_argv_is_refused` | 2 | PASS: same argv with a different start time is refused |
| `a_claim_of_a_published_warm_base_is_refused_in_both_shapes` | 9 | PASS: both claim shapes refused, five non-claims not read as claims, typed serialisation proven |
| `the_mount_readback_is_tri_state_and_never_foreign` | 5 | PASS: empty and unparsable tables are Unknown, prefix siblings excluded |
| `the_acceptance_receipt_states_what_was_measured` | live | PASS on a clean receipt |

Four negative controls that must fail, each observed failing:

| Negative control | Exit | What it proves |
| --- | --- | --- |
| Repaired test binary alone in an empty directory, one daemon gate | **101** | a missing sibling binary is never a pass |
| Receipt row seeded with JSON boolean `warm_base_published: true` | **101** | the guard sees a boolean claim |
| Receipt row seeded with string `warm_base_published: "true"` | **101** | the guard also sees the string claim, which it previously passed |
| Receipt row seeded with no `outcome` field | **101** | counting outcomes cannot silently undercount rows |

### The bounded runner, four child shapes, measured

Old code failed and new code passes, on the same host and the same 3s bound:

| Shape | Result |
| --- | --- |
| chatty, 4 MiB on stdout | exit 0, 4,194,304 bytes, **50ms** |
| empty stdout, exits at once | exit 0, well under bound |
| hangs | killed at its own bound, **3010ms**, only its own pid |
| exits at once, helper holds the pipe open | **3001ms**, classified incomplete drain, direct child's exit status preserved |

Figures from `acceptance.jsonl`. Another run of the same control recorded 46ms, 3001ms and 3005ms;
that is ordinary variation between runs on a shared host and neither set is a timing claim.

The fourth shape returned **20,012ms** before the repair, four times its 3s bound, on a 3s bound:
the phase that waits for the pipes was bounded by the caller's overall deadline instead of the
command's own. Both the root cause and the number are recorded because the earlier receipt for the
stranded daemon below is consistent with this path, though **that consistency is a hypothesis, not a
diagnosis**: the receipt does not say which call consumed the time.

That bound covers one call, the spawn, the child and the collection of its output, and it is
`min(caller's remaining deadline, the call's own cap)`, which is why the receipt records
`bound_ms: "3000"`.

### What N2 bounds, and what it does not

It does not cover the lifetime of the two reader threads the call starts. They are detached and never
reclaimed, so a descendant holding a pipe open past the bound keeps them until the test binary exits.
The call is classified and returns, so nothing hangs and no result is wrong, but the thread count
grows with the number of such calls rather than being reclaimed. The only producer this harness can
identify is this control's own `sleep` helper, which the control records about itself, verifies,
cleans and then asserts on.

The control's own helper is cleaned up by the pid it recorded about itself, verified to be exactly
`sleep 20`, and the test fails if that cleanup does not happen, so a control cannot leave an orphan
and still report success.

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
| Real export at a treehouse-shaped slot path | PASS | mount table lists it, fstype `nfs`, answering store exact |
| Export equals the source, both directions | PASS | 2573 vs 2573, 0 only-in-export, 0 only-in-source |
| Export is writable | PASS | write succeeded |
| Real `cargo build` inside the export | PASS | exit 0, after mount identity readback |
| Real `cargo test` inside the export | PASS | exit 0 |
| Reset returns the slot to an untouched base | PASS | 2573 both sides, identical digest, write gone |
| Cache hook installed and read back | PASS | asserted `Added` then `AlreadyThere` |

## Identity of what ran

```
sample project        this repository at the commit under test
workspace head        a64e1189ff5f8ac8d34b24aad5d820818f8eaf72
                      (the in-slot build and native control name the previous commit)
rustc                 rustc 1.99.0 (b940084d7 2026-09-28)
cargo                 cargo 1.99.0 (5f94df478 2026-08-27)
git                   git version 2.56.0
host                  macOS 26.6.2 (25G83), Apple M3 Max
sha256 cowfs-daemon   b6b9970944b487249d3041f40ed78d6c8755116e3df19b7d7d953cf14b4eeb51
sha256 cowfs          59b88405ab1e3ad00ed242938684ae01f56fe2254bfa18e34a6fe7c445bb9f3c
sha256 cowfs-treehouse 65a42acb158c37e46b4a454057cd0d56bec7d90d8c7c5ef51dc4f1f0b15fab6e
daemon backend        core
mount adapter         nfs, decoded from the native mount table
```

These three digests are the same ones two independent reviews recorded, which is consistent with
artifact identity and is **not** evidence about any build. Build provenance is UNVERIFIED.

## Native control, and the same project inside the core

```
native control (real filesystem)
  cargo build -p cowfs-ctl      exit 0
  cargo test  -p cowfs-ctl      exit 0
  libcowfs_ctl.rlib            sha256 e37bf5bf5579d21e8d688c579e98449ea772a3b59d5f91464d56b2bbb08f0b7d
  target size                  276,540 KiB

inside a real core-backed NFS export, forked from a base
  mount point                  <private>/th/.treehouse/p/1/sample
  mount source                 localhost:/cowfs-232e846cd919734088ef6072e7ad961c
  mount fstype                 nfs
  answering store              <private>/store, exact match against the store this run opened
  cargo build -p cowfs-ctl      exit 0
  cargo test  -p cowfs-ctl      exit 0
  libcowfs_ctl.rlib            sha256 7b3d1d7244d5fa21bdfe40c780e60dd2bd0fc740b9a0bdac24a2e2e221853d0b
  export size                  287,799 KiB
```

The mount identity is decoded from the native table and the answering store is compared for **exact
equality** against the canonical path of the store this run opened. The previous check compared
suffixes, and in Rust `"anything".ends_with("")` is true, so an unanswered status satisfied the
assertion it was meant to falsify; `status_store` now returns an error instead of an empty string
and the comparison has no prefix or basename arm.

The filesystem type is compared against an exact accepted list per platform (`nfs` on macOS;
`fuse`, `fuse.cowfs`, `cowfs` on Linux), never a prefix. The earlier parser read the Linux grammar's
literal `type` keyword as the filesystem type, which turned `ubuntu-latest` red while the very same
gate correctly turned an unverified export into a red test on both platforms.

The two rlib digests are **not** equal and no byte identity is claimed: the tree's absolute path is
baked into debug info and each run clones the sample elsewhere. The exit codes and the readback are
the right assertions. No duration is claimed for either build.

Reset returned the slot to the base, entry for entry and byte for byte:

```
import root hash                  40ee247b291bbc05a515c24a460f6238cb53ae85867af7804af7b40ad36e7b2d
import files / bytes             2194 / 12,857,077
base entries                     2573
slot entries after reset         2573
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

Deliberate and documented in the source:

- `crates/cowfs-daemon/src/handler.rs`: `base_refresh` calls `can_ingest()?` before anything else.
- `can_ingest` answers `unsupported` unless `backend.ingests_directories()`.
- `CoreBackend::ingests_directories()` is `false`, and its own doc comment says why: `base_refresh`
  still copies a git worktree into the store directory, which means nothing for a backend whose
  snapshots are trees.
- `import` does not come through that flag; it goes through `Backend::ingest`, which is why a real
  ingest on the core works while `base_refresh` cannot.

**This is the blocking dependency, and it is the one link an issue now scopes: #123, open, "Define
and implement tree-native Core warm-base publication for the companion".** Provenance work cannot
make `base_refresh` publish anything, because the call is refused before any persistence is
attempted. The refusal is intentional for the directory-import model and is not a new Core defect.

### Link 2: the worktree path

`crates/cowfs-daemon/src/import.rs` found the checkout by reading the last non-empty line of
`git worktree add --detach <commit>` stdout and treating it as a path. On this host's git no line of
that stdout is a path:

```
git version 2.56.0

git -C <repo> worktree add --detach <sha>
  exit    0
  stdout  HEAD is now at <sha> <subject>
  the parsed value is a directory:  false
  worktree left behind:             <repo>/<sha>

git -C <repo> worktree add --detach -q <sha>
  exit    0
  stdout  (empty)
  the parsed value is a directory:  false
  worktree left behind:             <repo>/<sha>
```

Both invocations succeed and both leak a worktree, so this is not a git failure. A failed
`base_refresh` therefore leaves a worktree inside the user's repository, named after the commit,
because the early return happens before the compensating `git worktree remove`. The harness measures
the leak on both forms and cleans both up; the readback is 0 entries.

**Attribution.** The repair is `b59bc3c` "fix(daemon): give base_refresh an explicit worktree path,
stop parsing stdout (#97)", touching `import.rs` and `crates/cowfs-treehouse/tests/canonical.rs`.
`e243cb1` is not the repair: it is "docs(namespaces): retract the through-the-wiring PASS", one
documentation file, no code. `b59bc3c` is not an ancestor of this lane's head and was not
re-measured here. `import.rs` is byte-identical between `46b0f26` and the `main` this PR is based on,
so the unfixed path is present there at the source level; the reproduction was measured only at this
lane's head.

Independent of link 1: repairing link 1 alone would expose this on whichever backend permits the call.

### Link 3: the companion never calls the `mount_snapshot` the daemon provides

`crates/cowfs-treehouse/src/mode_b.rs` refuses to materialise a slot and says the control protocol
has no `mount_snapshot` method. It does: `crates/cowfs-daemon/src/exports.rs` implements it,
`handler.rs` routes it, the daemon exposes `--export-root`, and `docs/v1-control-api.md` lists it.
This harness drives it directly and successfully.

The unsupported transition is exactly one and it is companion-side. Against a real daemon with a
real treehouse-shaped slot and a real base behind it:

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

### Link 4: provenance, which matters once a base exists

Acceptance needs `base status` to report the repository, the ref, the commit and `fresh`, and
`find_base` to discover the base. Because link 1 means no base is ever published here, the run records
that instead:

```
refresh exit        1
status exit         1
base snapshot       sample-e18c1e-base      (the name status looks for)
base_commit         (empty)
fresh               false
reason              no warm base sample-e18c1e-base for this repository
snapshot list       {"snapshots":[]}
```

The gate runs on every head, not only a fixed one. On a head where `base_refresh` succeeds it demands
the provenance and fails, which is correct until that seam is fixed. It is
`a_published_warm_base_must_be_discoverable_with_its_provenance`.

## Disclosure: a real Core daemon was stranded, and how it was dealt with

This was not in the previous revision of this document. It should have been.

The receipt, preserved byte for byte at
`bench/out/ready-real-project/acceptance-pre-fix.jsonl`,
sha256 `0309db70e65a46f3ab4da572f663732cd948726272ddcb1299dd77aa86666a5b`,
15,243 bytes, 29 rows:

```
daemon_pid                        2875
daemon_exited                     false
signalled_after_identity_check    false
unmounted                         ""
mounts_left_listed                unknown: deadline already spent before spawning mount
quarantined                       mount table unknown: deadline already spent before spawning mount
runtime_root                      /var/folders/.../T/.tmp53obIz
```

What that row shows, and what it does not:

- **What the harness did was correct.** It had spent its teardown deadline, could not read the mount
  table, and therefore unmounted nothing, signalled nothing and deleted nothing. That is the
  fail-closed behaviour the safety work exists for, exercised on a real daemon by accident.
- **What went wrong is disclosure and diagnosis.** A real `cowfs-daemon` on a real mount was left
  running with its store locked, and neither this document nor the evidence file mentioned it.
- **Why the deadline was spent is UNKNOWN.** The unbounded pipe join described above is a candidate,
  because it was the one unbounded path in `teardown`, but `shutdown` and `ps` also had caps and the
  receipt does not identify the call that consumed the time. **This is a hypothesis, not a finding.**
- **Who cleaned it up, and how: this lane did, by hand, from a shell, not through the harness.**
  There is therefore **no harness receipt for the cleanup**, and this document will not invent one.
  What was done, in order: the native mount table was read and the daemon's mount was found listed;
  `ps` confirmed the pid's argv was this lane's `cowfs-daemon` binary with this lane's private store
  and socket paths; the mount was unmounted by exact path; the pid was terminated after re-reading its
  identity; the private runtime root was removed only after the mount table was confirmed to hold
  nothing under it. Each of those steps was performed outside the harness, so each is
  **UNVERIFIED by any receipt this lane produced.**
- **That the pid is gone now proves nothing about the above.** It is consistent with this lane's
  cleanup and with anything else that might have ended it.

The file itself is a hand-assembled mixture and its name understates it: of its six teardown rows,
five carry the pre-repair schema and the pids of the `1ca8242` run, and only the sixth, pid 2875,
comes from the repaired code. The rows are preserved rather than rewritten, which is the right
choice, so the file is a record of several runs and not of one. Its name says "pre-fix" because that
is when the incident happened, not because the whole file is a pre-fix run.

## Harness honesty and safety, as built

- A missing sibling binary is always a hard failure, proven by a negative control at exit 101.
- A missing mount capability is recorded as `skipped-capability`, announced as not measured, and
  checked against the receipt by a test. `COWFS_ACCEPTANCE_REQUIRED=1` turns it into a failure, and
  is the only mode that can produce acceptance. This work had zero capability skips.
- Every command runs under one absolute bound covering the spawn, the child **and** the collection of
  its output. Only the direct child is ever signalled, and only by the handle that owns it. No process
  group, no pattern match.
- That bound is per call and does not reclaim the reader threads it starts. The one lifetime limit
  that leaves is stated in full under "What N2 bounds, and what it does not" below.
- A child reaped before its pipes reach EOF is not completion. That case is classified as an
  incomplete drain, carrying the real exit status and the partial output, and is never returned as
  success.
- The native mount table is decoded per platform with octal escapes, read as a tri-state, and scoped
  to this runtime root by construction. An unreadable or unparsable table quarantines instead of
  reporting a clean readback, and there is no fallback that invents a filesystem type.
- `Drop` never panics: a panic there during an unwinding failure is a double panic, which aborts the
  process and destroys the failure message.
- The temporary tree is not left to a `TempDir` drop, whose `remove_dir_all` walks a live mount point
  when teardown failed. It is removed only when the table is known and nothing of ours is mounted
  under it; otherwise it is left in place and named for inspection.
- A signal is sent only after the registered pid, kernel start time, exe, argv, store and socket are
  re-read and matched. The start time is compared, so a recycled pid carrying the same argv is
  refused; a negative control proves it. The shared `Watchdog` is unused because its abort path runs
  no destructors and would leave a live daemon and a live mount.
- All four private daemons in the recorded run exited on their own after a control-plane shutdown,
  none was signalled, and every teardown recorded an empty `mounts_left_listed` and an empty
  quarantine. Five ran in the earlier full-suite run at `1b1f2e1`, also clean.

Defects this harness had, found and fixed during this work, recorded because they are the kind that
damage a shared machine:

1. The teardown killed the daemon before unmounting its exports, stranding a live NFS mount, and the
   tree under it could not be removed.
2. The mount-table reader discarded everything when `mount` failed or its shape changed, so teardown
   recorded a clean readback while knowing nothing: failing open on its own safety check.
3. The `shutdown` request went to the wrong binary, so every teardown fell through to the fallback.
4. The bounded runner drained its pipes only after the child exited, deadlocking any command whose
   output fills the pipe buffer. Every importing `cowfs` call does that with its progress frames.
5. The pipe drain was then bounded by the wrong deadline, so a grandchild holding the pipe held the
   call for as long as the grandchild lived: 20,012ms against a 3s bound.
6. The mount parser encoded one platform's grammar and read the other's filesystem type as `type`.
7. The receipt writer typed every value as a string, so a guard comparing against a JSON boolean was
   unreachable for anything the harness wrote.

## Independent evidence carried honestly

An independent reviewer validated the small real Core chain separately: a verified import, a promote,
an O(1) fork, a write into the export, and a reset, over one real daemon and one real NFS mount, with
the same three binary digests. A second reviewer independently measured the reset's convergence on
the same chain.

That is **source-bound historical evidence for an earlier head**, not for this one and not an
acceptance of mode (b). It is cited because it independently confirms the data-plane half of this
lane's findings on a tree small enough to walk by hand. It establishes no warm-base publication, which
neither lane could reach.

### A measured limit: a post-reset readback through a still-mounted export can lag

Independently observed by the reviewer, and avoided here rather than characterised:

```
0.00s after reset   24 entries   marker present   != base
2.00s after reset   23 entries   marker absent    == base
6.04s after reset   23 entries   marker absent    == base
12.02s after reset  23 entries   marker absent    == base
```

A fresh daemon reading the same store saw the correct state immediately, so the store is correct and
the live export view converges within about two seconds. The cause was not determined and is not
claimed; it is consistent with a cache-propagation delay. This is a **limit of that observation, not
a claim that reads are eventually coherent in general.**

It matters for mode (b) because a slot reset is routine on a slot an agent already has mounted. This
harness therefore unmounts and re-exports before reading back after a reset, which avoids the race
and also means this harness does not characterise it.

## Ownership, so nothing here is duplicated

| Seam | Owner |
| --- | --- |
| core `base_refresh` publication and the `can_ingest` gate | **#123, open**: define and implement tree-native Core warm-base publication for the companion |
| provenance persistence and `BaseMeta` reconstruction | provenance lane |
| `git worktree add` path resolution in `import.rs` | repair `b59bc3c`, unmerged; **not** touched here |
| `CowfsMaterialiser` calling the real `mount_snapshot` | companion lane; **not** touched here |
| child-open-FD refusal on a private slot | holder lane; **not** duplicated here |

No production source was changed in this lane.

## Honest limits of this evidence

- Path-backend readback is not core `fsck` and says nothing about crash durability.
- A surviving artifact proves nothing about publication, which is why the provenance gate exists.
- No speed or overhead claim is made. The machine is shared with eleven other workers. The
  build-overhead success criterion needs a quiet host and is still open. The wall-clock figures in the
  evidence file are records of what ran, not measurements of anything, and none is offered as a
  performance number.
- Mode (a), unmodified treehouse against the mount, is not measured here.
- The blocker reproductions are for this lane's head. The worktree-path repair and any provenance
  repair are later, unmerged heads and were not re-measured here.
- `main` has moved since this branch forked, and its merge changed production NFS code relative to
  the branch base. A run on merged `main` is a different measurement and nothing here speaks to it.
- The three binary digests are artifact identity only; build provenance is UNVERIFIED.
- The rlib digests differ between the native control and the in-slot build, and between runs. That is
  expected and is not a defect; no artifact byte identity is claimed.