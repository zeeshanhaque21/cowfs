# Final delivery review: PR #112 at c2e8274, after the repair for the blocked fail-open

Independent, exact-head, no production code edited, no lease returned.

| | |
| --- | --- |
| PR | [#112](https://github.com/zeeshanhaque21/cowfs/pull/112), `OPEN`, not draft |
| Head reviewed | `c2e82746f80b5d7bc07e896c9f6587d821b179d7`, tree `52a93948d14aad5476820a6a726f65d07dcb7e99` |
| Repair commit | `02d46f3a71f16bffe50433d435a7401030d2e089`, an ancestor of the head; the only commit after it is the docs commit `c2e8274` |
| Supersedes | my `docs/reviews/open-descriptor20-final.md`, sha256 `7db8c0229e93845925ee95b07b57c54e7e1fe581402d37ab32fc4c4b0433be43`, read at `20d0e645`, verdict **BLOCK** |
| PR base ref | `03bbec85626c26a85ee4f5791d3a413fe47725bc`, an ancestor of current main |
| Current main | `252b93e981fe11b3ff8c811d25678f1c23caab75` |
| Diff | 14 files, +2546 / -135, 13 commits |
| Closing refs | **0**. No issue is auto-closed, neither #20 nor any other |
| Verdict | **The blocking fail-open is closed.** Nothing found in this review blocks the merge |

Evidence: `bench/out/open-descriptor20-final-critic/**` in this reviewer's lease, with a
`.sha256-manifest` per extracted tree. Prior reports and artifacts were treated as immutable.

## 1. The blocking defect is closed, measured old against new

The prior review blocked on this: `lsof +D <dir>` cannot enter a directory the scanning user cannot
read, so it exits 1 with **0 bytes of stdout and 0 of stderr**, byte-identical to its own "matched
nothing". `run_bounded` mapped exit 1 to `Ok("")`, `parse_lsof("")` returned `[]`, and a live holder
read back as a clean slot, after which the return proceeded into the reset the PR exists to prevent.

C2 removes `+D` and the path argument entirely (`holders.rs:288-292`: `lsof -nP -w -F pcfn`, no path)
and filters lexically here, so no directory walk happens at all and a permission cannot hide a
descriptor.

Recreated in the original shape on a private Core NFS loopback mount, one fully self-contained
attempt per side, real holder with cwd outside the slot, held file behind a `chmod 000` directory,
verified by `lsof -d 9` that the descriptor is still open and still resolves to that file:

| | old `20d0e645` | new `c2e82746` |
| --- | --- | --- |
| control, before `chmod` | holder visible | holder visible |
| `ps <slot>` with the subdir unreadable | `{"processes":[]}` | names the holder, `kind: fd` |
| direct `lsof +D <slot>` | rc 1, 0 bytes out, 0 bytes err | not used any more |
| direct `lsof -nP -w -F pcfn` | not used | rc 0, 2176933 bytes, holder's record present |
| `cowfs-treehouse return` | exit **1**, delegated to `treehouse`, which ran `git clean -fd` | exit **5**, refused |
| treehouse `lease_id` across the call | unchanged | unchanged, `9251265308b66a648d63fb99391cc00b` before and after |
| holder alive afterwards | yes | yes |
| after a verified-identity release | empty | empty, and this is the **empty-positive** control |

The old side's own failure mode is visible in its stderr: `treehouse return` ran, then
`git clean -fd: warning: could not open directory 'locked/': Permission denied`. That is the reset
landing under a live writer, which is the whole defect.

The empty-positive control matters: after the holder was closed by verified identity, the same
completed scan on the same mount returned an empty holder list. So the new code's refusal is not a
scan that always says "busy", it is a scan that distinguishes.

Source bindings, from the exact trees rather than from a summary: binaries built from `git archive`
of each head, `cowfs-daemon` sha256 `13f80c6006219ee3…` (old) and `5b3eadf2fb8c095d…` (new).

### A correction to my own method, and why it matters

My first attempt at this comparison restarted the daemon between the two sides, reusing one holder.
That reported the new code as failing. It was my probe that was wrong: a holder's descriptor belongs
to the mount instance it was opened through, so restarting the daemon tears that instance down, the
descriptor's vnode becomes revoked, and `lsof` reports `n(revoked)` for fd 9. The evidence is in the
raw log: `log/blocking-case/c2-lsof-table.out` line 51069 onward shows `f9 / n(revoked)` for the
holder, and the process-table listing therefore has nothing to filter.

Redone with one mount instance per side, the new code finds the holder with the same `chmod 000`
applied. I have recorded this because a reviewer who reused one fixture across two daemons would have
reported a BLOCK that does not exist, and a reviewer who only ran the unit test would have called the
matter closed without ever mounting anything.

## 2. The other prior findings

**Linux `/proc/locks` parser: fixed, and independently confirmed on real Linux.** The old parser read
the fourth whitespace token, which is the word `ADVISORY`, and parsed it in decimal. The new one
finds the first field shaped like a device, major and minor as hex, inode as decimal, and a row it
cannot parse refuses the whole table instead of contributing no locks.

Verified on `moonscape` (aarch64, kernel `6.12.109+rpt-rpi-2712`) against a real `flock` taken by a
real process, with the staged `holders.rs` md5 `f2f3030f74929f024f756ce4f7d12f96`, which is byte-for-byte
the head's file:

```
holder pid 1551754 st_dev triple (decimal) (8, 2, 383223)
MY /proc/locks row: '84: FLOCK  ADVISORY  WRITE 1551754 08:02:383223 0 EOF'
  device field at index 5: 08:02:383223   index 2: ADVISORY
  hex major/minor, decimal inode -> (8, 2, 383223)
  matches st_dev triple: True
```

`RUSTFLAGS=-Dwarnings cargo test --locked -p cowfs-daemon --lib` on that box: **55 passed, 0 failed,
exit 0**, including `a_real_flock_is_reported_as_a_lock_hold`, which is a pair: it waits for its own
inode to appear in `/proc/locks`, asserts a `Lock` hold on exactly that path, kills the holder, waits
for the row to leave the table, and asserts the lock is no longer reported. Without that second half
an empty table would pass. CI ran the same test on ubuntu and it passed there too, with zero
`skipping:` lines, so it was not a skip.

**Inherited-pipe false clean: fixed.** `collect()` now waits for both pipes to reach end of file
inside one absolute deadline and errors if either is still open after the child exited
(`holders.rs:439-465`). The macOS CI job ran `an_inherited_pipe_is_not_a_finished_answer ... ok`.

**Partial output: fixed.** `parse_lsof` returns `Result` and refuses an unknown tag, a record with no
usable pid, pid 0, a name with no descriptor, a descriptor before any pid, and a line with no field
tag at all. macOS CI ran `output_that_is_not_a_complete_answer_is_refused ... ok`.

**Absolute deadline over resolver, child and both pipes: fixed, with the residual disclosed.** The
whole scan now runs on its own worker thread under one `recv_timeout(SCAN_TIMEOUT)`
(`holders.rs:74-90`), so the deadline covers `canonicalize`, the child wait and both pipe drains
rather than the child wait alone. The code says plainly what it does not do: a thread stuck in a
syscall against a dead mount is still stuck when this returns. The deadline bounds what the caller
waits for, not what the kernel does, and it cites `90c9a8f` and `265fc3f` as the same known exposure.
That is an honest disclosure, not a silent reclassification. I did not run a hang experiment on any
borrowed mount, and I am not asking for one.

**`PoolEntry::leased()`: withdrawn, correctly.** The accessor had zero callers and is now removed. The
`status` field stays with a comment that says what it is and that nothing reads it yet, and that the
release path pins on `lease_id` and refuses a slot that has none. The claim that it fixed a safety
property is withdrawn in the repair document rather than quietly dropped. I checked the merged tree:
no `leased()` accessor, `status` field kept, `tail` still `pub(crate)` from main's change.

**`ps .` and friends: fixed.** A leading `.` and `..`, an empty component, and a trailing slash are
all refused, in two places that agree: `validate_mount_relative` at the RPC boundary and
`lexical_child` in the daemon, the latter costing no syscall. Verified live against the private
daemon: `.` -> `must not name the mount itself`, `./slot` -> same, `../escape`,
`a/../../elsewhere`, `a//b` and `.treehouse/repo/1/repo/` -> `must not contain a '..' or empty
component`, `/etc` -> `must be relative to the mount`. The legitimate sibling
`base/.treehouse/repo-9dae3d/1/repox` is **not** refused by a string-prefix rule; it answers
`not_found`, so component-boundary containment is real rather than asserted.

**Unbounded `canonicalize`: moved off the RPC path.** The lexical half of containment is pure path
arithmetic, and the resolving half now runs inside the scan's deadline. Symlink escape is refused
after resolution: `a_prefix_that_resolves_outside_the_mount_is_refused` passes on macOS and ubuntu.

**The exports best-effort gap: unchanged and still disclosed.** `exports.rs` still uses the
best-effort `holders::scan`, so `mount_snapshot` and `unmount_snapshot` keep today's fail-open
behaviour. I am not reopening it as a block on issue #20 as a whole; it is a pre-existing, separate
lane, and the body says so.

## 3. Process-table completeness, stated as far as it is true

C2 now enumerates the process table rather than walking the mount. The honest scope, and what I
checked:

- **Same-user, as the design intends.** `design.md` settles single-user semantics. `lsof` without
  root on macOS reports this user's processes; another user's holders are not visible. That is the
  same limit the pre-existing `/proc` scan has on Linux, where `/proc/<pid>/fd` is unreadable for
  another uid and is skipped by `if let Ok(fds)`.
- **EOF now means complete.** With `+D` gone, a complete walk is the only way lsof can exit, so exit 0
  or 1 with both pipes at EOF is a finished enumeration rather than an abandoned one. The distinction
  I could not previously distinguish is now structural.
- **Still not "every descriptor on the machine".** A process that exits mid-enumeration, or whose fd
  cannot be read, is absent without the scan reporting incompleteness. `parse_lsof` refuses malformed
  output, but it cannot detect a silently omitted row. The body does not claim otherwise and I am not
  treating it as a defect; it is the same-user contract, not a new fail-open.

## 4. Direct counts, at the exact head

| check | command | result |
| --- | --- | --- |
| integration fixture, same file on both sides | `cargo test --locked -p cowfs-treehouse --test issue20 -j2` | old `20d0e645`: **5 passed**, exit 0. new `c2e82746`: **5 passed**, exit 0 |
| daemon unit module | `cargo test --locked -p cowfs-daemon --lib -j2 holders` | old **8 passed**; new **14 passed** |
| three crates | `cargo test --locked -p cowfs-treehouse -p cowfs-ctl -p cowfs-daemon -j2` | **277 passed, 0 failed, 5 ignored**, exit 0 |
| fmt | `cargo fmt --all --check` | exit 0 |
| clippy | `cargo clippy --locked` three crates `--all-targets -j2 -D warnings` | exit 0 |
| Linux `/proc`, `RUSTFLAGS=-Dwarnings` | `cargo test --locked -p cowfs-daemon --lib` on `moonscape` | **55 passed, 0 failed**, exit 0 |

Fixture identity: the same `issue20.rs`, sha256 `b2a4da116afe68201d69e4da8b9596d83cd11d92bb1717820d2845796dc78d82`,
installed into both trees, and the two install digests compared before the run.

Two notes on the counts, because the numbers alone would mislead:

- The fixture passes on **both** heads, so it is not a discriminator and the author does not claim it
  is. The discriminating test for the repair is the daemon unit module, where old has 8 and new has
  14. The non-tautological comparison here is the live NFS cycle in section 1, old against new on
  identical fixture shape.
- The 5 ignored are `cowfs-nfs` and `cowfs-daemon/tests/end_to_end.rs` cases this PR does not touch.
  I compared the `#[ignore]` set at `46b0f26` and at the head: unchanged.

CI at this head, one read of run `37269821384`: head SHA matches, three jobs SUCCESS
(`check (ubuntu-latest)`, `check (macos-latest)`, `linux-fuse`), `cargo fmt`, `cargo clippy
--workspace --all-targets -- -D warnings`, `cargo test --workspace` and the bench harness all green
on ubuntu and macos. I read the ubuntu log and confirmed the repair's tests ran there rather than
skipping: `a_holder_behind_an_unreadable_directory_is_still_reported`, `proc_locks_rows_are_read_from_the_field_the_kernel_writes`,
`a_lock_row_that_is_not_understood_is_not_silently_zero`, `a_real_flock_is_reported_as_a_lock_hold`
all `ok`, and `skipping:` appears zero times. The macOS log shows the lsof-side tests including
`an_inherited_pipe_is_not_a_finished_answer` and `output_that_is_not_a_complete_answer_is_refused`.
Three runs are visible on the branch: `37268475461` failed at `1aa6f6b`, `37269390256` passed at
`02d46f3`, `37269821384` passed at the head. No polling, no dispatch, no rerun, no runner change.

## 5. Merge state against current main

`git merge-tree --write-tree c2e8274 252b93e` is clean, exit 0, merged tree
`661981efdf97c39aeb1adb06a0f2c908b05769ad`. Only two files are touched by both sides since the merge
base: `crates/cowfs-treehouse/src/cli.rs` and `crates/cowfs-treehouse/src/th.rs`, both merged without
conflict and both sides' semantics intact in the merged tree: `canonical_of` from main's `--canonical`
/ `--ns-helper` work and `if mode_b || env.socket.is_some()` from this PR's return wiring, `tail` still
`pub(crate)`. `holders.rs`, `handler.rs` and `validate.rs` are byte-identical between the merged tree
and the PR head, so the merge cannot have altered the repair.

I extracted and byte-verified that merged tree (577 tracked blobs, 0 mismatches) and ran the affected
crates from it. Result: **224 passed, 1 failed, 8 ignored**, exit 101. The single failure is
`cowfs-daemon/tests/namespace_durability_gate.rs::a_synced_namespace_survives_a_killed_daemon_in_ci`,
panicking `the daemon did not start serving within 45s`. I then ran that same gate on **main alone**
on this machine and it failed identically, same test, same message, 45.17s. That file does not exist
in the PR tree and is not touched by this PR. It is a main-side lane failure on this host, not
something this merge introduced, and not this reviewer's to repair. Every other suite in the merged
tree is green, including the five #20 integration cases run against the exact fixture.

## 6. Closing references

`closingIssuesReferences` is **0**. The body no longer carries `Closes #20`, and a scan of all 13
commit subjects and bodies finds no closing keyword for any issue. So there is no risk of an
incomplete acceptance criterion being auto-closed, and no negated `fix #20` anywhere in the history.
Issue #20 stays open, which is correct: its FUSE item is unmeasured, its `.nfs*` handling item is
undecided, and its upstream proposal is an unsent draft.

## 7. Carried evidence and its scope

The author's live-mount cycle binds to `02d46f3`, which is an ancestor of the head with only a docs
commit after it, so the code under those measurements is identical to the head's. I read the raw
receipts in the author's lease, read-only, and they are consistent and specific: real NFS mount line
`localhost:/cowfs-2df9fdcd99f7592b4…`, holder pid 84676 with cwd `/`, `return without --force exit 5`,
descriptor read after the name was gone with sha256 `9c072eb6…` matching the source, the silly-rename
`.nfs.20051706.4bf4` at the same digest, a second refusal exit 5 naming it, the native baseline leaving
`. ..` and zero silly-renames, and `silly-rename entries 2s after the kill, mount still up: 0`. The
blocking case itself was run there too, in `repair/att-final2`, with `VERDICT
treehouse_return_invoked=no`.

I did not re-run the full mount cycle at the head, because the captured receipts are source-bound to
the head's code and the native public seam I exercised in section 1 covers the same question against
the head's own binaries. No new daemon, mount or lease was needed for this review beyond my own
private attempts.

**FUSE remains unmeasured, and I am not requesting it.** The `fd`/`lock` scan has run against the
macOS NFS loopback and against `/proc` on real Linux, where there is no silly-rename and a held
descriptor is a plain open file. The repair document says this plainly and keeps issue #20's FUSE item
open. No new performance or conformance claim is introduced anywhere in this delta.

## 8. Recommendation

**Merge.** The defect that was blocked is closed, and closed for the right reason: the walk is gone
rather than the walk's failure mode being reclassified. The Linux lock parser is fixed and confirmed
against a real `flock` on a real Linux host with a release half in the test. The withdrawn claim is
withdrawn rather than quietly deleted. Counts, fmt, clippy and CI at the exact head are green and I
re-ran them myself. Closing references are empty. The merge against current main is clean and I built
and ran the merged tree.

Two things for the coordinator, neither a blocker:

1. `a_synced_namespace_survives_a_killed_daemon_in_ci` fails on main alone on this host. That is a
   main-side lane in someone else's ownership, unrelated to #20, and I left it alone.
2. The dead-mount worker thread is a disclosed limitation, not a closed question. It is documented in
   the code next to the commits that first recorded the trap. Worth keeping on the list for whoever
   owns the crash lane, not worth blocking a mount-side holder fix for.

Issue #20 should stay open after merge. This PR closes its cowfs-side work item and its
kill-time-versus-unmount-time question. It does not close the FUSE re-check, the `.nfs*` handling
decision, or the upstream proposal.

## Reviewer process notes

- Lease 12, still held, `e3493f6d6d7089385c5473d5683392dd`. HEAD stayed at
  `6a8075aedd53a69d6f6735acace5b6817f771290` on `review/xfstests-g5`. No checkout, reset, stash,
  branch change, source edit, commit, push, merge or lease return.
- Owned paths only: `bench/out/open-descriptor20-final-critic/**` in this lease, and this document in
  the primary checkout. My earlier `bench/out/holders20-critic/**` and the earlier report were left
  exactly as they were.
- Every process I started was verified by pid, argv, start time, store, socket and mount-table line
  immediately before any signal, and each private attempt carried an exit trap. One stale private
  mount was left behind by an early script whose trap raced its own `umount`; I identified it as mine
  by its export name and mount path, unmounted that single path, and confirmed the shared daemon's
  mount and the OrbStack mount were untouched.
- Shared surface after all work: 16 of 16 leases held, shared daemon `15263` with start time
  `Sat Oct 3 20:44:29 2026` unchanged, both pre-existing NFS mounts present, no mount of mine left,
  340 GiB free, 4.8 GiB of owned artifacts under the 8 GiB allowance. No sudo, sysctl, install,
  reboot, workflow dispatch, rerun or runner change.
- The one remote action was a read-only stage under `/home/moonscape/cowfs-ready-wave/task-20-critic/`
  on the shared Linux lane lock, running `cargo test` and a `/proc/locks` probe. No existing remote
  mount was touched.