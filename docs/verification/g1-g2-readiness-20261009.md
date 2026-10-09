# Readiness: gates g1 and g2 (cargo build, git status) within 1.5x of native

Status: READINESS ONLY.
No g1 or g2 gate number exists in this document.
The host was busy for the whole session, so no timed measurement was presented as a result.
Tracker bar: native APFS (this Mac) and native ext4 (moonscape), cargo build and git status each within 1.5x of native.

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

Built on 2026-10-08 at `43fe43cb54c96b4ebfdbe2c81c7e0083a15f9336`, 52 s:

- `target/release/cowfs-daemon` sha256 `c088fc0826dce7fc77d1003a43ecd3c41ca66c961e3f1b510ff24e39f1b74708`, 8,012,880 bytes.
- `target/release/cowfs` sha256 `1915c95e4203c0c2d2db124b64816af89e79c14796719de07da278808e184134`.
- The path is in the lease `.treehouse-ci/.treehouse/cowfs-7c1bf8/6/cowfs`, so rebuild if that lease is returned.

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

- Baseline window: 300 s, load1 sampled every 5 s plus `top -l N -s 5 -n 0` CPU idle (first top sample dropped, it is since boot).
- Baseline is refused if p95 load1 exceeds 4.0 (25 percent of 16 cores), if any foreign cargo, rustc, cc1 or ld process runs, or if there are no CPU samples.
- Limit is `min(baseline p95 load1 + 1.0, 4.0)`; idle floor is baseline median CPU idle minus 10 points.
- Before each of the four arms a 60 s window must satisfy the limit, the idle floor and no foreign processes, retrying for 15 minutes, else INVALID.
- After the run every rep's `load1_before` and `load1_after` is checked against the limit.
- `compare.py` has its own ceiling of 30 and a 2x arm skew limit; both are far looser than this gate and only a backstop.

Other cowfs daemons, including shared daemon 15263, do not count as foreign; their load shows up in load1 and CPU idle, which carry the gate.
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

- `daemon.release` true: argv path contains `/target/release/` and `--backend core`, and `daemon.mount_fstype` is `nfs`.
- `daemon.sha256` equals the built binary.
- Native root is not on NFS.
- `problems` is empty, rep counts equal `--reps` for every gate, and no rep above the load limit.
- `native_native` ratios within 1 +/- 0.10.
- `compare_rc` is `[0, 0]` for a PASS or `[1, 1]` or mixed 0 and 1 for a real FAIL; 2 or 3 is INVALID.
- `result` is PASS, FAIL or INVALID, and only PASS or FAIL is a gate statement.

The debug-backend artefact looks like: `daemon.release` false, cowfs/native ratios of 17x to 144x on every rep including a no-op rebuild costing 13 to 16 s, load1 above 8, and native-native outside the band.
A cowfs/native ratio of 10x or more with a release daemon, quiet host and good controls would be a real FAIL, not an artefact, and must be investigated rather than excused.

## ext4 on moonscape

Reached with `ssh moonscape@192.168.68.119`.
Checked 2026-10-08: aarch64 Pi, 4 cores, Linux 6.12.109+rpt-rpi-2712, ext4 on `/dev/sda2`, 48 GB free, cargo 1.95.0, git, fusermount3 present, load averages 2.03 1.56 1.44.
It is not idle by design: it also runs OmniRoute and Hermes, so a 4-core baseline there must be measured the same way and may fail the cap.
Work under `/home/moonscape/cowfs-ready-wave/`, not `$HOME` root.
Needs a Linux release `cowfs-daemon` (FUSE, built on the Pi, slow) and the Linux counterpart of the driver, which does not exist yet.
`bench/linux-sample.sh` is native-only and does not start a daemon.

Cachyos box (`zeeshan@100.122.64.51`, x86_64, 16 cores, load 0.01 at 17:36) is also available and the quietest host seen, but its home is btrfs, not ext4.
An ext4 loop image or spare partition would be needed for the ext4 bar.

## Sample run, NOT A GATE RESULT

Command: `python3 bench/g12_run.py --run-id sample1 --sample` (scale 1, gate g3 only, 1 rep, release private daemon, four arms).
It ran end to end: private release daemon started on NFS, `base` snapshot created, four `gates.py` runs wrote JSONL, `compare.py` ran twice, `verdict.json` written, daemon stopped, no mount or daemon left.
The validator did its job: it reported the baseline refused (load1 p95 12.8, foreign cowfs-daemon), all four reps above the load limit, native-native 2.169 outside the band, and compare.py exit 2 UNMEASURABLE (load1 peak 127.9).
The cowfs/native ratio of 11.2x in that sample is a busy-host number and is not evidence about cowfs.
The sample exercised only tracker g2 (harness g3).
Its setup did run `cargo fetch --locked` into `bench/out/cargo-home` for both arms, so the registry cache exists in this lease.
No cargo build ran: g1 and g2 builds were deliberately not run on the busy host, so the cargo build through the release daemon mount, the path that showed 17x to 144x, is unvalidated.

## Blockers checklist

- [ ] UNVALIDATED: cargo build through the release daemon mount; run `--sample --gates g1,g2 --reps 1` on the quiet host first.
- [ ] Quiet Mac: no other agent running cargo or rustc, so the baseline passes (p95 load1 at most 4.0).
- [x] cargo-home populated in this lease (`bench/out/cargo-home`); a new lease needs network once.
- [ ] Release binaries present in the lease (rebuild if the lease was returned).
- [ ] Time budget: 100k-file tree generation through NFS plus 4 arms x 5 reps of clean builds; expect hours, not minutes.
- [ ] ext4 bar on moonscape: Linux driver and daemon build missing, and the host runs other services.
- [ ] Real large tracked repo for git status is not covered (synthetic tree only).

## One-command recipe for the quiet-host Mac run

    cd <lease> && cargo build --release --locked -p cowfs-daemon -p cowfs-cli -j4 && mkdir -p bench/out && nohup python3 bench/g12_run.py --run-id g12-$(date +%Y%m%d) > bench/out/g12-$(date +%Y%m%d).log 2>&1 &

It takes hours: run it detached as shown and poll `verdict.json`.
