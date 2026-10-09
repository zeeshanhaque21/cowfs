# Issue 109: rmdir of a trailing `..` and nlink of an unlinked open file

Verdict: symptom 1 is fixed on main (server bug, fixed by ecc7022).
Symptom 2 is a macOS NFS client divergence, not a cowfs server bug.
No server change can alter symptom 2, so there is no code fix.
Symptom 3 does not reproduce.

Environment: macOS 26.6.2 (`sw_vers`), Darwin 25.6.0 arm64, `cowfs-daemon --backend core` release build of main 84bd305 plus the test-only change of this PR, private store and mount, pinned pjdfstest 85a8aea.
The daemon sha256 in each run's `identity.json` is `ccc766683151b98c79be43d049981bee6843e6c9269e25867d1beeffaa925397`, equal to `shasum -a 256` of the build's `target/release/cowfs-daemon`.
`cowfs_head` in `identity.json` is empty (see the harness deviation below), so the commit is established through that hash and the build, not through the field.

## Harness deviation (needed to rerun the receipt)

The leased worktree lives under the live cowfs NFS mount (`~/.cowfs/mnt/base/.treehouse/...`), so the daemon's control socket `repo/rt/c.sock` fails to bind there with EIO, and the daemon refuses a symlinked `rt`.
The harness was run as `python3 bench/pjdfstest.py --repo /private/tmp/cf109repo --out <primary>/bench/out/109/run ...`, where `/private/tmp/cf109repo` held a copy of the two release binaries, an empty `rt/` and a copy of the pinned tool checkout.
The harness code and cases are unchanged.
Evidence (gitignored): `bench/out/109/run/20261009T083356Z` (sample), `20261009T083955Z` (wide), `20261009T084130Z` (symptom 3).

## Reproduction

Sample, `rmdir/12.t,unlink/14.t`, run 20261009T083356Z:

| case | native APFS | cowfs over NFS |
| --- | --- | --- |
| `rmdir/12.t` | 6/6 ok | 6/6 ok |
| `unlink/14.t` | 7/7 ok | 6/7 ok, #4 `fstat 0 nlink` expected 0, got 1 |

Wide, groups `rmdir` and `unlink`, 31 cases, run 20261009T083955Z: per-case ok and not_ok counts equal native in 28 cases and differ in 3.
`rmdir/03.t` and `unlink/03.t` are the #108 pathconf class (0/5 and 0/4 against 5/5 and 4/4).
`unlink/14.t` is symptom 2 (6/7 against 7/7).
`rmdir/12.t` matches native.
Counts are not a per-assertion pairing; the harness reported 268 unpairable assertions (COVERAGE), which is why `unlink/14.t` #4 is read from its own TAP.

The same sequence by hand (open, unlink, fstat the open descriptor, list the directory):

```
APFS  : before nlink 1, after unlink nlink 0, listing [], stat f -> ENOENT
cowfs : before nlink 1, after unlink nlink 1, listing ['.nfs.20052acb.2b14'], stat f -> ENOENT, after close listing []
```

## Symptom 1: rmdir `a/b/..` answered EINVAL

Server bug, already fixed by ecc7022 (`rmdir of .. answers ENOTEMPTY, not EINVAL`).
Pinned at the protocol layer by `crates/cowfs-nfs/tests/rmdir_dotdot109.rs` and through a real mount by `rmdir_dotdot_and_open_unlinked_file_through_a_real_mount` in `crates/cowfs-nfs/tests/mount.rs` (ignored, needs mount_nfs; passes here).

## Symptom 2: nlink 1 after unlinking an open file

Client divergence.

Source: the macOS NFS client in apple-oss-distributions/NFS, `kext/nfs_vnops.c`, main at 93733ff (not necessarily the build shipped in macOS 26.6.2).
The comment on `nfs_vnop_remove` says: "a file that has other processes using the vnode is renamed instead of removed and then removed later on the last close."
The body computes `inuse = vnode_isinuse(vp, 0)` and, when the file is in use and not preserved, takes the branch `else if (!np->n_sillyrename) { ... error = nfs_sillyrename(dnp, np, cnp, ctx); }`, which is a RENAME RPC and not a REMOVE.

Observed: after `unlink(f)` the directory holds `.nfs.<id>` and not `f`, and it is gone after the descriptor is closed.
The server therefore sees a file with exactly one link, and 1 is the correct answer for it.
This is consistent with the source; the RPC sequence was not captured on the wire from the real client in this run, so the mechanism is source plus listing, not a trace.
NFSv3 has no open state, so once REMOVE does arrive the handle is STALE (RFC 1813) and the protocol has no way to report nlink 0 for a held file.
The same limitation is recorded in `crates/cowfs-vfs-test/src/lib.rs` (9 `Posix` checks fail through an NFS mount and pass over raw NFSv3).
The Core layer answers nlink 0 for an unlinked open file: `open_unlinked_data_is_pinned_until_released` in `crates/cowfs-core/tests/core.rs` asserts `getattr(ino).nlink == 0` after `unlink` with the handle open.

Pinned by:

- `silly_renamed_file_keeps_one_link_then_goes_stale_on_remove` in `crates/cowfs-nfs/tests/unlink_open_nlink109.rs`: raw NFSv3, the sequence the client's code implies (CREATE, RENAME to `.nfs.*`, GETATTR gives nlink 1, REMOVE, GETATTR gives STALE).
- `rmdir_dotdot_and_open_unlinked_file_through_a_real_mount` in `crates/cowfs-nfs/tests/mount.rs`: nlink 1 and a `.nfs*` name through the real client.

## Symptom 3 (timestamp-order checks)

`ftruncate/12.t` and `truncate/12.t` pass 3/3 on both arms in run 20261009T084130Z.
Not reproduced; not a part of the verdict.

## Prepared known_limits.rs entry

`crates/cowfs-nfs/tests/known_limits.rs` is owned by the #204 docs lane, so it is not edited here.
When that lane lands, add (the same test lives today in `unlink_open_nlink109.rs`; move it rather than duplicate it):

```rust
/// pjdfstest unlink/14.t #4 (#109): the macOS NFS client silly-renames an open unlinked file, so
/// the server reports nlink 1. See docs/verification/evidence/nfs109-unlink-open-nlink.md.
#[test]
#[ignore = "macOS NFS client silly rename: an open unlinked file keeps nlink 1 (RFC 1813: NFSv3 has no open state)"]
fn open_unlinked_file_reports_nlink_zero() { /* mount-based; expected 0, mount gives 1 */ }
```

## Accepted divergence for the g3 harness

`unlink/14.t` #4, `open O_RDONLY : unlink : fstat 0 nlink`, expected 0, got 1, is a macOS NFS client divergence.
The g3 harness owner should add it to the accepted-divergence data file with this document as the evidence.
