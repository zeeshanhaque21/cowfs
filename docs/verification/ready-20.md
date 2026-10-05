# Ready task #20: open-FD holders and `.nfs` dirt on a slot

Task: issue [#20](https://github.com/zeeshanhaque21/cowfs/issues/20).
Lease: `.treehouse-ready-wave/.treehouse/cowfs-7c1bf8/2/cowfs`, branch `fix/open-fd-holders-20`.
Base: `46b0f269d5bef4a2c204c25f5b3015da601d3beb`.
PR: [#112](https://github.com/zeeshanhaque21/cowfs/pull/112).
Code head: `02d46f3a71f16bffe50433d435a7401030d2e089`, tree `062c0b1b7335284d8dd37bfb394dcdc46e0a57e1`.
CI for that head: run `37269390256`.
The only commit after the code head is this document.

This document covers two waves.
The first implemented the feature.
The second answered an independent review that blocked the merge, reproduced the blocking defect
before touching it, and fixed it.
Where the two waves disagree, the second one is what the code does now, and the review's findings
are recorded rather than smoothed over.

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

## The review, and the one thing it blocked

Review: `docs/reviews/open-descriptor20-final.md`, sha256
`7db8c0229e93845925ee95b07b57c54e7e1fe581402d37ab32fc4c4b0433be43`, read at head `20d0e645`.
Verdict **BLOCK**, on one fail-open defect, with four further findings.
Nothing else in that review blocked the merge.

### The blocking defect, reproduced before it was fixed

`lsof +D <dir>` is a recursive walk of the directory.
Handed a directory the scanning user cannot read, it gives up.
It exits 1 with no output and no diagnostic, which is byte-identical to its own "matched nothing".
The code trusted exit 1 as an answer, so a live holder behind an unreadable subdirectory read back as
a clean slot, and the return went on to ask treehouse to release it.

Reproduced on this worker's own private slot inside this worker's own private mount, as uid 501
(`id -u` recorded in the log, so a mode 000 directory is a real denial here and not a no-op):

- a real `treehouse get --lease` slot in the mount,
- a real held file, inside a subdirectory,
- a real single-process holder with an open descriptor on it and its working directory at `/`,
- that subdirectory then `chmod 000`, which is ordinary for a build artifact,
- and the `treehouse` binary the companion was given replaced by a **shim that records its argv and
  refuses**, forwarding only `status`, which the companion needs to read a lease id at all.

Measured at the reviewed head `20d0e645`, tree `e655bb13`, binaries built from a `git archive` of that
commit into an isolated target directory and bound by sha256 (manifest
`bench/out/ready-20/repair/old-manifest.txt`):

| step | result |
| --- | --- |
| everything readable, `ps <slot>` | 2 holders, `kind: fd`, no `cwd` |
| `chmod 000 <slot>/locked`, owner uid 501 | mode `d---------`, unreadable by this user |
| `lsof -nP -w -F pcfn +D <slot>` | **rc=1, stdout 0 bytes, stderr 0 bytes** |
| `ps <slot>` | **`{"processes":[]}`**, rc 0 |
| `cowfs-treehouse return` | **exit 0**, empty stderr |
| the shim's recorded argv | `status --json --root <pool>` then **`return <slot> --if-lease-id 391e7b04bb05651339bfad2ea01e4f7b --root <pool>`** |
| the holder | still alive |

So the destructive call really was reached, and it was reached on a slot with a live open descriptor
in it.
The same run's `return.json` even claimed the work: `"holder_scan":"scanned base/.treehouse/cowfs-7c1bf8/1/cowfs"`,
`"holders":[]`, `"refused_busy":false`.

**Nothing destructive actually happened.**
The shim records and refuses, so the proof is the recorded argv and nothing else.
No `treehouse return`, `prune` or `destroy` was run against any of the 32 held leases, any shared
slot, or any coordinator pool.

One shape does not reproduce, and is worth recording so nobody re-chases it.
With the held file **outside** the unreadable subdirectory, `lsof +D` returns rc 1 with 381 bytes of
stdout and the holder is still found: lsof reports what it walked before it gave up.
The defect needs the holder to be behind the directory it cannot enter.
That is the ordinary case, not an exotic one.

### The same fixture after the fix

Same script, same fixture, same shim, at head `02d46f3`, attempt `att-final2`:

| step | result |
| --- | --- |
| everything readable, `ps <slot>` | holder found, `kind: fd` |
| `chmod 000 <slot>/locked`, owner uid 501 | mode `d---------`, unreadable by this user |
| `ps <slot>` | **holder found**, `kind: fd`, path inside the unreadable directory |
| `cowfs-treehouse return` | **exit 5**, stderr names the pid, the file, and `--force` |
| the shim's recorded argv | `status` only. **No `return`.** |
| the holder | still alive |
| holder killed, directory still `d---------`, `ps <slot>` | `{"processes":[]}`, and it is a **proven-complete empty**, not a refusal |

The last row matters as much as the third.
An empty answer has to be a real answer, and it is only real because completeness no longer depends
on the mount being readable.

Raw logs: `bench/out/ready-20/repair/att-old/` and `bench/out/ready-20/repair/att-final2/`.
Harnesses: `repair/build-old.sh`, `repair/probe.sh`.
Summary: `docs/verification/evidence/open-descriptor20-repair.md`.

## What changed

Wave one, commit `d699bfe51a4509addd7f6398ec5f2467c63d22e4`.
Wave two, commits `3226b76`, `80c342c`, `3fe97e4`, `eece219`, `1aa6f6b`, `02d46f3`.
All pushed over authenticated HTTPS.

| File | Change |
| --- | --- |
| `crates/cowfs-treehouse/src/mode_a.rs` | Scan the slot's own mount-relative directory, not only a snapshot. Refuse a hold treehouse cannot see. Refuse an unsignalable holder. Report what the scan established. |
| `crates/cowfs-treehouse/src/cli.rs` | Mode (a) connects to a named `--socket`, which is what lets a slot on a mount be scanned at all. |
| `crates/cowfs-daemon/src/holders.rs` | `Scan` separates "nobody is holding this" from "this platform could not be asked". Nothing walks the mount. An answer that did not arrive whole is refused. The Linux `/proc/locks` parser reads the field and the radix the kernel actually writes. |
| `crates/cowfs-daemon/src/handler.rs` | `ps` answers `unsupported` when the platform cannot scan. Containment in two halves: a no-syscall lexical gate, then the resolved check inside the scan's deadline. |
| `crates/cowfs-ctl/src/validate.rs`, `server.rs` | `ps` takes a mount-relative directory name; the rule keeps it inside the mount and off the mount root itself. |
| `crates/cowfs-treehouse/src/th.rs` | `PoolEntry.status` is recorded as the string treehouse prints. **The claimed `leased()` fix from wave one is withdrawn**: nothing called it, so no safety property depended on it. |

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
| `lsof` outruns its deadline, including the resolve and both drains | `unsupported`, caller blocks |
| `lsof` leaves a pipe open after it exits | `unsupported`, caller blocks |
| `lsof` output has a tag, a pid or a record this build does not understand | `unsupported`, caller blocks |
| `/proc` or `/proc/locks` unreadable on Linux, or a row that does not parse | `unsupported`, caller blocks |
| the prefix cannot be resolved | `unavailable`, caller blocks |
| the prefix resolves outside the mount | `unavailable`, caller blocks |

`lsof` exiting 1 is its "matched no file" and stays an answer, because with the process-table form
below it means the whole table was read and nothing was below the prefix.
That is the whole difference between wave one and wave two: in wave one exit 1 also meant "I gave up
part way through a walk".

## Completeness: what makes an empty answer real

The review's instruction was not to check directory mode bits, not to check stderr, and not to add
quiet flags to `lsof`.
Those all describe one failure while leaving the others open.
What is needed instead is an answer whose completeness can be argued.

**The enumeration is over the process table, not over the filesystem.**
`lsof -nP -w -F pcfn` is asked for every open file on the machine, with no path argument and no
`+D`, and the paths are filtered here against the prefix.
Two things follow.
A directory's permissions cannot hide a process holding a file in it, because lsof reads each
descriptor's path from the kernel and never enters the directory.
And a scan no longer scales with the size of a worktree, which on a network mount is the operation
that can wedge.
Measured on this machine, the whole-table listing is 0.38 s to 0.49 s for about 57,000 lines and
2.1 MB, against 0.3 s for `lsof +D` on an empty slot and 0.86 s for `+D` on a 100,000-file one.

**The answer arrived whole, or it is not an answer.**
All of these are inside one deadline, on one worker thread, so the deadline covers the resolve as
well as the child:

- the prefix is resolved with `canonicalize`, which is the syscall a stale mount wedges,
- the child exits,
- **both** pipes reach end of file. A grandchild that inherited the descriptor keeps the pipe open
  after the child is gone. Wave one did `recv_timeout(...).unwrap_or_default()`, which turned that
  case into an empty string, and an empty string into a clean slot. It is an error now, and there is
  a test that produces it with a real inherited pipe.
- the parse understands every line: an unknown field tag, a record with no usable pid, a name with
  no descriptor, a field-less line are all refused.
  A descriptor with no name is **not** refused, because an anonymous mapping has one and refusing
  those would refuse every complete answer.
- the exit status is 0 or 1, and any other status carries its own stderr text into the refusal.

**What is deliberately not claimed.**
Completeness is argued over the process table, not over the whole kernel.
A process running as another user can have its descriptors hidden from `lsof` in this single-user
model; that is a scope limit of the platform, not an empty answer.
On Linux the equivalent gap is a `/proc` mount that hides another user's `fd` directory.
And a thread stuck in a syscall against a dead mount is still stuck when the deadline returns: what
the deadline bounds is what the caller waits for, not what the kernel does.
That is the exposure commits `90c9a8f` and `265fc3f` recorded, and the alternative would be never
answering at all.

**`ps .` is refused.**
A legal request that scanned the whole mount was a scan of the wrong size with no bound but the
timeout, after which every return on the machine would block.
`validate_mount_relative` now refuses a leading `.` component and a `..` or empty component in any
position, and the daemon repeats the check lexically before touching any path.

### The Linux `/proc/locks` parser

Wave one read the device field from the fourth whitespace token and parsed it in decimal.
The fourth token is the word `ADVISORY`.
So `HoldKind::Lock` was **never** reported on Linux at all, and an empty set is indistinguishable
from reporting no locks.

The kernel writes the row as
`id: CLASS ADVISORY MODE PID MAJ:MIN:INO START END`, optionally followed by `-> PID` for a lock
waiting on another, and formats the device `%02x:%02x:%lu`.
So: the device is the first field shaped like a device rather than a fixed index, major and minor
are hex, and the inode is decimal.
A row that does not parse now refuses the whole table instead of contributing no locks.

Real rows captured from moonscape, `6.12.109+rpt-rpi-2712`, that the parser now has to accept:

```
1: FLOCK  ADVISORY  WRITE 1437365 08:02:768494 0 EOF
2: FLOCK  ADVISORY  READ 1437365 08:02:5629643 0 EOF
3: OFDLCK ADVISORY  WRITE -1 08:02:142895 0 4611686018427388799
4: OFDLCK ADVISORY  WRITE -1 08:02:142895 4611686018427388928 EOF
```

## Tests

`cargo test --locked -p cowfs-treehouse -p cowfs-ctl -p cowfs-daemon -j2`, through the wave's
resource lock, exit 0.

| crate | suites | passed | failed | ignored |
| --- | --- | --- | --- | --- |
| `cowfs-ctl` | lib 11, admission 14, flood 1, regress 19, server 31, wire 20 | 96 | 0 | 0 |
| `cowfs-daemon` | lib 59, main 0, end_to_end 0 | 59 | 0 | 5 |
| `cowfs-treehouse` | lib 60, issue20 5, regressions 14, sandbox_treehouse 15, spawned_server 6, stub_server 22 | 122 | 0 | 0 |
| total | | **277** | **0** | 5 |

The 5 ignored are `cowfs-daemon` `end_to_end` cases that are not for this platform; they were already
ignored at the base commit.

`cargo clippy --workspace --all-targets -j2 -- -D warnings` exit 0, the command CI runs.
`cargo fmt --all --check` exit 0.

Both are quoted with their real exit status and not through a pipeline, after a run where a
`| tail -2` swallowed a clippy failure and the chain carried on anyway.

### The five new integration cases

`crates/cowfs-treehouse/tests/issue20.rs`, all five passing, each with a **real** detached child that
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
5. `force_refuses_a_holder_it_may_not_signal_and_leaves_the_descriptor_open`
   The holder is the test process itself, which is an ancestor of the companion it starts.
   `--force` does not become a licence to signal it: exit 5, "may not signal", the pid named, and
   the descriptor this process still holds is read afterwards as the native control that nothing was
   signalled.

In `crates/cowfs-daemon/src/holders.rs`, the discriminating one is
`a_holder_that_chdird_out_is_still_reported_and_only_by_its_descriptor`: it asserts the found process
has an `fd` hold and **no** `cwd` hold, so a detector that only looked at working directories would
fail it.
Its companions are
`a_holder_behind_an_unreadable_directory_is_still_reported` (the review's blocking case, and it says
so plainly when the machine lets the closed directory through anyway instead of pretending to have
proved something),
`an_empty_answer_is_still_an_answer_with_an_unreadable_directory_present`,
`a_prefix_that_resolves_outside_the_mount_is_refused`,
`a_name_that_could_leave_the_mount_is_refused_without_a_syscall`,
`a_prefix_that_cannot_be_resolved_is_unavailable_rather_than_clear`,
`output_that_is_not_a_complete_answer_is_refused`,
`an_inherited_pipe_is_not_a_finished_answer`,
`a_command_that_cannot_be_run_or_never_finishes_is_unknown_not_empty`,
`an_unexpected_exit_status_is_a_failure_with_its_reason`,
`a_complete_listing_with_nothing_below_the_prefix_is_empty_not_an_error`,
and on Linux `a_real_flock_is_reported_as_a_lock_hold`,
`proc_locks_rows_are_read_from_the_field_the_kernel_writes`,
`a_lock_row_that_is_not_understood_is_not_silently_zero`.

## The one thing a scoped macOS check cannot see

CI was red twice on the first push, and both were mine.
`cargo fmt --all --check` wanted rustfmt's line breaking, fixed in `38ac3f1`.
Then ubuntu failed clippy with `error[E0308]: mismatched types` at
`crates/cowfs-daemon/src/holders.rs:123`: `locked()` had been changed to return `Result` and the
`/proc` module returned the bare set.
**macOS never compiles that module**, so the scoped macOS check, the scoped macOS clippy and the
workspace macOS clippy were all green and CI was the first thing to look at it.
Fixed in `c82bb35`.

The repair wave hit the same wall twice more, and the answer was the same each time:

- `80c342c`: a leftover `PathBuf` import and a parser helper that was private to its module, both
  inside the Linux-only code. macOS compiled and passed.
- `3fe97e4`: the real-flock test never created the fifo it told its holder to block on, so the
  holder exited at once, released the lock, and the scan correctly found nothing.
  Only the Linux run could have found that, and it did.
- `eece219`: util-linux `flock FILE cmd` **forks**, and the child inherits the open file
  description, so killing the wrapper left the lock held by an orphan and the test timed out on its
  own cleanup. The holder is one `python3` process now, and an orphan `sleep 600` left on the box by
  the failed run was killed by verified pid.

Each Linux-only change was followed by a moonscape run before moving on, not at the end.

Then the repair wave hit it once more, and this time it reached CI.
`Instant` was imported at module scope for the `lsof` deadline, so on Linux, where the `/proc`
module reads no clock, it was an unused import and ubuntu's `-D warnings` failed the job while
macOS stayed green. Fixed in `02d46f3` by importing it only where a clock is read.
The permanent answer is the remote run itself: it now uses `RUSTFLAGS=-Dwarnings`, which is the
same gate CI applies, so a Linux-only warning is caught on the box instead of in CI.

The tree is copied to moonscape under
`/home/moonscape/cowfs-ready-wave/task-20/repair-<head>/src`, with `crates/cowfs-daemon/src/holders.rs`
md5 `f2f3030f74929f024f756ce4f7d12f96` recorded on the box before anything of ours runs.
Cross-compiling from the Mac to `x86_64-unknown-linux-gnu` was tried first and cannot work here:
`zstd-sys` has a build script that needs `x86_64-linux-gnu-gcc`, and installing a cross toolchain is
not permitted.
`cargo clippy` on the box is not a usable signal: its clippy is 1.95.0 and reports a pre-existing
`collapsible_match` in `crates/cowfs-meta/src/tx.rs`, a file this task never touched.
So `cargo check` and `cargo test` run there, and clippy runs on the Mac and in CI.

## Cross-lane surfaces touched

Reported, not silently taken.

- `crates/cowfs-ctl/src/server.rs`, the `Request::Ps` validation arm only.
  The arm validates `p.snapshot`, which forbids `/` and a leading dot, so it rejected a mode (a)
  mount-relative slot directory outright.
  No shutdown behaviour was touched.
- `crates/cowfs-daemon/src/handler.rs`, the `holders` function and one new free function beside it.
  It is the same file that carries the `import` implementation owned by the #97 worker; the import
  path is untouched.
- `crates/cowfs-daemon/src/holders.rs` `scan()` is still best-effort for the two `exports.rs` callers
  (`mount_snapshot`, `unmount_snapshot`), which keep exactly today's behaviour and now log the
  reason to stderr. Those two gates are still fail-open on an unavailable platform.
  They are not destructive, and they are not mine: report, do not widen.

## Linux, on a real Linux

moonscape, `192.168.68.119`, Debian aarch64, kernel `6.12.109+rpt-rpi-2712`.
Run under the wave's remote lock, one immutable stage per head, nothing installed, no runner touched.

`RUSTFLAGS=-Dwarnings cargo test --locked -p cowfs-daemon --lib`: exit 0, **55 passed, 0 failed**.
The `-D warnings` is the point: clippy on the box is unusable, and without promoting rustc warnings to
errors that run cannot see what CI's ubuntu job sees, which is exactly how a Linux-only unused import
reached CI in the first place.

The case that matters for this repair:

```
test holders::tests::a_real_flock_is_reported_as_a_lock_hold ... ok
```

It takes a real `flock` on a real file, held by a real other process, waits for the inode to appear
in `/proc/locks`, then asserts the scan reports `HoldKind::Lock` on exactly that path, then kills the
holder, waits for the row to leave `/proc/locks`, and asserts the lock is no longer reported.
That pair is what distinguishes a parsed lock from an empty table that happens to look the same.

Also on Linux and passing:
`proc_locks_rows_are_read_from_the_field_the_kernel_writes`, over captured real rows including hex
digits that are not decimal, a waiting lock's `-> PID` and an OFD lock's pid `-1`;
`a_holder_behind_an_unreadable_directory_is_still_reported` and
`a_holder_that_chdird_out_is_still_reported_and_only_by_its_descriptor`, both driven entirely by
`/proc`;
`a_prefix_that_cannot_be_resolved_is_unavailable_rather_than_clear`;
and the lexical containment gate.

The Linux scan is measured against `/proc` on real hardware.
It is **not** measured against a real FUSE mount, and nothing here claims it is.

## Remaining acceptance

1. **FUSE.** The `fd`/`lock` scan has been run against the macOS NFS loopback, not against a real
   FUSE mount, where there is no silly-rename and a held descriptor is a plain open file.
   Issue #20's work item "re-check on Linux (`gopsutil` reads `/proc`) and on FUSE" is half done:
   the `/proc` half is measured above, the FUSE half is not.
   No unprivileged capability for a FUSE mount was set up for this wave.
2. **`exports.rs` stays fail-open.** `mount_snapshot` and `unmount_snapshot` still treat an
   unavailable scan as an empty one. Deliberate, not mine, not destructive, still on the list.
3. **Independent review of the repair**, before merge.
4. **The upstream proposal** is drafted at `docs/upstream-treehouse-proposal.md` section 3 and has
   **not** been sent. Section 3 gained a paragraph: an unanswered scan must not read as an empty one,
   which is the detail cowfs learned implementing it.

## Safety trail

- Shared daemon PID 15263, its store `~/.cowfs/store`, its mount `~/.cowfs/mnt` and its socket were
  never signalled, reset or used as state.
- Every `treehouse` invocation in the automated tests goes through the sandbox shim in
  `crates/cowfs-treehouse/tests/common/mod.rs`, which refuses any call without an explicit `--root`
  inside the sandbox and refuses any absolute path argument outside it.
  The mount runs use their own sandbox `HOME` and their own pool inside their own mount.
- The repair probe goes further: the companion is handed a **shim** that records its argv, forwards
  only `status`, and refuses everything else.
  The destructive-call proof is a recorded argv, never an executed release.
- `chmod 000` was applied only to this worker's own fixture, inside this worker's own mount, as uid
  501, and the run records `id -u` and verifies the directory really became unreadable before
  treating the denial as evidence.
  The unit test that does the same says "skipping" when the machine lets the closed directory through
  anyway, which is what a root runner would do, rather than passing as if it had proved something.
- Binaries are bound to their source: the old build is a `git archive` of the reviewed commit in an
  isolated target directory with a sha256 manifest and a staleness check, and the new run records the
  head, the tree and each binary's sha256 before it starts.
- Every signal is by pid after that pid's argv was verified, never by pattern, and never to a group.
- No `treehouse return`, `prune` or `destroy` was run against a leased slot in any coordinator pool.
- No daemon, mount or socket of this worker's was left running: checked after the run, and the
  external socket directory is removed by the harness on every exit path.

## Old fail, new pass

### The integration suite

The new test file is kept in both runs; only the product files move, reverted to the base commit and
then restored.
Script `bench/out/ready-20/old-fail-new-pass.sh`, which now takes the base commit as an argument.
It used to use `HEAD~1`, which after a repair commit is a commit that already contains the fix, and a
run against that "base" reports that the tests prove nothing.

| code | `cargo test -p cowfs-treehouse --test issue20` |
| --- | --- |
| base `46b0f26` | exit 101, **0 passed, 5 failed** |
| head `02d46f3` | exit 0, **5 passed** |

All five fail at the base commit.

The `holders.rs` unit tests cannot be shown failing this way, and the reason is worth stating rather
than hiding: they call `scan_mounted`, `scan_checked`, `Scan::Unavailable` and `HoldKind::Lock`
reporting, none of which exist at the base commit, so the base cannot compile them at all.
Their old-fail/new-pass statement is the live probe in
[The review, and the one thing it blocked](#the-review-and-the-one-thing-it-blocked): old binaries
report `{"processes":[]}` on a slot with a live holder and reach `treehouse return`; new binaries
report the holder and never get there.

### Live mount: the real `.nfs` silly-rename and the real holder scan

`bench/out/ready-20/run-mount-sample.sh`, attempt `att7`, exit 0, at head `02d46f3`, run through the
wave's resource lock.
Raw evidence in `bench/out/ready-20/repair/att7/log/`.
Earlier attempts `att4` at `20d0e645` and `att6` at `1aa6f6b` produced the same cycle with different
pids and silly-rename names and are kept.

Everything was private to the attempt: store `att7/store`, mount `att7/mnt`, socket in the
pre-approved external temp directory, sandbox `HOME` `att7/home`, pool `att7/mnt/base`.
The socket is the one artifact that cannot live in the lease: `sockaddr_un.sun_path` is 104 bytes on
macOS and this lease's path is already 95, so the socket went out to
`/private/var/folders/np/j2zqjrgd4sj40jx6qqxyr3qh0000gn/T/opencode/ready-20-att7`, 91 bytes, and was
removed at the end.
Store, mount and logs all stayed in the lease.

Binaries, debug profile, from this lease's target directory at head `02d46f3`, sha256:

| binary | sha256 |
| --- | --- |
| `cowfs-daemon` | `1195e81eff9e93972f705e63a7082aa4b229adced416bb8dcdb3f2fc82a7ebb8` |
| `cowfs` | `50ca196d7c93bcdb2b2e0e6959336df9a66a89ae8907fb25f6ea3f066535f10a` |
| `cowfs-treehouse` | `7ac1ce42e9480c0384816c40bb1f9f38090a95251859b85ddb957b1af8a2617c` |

`cowfs mount-info` on the private daemon: `adapter: nfs`, `mounted: true`.
The mount table line:
`localhost:/cowfs-<id> on <lease>/bench/out/ready-20/repair/att7/mnt (nfs, nodev, nosuid)`.

A real `treehouse get --lease` then created a real slot inside the mount:
`att7/mnt/base/.treehouse/cowfs-7c1bf8/1/cowfs`.

### The holder is a real process with its working directory outside the slot

pid 84676, `/bin/sh -c "cd <slot>; exec 9<<slot>/held.txt; trap ...; cd /; while :; do sleep 0.2; done"`.
`lsof -a -d cwd` reports its working directory as `/`.
The file is 18 bytes, sha256 `9c072eb692d854e56d5838f0fae0a00f285b3f085c58fb3010a84e0b20efe9fc`.

### The control API names it, through the real adapter

```
cowfs --socket <sock> --json ps base/.treehouse/cowfs-7c1bf8/1/cowfs
{"processes":[
  {"command":"bash","pid":84676,"holds":[{"kind":"fd","path":"<mnt>/.../held.txt"}]},
  {"command":"sleep","pid":84683,"holds":[{"kind":"fd","path":"<mnt>/.../held.txt"}]}]}
```

Two pids, both correct: the fixture's `sh` and the `sleep` it exec'd, which inherited the descriptor.
There is no `cwd` hold anywhere in the output, which is the whole case: a detector that only looked
at working directories would have found nothing here.
This closes item 9 of the `docs/v1-treehouse.md` list, "ps reporting real `fd` and `lock` holds from
a real adapter".

Containment, same run: `cowfs ps ../escape` answers
`invalid_params: invalid mount-relative name "../escape": must not contain a '..' or empty component`.

### The return refuses, before treehouse is asked to release anything

`cowfs-treehouse return --slot <slot> --root <pool>`, no `--force`:

- exit **5**.
- stderr names both pids and the file, and says why:
  `<slot> holds pid 84676 (bash): fd <slot>/held.txt, pid 84683 (sleep): fd <slot>/held.txt that
  treehouse cannot see, and --force was not given; returning it now would unlink a file a live
  process still has open`.
- `treehouse status` still reports the slot `leased`, with the same `lease_id` as before the call.
- The held file is still there and the holder is still alive. Nothing was signalled and nothing
  changed.

### The unlink a reset performs, while the file is held

`rm <slot>/held.txt` on the mount: exit **0**, and the slot then contains
`.nfs.20051706.4bf4`.
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
running, the slot listed **0** `.nfs` entries and the silly-renamed name was gone.
The daemon was stopped afterwards and its mount disappeared; nothing about the cleanup depended on
that.

## Earlier attempts, and what they cost

| attempt | result | cause |
| --- | --- | --- |
| `att1` | exit 1 | macOS has no `setsid`; the daemon never started |
| `att2` | exit 1 | a relative `--mount` did not match the mount table line being polled. Left a live daemon, stopped by verified pid |
| `att3` | exit 75, twice | the shared lock was held: bounded 600 s foreground waits, seven peer lanes queued on the same lock file |
| `att3` | exit 1 | the socket path exceeded `sun_path`; moved to the pre-approved external temp directory |
| `att4` | exit 0 | the first successful `.nfs` sample, at `20d0e645` |
| repair `att-old` | exit 0 | the review's blocking defect reproduced: false clean, `treehouse return` reached |
| repair `att-final`, `att-final2` | exit 0 | the same fixture after the fix: holder found, return refused |
| repair `att6`, `att7` | exit 0 | the `.nfs` sample re-run at each repair head |

The repair wave also lost time to three of its own mistakes, all caught by re-reading rather than by
a test: a `set -e` that aborted the harness on the very exit code it was trying to record, a `say`
function writing a remote path from the local shell, and a fixture holder that forked so that killing
it left an orphan holding the descriptor.

Total owned artifacts under `bench/out/ready-20`: about 1.0 GB, against an 8 GiB allowance, almost
all of it the isolated old-code target directory.
On moonscape, four superseded stage directories were removed after the final head passed; the final
stage and its log are kept for review.
