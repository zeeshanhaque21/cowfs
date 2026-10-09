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
- `result` is PASS, FAIL or INVALID, and only PASS or FAIL is a gate statement.
- A non-sample run is refused up front unless gates include g1, g2 and g3, scale is 100, reps are at least 5 and `--load-cap` is at most 4.0 (a guess: 25 percent of 16 cores).
- Any recorded problem, including in `--sample`, gives exit 2; a valid FAIL is exit 1.
- `cowfs_over_native` in `verdict.json` holds per-gate median ratios; on macOS harness g2 passes on an added-seconds budget, so it can PASS above a literal 1.5x for a short native rebuild.
- Any problem (for example native-native out of band) turns a would-be FAIL into INVALID.

The debug-build artefact looks like: the driver refuses the build (opt_level 0 or debug assertions), or in old logs cowfs/native ratios of 17x to 144x on every rep including a no-op rebuild costing 13 to 16 s, load1 above 8, and native-native outside the band.
A cowfs/native ratio of 10x or more with a release daemon, quiet host and good controls would be a real FAIL, not an artefact, and must be investigated rather than excused.

## Linux native arm: cachyos, btrfs

What exists: nothing.
There is no Linux driver and no Linux release daemon built for this gate.
`bench/linux-sample.sh` is native-only and starts no daemon.

Host facts (checked 2026-10-08): `zeeshan@100.122.64.51`, x86_64, 16 cores, Linux 7.2.8 cachyos, load average 0.01, cargo 1.99.0, git, fusermount3 present.
The cachyos root and `/home` (nvme0n1p2) and `/mnt/docs` are btrfs.
So the Linux native number is a BTRFS number and must be reported as such.

Optional proxy, explicitly labelled loop-on-btrfs: an ext4 loop image under `/mnt/docs`.
It needs sudo for loop, mkfs and mount only, using the password from the `.env` `CACHY_OS_PASS` piped on stdin, never in argv.
The recipe is `docs/verification/evidence/namespaces171/fsimg.sh`, in commit `29f6e9a` (not on this branch).
A loop-on-btrfs ext4 number is never the gate.

`bench/g12_run.py` parts that are macOS-specific and must not be reused as is on Linux:
- `top -l` CPU parsing (`window`).
- `mount_entry` parses the macOS form `X on Y (type, opts)`; Linux prints `X on Y type T (opts)`, so the regex would read the type as `rw`.
- The daemon mount check `fstype == "nfs"` and the `localhost:/cowfs-` NFS export source in `arm_problems`.
- The shared CPU lock path under `spikes/nfs-loopback/out`.
- The `~/.cowfs/sock` socket location.

Requirements for a Linux arm, text only, not built in this PR:
- A Linux release daemon built on cachyos with the same `--message-format=json` profile and digest checks.
- A FUSE mount through `cowfs-fuse` inside the daemon, `fusermount3` for unmount, and a mount-table check for `fuse.cowfs` instead of NFS (see how `docs/verification/ready-g4.md` attests the arm).
- Everything under `/mnt/docs/...` per the `cachyos-gpu` skill (not `/` or `/home`), including the private store, mount, socket, cargo home and native root.
- A Linux idle baseline from `/proc/loadavg` and `/proc/stat` over the same window rules.
- A native control on the same btrfs filesystem and the same native-native band.
- Sudo only for the optional loop image, never for the daemon.
- Coordination with the cachyos self-hosted CI runner, which shares the box.

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
- [ ] Linux native arm on cachyos (btrfs): Linux driver and Linux release daemon do not exist (requirements above).
- [ ] The foreign CPU rule, the plateau cool-down and the induced set have never run live in a driver run, only as unit tests and one `ps` parse on this Mac; they are untested against a real build on a quiet host.
- [ ] Cargo build through the mount is still unvalidated on a quiet host.
- [ ] Real large tracked repo for git status is not covered (synthetic tree only).

## One-command recipe for the quiet-host Mac run

    cd <lease> && mkdir -p bench/out && nohup python3 bench/g12_run.py --run-id g12-$(date +%Y%m%d) > bench/out/g12-$(date +%Y%m%d).log 2>&1 &

The driver builds the release daemon itself.
It takes hours: run it detached as shown and poll `verdict.json`.
