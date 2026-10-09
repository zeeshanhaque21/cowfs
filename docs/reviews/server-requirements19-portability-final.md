# Review: PR #113 portability delta, `395c236` to `b434afe`

Reviewed head: `b434afe98f95db3ad5c134e80747ccad0e1a5732` (`followup/server-requirements-19`).
Prior review of this PR: `docs/reviews/server-requirements19-final.md`, sha256 `7d975c80…`, unchanged.

Verdict: **PASS on the delta's own terms, with the Linux requirement still open.**
The two Linux failures this review was sent to check are genuinely repaired, by measurement rather than
by assertion, and the repair is honest about what it does not fix.
What this delta delivers is a portable harness and a correct statement of an unmet requirement, not a
fix for Linux symlink `chmod`.

Method: the exact head was fetched over HTTPS and materialised with `git archive` into a private copy.
No branch was checked out, reset or stashed, the lease branch is still `review/mounted-fsx-g4` at
`f816b5e`, and the previous review's artifact directory is untouched.

- Private archive: `<lease>/bench/out/requirements19-portable-critic/src`
- Private target dir, logs, two mutant copies: `<lease>/bench/out/requirements19-portable-critic/`
- Host: Darwin 25.6.0, rustc/cargo 1.99.0, TZ PDT-0700
- The co-located FSX critic's `bench/out/fsx-g4-repair-critic/` was not entered.
- The shared daemon, its store, socket and mount were never signalled, read or used as a fixture.

## 1. Source identity and the locked-manifest guard

All 532 tracked files of `b434afe` hashed with `git hash-object` against `git ls-tree -r b434afe`:
**532 tracked, 0 missing, 0 hash mismatches**, and the same after all review work.

`Cargo.lock` sha256 `706645b958fb89e4b90f56a4c31f9c04e68816e0a652137aec7960479628de63`, recorded before
any cargo command and unchanged after every one:

| Point in time | Command | lock |
| --- | --- | --- |
| pristine | none | `706645b9…` |
| after `cargo metadata --locked` | exit 0 | unchanged |
| after `cargo test --locked --no-run` | exit 0 | unchanged |
| after default battery, mount run, protocol, scoped clippy | 0 | unchanged |
| after both mutant builds | n/a | unchanged |

That is the same value this review recorded at `395c236`, so the dev-dependency edge the PR carries was
resolved once and never re-added. The delta itself touches no manifest: `git diff 395c236 b434afe --
Cargo.toml Cargo.lock crates/cowfs-nfs/Cargo.toml` is **empty**.

Worth naming so it is not misread as evidence: the built test binary is named
`requirements19-eb8f6c5a8f063e18` in **both** this review and the previous one, on **different source**.
Cargo names that file from package metadata, not content. Binary naming is not source identity; the
blob comparison in section 2 is.

## 2. Proof-carry: the author's Linux run was at `4dc63ff`, not the head

The portability evidence records the Linux run as `git archive` of `4dc63ff`. The head is `b434afe`.

`git diff --stat 4dc63ff b434afe` is two files, both documentation:
`docs/verification/evidence/server-requirements19-portability.md` (new) and
`docs/verification/ready-19.md`. Blob comparison across the two:

| Path | 4dc63ff vs b434afe |
| --- | --- |
| `Cargo.toml` | identical, `55adec65` |
| `Cargo.lock` | identical, `49c732e0` |
| `crates/cowfs-nfs/Cargo.toml` | identical, `c7700c20` |
| `crates/cowfs-nfs/tests/requirements19.rs` | identical, `381c0c8b` |

So the Linux figures were taken at source byte-identical to the head, and the proof carries.

`git diff --stat 395c236 b434afe` is three files, +714 / -180:
`requirements19.rs` (359 changed), the new evidence doc (169), `ready-19.md` (366).
The delta against the previously reviewed head is one test file plus documentation. No production source.

## 3. What actually changed, and whether it is honest

The previously blocking failure was that two tests asserted macOS symlink semantics with no host gate.
Four commits address it: `a913a4f`, `8d8f8c6`, `4dc63ff`, `b434afe`.

**The native control is now host-derived, not literal.** `a_native_chmod_through_a_symlink_lands_on_the_target`
reads the link's own mode before the `chmod` and compares it after, asserts `target_before == 0o644`,
`target_after == 0o600`, `link_after == link_before`, and `target_before != target_after` so the control
cannot pass as a no-op. That is the correct repair for the old `0o755` literal.

**The combined SETATTR test is split three ways.** The old single test asked one call to set both times
and mode, which cannot hold on a host with no symlink `chmod`:

| test | gate | what it asserts |
| --- | --- | --- |
| `setattr_gives_a_symlink_its_own_times_over_the_real_filesystem` | both hosts | link and dangling link take their own times; target mtime, target bytes and link target string unchanged, read natively; dangling stays a symlink with no target created |
| `setattr_gives_a_symlink_its_own_mode_where_the_host_stores_one` | `#[cfg(target_os = "macos")]`, line 193 | behind a host capability probe; link mode moves, target's does not |
| `setattr_refuses_a_symlink_mode_where_the_host_cannot_store_one` | `#[cfg(target_os = "linux")]`, line 242 | the **backend** refuses when asked directly, the server does not report success, the target is untouched |

The Linux test asks `PathVfs::setattr` directly before going through NFS, and asserts
`direct_mode.is_err()`. That attributes the refusal to the host and backend rather than to the protocol
layer inventing an error, which is the right attribution and the reason this is a real measurement of
`fchmod` on `O_PATH` rather than an assertion about an NFS status code.

**It refuses to call the refusal a contract.** The test's own doc comment says the status is "an
artefact of how the errno is mapped, and a later fix may legitimately answer `NOTSUPP` or succeed".
Confirmed in source: the test asserts `assert_ne!(mode_status, OK)` and prints the number, rather than
asserting `== NFS3ERR_IO`. The dispatch instruction not to approve the Linux requirement as fixed is
honoured by the code itself, not only by this report.

**Atomicity is not claimed.** The combined times-and-mode call records whether the times landed before
the error and asserts nothing in either direction. On Linux the recorded order is
`times_applied_before_the_error=true`, and the test says so without turning it into a guarantee.

## 4. Test counts, measured on both hosts

`--list` on the binary built from this archive reports **10 tests, 0 benchmarks**.
A default run on this Mac reports **7 passed; 0 failed; 3 ignored**.

| test | Mac | Linux, author's run |
| --- | --- | --- |
| `a_native_chmod_through_a_symlink_lands_on_the_target` | default | default |
| `setattr_gives_a_symlink_its_own_times_over_the_real_filesystem` | default | default |
| `setattr_gives_a_symlink_its_own_mode_where_the_host_stores_one` | default (macOS) | absent |
| `setattr_refuses_a_symlink_mode_where_the_host_cannot_store_one` | absent | default (Linux) |
| `a_requested_mount_that_did_not_happen_is_an_error_and_an_unrequested_one_is_a_skip` | default | default |
| `readdir_replies_carry_the_link_count_of_the_moment` | default | default |
| `hardlinked_names_in_one_directory_are_listed_once_each` | default | default |
| `namespace_churn_over_the_real_filesystem_stays_bounded` | default | default |
| `a_large_directory_is_read_once_per_listing` | `#[ignore]` | `#[ignore]` |
| `find_links_counts_the_same_through_the_mount_as_natively` | `#[ignore]` | `#[ignore]` |
| `touching_a_symlink_on_the_mount_leaves_its_target_alone` | `#[ignore]` | `#[ignore]` |

Both hosts report 10 discovered, 7 default, 3 ignored. The one mode test swaps by `cfg`, which is why
the totals match rather than differ. No host gate is expressed as a runtime skip that could hide a
count.

## 5. macOS reproduction on this archive, at `b434afe`

Default battery, one run, `--test-threads=1`:

```
test result: ok. 7 passed; 0 failed; 3 ignored; 0 measured; 0 filtered out; finished in 16.36s

NATIVE_CHMOD link 755 -> 755, target 644 -> 600
SKIP label=no-mount-capability reason=mount_nfs is not available
CHURN iterations=6000 rss_warm=9 MiB rss_end=11 MiB growth=2 MiB
SYMLINK_MODE_CAPABILITY host=macos native_link_mode=755
SYMLINK_MODE host=macos status=0 link 755 -> 600 target=644
```

Every figure matches the author's record, and the two host-specific ones now differ in the right way:
this Mac derives `755` from the host instead of asserting it, and the Linux run derives `777` and takes
the refusal branch.

The `SKIP` line is from the guard's own unit test, which feeds `accept` a synthetic `Err`. It is a
deliberate negative case, not a host capability problem.

Direct RPC regression, separate target, on this archive:

```
test result: ok. 23 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.10s
```

Scoped lint, this lane's own target only:

```
cargo clippy -p cowfs-nfs --test requirements19 -- -D warnings   ->  exit 0
```

## 6. Mounted proof on this Mac, with `COWFS_REQUIRE_MOUNT=1`

```
running 2 tests
RECEIPT label=nfs-mount mount_dev=436209727 backing_dev=16777234 line=localhost:/cowfs-6eda50564fd22b69b64cde64421b0afe on /private/var/.../cowfs-nfs-ready19-Bfr05r/mnt (nfs, nodev, nosuid, mounted by zeeshanhaque)
LINKS native=151 mount_first=151 mount_cached=151
RECEIPT label=nfs-teardown path=/private/var/.../cowfs-nfs-ready19-Bfr05r/mnt mount_dev=436209727 backing_dev=16777234 line=…
RECEIPT label=nfs-mount mount_dev=436209728 backing_dev=16777234 line=localhost:/cowfs-32d0a2e0667661643f8fb8a1ba534092 on /private/var/.../cowfs-nfs-ready19-sO1CJu/mnt (nfs, nodev, nosuid, mounted by zeeshanhaque)
RSYNC ok=true out=""
RECEIPT label=nfs-teardown path=/private/var/.../cowfs-nfs-ready19-sO1CJu/mnt mount_dev=436209728 backing_dev=16777234 line=…
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 8 filtered out; finished in 0.61s
```

- `native=151 mount_first=151 mount_cached=151`, matching the author's record, with `native > 0`
  asserted in the test, so this is not a zero-vacuous pass.
- `mount_dev` `436209727` and `436209728` both differ from `backing_dev 16777234`, and the test asserts
  that inequality. The author's run recorded `436209696` and `436209697`. Those are per-mount
  pseudonymous device ids, not a fixed constant, so a different number is expected and is not a
  discrepancy. The invariant under test is the inequality with the backing device, and it holds.
- **The mount source is a private `PathVfs`-backed in-process server, not Core.** `mounted_backing`
  constructs `PathVfs::new(backing)` and `cowfs_nfs::Mount::new` over it, and the export name
  `localhost:/cowfs-<32 hex>` is this run's own. Nothing here is real-Core acceptance for #15 or any
  pjdfstest gate.
- Teardown receipts are printed per test, and after the run `mount | grep -c cowfs-nfs-ready19` is 0.

## 7. The mount guard is real, and this is negative proof

The old code returned `None` and passed when nothing mounted, so a green run of those two names proved
nothing. The new code is fail-closed by construction and unit-tested in the same default battery:

- `mount_required()` reads `COWFS_REQUIRE_MOUNT` and treats anything but `"0"` as required.
- `mounted_backing` returns `Result<Mounted, String>`; it cannot return a successful-looking `None`.
- `accept(outcome, required)` turns `Err` into `Err("UNMEASURABLE: …")` when the mount was requested,
  and into a labelled `SKIP label=no-mount-capability` only when it was not.
- `a_requested_mount_that_did_not_happen_is_an_error_and_an_unrequested_one_is_a_skip` is a **default**
  test that needs no filesystem and no mount, and asserts all three branches. It ran on this Mac and
  passed.

On Linux with `COWFS_REQUIRE_MOUNT=1` and no `mount_nfs`, the author records
`LINUX_MOUNT_REQUIRED_EXIT=101` and `FAILED. 0 passed; 2 failed`. That is the **negative proof** that a
requested mount cannot turn into a green pass. It is not a test failure to be waived, and it is not
acceptance either: the author's own log line reads `UNMEASURABLE: … this run asked for a mount and did
not get one, so it established nothing`.

Two things this review did **not** do, stated plainly:

- No Linux host was run by this reviewer. No `mount_nfs` install, no privilege, no sudo. The Linux
  figures are the author's, on `moonscapenas`, Debian aarch64, kernel 6.12.109, rustc 1.95.0, 4 cores,
  from `git archive` of `4dc63ff` whose source is byte-identical to this head per section 2.
- The author's raw Linux logs live at `/home/moonscape/cowfs-ready-wave/task-19/logs/` and are **not in
  the PR**. The evidence document is the only public record, same gap as the earlier lane.

## 8. Two Linux failures that are not this lane's

Both are outside `git diff --name-only 395c236 4dc63ff`, which is one file. Verified: neither
`crates/cowfs-nfs/tests/contract.rs` nor `crates/cowfs-vfs-path/src/tests.rs` is in the PR's changed
set, and both are byte-identical to `b434afe` in this review's private archive.

**`crates/cowfs-vfs-path/src/tests.rs:328`**, `readdir_sees_a_name_created_outside_the_vfs_while_a_listing_is_paged`:
32 passed, 1 failed on the author's host. The assertion is an exact listing comparison against
`[a, b, c, d, e]`, so a name created outside the VFS while a listing is paged is the shape that differs.
The same suite passes in CI on `ubuntu-latest`, so this is a property of that host's filesystem, not a
regression from this PR. Not re-run by this reviewer, because running it would mean mutating
`cowfs-vfs-path`, which this lane and this review do not own.

**`crates/cowfs-nfs/tests/contract.rs:233`**, read verbatim from this archive:

```rust
* vfs.fail_with.lock().unwrap() = None;
let (_, _, _, post) = c.lookup(&root, "nope");
assert!(post.is_none() || true);
```

`assert!(post.is_none() || true)` is a tautology: the expression is unconditionally true, so the
assertion can never fail and checks nothing. rustc 1.95's clippy flags it under
`cargo clippy -p cowfs-nfs --all-targets -- -D warnings`; 1.99's does not, which is why the author's
macOS `--all-targets` run is clean and the Linux one is not.

This is a **real defect in the test suite, recorded unresolved and not waived.** It is in a file this
lane does not touch, it belongs to the root-handle commit-barrier work, and CI runs
`cargo clippy --workspace --all-targets -- -D warnings` on ubuntu-latest, so whichever lane owns
`contract.rs` has to replace the tautology with the assertion it was standing in for before the
workspace lint bar can pass on 1.95. Fixing it here would be an unowned edit.

Note the asymmetry that matters for review: the author's Linux run of the *scoped* target,
`cargo clippy -p cowfs-nfs --test requirements19 -- -D warnings`, exits 0, and this review reproduces
that scoped exit 0 on 1.99. Scoped-green does not establish workspace-green.

## 9. Mutation checks: both discriminate

Two mutants, each in its own copy of the archive. The 532 pristine files were untouched throughout, and
re-verified afterwards.

**Mutant A, the #19 bug itself.** `crates/cowfs-vfs-path/src/sys.rs`, `utimensat` only, the flag
argument changed from `libc::AT_SYMLINK_NOFOLLOW` to `0`. This is the identical mutation to the one in
the previous review, so the flag-0 mutant is carried across unchanged, and the host-capability derive
is untouched by it.

```
test setattr_gives_a_symlink_its_own_times_over_the_real_filesystem ... FAILED
panicked at crates/cowfs-nfs/tests/requirements19.rs:142:5:
  left: (1791171100, 541267851)
 right: (1000, 5)
test result: FAILED. 0 passed; 1 failed; 0 measured; 0 filtered out
```

The reply carried the target's real mtime instead of the requested stamp, and the test failed on the
**timestamp assertion**, at a meaningful operation rather than a bookkeeping line. In the previous
review the equivalent failure was at line 108 of the combined test; the split moved it to line 142 of
the times test, which is the right place for it.

**Mutant B, a fake-success no-op.** `sys::fchmod` replaced with a body that returns `Ok(())` without
calling the syscall. The server therefore reports success and writes nothing.

```
SYMLINK_MODE_CAPABILITY host=macos native_link_mode=755
SYMLINK_MODE host=macos status=0 link 755 -> 755 target=600
panicked at crates/cowfs-nfs/tests/requirements19.rs:224:5:
assertion `left == right` failed: the reply reports the link's new mode
  left: 493
 right: 384
test result: FAILED. 0 passed; 1 failed; 0 measured; 0 filtered out
```

`status=0` is success and `link 755 -> 755` is nothing written. The test still failed. A backend that
reports OK and does nothing is caught, which is the property that matters for a requirement test.

Both are synthetic artifact mutations inside this reviewer's own archive, labelled mutants.
Neither is a production defect and neither is a proposed fix.

## 10. Claims that stay bounded

- **Linux symlink `chmod` is still open.** `fchmod` on `O_PATH | O_NOFOLLOW` is `EBADF`, the server
  answers `NFS3ERR_IO`, and the author's log shows `backend_setattr=Some("i/o error: errno 9")` and
  `nfs_status=5`. Closing it needs a production decision in `cowfs-vfs-path` that this lane does not
  own. The delta does not claim to fix it and this review does not approve it as fixed.
- **The refusal is not a contract.** The test names the status it saw and calls it an errno-mapping
  artefact. A later fix may legitimately answer `NOTSUPP`, or succeed. Nothing downstream may depend on
  `5`.
- **Resident memory.** Mac `9 MiB` warm, `11 MiB` end, growth `2 MiB`; Linux `8 MiB` warm, `10 MiB` end,
  growth `1 MiB`. The printed growth is `bytes >> 20`, an integer truncation, so the Mac delta lies in
  `[2 MiB, 3 MiB)` and the Linux delta in `[1 MiB, 2 MiB)`. The one-MiB difference between hosts is a
  reporting-granularity artefact and the underlying byte deltas were not captured to more precision.
  Observed scope only; precision is not recomputed from the rounded labels. Against a 64 MiB bound, on
  a shared machine, one run per host, this is **not** a leak result, no root cause, and the named
  suspect stays a hypothesis.
- **readdir paging.** Still `#[ignore]`d, still the author's two runs, still a structural bound
  (`mean_rest` against one native directory read) and **not** the 1.5x build-overhead criterion. Not
  re-run here.
- **The 3 mount tests are manual-only.** No CI job passes `--ignored` for `-p cowfs-nfs`; the
  `linux-fuse` job names `cowfs-vfs-path` and `cowfs-fuse` only. `COWFS_REQUIRE_MOUNT` makes a manual
  run honest; it does not create CI coverage.

## 11. CI at this head

One read, no poll, no dispatch, no rerun:

| Job | Status at read |
| --- | --- |
| `check (ubuntu-latest)` | in_progress |
| `check (macos-latest)` | in_progress |
| `linux-fuse` | in_progress |

**CI for `b434afe` is pending and this review does not report a conclusion for it.** The author's
statement that exact CI is pending is a disclosure, not an execution. Two author reads are cited in the
evidence document and are labelled as the author's, not replayed here.

What can be said without CI: the source that ubuntu-latest will compile is byte-identical to what this
review compiled and ran on macOS, and the two assertions that failed at `395c236` no longer exist in
that form. Whether the ubuntu job is green is not established by this review, and the workspace clippy
tautology in section 8 is a live reason to expect it not to be.

## 12. Combination with current main

Main is now `951045f`, "Merge pull request #96 … nfs-namespace-durability-90".

- `git merge-base 951045f b434afe` is `46b0f269`, and `951045f --is-ancestor b434afe` is **false**. The
  branch is still cut from an older main, so the base drift noted in the previous review persists. The
  API's `base.sha` is the current tip, not the fork point.
- `git merge-tree --write-tree 951045f b434afe` exits **0** and writes tree `9ffa89ae…`, so the
  combination is **conflict-free**. No source edit, cherry-pick or branch merge was performed.
- The PR's one shared-file change is the `cowfs-vfs-path` dev-dependency key inside an existing table,
  and `Cargo.lock` carries the matching edge once. The clean merge-tree is consistent with that.
- **Not run:** the combined head was not built or tested. No test result here may be read as a green
  run against main. The earlier lane's Path artifacts are bound to `395c236` source, not to `951045f`.
- The combined run against PR #111 and the Translate guard is **still owed**. This review did not
  perform it and the PR's own "Not claimed" section says so.

## 13. #19 stays open, and nothing here closes it

`closingIssuesReferences` via GraphQL at this head is an **empty node list**, which is authoritative.
The PR body says "Reconciles the server requirement list in #19" and never uses a closing keyword.
All four added commit messages were read in full: **none contains `close`, `fix` or `resolve` against an
issue number.** The body does contain the word "Closing" once, at line 59, in "Closing it needs a
production decision in `cowfs-vfs-path`" where *it* is the requirement, not an issue. That is not a
GitHub closing phrase and creates no auto-close link. No negated-closing-phrase bug is present.

Issue **#19 is open** and must stay open. Also open and untouched by this delta:

- #107 typed `create` for fifo, socket and device node.
- #108 `pathconf` answers -1, so `NAME_MAX` and `PATH_MAX` cannot be read.
- #109 `rmdir` of a trailing `..`, and `nlink` after unlinking an open file.
- #110 `chown` to another uid answers success and changes nothing.
- #43 Translate review, security review, dead-server hang. The AppleDouble `._*` default-hide policy
  remains a **user and maintainer decision**; no default may be flipped on this evidence.
- The nfsserve fork or upstream-contribution decision.
- Resident-memory root cause, and the store-side growth question the author lists as untracked.
- Linux symlink `chmod`, as above.

## 14. Resource and bounded-lane policy

- This review used **one** bounded foreground wait for the main verification, inside a single 600 s cap,
  plus short separate waits for the build, protocol, scoped clippy and the two mutants. There was no
  10 x 600 s pattern and no repeated busy-wait.
- **Disclosed breach in the author's lane:** the prior readiness material for this task records
  3 x 600 s author runs with expiry. That is a bounded-lane-policy breach. It is recorded, not
  replicated, and no aggregate waiting is reported here.
- Not run, deliberately: the 12,000-entry readdir measurement, any repeat churn run, the workspace-wide
  clippy, and the combined-main build. The machine is shared with at least ten other active slots.
- **Not loaded this round:** `caveman` and `ponytail` skills were not invoked; the terse-report and
  simplest-solution rules were applied directly. `environment-traps` was loaded before the mounted run.
- No installs, sudo, sysctl, reboot, device formatting, runner configuration, workflow dispatch, rerun
  or polling.

## 15. Cleanup

| Check | Result |
| --- | --- |
| mounts before the mount run | 2: shared daemon `~/.cowfs/mnt`, OrbStack |
| mounts after | the same 2 |
| `mount \| grep -c cowfs-nfs-ready19` after | 0 |
| `cowfs-nfs-ready19-*` temp directories | 0 |
| shared daemon PID 15263 | alive, started Sat Oct 3 20:44:29 2026, `--backend core`, never signalled |
| previous review artifact `bench/out/requirements19-critic` | present, untouched |
| co-located `bench/out/fsx-g4-repair-critic` | not entered |
| lease branch | `review/mounted-fsx-g4` at `f816b5e`, never checked out or reset |
| previous report `server-requirements19-final.md` | sha256 `7d975c80…`, unchanged |
| 532 tracked PR files after all work | byte-identical |
| other-lane files `contract.rs`, `cowfs-vfs-path/src/tests.rs` | byte-identical, never edited |

All signals and unmounts were path-scoped to this review's own private mountpoints, taken from the
`mount` output the test itself printed, with the mount table re-read afterwards. No `pkill`, no process
group kill, no mount-table walk, no signal to any PID other than work this review started.

## 16. Raw evidence

Under `<lease>/bench/out/requirements19-portable-critic/logs/`:

| File | Content |
| --- | --- |
| `00-pristine.sha256` | the four PR files before any cargo command |
| `01-metadata.err` | `cargo metadata --locked`, exit 0, no diagnostics |
| `02-mounts-before.txt` | mount table baseline |
| `03-mac.log` | discovery, 10 tests; default battery 7/3 with all receipts; mount run with `COWFS_REQUIRE_MOUNT=1` |
| `04-protocol-clippy.log` | protocol 23 passed; scoped clippy exit 0 |
| `05-mutants.log` | mutant A, `AT_SYMLINK_NOFOLLOW` -> 0, fails at the timestamp assertion |
| `06-mutantB.log` | mutant B, no-op `fchmod` reporting success, still fails |

Mirrored copy of this report, same bytes, in the artifact directory alongside the logs.