# Git index integrity harness repair: evidence

Lane: ready-wave slot 3, `investigate/integrity-21`.
Lease: `.treehouse-ready-wave/.treehouse/cowfs-7c1bf8/3/cowfs`, base `6df8b9fcfd6f9d54a6af0c8e424f3851fc6ecc71`.

Purpose: record what the independent review of PR #104 found in
`scripts/verify-git-index-integrity.py`, what was changed, and what was re-measured afterwards.

Issue #21 remains open. Nothing here reproduces, explains or resolves the historical `.idx`
corruption, and no production source was changed.

## Finding status

| Finding | Severity in review | Status |
| --- | --- | --- |
| F1, PR body autocloses issue #21 | BLOCK | FIXED at the PR metadata level, verified `closingIssuesReferences: []`, issue open |
| F2, documented commands cannot work as written | MEDIUM | FIXED, build command corrected, real bounds enforced |
| F3, teardown reports a mount as gone when it is not | MEDIUM | FIXED, tri-state reader plus quarantine |
| F4, no device-identity oracle | MEDIUM | FIXED, attestation requires nfs plus a foreign device plus negative controls |
| F5, `show-index` overstated | MEDIUM | FIXED, combined gate plus corrected claim |
| F6, zero-run scan is a hypothesis generator | LOW | ALREADY CORRECT, deliberately outside `verdict_parts` |
| 7 ruff findings | - | FIXED, no suppression |

## F1 was real, and it was a live failure

The review measured `closingIssuesReferences(first: 20)` as `[issue #21]` and
`closingIssuesReferences(userLinkedOnly: true)` as `[]`, so the link came from GitHub's
closing-keyword parser, not from a human.
The trigger was body line 21, where a negated closing keyword sat immediately before the reference
on the same line; the parser is purely lexical and does not understand English negation.
Merging #104 would therefore have resolved issue #21.
The body sentence has been rewritten so no closing keyword shares a line with the reference, and no
line of the source, the commit messages or this document pairs a closing keyword with an issue
reference.

Re-measured after the repair, from the same GraphQL query:

```json
{"pullRequest":{"closingIssuesReferences":{"nodes":[]},"state":"OPEN"},"issue":{"state":"OPEN"}}
```

Both PR #104 and issue #21 are `OPEN` and the closing references list is empty.

## The parser could not read this host's mount table

The review's F3 fix introduced a tri-state reader on top of `split_mount_line`.
Measured against the real table on this host:

```text
before: total 17  unparsed 17
after:  total 17  unparsed 0
```

`split_mount_line` took the fstype from the source head, but a mount source carries no ` on ` of its
own, so every real line was refused.
A one-character mountpoint, `/`, was rejected by an off-by-one in the mountpoint bracket.
Because the reader could not parse anything, the table degraded to `UNKNOWN` for every target, and
the harness could never have reached a verdict on this host at all.

The fstype now comes from the first token inside the options list, which is where `mount`(8) prints
it:

```text
OK: ('map auto_home', '/System/Volumes/Data/home', 'autofs')
OK: ('localhost:/cowfs-6952209e...', '/Users/zeeshanhaque/.cowfs/mnt', 'nfs')
```

A single-character mountpoint and a source containing a space both parse.
The escape decoder runs exactly one pass, so `\040040` is a space followed by the literal text `040`
and never a double decode.
The match is an exact parsed comparison, so `/tmp/mnt-a` no longer matches a mountpoint of
`/tmp/mnt-ab`, which the review's original substring reader would have accepted.

## Attestation: it had to fail, and then it had to stop failing for the right reason

The first real private-mount run at this head failed closed, and it was the attestation's own bug
rather than cowfs's:

```text
FSTYPES ['nfs']
MOUNT_STATE present
DEV native/mount/scratch: [16777234, 436209529, 436209529]
WHY: mount arm st_dev equals the scratch root device
```

The attestation compared the mount arm against the mountpoint, and the mountpoint is legitimately on
the NFS device, so it compared the export against itself and rejected every correct run.
It now compares against the mountpoint's parent, the local scratch directory the harness created,
which is the actual silent-fallback shape.

Negative controls run with it and both hold:

| Control | Result |
| --- | --- |
| two APFS directories, no mount line, no daemon | attestation `pass: false` |
| mount arm is a local directory, native arm a real repo | attestation `pass: false` |

## `show-index` does not detect the historical shape

Measured on git 2.56.0 and reproduced on Apple's git 2.54.0:

| idx mutation | `show-index` | `verify-pack` | `fsck --full` |
| --- | --- | --- | --- |
| pristine | 0 | 0 | 0 |
| 256 zero bytes at a fixed offset | 128 | 1 | 27 |
| trailing SHA-1 last byte flipped | **0** | 1 | 1 |
| whole idx zeroed, the historical shape | **0** | 1 | 27 |
| truncated to half | 128 | 1 | 27 |

`git show-index` returns **0 on a wholly zeroed idx**, which is exactly the shape issue #21
describes, so the per-op `idx.check` now requires `show-index`, `verify-pack` and a present sibling
`.pack` together.
An empty pack directory is a fail, not a vacuous pass.
Unit tests zero the idx, flip its trailer, delete its pack and empty the pack directory, and require
each to fail the gate.

## Window bounds

`build_ops()` defines **44** operations, not the documented 60.
The review found that any `--ops` above 44 was silently sliced to 44 while the run then compared
its own `planned == executed` check, which a shortened window trivially satisfies.
A request outside 30..44 now exits 3 and names the real ceiling.

```text
ops=60 rc=3 :: requested window 60 is outside 30..44; build_ops() defines 44 operations
ops=45 rc=3 :: requested window 45 is outside 30..44
ops=29 rc=3 :: requested window 29 is outside 30..44
ops=30 accepted, 44 accepted
```

## Teardown cannot read an absence out of a failed read

`MountTable` is tri-state.
`PRESENT` requires an exact parsed match for the requested path.
`ABSENT` is reported only when the whole table read cleanly and every line parsed.
`UNKNOWN` covers a failed reader, a timeout, empty output, or any line this parser cannot read.

`stop()` keeps the process state and the mount state separate.
A dead pid whose mount reads `PRESENT` or `UNKNOWN` is a quarantine, not a cleanup:

| Case | `process_stopped` | `mount_state` | `clean` |
| --- | --- | --- | --- |
| dead pid, mount absent | true | absent | true |
| dead pid, mount present | true | present | **false**, with `quarantine` |
| dead pid, table unreadable | true | unknown | **false** |

This harness never calls `umount` and never walks a mount, so it has no way to clear a mount it did
not create, and it makes no cleanup claim it cannot support.

## Re-measured after the repair

One bounded real run, 30 operations, 64 cookie entries, on a private Core NFS mount.
Not the 42-op window and not a soak.

```text
verdict NOT REPRODUCED: no git index or pack-index corruption in the declared window on either arm
exit_code 0
requested_ops 30   declared_ops 30   available_ops 44
window_planned_eq_executed true          window_no_failed_op true
mount_attested true                       mount_attestation_negative_controls true
idx_integrity_native true                 idx_integrity_mount true
checks_native true                        checks_mount true
pack_compare true                         history_compare true
worktree_compare true                     reopen true
shared_daemon_untouched true
```

| Item | Value |
| --- | --- |
| attempt | `bench/out/ready-21/repair/20261004T185409-attempt` |
| raw logs | `log.jsonl` 96 records, `summary.json`, `daemon-a.log`, `daemon-b.log` |
| raw logs location | gitignored under `bench/out/`, named as a path rather than committed as blobs |
| mount line | `localhost:/cowfs-83f9377472c93587b47d6abfa48f41bb on .../mnt-a (nfs, nodev, nosuid)` |
| fstypes | `['nfs']` |
| devices, native / mount / scratch | `16777234` / `436209587` / `16777234` |
| daemon backend | `--backend core`, real store, private socket |
| write witness | bytes matched, on the foreign device, removed |
| window executed / skipped | 30 / 0 native, 30 / 0 mount |
| idx per arm | 1 idx, `show-index` pass, `verify-pack` pass, `idx_integrity` pass |
| idx sha256, both arms | `c0cef4476687747f2a8a5b16b710b54f624017b27a6ee3ffc9fa2523e703c165` |
| commits, both arms | 5, equal lists, equal HEAD |
| worktree compare | 7 paths, byte identical |
| import | rc 0, 35 files, 1,852,640 bytes, `mismatches: []` |
| reopen | daemon A stopped, daemon B on the same store, `cowfs fsck` ok, `git fsck --full` rc 0 |
| teardown | 1 daemon, `clean: true`, `mount_state: absent` |
| cookie probe | pass, no divergence |

Binaries reused from the predecessor's release artifacts, not rebuilt:

| Binary | sha256 |
| --- | --- |
| `cowfs-daemon` | `6273f72478025d49911898d20a67f8bacdc375f2544b34fe6887d2e8e11abfa1` |
| `cowfs` | `4da060426e96b555e80fd36b48fcdc1270fd3345cf29c731795ea48dcc148667` |

Production source is unchanged by this lane: `git diff --stat origin/main...HEAD -- crates/` is empty.

## Checks run, with true exit codes

| Check | Command | rc |
| --- | --- | --- |
| lint | `ruff check scripts/verify-git-index-integrity.py scripts/test_verify_git_index_integrity.py` | 0, `All checks passed!` |
| compile | `python3 -m py_compile` on both files | 0 |
| unit tests | `python3 scripts/test_verify_git_index_integrity.py` | 0, 42 tests OK |
| window bounds | `--ops` 29 / 45 / 60 | 3 each |
| real run | `--ops 30 --cookie-entries 64` | 0 |

The unit tests start no daemon, mount nothing, send no signal and read no real mount table, so they
are safe to run on a machine serving other agents.

## What was deliberately not done

- **No 805 MiB soak, no 16-cell cookie grid rerun, no perf measurement, no crash matrix.** The
  dispatch forbids them for this lane and they would not change any finding.
- **No cold Rust build.** The predecessor's release artifacts were reused and their hashes recorded
  instead. Production source did not change, so a rebuild would produce the same binaries.
- **No merge, no lease return, no force push, no rebase, no manual edit of a generated file.**
- **The shared daemon 15263 was snapshotted before and after and never addressed.** Same pid, same
  start time `Sat Oct  3 20:44:29 2026`, same mount line, `untouched: true`.

## What this does not establish

- The original 42/42 run's claims are historical and were produced by the author's harness at an
  older head. The bugs listed above were in that harness, so its numbers are not re-derived here;
  only the post-repair run is claimed.
- Issue #21 bullet 2 is not reproduced and not explained. The spike's long soak on the 805 MiB
  OmniRoute repo is still outstanding.
- No crash or power-loss claim. No SIGKILL, no crash injection.
- The mount arm is one pack and one idx, about 864 KiB. A defect needing many objects or many
  concurrent writers is outside this window.
- No performance claim. One machine, one moment, under contention from other workers.
- Independent review is still required before merge.