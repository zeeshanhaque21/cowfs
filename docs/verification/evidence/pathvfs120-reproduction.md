# Issue #120 reproduction: PathVfs omits an externally created name when the directory stamp collides

Refs #120, behind it #118 and #19.
This is a reproduction and a proposal, not a fix.
No production source was changed, no test was changed, no issue was closed.

## Verdict

**REPRODUCED on Linux, through the public `PathVfs` API, at current `main`.**
**NOT REPRODUCED on APFS.**

| host | filesystem | natural stamp collisions | resumed listing missed `e` | verdict |
| --- | --- | ---: | ---: | --- |
| `moonscapenas`, Linux `6.12.109+rpt-rpi-2712 aarch64` | ext2/ext3 | 20/20 | 20/20 | REPRODUCED |
| `moonscapenas`, same host | tmpfs | 19/20 | 19/19 colliding | REPRODUCED |
| Mac, `Darwin 25.6.0 arm64` | APFS | 0/20 | 0/20 | NOT REPRODUCED |

The APFS result says nothing about a fix, because nothing was fixed.
It says APFS's own directory clock moved on every attempt, so the collision this issue is about did
not occur there.
A green run on this Mac must not be reported as evidence that the Linux behaviour is handled.

Sample sizes are the attempt counts above, 20 each, the most the bounded search was allowed.
This is one representative complete sample plus a bounded search for a natural fixture, not a
frequency, stress, or performance study.

## Source under test

Everything ran against a fresh `git archive` of `main` at
`93cfef94457a989d031cb6b0a475ac4edbdb85ef`, extracted into a private directory, with an isolated
`CARGO_TARGET_DIR` and a private project-local `TMPDIR`.
No target directory, binary, or mtime was copied or seeded from another run.

| file | sha256 of the extracted copy, on the Linux host | git blob at `93cfef9` |
| --- | --- | --- |
| `crates/cowfs-vfs-path/src/table.rs` | `e6a75bb0186188caecaad0ef9f3fdfd14dae439ac783e34e816e49237f674162` | `d61ca4226accb9db1bd15d739c0ccda43d8276d0` |
| `crates/cowfs-vfs-path/src/cookies.rs` | `655704fd3b5ff487c1f28cc59ac181d773b58b7db4820cca6338c92fe91d3790` | `599de121881e3ffc769cb1da5038240a87897fea` |
| `crates/cowfs-vfs-path/src/sys.rs` | `1105243ab2922247eb7a3ecb27e65b3973945f6281f360b5129ffe30b8c6b1e9` | `e94ebf03015fb557c4d953e4215382f092babcff` |
| `crates/cowfs-vfs-path/src/lib.rs` | `17b62cb88f166a5593da4f4136323d13c59d2200b6897f3a6d94c8da9b1c71c0` | `bf48900a9fb44394f157bc572d547424e7ea2336` |
| `crates/cowfs-vfs-path/src/tests.rs` | `8d4fc0013c6d99af8f9c19c8379bd15064c63307d0800f93fd3767118a4dc02b` | `3577e61a56cd2f6f8f1decfe5ff1be0e4304110d` |
| `bench/compare.py` | `07db8641b027b6b055170b1e00e5b50ff349cdcd170a81deb27b88400888ab22` | `1376b8fc543a7a9bd3fbd5df536345efc84a8d6b` |
| `bench/test_gates.py` | `2b2f670cc318b0e8b1547c41313f342222dda8e3d25fabf8d65ca6218a105429` | `98e0f277e64dcdcf10f1e748e77928c903c13857` |

Those blob ids are exactly the ones delivered and reviewed for #118, including
`3577e61a56cd` for `tests.rs`, so this runs the same test-side source that #118 delivered.
The remote host has no `.git` for the archive, so identity was re-derived there by sha256 and matched
against the sha256 of those same blobs, which is why the table carries both columns.
Guards the run also asserted: `listing_stamp != Some(stamp)` present in `table.rs` (1),
`fn force_observable_mtime` present in `tests.rs` (1), and `set_times`/`utimens` in the probe (0).

### How the probe was added without touching a tracked file

The probe is a new workspace member crate under `crates/pathvfs120-probe`, which the root manifest's
`members = ["crates/*"]` glob picks up on its own.
Not one tracked file was edited: every file of the archive was compared by blob against `93cfef9` and
all of them matched, so the probe is pure addition.

It uses only public surface: `PathVfs::new`, the `cowfs_vfs::Vfs` trait, `ROOT_INO`, and plain
`std::fs` for the external mutations.
It never touches an internal, never sets a timestamp, and never alters the production comparison, so
nothing here can manufacture a collision or hide one.

`Cargo.lock` needed exactly one added stanza, the `pathvfs120-probe` package, and nothing else.
An initial `cargo generate-lockfile --offline` was rejected because it silently upgraded
`libc 0.2.189 to 0.2.190`, `memchr 1.5.1 to 1.6.0` and two more, which would have changed what was
actually compiled.
The lock was therefore restored from `93cfef9` and only the new package stanza inserted, and the
version census re-checked: 129 packages to 130, the only changed entry being `pathvfs120-probe`.

## The reproduction, one complete record

From the Linux host, ext2/ext3, `sample-ext-1.log`.

```
REPRO attempt=0 FULL RECORD
  native_before=["a", "b", "c", "d"]
  native_after =["b", "c", "d", "e"]
  stamp_before =(1791227935, 274526951, 1791227935, 274526951, 4096, 2)
  stamp_after  =(1791227935, 274526951, 1791227935, 274526951, 4096, 2)
  size_delta   =0
  resumed_names   =["a", "b", "c", "d"]
  resumed_cookies =[1, 2, 3, 4]
  fresh_from_start=["b", "c", "d", "e"]
  note=stamp identical across the external change, so the Vfs cache has no signal; resumed listing omitted e
```

Reading it field by field:

The six fields are `mtime.0, mtime.1, ctime.0, ctime.1, size, nlink`, in that order, the same six and
the same order `table.rs` builds at lines 176 to 184.
Every one of them is identical before and after the external change, including both nanosecond fields.
The change was natural: the host's own clock did not move, and the probe wrote nothing to force it.

`size` is the field worth dwelling on, because it looks like it should have caught this.
A create and a removal net to the same entry count, so `size_delta=0`, and `size` stayed at `4096`
across both operations.
`nlink` is 2 before and after, as a directory with no subdirectories always is.
So on this host only a timestamp could have distinguished the two states, and the timestamp did not
move.

`native_after` is the do-nothing control for the whole claim: an ordinary enumeration of the same
directory at the same instant sees `["b", "c", "d", "e"]`.
The file `e` exists on the backing filesystem, `std::fs::write` succeeded, and the external removal of
`a` succeeded.
Nothing is wrong with the directory.

`fresh_from_start` is the do-nothing control for the cache: the same `PathVfs`, the same directory, the
same instant, one call later, and it reports `["b", "c", "d", "e"]`.
`readdir_page` always restarts when `cookie == 0`, so that call never consults the cache.
`PathVfs` can see `e` perfectly well the moment the cache is not in the way, so this is not an inode
table, `register`, `attr_of`, or `fstatat` problem.

`resumed_names` is `["a", "b", "c", "d"]`, the paged listing joined across both pages.
It is missing `e`.
The `a` in that list is not a stale emission, and it must not be read as one: page 1 already returned
`a` and `b` before the removal happened, so those two entries are the legitimate page-1 output of an
ordinary paged readdir.
The resumed page started at `partition_point(cookie <= 2)`, which on the retained four-name cached
listing is index 2, so only `c` and `d` were even candidates on the resume.
The single defect visible in the record is the omission of `e`.

`resumed_cookies` is `[1, 2, 3, 4]`, dense and gap-free, so the cookie sequence itself was not
corrupted.
That matters for the proposal, because the surviving cookies are the thing any repair must preserve.

`PAGING duplicate_names_in_resumed_listing=0`, so no duplicate entry appeared in any colliding
attempt on any filesystem.
The failure mode is omission only, on this evidence.

## Exactly what current code does, and why it collides

`readdir_page`, `crates/cowfs-vfs-path/src/table.rs`, lines 171 to 224.

On every call it `fstat`s the directory once and folds the result into the six-field tuple.
`restart` is true when `cookie == 0`, or when there is no cached listing, or when
`n.listing_stamp != Some(stamp)`.
When `restart` is false the cached `listing` is taken as-is and served from.

That is the entire invalidation mechanism: one `fstat`, compared field by field.
There is no second source of truth, no generation counter, no dirty flag from another path.
So when the host leaves all six fields equal, the cached listing is not merely stale, it is
*indistinguishable* from a listing that is still current, and the code takes the reuse branch with no
way to know otherwise.
The doc comment above the function states this as the design, not as an oversight: the directory is
read again only when the call starts from the beginning or when the directory itself changed, "which
is the only signal that a name appeared or vanished without `PathVfs` doing it".

`Cookies::sync` is not implicated.
On the reuse branch it is not called at all.
Its own unit test `cookies_survive_removal_and_new_names_sort_last` already encodes the semantics any
repair needs: surviving names keep their cookie, a removed name drops out, and a newly present name
sorts after every name already listed.
So the cookie seam is ready for the fix and the fix needs no new cookie design.

Two related limits are visible in the code and are not part of this reproduction, listed so the
proposal does not overreach:

A name that vanishes between `list_dir` and `fstatat` is skipped by the `Err(ENOENT) => continue`
arm, and `consumed` was already incremented for it, so such a name is consumed but never emitted.
That is existing tolerate-and-skip behaviour for a mid-page race, not the same-tick omission above.

`eof` is computed as `consumed == listing.len() - start`.
That is safe only because `partition_point` can never exceed `listing.len()`, so it is recorded as a
constraint any change to the restart condition must preserve, not as a defect found here.

## Proposal, not implemented

Contract under consideration, as stated for approval rather than assumed:

> A name that appeared in the directory before a pagination resumes is visible in the resumed
> listing, even when the six-field directory stamp is identical on both sides of that change, with
> no duplicate and no omitted entry among the names that were present throughout.

What that does and does not promise:
it is about *completed* external changes being visible at a resume boundary.
It is not filesystem snapshot isolation, it does not promise atomicity across pages, and it says
nothing about a change that lands in the middle of a page being included in that page.
No promise of strong snapshot semantics is made or implied, because that is a different design
question this reproduction did not examine.

Options, with their costs stated rather than assumed:

**A. Treat the six fields as a hint and refresh the listing on every resume.** Call `list_dir` and
`Cookies::sync` whenever `cookie != 0`, then serve from the refreshed listing. The resumed
`partition_point(cookie <= cookie)` still skips correctly because surviving names keep their numbers,
and a new name sorts last, so the contract is satisfied using the existing cookie semantics with no
new machinery. The cost is one `getdents` pass per page instead of one per listing, so a listing of
`n` entries in pages of `m` goes from `O(n)` to `O(n * n / m)` directory reads. That directly
contradicts the "paging a large directory stays linear" claim in the function's own doc comment, and
the 50,000-entry conformance check in `cowfs-vfs-test` would be the thing that shows it. This option
is contract-complete and I am not recommending it as cheap.

**B. Add a seventh cheap field to the key.** Measured dead on this evidence. `size` and `nlink` are
already in the six and both were inert here. A directory generation number is not reachable from
Linux `fstat`, `FS_IOC_GETVERSION` is not portable and not on tmpfs, and `statx` `btime` is the
inode's birth time, which does not move. There is no cheap portable seventh field.

**C. Verify the cache once per listing rather than once per page.** Refresh on the first resume after
each page boundary and remember that it was verified. This lowers the readdir count but weakens the
guarantee to "a change is caught on some later page boundary", which is not the contract above, and it
adds state whose invalidation is exactly the problem being fixed. Listed for completeness.

The cheap-namespace-key idea and the always-rescan idea both resolve to the same trade: detecting a
change the host's clock refused to signal costs a directory read, so the real decision is *how many
reads per listing*, not whether to read.
Whichever is chosen, the minimum outcome contract to test is the one above, together with unchanged
cookie stability across the change, unchanged inode allocation, unchanged `register` and export
security semantics, and paging completeness on an unchanged directory.

I have not implemented any of these.
No production change is proposed for merge until the contract is approved and the cost of the chosen
option is measured rather than argued.

## Open and explicitly not resolved here

`CONFIG_HZ` and Ubuntu `ubuntu-latest` granularity remain unmeasured, as #120 already records.
This run adds one kernel and two filesystems on one host, which is not a portability result.

Nothing here re-litigates or alters the #118 delivery, its test, its `force_observable_mtime`
diagnostic helper, or its immutable reports.
`#118`/`#19` stay where they are and this does not change what they claim: #118's test establishes
that a change *observable in the stamp* invalidates the cache, which is true and unrelated to this
case, where the stamp is not observable.

Nothing here bears on #131's CI event. That event was never reproduced, and neither a reproduced
stale listing nor a green host run says anything about its root cause.

## Artifacts

| artifact | sha256 |
| --- | --- |
| `main-93cfef9.tar.gz` | `b936a8dd492b59cf81f627224c955464874f8b5205fe12b80ab0d37dcb87bd15` |
| `probe-src-93cfef9.tar.gz`, the archive actually sent to the Linux host | `062edc43e0a0bd1eec8cb07e9aa16f5d7c05bb6ba79428f93f97da966aa18bc9` |
| `linux-logs/run.log` | `ef5295c8f8468a0177cefc6953d6df141b8db17d2e3c0998b37bd9b9fd5abd89` |
| `linux-logs/sample-ext-1.log` | `9171134db0e2199926a25a1019c416b536b3ae81279c170ac4c615900be95f3a` |
| `linux-logs/search-ext-20.log` | `1cc8953fdf5e0793da371d82e1a3a343c44e822be6b08389d9733f4c25b35061` |
| `linux-logs/search-tmpfs-20.log` | `b0b74f08471e1e8e25b6c8c534e445cc8957c500e6f41f53a3136b0205dfc0e3` |
| `linux-logs/build.log` | `58e88958f3ec543f496f856694561615fae63953b4af56c86e01fd20d21b7bf6` |
| `logs/mac-20attempts.log` | `3997c2970d72828f7baa16b77499b90da61c6321ea4246f404cfc175afa6c40d` |

All of them live under `bench/out/pathvfs120-reproduction/` in the held worktree, which is
gitignored, so none of this enters the repository and none of it disturbs the #118 artifacts beside
it.

Remote residue is confined to `/home/moonscape/cowfs-ready-wave/task-pathvfs120-reproduction/`, a new
subdirectory created for this task, holding the extracted source, its isolated target directory, its
private fixture and its logs, 89 MiB total.
No install, no sudo, no mount, no shared store, no runner change, no daemon, no signal, and no other
agent's directory or lease was touched.
The Mac run held `.treehouse-ready-wave/mac-heavy.lock` and the Linux run held
`linux-heavy.lock`, both for the duration of one bounded foreground command.