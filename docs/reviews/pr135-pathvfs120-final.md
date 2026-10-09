# PR 135 final independent review: the #120 terminal-boundary repair

Reviewer verdict: **PASS on the contract, with two non-blocking documentation defects recorded.**
Merge gate unmet at my snapshot: CI had not finished.

This review did not merge, did not push, did not edit production source, did not lease, return, prune or
destroy anything, and did not close or open any issue.

## Verdict in one paragraph

The terminal check is correct for every case I could construct, it is correctly bounded, and the
claimed effect is real on the affected host: I independently reproduced the natural six-field stamp
collision on Linux ext4 and watched the old logic omit the externally created name 20 times out of 20
while the new logic listed it 20 times out of 20, with the same probe binary source against both
variants.
Two sentences in the merged evidence are wrong and should be corrected before merge, neither affects
behaviour: the scan-count bound is misstated, and the reproduction table mislabels the filesystem.
The scan-count counter is compiled out of release, the new test rejects both the old behaviour and a
per-page rescan, and `Cookies::sync` numbering is what makes the skip sound.
CI was `queued` on all three checks when I looked, so `UNSTABLE` is explained by pending runs and not
by a failure I observed.

## Identities under review

| item | value | verified how |
| --- | --- | --- |
| PR | https://github.com/zeeshanhaque21/cowfs/pull/135 | `gh pr view`, yes |
| head | `d192e734a6f9dd60510e20ac840c38d685cdda37` | local `git cat-file` and `gh pr view`, yes |
| base | `93cfef94457a989d031cb6b0a475ac4edbdb85ef` | equals `main` at review time, yes |
| branch | `fix/pathvfs-same-stamp-120` | yes |
| state | `OPEN`, `isDraft` false, `mergeable` MERGEABLE, `mergeStateStatus` UNSTABLE | GraphQL, yes |
| commits | 2, `2ee78277` then `d192e734` | GraphQL, yes |
| parent of first commit | exactly `93cfef9` | `git rev-parse 2ee7827^`, yes |
| review comments / reviews | 0 / 0 | REST, yes |
| `closingIssuesReferences` | empty | GraphQL, yes |
| issue 120 | `open`, `state_reason` null | REST, yes |
| issue 118 | `closed`, completed | REST, yes |
| `cargo fmt --all -- --check` | exit 0 | head archive, yes |
| `cargo clippy -p cowfs-vfs-path --all-targets -- -D warnings` | exit 0 | head archive, yes |
| head lib tests on APFS | 35 passed, 0 failed | head archive, yes |
| head lib tests on Linux ext4 | 35 passed, 0 failed | head archive, yes |

The two commits are cleanly split: `2ee78277` is `table.rs` plus `tests.rs`, `d192e734` is the two
evidence documents and nothing else.

### Diff shape, with the dispatch number corrected

The dispatch described `table.rs` as "97 ins 29 del".
That is the diffstat bar width, not an insertion count.
The actual numstat is **68 insertions and 29 deletions** in `table.rs`, which is 97 changed lines.
Nothing is wrong with the commit, the dispatch number was a misread of `--stat`.

| file | insertions | deletions |
| --- | ---: | ---: |
| `crates/cowfs-vfs-path/src/table.rs` | 68 | 29 |
| `crates/cowfs-vfs-path/src/tests.rs` | 119 | 0 |
| `docs/verification/evidence/pathvfs120-repair.md` | 219 | 0 |
| `docs/verification/evidence/pathvfs120-reproduction.md` | 251 | 0 |

Exactly four paths.
`cookies.rs`, `sys.rs`, `lib.rs` and `Cargo.lock` are not in the diff, and I confirmed the first three
are byte-identical between base and head by sha256, not merely absent from the path list:

```
cookies.rs  655704fd3b5ff487c1f28cc59ac181d773b58b7db4820cca6338c92fe91d3790  (base == head)
sys.rs      1105243ab2922247eb7a3ecb27e65b3973945f6281f360b5129ffe30b8c6b1e9  (base == head)
lib.rs      17b62cb88f166a5593da4f4136323d13c59d2200b6897f3a6d94c8da9b1c71c0  (base == head)
table.rs    e6a75bb0186188caecaad0ef9f3fdfd14dae439ac783e34e816e49237f674162  (base)
table.rs    46652a6d3b8d70e2c35d16c93a0e585ed91be0af02b15e9e0022cdcbe189c0af  (head)
tests.rs    8d4fc0013c6d99af8f9c19c8379bd15064c63307d0800f93fd3767118a4dc02b  (base)
tests.rs    7282cd1d62ed84688767de796303723b65c4c9e17bd999b6b4a514531bb8662e  (head)
```

Those five base-side sha256 values are exactly the ones the author's reproduction table publishes, so
the author's source identity table is corroborated rather than taken on trust.

### The 131 requirement, honoured exactly

`tests.rs` is a pure insertion file: the diff against base removes **zero** lines.
`fn force_observable_mtime` is present at `tests.rs:318` and still called at `tests.rs:352`, unchanged
and still in force.
That was a fixed constraint from the prior review of PR 131 and it is satisfied.

## Ownership and workspace, checked before anything else

I verified the assigned slot before using it, because the dispatch description of it was wrong in a
way worth recording.

The dispatch called ready-wave slot 14 a held lease left by the finished prior 131 critic.
The pool state file disagrees on the holder: `lease_holder` is `cowfs-g4-critic`, `base_branch` is
`verify/fsx-g4`, and the working tree carried three untracked PR 102 documents
(`mounted-fsx-g4-final.md`, `mounted-fsx-g4-repair-final.md`, `pr102-final-delivery.md`) with no
pathvfs 118, 131 or 135 artefact anywhere in it.

I did not treat that as disqualifying, and I did not repair it either: no acquire, no return, no
reset, no stash, no checkout, no branch change.
The prior report `docs/reviews/pr131-pathvfs118-final.md` names the same path, the same branch
`review/mounted-fsx-g4` at `f816b5e9`, and says "idle before use", and it lists those three documents
as prior-reviewer content it also left alone.
So this slot is a shared critic slot that successive read-only reviews reuse without a lease change,
and the prior artefacts there are not mine to touch.
I wrote only `bench/out/pathvfs120-final-critic/**` in that lease and my one canonical document in the
main checkout.
The prior `bench/out/pathvfs118-final-critic/**` and the three PR 102 documents were not modified.

Two other pools and their workers were out of scope and untouched: the build-train slot for the PR 134
critic and ready-wave slot 6 for the PR 132 lane.

## Source pin

Both variants were materialised with `git archive` into directories that were asserted absent first,
then verified file by file against the git manifest by recomputing the git blob id of every extracted
blob.

```
base  manifest_entries=641 blobs_verified=641 mismatches=0 extra_files=0
head  manifest_entries=643 blobs_verified=643 mismatches=0 extra_files=0
```

No tree was copied with `cp -a` and no mtime was preserved or trusted.
Each variant built with its own `CARGO_TARGET_DIR` and its own `TMPDIR`.
No target directory, binary or build output was seeded from another run.
The two tarballs carried to Linux hashed identically on both ends:

```
base.tar.gz  b936a8dd492b59cf81f627224c955464874f8b5205fe12b80ab0d37dcb87bd15
head.tar.gz  61042eb8923ad261a0b46cdb829ce4160be69fc49f26cc2e6f74e1902f96460d
```

## What the change actually does

`readdir_page` keeps the cached listing for the middle of a traversal, but when the page walk reaches
the end of the cached names it re-reads the directory before it will report `eof`.
The re-read result is pushed through `Cookies::sync`, so survivors keep their numbers, removed names
drop out, and names that appeared are numbered above every name already listed.
The loop then recomputes the cursor past the last cookie it actually emitted and runs again.
`eof` is computed once at the end as `cursor == listing.len()`.

Three properties make this sound, and I checked each rather than assuming them.

**Cookie numbering is strictly monotonic, so the cursor skip cannot re-admit a seen name.**
`Cookies::sync` assigns `self.next += 1` to a name it has never numbered before and never decrements or
reuses `next`, so a name that is removed and later recreated gets a fresh, higher number rather than
the freed one. `crates/cowfs-vfs-path/src/cookies.rs:18` and its own
`removed_then_recreated_name_gets_a_new_position` test agree.

**The loop is bounded by the caller's page, not by the churn.**
The inner `while` condition is `entries.len() < max`, so at most `max` entries are ever emitted, and
every loop iteration either emits at least one entry or breaks. Iterations are therefore bounded by
`max + 1` per call no matter what an external writer does.
There is no fixed verification budget, so there is no stale `eof` waiting to be accepted, and there is
no internal unbounded retry loop. The author's statement of this at `repair.md:47` to `repair.md:49` is
correct.

**Storing the pre-rescan stamp is the safe direction.**
At the end the code writes `n.listing_stamp = Some(stamp)` where `stamp` was read at entry, while
`n.listing` holds the post-rescan names. A stale, older stamp makes the next call more likely to
detect a change and restart, never less likely, so this cannot hide a change. It can cost one extra
scan, which is not a correctness problem.

## Contract cases, each run against the public API

I drove `PathVfs::readdir` through the `cowfs_vfs::Vfs` trait only, with external mutations through
`std::fs`, on the Linux host, on real directories.
Nothing internal was touched, no timestamp was ever written, and no comparison was mocked.

| case | result |
| --- | --- |
| empty directory, read three times, then a cookie past the end | empty and `eof` every time |
| single page of 5 names at `max` 100, then three repeated reads at the last cookie | 5 then empty and `eof`, no duplicates |
| 300 names drained, then 290 removed, fresh traversal | 10 survivors keep cookies 291 to 300, strictly increasing, terminal `eof` |
| tail deleted between pages, including a page that yields zero entries | `["a","b","c","d"]`, cookies strictly increasing, the zero-entry page still terminal |
| tail name renamed between pages | `["a","b","c","z"]`, 0 duplicates |
| colliding stamp with a tail name added on every resumption round, 20 attempts, 20 real collisions | every name present exactly once, no duplicates, every empty page terminal |

The sparse-cookie row needed a correction to reach.
My first attempt created 300 names and removed 290 without ever listing in between, so no cookie had
been assigned before the removals and the survivors were numbered 1 to 10, which is not a sparse
cookie at all.
Draining the directory while all 300 are present first is what makes the numbers sparse, and then the
resumed traversal really does return 291 to 300.
I am recording that because a passing-looking test that does not exercise the case it names is exactly
the failure mode this review is here to catch.

Every one of those cases passes.
The rename case is worth one clarification: a name deleted and recreated with the *same* name can be
emitted twice, because the recreated name takes a new higher cookie.
That is inside the contract, which is scoped to "names that were present throughout", and it is what a
native `readdir` does too, so I did not treat it as a defect.

## Independent Linux reproduction of the old failure and the new pass

This is the part that needed the affected host, and the part a green run on this Mac cannot supply.

Host: `moonscapenas`, Linux `6.12.109+rpt-rpi-2712 aarch64`, `rustc 1.95.0`,
`/home` on `/dev/sda2` which `findmnt` reports as **ext4**.

One probe binary source, sha256 `98a00beb5adbf78249ef9cfaf39f85da13ef800984ecd4434f00609385d23dff`,
compiled twice against the two archives, differing only in the path of its two dependency crates.
20 natural attempts per variant, the bounded maximum I allowed myself.
No forced mtime or ctime anywhere.

OLD, base `93cfef9`, no fix:

```
SUMMARY attempts=20 collisions=20 omitted_e_among_collisions=20 dup_names=0 empty_page_without_eof=0
ROW i=0 moved=false has_e=false dups=0 pages=3 names=["a", "b", "c", "d"] cookies=[1, 2, 3, 4]
```

NEW, head `d192e734`, with the fix, same probe source:

```
SUMMARY attempts=20 collisions=20 omitted_e_among_collisions=0 dup_names=0 empty_page_without_eof=0
ROW i=0 moved=false has_e=true dups=0 pages=3 names=["a", "b", "c", "d", "e"] cookies=[1, 2, 3, 4, 5]
```

All 20 attempts collided naturally on both variants, so this is a real collision case on every
attempt and not a lucky one.
The old variant omits the externally created `e` in 20 of 20 and the new variant lists it in 20 of 20,
survivor cookies 1 to 4 are unchanged, and the added name takes 5.
Both binaries built and both ran, so the old failure is a failure of the old logic and not a missing
build.

On this Mac the same head archive prints `moved=true` for the same fixture, because APFS advanced the
directory clock between the two stats.
That is why the Mac result carries no weight as proof of the repair, and I am not presenting it as
such.
The binding receipt is the Linux ext4 run above.

## The scan-count measurement, reproduced and then audited

I reproduced the author's static-listing table on a third host, APFS, and got the author's numbers
exactly:

```
SCANS page=7 pages=73 entries=500 scans=3
SCANS page=10 pages=51 entries=500 scans=3
SCANS page=1000 pages=2 entries=500 scans=3
```

A constant 3 across 73 pages and across 2 pages is the property that matters: the terminal check is
not one rescan per page, and it is not zero.
The new test asserts a lower bound of 2 and an upper bound of 4, which rejects the old `scans == 1` and
would reject a per-page rescan, and it additionally asserts `seen == 500` and a `pages < 5000`
termination guard, so it is a real guard on a real collection rather than a counter check on nothing.

The counter is `#[cfg(test)]`, it is a `thread_local` cell and not behind the state lock, and I
confirmed it is absent from release:

```
release rlib, grep -c for DIR_SCANS or dir_scan_count : 0
```

Both Mac and Linux release builds succeeded, so the wrapper `scan_dir` compiles in release as a plain
pass-through to `sys::list_dir` and the release behaviour is unchanged.

### Finding 1, non-blocking: the scan bound is misstated in the merged evidence

`docs/verification/evidence/pathvfs120-repair.md:125` to `repair.md:126` says:

> Under churn that adds a name at the tail on every pass, the count grows with the churn, bounded by the
> number of names present, and that is stated rather than hidden.

The first clause is right.
The bound is wrong, and I measured it rather than arguing it.
I added one bounded private test to a throwaway copy of the head archive, drained a directory to
completion after each of ten external tail additions, page size 1000, and read the production counter:

```
CHURN names_present=12 scans=33 ratio=2.75 scans_exceed_present=true
```

Thirty-three scans against twelve names present.
The scan count is not bounded by the number of names present and cannot be, because a small directory
churned repeatedly produces a scan count that grows with the churn while the live entry count stays
tiny.
The correct statement is the one already made correctly at `repair.md:47` to `repair.md:49`: within a
single call the loop is bounded by the caller's page size, and across calls the count is driven by
external change events, not by how many names exist.
The phrase in the `table.rs` doc comment that "a page-bounded caller already paces" it is in the same
family: the mechanism is the page bound, not the caller's pacing.

This is a documentation defect only.
It does not change the code, the measured 3-scan result for an unchanged listing is unaffected, and no
performance or no-regression claim rests on the wrong sentence.
I recommend correcting the two sentences before merge so the merged evidence does not carry a bound
that is measurably false.

### Finding 2, non-blocking: the reproduction table mislabels the filesystem

`docs/verification/evidence/pathvfs120-reproduction.md:14` records the 20-of-20 collision row against
`ext2/ext3`.

There is no ext2 or ext3 filesystem anywhere on that host.
`findmnt` reports ext4 on `/dev/sda2`, and a search of every mount returns `NONE` for `^ext[23]`.
The row that actually ran was ext4.

This matters more than a typo would, because ext2, ext3 and ext4 do not share timestamp granularity
behaviour, and the whole point of the document is to characterise which host clock collides.
The label should say ext4.

I did not rerun the tmpfs row, so I neither confirm nor dispute it.

### Gate item, not a defect: CI had not finished

One snapshot, taken once, not polled and not rerun:

```
check (macos-latest)  queued  null  started=2026-10-05T19:36:58Z  completed=null
check (ubuntu-latest) queued  null  started=2026-10-05T19:36:58Z  completed=null
linux-fuse           queued  null  started=2026-10-05T19:36:58Z  completed=null
workflow: ci  run 526  status queued
```

The workflow exists and is active, so this is not an unconfigured-workflow case and not a zero-check
case: three checks are configured and all three were still queued.
`mergeStateStatus` UNSTABLE is consistent with pending runs.
The PR body itself says CI on the exact head is required before merge, so this is the outstanding
merge gate, not a finding against the change.
Someone should re-read the three conclusions after they land.

## What I deliberately did not do

- I did not rerun the author's heavy conformance tier, the 19 checks including the 50,000-entry case.
  I carried that evidence forward as the author's and did not restate it as mine.
- I made no performance claim and no no-regression claim.
  The scan counts above are counts.
  No timing was taken, no throughput was measured, and no thermal or budget number was guessed.
- I ran `-p cowfs-vfs-path` only, not the whole workspace test suite.
- I used no live daemon, socket or mount at any point.
- I did not initialise or run `no-mistakes`.
- Browser-based verification is UNVERIFIED; nothing here depends on a UI.
- I did not dispatch other agents, touch leases, signals, runners, installs, sudo, the shared store, or
  any other worker's paths.

## Recommendation

**PASS on the contract.**
The repair does what it claims, on the affected host, with a bound that holds and a counter that cannot
leak into release.
Merge once the three queued CI checks report, after correcting the two sentences in
`pathvfs120-repair.md` and the filesystem label in `pathvfs120-reproduction.md`, and after a reviewer
confirms those corrections did not disturb the measurements those documents report.
Leave issue 120 open until the merge lands, which is what the PR body already says and what
`closingIssuesReferences` being empty already guarantees.

## Evidence index

Everything below is under
`.treehouse-ready-wave/.treehouse/cowfs-7c1bf8/14/cowfs/bench/out/pathvfs120-final-critic/` in the
assigned lease, and the remote half under
`/home/moonscape/cowfs-ready-wave/task-pathvfs120-final-critic/` on `moonscapenas`.

| path | what it holds |
| --- | --- |
| `logs/head-libtest.log` | 35 head tests on APFS |
| `logs/head-newtests-nocapture.log` | the `STAMP`, `LISTING` and `SCANS` lines |
| `logs/head-fmt.log`, `logs/head-clippy.log`, `logs/head-release.log` | exit-0 quality and release logs |
| `logs/churn-probe.log` | the `CHURN` measurement that falsifies finding 1 |
| `src-base/`, `src-head/`, `src-head-probe/` | the verified archives, plus one probe copy |
| `lt-base.txt`, `lt-head.txt` | the git manifests used for the blob verification |
| `probe/`, `probe2/`, `probe3/` | the three probe sources |
| `tar/` | the two tarballs that crossed to Linux |
| `remote-run.sh` to `remote-run5.sh` | the bounded remote runners, each with a source sha guard |

Reproduce the Linux half with, from the lease:

```
scp -o BatchMode=yes -q probe/src/main.rs remote-run.sh moonscape@192.168.68.119:$T/
ssh moonscape@192.168.68.119 "flock -w 600 /home/moonscape/cowfs-ready-wave/linux-heavy.lock bash $T/remote-run.sh 20"
```

That is one foreground command per host under the shared heavy lock, 20 attempts, no installs, no
root, no shared store, no mount, no jobs.