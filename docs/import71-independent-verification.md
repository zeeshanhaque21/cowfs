# Independent import verification

PR #71 was reviewed and tested from a separate `verify/import-71` lease.
The shared live cowfs daemon was not restarted or modified.

## Tests

`cargo test --locked -p cowfs-core --test import -j2` passed all 10 tests.
The CLI and daemon built successfully with `cargo build --locked -p cowfs-daemon -p cowfs-cli -j2`.

## CLI and real-mount sample

`scripts/verify-import71-smoke.py` starts its own Core-backed daemon, store and NFS mount under an exclusive private output directory.
The final run used `bench/out/import71-smoke-20261002c` in the primary project directory.
The output directory is mode 0700, as required for the control socket.
The initial attempt used an insufficiently private directory and was refused before import; that attempt was not a passing test.

The fixture has four regular files totaling 1,048,603 bytes, including a `.git/HEAD`, an empty file and a UTF-8 filename.
Its 1 MiB binary is a repeating byte sequence and is highly compressible, not a representative incompressible workload.
It also has an empty directory, a relative symlink and a dangling symlink.

The CLI reported matching source/imported BLAKE3 roots:

```text
d9d706f6a710d3aaee2fae45a65d24777b4b8f03815e4ac87df668b985a8224a
```

All file bytes were independently read back through the NFS mount and compared to the fixture.
Source regular-file bytes, modes, mtimes and ctimes remained unchanged against the pre-import baseline.
Symlink targets and the empty directory were preserved in the imported snapshot.
A duplicate snapshot name was refused with `already_exists`.
A FIFO was refused with `invalid_params`, and no snapshot with its requested name was published.
A second import under a new name reported zero additional stored bytes and the same root hash.
The first import reported 486 stored bytes for this deliberately compressible fixture.
No performance verdict is inferred from that figure.

The private daemon was terminated gracefully only after checking its PID's executable and socket identity.
The completed first successful run left no private daemon process or mount behind.
This is not a crash-injection test.

## Evidence limitation

Review found that the original `import-e2e.sh` preservation check captured its baseline after import and compared names only.
The worker corrected that harness at `21bab727d3d8fe083375778ffa7cdc5d5d974088`.
The reported 192 MiB workload and macOS/Linux crash/restart figures remain builder-reported, not independently rerun here.
Independent review confirmed the manifest is taken before imports and compared afterward.
The extracted manifest selftest independently passed: unchanged trees compare equal, while same-size content, mode, mtime, symlink-target and rename mutations are detected.
Source review confirmed the Core backend's `with_core` mutex guard spans the entire ingest and publication operation.
Only harness and documentation changed since the independently tested production code.
PR #71 was merged after confirming the exact reviewed head and all three green CI checks.
The existing NFS regression #57 remains a reliability blocker, and this import validation does not close full-stack crash or performance gates.
