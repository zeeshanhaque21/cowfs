# NFS #19 residual triage: typed create and PATHCONF

Pinned commit: bc3ea7d33e3ca55ccb14e0f418a7fd223e3032d8 (main), read via treehouse slot 5 with `git show` and `git grep`. No worktree was modified and nothing was built or mounted.

Source of the residual: `progress/plan.json` item 19 detail, quoted: "Missing typed-create and pathconf behavior reported by g3 must be triaged with verified raw evidence; existing symlink/readdir coverage does not retire those findings."
Issue #19 body does not mention MKNOD or PATHCONF.
g3 in plan.json is "pjdfstest no worse than native", state blocked.

## (a) MKNOD for FIFO, socket, device

Status: MISSING for FIFO and socket. Devices are refused by the same path with no separate handling.

- Dispatch answers NFS3ERR_NOTSUPP for every MKNOD, ignoring the requested type: `crates/nfsserve/src/nfs_handlers.rs:134-137`.
- Deliberate and documented: `crates/nfsserve/PATCHES.md:28`.
- No mknod method in the adapter: `crates/cowfs-nfs/src/adapter.rs` has create, create_exclusive, mkdir, symlink, link only.
- Core refuses by design: `crates/cowfs-core/src/import.rs:15`, test `crates/cowfs-core/tests/import.rs:132`.
- FUSE refuses non-regular mknod: `crates/cowfs-fuse/src/fs.rs:613`, `convert.rs:51`, test `crates/cowfs-fuse/tests/battery/test_cowfs_extra.py:96`.

Tests:
- `crates/cowfs-nfs/tests/protocol.rs:25` (`null_and_unknown_procedures`) asserts NOTSUPP, but sends empty args. It never exercises FIFO, socket, or device.
- Grep for mkfifo, mknod, S_IFIFO, S_IFSOCK, S_IFBLK, S_IFCHR in `crates/cowfs-nfs` returns zero hits.

Failures this gap would cause: pjdfstest mkfifo and mknod cases that expect success and then stat S_IFIFO or S_IFSOCK fail on an NFS mount.
These are already accepted gaps on the FUSE mount (`crates/cowfs-fuse/src/lib.rs:91,99`).
The NFS side has no test pinning the refusal, so the expected failure is undocumented there.
The macOS client errno mapping for NFS3ERR_NOTSUPP was not verified.

## (b) PATHCONF values

No override in cowfs-nfs (`git grep 'fn pathconf'` finds only the nfsserve default).
Default at `crates/nfsserve/src/vfs.rs:173-181`, copied verbatim by `nfs_handlers.rs:882-900`:
linkmax 65000, name_max 255, no_trunc true, chown_restricted true, case_insensitive false, case_preserving true.

## (c) Advertised value vs enforcement vs test

| Field | Advertised | Enforced by cowfs at main | Test | Status |
|---|---|---|---|---|
| name_max | 255 | Yes. NAME_MAX=255 at `cowfs-meta/src/types.rs:90`, checked at `tx.rs:682`, `read.rs:81`, `types.rs:447`. Adapter `new_name` (`adapter.rs:176-181`) returns NAMETOOLONG. | `adapter.rs:1156-1168`; `protocol.rs:485` | DONE |
| no_trunc | true | Behaviour matches: over-long names get NAMETOOLONG, not truncation (`adapter.rs:1165`). | Behaviour tested, flag value not | PARTIAL |
| linkmax | 65000 | No. Only a u32 overflow check (`cowfs-core/src/ns.rs:307`). No link cap in cowfs-meta. 65000 is the MemVfs test constant (`cowfs-vfs-test/src/memvfs.rs:13`). NFS maps TooManyLinks to MLINK (`cowfs-nfs/src/errors.rs:16`). | `protocol.rs:485` asserts linkmax > 0 only | MISSING |
| chown_restricted | true | Not checked. `cowfs-vfs/src/vfs.rs:29` says no permission enforcement, adapters check mode bits. | None | UNVERIFIED |
| case_insensitive | false | Not read. Grep for `case_insens` in core and meta finds nothing. Byte-exact comparison assumed. | None | UNTESTED |
| case_preserving | true | Same as above. | None | UNTESTED |

Failures this gap would cause:
- linkmax: a link-count case that expects EMLINK at the advertised ceiling never gets it. Heavy to run, so low priority. Mapping by pjdfstest category name only; contents not checked this run.
- chown_restricted: a chown case expecting EPERM for non-root chown would fail if cowfs allows it. Unverified.
- case_insensitive and case_preserving: only matter if the client probes them. No known pjdfstest assertion.

## Verdict

1. Typed create FIFO and socket on NFS: MISSING. Refusal is deliberate and matches FUSE, but no NFS test pins it.
2. Device nodes on NFS: refused via the same NOTSUPP path, no distinct handling, no test.
3. PATHCONF name_max 255 and no_trunc behaviour: DONE, enforced in meta and adapter.
4. PATHCONF linkmax 65000: MISSING. Advertised ceiling is not enforced; only u32 overflow.
5. chown_restricted, case_insensitive, case_preserving: nfsserve defaults, untested or unverified.
6. Existing tests (protocol.rs:25, 468, 485) do not cover these gaps; requirements19.rs has no typed-create or PATHCONF test.
7. Next: add NFS tests asserting NOTSUPP for each typed MKNOD; enforce linkmax or advertise the real ceiling with a test.
8. Not run: no mount, no pjdfstest, no cargo. pjdfstest mapping is by category name only.
