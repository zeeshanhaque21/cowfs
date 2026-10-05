# Repair evidence: review of #112, finding 1

Task: issue [#20](https://github.com/zeeshanhaque21/cowfs/issues/20), PR
[#112](https://github.com/zeeshanhaque21/cowfs/pull/112).
Canonical document: `docs/verification/ready-20.md`.
Review answered: `docs/reviews/open-descriptor20-final.md`, sha256
`7db8c0229e93845925ee95b07b57c54e7e1fe581402d37ab32fc4c4b0433be43`.

Reviewed head: `20d0e645b8bf2836fc88978a07e12d0f8d8dd20c`, tree
`e655bb13b38e666fc35634facd46ae02699e5f1e`.
Repair head: `02d46f3a71f16bffe50433d435a7401030d2e089`, tree
`062c0b1b7335284d8dd37bfb394dcdc46e0a57e1`.

This file is the raw record behind one section of the canonical document.
Everything here was run; nothing here is projected.

## The blocking finding

`lsof -nP -w -F pcfn +D <dir>` walks the directory.
On a directory the scanning user cannot enter it gives up, and it exits 1 with no stdout and no
stderr, which is byte-identical to its own "matched nothing".
The code mapped exit 1 to `Ok(text)`, `parse_lsof("")` to an empty list, and an empty list to a clear
slot, so `cowfs-treehouse return` went on to ask treehouse to release a slot with a live open
descriptor in it.

Two distinct false-empty paths were identified.
This one.
And `recv_timeout(5s).unwrap_or_default()`, which discarded any output that had not arrived within
five seconds, so a grandchild still holding the pipe turned a real answer into an empty string.

## Harness

`bench/out/ready-20/repair/probe.sh`, run once per build.
Everything it touches is private to the attempt: its own store, its own mount, its own daemon, its own
socket, its own sandbox `HOME`, and a pool created inside its own mount by a real
`treehouse get --lease`.

The `treehouse` binary the companion is given is a **shim** that appends its argv to a sentinel file
and refuses, forwarding only `status`, which the companion needs in order to read a lease id.
So a run can prove exactly one thing: whether the companion got as far as asking treehouse to release
a slot it should have refused.
No real release is ever executed.

The holder is a single process that forks nothing, so killing it releases the only descriptor there
is.
It writes its descriptor into a `chmod 000` subdirectory, which is the reviewer's shape and the only
one where a permission can hide the holder at all.
Its working directory is `/`, so nothing but the descriptor gives it away.

`uid` is recorded and asserted non-zero at the start of every run: a mode 000 directory is only a real
denial for an unprivileged user, and a root runner would make the whole case a no-op.

## Old build binding

`bench/out/ready-20/repair/build-old.sh`:
`git archive` of the reviewed commit, so the tree is exactly the committed bytes;
an isolated `CARGO_TARGET_DIR`, so no warm artefact from another tree can be mistaken for it;
a sha256 manifest; and a staleness check that fails the build if any source file is newer than any
binary.

```
head 20d0e645b8bf2836fc88978a07e12d0f8d8dd20c
tree e655bb13b38e666fc35634facd46ae02699e5f1e
cowfs-daemon  0294081786299af270cef3edbbf6f3cf563d087f88b9e9570a05fce21a11cd1d
cowfs         442a6ceeff52ae5dae11575813dba159627c28ec0d5307b5c3c38e2efaada8e1
cowfs-treehouse b759987c4afa3429d3779ab927e224fc4ae64bf4dd81e538ae7c4f7d8836c70f
OLD_BUILD_BOUND
```

## Result at the reviewed head

Attempt `bench/out/ready-20/repair/att-old/`, exit 0, raw log in `att-old/log/`.

```
uid 501 (zeeshanhaque), not root, so a mode 000 directory is a real denial
holder pid 74556 cwd [/], holds only <slot>/locked/held.txt
A readable: 2 holders, kind "fd", no "cwd" hold
B chmod 000 <slot>/locked, mode d---------, unreadable by this user: yes
RAW lsof +D: rc=1 stdout=0 bytes stderr=0 bytes
B ps rc=0: {"processes":[]}
VERDICT holder_found_while_a_subdirectory_is_unreadable=no
C return exit 0
C return stderr: []
C treehouse calls recorded:
    1791175433 status --json --root <pool>
    1791175433 return <slot> --if-lease-id 391e7b04bb05651339bfad2ea01e4f7b --root <pool>
VERDICT treehouse_return_invoked=yes
VERDICT holder_still_alive=yes
```

And what the command told the operator while doing it:

```json
{"slot":"<slot>","snapshot":null,"holders":[],"holder_scan":"scanned base/.treehouse/cowfs-7c1bf8/1/cowfs",
 "terminated":[],"killed":[],"skipped":[],"nfs_dirt":[],"refused_busy":false,"lease_id":"391e7b04bb05651339bfad2ea01e4f7b"}
```

The exit code, the empty stderr and the empty `holders` are all three of the things the review said
are not evidence: rc 1 with empty stderr is exactly what `lsof` emits when it gives up.

### A shape that does not reproduce

With the held file **outside** the unreadable subdirectory, at the same head:

```
B RAW lsof +D: rc=1 stdout=381 bytes stderr=0 bytes
B ps rc=0: 2 holders, kind "fd"
```

`lsof` reports what it walked before giving up, so the holder is still found.
The defect needs the holder behind the directory lsof cannot enter.
Recorded because it is the ordinary shape of a build tree with one restricted subdirectory, and
because it means a partial-output exit 1 is not safe to trust either.

## Result at the repair head

Attempt `bench/out/ready-20/repair/att-final2/`, exit 0, same script, same fixture, raw log in
`att-final2/log/`.
Binaries from this lease at `02d46f3`:

```
cowfs-daemon    1195e81eff9e93972f705e63a7082aa4b229adced416bb8dcdb3f2fc82a7ebb8
cowfs           50ca196d7c93bcdb2b2e0e6959336df9a66a89ae8907fb25f6ea3f066535f10a
cowfs-treehouse 7ac1ce42e9480c0384816c40bb1f9f38090a95251859b85ddb957b1af8a2617c
```

```
uid 501 (zeeshanhaque), not root, so a mode 000 directory is a real denial
holder pid 80823 cwd [/], holds only <slot>/locked/held.txt
A readable: 1 holder, kind "fd"
B chmod 000 <slot>/locked, mode d---------, unreadable by this user: yes
B ps rc=0: 1 holder, kind "fd", path inside the unreadable directory
VERDICT holder_found_while_a_subdirectory_is_unreadable=yes
C return exit 5
C return stderr: cowfs-treehouse: <slot> holds pid 80823 (bash): fd <slot>/locked/held.txt that
  treehouse cannot see, and --force was not given; returning it now would unlink a file a live
  process still has open
C treehouse calls recorded:
    <ts> status --json --root <pool>
VERDICT treehouse_return_invoked=no
VERDICT holder_still_alive=yes
D holder 80823 killed, <slot>/locked still d---------
D ps rc=0: {"processes":[]}
VERDICT empty_scan_is_a_real_answer=yes
```

Row D is the one that makes row B mean something.
An empty answer is only usable if it is real, and it is real here because the enumeration no longer
depends on the mount being readable.

## What replaced the walk

| | before | after |
| --- | --- | --- |
| command | `lsof -nP -w -F pcfn +D <prefix>` | `lsof -nP -w -F pcfn` |
| what is enumerated | the directory tree | the process table |
| a mode 000 subdirectory | hides the holder, exit 1, empty | irrelevant |
| cost | 0.3 s empty slot, 0.86 s on 100k files | 0.38 s to 0.49 s, about 57k lines, 2.1 MB, independent of tree size |
| recursive walk of the mount | yes | no |

Cost measured three times on this machine at the time of the change.

Completeness rules, all of which must hold or the answer is refused:

1. the prefix resolves, inside the deadline, and still sits inside the mount,
2. the child exits,
3. both pipes reach end of file, inside the same deadline,
4. every stdout line parses: an unknown tag, a record with no usable pid, a name with no descriptor
   and a field-less line are all refused, and a descriptor with no name is accepted,
5. the exit status is 0 or 1, and any other status carries its stderr into the refusal.

Rule 3 has a test that produces the condition with a real inherited pipe, because wave one read that
case as `Ok("")` and an empty string was a clean slot.

## The Linux `/proc/locks` parser

Before: the fourth whitespace token, parsed in decimal.
The fourth token is the word `ADVISORY`.
So `HoldKind::Lock` was never reported on Linux at all.

After: the first field shaped like a device, major and minor in hex, inode in decimal, matched
against `libc::major`/`libc::minor` and `st_ino`.
A row that does not parse refuses the whole table rather than contributing no locks.

Real rows from the box, kernel `6.12.109+rpt-rpi-2712`:

```
1: FLOCK  ADVISORY  WRITE 1437365 08:02:768494 0 EOF
2: FLOCK  ADVISORY  READ 1437365 08:02:5629643 0 EOF
3: OFDLCK ADVISORY  WRITE -1 08:02:142895 0 4611686018427388799
4: OFDLCK ADVISORY  WRITE -1 08:02:142895 4611686018427388928 EOF
rows: 119
```

`OFDLCK` rows carry pid `-1`, and the parser ignores the pid value entirely because the lock set is
keyed on the device and inode.
Captured rows in the test suite additionally cover a waiting lock's `-> PID` field, which moves the
device one position later, and hex digits that are not decimal.

End to end on the box, `RUSTFLAGS=-Dwarnings cargo test --locked -p cowfs-daemon --lib`, exit 0.
Stage `/home/moonscape/cowfs-ready-wave/task-20/repair-02d46f3a71f16bffe50433d435a7401030d2e089/src`,
with `crates/cowfs-daemon/src/holders.rs` md5 `f2f3030f74929f024f756ce4f7d12f96` recorded on the box
before anything of ours runs:

```
test holders::tests::a_real_flock_is_reported_as_a_lock_hold ... ok
test holders::tests::proc_locks_rows_are_read_from_the_field_the_kernel_writes ... ok
test holders::tests::a_lock_row_that_is_not_understood_is_not_silently_zero ... ok
test holders::tests::a_holder_behind_an_unreadable_directory_is_still_reported ... ok
test holders::tests::a_holder_that_chdird_out_is_still_reported_and_only_by_its_descriptor ... ok
test result: ok. 55 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

The real-flock test is a pair, not a single assertion: it waits for its own inode to appear in
`/proc/locks`, asserts the scan reports a `Lock` hold on exactly that path, kills the holder, waits
for the row to leave the table, and asserts the lock is no longer reported.
Without the second half, an empty table would pass.

## The withdrawn claim

Wave one changed `PoolEntry` from `leased: bool` to `status: String` and added a `leased()` accessor.
The accessor had **zero callers**, and the release path pins on `lease_id` and refuses a slot that has
none, so no safety property depended on it.

The accessor is removed and the claim that it fixed a safety property is withdrawn.
The `status` field stays, because it is what treehouse actually prints and a `bool` there
deserialised as false for every slot ever read.

## Containment, in two halves

First half, no syscall, so it cannot be what wedges on a stale mount:
a name that is absolute, climbs out with `..` in any position, contains an empty component, or names
the mount itself with a leading `.`, is refused before any path is touched.
This also closes `ps .`, which was a legal request that scanned the whole mount with no bound but the
timeout.

Second half, inside the scan's deadline:
the prefix is resolved and checked against the mount, so a symlink out of the mount is refused rather
than followed.

Both are verified on the mount in `att7`:

```
cowfs --socket <sock> --json ps ../escape
{"error":{"code":"invalid_params","message":"invalid mount-relative name \"../escape\": must not contain a `..` or empty component"}}
```

and in `att-final`, where a symlink from inside a tempdir mount out to a sibling directory is refused
with `outside the mount`.

## Suite deltas

| | reviewed head | repair head |
| --- | --- | --- |
| `cowfs-daemon --lib` | 49 on Linux, 53 on macOS | 55 on Linux, 59 on macOS |
| `cowfs-treehouse --test issue20` | 4 | 5 |
| `cowfs-treehouse` total | 121 | 122 |
| all three crates | 270 passed, 0 failed, 5 ignored | 277 passed, 0 failed, 5 ignored |

Old fail, new pass on the integration suite, product files only:

| code | result |
| --- | --- |
| base `46b0f26` | exit 101, 0 passed, **5 failed** |
| head `02d46f3` | exit 0, **5 passed** |

`bench/out/ready-20/old-fail-new-pass.sh` takes the base commit as an argument.
It used to use `HEAD~1`, which after a repair commit is a commit that already contains the fix; a run
against that base reports that the tests prove nothing, and did.

The `holders.rs` unit tests cannot be shown in the same table.
They call `scan_mounted`, `scan_checked`, `Scan::Unavailable` and the Linux lock reporting, none of
which exist at `46b0f26`, so the base cannot compile them.
Their old-fail statement is the live probe above.

## What CI caught that macOS could not

`Instant` was imported at module scope for the `lsof` deadline.
On Linux the `/proc` module reads no clock, so the import was unused, and ubuntu's
`cargo clippy --workspace --all-targets -- -D warnings` failed the job while the macOS job passed.
Run 37268475461, job `check (ubuntu-latest)`, exit 101:

```
error: unused import: `Instant`
   --> crates/cowfs-daemon/src/holders.rs:15:27
    |
15 | use std::time::{Duration, Instant};
    |                           ^^^^^^^
    = note: `-D unused-imports` implied by `-D warnings`
```

Fixed in `02d46f3` by importing it in the module that reads a clock.
The permanent answer is that the remote run now promotes warnings to errors with
`RUSTFLAGS=-Dwarnings`, which is the same gate CI applies, so this class of defect is caught on the
Linux box rather than in CI.
Clippy itself is still unusable on that box: its clippy is 1.95.0 and reports a pre-existing
`collapsible_match` in `crates/cowfs-meta/src/tx.rs`, which this task never touches.

## Cleanliness after the runs

- no `cowfs-daemon` of this worker's left running; the shared daemon PID 15263 was never touched,
- no private mount left mounted,
- no external socket directory left,
- the orphan `sleep 600` that one failed Linux test left on moonscape was killed by verified pid,
- four superseded stage directories on moonscape removed; the final stage and its log kept,
- the Linux test's own holder is one `python3` process precisely so that a failed run does not leave
  a lock held on the box.

## Not claimed

- **FUSE.** The `fd` and `lock` scan has been run against the macOS NFS loopback and against
  `/proc` on real Linux. It has **not** been run against a real FUSE mount, where there is no
  silly-rename and a held descriptor is a plain open file.
  Issue #20's work item covering FUSE is therefore still open.
- **`exports.rs`.** `mount_snapshot` and `unmount_snapshot` still use the best-effort scan and are
  still fail-open on an unavailable platform. Unchanged, deliberate, another lane, not destructive,
  and still on the list.
- **Other users' processes.** Completeness is argued over the process table, not over the kernel.
  A process running as another user can have its descriptors hidden from `lsof`; that is a scope
  limit of the platform in this single-user model, not an empty answer, and it is not claimed to be
  covered.
- **A wedged mount.** The deadline bounds what the caller waits for. A thread stuck in a syscall
  against a dead mount is still stuck when it returns.
