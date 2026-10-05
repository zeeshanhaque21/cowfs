# Ready task #20: open-FD holders and `.nfs` dirt on a slot

Task: issue [#20](https://github.com/zeeshanhaque21/cowfs/issues/20).
Lease: `.treehouse-ready-wave/.treehouse/cowfs-7c1bf8/2/cowfs`, branch `fix/open-fd-holders-20`.
Base: `46b0f269d5bef4a2c204c25f5b3015da601d3beb`.
PR: [#112](https://github.com/zeeshanhaque21/cowfs/pull/112).
Head at the time of writing: `c82bb359ba8de069f6f4a211536435792cb4a7c5`.

## What was broken

`treehouse return` finds lingering processes by working directory alone
(`internal/process/detect.go`, `p.Cwd()`), so a process that has chdir'd out of a slot while still
holding a file or a `flock` inside it is missed.

Off a network mount that costs an orphan process.
On one it costs the slot: the reset unlinks the held file, the macOS NFS client silly-renames it to
`.nfs*`, `return` still exits 0 and prints that it returned the worktree, and `treehouse status`
reports the slot as `dirty` for good, because the next `get` skips dirty slots.
A held descriptor or lock also makes `umount` fail with "Resource busy".

cowfs already had the detector and already used it for a mode (b) snapshot.
What was missing was the case the issue is actually about.

## What changed

Commit `d699bfe51a4509addd7f6398ec5f2467c63d22e4`, pushed over authenticated HTTPS.

| File | Change |
| --- | --- |
| `crates/cowfs-treehouse/src/mode_a.rs` | Scan the slot's own mount-relative directory, not only a snapshot. Refuse a hold treehouse cannot see. Refuse an unsignalable holder. Report what the scan established. |
| `crates/cowfs-treehouse/src/cli.rs` | Mode (a) connects to a named `--socket`, which is what lets a slot on a mount be scanned at all. |
| `crates/cowfs-daemon/src/holders.rs` | `Scan` separates "nobody is holding this" from "this platform could not be asked". Fail closed. Bounded `lsof` that can actually enforce its deadline. |
| `crates/cowfs-daemon/src/handler.rs` | `ps` answers `unsupported` when the platform cannot scan, and refuses a name that does not resolve inside the mount. |
| `crates/cowfs-ctl/src/validate.rs`, `server.rs` | `ps` takes a mount-relative directory name; a new rule keeps it inside the mount. |
| `crates/cowfs-treehouse/src/th.rs` | `PoolEntry.leased` deserialised as false for ever: treehouse prints a `status` string, not a boolean. |

### The policy, narrowly

- A hold treehouse cannot see, an open file or a lock, **stops the return before treehouse is asked
  to release anything**. The file is named and `--force` is named as the way out.
- A working-directory holder is **left to treehouse**, which refuses it in its own words. No existing
  message or exit code changes for that case.
- A pid this process may not signal, such as the shell that invoked the return, is refused.
  Mode (b) would meet it again at the reset; mode (a) has no second gate.
- Nothing is ever signalled without `--force`, and the kill is treehouse's own SIGTERM, 2 s,
  SIGKILL policy.
- An empty scan is never reported as proof. `ReturnOutcome.holder_scan` says `not scanned` when the
  slot is not inside the daemon's mount.

### The fail-closed half

The quiet failure is an unanswered scan that reads back as a clear slot.
Every one of these used to answer "no holders":

| condition | now |
| --- | --- |
| `/usr/sbin/lsof` missing or not executable | `unsupported`, caller blocks |
| `lsof` exits with anything but 0 or 1 | `unsupported`, caller blocks |
| `lsof` outruns its deadline | `unsupported`, caller blocks |
| `/proc` or `/proc/locks` unreadable on Linux | `unsupported`, caller blocks |
| the prefix cannot be resolved | `unavailable`, caller blocks |

`lsof` exiting 1 is its "matched no file" and stays an answer, because it is the common clean case.

The deadline used to be unreachable: `read_to_string` on the child's stdout blocks until EOF, so a
wedged mount hung the daemon instead of reporting that the answer was unknown. The pipes are now
drained off the waiting thread, each on its own channel so neither stream can be mistaken for the
other.

## Tests

`cargo test --locked -p cowfs-treehouse -p cowfs-ctl -p cowfs-daemon -j2`, through the wave's
resource lock, exit 0.

| crate | suites | passed | failed | ignored |
| --- | --- | --- | --- | --- |
| `cowfs-ctl` | lib 11, admission 14, flood 1, regress 19, server 31, wire 20 | 96 | 0 | 0 |
| `cowfs-daemon` | lib 53, end_to_end | 53 | 0 | 5 |
| `cowfs-treehouse` | lib 60, issue20 4, regressions 14, sandbox_treehouse 15, spawned_server 6, stub_server 22 | 121 | 0 | 0 |
| total | | **270** | **0** | 5 |

The 5 ignored are `cowfs-daemon` `end_to_end` cases that are not for this platform; they were already
ignored at the base commit.

`cargo clippy --locked -p cowfs-treehouse -p cowfs-ctl -p cowfs-daemon --all-targets -j2 -- -D warnings`
exit 0.
`cargo clippy --workspace --all-targets -j2 -- -D warnings` exit 0, the same command CI runs.

CI at `c82bb35`, run 37256710176, all three jobs green: `check (ubuntu-latest)`, `check (macos-latest)`,
`linux-fuse`.

### The one thing a scoped macOS check cannot see

The first push was red twice, and both were mine.

`cargo fmt --all --check` wanted rustfmt's line breaking in three files.
Fixed in `38ac3f1`.

Then ubuntu failed `cargo clippy --workspace --all-targets -- -D warnings` with
`error[E0308]: mismatched types` at `crates/cowfs-daemon/src/holders.rs:123`: `locked()` had been
changed to return `Result` for the fail-closed scan, and the `/proc` module returned the bare set at
the end. **macOS never compiles that module**, so the scoped macOS check, the scoped macOS clippy and
the workspace macOS clippy were all green and CI was the first thing to look at it.
Fixed in `c82bb35`.

Not left to inspection this time: the tree was copied to moonscape (aarch64 Linux) under
`/home/moonscape/cowfs-ready-wave/task-20/src` and
`cargo check --locked -p cowfs-daemon -p cowfs-ctl -p cowfs-treehouse --all-targets -j2` ran there
under the wave's remote lock. Exit 0, no errors.
Cross-compiling from the Mac to `x86_64-unknown-linux-gnu` was tried first and cannot work here:
`zstd-sys` has a build script that needs `x86_64-linux-gnu-gcc`, and installing a cross toolchain is
not permitted.
`cargo clippy` on the box is not a usable signal: its clippy is 1.95.0 and reports a pre-existing
`collapsible_match` in `crates/cowfs-meta/src/tx.rs`, a file this task never touched.

### The four new cases

`crates/cowfs-treehouse/tests/issue20.rs`, all four passing, each with a **real** detached child that
has chdir'd out of the slot and holds an open descriptor in it, and each asserting the slot is still
leased afterwards:

1. `a_mode_a_slot_on_the_mount_is_scanned_and_an_unseen_holder_refuses_the_return`
   Exit 5, names the held file and `--force`, nothing signalled, the file is still there, the lease
   is still held.
2. `force_kills_the_real_holder_and_only_then_returns_the_slot`
   `--force` signals the real pid, the pid dies, and only then does the slot go back.
3. `a_mode_a_slot_off_the_mount_is_reported_as_unscanned_and_still_returns`
   The off-mount mode (a) case is unaffected, and reports `not scanned` rather than a clean scan.
4. `a_daemon_that_cannot_answer_the_holder_scan_stops_the_return`
   A handler whose `ps` answers `unsupported` stops the return, names the missing capability, and
   changes nothing.

In `crates/cowfs-daemon/src/holders.rs`, five more, including the discriminating one:
`a_holder_that_chdird_out_is_still_reported_and_only_by_its_descriptor` asserts the found process has
an `fd` hold and **no** `cwd` hold, so a detector that only looked at working directories would fail
it.

## Cross-lane surfaces touched

Reported, not silently taken.

- `crates/cowfs-ctl/src/server.rs`, the `Request::Ps` validation arm only.
  The arm validates `p.snapshot`, which forbids `/` and a leading dot, so it rejected a mode (a)
  mount-relative slot directory outright.
  No shutdown behaviour was touched.
- `crates/cowfs-daemon/src/handler.rs`, the `holders` function only.
  It is the same file that carries the `import` implementation owned by the #97 worker; the import
  path is untouched.
- `crates/cowfs-daemon/src/holders.rs` `scan()` is still best-effort for the two `exports.rs` callers
  (`mount_snapshot`, `unmount_snapshot`), which keep exactly today's behaviour and now log the
  reason to stderr. Those two gates are still fail-open on an unavailable platform.
  They are not destructive, and they are not mine: report, do not widen.

## Linux, on a real Linux

The tree was copied to `moonscape@192.168.68.119` (Debian aarch64) under
`/home/moonscape/cowfs-ready-wave/task-20/src`, 9.2 MB of source plus a 1.1 GB target directory, and
run there under the wave's remote lock.

`cargo check --locked -p cowfs-daemon -p cowfs-ctl -p cowfs-treehouse --all-targets -j2`: exit 0, no
errors. This is what caught the `/proc` compile error a macOS-only check cannot see.

`cargo test --locked -p cowfs-daemon --lib`: exit 0, **49 passed, 0 failed**.
The four fewer than macOS are the `lsof`-specific cases, compiled out.
The one that matters here is the same discriminating case, now driven entirely by `/proc`:

```
test holders::tests::a_holder_that_chdird_out_is_still_reported_and_only_by_its_descriptor ... ok
```

so the Linux scan does name a process whose working directory is elsewhere and does so through its
open descriptor, and `a_prefix_that_cannot_be_resolved_is_unavailable_rather_than_clear` passes too,
which is the fail-closed half on Linux.

## Remaining acceptance

1. **FUSE.** The `fd`/`lock` scan has been run against the macOS NFS loopback, not against a real
   FUSE mount, where there is no silly-rename and a held descriptor is a plain open file.
   Issue #20's work item "re-check on Linux (`gopsutil` reads `/proc`) and on FUSE" is half done:
   the `/proc` half is measured above, the FUSE half is not.
2. **Independent review**, before merge. CI is green at the head.
3. **The upstream proposal** is drafted at `docs/upstream-treehouse-proposal.md` section 3 and has
   **not** been sent. Section 3 gained one paragraph: an unanswered scan must not read as an empty
   one, which is the detail cowfs learned implementing it.

## Safety trail

- Shared daemon PID 15263, its store `~/.cowfs/store`, its mount `~/.cowfs/mnt` and its socket were
  never signalled, reset or used as state, and PID 15263 still carries its original start time.
  NFS mounts after the run are the shared one and OrbStack only.
- Every `treehouse` invocation in the automated tests goes through the sandbox shim in
  `crates/cowfs-treehouse/tests/common/mod.rs`, which refuses any call without an explicit `--root`
  inside the sandbox and refuses any absolute path argument outside it.
  The mount run uses its own sandbox `HOME` and its own pool inside its own mount.
- Every signal in the mount harness is by pid after that pid's argv was verified, never by pattern.
- The private daemon `att2` left behind was stopped by pid 83803 after verifying its argv, its start
  time and its own mount line; its mount disappeared with it.
- No `treehouse return`, `prune` or `destroy` was run against a leased slot in any coordinator pool.
  The only pool used is the one created inside this run's own mount.

## Old fail, new pass

The new test file is kept in both runs; only the product files move, reverted to the base commit and
then restored. Script: `bench/out/ready-20/old-fail-new-pass.sh`.

| code | `cargo test -p cowfs-treehouse --test issue20` |
| --- | --- |
| base `46b0f26` | exit 101, **0 passed, 4 failed** |
| fix `d699bfe` | exit 0, **4 passed** |

All four fail at the base commit:
`a_mode_a_slot_on_the_mount_is_scanned_and_an_unseen_holder_refuses_the_return`,
`force_kills_the_real_holder_and_only_then_returns_the_slot`,
`a_mode_a_slot_off_the_mount_is_reported_as_unscanned_and_still_returns`,
`a_daemon_that_cannot_answer_the_holder_scan_stops_the_return`.

## Live mount: the real `.nfs` silly-rename and the real holder scan

`bench/out/ready-20/run-mount-sample.sh`, attempt `att4`, exit 0, run once through the wave's
resource lock. Raw evidence in `bench/out/ready-20/att4/log/`.

Everything was private to the attempt: store `att4/store`, mount `att4/mnt`, socket
`att4/../ready-20-att4/daemon.sock`, sandbox `HOME` `att4/home`, pool `att4/mnt/base`.

The socket is the one artifact that cannot live in the lease: `sockaddr_un.sun_path` is 104 bytes on
macOS and this lease's path is already 95, so the socket went to the pre-approved external temp
directory, 91 bytes, and was removed at the end. Store, mount and logs all stayed in the lease.

Binaries, from this lease's target directory, debug profile:

| binary | built | source |
| --- | --- | --- |
| `cowfs-daemon` | Oct 4 16:36 | this lease, `d699bfe` and its base, current with every source file |
| `cowfs` | Oct 4 17:43 | this lease, `d699bfe` |
| `cowfs-treehouse` | Oct 4 18:40 | this lease, `d699bfe` |

`lsof` revision 4.91, `treehouse` v3.1.2.
`cowfs mount-info` on the private daemon: `adapter: nfs`, `mounted: true`.
The mount table line:
`localhost:/cowfs-ce252af9367b61e6982619328afa30d4 on <lease>/bench/out/ready-20/att4/mnt (nfs, nodev, nosuid)`.

A real `treehouse get --lease` then created a real slot inside the mount:
`att4/mnt/base/.treehouse/cowfs-7c1bf8/1/cowfs`.

### The holder is a real process with its working directory outside the slot

pid 49430, `/bin/sh -c "cd <slot>; exec 9<<slot>/held.txt; trap ...; cd /; while :; do sleep 0.2; done"`.
`lsof -a -d cwd` reports its working directory as `/`.
The file is 18 bytes, sha256 `9c072eb692d854e56d5838f0fae0a00f285b3f085c58fb3010a84e0b20efe9fc`.

### The control API names it, through the real adapter

```
cowfs --socket <sock> --json ps base/.treehouse/cowfs-7c1bf8/1/cowfs
{"processes":[
  {"command":"bash","pid":49430,"holds":[{"kind":"fd","path":"<mnt>/base/.treehouse/cowfs-7c1bf8/1/cowfs/held.txt"}]},
  {"command":"sleep","pid":49439,"holds":[{"kind":"fd","path":"<mnt>/base/.treehouse/cowfs-7c1bf8/1/cowfs/held.txt"}]}]}
```

Two pids, both correct: the fixture's `sh` and the `sleep` it exec'd, which inherited the descriptor.
There is no `cwd` hold anywhere in the output, which is the whole case: a detector that only looked
at working directories would have found nothing here.
This closes item 9 of the `docs/v1-treehouse.md` list, "ps reporting real `fd` and `lock` holds from
a real adapter".

Containment, same run: `cowfs ps ../escape` answers
`invalid_params: invalid mount-relative name "../escape": must not contain a '..' component`.

### The return refuses, before treehouse is asked to release anything

`cowfs-treehouse return --slot <slot> --root <pool>`, no `--force`:

- exit **5**.
- stderr names both pids and the file, and says why:
  `<slot> holds pid 49430 (bash): fd <slot>/held.txt, pid 49439 (sleep): fd <slot>/held.txt that
  treehouse cannot see, and --force was not given; returning it now would unlink a file a live
  process still has open`.
- `treehouse status` still reports the slot `leased`, with the same `lease_id`
  `a7e3e319c0be6de1743b08605e26fefc` as before the call.
- The held file is still there and the holder is still alive. Nothing was signalled and nothing
  changed.

### The unlink a reset performs, while the file is held

`rm <slot>/held.txt` on the mount: exit **0**, and the slot then contains
`.nfs.200516cf.c235`.
The silly-rename is 18 bytes, sha256 `9c072eb692d854e56d5838f0fae0a00f285b3f085c58fb3010a84e0b20efe9fc`,
identical to the source.

**Native control, through the real descriptor.** After the name was gone, the holder was sent
`SIGUSR1` and read its own open descriptor through `cat <&9`:
18 bytes, sha256 `9c072eb692d854e56d5838f0fae0a00f285b3f085c58fb3010a84e0b20efe9fc`.
Same hash before the unlink, through the silly-renamed path, and through the descriptor with the name
gone. The data is intact; only the name moved.

A second return, with the dirt present, exits **5** again and names the silly-renamed file as the
held path.

### Matched native baseline

The same held file, the same descriptor, the same unlink, on an ordinary directory off the mount:
unlink exit **0**, the directory is empty afterwards, and there are **0** `.nfs` entries.
So the silly-rename, and therefore the dirty slot, is a property of the network mount, not of the
unlink. That is the difference between the two columns of the spike 5 table, measured rather than
assumed.

### Does the `.nfs` file go at kill time or at unmount time?

Issue #20 asked and spike 5 did not answer it.
Answered here: **at descriptor-release time.**

Two seconds after SIGKILL of the verified holder, with the mount still up and the daemon still
running, the slot listed no `.nfs` entry and the silly-renamed name was gone. The daemon was stopped
afterwards and its mount disappeared; nothing about the cleanup depended on that.

## Earlier attempts, and what they cost

| attempt | result | cause |
| --- | --- | --- |
| `att1` | exit 1 | macOS has no `setsid`; the daemon never started |
| `att2` | exit 1 | a relative `--mount` did not match the mount table line being polled. Left a live daemon, stopped by verified pid |
| `att3` | exit 75, twice | the shared lock was held: bounded 600 s foreground waits, seven peer lanes queued on the same lock file |
| `att3` | exit 1 | the socket path exceeded `sun_path`; moved to the pre-approved external temp directory |
| `att4` | **exit 0** | the sample above |

`att3` also carries the old-fail/new-pass evidence, run in the same lock hold.
Total owned artifacts under `bench/out/ready-20`: 5.9 MB, against an 8 GiB allowance.