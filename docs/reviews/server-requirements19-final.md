# Review: PR #113, the #19 NFS server requirements test lane

Reviewed head: `395c2361b0c2bfcae41aa8d949fae53b9eb830eb` (`followup/server-requirements-19`).

Verdict: **BLOCK**. The lane is well built and its macOS evidence is sound and independently reproduced,
but the PR is **red on `ubuntu-latest`** and fails 2 of its own 8 tests there.
No production source changed, so nothing in this lane can be a source of a green Linux claim.

Method: the exact head was fetched over HTTPS and materialised with `git archive` into a private copy.
The PR branch was never checked out, reset or stashed, and the lease branch was never touched.

- Private archive: `<lease>/bench/out/requirements19-critic/src`
- Private target dir, logs and mutant copy: `<lease>/bench/out/requirements19-critic/`
- Host: Darwin 25.6.0, uid 501, TZ PDT-0700, rustc/cargo 1.99.0
- The shared daemon, its store, socket and mount were never signalled, read or used as a fixture.

## 1. Source identity

All 531 tracked files of the head were hashed with `git hash-object` and compared to
`git ls-tree -r 395c236`.

- 531 tracked files checked, 0 missing, 0 hash mismatches.
- After all review work, re-checked: still 531/531 byte-identical, the only added file being this
  reviewer's own probe.

The archive is the PR source, not a working-tree approximation of it.

## 2. The `b4b3f37` proof-carry question

The PR body attributes every measured figure to `b4b3f37905d7f571a38c55d8ae37a83c26df9a23`, and the
head is `395c236`. Blob comparison between the two:

| Path | b4b3f37 vs head |
| --- | --- |
| `Cargo.toml` | identical, `55adec65` |
| `Cargo.lock` | identical, `49c732e0` |
| `crates/cowfs-nfs/Cargo.toml` | identical, `c7700c20` |
| `crates/cowfs-nfs/tests/requirements19.rs` | identical, `2ab65df8` |
| `docs/verification/ready-19.md` | **differs**, `0211923f` vs `caf79bc6` |

`git diff --stat b4b3f37 395c236` is one file, `docs/verification/ready-19.md`, 14 insertions and
11 deletions. Every source, test and dependency blob is byte-identical, so the author's measured proof
carries to the head unchanged. This is a source-bytes argument, not a build-digest argument.

## 3. The locked-manifest guard

This was the first thing checked, before any cargo command, on the pristine exact archive.

| Point in time | Command | `Cargo.lock` sha256 |
| --- | --- | --- |
| pristine | none | `706645b958fb89e4b90f56a4c31f9c04e68816e0a652137aec7960479628de63` |
| after `cargo metadata --locked` | exit 0 | unchanged |
| after `cargo test --locked --no-run` | exit 0 | unchanged |
| after `--locked` test runs and the reviewer probe | n/a | unchanged |

The dev-dependency concern does not apply here. The PR does carry the lock edit:
`crates/cowfs-nfs/Cargo.toml` gains `cowfs-vfs-path = { path = "../cowfs-vfs-path" }` and `Cargo.lock`
gains the matching `cowfs-vfs-path` edge on the `cowfs-nfs` package. The `cowfs-vfs-path` package entry
itself already existed in the lock. `--locked` never needed to rewrite anything, and the lock is
unchanged after every command run.

## 4. What the PR actually contains

Four files, 726 insertions, 0 deletions. Zero production source changed.

| File | Change |
| --- | --- |
| `crates/cowfs-nfs/tests/requirements19.rs` | new, 553 lines |
| `docs/verification/ready-19.md` | new, 171 lines |
| `crates/cowfs-nfs/Cargo.toml` | +1 dev-dependency |
| `Cargo.lock` | +1 dependency edge |

## 5. Test inventory, measured not counted

`--list` on the built binary reports **8 tests, 0 benchmarks**, all 8 discovered.
A default run reports **5 passed; 0 failed; 3 ignored**, matching the PR body's 5 and 3.

Default, no `--ignored`:

1. `a_native_chmod_through_a_symlink_lands_on_the_target`
2. `setattr_gives_a_symlink_its_own_times_and_mode_over_the_real_filesystem`
3. `readdir_replies_carry_the_link_count_of_the_moment`
4. `hardlinked_names_in_one_directory_are_listed_once_each`
5. `namespace_churn_over_the_real_filesystem_stays_bounded`

`#[ignore]`d, three:

6. `a_large_directory_is_read_once_per_listing`
7. `find_links_counts_the_same_through_the_mount_as_natively`
8. `touching_a_symlink_on_the_mount_leaves_its_target_alone`

There is **no `#[cfg]` host gate anywhere in the file**. The three mount tests gate at runtime on
`cowfs_nfs::mount_nfs_available()`, which is `cfg!(target_os = "macos") && /sbin/mount_nfs exists`.

## 6. BLOCKING: the PR is red on ubuntu-latest

CI run `37253685951` on the exact head:

| Job | Conclusion |
| --- | --- |
| `check (ubuntu-latest)` | **failure** |
| `check (macos-latest)` | still in progress at review time |
| `linux-fuse` | success |

The ubuntu job fails in `requirements19` itself, not in clippy or fmt:

```
running 8 tests
test a_large_chmod_through_a_symlink_lands_on_the_target ... FAILED
test setattr_gives_a_symlink_its_own_times_and_mode_over_the_real_filesystem ... FAILED
test readdir_replies_carry_the_link_count_of_the_moment ... ok
test hardlinked_names_in_one_directory_are_listed_once_each ... ok
test namespace_churn_over_the_real_filesystem_stays_bounded ... ok

test result: FAILED. 3 passed; 2 failed; 3 ignored
error: test failed, to rerun pass `-p cowfs-nfs --test requirements19`
Process completed with exit code 101
```

The two failures:

- `a_native_chmod_through_a_symlink_lands_on_the_target` at line 81: expected `(493, 384)` =
  `(0o755, 0o600)`, got `(511, 384)` = `(0o777, 0o600)`. The link's own mode on Linux is `0o777`,
  because Linux does not store a symlink mode. The **target** did take `0o600` on both hosts, so the
  control's actual purpose still holds; the literal `0o755` expectation is the Mac-only part.
- `setattr_gives_a_symlink_its_own_times_and_mode_over_the_real_filesystem` at line 131:
  `assert_eq!(st, OK)` got `left: 5, right: 0`, a non-OK `nfsstat3` for the symlink `chmod`.

The cause is in the production source this lane does not change, and it is not a test bug:

- `crates/cowfs-vfs-path/src/lib.rs:200-211` sets a mode with `sys::fchmod(s.open_kind(ino)?)`.
- `crates/cowfs-vfs-path/src/sys.rs:176` on macOS: `OPEN_SYMLINK = O_SYMLINK | O_RDONLY`, and
  `fchmod` on such a descriptor changes the link's own mode. This is a real macOS behaviour.
- `crates/cowfs-vfs-path/src/sys.rs:173` on Linux: `OPEN_SYMLINK = O_PATH | O_NOFOLLOW`, and
  `fchmod` on an `O_PATH` descriptor is `EBADF`. Linux also has no chmod on a symlink at all.
- The **times** half is portable: `sys::utimens_fd` carries an explicit Linux branch using
  `AT_EMPTY_PATH` (`sys.rs:338-352`). The **mode** half has no such branch.

So the mode assertions are Mac-only semantics asserted on a host gate that does not exist.
The PR body reports only macOS figures, and `docs/verification/ready-19.md` records no Linux run.

**Required before merge**: either a `#[cfg(target_os = "macos")]` split on the mode assertions with a
Linux branch that asserts the real Linux outcome, or a native control on Linux that establishes what
a symlink `chmod` should return there. Silently green on Linux is not available, and no fix is in
this lane's remit to apply here.

## 7. Independent macOS reproduction on this archive

Default battery, single run, `--test-threads=1`, on the private exact archive:

```
test result: ok. 5 passed; 0 failed; 3 ignored; 0 measured; 0 filtered out; finished in 21.23s
CHURN iterations=6000 rss_warm=9 MiB rss_end=11 MiB growth=2 MiB
```

That independently reproduces the author's churn figures (9 MiB warm, 11 MiB end, 2 MiB growth)
from the author's own raw logs, on this archive, at this head.

The two mount tests, real `mount_nfs`, private server, private port, private mountpoint, private
backing directory, all three ignored tests filtered out so only the two mount tests ran:

```
running 2 tests
test find_links_counts_the_same_through_the_mount_as_natively ... LINKS native=151 mount_first=151 mount_cached=151
test touching_a_symlink_on_the_mount_leaves_its_target_alone ... RSYNC ok=true out=""
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out; finished in 0.54s
```

`native=151, mount cold=151, mount cached=151` reproduces the author's table exactly, and
`assert!(native > 0)` is in the test, so the count is not a zero-vacuous pass.
The counts come from a real `find . -links +1 | wc -l` in two distinct trees, one native and one
through the mount, not from two reads of one root.

## 8. Reviewer's own mounted probe

The author asserts the touch case but prints nothing for it, so this review wrote its own probe,
`zz_critic_probe.rs`, a separate test file in the private archive, never in the PR.

```
PROBE mount_line=localhost:/cowfs-4f1e41cafe157c100ec1c2f3554e1071 on /private/var/.../.tmpcRvArX/mnt (nfs, nodev, nosuid, mounted by zeeshanhaque)
PROBE export_source=localhost:/cowfs-4f1e41cafe157c100ec1c2f3554e1071
PROBE device_mount=436209609 device_backing=16777234
PROBE before tool=(1791166582, 817680994) link=(1791166582, 817897871) dangling=(1791166582, 817994747) readlink_link=tool readlink_dangling=nowhere
PROBE derived_expected_stamp=(946811040, 0) (local time, no hardcoded epoch)
PROBE touch_h_ok=true out=""
PROBE after link on_mount=(946811040, 0) on_backing=(946811040, 0) type_is_symlink=yes readlink=tool
PROBE after dangling on_mount=(946811040, 0) on_backing=(946811040, 0) type_is_symlink=yes readlink=nowhere
PROBE target tool_before=(1791166582, 817680994) tool_after_backing=(1791166582, 817680994) tool_after_mount=(1791166582, 817680994) plain_before=(946811040, 0) plain_after=(946811040, 0)
PROBE rsync native_ok=true mount_ok=true mount_out=""
PROBE rsync_listing="dangling link tool "
PROBE after_unmount_still_listed=false
test result: ok. 1 passed; 0 failed
```

What this establishes beyond the author's own assertions:

- **Mount identity.** The export is `localhost:/cowfs-<32 hex>`, the filesystem is `nfs`, and the
  mount's device id `436209609` differs from the backing directory's `16777234`. It is a real cowfs
  NFS mount, not a local view of the backing directory reached by any other path.
- **Target unchanged.** The link's own mtime lands in 2000 (`946811040`) while the target stays in
  2026 (`1791166582`) and is bit-identical before, on the backing directory after, and through the
  mount after. Different eras, so this is not a neighbour-timestamp or relabel artefact.
- **Dangling link too.** The dangling link takes the same stamp, stays a symlink, and keeps
  `readlink` = `nowhere`.
- **No hardcoded epoch.** `touch -t` writes local time, so the probe reads the expected value back
  from a native control that received the same command. Worth recording: this reviewer's first probe
  attempt hardcoded `946684640` and failed, because the host is PDT and `touch -t` is local time.
  The author's test already derives the expectation natively and is right to.
- **rsync** of a tree holding a dangling symlink onto the mount exits 0, and the listing is
  `dangling link tool`.
- **Cleanup** is verified, not assumed: `after_unmount_still_listed=false`.

## 9. Mutation check: does the test actually discriminate?

One mutant, applied to a **separate copy** of the archive. The pristine 531 files were never touched,
and the mutant was re-verified afterwards.

Mutant: `crates/cowfs-vfs-path/src/sys.rs`, in `utimensat` only, the flag argument changed from
`libc::AT_SYMLINK_NOFOLLOW` to `0`. This re-introduces exactly the #19 bug, SETATTR following a
final symlink, at the publicly reachable seam the PR claims to cover.

Result, pristine binary against the mutant source:

```
test setattr_gives_a_symlink_its_own_times_and_mode_over_the_real_filesystem ... FAILED
panicked at crates/cowfs-nfs/tests/requirements19.rs:108:5:
  left: (1791166603, 381345426)
 right: (1000, 5)
test result: FAILED. 0 passed; 1 failed
```

The reply returned the target's real mtime instead of the stamp the client asked for. The test
catches the old bug. It is not a no-op-tolerant test.

This is a **synthetic artifact mutation inside this reviewer's own archive**, labelled a mutant.
It is not a production defect and it is not a proposed fix.

## 10. Backend and scope: this is PathVfs, not Core

Every figure above is `PathVfs` on a scratch directory on the host, or a real kernel NFS client over a
private cowfs NFS export of that `PathVfs`.

- Not `MemVfs`. `common::serve` starts a real `Server` and `Nfs::connect` opens a real `TcpStream` to
  `127.0.0.1:<port>` and performs a real MNT, so the transport and the wire protocol are exercised.
- **Not the Core backend.** These are Path-backend cases and must not be counted as real-Core
  acceptance for #15 or the g3 train. `cowfs_vfs_path::PathVfs` is the backend in every test.
- `common::serve` sets `opts.check_peer_uid = false`, a test-only relaxation. Acceptable for a
  one-shot server on 127.0.0.1 with a secret export name, and worth knowing rather than hiding.

## 11. The three ignored tests have no CI coverage at all

`.github/workflows/ci.yml` runs `cargo test --workspace` in the `check` matrix, which does not pass
`--ignored`. The only job that runs ignored tests is `linux-fuse`, and it names
`-p cowfs-vfs-path --test native` and `-p cowfs-fuse`. **No job runs `--ignored` for `-p cowfs-nfs`.**

So the two mount tests, which are the only coverage of the kernel client attribute cache and of
`touch -h` becoming SETATTR on a link handle, are manual-only. The PR body says the mounted tests are
ignored like every other mount test in the crate, which is true of the file, but it does not follow
that these two are now the *only* tests of that shape and nothing runs them automatically.

There is a second, softer problem in the same code path. `mounted_backing` returns `None` when
`mount_nfs_available()` is false or the mount fails, and both mount tests then `return`. That is a
green test that never mounted anything. It is acceptable for a test that is already `#[ignore]`d, but
it means a green run of those two names proves nothing on its own. Only the printed
`LINKS native=... mount_first=... mount_cached=...` and `RSYNC ok=...` lines show a real mount
happened, which is why this review required them rather than trusting the pass line.

## 12. Claims that are bounded, and must stay bounded

- **Memory.** `namespace_churn_over_the_real_filesystem_stays_bounded` reported
  `rss_warm=9 MiB rss_end=11 MiB growth=2 MiB` here, matching the author. `rss_bytes()` is
  `/bin/ps -o rss= -p <own pid>`, so it is this process's resident set in KiB times 1024, read at
  iteration 600 and at 6000. The assertion is `growth < 64 MiB` against an observed 2 MiB, on a
  single run, on a shared machine. That is a wide, single-sample bound. It is **not** a proof that
  there is no leak, and it does not diagnose one. The adapter `parents` map plus the `PathVfs` table
  named as the suspect is a **hypothesis**, not a finding, and the mirror spike is the same
  hypothesis. Root cause remains undiagnosed.
- **readdir paging.** The code asks for `dircount = 64 * 24 = 1536`, which the helper turns into
  `maxcount = 12288`; the server clamps to `MAX_DIR_REPLY` and packs to `DIRENTPLUS_MIN_BYTES = 140`.
  The author's reported **204 pages** for 12,000 entries is arithmetically consistent with that,
  and is not the 8 a naive reading of `PER_PAGE = 64` would suggest. This review did **not** re-run
  the 12,000-entry measurement, so 204 and both latency columns are source-consistent but
  **not independently reproduced here**. They stay the author's measurement.
- **The assertion is structural, not a perf budget.** `mean_rest < native_read_ms` compares a page
  against one whole native directory read of the same directory. That is a genuine discriminator
  between a cached listing and a per-page re-read, and it is deliberately loose because the machine
  is shared. It is not the 1.5x build-overhead criterion from `AGENTS.md`, and nothing here should be
  read as satisfying it.
- **The native-control reference.** The 23 to 71 ms per page figure is the old #19 spike reading, a
  broad historic range from a different machine and code. This PR changes no production source, so it
  cannot be said to have fixed it. The PR body says as much.

## 13. Base drift and closing references

- `git merge-base 03bbec8 395c236` is `46b0f269`, and `03bbec8 --is-ancestor 395c236` is **false**.
  The branch was cut before current main and main has advanced. The API's `base.sha` of `03bbec8`
  is the current tip, not the fork point, so the reported base is not the actual merge base. Expect
  a rebase; the only shared-file change is one new key inside an existing table, which the author
  notes is conflict-free.
- `closingIssuesReferences` is **NONE**. The body says "Reconciles the server requirement list in #19"
  and never says fixes, closes or resolves. No auto-close risk. Issue #19 is **open** and stays open.

## 14. #19 is not complete, and this PR does not claim it is

This PR is the requirements **harness**, plus a documented statement of what is still open. The PR
delivers a way to check requirements, not the whole requirement set.

Still open, by the lane's own admission and by the current issue state:

- #107 typed `create` for FIFO and socket, #108 `pathconf`, #109 `rmdir` of a trailing `..` plus
  `nlink` after unlinking an open file, #110 `chown` to another uid answering success and changing
  nothing. New g3 findings, assigned to the requirement lane, not addressed here.
- The g3 breakdown arithmetic reported elsewhere was **not** verified by this review and is not
  accepted. READY16 owns that count.
- #43, the AppleDouble `._*` default-hide policy, is explicitly deferred to slot 7 and is a **user
  and maintainer decision**. No default may be flipped on this evidence.
- The fork or upstream-contribution decision for vendored `nfsserve` needs the maintainer.
- Server resident-memory root cause, as above: undiagnosed.
- No claim is made anywhere that a local Path-backend run is real-Core #15 or g3 acceptance.

## 15. Cross-platform and cleanup status

- **Mac-only or uncontrolled expectations:** the two ubuntu failures in section 6, plus the
  `0o755` link-mode literal, plus `fchmod` on `O_SYMLINK`. These are Mac semantics with no gate.
  No Linux-native control establishes what a symlink `chmod` should do there.
- **Portability evidence that does exist:** the times path is genuinely portable through the
  `AT_EMPTY_PATH` branch, and `readdir_replies_carry_the_link_count_of_the_moment`,
  `hardlinked_names_in_one_directory_are_listed_once_each` and the churn test passed on ubuntu.
- **Cleanup, verified not assumed.** Mount count before the mount tests and after the probe is 2 both
  times, and both are the shared daemon's `~/.cowfs/mnt` and OrbStack. Zero
  `cowfs-nfs-ready19-*` residue, zero `.tmp*/mnt` residue, no leftover test process. The shared
  daemon PID 15263 is alive, started Sat Oct 3 20:44:29 2026, its mount intact, never signalled.
  The mount tests' own `finish()` verifies absence from the mount table, and the probe printed
  `after_unmount_still_listed=false`.

One observation for the author, not a blocker. `Mounted` has no `Drop`, and `Watchdog`'s `Drop` only
drops the channel sender, so the watchdog thread sees a disconnect and does not unmount. On a panic
between mount and `finish()`, the `TempDir` is removed with the mount possibly still up. The
900-second watchdog does cover a hang inside `finish()`, because `self` is alive during that call.
Not reproduced, and the mount survived every clean path observed here.

## 16. What this review did not do

- No production source or test file in the PR was modified. The PR's 531 files are byte-identical.
- No lease taken or returned, no branch checked out, reset or stashed, no commit, no push, no merge.
- No other worker's files touched: READY10, READY11, READY3, the original 10, 13, 15, READY13,
  READY16, #96, #111, #42, #43 all left alone.
- No install, sudo, sysctl, reboot, device format, workflow dispatch, rerun or poll.
- Combined-head compatibility with PR #111 and the Translate guard was **not** run. This PR's own
  head is green on macOS for this suite. A combined rerun is still owed.
- The 12,000-entry readdir measurement and any second 6,000-cycle churn run were deliberately not
  repeated, to keep the machine free for the other workers.

## Raw evidence

All logs are under `<lease>/bench/out/requirements19-critic/logs/`:

| File | Content |
| --- | --- |
| `00-pristine-inputs.sha256` | sha256 of the four PR files before any cargo command |
| `01-locked-guard.log` | `cargo metadata --locked`, exit 0, lock unchanged |
| `03-list.log` | `--list`, 8 discovered, host, `mount_nfs` present |
| `04-default-battery.log` | the 5 default tests, 3 ignored, 21.23s |
| `05-mount-before.txt`, `05-mount-tests.log` | mount table before, then the 2 mount tests |
| `06-critic-probe.log` | this reviewer's mounted probe, full printed evidence |
| `07-mutantA.log` | the `AT_SYMLINK_NOFOLLOW` mutant, test fails as it must |

The author's own raw logs stay in the builder's slot at `bench/out/ready-19/` and are **not in the PR**,
so a reviewer cannot read them from the diff. The `docs/verification/ready-19.md` tables are the only
public record of those numbers.