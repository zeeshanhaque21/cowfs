# PR 135 erratum: current claims that supersede two wrong ones in the merged evidence

Refs #120.

This document supersedes specific sentences in two files that stay on disk unchanged.
It is not a rewrite of them and it does not replace their receipts.

| file | status | sha256 |
| --- | --- | --- |
| `docs/verification/evidence/pathvfs120-repair.md` | immutable, unchanged, historical | `994a03063aec5e7b5ceb3278472392d70f622d7e2a1c2d54faebdaee0d701672` |
| `docs/verification/evidence/pathvfs120-reproduction.md` | immutable, unchanged, historical | `bec6cd080cc691046930e7108e5954610ae7976515df8743eff320e1eaa6d61e` |
| `docs/reviews/pr135-pathvfs120-final.md` | independent review, unchanged | `f987721ee6fc62d3ac25fbca6a8412bb4e6e19a8f0e59763cd2181331807547c` |

The review passed the change and recorded two non-blocking documentation defects.
Both are documentation defects: neither changes the code, and the measured results they sit next to
are unaffected.
Both are corrected here rather than by editing the immutable files, so the history of what was claimed
and when stays readable.

No code, test, or `Cargo.lock` change accompanies this erratum.
No test was rerun for it.

## Erratum 1: the filesystem label was wrong, and there is no tmpfs row at all

`pathvfs120-reproduction.md` line 14 records the 20-of-20 collision row against `ext2/ext3`.
That is wrong, and not merely a typo.
The independent review checked it with `findmnt` on the same host:

```
/home               -> /dev/sda2  ext4
findmnt -rn -o FSTYPE | grep -cE '^ext[23]$'  ->  0
```

There is no ext2 and no ext3 filesystem anywhere on that host.
The row that ran was **ext4**.
The same label is repeated at `pathvfs120-repair.md` line 64 and in the limitation table at lines 177
to 178.

This matters beyond wording, because ext2, ext3 and ext4 do not share timestamp granularity behaviour,
and the whole purpose of naming a filesystem in those documents was to say which host clock collides.
An `ext2/ext3` label invites exactly the wrong portability conclusion.

**Corrected current fact: the Linux collision evidence is ext4.**

### The tmpfs row is also wrong, and this one is mine alone to retract

The independent review explicitly did not rerun the tmpfs row and neither confirmed nor disputed it.
I checked it, because the same question applies.

My own probe log records the directory each row ran in:

```
sample-ext-1.log     PROBE base=/home/moonscape/cowfs-ready-wave/task-pathvfs120-reproduction/fixture/ext
search-ext-20.log    PROBE base=/home/moonscape/cowfs-ready-wave/task-pathvfs120-reproduction/fixture/ext
search-tmpfs-20.log  PROBE base=/home/moonscape/cowfs-ready-wave/task-pathvfs120-reproduction/fixture/tmpfs
```

Both directories are under `/home`, which `findmnt` reports as `/dev/sda2 ext4`.
The directory named `fixture/tmpfs` was a directory whose name was `tmpfs`, on ext4.
`/tmp` and `/dev/shm` are tmpfs on that host, and neither was used.

**Corrected current fact: there is no tmpfs evidence in this work.**
The `19/20` row in `pathvfs120-reproduction.md` line 15 is a 19-of-20 ext4 result wearing the wrong
label, and the row in `pathvfs120-repair.md` line 178 that credits "Linux tmpfs" is withdrawn.
The repair was demonstrated on ext4 only.

That does not weaken the reproduction.
The ext4 result is real, it is the one the independent review reproduced 20 of 20, and ext4 is the
host where the original failure was measured.
It does mean the portability claim in those documents was overstated by one filesystem, and no tmpfs
result may be cited from this work.

## Erratum 2: the churn bound was false, and the reviewer measured the falsifier

`pathvfs120-repair.md` lines 125 and 126 say:

> Under churn that adds a name at the tail on every pass, the count grows with the churn, bounded by the
> number of names present, and that is stated rather than hidden.

The first clause is right.
The bound is wrong and cannot be right, because a small directory churned repeatedly grows its scan
count while the live entry count stays tiny.

The independent review measured exactly that, on a throwaway copy of the head archive, draining the
directory to completion after each of ten external tail additions, page size 1000, reading the
production counter:

```
CHURN names_present=12 scans=33 ratio=2.75 scans_exceed_present=true
```

Thirty-three scans against twelve names present.
The count is not bounded by the number of names present.

**Corrected current fact, in two parts, per call and across calls.**

Within a single `readdir_page` call, the work is bounded by the caller's page size: the loop either
emits at least one entry or exits, so one call costs at most `max` plus one entries' worth of work
however many external writes land in between.
This is the statement already made correctly at `pathvfs120-repair.md` lines 47 to 49, and it is the
one that governs.

Across calls, the scan count is driven by external change events, not by how many names exist.
A caller that keeps a directory churning will pay a scan per boundary crossing, and the total is
bounded by the number of those events, which is the caller's own write rate.

The mechanism is the page bound, not the caller's pacing.
The phrase in the `table.rs` doc comment, "which a page-bounded caller already paces", is in the same
family and overstates it the same way; the code comment is not edited here because this erratum
authorises no source change, and the code comment does not make a false factual claim about a bound.

### What survives unchanged

The static result is not touched by either erratum and remains the proof it was:

```
SCANS page=7    pages=73 entries=500 scans=3
SCANS page=10   pages=51 entries=500 scans=3
SCANS page=1000 pages=2  entries=500 scans=3
```

A fixed count across 73 pages and across 2 pages is still the property that matters: the terminal check
is not one rescan per page, and it is not zero.
The reviewer reproduced these numbers exactly on a third host.

That is a **scan count**, not a performance result.
No timing, throughput, or "no regression" claim is made anywhere in this erratum, and none was made in
the two immutable files.
The 50,000-entry conformance check and the other 18 heavy `readdir` checks were the author's, were not
rerun by the reviewer, and are carried forward as the author's, not restated as the reviewer's.

## Independent review, carried as the reviewer's

Reviewer: PASS on the contract, two non-blocking documentation defects, merge gate unmet at the
reviewer's snapshot because CI had not finished.

Reproduced independently on Linux ext4, one probe binary source sha256
`98a00beb5adbf78249ef9cfaf39f85da13ef800984ecd4434f00609385d23dff`, compiled against both archives,
20 natural attempts per variant, no forced mtime or ctime anywhere:

```
OLD  base 93cfef9     collisions=20  omitted_e_among_collisions=20  dup_names=0
NEW  head d192e734    collisions=20  omitted_e_among_collisions=0   dup_names=0
ROW i=0 OLD moved=false has_e=false names=["a","b","c","d"]   cookies=[1,2,3,4]
ROW i=0 NEW moved=false has_e=true  names=["a","b","c","d","e"] cookies=[1,2,3,4,5]
```

That is the same real collision the author measured, 20 of 20 on both variants, and it binds the
review to the source under review rather than to a summary of it.

Contract cases the reviewer drove through the public `Vfs` trait only, with external mutations through
`std::fs`, no timestamp ever written and no comparison mocked, all passing:

| case | reviewer result |
| --- | --- |
| empty directory, three reads, then a cookie past the end | empty and `eof` every time |
| single page of 5 names at `max` 100, then three reads at the last cookie | 5 then empty and `eof`, no duplicates |
| 300 names drained, 290 removed, fresh traversal | 10 survivors keep cookies 291 to 300, strictly increasing, terminal `eof` |
| tail deleted between pages, including a page yielding zero entries | `["a","b","c","d"]`, cookies strictly increasing, zero-entry page still terminal |
| tail name renamed between pages | `["a","b","c","z"]`, 0 duplicates |
| colliding stamp with a tail name added every resumption round, 20 attempts, 20 real collisions | each name present exactly once, no duplicates, every empty page terminal |

The reviewer's own note on its sparse-cookie row is recorded rather than dropped: a first attempt that
created 300 names and removed 290 without listing in between numbered the survivors 1 to 10, which is
not a sparse cookie, and draining the directory while all 300 are present first is what makes the
numbers sparse.

One case is inside the contract and is recorded as not a defect: a name deleted and recreated with the
same name can be emitted twice, because the recreated name takes a new higher cookie. The contract is
scoped to names present throughout, and a native `readdir` does the same.

Release-boundary checks the reviewer ran: the `#[cfg(test)]` counter is absent from the release rlib
(`grep -c` for `DIR_SCANS` or `dir_scan_count` returns `0`), and both release builds succeeded, so the
`scan_dir` wrapper compiles in release as a pass-through and release behaviour is unchanged.

The reviewer also corrected a figure I reported in dispatch: `table.rs` is **68 insertions and 29
deletions**, 97 changed lines. The 97 was the `--stat` bar width, not an insertion count.
Actual numstat: `table.rs` 68/29, `tests.rs` 119/0, and the two evidence documents as committed.

## CI at the reviewer's snapshot, and what is still outstanding

The reviewer's single snapshot, not polled and not rerun: `check (macos-latest)`, `check
(ubuntu-latest)` and `linux-fuse` all `queued`, workflow `ci` run 526 `queued`.
Three checks are configured and the workflow is active, so this is not an unconfigured case and not a
zero-check case.
`mergeStateStatus` `UNSTABLE` is consistent with pending runs.
That is the outstanding merge gate, not a defect against the change.

A fresh CI snapshot on whatever head this erratum produces is required before merge, and the three
conclusions must be re-read once they land.

## Current state summary

| claim | status |
| --- | --- |
| Linux natural six-field collision, ext4, reproduced 20/20 OLD and 20/20 NEW | confirmed independently |
| The old logic omitted the external name, the new logic lists it | confirmed independently |
| Cookie, inode and export semantics unchanged; `cookies.rs`, `sys.rs`, `lib.rs`, `Cargo.lock` untouched | confirmed |
| Scan count constant at 3 for an unchanged listing across 73, 51 and 2 pages | confirmed independently |
| No tmpfs evidence exists in this work | corrected here |
| The filesystem of the Linux collision evidence is ext4, not ext2/ext3 | corrected here |
| Scan count is bounded per call by page size; across calls it follows external change events | corrected here |
| Scan count is bounded by the number of names present | **retracted, measurably false** |
| No performance or no-regression claim | held throughout |
| Not snapshot isolation, not cross-page atomicity | held throughout |
| `CONFIG_HZ` and `ubuntu-latest` granularity | still unmeasured |
| Independent review and exact-head CI before merge | outstanding |

`#120` stays open and nothing here closes it.