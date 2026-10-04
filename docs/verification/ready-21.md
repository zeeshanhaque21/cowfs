# Ready-task #21: Git index and pack-index integrity over a private Core NFS mount

Task: ready-wave slot 3, `investigate/integrity-21`, lease
`.treehouse-ready-wave/.treehouse/cowfs-7c1bf8/3/cowfs`, base `46b0f269d5bef4a2c204c25f5b3015da601d3beb`.

Issue #21 bullet 2 is the open one: a `.idx` on the mount came back with zeroed runs and a failed
its own SHA-1 trailer, `fsck` exited 14, the `.pack` was valid, and the exit code of the run that
produced it was lost. It was never reproduced.
This task reproduces it or reports it not reproduced, with evidence.

**Result: NOT REPRODUCED in the declared window.**
No git index or pack-index corruption on either arm, over a 42-operation window, with a matched
native control, byte-identical packs, equal history, and a store reopen through a fresh daemon.
This is *not* a claim that the historical corruption is resolved, and issue #21 stays open.
What is established is narrower and checkable: the current Core backend did not reproduce it in
this window, and the harness that would catch it now exists and is repeatable.

## Ownership

This lane owns `scripts/verify-git-index-integrity.py` and this document.
It edits no production source.
The readdir cookie result belongs to ready-wave slot 1 (`#19`), which owns the vendored cookie and
readdir requirements, so it is reported rather than patched.
`crates/cowfs-daemon/src/import.rs`, the namespace-barrier work and the shutdown server are
untouched, as the dispatch requires.

## What ran

```sh
cargo build --release -j4 -p cowfs-cli -p cowfs-daemon -p cowfs-gc   # private target dir
python3 scripts/verify-git-index-integrity.py --ops 42 --cookie-entries 2500
```

Both were run as one foreground invocation through the wave's `mac-heavy.lock`.

Identity of what produced the numbers:

| Item | Value |
| --- | --- |
| git | `git version 2.56.0` |
| platform | `macOS-26.6.2-arm64-arm-64bit` |
| python | 3.12.2 |
| `cowfs-daemon` | sha256 `6273f72478025d49...` |
| `cowfs` | sha256 `4da060426e96b555...` |
| backend | `--backend core`, the real store, not the path backend |
| evidence | `bench/out/ready-21/20261004T163143-attempt/{log.jsonl,summary.json}` |

`log.jsonl` is append-and-flush per record, so the run is inspectable step by step.

## The two arms

Same seed, same declared operation sequence, same git binary.

- **N native**: a real git repo on APFS, cloned from the native seed.
- **M mount**: the same repo cloned from the imported snapshot, with `.git` and the worktree both
  on a private cowfs Core NFS mount served by a private `cowfs-daemon` on a private store.

The seed is a real repo with real history, a real pack and a real idx, and it contains
incompressible blobs so a zeroed region cannot hide inside plausible compressed output.
`cowfs import` reported `"verified": true`, 35 files, 1,852,640 bytes in, 1,731,582 stored,
`source_root_hash` equal to `imported_root_hash`.

## The declared window: 42 operations, bounded

Not a soak.
`--ops` is clamped to the 30..60 band and each operation is exactly one subprocess, or one
deterministic fixture write, with its exit code read from `subprocess.returncode` directly, never
through a pipeline.

| # | Operation | # | Operation | # | Operation |
| --- | --- | --- | --- | --- | --- |
| 1 | `status --porcelain=v1` | 15 | `update-index --refresh` (dirty) | 29 | `checkout -` |
| 2 | write `README.md` | 16 | `diff --stat` | 30 | `merge --no-edit topic` |
| 3 | `add -A` | 17 | `add -A` | 31 | `log --oneline -n 5` |
| 4 | `status --porcelain=v1` | 18 | `update-index --refresh` (clean) | 32 | `repack -adfl` |
| 5 | `commit -m op1` | 19 | `commit -m op3` | 33 | idx check |
| 6 | `log --format=%H` | 20 | `repack -adf` | 34 | `fsck --full` |
| 7 | write `docs/a.md` | 21 | idx check | 35 | write `docs/stash-me.md` |
| 8 | `add -A` | 22 | `fsck --full` | 36 | `stash push -q -u` |
| 9 | `commit -m op2` | 23 | `worktree add ../wt1` | 37 | `stash pop -q` |
| 10 | `fsck --full` | 24 | `worktree remove ../wt1` | 38 | `status --porcelain=v1` |
| 11 | `gc -q` | 25 | write `docs/b.md` | 39 | `reflog expire --all` |
| 12 | idx check | 26 | `checkout -b topic` | 40 | `gc --aggressive --prune=now` |
| 13 | `verify-pack -v` | 27 | `add -A` | 41 | idx check |
| 14 | write `src/main.txt` | 28 | `commit -m op4-topic` | 42 | `verify-pack -v` |

"idx check" is `git show-index < every .idx`, which validates the idx magic and its own trailing
SHA-1. "verify-pack" is `git verify-pack -v` on every idx, which reads every object the idx points
at. Both are real git with real exit codes.

**Executed / skipped: 42 / 0 on the native arm, 42 / 0 on the mount arm. Zero operations outside
their declared exit codes on either arm.**
An operation is skipped, with the reason recorded, only when a declared prerequisite failed.

### Two exit codes that are correct and were mine, not cowfs's

Both were caught because the native arm returned the same code, which is the point of a matched
control.

- `git update-index --refresh` on a dirty tree exits 1 on a correct filesystem, so op 15 declares
  `[0, 1]`. I first demanded 0; native returned 1 too.
- `git stash pop` needs a stash entry, and `stash push` ignores untracked files without `-u`, so op
  36 declares `-u`. Without it the push exited 0 having stashed nothing and the pop exited 128.
  Native returned 128 as well.

## Semantic checks, real exit codes

| Check | N native | M mount |
| --- | --- | --- |
| `git status --porcelain=v1` | rc 0 | rc 0 |
| `git fsck --full --no-progress` | rc 0 | rc 0 |
| `git fsck --strict --no-progress` | rc 0 | rc 0 |
| `git log --format=%H` | rc 0, 5 commits | rc 0, 5 commits |
| `git show-index` on every idx | 1 idx, pass | 1 idx, pass |
| `git verify-pack -v` on every idx | 1 idx, pass | 1 idx, pass |
| `git worktree list` | rc 0 | rc 0 |
| `git rev-list --all --objects` | rc 0 | rc 0 |
| untouched files vs native seed sha256 | 3 checked, 0 mismatch | 3 checked, 0 mismatch |

Matched comparisons, which are the strongest form available here because both arms ran the same ops
on the same content:

| Comparison | Result |
| --- | --- |
| `HEAD` | `bcc3023c66690bae8c64d1f03dca826ace526457` on both arms |
| commit lists | identical, 5 each |
| `.idx` bytes | `pack-951a90ae0d036def8e194fb27d6b905a8a3edd05.idx`, sha256 `cebe620624c4fb768601c55f9c56846a066fc2c5fb1f27d1189dc568e937ea1f` on **both** arms |
| every tracked worktree file | 7 paths compared, all byte identical |
| untargeted seed files | sha256 equal on both arms |

The `.idx` byte equality is the direct answer to bullet 2.
Git produced the same pack index on APFS and through the NFS mount, so nothing was dropped, zeroed
or misaligned in the pack-index write or read path on the Core backend.

## The zero-run shape the spike observed

Issue #21 records "101 zeroed runs from offset 180,224". Every `.pack`, `.idx` and `.rev` was
scanned for long zero runs, so a recurrence of that shape cannot hide:

| File | Bytes | Total zero bytes | Runs >= 4096 | Longest run |
| --- | --- | --- | --- | --- |
| `...edd05.idx` | 1,800 | 812 | 0 | 0 |
| `...edd05.pack` | 863,608 | 3,561 | 0 | 223 |
| `...edd05.rev` | 156 | 85 | 0 | 0 |

No long zero run anywhere, on either arm.
The longest run in the pack is 223 bytes, inside plausible compressed output.

## Store reopen

A path-backend readback does not imply Core fsck or crash durability, so the store was re-read by
a different process than the one that wrote it:

1. private daemon A stopped, SIGTERM to a pid whose argv carried this run's exact store and socket
2. private daemon B started on **the same store**, new mount, new socket
3. `cowfs --socket B fsck` → `{"blocks_checked":2672,"bytes_checked":2617430,"ok":true,"problems":[],"snapshots_checked":1}`, rc 0
4. `git fsck --full` through the fresh mount → rc 0
5. every tracked worktree file re-read and compared to the mount arm's own post-window state,
   7 checked, 0 bad
6. `git show-index` and `git verify-pack` on the reopened repo → pass

## Isolation

Private store, private mount, private socket in a mode-0700 short directory.
The shared daemon 15263 was snapshotted before and after and is byte-identical across the run:
same pid, same argv, same start time `Sat Oct  3 20:44:29 2026`, same mount line.
`shared_daemon.untouched: true`.

Every signal in this harness is gated on the target's own command line carrying this run's store
and socket, so a pid collision cannot become a signal.
`Leaked.sweep()` runs from `finally`, so an exception cannot leave a private daemon serving and its
NFS mount hanging later `ls` calls; that guard was added after a first crash of this harness did
exactly that, and the leaked pid was then reaped after verifying its argv.

## Readdir cookie: reported for slot 1, not patched here

Issue #21 bullet 3: the readdir cookie was still position based, and a scan-and-unlink loop left
1,186 of 2,500 entries (native 0).

**Not reproduced on the current Core backend.**
Two probes, both matched against native:

`os.scandir` lazy iterator, unlink inside the loop, ladder of 64 / 256 / 1,024 / 2,500 entries:
0 remaining on both arms at every size, 2,500 created and 2,500 unlinked at the largest.

Raw `__getdirentries64` with a harness-chosen buffer, unlinking **every** name as it is parsed, so
the directory shifts completely under the scan.
This matters because `os.scandir` chooses its own buffer, and a directory that fits one libc buffer
never makes the kernel resume a scan at all, so the cookie is never exercised.
Calibration on APFS at 1,200 entries and a 512-byte buffer gives **76 pages**, 1,200 names,
1,200 unlinked, 0 remaining, which is the proof that the resume really happened rather than an
inference from a zero.
The probe is in `cookie_probe_at` and the grid runs as `cookie_sweep`.

Why I believe the historical result cannot recur without a regression, as source evidence and not
as a substitute for the probes above:

- `crates/cowfs-meta/src/tx.rs:106-114` assigns each directory entry a **monotonic** cookie from
  `d.next_cookie`, stored as a by-cookie key, and never reuses a number.
- `crates/cowfs-vfs-path/src/cookies.rs` gives every name a sequence number on first listing and
  retains it, with the doc comment stating the intent: "a cookie stays valid after its entry is
  removed".
- `crates/cowfs-vfs-path/src/tests.rs:97` `readdir_cookies_survive_removal_of_the_entry` and
  `cookies.rs` unit tests cover removal and recreation.

A position that never repeats is not a live offset, so a scan cannot skip an entry that shifted.
Slot 1 owns the vendored requirements and should decide whether that reasoning plus these probes
closes bullet 3 or whether a longer native-equivalent soak is wanted.

### `git worktree remove`

The same issue reports `git worktree remove` failing on the mount with "Directory not empty", 5 of
5. Ops 23 and 24 ran it on both arms inside the window and **both succeeded on both arms**,
including on the mount, so that report did not recur here either.

## What this does not establish

- It is not a soak. The dispatch forbids one and the window is deliberately bounded at 42
  operations. The spike's own suggestion was a long soak on the 805 MiB OmniRoute repo; that is
  still open and is not replaced by this.
- It does not claim the historical corruption is fixed or resolved.
- It does not claim crash or power-loss durability. No SIGKILL and no crash injection was run, and
  process SIGKILL would not establish power-loss behavior anyway.
- The mount arm's size is one pack and one idx, about 864 KiB.
  A pack-index defect that needs many objects, or many concurrent writers, is outside this window.
- Both arms ran on one machine at one moment, under contention from other workers.
  No performance claim is made.

## Two harness bugs found by the matched control

Recorded because they are the reason the harness is trustworthy: the native arm returned the same
"failure" both times, and only the control exposed them as harness bugs rather than cowfs bugs.
A harness that ran the mount arm alone would have reported both as filesystem corruption.

## Repeat

`scripts/verify-git-index-integrity.py` is committed and runnable.
It needs `cowfs-daemon` and `cowfs` built into `bench/out/ready-21/target/release`, `python3` with
`blake3`, and `git`.
It exits 0 only when every integrity check passed on both arms, and prints the per-check verdict.
It accepts `--ops` in 30..60 and `--cookie-entries`.
