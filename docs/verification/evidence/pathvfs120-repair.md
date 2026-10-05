# Issue #120 repair: check the directory before a listing reports end of file

Refs #120, behind it #118 and #19.

The reproduction this rests on is `docs/verification/evidence/pathvfs120-reproduction.md`, sha256
`bec6cd080cc691046930e7108e5954610ae7976515df8743eff320e1eaa6d61e`, accepted.
The #118 documents are unchanged.

| | |
| --- | --- |
| branch | `fix/pathvfs-same-stamp-120` |
| head | `2ee782776d4fab846ce4ea4a80fad6ec8c3d4615` |
| base | `main` at `93cfef94457a989d031cb6b0a475ac4edbdb85ef` |
| production diff | `crates/cowfs-vfs-path/src/table.rs` only |
| test diff | `crates/cowfs-vfs-path/src/tests.rs`, two new tests |
| untouched | `cookies.rs`, `sys.rs`, `lib.rs`. No new public API. No new dependency |

## The contract this implements, and its boundary

A name that appeared in the directory before a pagination resumes is visible in the resumed listing,
even when all six directory stamp fields are identical on both sides of that change, with no duplicate
and no omitted entry among the names that were present throughout.

What that does not promise, stated because it is easy to overread:

It is not filesystem snapshot isolation.
It is not atomicity across pages.
It does not put a name into the page it appeared in, and it never will: `Cookies::sync` numbers a new
name after every name already listed, so a new name cannot appear before the old survivors by
construction. A caller that wants it earlier needs a different cookie design, which this does not
attempt.
A change landing in the middle of a page is out of scope; that page may or may not contain it, as
before.
An entry that disappeared is still handled by the existing `fstatat` `ENOENT` skip, which this change
leaves in place and does not excuse.

## What was wrong

`readdir_page` had exactly one invalidation mechanism: one `fstat` folded into the six-field tuple at
`table.rs` lines 176 to 184, compared field by field. When the host leaves all six equal, the cached
listing is not merely stale, it is indistinguishable from a current one, and `eof` was computed from
the cache alone, so it asserted a completeness about the directory that nothing had checked.

## The change

When the pages run out of cached names, the call reads the directory itself before it reports `eof`.
The loop is bounded by the caller's page: every iteration either emits at least one entry or exits, so
a directory churning under a page-bounded caller costs the existing multiple calls, not an internal
unbounded scan.

No fixed verification limit is involved, so there is no stale `eof` waiting to be accepted: if new
names extend the tail, `cursor` lands on them, the loop runs again, and the boundary is checked again
against the refreshed tail before completion is ever declared.

Cookie, inode, and export semantics are untouched. `Cookies::sync` was already correct for this and was
not modified: survivors keep their numbers, a removed name drops out, a new name sorts last. That is
what makes the refreshed `partition_point(cookie <= last)` skip correctly, so no stable name is
emitted twice and none present throughout is dropped.

The scan counter used by the tests is `#[cfg(test)]` only, so no production observability was added.

## Proof, old then new, on the same real host

Both variants ran on `moonscapenas`, Linux `6.12.109+rpt-rpi-2712 aarch64`, rustc 1.95.0, ext2/ext3.
One fresh archive per variant, one `CARGO_TARGET_DIR` per variant, one private `TMPDIR`, one bounded
600-second `linux-heavy.lock`. The old variant carries the scan instrumentation and the new tests but
not the fix, so the old failure is a real failure of the old logic and not a missing build.

Archive sha256: old `e658b362f3340d3d2dea17d9dff51288aa03a1eddfda15e95a4d5eccc511b212`,
new `6eadacd1ed10b7a8f51386949c3f19ae7594907a0dc20677b31a5184b8772a0a`.
Each was extracted fresh, so no target directory or mtime was carried over.
The run asserted in each variant that `listing_stamp != Some(stamp)` is still present,
`fn force_observable_mtime` is still present, and that the new test is in `--list` before running it.
Every case derives "did it run" from libtest's own `34 filtered out` accounting, so 35 tests exist and
exactly one ran.

### OLD: natural six-field collision, external name omitted

```
STAMP stamp_before=(1791228500, 838707262, 1791228500, 838707262, 4096, 2)
      stamp_after =(1791228500, 838707262, 1791228500, 838707262, 4096, 2) moved=false
LISTING pages=3 names=[[97], [98], [99], [100]] cookies=[1, 2, 3, 4]
exit=101, 1 test ran, 0 passed; 1 failed
```

`[[97], [98], [99], [100]]` is `a, b, c, d`. `e` is `[101]`.
All six fields identical including both nanosecond fields, naturally, with nothing forced.
The scan-count test also failed here, reporting `scans=1`, which is the honest old behaviour: nothing
ever checked the directory at the end.

### NEW: same collision, external name present

```
STAMP stamp_before=(1791228693, 306768624, 1791228693, 306768624, 4096, 2)
      stamp_after =(1791228693, 306768624, 1791228693, 306768624, 4096, 2) moved=false
LISTING pages=3 names=[[97], [98], [99], [100], [101]] cookies=[1, 2, 3, 4, 5]
exit=0, 1 test ran, 1 passed
```

`moved=false` is the same real collision, so this is not passing through the pre-existing
stamp-invalidation path.
`[101]`, the external `e`, is now listed, and `cookies=[1, 2, 3, 4, 5]` shows the three survivors
keeping their numbers and the added name taking the next one, with no gaps.
The `a` in the listing is still the legitimate page-1 output from before the removal, not a stale
emission, exactly as in the reproduction.

## The scan bound, measured not asserted

A `#[cfg(test)]` counter wraps the single directory-read site, so nothing is exposed in production.
500 entries, three page sizes, Linux and APFS alike:

| page size | pages taken | entries | directory scans, Linux | directory scans, APFS |
| ---: | ---: | ---: | ---: | ---: |
| 7 | 73 | 500 | 3 | 3 |
| 10 | 51 | 500 | 3 | 3 |
| 1000 | 2 | 500 | 3 | 3 |

A fixed count across 73 pages and across 2 pages is the property that matters: it is not one scan per
page, which is what the always-rescan option would cost, and it is not zero.
The test asserts a lower bound of 2 and an upper bound of 4, so the old `scans=1` fails it and a
future per-page rescan would fail it too.

This is a scan count, not a performance result.
No timing comparison, throughput number, or "no regression" claim is made.
Under churn that adds a name at the tail on every pass, the count grows with the churn, bounded by the
number of names present, and that is stated rather than hidden.

## Correctness at scale, and the existing change-during-listing checks

The `readdir` conformance category, heavy tier so the 50,000-entry check runs, on the patched tree,
with a private native directory and strict mode. 19 checks, all `ok`:

```
readdir_empty_directory, readdir_one_entry, readdir_5000_entries,
readdir_never_lists_dot_entries, readdir_cookies_resume_after_every_entry,
readdir_max_one, readdir_max_zero_is_invalid, readdir_attrs_matches_readdir,
readdir_eof_flag_is_exact, readdir_cookies_distinct_and_nonzero,
readdir_stable_order, readdir_entries_match_lookup, readdir_on_file_is_not_dir,
readdir_delete_returned_entries_between_pages,
readdir_delete_upcoming_entries_between_pages,
readdir_delete_everything_between_pages, readdir_add_entries_between_pages,
readdir_resume_from_removed_entry_cookie, readdir_50000_entries
```

`readdir_50000_entries` passed on both hosts: 5.42s on Linux, 6.33s on APFS, both over page sizes 97,
4096 and 100000. That check asserts no duplicate names, no duplicate cookies, no zero cookie, the same
order at every page size, and that every one of the 50,000 names appears.
The four checks that mutate the directory between pages, plus the removed-cookie resume, are the ones
this change could most plausibly break, and they pass unchanged.

## Scoped checks, both hosts, true exit codes

| check | Linux | APFS |
| --- | --- | --- |
| new regression, `--exact`, one test | 0 | 0 |
| new scan-count test, `--exact`, one test | 0 | 0 |
| `cargo test --locked -p cowfs-vfs-path --lib`, all 35 | 0, `35 passed; 0 failed; 0 ignored` | 0, `35 passed; 0 failed; 0 ignored` |
| `cargo fmt --all -- --check` | 0 | 0 |
| `cargo clippy --locked -p cowfs-vfs-path --all-targets -- -D warnings` | 0, no diagnostics | 0, no diagnostics |
| `readdir` conformance, heavy, 19 checks | 0 | 0 |

An earlier Mac clippy line printed the exit of the `grep` in a pipeline rather than clippy's own exit,
which read as a failure. Re-run capturing clippy's own exit: `0`, with zero `error` or `warning` lines.
Reporting it rather than quietly dropping it.

`Cargo.lock` is unchanged from `93cfef9` and no lockfile was generated. The independent probe crate
used for the reproduction stays under `bench/out`, outside the repository, and ships nothing.

## APFS measured limitation, stated as such

On APFS the new regression prints `moved=true`: the seconds are equal and only the two nanosecond
fields move, so the pre-existing stamp comparison already invalidates the cache and the terminal check
is never the thing that makes the test pass here.

| host | `moved` | six-field collision | what makes the test pass |
| --- | --- | --- | --- |
| Linux ext2/ext3 | false | yes | the terminal check |
| Linux tmpfs | false | yes | the terminal check |
| APFS | true | no | the pre-existing stamp comparison |

So APFS cannot reproduce #120 and cannot demonstrate this repair working.
The repair is demonstrated on Linux, on both ext2/ext3 and tmpfs, where the collision is real.
APFS is a limitation of the reproduction, not evidence of a fix, and a green APFS run must not be
reported as the latter.

## What this does not settle

`CONFIG_HZ` and `ubuntu-latest` granularity are still unmeasured. Two filesystems on one Linux kernel
is not a portability result, and the conformance numbers above come from one host each.

Nothing here is a filesystem redesign, and there is no path watcher, `inotify`, `FSEvents`, new
platform API, whole-listing hash per page, TTL, or sleep.

Independent review and CI on the exact head are required before this merges. #120 stays open until
they do, and nothing here claims the old #131 CI event's cause.

## Artifacts

| artifact | sha256 |
| --- | --- |
| `old-tests-only.tar.gz` | `e658b362f3340d3d2dea17d9dff51288aa03a1eddfda15e95a4d5eccc511b212` |
| `new-patched.tar.gz` | `6eadacd1ed10b7a8f51386949c3f19ae7594907a0dc20677b31a5184b8772a0a` |
| `linux-logs/run.log` | `1bfebe15206d78cc43b75ee488e74e22090517639235ff8cd233c7e2d27ad995` |
| `linux-logs/old-new.log` | `4994158d8037c6db5815ef5180059ffde7cd8417c5ccddfb0531827e75a096c7` |
| `linux-logs/old-scan.log` | `32e37b420654d66425c0236e0f6fd540452ec3689c21ed289adfb79362b8ff76` |
| `linux-logs/new-new.log` | `5d74d74911ef82f7fc56a50e9ef3fae43224a0af564c4e4e05561fd44eaa9f74` |
| `linux-logs/new-scan.log` | `0fd39ed1fbb17baf7eec2f13b2275ba8c3dfb028e571d03d2e6fe858517268c4` |
| `linux-logs/suite.log` | `48fc6245d3fbd58e14b6b6637d3386acee4dac52f7666ac0e3f9ffb653ef949c` |
| `linux-logs/conformance-readdir.log` | `42c3772645a6c031e57a71f9b1f7b7208c74576213eacd5010bd630a8c4e261a` |
| `linux-logs/clippy.log` | `bfb62f38a28ba3f0cd02d9c43fd19db8446ed7497a658a6bd9f3dd7341753f21` |

Under `bench/out/pathvfs120-repair/` in the held worktree, which is gitignored, beside the untouched
#118 and #120 reproduction artifacts.

Remote residue is confined to `/home/moonscape/cowfs-ready-wave/task-pathvfs120-repair/`, a new
subdirectory holding one extracted tree and one target directory per variant, its logs and its
fixtures. The earlier `task-pathvfs120-reproduction/` directory is preserved untouched.
No install, sudo, mount, shared store, runner change, daemon, signal, or other agent's directory or
lease was touched. One bounded foreground lock on each host.