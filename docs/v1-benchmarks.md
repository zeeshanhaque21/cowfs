# Criterion 2 measurement harness

Success criterion 2 in `docs/design.md`, amended by issue #18: a clean `cargo build` and `git status` on a large tree within 1.5x of native, and on macOS warm and incremental builds against an absolute budget instead of a ratio.
This document is how to run the harness and how to read its output.
It records no results: results go in `bench/out/<label>-<timestamp>.jsonl` and are reported from a run, not written down here ahead of time.

## The arms

The harness runs every gate inside one directory, so the same script produces both arms and the only difference between them is the path.
The native arm is a plain directory on APFS (macOS) or ext4 (Linux).
The cowfs arm is a mount point.
Nothing inside the harness knows which one it is measuring.

The only mountable backend today is `cowfs-vfs-path` (native passthrough) over the NFS adapter on macOS or the FUSE adapter on Linux.
So a measurement made now is adapter overhead, not cowfs overhead.
The real core backend plugs in behind the same mount later and the harness does not change.

## The gates

| gate | what it does | bar |
|---|---|---|
| g1 | clean `cargo build` of a pinned clone of this repo, `CARGO_TARGET_DIR` inside the measured directory | ratio 1.5x |
| g2 | warm edit-and-rebuild: touch a leaf file, `cargo build` again | ratio 1.5x off macOS, under 1 s added over native on macOS |
| g3 | `git status` on a generated 100k+ file tree, warm and with 1% of the files touched | ratio 1.5x |
| g4 | tree walk plus a small-file read pass over 20k files | reported only |
| g5 | large sequential 1 GiB write and read back, `fsync`, MiB/s | reported only |
| g6 | metadata storm: create, `stat` and unlink 50k files | reported only |

g1 and g2 build the same corpus both times: this repo at a pinned commit, cloned with `git clone --no-hardlinks` from the local repository so the clone does not share objects with it.
`CARGO_INCREMENTAL` is left at the project default.
`cargo fetch` runs once beforehand into a shared registry cache outside the measured directory, so no build ever waits on the network.
g2 edits `crates/cowfs-vfs-path/src/cookies.rs`, a leaf nothing in the workspace depends on, so what is measured is one small crate plus the tail of the graph behind it.

g3 generates its own tree with a fixed seed (100k small files across 256 directories plus 64 files of 8 MiB), `git init`, `git add`, `git commit`, then times `git status` twice: warm, and again after touching 1% of the files in one directory.
The warm number is what criterion 2 names.
The dirty number is reported next to it because a tree with pending changes is what an agent actually leaves behind.

g4, g5 and g6 have no bar.
They exist so a regression in readdir, large-file throughput or metadata cost shows up as a number rather than as an unexplained slow build later.

## Interleaving and the CPU lock

`bench/run-pair.sh` runs the four arms in a fixed order: native, cowfs, native, cowfs.
Interleaving is the point.
Two arms measured back to back carry whatever the machine did in between; two arms measured at the start and the end of a run carry the whole drift between them.
The order also puts the two native runs around the pair, so their ratio is the noise floor of the machine.

The lock is an atomic `mkdir`, retried every 10 s for 15 minutes, with an owner file naming the user, pid and time, and removed by a trap so a kill still frees it.
`COWFS_BENCH_CPU_LOCK` moves it.

## Refusing to print a ratio

`bench/compare.py` prints per-gate median, min and max of the per-rep ratios, and both rep counts.
It refuses to print a ratio at all when the machine was too loaded for a ratio to mean anything: `load1` above 30 on either side, or the two arms more than 2x apart.
It prints the load numbers and the word `unmeasurable` instead, and exits 2.
Two previous reviewers found timings unmeasurable at load 100 to 300, so a ratio there is noise with a decimal point on it, and the harness must not hand one to the next reader as if it were a finding.

Every rep records `load1` before and after its own timed section, so a gate that ran quiet inside a noisy run is still visible as such.

## The noise floor

The native arm is run twice and compared against itself.
The result is not a pass or a fail, it is the number below which no cowfs measurement means anything.
A cowfs ratio under the noise floor is indistinguishable from the native arm measured twice.
`run-pair.sh` produces this for free; `compare.py` prints it from `--noise-floor`, and says so plainly when it was not supplied.

## Output

Each invocation appends one JSONL line per rep to `bench/out/<label>-<timestamp>.jsonl` and flushes and `fsync`s it before the next rep starts.
A `kill -9` loses at most the rep in flight and the file stays valid JSONL.
Re-running the same invocation resumes the same file and skips the reps already recorded, so a crashed run costs one rep and not the whole run.
The first line is a `meta` record with the root, the gate list, the counts, the pinned commit, the cargo jobs, the host and the platform, so two files can be checked for comparability before they are compared.

## Environment

| variable | meaning |
|---|---|
| `COWFS_BENCH_SCALE` | percent of the full file counts, default 100 |
| `COWFS_BENCH_CORPUS_SHA` | pinned commit for the g1 and g2 corpus |
| `COWFS_BENCH_CARGO_HOME` | shared registry cache, outside the measured directory |
| `COWFS_BENCH_CARGO_JOBS` | `cargo -j`, identical for both arms, default 4 |
| `COWFS_BENCH_FAKE_LOAD1` | test hook: the recorded `load1`, for exercising the refusal path |
| `COWFS_BENCH_CPU_LOCK` | the lock directory `run-pair.sh` takes |

`COWFS_BENCH_SCALE` is a shrunken-corpus switch for checking the harness itself.
It is not a result: a 2% run tells you the script works and nothing about the filesystem.

## Running it

Native arm, full size, five reps, on a quiet machine under the lock:

```sh
COWFS_BENCH_SCALE=100 sh bench/run-pair.sh /tmp/bench-native /tmp/bench-cowfs 5
python3 bench/compare.py \
  --native bench/out/native1-*.jsonl bench/out/native2-*.jsonl \
  --cowfs  bench/out/cowfs1-*.jsonl \
  --noise-floor bench/out/native2-*.jsonl
```

The cowfs arm needs a mount first.
`bench/mount.sh BACKING MOUNTPOINT start` mounts a `PathVfs` rooted at `BACKING` over the macOS NFS loopback through `bench/mount-nfs`, and `stop` unmounts and verifies the mount table.
It builds with `cargo build --release --manifest-path bench/mount-nfs/Cargo.toml`.
`stop` checks the pid's command line before signalling it and refuses to signal anything else.

On Linux the cowfs arm is a `cowfs-fuse` mount; the harness is unchanged, only the mount point differs.

## Linux, in the VM

The Linux arm runs in the `cowfs-spike3` OrbStack VM:

`bench/linux-sample.sh` is that script:

```sh
orb -m cowfs-spike3 env \
  COWFS_BENCH_REPO=/path/to/cowfs-as-the-vm-sees-it \
  COWFS_BENCH_SCALE=2 \
  sh /path/to/cowfs-as-the-vm-sees-it/bench/linux-sample.sh
```

The measured root is VM-local, never a shared host mount, and g1 and g2 build with a tmpfs `CARGO_TARGET_DIR`, so the number is of the filesystem under test and not of the VM's own disk.

Two VM facts worth knowing, both hit on 2026-10-01 and both worked around inside the script:

- The btrfs subvolume behind `/home/zeeshanhaque` refuses new files with ENOSPC while `df` reports 200 GiB free.
  So the default `COWFS_BENCH_WORK` is `/tmp/cowfs-bench-work`, which is tmpfs.
- `RUSTUP_HOME` must be set alongside `CARGO_HOME`.
  Without it `cargo` is a rustup shim with no default toolchain and every build fails.
  `COWFS_BENCH_CARGO_HOME` must also be writable and outside the measured directory, since cargo unpacks the whole registry into it.

## What this does not measure

Whether the ratio criterion is met is a finding from a run under the lock on a quiet machine.
Until then the numbers in `bench/out` are a harness validation.
