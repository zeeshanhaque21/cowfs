# Clone vs cowfs on Linux, 2026-10-10 (WIP)

Status: work in progress.
Arms A (plain copy) and B (btrfs reflink) are done.
Arm C (cowfs FUSE) is running.
Arm D (overlayfs) is pending.

Host: cachyos, kernel 7.2.8-2-cachyos, x86_64 (not arm), 16 threads.
Filesystem: /mnt/docs is btrfs with compress=zstd:1.
Pinned commit: 65a551957134c361c70fd7c843ef265a2327dc7c.

Early numbers, all in seconds:

- Base build (cargo test --no-run --workspace): 36.4 s, 121 units.
- Arms A and B, R2 at a new path: 1 to 5 units rebuilt and 2 to 19 s per slot, so the build was incremental.
- Arm C, R2: 106 s for slot 1 (a one-crate edit).
- The first arm C attempt ran with the daemon in SCHED_IDLE, inherited from ananicy's sshd rule, and it was discarded.
