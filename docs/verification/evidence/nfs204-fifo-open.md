# Issue 204: a fifo cannot be opened on the macOS NFS mount

Verdict: a macOS NFS client limitation, not a cowfs server bug.
No server change that keeps the node a fifo can alter the result, so there is no code fix.
Environment: macOS 26.6.2 (Darwin 25.6.0), cowfs-daemon built from 560013c plus temporary scratch edits (below), nfs adapter, `mount_nfs` options `locallocks,nodev,nosuid,vers=3,tcp`, private store and mount under bench/out/nfs204.
The scratch edits were an `eprintln` of each NFS procedure and of each ACCESS request and answer in nfsserve, and in one run a refusal of every CREATE of a `._` name in the adapter.
They were reverted and are not part of any commit.
The NFS source files in crates/nfsserve, crates/cowfs-nfs and crates/cowfs-daemon are identical between 560013c and main 9c32468 (`git diff 9c32468 560013c` over them is empty).

## Reproduction

1. Start the daemon on a private store and mount, then `cowfs snapshot create s1`.
2. In `mnt/s1`: `mkfifo f` succeeds. `ls -ln f` gives `prw-r--r-- 1 501 20`, the same type, mode, uid and gid as a native APFS fifo.
3. `open(f)` with O_RDONLY|O_NONBLOCK, O_WRONLY|O_NONBLOCK and O_RDWR all fail with EACCES.
4. The same three opens on a fifo in /tmp (APFS) give ok, ENXIO, ok.

## Cause (source-backed)

The macOS NFS client source is published in the apple-oss-distributions/NFS repository (kext/nfs_vnops.c, main at 93733ff), not in the xnu repository.
`nfs_vnop_open` contains, before any RPC:

```
vtype = vnode_vtype(vp);
if ((vtype != VREG) && (vtype != VDIR) && (vtype != VLNK)) {
    error = EACCES;
    goto out_return;
}
```

A fifo vnode is meant to get the `fifo_nfsv2nodeop_p` table, whose open is the kernel's `fifo_open` and whose pathconf is `fifo_pathconf`, but `nfs_nget` (kext/nfs_node.c) selects that table only inside `#if FIFO`.
`FIFO` is a kernel configuration option (`options FIFO` in xnu config/MASTER) and the NFS project file (NFS.xcodeproj/project.pbxproj) does not define it.
Then a fifo gets the regular NFS vnode ops and `nfs_vnop_open` refuses it with EACCES.
The source quoted is the repository main at 93733ff, not necessarily the build shipped in macOS 26.6.2.
The regular-ops claim is confirmed by behaviour on this machine: `pathconf` on the mount fifo returns exactly what it returns on a regular file (PC_NAME_MAX 255, PC_LINK_MAX 65000, PC_CHOWN_RESTRICTED 200112, PC_PIPE_BUF EINVAL on both), whereas the fifo table would route pathconf to `fifo_pathconf`.
That the cause of the missing table is an undefined `FIFO` in the shipped build is still an inference; the type check in `nfs_vnop_open` is the quoted code.

## Observations that match this

1. The server never sees the open.
   A scratch build printed every NFS procedure.
   After MKNOD, GETATTR and LOOKUP, an open of the fifo added zero RPCs, including no ACCESS.
   An open of a regular file in the same directory sent one ACCESS (requested 0x3f, granted 0xd).
   So the ACCESS reply bits and the fattr3 of a fifo are not what is judged.
2. The mode is irrelevant.
   With chmod 600, 666, 777 and 000 the result is EACCES every time.
   access(2) with R_OK and W_OK returns true.
3. O_EVTONLY, O_SYMLINK and O_EXEC give EACCES as well.
4. `xattr -l f` on the mount fifo gives EACCES too (observed, same refusal class, not investigated further).
5. The opens run with the Bash sandbox disabled give EACCES.
   The creating `mkfifo` ran sandboxed in the first run, so a second run created `g` through the unsandboxed path as well; it also gave EACCES.
6. Control for the `com.apple.provenance` attribute.
   The mount fifo `f` carried that attribute (set through the AppleDouble `._f` sidecar: CREATE, WRITE, COMMIT and SETATTR follow MKNOD), as does the /tmp fifo that opens.
   A scratch build that refused every CREATE of a `._` name produced a fifo `g` with no extended attribute at all (`ls -ln@` shows no `@`), and its opens still gave EACCES.
   So the attribute and the sidecar path are not the cause.

## Other causes ruled out

1. Not nodev and nosuid: an HFS+ image mounted nodev,nosuid opens its fifo (issue 107 evidence).
2. Not the MKNOD reply state: restarting the daemon on the same store still fails (issue 107 evidence).
3. Not the ACCESS handler: `access_granted` would grant read and modify bits for a mode 0644 fifo, and it is never called for the open.

## What could not be tried

1. Manual `mount_nfs` of the same export with other options failed with Permission denied, because MNT is answered once for the daemon's own mount.
   The source reading above says no option changes the type check.
2. A second NFS server (macOS nfsd, or a Linux nfsd) to compare the same fifo needs root, so it was skipped.
3. dtrace and fs_usage need root.

## What remains

1. pjdfstest open/17.t #2 (ENXIO expected, EACCES got) stays an established difference on the macOS NFS mount.
2. Fifo creation and stat work on the mount; opening one does not, so data cannot be exchanged through a fifo there.
   Rename and unlink of a fifo were not exercised in this investigation.
3. docs/special-files-107.md and the README should state that fifos on the macOS NFS mount can be created but not opened.
