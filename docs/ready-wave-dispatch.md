# Ready-task wave, 2026-10-04

All eleven ready tasks have dedicated treehouse leases under `.treehouse-ready-wave/.treehouse/cowfs-7c1bf8/`.
The base is `46b0f269d5bef4a2c204c25f5b3015da601d3beb`.
Existing build-train and mounted leases remain held and untouched.

| Slot | Task | Owned implementation surface |
| --- | --- | --- |
| 1 | #19 server requirements | NFS requirement reconciliation; narrow vendored fixes, not Translate/security |
| 2 | #20 open-FD holders | Treehouse holder detection and private-slot reset refusal; not import/refresh |
| 3 | #21 Git-index integrity | Private Git/NFS integrity reproduction and scoped regression; coordinate source changes |
| 4 | #15/#16 real project | Acceptance fixtures and cache-hook evidence; no production source changes without diagnosis |
| 5 | #40 metadata health | Metadata recovery counters and health reporting |
| 6 | #42 Core/meta requests | Reconcile atomic snapshot rename, names, holes, reservations and batch_at |
| 7 | #43 Translate/security | NFS Translate, authorization and owned dead-server behavior |
| 8 | #45 FUSE torn read | Linux FUSE reader/writer reproduction and responsible fix |
| 9 | g3 | Matched pjdfstest acceptance and gate-specific harness fixes |
| 10 | g4 | Matched fsx acceptance and gate-specific harness fixes |
| 11 | g5 | Isolated Linux xfstests acceptance and gate-specific harness fixes |

## Shared constraints

Use `opencode/space-bunny-free` for every worker and later independent reviewer.
Read `docs/design.md`, current source and the linked issue before implementing.
Work only in the assigned lease and report exact source ownership when a residual defect overlaps another lane.
Do not edit the primary tracker or this coordination document.
Do not merge, return leases, reset, stash, prune, destroy or discard another worker's artifacts.
Push logical WIP commits over authenticated HTTPS immediately and run ship-aftercare.
No co-author trailer, CHANGELOG editing, workflow dispatch, rerun or runner configuration.
Independent review and exact-head green CI are required before merge.

Shared daemon PID 15263 is on the local Mac, not moonscape or another Linux host.
Its unchanged start time is `Sat Oct 3 20:44:29 2026`, store is `/Users/zeeshanhaque/.cowfs/store`, mount is `/Users/zeeshanhaque/.cowfs/mnt`, and socket is `/Users/zeeshanhaque/.cowfs/sock/daemon.sock`.
The PID, store, mounted pools and sockets are not test fixtures.
An absent PID on a different host says nothing about this daemon; verify process identity on the correct host without traversing its mount.
Never restart, upgrade, signal, reset or garbage-collect them.
Existing workers own shutdown ctl code, crash-evidence scripts, namespace durability and import/refresh #97 in the original build train.
In particular, do not alter `crates/cowfs-daemon/src/import.rs`, the ongoing namespace-barrier implementation or the shutdown server to fix an unrelated acceptance lane.
Report those dependencies explicitly.
Original namespace builder slot 10 also owns issue #98's narrow `Snapshots`/Path/Core backend warm-base provenance setter, persistence and status reconstruction.
The ready #42 worker owns other Core/meta API residuals, not this repository/ref/commit metadata-publication seam.
Real-project acceptance must require discoverable published-base metadata; issue #98 is a dependency, not permission to accept imported artifacts as a warm base.

The Mac had approximately 36.7 GiB free at launch and now has 261.22 GiB free, verified after the user's capacity update.
The launch-time 1 GiB cap is lifted; each worker may grow owned build/test artifacts up to 8 GiB before requesting a capacity review.
Use scoped tests, no full-workspace build, stress suite or corpus copy.
No large dependency/model downloads or broad cache deletion.
Check free disk before a build and stop if below 20 GiB.
Serialize heavy local builds and mounted workload runs across this wave using the lock recipe below.
Discovery, editing and small unit fixtures can proceed concurrently.
Do not share mutable Cargo targets between leases.
Do not claim quiet performance acceptance while other workers are active.

## Resource lock recipe

Run the complete build or mounted workload as one foreground invocation through this command.
Substitute the owned command argv after the lock path.
The parent must remain alive until its child exits.
A lock wait failure is a blocker, not permission to run without the lock.

```sh
rtk proxy python3 -c 'import fcntl,os,subprocess,sys,time; p=sys.argv[1]; os.makedirs(os.path.dirname(p),exist_ok=True); f=open(p,"a"); until=time.monotonic()+600
while True:
 try: fcntl.flock(f,fcntl.LOCK_EX|fcntl.LOCK_NB); break
 except BlockingIOError:
  if time.monotonic()>until: print("resource lane busy; blocked",file=sys.stderr); sys.exit(75)
  time.sleep(.25)
sys.exit(subprocess.run(sys.argv[2:]).returncode)' /Users/zeeshanhaque/Projects/cowfs/.treehouse-ready-wave/mac-heavy.lock COMMAND ARGS
```

For Linux workloads use the same Python recipe remotely with lock path `/home/moonscape/cowfs-ready-wave/linux-heavy.lock`.
Keep each worker's stores, sockets, mounts and artifacts under `/home/moonscape/cowfs-ready-wave/task-ID/` and copy small proof back into its assigned lease.
Use a real existing unprivileged capability only; no sudo, sysctl, installation, shared-device format, global FUSE setup or changes to existing remote mounts.
If the tool or required capability is absent, report UNMEASURABLE with the exact reason and finish an actionable scoped delivery rather than guessing a pass.

## Proof and safety

Validate one representative real deliverable end to end before any batch.
Always include a matched native or do-nothing baseline, exact executed/skipped counts and source/binary identity.
Do not treat a compiled binary or surviving artifact as a successful command.
Capture real exit codes directly, never `$?` from head, tail or tee pipelines.
Real-project refresh acceptance requires published warm-base postconditions, not imported artifacts.
Path-backend readback does not imply Core fsck or crash durability.
Process SIGKILL does not establish power-loss behavior.

Before every signal, unmount, deletion or overwrite verify exact owned PID, argv, start time, store, socket and mount path, and flush evidence.
Track every private daemon and reopen daemon individually.
No pkill or process-group signals.
Read environment-traps before launching daemons and use isolated sessions.
Use bounded foreground waits that exit on failure and have minute-scale no-progress limits.
Never walk or delete a stale mount; verify the native mount table first.
macOS umount has no `-z` flag.
Keep immutable per-attempt paths and preserve unknown or failed fixtures.
Append and flush logs per case so interruption does not destroy all results.

Each worker writes `docs/verification/ready-TASK.md` and raw ignored `bench/out/ready-TASK/**` inside its own lease.
Commit a small readable sanitized proof when useful, not private stores, binaries or entire raw logs.
Return task status, full SHA and PR if changed, real tests and exit codes, actual remaining dependencies and evidence path.
An umbrella reconciliation may finish with no code if every requested feature already exists, but it needs runnable proof rather than an unsupported closure claim.
