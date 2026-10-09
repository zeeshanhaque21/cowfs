# Live treehouse trial

The core-backed NFS daemon is running on this Mac.
This is a development trial, not a production-readiness claim.

## Paths

- Mount: `/Users/zeeshanhaque/.cowfs/mnt`.
- Writable snapshot: `/Users/zeeshanhaque/.cowfs/mnt/base`.
- New pools: `/Users/zeeshanhaque/.cowfs/mnt/base/.treehouse`.
- Store: `/Users/zeeshanhaque/.cowfs/store`.
- Control socket: `/Users/zeeshanhaque/.cowfs/sock/daemon.sock`.
- Daemon PID at validation: `11068`.
- Daemon log: `/Users/zeeshanhaque/.cowfs/daemon.log`.

The mount root is a read-only namespace of snapshots.
Create files inside `base`, not directly at the mount root.
The global treehouse configuration now selects `base` and disables APFS sharing.
Amicable's repo-local `root = ".."` overrides that default, so use the explicit launcher below.

## New lumen and amicable work

Run from the project's existing main checkout:

```sh
sh /Users/zeeshanhaque/Projects/cowfs/scripts/treehouse-cowfs.sh get --lease --json --no-fetch
```

Then work in the path returned by treehouse.
The launcher explicitly selects the cowfs root, disables APFS sharing, checks the NFS mount, and checks the control daemon before invoking treehouse.
It refuses an absent mount instead of silently provisioning on the underlying native directory.
`COWFS_HOME` defaults to `$HOME/.cowfs`; `COWFS_MOUNT` defaults to `$COWFS_HOME/mnt` and `COWFS_SOCKET` to `$COWFS_HOME/sock/daemon.sock`.
`COWFS_BIN` defaults to the stable `cowfs` copy under `spikes/nfs-loopback/out/live/bin` in the project checkout, and `TREEHOUSE_BIN` to `treehouse` on `PATH` (else `$HOME/.local/bin/treehouse`).
With no arguments, `-h` or `--help` the launcher prints usage without touching the mount.
Existing worktrees and runners have not been moved or restarted.
Main checkouts and their shared Git object databases remain native unless separately cloned or imported onto cowfs.
The smoke main checkout, including its Git database, is on cowfs at `base/repos/cowfs`.

Use the same launcher for `status` and for returning a newly acquired cowfs lease.
For old native leases, pass their original `--root` to the ordinary treehouse binary.
Do not use the new cowfs root to return old native leases.
Do not return any slot while its agent or build is still using it.

The existing Cargo cache-seeding hook has not been validated for lumen or amicable on NFS.
Neither project's full build has been validated on this mount yet.
The launcher does not migrate dependencies, ignored files, credentials, or in-flight work.

## Validation on 2026-10-02

Validated daemon build: main merge `aa1f3d0`.
A fresh treehouse lease was acquired at `base/.treehouse/cowfs-b2142b/1/cowfs` with holder `cowfs-mount-smoke`.
The acquisition succeeded through ordinary treehouse with APFS sharing disabled.
`cargo test -p cowfs-vfs --locked -j2` passed four tests across two suites with build artifacts stored on cowfs.
`git fsck --full` passed.
A 1 MiB fixture passed fsync/readback, hardlink identity and readback, symlink readback, mmap modification and flush, rename readback, and cross-process exclusive-flock contention and release.
Its final SHA-256 was `440c640d64dcd3b8750f24f071291719cc30c29b7ef913a0fd456716639bb98c`.
The fixture remains under the smoke lease's ignored `target/mount-smoke-2d46795a33ad41748e96cdbed2ea9644` directory.
The smoke runner is `spikes/nfs-loopback/out/treehouse-live-smoke.py`.

The existing `cowfs-treehouse doctor` incorrectly probes locks and hardlinks at the read-only namespace root and reports failures there.
The direct tests above instead probe the writable snapshot.
No throughput ratio, full-project compatibility, restart recovery, or production durability claim follows from this small sample.

## Lifetime and rollback

The daemon is detached, but is not installed as a login or reboot service.
Its currently running executable comes from the native `verify-main` slot 7.
Do not recycle that slot as part of this setup.
Stable copies of the CLI and daemon are under `spikes/nfs-loopback/out/live/bin` for later supervised startup.
Do not restart or unmount while agents are using cowfs leases.

To route future ordinary treehouse calls back to native storage, remove only the `root` entry from `~/.config/treehouse/config.toml`, preserving unrelated changes.
Use the explicit cowfs launcher to manage any remaining cowfs leases.
The earlier configuration backup remains in `~/.config/treehouse/config.toml.bak-*`.
Do not replace the whole configuration blindly from a backup.
