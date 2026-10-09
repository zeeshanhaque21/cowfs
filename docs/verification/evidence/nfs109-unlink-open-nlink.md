# Issue 109: rmdir of a trailing `..` and nlink of an unlinked open file

Verdict: symptom 1 is fixed on main (server bug, fixed by ecc7022).
Symptom 2 is a macOS NFS client divergence, not a cowfs server bug.
There is no server change that can alter symptom 2, so there is no code fix.
Environment: macOS 26.6.2 (Darwin 25.6.0), cowfs-daemon release build of main 84bd305, `--backend core`, private store and mount, pinned pjdfstest 85a8aea.

## Reproduction (bench/pjdfstest.py, cases rmdir/12.t and unlink/14.t, run 20261009T083356Z)

| case | native APFS | cowfs over NFS |
| --- | --- | --- |
| `rmdir/12.t` | 6/6 ok | 6/6 ok |
| `unlink/14.t` | 7/7 ok | 6/7 ok, #4 `fstat 0 nlink` expected 0, got 1 |

The same sequence by hand (open, unlink, fstat the open descriptor, list the directory):

```
APFS  : before nlink 1, after unlink nlink 0, listing [], stat f -> ENOENT
cowfs : before nlink 1, after unlink nlink 1, listing ['.nfs.20052acb.2b14'], stat f -> ENOENT, after close listing []
```

## Symptom 1: rmdir `a/b/..` answered EINVAL

Server bug, already fixed by ecc7022 (`rmdir of .. answers ENOTEMPTY, not EINVAL`).
`rmdir/12.t` passes 6/6 on the mount above.
Pinned at the protocol layer by `crates/cowfs-nfs/tests/rmdir_dotdot109.rs` and through a real mount by `rmdir_dotdot_and_open_unlinked_file_through_a_real_mount` in `crates/cowfs-nfs/tests/mount.rs`.

## Symptom 2: nlink 1 after unlinking an open file

Client divergence.
After `unlink(f)` on the mount, the directory holds `.nfs.<id>` and not `f`.
That is the NFS client's silly rename: the client does not send REMOVE for a file it has open, it sends RENAME to a `.nfs.*` name and removes that name on the last close.
The server therefore sees a file with exactly one link, and 1 is the correct answer for it.
After the descriptor is closed the `.nfs.*` entry is gone, so the file is reclaimed.
NFSv3 has no open state, so once REMOVE does arrive the handle is STALE (RFC 1813) and the protocol has no way to report nlink 0 for a held file.
The same limitation is already recorded in `crates/cowfs-vfs-test/src/lib.rs` (9 `Posix` checks fail through an NFS mount and pass over raw NFSv3).

Pinned by:

- `silly_renamed_file_keeps_one_link_then_goes_stale_on_remove` in `crates/cowfs-nfs/tests/known_limits.rs`: raw NFSv3, the client's RPC sequence (CREATE, RENAME to `.nfs.*`, GETATTR gives nlink 1, REMOVE, GETATTR gives STALE).
- `rmdir_dotdot_and_open_unlinked_file_through_a_real_mount` in `crates/cowfs-nfs/tests/mount.rs` (ignored, needs mount_nfs): nlink 1 and a `.nfs*` name through the real client.

The Core layer already answers nlink 0 for an unlinked open file (`open_unlinked_data_is_pinned_until_released` in `crates/cowfs-core/tests/core.rs`).

## Symptom 3 (timestamp-order checks)

`ftruncate/12.t` and `truncate/12.t` pass 3/3 on both arms in the 2026-10-09 g3 re-run (docs/reviews/g3-status-20261009b.md).
Not reproduced, not a part of this verdict.

## Accepted divergence for the g3 harness

`unlink/14.t` #4, `open O_RDONLY : unlink : fstat 0 nlink`, expected 0, got 1, is a macOS NFS client divergence.
The g3 harness owner should add it to the accepted-divergence data file with this document as the evidence.
