# pjdfstest g3 independent review

Reviewer lane: read-only review of PR #106 at head `025bde2f3ccea609f54edf67a9d86d66d28abeca`.
Lease `a53321161c0b6660c9124671c6c6654c`, branch `review/pjdfstest-g3`, clean tree, verified before and after the review.
No production source was edited, no commit, no push, no lease return.

## Verdicts, kept separate

| lane | verdict |
| --- | --- |
| macOS NFS loopback against private real-Core mount | **FAIL**, independently reproduced |
| Linux FUSE arm | **UNMEASURABLE**, not attempted, still missing coverage |
| harness source `bench/pjdfstest.py` + `bench/test_pjdfstest.py` | **PASS** with named gaps, no correctness bug found |
| write-up `docs/verification/ready-g3.md` and PR #106 body | **BLOCKED**, the attribution arithmetic does not reproduce |

The two acceptance lanes do not merge into one number.
g3 cannot be called complete while the Linux arm is unmeasured, and this review does not attempt it.
The macOS FAIL stands on its own and is not weakened by the Linux gap.
80 suite-declined cases and 1968 native plus 1981 cowfs privilege-gated assertions remain explicit missing coverage, so even a green differential would not have closed g3.

## Independent derivation of the TAP accounting

Derived in code from the preserved builder records at `bench/out/ready-g3/run/20261005T004337Z/cases.jsonl` in lease 9, sha256 prefix `bb55fe4912a36302980c9a1b0f8214a4`, without reusing the harness's own arithmetic.

| | native | cowfs |
| --- | --- | --- |
| records | 238 | 238 |
| test sets identical | yes | yes |
| cases on one arm only | 0 | 0 |
| child exit code non-zero | 0 | 0 |
| timed out | 0 | 0 |
| cases with empty TAP output | 0 | 0 |
| cases with no plan line | 0 | 0 |
| cases where plan != assertions emitted | 0 | 0 |
| cases with a duplicate assertion id | 0 | 0 |
| cases whose ids are not contiguous 1..N | 0 | 0 |
| `Bail out!` occurrences | 0 | 0 |
| `# skip` directives | 0 | 0 |
| `# TODO` assertions | 0 | 0 |
| suite-declined by quick_exit | 80 | 80 |
| executed | 158 | 158 |
| assertions | 8686 | 8686 |
| passing | 2985 | 3140 |
| failing | 5701 | 5546 |
| privilege-gated assertions | 1968 | 1981 |
| privilege-gated that PASSED | 0 | 0 |
| non-privilege-gated that FAILED | 3733 | 3565 |

Every headline number in the write-up reproduces exactly, and so does `summary.json`, which reports `state FAIL`, 700 regressions of which 687 outside the privilege gate, 855 looser, 0 unpaired.

The 80 declined cases are the same 80 on both arms and all carry `plan == 1` with a single passing textless assertion.
They are not a blanket unsupported marker: `chflags/00.t` gates on an explicit `require chflags` plus a `case "${os}:${fs}"` allowlist, and `posix_fallocate/00.t` gates on `require posix_fallocate`.
Every declined case carries a human-authored reason in the pinned source, so the "suite declined here" classification is explanatory.
Group spread: chflags 14, utimensat 10, open 9, granular 7, link 6, rename 6, rmdir 4, ftruncate 3, mkdir 3, mkfifo 3, symlink 3, truncate 3, unlink 3, chmod 2, chown 2, mknod 1, posix_fallocate 1.

## How the assertion counts overlap, which the write-up does not state

The privilege-gated counts are a subset of the failure counts, not an additional bucket.
On both arms every privilege-gated assertion fails, because none can pass without root.
So the 1968 and 1981 sit inside the 5701 and 5546 failures respectively.

- native: 8686 assertions = 2985 pass + 5701 fail, and the 5701 fails split into 1968 privilege-gated and 3733 not.
- cowfs: 8686 = 3140 pass + 5546 fail, splitting into 1981 privilege-gated and 3565 not.

The verdict uses only the 687 regressions that fall outside the privilege gate.
The 13-assertion asymmetry between 1968 and 1981 is itself a consequence of the chown divergence changing which branch a case takes, and those 13 surface as privilege-gated regressions in `mknod/00.t`, `mknod/01.t`, `mknod/02.t`, `mknod/03.t` and `mknod/08.t`, all `EIO` at the create.

## The differential pairs by ordinal position, which bounds what 687 means

`compare()` pairs assertions by `(test, assertion number)` only.
It never pairs on assertion text or on any stable per-assertion identity.
Because cowfs answers `chown` with success where native answers `EPERM`, the suite takes a different branch and every later assertion number in that case refers to a different check on the two arms.

Measured on the preserved records:

- of the 687 regressions, 48 pair two assertions whose normalized text matches, and all 48 are textless on both sides, so even those are not verified same-assertion pairs;
- the other 639 pair structurally different assertions;
- of the 855 looser, zero pair matching text, every one is a mispaired ordinal;
- re-pairing on `(test, text signature, occurrence)` instead yields only 108 regressions and leaves 5306 assertions unpairable, which is the direct measure of the control-flow divergence.

This does not make the metric meaningless.
"687 ordinal positions where native passed and cowfs failed" is well defined and the FAIL verdict follows from it.
What it does not support is reading 687 as a count of distinct defects.
The write-up's per-cause table is indexed by those positions, so its row totals inherit the pairing.

## The attribution arithmetic does not reproduce

The write-up says every regression is accounted for and gives five rows.
Those rows sum to 684, not 687.
The PR body gives the same content as 617 plus 63 plus "three small separate divergences", which sums to 683.
The two artifacts disagree with each other and neither reaches the headline.

My exact partition of the same 687, derived from the cowfs detail text:

| bucket | count |
| --- | --- |
| A: non-regular create answered `EIO` (mkfifo 106, bind 92, mknod 0) | 198 |
| B: `pathconf` -1, the 13 cases whose stderr says so, non-`EIO` rows | 66 |
| C: cascade rows whose cowfs detail is `ENOENT` | 360 |
| D: textless rows | 48 |
| E: rows with a non-`ENOENT` errno answer | 15 |
| total | 687 |

Per-claim corrections against the raw data:

- "103 `mkfifo` `EIO`" is 106.
- "92 `bind` `EIO`" is 92, correct.
- "419 cascades" does not reproduce under any reading I tried.
- "617" does not reproduce: the `EIO` rows alone are 198, `EIO` plus `ENOENT` cascades are 558, and regressions located in the `mkfifo`/`mknod`/`bind` case groups are 73.
- "63 `pathconf`" is 66; 67 regressions sit in those 13 cases, of which 1 is also an `EIO` row.
- `rmdir/12.t` #4 is exactly 1 row, and `unlink/14.t` #4 is exactly 1 row, both as claimed. The `nlink` figure of 1 is right; 259 rows merely mention `nlink` in an `lstat type,nlink` cascade.
- the two textless timestamp rows are `ftruncate/12.t` #2 and `truncate/12.t` #2, both present, and both are unmechanized, so the write-up is right to label them a hypothesis rather than a code finding.
- "713 of the 855 are `chown`" is 714. The remaining 141 are not explained in either artifact: 70 in `rename/09.t`, 60 in `rename/10.t`, 10 in `unlink/11.t`, 1 in `mkdir/10.t`, all `lstat` uid assertions that are part of the same ownership divergence.
- the `rename/09.t` and `rename/10.t` figures reproduce exactly: native 1900 and 1616 failures against 1630 and 1364 on cowfs.

So the qualitative triage is right and the numbers are not.
The 15 rows in bucket E reduce to the 2 the write-up names plus 13 fifo-loop rows, which I first misread as an independent defect class and then withdrew after reading the pinned test scripts.

## The cascade attribution holds, checked against the pinned source

I initially classified `link/10.t` #9 and #18, `mkdir/10.t` #8 and #9, `symlink/08.t` #8 and #17, `rmdir/06.t` #9, `rename/20.t` #10, `rename/13.t` #8, #10 and #11, and `open/22.t` #8 and #17 as cowfs failing to enforce `EEXIST` or `ENOTEMPTY`, which would have been a distinct unfiled defect class.
That reading was wrong.
All of those cases loop `for type in regular dir fifo block char socket symlink` and call the suite's `create_file` helper, so on cowfs the fifo, socket, block and char targets are never created and the collision never arises.
The write-up's `open/22.t` example is exactly this mechanism and is correct.
Only `rmdir/12.t` #4 and `unlink/14.t` #4 survive as divergences not explained by non-regular creation or `pathconf`.

## The native baseline is a weak oracle, and the write-up does not say so

The native arm fails 5701 of 8686 assertions, of which 3733 are outside the privilege gate.
Its most common non-privileged failure shapes are 868 `expected 0, got EPERM`, 467 `expected 0, got ENOENT`, 206 `expected 0, got EEXIST`, 67 `expected 0, got EADDRINUSE`, and roughly 1098 rows in other shapes including a large family of uid-comparison rows.

A differential gate is right to ignore assertions both arms fail, so this does not invalidate the FAIL.
It does mean 3733 assertions the native arm cannot adjudicate are outside the verdict by the same logic that excludes the 1968 privileged ones, and the write-up discloses only the 1968 and 1981.
A reader would not learn that the native reference itself fails 43 percent of all assertions on plain APFS.
That is a disclosure gap, and it is the same gap as the "both arms fail so it cannot be called a pass" concern, stated in the write-up's favour rather than against it.

## EIO does not distinguish unsupported policy from corruption

This is the sharpest technical finding.

- `crates/nfsserve/src/nfs_handlers.rs:135` returns `NFS3ERR_NOTSUPP` for every `NFSPROC3_MKNOD`, with no condition.
- `crates/nfsserve/PATCHES.md:28` states "MKNOD answers NFS3ERR_NOTSUPP", and line 21 states "`fsstat` and `pathconf` come from the file system".
- `crates/cowfs-fuse/src/convert.rs:51` `mknod_is_regular` plus `crates/cowfs-fuse/src/fs.rs:613` answer `ENOTSUP` for a non-regular `mknod`, and `crates/cowfs-fuse/src/lib.rs:91` lists device nodes, fifos and sockets as unsupported.
- `crates/cowfs-vfs/src/types.rs:103` defines `SetAttr` with only `mode`, `size`, `atime` and `mtime`, so there is no uid or gid to set. The write-up's chown reading is exactly right.
- `crates/cowfs-vfs/src/error.rs:74` maps `Error::Corrupt(_)` and `Error::Io(_)` to `libc::EIO`.

So `EIO` is simultaneously cowfs's own corruption and I/O error.
There is no errno translation table anywhere in the tree, so the `EIO` the client sees at a create is rendered by the macOS NFS client from `NFS3ERR_NOTSUPP`.
I could not verify that translation: it lives in the XNU client, which is not on this host, and the shared Mac lock was held so I could not mount and observe it directly.
The consequence stands regardless: attributing all 198 `EIO` rows to the documented non-regular policy is consistent with `PATCHES.md` but is not proven, because the same errno is what a genuine `Corrupt` or `Io` failure would produce.
Neither the harness nor the run records anything that would separate the two.
The 360 `ENOENT` cascade rows and the 66 `pathconf` rows are unaffected, since those are `ENOENT` and `-1`.

## Tool provenance and feature coverage

Verified read-only against the builder's own checkout, no cold rebuild.

| item | value |
| --- | --- |
| source | `pjd/pjdfstest` at `85a8aea9e685999ef0540392fd80535f873d7ff7`, checkout clean at that commit |
| `pjdfstest.c` sha256 | `a6c354f2c42015a1d2d538ea4276091278a1e4955bb6a10d0ec9b9838f24a23e` |
| generated `config.h` sha256 | `493cda00f566b96eae2c032c26bfb22dcc000b0b44c7f46c1726ba31957a5a45` |
| binary sha256 | `5fa40986f39bb9033ef2fc15a7b110f3405541ff97344ca34756fceb090cdb71` |
| compiler | Apple clang 21.0.0, clang-2100.3.34.2 |
| features detected | 29 |

All four hashes and the commit reproduce exactly, and the checkout has no local modification, so there is no source-hash drift and no synthetic or stubbed binary in this run.

The probes are honest.
A function macro is defined only when a program taking `&func` compiles and links, which is what `AC_CHECK_FUNCS` does, and no macro is stubbed or hand-written.
A `struct stat` member is probed by assigning it, and `ACL_TYPE_NFS4` by returning it.
All eleven claimed-absent macros are confirmed absent from the generated `config.h`: `posix_fallocate`, `bindat`, `connectat`, `lpathconf`, `chflagsat`, `lchflagsat`, `HAS_NFSV4_ACL_SUPPORT`, and the `st_atim`, `st_ctim`, `st_mtim`, `st_birthtim` spellings.
The `timespec` spellings and `st_birthtime` are present and were used.
This is a real host-capability gap, not a harness shortcut, and it is disclosed.
Those absences do mean the corresponding POSIX surface is untested on this host, which the write-up lists.

The binary hash is build-specific rather than reproducible from source alone, so `5fa40986...` plus the recorded `config.h` and compiler is the honest manifest.
A different `config.h` would give a different binary, so the binary hash should not be treated as a source identity.

## Harness source findings

No bug that can turn a bad run into a PASS was found.
The verdict function refuses rather than guesses: an arm that executed nothing, an arm with empty TAP output, a timeout, or any case present on only one arm all force `UNMEASURABLE`, and `UNMEASURABLE` takes precedence over `FAIL`.
The unit tests cover exactly that refusal behaviour.

Gaps, all of them latent rather than triggered by this run:

1. **The mount check is not tri-state.** `mount_listed()` runs `/sbin/mount`, ignores the return code, and substring-matches. A failed or truncated `mount` invocation yields an empty string and is reported as "not mounted", which is indistinguishable from a verified absence. In `stop_daemon()` that can produce a false "mount gone" and orphan an NFS mount with no owner. A three-state result, exact match on the mount table, and an `UNKNOWN` that refuses to claim absence would close it.
2. **The verdict ignores several invariants it reports.** `totals["nonzero_rc"]` is collected and printed in the write-up's table but never consulted by `verdict()`. Plan completeness, truncation, duplicate ids and contiguous numbering are likewise unenforced. I verified all four hold for this run, so nothing is currently wrong, but a case that died mid-stream would be scored on partial data instead of being refused.
3. **Source integrity is recorded, not enforced.** `fetch_tool()` reuses a checkout whose `HEAD` equals the pin but never checks for local modification, and `tool_identity()` records `pjdfstest.c` sha256 without comparing it to an expected value. A dirty tree at the pinned commit would be accepted and only reported. Clean here, verified independently.
4. **`stop_daemon()` raises `SystemExit` from inside `finally`**, so a teardown failure can mask the exception that caused the teardown.
5. **Cleanup is narrow and silent.** Teardown removes only `repo/rt` with `ignore_errors=True`, which swallows failures and would delete a concurrent run's socket directory in the same worktree. The run directory, including the block store, is never removed, so a failed run leaves it behind.
6. **`host.native_fs` is recorded empty.** The harness captures `df -T`, which macOS `df` does not support, so the list is `[]`. The write-up asserts the native arm ran on the host's APFS volume with no recorded evidence behind it. The run is in fact correctly separated: all 238 cowfs `case_dir` values are under `run/.../mnt/pjd` and all 238 native values are under `run/.../native`, which the `case_dir` field proves. But nothing in the harness asserts that separation, so a future run pointed at `--out` inside the mount could put both arms on NFS and still score. That is the false-PASS shape worth closing.
7. **The measured cowfs build is not attested by commit.** `summary.json` records the daemon path, pid and argv but no binary sha256 and no worktree `HEAD`, so which cowfs build was measured is not pinned by the record.

Signal handling and process ownership are correct where it matters: `stop_daemon()` signals the single recorded pid, never a process group, after verifying the pid's argv still contains the binary it started, then verifies the mount is gone. No `pkill`, no unbounded wait.

## Runtime identity and cleanup receipts

Read-only, two independent receipts for absence, no signalling and no unmount performed.

- daemon pid 97710 is absent from `ps`.
- the run's mount `run/20261005T004337Z/mnt` does not appear in `/sbin/mount`.
- `daemon.log` ends with `cowfs-daemon: signal 15, shutting down`, matching the SIGTERM path.
- `daemon.json` recorded the mount table line at start: `localhost:/cowfs-cf052f1312b2c3ec668ebf09e03162ad on .../run/20261005T004337Z/mnt (nfs, nodev, nosuid, mounted by zeeshanhaque)`, which gives device type `nfs`, a localhost NFS source, and the mount point.
- `daemon.log` states "with the nfs adapter" and the recorded argv carries `--backend core`, and the store holds `meta.redb`, `virt.ino.b`, and `store/store/{LOCK,SYNCED,index.cix}` with `store/store/packs/pack-00000000.cpk`. That is a real Core block store behind the NFS adapter, not a passthrough, so the "real-Core" claim is attested rather than asserted.
- store, mount and socket all sit under the run directory or the lease root; the control socket path is 98 bytes against the 103-byte `sun_path` limit.
- The pre-existing shared mount at `~/.cowfs/mnt`, pid 15263 up since Oct 3, and the other live cowfs mounts in temporary directories were not touched.
- The builder's run directory and the 238-case raw record are intact and unmodified; the lease 16 checkout is clean at the reviewed head with no files written.

## Tests, lint and CI

`python3 bench/test_pjdfstest.py` runs 15 tests, all pass, exit 0, in 0.085s.
That is a direct run of the file, not a workspace-wide sweep.

`ruff check` on the two new files reports 17 findings: 11 `PLW1510` subprocess without explicit `check`, 2 `RUF012`, 1 each `RUF100`, `PERF402`, `FURB167`, `EXE001`.
The repo has no ruff configuration, no `ruff.toml` and no `[tool.ruff]` section, and CI does not run ruff, so these are advisory rather than a project gate.
Nothing here is a defect.
`EXE001` is the only one worth acting on: `bench/pjdfstest.py` carries a shebang and is mode 644.

CI discovery is genuine and needs no workflow edit: `.github/workflows/ci.yml` already runs `python3 -m unittest discover -s bench -v`, whose default `test*.py` pattern picks up `bench/test_pjdfstest.py` on both the ubuntu and macos matrix legs.
No source at the reviewed head is assumed green; I read the head and ran the tests, and PR #106 reports 3 passed checks at this head.
I did not poll, rerun or dispatch anything.

## Issue ownership and the closing-reference trap

- #107, #108, #109 and #110 are all OPEN, filed 13 to 15 minutes before this review, and correctly map to non-regular creation, `pathconf`, `rmdir` `..` with `nlink`, and the `chown` contract.
- #107 is titled with "617 of the 687", #108 with "63", #110 with "713". Those three counts do not reproduce, as shown above. The issue titles repeat numbers I could not derive, so they need the same correction as the write-up. Issue text, not just titles, should be checked.
- Ownership is placed correctly and nothing is duplicated: non-regular creation belongs to the NFS server-requirements lane #19 with a Core create-with-type path behind it, and #19 is open. `SetAttr` carrying no uid or gid is a Vfs trait contract question for #43, and #42 and #43 are open. The `rmdir a/b/..` `EINVAL` and the `nlink`-after-unlink behaviour are Core or adapter behaviour reported for triage, which #109 does.
- `closingIssuesReferences` on PR #106 returns `totalCount 0` with an empty node list, so merging it will not close the acceptance issue. There is no closing keyword and no negated "does not close" phrase in the body; the four `#107` to `#110` mentions are neutral bullet references. The no-auto-close claim holds.
- Ownership of the `chown` success-ignore behaviour is a documented capability contract, not a security claim, and #110 is filed at that level. Correct.
- I filed nothing, fixed nothing in production, and delegated to no builder.

## Missing coverage, stated as missing

- The Linux FUSE arm is unmeasured. `moonscape` has `/dev/fuse` and `fusermount3`, so the arm is available and the gap is a cross-build plus the shared Linux lock, not an unreachable host. This review does not start it.
- 80 suite-declined cases per arm, each with an explicit reason in the pinned source.
- 1968 native and 1981 cowfs privilege-gated assertions, all of which failed on both arms because root is unavailable and none of which ran.
- 11 absent host capability macros.
- 3733 native and 3565 cowfs non-privileged assertions the native arm cannot adjudicate.

g3 does not close on this evidence.

## My own sample run: RESOURCEBLOCK, not attempted

The shared Mac lock at `.treehouse-ready-wave/mac-heavy.lock` was held by another worker on both checks, before and after this review, and load average was 14.68 on 16 cores.
Lease 16 has no `target/release` binaries, so the harness would have needed a cold release build of `cowfs-daemon` and `cowfs-cli`.
I therefore ran no own-mounted sample, took no lock, started no daemon, and did not wait in a loop.

This is a declared resource block, not a substitute for a result.
The macOS FAIL verdict is independently reproduced from the builder's preserved raw records, which is a distinct line of evidence from an own-mounted run and does not depend on one.
The source and raw-record audit carries the review.

## Required before merge

1. Correct the attribution numbers in `docs/verification/ready-g3.md`, the PR #106 body, and issues #107, #108 and #110 to the derived values: 198 `EIO` rows, 66 `pathconf` rows, 360 `ENOENT` cascade rows, 48 textless rows, 15 non-`ENOENT` errno rows, and 714 `chown`/`lchown` with the 141 uid rows named.
2. State that 687 is an ordinal-position differential and not 687 distinct defects, with the 48-of-687 matching-text measurement as evidence.
3. Disclose the 3733 and 3565 non-privileged native-arm failures the native oracle cannot adjudicate.
4. Either prove the `NFS3ERR_NOTSUPP` to `EIO` translation or mark the 198 `EIO` rows as policy-attributed-by-inference, noting that `Error::Corrupt` and `Error::Io` map to the same `EIO`.
5. Make `mount_listed()` tri-state with an exact match and an `UNKNOWN` that refuses to claim absence.
6. Enforce in `verdict()` the invariants already reported, including non-zero child exit, plan completeness and truncation, so a partial case is refused rather than scored.
7. Assert the two arms are on different filesystems instead of recording an empty `df -T`, and pin the measured cowfs binary by sha256 and commit.
8. Keep g3 open for the Linux arm, the 80 declined cases, the 3969 privilege-gated assertions and the absent macros.