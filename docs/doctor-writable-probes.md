# Doctor probes the wrong directory

## Reproduction

On the live Core-backed NFS mount, run:

```sh
cowfs-treehouse doctor --mount /Users/zeeshanhaque/.cowfs/mnt --pool-root /Users/zeeshanhaque/.cowfs/mnt/base --socket /Users/zeeshanhaque/.cowfs/sock/daemon.sock --timeout 5 --json
```

The command exits 1.
Mount identity, snapshot visibility and pool containment pass.
Lock and hardlink checks fail because they create files directly under the read-only snapshot namespace.
The configured pool root is a writable directory in the `base` snapshot.

## Fix

Probe the supplied pool root instead of the namespace root.
Retain the mount-directory fallback when no pool root is supplied.
Canonicalize the probe directory and mount, and refuse missing directories or paths outside the mount before writing probe files.
This rejects existing symlink escapes, but does not eliminate a concurrent symlink-swap race.
Report the actual probe directory and result rather than checking whether the already-cleaned lock file still exists.

## Validation

Regression tests cover pool-root selection, the no-pool fallback, outside roots, missing roots, and a symlink escaping the mount.
The unfixed live command above is the end-to-end baseline.
`cargo test --locked -p cowfs-treehouse -j2` passed all 115 tests.
`cargo clippy --locked -p cowfs-treehouse --all-targets -j2 -- -D warnings` passed.
The fixed live command exits 0: all seven report checks pass, including flock and hardlinks in the configured `base` pool root.
The holder check reports that no slot-backed snapshot was scanned; it does not prove active agents have no open files.
No daemon restart or existing lease modification was needed.

Closes #69.
