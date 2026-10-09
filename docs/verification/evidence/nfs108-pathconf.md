# Issue 108: PATH_MAX cannot be read on the macOS NFS mount

Verdict: a macOS NFS client limitation, not a cowfs server bug.
No server change can alter the result, so there is no code fix and the PR for this issue is documentation only.
The issue title says "pathconf answers -1"; the precise fact is narrower: `_PC_NAME_MAX` answers 255 and only `_PC_PATH_MAX` (and `_PC_PIPE_BUF`) answers -1 with EINVAL.
Environment: macOS 26.6.2 (Darwin 25.6.0), cowfs source 6914f6c (main after PR 212) with the NFS source files unchanged except the temporary scratch edit below, nfs adapter, `cowfs-daemon --backend core`, private store and mount.
The scratch edit was an `eprintln` of each NFS procedure and of the PATHCONF reply in nfsserve.
It was reverted and is not part of any commit.

## Reproduction

1. Start a private daemon, create snapshot `s1`, and call `pathconf(path, name)` for six names on the mount and on APFS.
2. Probe program: a 12-line C file that prints `pathconf` and `errno` per name.

| name | cowfs mount (`.`, a dir, a file: identical) | OrbStack NFS mount (another server) | APFS `/tmp` |
| --- | --- | --- | --- |
| `_PC_LINK_MAX` | 65000 | 255 | 32767 |
| `_PC_NAME_MAX` | 255 | 255 | 255 |
| `_PC_PATH_MAX` | -1, EINVAL | -1, EINVAL | 1024 |
| `_PC_PIPE_BUF` | -1, EINVAL | -1, EINVAL | 512 |
| `_PC_CHOWN_RESTRICTED` | 200112 | 200112 | 200112 |
| `_PC_NO_TRUNC` | 200112 | 200112 | 200112 |

The OrbStack row is a control: a different NFS server on the same client gives the same -1 for PATH_MAX and PIPE_BUF.

## What the server sends

The scratch log shows the client's whole conversation about this.
At mount time it sends FSINFO then PATHCONF once.
The PATHCONF reply, decoded: `linkmax 65000, name_max 255, no_trunc true, chown_restricted true, case_insensitive false, case_preserving true`.
That is every field NFSv3 PATHCONF3resok carries (RFC 1813 section 3.3.20).
It has no path_max and no pipe_buf field, so there is nothing a server could add.
After that, repeated `pathconf` calls (all six names, three paths) sent zero RPCs.
The FSINFO reply sets `FSF_HOMOGENEOUS`, so the client caches the one PATHCONF answer for the whole mount.

## Cause (source-backed)

`nfs_vnop_pathconf` in apple-oss-distributions/NFS `kext/nfs_vnops.c` (main at 93733ff, line 7686) switches on the name:

```
case _PC_LINK_MAX: case _PC_NAME_MAX: case _PC_CHOWN_RESTRICTED:
case _PC_NO_TRUNC: case _PC_CASE_SENSITIVE: case _PC_CASE_PRESERVING: break;
case _PC_FILESIZEBITS: ... return 0;
case _PC_XATTR_SIZE_BITS: ...
default:
    /* don't bother contacting the server if we know the answer */
    error = EINVAL;
```

`_PC_PATH_MAX` is not a case, so it falls to `default` and returns EINVAL without contacting the server.
The source quoted is the repository main, not necessarily the build shipped in macOS 26.6.2, but the behaviour above (no RPC, same answer from a second server) matches it.

## What the suite does with it

pjdfstest `tests/misc.sh` `dirgen_max` runs `pathconf . _PC_PATH_MAX`, gets -1, and builds an empty path.
All 13 affected scripts contain `dirgen_max` (checked by counting it in each file; the 63 to 66 failing rows of the issue).
The NAME_MAX read itself works: it is the first line of `dirgen_max`, and the failure text names only the PATH_MAX call.
The harness run `bench/pjdfstest.py --repo /tmp/n108repo --out /tmp/n108r --tests chmod/03.t,rename/02.t` reproduced the rows: native 5 of 5 and 6 of 6, cowfs 0 of 5 and 1 of 6, with stderr `pathconf returned -1`.
`--repo` pointed at a scratch directory holding only a `target` symlink, because this lease path sits on a live cowfs NFS mount and a unix socket cannot be created there, so the identity receipt has an empty `cowfs_head`.
The binaries are a clean release build of 6914f6c (`git status` empty before and after): `cowfs-daemon` sha256 `93af11e8ab2334ebd20610d3cfc9b361bd12f2c154edc6dbb45cf092e065f4e4`, `cowfs` sha256 `4881352deb885abbc9ea7118182cbce9e7eba7c3cd9894665e1308a20ce045bd`.

## Proof that the server is fine

A scratch copy of the pinned suite (`85a8aea`) with one line changed in `misc.sh` (`path_max=1024` instead of reading `_PC_PATH_MAX`, 1024 being the value APFS reports) was run on the 13 cases in a fresh directory on APFS and on the cowfs mount.
Counts of `ok` and `not ok` per case:

| case | native ok / not ok | cowfs ok / not ok |
| --- | --- | --- |
| chmod/03 | 5 / 0 | 5 / 0 |
| chown/03 | 6 / 4 | 6 / 4 |
| ftruncate/03 | 5 / 0 | 5 / 0 |
| link/03 | 13 / 0 | 13 / 0 |
| mkdir/03 | 3 / 0 | 3 / 0 |
| mkfifo/03 | 4 / 0 | 4 / 0 |
| mknod/03 | 4 / 8 | 4 / 8 |
| open/03 | 4 / 0 | 4 / 0 |
| rename/02 | 6 / 0 | 6 / 0 |
| rmdir/03 | 5 / 0 | 5 / 0 |
| symlink/03 | 6 / 0 | 6 / 0 |
| truncate/03 | 5 / 0 | 5 / 0 |
| unlink/03 | 4 / 0 | 4 / 0 |

All 13 cases match native exactly once the suite is given a PATH_MAX (chown/03 and mknod/03 fail the same way on both arms for non-root reasons).
So the PATH_MAX rows hide no cowfs difference.
Which side raises ENAMETOOLONG for an over-long path was not captured (the RPC logging build had been reverted before this run), so it is not claimed here.

## Receipts

Gitignored, in the primary checkout under `bench/out/nfs108/`: `harness-run/` (the harness stamp `20261009T090222Z`), `daemon3.log` (the RPC log with the decoded PATHCONF reply), `pc.c` (the probe: `pathconf` plus `errno` for six names), `run13.sh` (the 13-case loop), `misc.sh.patched` (the suite file with `path_max=1024`).

## What remains

1. The 13 cases cannot run as written on any macOS NFS mount, because the client refuses `_PC_PATH_MAX`.
2. Supplying PATH_MAX from the native arm in the g3 harness, and rewording the accepted-divergence entry for issue 108 to "client answers EINVAL for `_PC_PATH_MAX`; NAME_MAX works", are tracked in issue 218 (harness and divergence data are owned elsewhere and not touched here).
