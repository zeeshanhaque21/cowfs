# Readiness: gates g1 and g2 (cargo build, git status) within 1.5x of native

Status: READINESS ONLY.
No g1 or g2 gate number exists in this document.
The host was busy for the whole session, so no timed measurement was presented as a result.
Tracker bar (changed 2026-10-09): native APFS (this Mac) plus a Linux native filesystem on the cachyos box, cargo build and git status each within 1.5x of native.
Moonscape is dropped from this gate.
The cachyos native filesystem is btrfs, so the Linux number is a btrfs number, not ext4.

## Name mapping

Tracker g1 is harness gates `g1` (clean build) plus `g2` (edit and rebuild) in `bench/gates.py`.
Tracker g2 is harness gate `g3` (git status).
`bench/compare.py` bars: g1 and g3 ratio at most 1.5x on every platform.
Harness g2 is at most 1.5x off macOS, and on macOS the added seconds must stay under 1.0 s (issue #18 amendment, not the tracker's 1.5x).
The driver prints both, so the tracker wording and the macOS budget are both visible.

## Why the earlier live trial proves nothing

`docs/live-trial-metrics.md` and its review recorded 17x to 144x slower than native in every repetition.
That daemon was a debug build, shared with other agents, on a host with load1 10 to 24 and no idle baseline.
A valid run needs all of: a private release daemon and store, a validated small end-to-end sample, a quiet host, and native-native controls.

## The workload

- Project: this repo pinned at `c1619ec16df3a6b11dd5a1e08e8a512b4fedd240` (`DEFAULT_SHA` in `bench/gates.py`), the cowfs Rust workspace, cloned `--no-hardlinks` into each arm.
- Tree for git status: generated, 100,000 files of 256 bytes in 256 directories plus 64 files of 8 MiB, committed in a fresh git repo (`ensure_tree`).
  This is a synthetic tree, not a large real tracked repo.
- Shared `CARGO_HOME` at `bench/out/cargo-home` on APFS, outside every arm, filled once by an untimed `cargo fetch --locked` (needs network on the first run).
- `CARGO_TARGET_DIR` is `<root>/corpus-target` inside the arm.
  `CARGO_INCREMENTAL`, `RUSTC_WRAPPER` and `RUSTFLAGS` are removed from the environment.

## Exact command lines the harness times

Clean build (harness g1), after `rm -rf corpus-target`:

    cargo build --offline --locked -j 4

Warm no-op rebuild is the same command again with nothing changed.
`bench/gates.py` does not time it.
Only `scripts/measure-live-trial.py` does, and its cowfs arm path is tied to where the repo lives, so it is not used here.

Edit rebuild (harness g2): append `// cowfs bench g2 edit <ns>` to `crates/cowfs-vfs-path/src/cookies.rs`, then:

    cargo build --offline --locked -j 4

Git status (harness g3), warm then after touching 1 percent of `d000`:

    git -c core.fsmonitor=false -C <root>/tree status --porcelain

## Private release daemon: build and launch

Build, once, in a leased worktree, never two cargo builds at once:

    cargo build --release --locked -p cowfs-daemon -p cowfs-cli -j4

Built by hand on 2026-10-08 at `43fe43cb54c96b4ebfdbe2c81c7e0083a15f9336`, 52 s (a later driver build at a different head will have a different digest, and that is expected):

- `target/release/cowfs-daemon` sha256 `c088fc0826dce7fc77d1003a43ecd3c41ca66c961e3f1b510ff24e39f1b74708`, 8,012,880 bytes.
- `target/release/cowfs` sha256 `1915c95e4203c0c2d2db124b64816af89e79c14796719de07da278808e184134`.
- The path is in the lease `.treehouse-ci/.treehouse/cowfs-7c1bf8/6/cowfs`, so rebuild if that lease is returned.

The driver builds with `cargo build --release --locked -p cowfs-daemon -p cowfs-cli -j 4 --message-format=json` itself.
It refuses any artifact whose reported profile has `opt_level` 0, unknown or on `debug_assertions`, or whose executable is not under a `release` directory.
The sha256 of the daemon is recorded at build, re-compared before the launch and before and after every cowfs arm, and the running argv must start with that binary.

Launch, done by the driver (own store, mount and socket, never the shared daemon 15263):

    target/release/cowfs-daemon --store S --mount M --socket ~/.cowfs/sock/g12-ID.sock --backend core
    target/release/cowfs --socket ~/.cowfs/sock/g12-ID.sock snapshot create base

The cowfs arm root is `M/base/g12`.
The socket lives under `~/.cowfs/sock` because AF_UNIX paths are limited to 104 bytes.
Stop is SIGTERM after checking the pid's argv, then the mount must be gone (`mount | grep cowfs`).

## Native-native control

The driver runs `bench/gates.py` four times in the `bench/run-pair.sh` order: native1, cowfs1, native2, cowfs2.
Native root is `bench/out/g12/ID/native` on APFS.
The native-native median ratio (native2 over native1) per gate must be within 1 +/- 0.10.
`compare.py` is run twice: native1 vs cowfs1 with native2 as noise floor, native2 vs cowfs2 with native1 as noise floor.
The CPU lock `spikes/nfs-loopback/out/cpu.lock` (same as `run-pair.sh`) is held for the whole run.

## Quiet-host gate

Measured baseline: NONE.
The host was never idle while I looked, so no idle baseline exists and none is claimed.
What I saw on this Mac (16 logical cores, `uptime`): load averages 6.18 7.65 14.54 at 17:34, 20.49 at 17:35, 15.5 at 17:36, 139.5 at 17:48, 85.8 during the sample.
`top -l 2` CPU idle was 54 percent once and a median of 13 percent in the sample baseline.
Other agents were running cargo, rustc and daemons the whole time.

The gate is defined from a baseline taken at the start of each real run, by command:

    python3 bench/g12_run.py --run-id ID

- After the release build the driver waits for a load1 plateau: 7 polls 10 s apart, all at or below the cap (4.0) and spread under 0.5 (the 0.5 is chosen), at most 600 s, else INVALID with the reason, so the build's decaying load never enters the baseline.
- Baseline window: 300 s, sampled every 5 s: load1, foreign CPU, plus `top -l N -s 5 -n 0` CPU idle (first top sample dropped, it is since boot).
- Foreign CPU is the sum of `ps` pcpu of every process outside the driver's own process tree and the daemon's tree (the arm's build, the mount server and the driver itself are own).
  `ps` pcpu is a decaying average, a heuristic, so it is only ever compared with the same metric from the baseline, never read as absolute usage.
- Baseline is refused if p95 load1 exceeds 4.0 (25 percent of 16 cores), if p95 foreign CPU exceeds 100 points (one core), if any foreign cargo, rustc, cc1 or ld process runs, or if there are no CPU samples.
- Derivation: the limits are the measured baseline plus a stated margin, and the margins are chosen, not measured.
  - load1 limit = baseline p95 load1 + 1.0, capped at 4.0.
  - Foreign CPU limit = baseline p95 foreign CPU + 50 points (half a core).
  - Idle floor = baseline median CPU idle - 10 points.
- CPU of `kernel_task` (the NFS client), `mds`, `mds_stores`, `mdworker_shared` and `fseventsd` is induced by the arm's own file I/O: it is recorded per arm as `induced_max` but not gated.
  That set is chosen, not measured. Spotlight indexing was reported disabled on this Mac (`mdutil -s`).
- The top 3 gated contributors at each arm's peak sample are recorded in `verdict.json` (`foreign_cpu.top3_at_peak`) so a breach can be diagnosed.
- During an arm only gated foreign CPU counts: a sampler takes one reading at the start, every 5 s, and one at the end, and the arm's p95 must be within the foreign CPU limit.
  A breach stops the run as INVALID, since the remaining arms would be wasted.
- load1 is used only for the baseline and for the settle window before each arm (60 s, retried up to 15 minutes, with the load1 limit, the idle floor and no foreign build processes).
  It is not applied per rep, because the arm's own `cargo build -j 4` raises load1 above any idle-derived limit.
- `compare.py` has its own ceiling of 30 and a 2x arm skew limit; both are far looser than this gate and only a backstop.

Other cowfs daemons, including shared daemon 15263, do not count as foreign; their CPU shows up as foreign CPU, load1 and CPU idle, which carry the gate.
Run it when the other builders are finished.

## Thermal and cool-down

`pmset -g therm` showed no warning level on this Mac, which does not prove there was no throttling.
Stay at `-j 4` for both arms.
Take the baseline after at least 5 minutes of no heavy work, since a hot machine shows as lower idle and higher load in the baseline itself.
Do not run on battery.
Between arms the driver's quiet window gives a minimum 60 s pause.

## What a valid run looks like

`bench/out/g12/ID/verdict.json` is written for every run.
Valid means all of:

- `build` shows `opt_level` 3 and `debug_assertions` false for both binaries, and `daemon.launched_from_built_binary` is true with `daemon.mount_fstype` `nfs`.
- Every cowfs entry in `arms` has `pre` and `post` with `alive` true, `fstype` `nfs`, a `localhost:/cowfs-` source, an `st_dev` different from the native root, and no `problems` (a dead daemon makes `gates.py` recreate the root on local disk, which would be a native run labelled cowfs).
- `problems` is empty and rep counts equal `--reps` for every gate; each arm's `foreign_cpu.p95` is within its `limit`.
- `native_native` ratios within 1 +/- 0.10.
- `compare_rc` is `[0, 0]` for a PASS or `[1, 1]` or mixed 0 and 1 for a real FAIL; 2 or 3 is INVALID.
- On Linux the cowfs arm is `fuse.cowfs` with source `cowfs` (not `nfs`), `daemon.mount_fstype` is `fuse.cowfs`, and `platform` is `linux` with `native_fs` `btrfs` on the cachyos box; the rest of this list applies unchanged.
- `result` is PASS, FAIL or INVALID, and only PASS or FAIL is a gate statement.
- A non-sample run is refused up front unless gates include g1, g2 and g3, scale is 100, reps are at least 5 and `--load-cap` is at most 4.0 (a guess: 25 percent of 16 cores).
- Any recorded problem, including in `--sample`, gives exit 2; a valid FAIL is exit 1.
- `cowfs_over_native` in `verdict.json` holds per-gate median ratios; on macOS harness g2 passes on an added-seconds budget, so it can PASS above a literal 1.5x for a short native rebuild.
- Any problem (for example native-native out of band) turns a would-be FAIL into INVALID.

The debug-build artefact looks like: the driver refuses the build (opt_level 0 or debug assertions), or in old logs cowfs/native ratios of 17x to 144x on every rep including a no-op rebuild costing 13 to 16 s, load1 above 8, and native-native outside the band.
A cowfs/native ratio of 10x or more with a release daemon, quiet host and good controls would be a real FAIL, not an artefact, and must be investigated rather than excused.

## Linux arm: cachyos, btrfs

The same driver runs the Linux arm: `python3 bench/g12_run.py --run-id ID` picks the platform from `sys.platform`.
No second driver: the profile check, digest check, quiet rule, cool-down, minimums, `verdict.json` and exit codes are one code path.

Host facts (checked 2026-10-08): `zeeshan@100.122.64.51`, x86_64, 16 cores, Linux 7.2.8 cachyos, cargo 1.99.0, git, fusermount3, `/dev/fuse` mode 666 (so FUSE mounts need no sudo, verified by the sample below).
The cachyos root and `/home` (nvme0n1p2) and `/mnt/docs` (nvme0n1p3) are btrfs.
So the Linux native number is a BTRFS number and must be reported as such: `verdict.json` carries `platform` and `native_fs` (the native root's filesystem, read from `mount`).

What the Linux arm does differently (all in `bench/g12_run.py`):
- cowfs arm: the same private release `cowfs-daemon`, serving a FUSE mount; the arm must be `fuse.cowfs` with source `cowfs` (the fsname default of `cowfs-fuse`), `st_dev` different from the native root, daemon alive, digest unchanged, checked before and after every cowfs arm.
  The native root being on a `fuse.cowfs` mount stops the run.
- Parsing: `mount_entry` reads both `X on Y (type, ...)` (macOS) and `X on Y type T (...)` (Linux).
- CPU idle: `/proc/stat` deltas (idle + iowait over all ticks) every 5 s instead of `top -l`.
- Foreign CPU: `ps -A -o pid=,ppid=,pcpu=,comm=` as on macOS, same derivation (limit = baseline p95 + 50 points; load1 only for baseline and settle; same plateau cool-down and the same minimums).
  On Linux `ps` pcpu is CPU time over process lifetime, which hides a long-lived process that spikes now, so the Linux driver does not use it: it reads `/proc/[pid]/stat` twice 1 s apart and takes utime+stime tick deltas (a process is identified by pid and starttime, not by name, since kworker and setproctitle processes rename themselves; one born or reused inside the interval counts all its ticks), then feeds the same rows to the same `foreign_cpu` rule.
  The metric still differs from the macOS one (1 s delta against a decaying average), so the two platforms' baselines and limits are never compared with each other.
  The induced set (reported, not gated) is kernel threads `kworker`, `ksoftirqd`, `kswapd`, `jbd2`, `btrfs-*`, `fuse*`; chosen, not measured.
  The cachyos self-hosted CI runners (`Runner.Listener`, `Runner.Worker`) and any login session count as foreign and are gated, deliberately.
- Paths: everything resolves under `/mnt/docs/Projects/cowfs-g12` (override `COWFS_G12_BASE`): the checkout and `bench/out/g12/ID`, the CPU lock `cpu.lock`, the socket `sock/g12-ID.sock`.
  The driver refuses a path outside that base, and a socket path over 100 bytes (AF_UNIX).
  The socket directory is created mode 0700, because `cowfs-daemon` refuses any other mode (found live on the first sample).
- Stop: SIGTERM after an argv check, then if a `fuse.cowfs` mount is still listed, `fusermount3 -u` on that exact mountpoint, then the table is re-read and a remaining mount is a recorded problem.
- Cargo and temp: `CARGO_HOME`, `COWFS_BENCH_CARGO_HOME` and `TMPDIR` default to `<base>/cargo-home` and `<base>/tmp`, and the path check refuses an override outside the base.
  The rustup toolchain stays in `~/.rustup` (read only).

Shipping the tree: a `git bundle` of the branch cloned on the box (the gate corpus clone needs the pinned sha `c1619ec`, which `git archive` has no history for), then the changed files copied over.
There are no GitHub credentials on the box.

Optional proxy, explicitly labelled loop-on-btrfs: an ext4 loop image under `/mnt/docs`.
It needs sudo for loop, mkfs and mount only, using the password from the `.env` `CACHY_OS_PASS` piped on stdin, never in argv.
The recipe is `docs/verification/evidence/namespaces171/fsimg.sh`, in commit `29f6e9a`.
Not built or run here.
A loop-on-btrfs ext4 number is never the gate.

Validated on cachyos (2026-10-08), NOT GATE RESULTS.
Driver files sha256 prefixes `0a7d8aac` (g12_run.py) and `2c363734` (test_g12_run.py) are what ran in `lsample4`, run from a bundle of this branch's base plus those two files (the branch head is identical to them for these files).
- `python3 -m unittest bench.test_g12_run` (32 tests) passes on the box and on the Mac; `python3 -m unittest discover -s bench` passes on the Mac (516 tests, 13 skipped).
- `lsample4` (final driver): `--sample` (scale 1, g3, 1 rep, 20 s baseline).
  Release build accepted (`opt_level` 3, `debug_assertions` false), daemon launched from the built binary, `mount_fstype` `fuse.cowfs`, all four arms ran and were quiet, `pre` and `post` arm checks empty, `native_fs` `btrfs`, `induced_max` at most 4 points.
  Exit 2 with the only recorded problem native-native 0.412 outside 1 +/- 0.10: g3 takes 0.03 to 0.1 s at scale 1 with one rep, so that is noise, and the sample's g3 cowfs/native ratios of 1.62 and 1.27 (`compare_rc` `[1, 0]`) say nothing about cowfs.
- `lsample3` (earlier driver, same box): `--sample --gates g1,g2,g3`, so a clean cargo build and an edit rebuild ran through the FUSE mount.
  Harness g1 cowfs/native was 1.20 and 1.18 (16.2 s native against 19.5 s cowfs), from one rep, which is not an estimate.
  Its c1, n2 and c2 arms were "not quiet" by the settle check because load1 (1.6 to 2.1) was above the baseline-derived limit of 1.42 right after the previous arm's `-j4` build; foreign CPU was within its limit.
  That run also exposed the process-identity bug (`induced_max` of 3000 to 18000 points because a renamed kernel thread counted its lifetime ticks), fixed by the starttime rule and a test before `lsample4`.
- `lsample1` (first driver) died on the daemon's 0700 socket-directory rule; fixed, test added.
- `lsample2` (lifetime-pcpu driver) got through the plumbing with native-native 0.522 and `compare_rc` `[1, 1]`; superseded by the /proc delta metric.
- Afterwards each time: no `cowfs` mount, no daemon, no driver process, `cpu.lock` and socket gone.

Still unvalidated on Linux:
- Harness g1 and g2 as a measurement: one sample rep only, no 5-rep run.
  Harness g2 took 0.11 to 0.36 s in `lsample3` on both arms; the edited file is compiled (`mod cookies`, workspace member `crates/*` at the pinned sha), so the time is unexplained, possibly a leaf crate rebuilding incrementally with nothing downstream relinked.
  It applies to both platforms, was not investigated (scope freeze), and bears on the tracker g1 bar.
- The plateau cool-down did not settle in samples (a sample does not wait); it has never run live with a real 300 s baseline.
- A full gate run (scale 100, 5 reps) has not been attempted.
- The box has an interactive desktop session (`kitty`, `btop`, `hyprlock` showed in the foreign-CPU top lists) and the self-hosted CI runners: a real run needs a window with neither, and no lock is shared with the runners.
- The foreign-CPU limit on Linux (baseline p95 + 50 points) is a chosen margin, as on macOS.
- Each Linux foreign-CPU sample includes a 1 s delta, so the sampling cadence is about 6 s and `--baseline-window 300` spans about 360 s.
- The `fusermount3 -u` fallback is covered by a unit test only; in every live run the daemon unmounted itself on SIGTERM.

Linux one-command recipe (run it detached; the harness timeout kills foreground jobs):

    export PATH=/home/zeeshan/.cargo/bin:$PATH && cd /mnt/docs/Projects/cowfs-g12/src && mkdir -p ../logs && setsid nohup python3 bench/g12_run.py --run-id g12-$(date +%Y%m%d) > ../logs/g12.log 2>&1 < /dev/null & echo $! > ../logs/g12.pid

Afterwards verify: `mount | grep cowfs` is empty, `pgrep -f '^python3 bench/g12_run'` is empty, `../cpu.lock` is gone.

## Sample run, NOT A GATE RESULT

Both samples are described here only; their `verdict.json` files are not committed.

`sample2` ran an earlier (round 2) driver, before the foreign CPU rule, the plateau cool-down and the induced set existed, so its load findings below come from a rule that no longer exists: `python3 bench/g12_run.py --run-id sample2 --sample` (scale 1, gate g3 only, 1 rep, four arms).
- `build` shows both binaries accepted from cargo's own reported profile.
- `daemon.launched_from_built_binary` is true.
- c1 and c2 `pre` and `post` arm checks had empty problem lists.
- `mount | grep g12/` gave 0 and the CPU lock was gone afterwards.
- It exited 2 because problems were recorded on the busy host: baseline refused, all reps above the (since removed) per-rep load limit, native-native 0.626 outside the band, `compare.py` rc 2 twice.
- The 23.4x and 12.0x cowfs/native ratios in it are busy-host numbers, not evidence about cowfs.

`sample1` is superseded: it came from the first driver version, which could not fail its release check or detect a dead daemon.
Neither sample ran cargo builds through the mount, which stay unvalidated (see blockers).

## Blockers checklist

- [ ] UNVALIDATED: cargo build through the release daemon mount; run `--sample --gates g1,g2 --reps 1` on the quiet host first.
- [ ] Quiet Mac: no other agent running cargo or rustc, so the baseline passes (p95 load1 at most 4.0).
- [x] cargo-home populated in this lease (`bench/out/cargo-home`); a new lease needs network once.
- [ ] Release binaries present in the lease (rebuild if the lease was returned).
- [ ] Time budget: 100k-file tree generation through NFS plus 4 arms x 5 reps of clean builds; expect hours, not minutes.
- [ ] Linux arm on cachyos (btrfs): driver exists and a g3 sample passed its plumbing (see the Linux section); cargo builds through FUSE and a full run are not done.
- [ ] The foreign CPU rule, the plateau cool-down and the induced set have never run live with a real baseline, on either platform: unit tests, a `ps` parse on this Mac, and the 20 s Linux sample only.
- [ ] Cargo build through the mount is still unvalidated on a quiet host.
- [ ] Real large tracked repo for git status is not covered (synthetic tree only).

## One-command recipe for the quiet-host Mac run

    cd <lease> && mkdir -p bench/out && nohup python3 bench/g12_run.py --run-id g12-$(date +%Y%m%d) > bench/out/g12-$(date +%Y%m%d).log 2>&1 &

The driver builds the release daemon itself.
It takes hours: run it detached as shown and poll `verdict.json`.
