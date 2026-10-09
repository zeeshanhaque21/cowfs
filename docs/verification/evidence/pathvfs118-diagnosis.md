# Diagnosis: `cowfs-vfs-path` readdir test failure on moonscape, issue #118

Refs #118, and through it #19. Nothing here is a fix: the source was read-only for this task, and
PR #113 was left untouched at `b434afe98f95db3ad5c134e80747ccad0e1a5732` while its independent
review runs.

Every statement below is labelled **measured** (with the command and the artifact) or **hypothesis**.
Where a fact could not be obtained without an unauthorized action, it is listed as missing rather than
filled in.

Lease: `.treehouse-ready-wave/.treehouse/cowfs-7c1bf8/1/cowfs` (slot 1).
Local artifacts: `bench/out/pathvfs118/`.
Remote artifacts: `/home/moonscape/cowfs-ready-wave/task-19/{logs/pathvfs118,stamp_probe.rs,stamp_probe}`.

## 1. Failure identity, measured

| | |
| --- | --- |
| test | `tests::readdir_sees_a_name_created_outside_the_vfs_while_a_listing_is_paged` |
| location | `crates/cowfs-vfs-path/src/tests.rs:328:5` |
| assertion | `assert_eq!(left, right, "'a' was listed in the page taken before it was removed, 'e' was created outside")` |
| left (got) | `[[97], [98], [99], [100]]` |
| right (want) | `[[97], [98], [99], [100], [101]]` |
| direct rc | `101` |
| suite | `32 passed; 1 failed; 0 ignored`, 33 tests, 0.46s |

Original log preserved byte-exactly:
`bench/out/pathvfs118/original/linux-vfspath.log`, 3725 bytes,
sha256 `ae7381e248247f8531d8946b5739c8818f70ce168b0461f35ff51ea43ac06471`, identical on the remote
host before and after the copy. The companion `linux-default.log`,
sha256 `12bc0d1aa233f845d6e94305a4b2e910fefe32116830bf734907deebf88c7c1b`, is beside it.

97, 98, 99, 100, 101 are `a`, `b`, `c`, `d`, `e`. The name `e`, created outside the `Vfs` between two
pages of one listing, never appeared.

## 2. Source revision, measured

| | |
| --- | --- |
| commit | `4dc63ff7ce1b795d63265fc40a99b6434f8bc65d` |
| blob | `crates/cowfs-vfs-path/src/tests.rs` = `90de21aa99f22c1d75f6f45f571ff1d33087597b` |
| cache signal | `crates/cowfs-vfs-path/src/table.rs:169-195`, stamp fields read at `:176-184` |
| cookie table | `crates/cowfs-vfs-path/src/cookies.rs:18-35` |
| scratch dir | `std::env::temp_dir()` (`tests.rs:11-19`), so `$TMPDIR` or `/tmp` |

The stamp is six fields of the directory's own `fstat`: `mtime` seconds and nanoseconds, `ctime`
seconds and nanoseconds, `size`, `nlink`. `readdir_page` re-reads the directory when the cookie is
zero, when nothing is cached, or when that stamp differs from the one stored with the cached listing.

## 3. Runtime and host, measured

| | |
| --- | --- |
| host | `moonscapenas`, Linux `6.12.109+rpt-rpi-2712` aarch64, 4 cores |
| toolchain | `rustc 1.95.0 (59807616e 2026-04-14)`, `cargo 1.95.0 (f2d3ce0bd 2026-03-21)` |
| cwd of the run | `/home/moonscape/cowfs-ready-wave/task-19/src-4dc63ff`, a `git archive` of the commit, 531 files |
| `TMPDIR` | unset, so the scratch directory was under `/tmp` |
| `/tmp` | **`tmpfs`**, `rw,relatime`, 8.0G |
| task root, `$HOME` | `/dev/sda2`, `ext4`, `rw,noatime,nodiratime`, 53G free |
| lock edge | `Cargo.lock` sha256 `706645b9…` unchanged before and after every command |

`CONFIG_HZ` is not readable: `/proc/config.gz` does not exist for this kernel, so the tick is
inferred from measurement below, not read from configuration.

## 4. Native controls

Two controls were run so that the failure could be attributed rather than guessed. The first one was
**invalid and is recorded as such**; the second replaced it.

### 4a. An invalid probe, and what it wrongly suggested

**Measured, and wrong.** The first probe ran `: > base/new` (truncate of a file that already exists,
which does not change the parent directory) and `rm -f base/gone` (removal of a name that does not
exist). It therefore measured a no-op and reported the stamp as never moving: 199/200 unchanged on
tmpfs and 200/200 on ext4. Those numbers say nothing about the failing test and must not be quoted.

Its one real result was accidental and still stands: the failing test reproduced identically with
`TMPDIR` pointed at ext4, so the failure is not a tmpfs artifact.

### 4b. The control that replays the test's own sequence, natively

**Measured.** 200 trials per filesystem. Each trial creates `a`, `b`, `c`, `d`, stamps the directory,
creates `e`, removes `a`, stamps again, and compares all six fields.

```
REPLAY fs=tmpfs type=tmpfs        trials=200 stamp_unchanged=39  stamp_changed=161
REPLAY fs=ext4 type=ext2/ext3     trials=200 stamp_unchanged=27  stamp_changed=173
REPLAY_NANOS fs=tmpfs  whole_seconds_advanced_while_stamp_unchanged=0
REPLAY_NANOS fs=ext4  whole_seconds_advanced_while_stamp_unchanged=0
```

So on both filesystems the six-field stamp is genuinely unchanged across a create-and-remove a
substantial fraction of the time. The rate is lower than the test's own because a shell trial
contains two `stat` forks, roughly a millisecond each, which widens the measured window; the test's
window is far tighter.

### 4c. Granularity, and cached descriptor versus fresh lookup

**Measured.** A std-only Rust program (`stamp_probe.rs`, sha256
`2a0818d2619f5b484df140b40f6822b189ccc722c4823958062602dcab58ec72`, built with plain `rustc`, no cargo
and no dependencies) runs 500 trials in the test's tight regime and compares two ways of reading the
stamp: `fstat` on one long-lived cached directory descriptor, which is what `PathVfs::dir_fd` hands to
`readdir_page`, and a fresh `std::fs::metadata(path)` lookup, which is what a shell `stat` does.

```
PROBE118 fs=tmpfs trials=500 cached_fd_unchanged=1   cached_fd_changed=499 fresh_stat_unchanged=1   fresh_stat_changed=499 cached_fd_and_fresh_stat_disagreed=0
PROBE118 fs=ext4  trials=500 cached_fd_unchanged=498 cached_fd_changed=2   fresh_stat_unchanged=498 fresh_stat_changed=2   cached_fd_and_fresh_stat_disagreed=0
PROBE118_DELTA fs=tmpfs nonzero_sec_deltas=1 min=Some(4000006) max=Some(4000006)
PROBE118_DELTA fs=ext4  nonzero_sec_deltas=2 min=Some(4000006) max=Some(4000006)
```

Two facts fall out, and they are the whole diagnosis.

1. **A cached directory descriptor is not the cause.** The two ways of reading the stamp never
   disagreed, 0 of 500 on either filesystem. That hypothesis is eliminated by measurement.
2. **Directory timestamps on this host move in 4 ms steps.** Every non-zero delta observed, on both
   filesystems, was exactly `4000006` ns. In the test's own regime the stamp is unchanged 498 of 500
   times on ext4.

### 4d. Determinism, and a control that passes

**Measured.**

- The exact failing test, same binary, nothing changed, 20 consecutive runs: `passed=0 failed=20`.
  Deterministic on this host, not flaky.
- With `TMPDIR` pointed at ext4: fails with the identical `left` and `right`. Not a tmpfs artefact.
- `tests::readdir_cookies_survive_removal_of_the_entry`, which needs no external change between
  pages: `1 passed`. The cookie machinery works on this host.

## 5. Diagnosis, evidence-ranked

**Rank 1, measured, high confidence. The test asserts an invariant that this host cannot observe.**
`PathVfs` has exactly one signal for "someone else changed this directory": the six-field stamp. When
that stamp does not move, `readdir_page` takes the cached-listing branch and no name that appeared
during the paging is seen. The observed `left` is precisely that branch's output: page one contributes
`a` and `b`, the resumed pages contribute `c` and `d`, `a` is skipped because its name is gone, and
`e` is absent because the listing was never re-read. On this host the directory mtime advances in 4 ms
steps and the test's create-and-unlink window is well under that, so the stamp is byte-identical and
the assertion cannot hold. Deterministic here, 20 of 20, and reproduced independently of the test by a
probe that never touches cowfs.

**Rank 2, source-read, supported by measurement. The cookie table is not at fault.**
`Cookies::sync` (`cookies.rs:18-35`) keeps the position of every surviving name and gives a new name a
higher one, and it reads nothing from the host. Had the listing been re-synced with `[b, c, d, e]`,
the resume from `b`'s cookie would have emitted `c`, `d`, `e` and the assertion would have passed. The
output shows it was not re-synced. The sibling control test passes here.

**Rank 3, real but separate, and not a defect the test assumes. A create-and-unlink inside one
timestamp tick is invisible to any resumed listing.** For an adapter serving a directory under a
mountpoint, that is a genuine staleness window: a client resuming a listing can miss a name that
appeared and vanished between two pages. It is bounded by the host's directory timestamp granularity.
Widening the signal is a `cowfs-vfs-path` design decision and is not this lane's to take.

**Corrected claim.** An earlier statement in the PR #113 body said the failure "is a property of this
machine's filesystem", on the grounds that the suite passes on `ubuntu-latest`. Measurement does not
support the filesystem half of that: the test fails identically on ext4. What differs between the
hosts is the granularity of directory timestamps, not the filesystem and not the code. PR #113 is under
independent review and was not modified; this correction is recorded here for the reviewer and the
coordinator to apply.

## 6. What is still missing

- **The `ubuntu-latest` directory timestamp granularity.** This is the one fact that would confirm the
  cross-host half. Measuring it needs a new CI run, which is not authorized for this task, so it stays
  a hypothesis: that runner's kernel advances directory timestamps finely enough that the same window
  straddles a change.
- **`CONFIG_HZ` on moonscape.** `/proc/config.gz` is absent. The 4 ms figure is measured; that it is
  `1/HZ` with `HZ=250` is consistent with it but not read from configuration.
- **Whether the 4 ms step is the kernel tick or the filesystem's `s_time_gran`.** Both would produce
  the same observation and they were not separated.
- **The failing CI run's toolchain.** The preserved log is the test output only; the runner's rustc
  version was not read from it.

## 7. Narrow plan, proposed and not implemented

The source was read-only for this task. Nothing below has been applied, and no assertion is weakened
and no host is excluded.

**Preferred, test-side, in `crates/cowfs-vfs-path/src/tests.rs`.** After the external create and
removal, wait until the directory's stamp has actually moved before resuming the listing: poll the
same `fstat` with a bounded deadline of a few hundred milliseconds and fail loudly if it never moves.
The assertion stays exactly as it is, an externally created name must be visible to a resumed listing,
and the dependency on tick granularity disappears. This is the change that keeps testing the
invariant everywhere.

**Alternative, test-side, if a poll is judged too clever.** Measure the host's directory timestamp
granularity once, the way §4c does, and when it is coarser than the change window emit a labelled
`SKIP` carrying the measured number. That records the environment instead of asserting something the
environment cannot show. It is weaker than the first option and should be the fallback, not the
choice.

**Rejected.** A `#[cfg(target_os)]` gate, which is a waiver rather than a fix and is not even
mac-specific here. An arbitrary sleep, which is slow and still flaky.

**Separate, not this lane.** Whether `listing_stamp` should be augmented so a resumed listing re-reads
when the directory's entry count could have changed inside one timestamp tick belongs to whoever owns
`cowfs-vfs-path`. It should be its own issue with the §4c numbers attached, not a change smuggled in
with a test fix.

## 8. Ownership and safety

- PR #113 branch, source and pushed head untouched: local and `origin/followup/server-requirements-19`
  both `b434afe98f95db3ad5c134e80747ccad0e1a5732`.
- Issue #117, the tautological `contract.rs` assertion, is not touched here. It belongs to the barrier
  worker.
- No production file, test file or manifest was modified. `git diff` in the lease is empty of source
  changes; artifacts live under `bench/out/pathvfs118/`.
- Writes were confined to `/home/moonscape/cowfs-ready-wave/task-19/` and this lease's
  `bench/out/pathvfs118/`. No other worker's directory, corpus or store was written.
- No mount was created, walked, signalled or removed. No process was signalled. The shared Mac daemon
  PID 15263 was not touched.
- Resource: three bounded 600 second foreground acquisitions of
  `/home/moonscape/cowfs-ready-wave/linux-heavy.lock`, each as one foreground invocation whose parent
  outlived its child. The lane was free on every attempt, so no expiry occurred. Nothing ran unlocked.
  Free space before and after: 53 GiB, well over the 20 GiB floor. Artifacts are a few hundred KiB
  against the 8 GiB cap.

## 9. Artifact hashes

| artifact | sha256 |
| --- | --- |
| `original/linux-vfspath.log` | `ae7381e248247f8531d8946b5739c8818f70ce168b0461f35ff51ea43ac06471` |
| `original/linux-default.log` | `12bc0d1aa233f845d6e94305a4b2e910fefe32116830bf734907deebf88c7c1b` |
| `remote-logs/run.log` | `1b556c1f893ec9ce44d154b48cd5a780ff86402debcbdfabb74cb73a4c8c3e01` |
| `remote-logs/probe2.log` | `3e298b63723a5313dbda77da62572dad16a5ccdee1da34532a87d2da08f6b37e` |
| `remote-logs/exp3.log` | `1f2c25dd7f591edb8ce06099ff5f3d06533c7fbe569e8e666d63c67c134187cf` |
| `remote-logs/test2-tmpfs.log` | `346e7d16ad335225d2e49ff5fd1b67aee2279268bbebcdecaaed471363286ea5` |
| `remote-logs/test2-ext4.log` | `837e56b976779c964b77d5a06da3e9d6fcb019d9652a7a424a4315f012bdbc46` |
| `remote-logs/test2-sibling.log` | `b8276715ac9ce7aeba403b659962482b12c4b27ffa9965f474d11ff97aafd08f` |
| `stamp_probe.rs` (remote and local) | `2a0818d2619f5b484df140b40f6822b189ccc722c4823958062602dcab58ec72` |