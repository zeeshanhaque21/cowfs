# Live trial metrics: build time and storage on the live cowfs mount

Measured 2026-10-02, 21:48 to 22:41 PDT, against the daemon that has been serving the treehouse pools since 11:27.
Tooling: `scripts/measure-live-trial.py`.
Run id `lt-20261002a`, raw JSONL (append, flush, fsync per record) at `bench/out/live-trial-native/lt-20261002a/metrics.jsonl` in the primary checkout, not committed.
Refs #3 (build and `git status` overhead versus native) and #1 (dedup ratio).
This closes nothing.

Status of every number below: **provisional**.
The host was never quiet, so no build timing here is a pass or a fail against the criterion 2 bars.

## What was measured

- The mount is `/Users/zeeshanhaque/.cowfs/mnt`, an NFS loopback served by `cowfs-daemon --backend core`.
- The daemon binary is a **debug build** (`.../cowfs-7c1bf8/7/cowfs/target/debug/cowfs-daemon`).
  Every cowfs-arm number includes that.
  How much of the slowdown is debug code, the NFS client, or the core backend was not separated.
- This is mode (a), transparent.
  The control API reports `snapshot_count 1` (only `base`), so mode (b) snapshot-native warm-base slots was not exercised and nothing here says anything about it.
- Treehouse pool slots live on the mount and were being written by other agents throughout.

## No history exists

`~/.cowfs/daemon.log` holds one 160-byte startup line.
`cowfs status` returns point-in-time counters only.
There are no per-operation timings and no earlier store sizes, so "build time over the last 5 to 6 hours" cannot be reconstructed.
What exists is what this run recorded, plus three status readings taken by hand earlier in the session:

| time | blocks | logical bytes | stored bytes |
|---|---|---|---|
| 21:32 | 99,181 | 5,342,563,033 | 1,834,441,799 |
| 21:48 | 122,100 | 6,245,218,717 | 2,187,525,512 |
| 22:03 | 150,780 | 7,717,786,521 | 2,692,373,069 |
| 22:40 | 195,364 | 10,271,943,019 | 3,601,398,786 |

The store grows by roughly 1 to 2 MB/s of logical bytes from other writers.
That background rate matters below.

## Storage

### Definitions

`Store::stats` (`crates/cowfs-store/src/store.rs`, surfaced by `status`):

- `logical_bytes` = sum of uncompressed sizes of **unique** blocks in the index (`uncompressed_bytes`).
  It is not the size of the visible trees.
- `stored_bytes` = sum of record sizes (header plus compressed payload) of those blocks.
- `put_bytes`, `dedup_hits` and `dedup_bytes` exist in `Stats` but the control API does not expose them (`Usage` has three fields).
  So bytes written versus bytes deduplicated is not observable from outside.

Derived here:

- A = apparent bytes of the live tree, regular files only, hardlinked files counted once by `(st_dev, st_ino)`.
  APFS stores a hardlink once, so this is the native baseline.
- D = A / `logical_bytes`.
  Chunk-level dedup factor, before compression.
- C = `stored_bytes` / `logical_bytes`.
  Compression only.
- P = pack bytes plus `meta.redb` plus the small files in the store directory, by `stat`, not `du`.
- S = A / P.
  End-to-end storage factor against the native do-nothing baseline.

### Measurement

- Bounded read-only walk of `/Users/zeeshanhaque/.cowfs/mnt/base`, single thread, 300 s budget, 90 s no-progress abort.
  It took 11 s, covered all 24 roots (every pool slot plus `repos`), saw 95,513 entries, 0 vanished files, 0 errors except one on the top-level file `test.txt`, which was passed as a root and is 6 bytes.
- Walk window 22:01:38 to 22:01:49.
  Store counters moved from 149,849 to 150,220 blocks during it, so walk and store figures are not atomic.
- The bench run directory itself was excluded from the walk and walked separately at about 22:02 (783 MB, 3,625 files), then added back.
  Its smoke build had already written into the store.
- Store `stat` reading at 22:03:02.

| quantity | value |
|---|---|
| files walked (hardlinks once) | 67,927 plus 3,625 in the bench dir |
| hardlinked duplicates skipped | 18,381 files, 993,817,388 bytes (plus 126,392,776 in the bench dir) |
| A, apparent | 10,286,735,518 (9,503,837,468 slots plus 782,898,050 bench dir) |
| A, allocated (`st_blocks`) | 10,306,162,688 |
| of which under `target/` | 8,085,286,848 (85%) in the slots |
| `logical_bytes` | 7,717,786,521 at 22:03 (7,681,357,595 at walk end) |
| `stored_bytes` | 2,692,373,069 |
| pack bytes (11 packs) | 2,692,373,245 |
| P | 2,812,246,866 (allocated 2,807,406,592) |
| `meta.redb` | 119,873,536 |

| derived | value |
|---|---|
| D = A / logical | 1.33 to 1.34 |
| C = stored / logical | 0.349 (2.87x compression) |
| S = A / P | 3.66 (3.67 against allocated) |
| A / stored (packs only) | 3.82 |

Reading it:

- Against the native baseline the store holds the same trees in about 27% of the bytes (1 / 3.66), including metadata.
- Compression accounts for most of it (2.87x).
  Dedup adds about 1.33x on top.
- A per-file-compression-only baseline was not measured.
  The figure A x C = 3.59 GB is an approximation that assumes per-file compression behaves like per-block compression, and it is unverified.
- D is a lower bound.
  Pack bytes exceed `stored_bytes` by 176 bytes across 11 packs, so no space has ever been reclaimed, and the index also counts blocks of files that were since deleted or rewritten and not garbage-collected.
  A and the store are also measured minutes apart under constant writes.
- Hardlink dedup in A matters: counting every link would add about 1.0 GB and raise D and S by about 10%.
- The sample is the whole mount.
  Slots leased to other agents were read while in use.

### What a rebuild adds to the store

Each cowfs-arm clean build writes a 949 MB `corpus-target` (5,142 files, apparent) through the mount.
Store growth measured across the three clean builds was 560, 316 and 755 MB logical.
Native-arm builds, which write nothing to the store, measured 0 to 2.9 MB/s of background logical growth during the same phases, and two 60 s idle samples measured 1.1 MB/s (22:03) and 0.7 MB/s (22:40).
The cowfs-build rates were 1.4, 0.7 and 1.4 MB/s.
So a 949 MB rebuild into the same path added no growth distinguishable from the other writers.
That is consistent with near-total dedup of repeated builds of identical output, but it cannot be quantified here.
Settling it needs the `put_bytes` and `dedup_bytes` counters, or a quiesced store.

## Builds

### Method

- Source: this repo at `ab99868ba36d12fa1cb4ba35738bf71ee8fede63`, cloned `--no-hardlinks` into each arm.
  Manifest sha256 (`git ls-files -s`) `14a6b6dee9df...`, 497 files, and `Cargo.lock` sha256 identical on all three arms.
- Build: `cargo build --offline --locked -p cowfs-daemon -j 4`, debug profile, `rustc 1.99.0 (b940084d7 2026-09-28)`, 77 crates compiled clean.
  Separate `CARGO_TARGET_DIR` inside each arm.
  One shared `CARGO_HOME` on APFS outside every arm, populated once with `cargo fetch`.
  No global cache was touched.
  No `RUSTFLAGS`, `RUSTC_WRAPPER` or `CARGO_INCREMENTAL` was set, and no `.cargo/config` was found at the primary checkout, the worktree, the mount root or `~/.cargo`.
- Arms: `nativeA` and `nativeB` on APFS (`/dev/disk3s5`), `cowfs` on the NFS mount.
- Smoke on all three arms first: exit 0, `target/debug/cowfs-daemon --help` exit 0, identical help text hash, 77 crates compiled.
- Per rep and arm: clean (target dir removed), no-op rebuild, edit-and-rebuild.
  The edit appends one comment line to `crates/cowfs-vfs-path/src/cookies.rs`.
  2 crates recompile.
  The file is restored afterwards.
- 3 reps, arm order rotated each rep: A, cowfs, B; cowfs, B, A; B, A, cowfs.
- Before and after every phase: `load1`, summed `ps` CPU, the daemon's CPU, RSS and store counters.
- The only deletion was the arm's own `corpus-target`, after checking the run marker file and the exact path.

### Results

Wall seconds, 3 reps each.
Ratio = cowfs over the mean of the two native arms in the same rep.
Native-native = `nativeB` over `nativeA`.

| phase | nativeA median (range) | nativeB median (range) | cowfs median (range) | cowfs/native median (range) | cowfs added median (range) | native-native median (range) |
|---|---|---|---|---|---|---|
| clean | 24.54 (18.08 to 25.14) | 21.86 (20.03 to 22.14) | 433.76 (399.97 to 538.40) | 22.8x (17.0 to 23.1) | +414.7 s (376.5 to 515.1) | 0.90 (0.87 to 1.11) |
| no-op | 0.12 (0.09 to 0.17) | 0.20 (0.11 to 0.25) | 15.01 (13.12 to 16.37) | 88x (72 to 144) | +14.8 s (12.9 to 16.3) | 1.17 (0.97 to 2.92) |
| edit | 0.84 (0.73 to 0.95) | 0.88 (0.82 to 1.10) | 46.86 (41.88 to 73.35) | 50x (46 to 91) | +45.8 s (41.0 to 72.5) | 1.17 (0.98 to 1.21) |

What the numbers do show, with the caveats of the next section:

- Every cowfs-arm build was slower than native in every rep, by a factor (17x to 144x) far larger than the native-native spread (about 0.87x to 1.21x, outside the one 2.92x no-op outlier).
- The slowdown is worst for the metadata-heavy phases.
  A no-op rebuild does no compilation and still costs 13 to 16 s on cowfs against about 0.1 s native.
- Nothing got faster on cowfs.
  Nothing benefited.

### Why these are provisional

- `load1` ran 10.2 to 23.8 during phases (lowest sampled all run 8.9) on 16 logical CPUs, against a pre-declared quiet limit of 8.
  Median summed `ps` CPU was 650 to 1,070%.
  Other agents were building, indexing and writing the same store throughout.
- The cowfs arm saw `load1` 10.5 to 17.8, the native arms 10.2 to 23.8, so load was not matched between arms.
- The native-native median ratio for no-op and edit is outside the pre-declared 1 +/- 0.10 band.
- The daemon is a debug build, shared with other consumers, and was at 40 to 120% CPU during the cowfs phases.
- Thermal state: `pmset -g therm` reported no warning level, which does not prove there was no throttling.
  No idle baseline could be taken because the host was never idle.
- 3 reps per cell is a small sample.
- The script prints `PROVISIONAL: no gate verdict` unless load1 is at most 8 in every phase and the native-native ratio is inside the band.
  It did here.

The criterion 2 bars (1.5x for clean, under 1 s added for macOS edit-and-rebuild) are therefore **not** evaluated.
The measured magnitudes would fail those bars by orders of magnitude, but the debug daemon and the loaded host mean this run does not show that the bars fail on a release daemon on a quiet machine.

## Next recipe for a valid run

1. Run a release `cowfs-daemon` (`cargo build --release -p cowfs-daemon`) on a separate store and mount, so no other agent writes to it.
2. Wait for `load1` below 8 on every sample, or take the CPU lock `bench/run-pair.sh` uses.
3. `python3 scripts/measure-live-trial.py --run <new-id> init`, then `build --smoke`, then `build --reps 5`, then `summarize`.
4. Take `bg` samples before and after and use the idle growth rate to correct store deltas.
5. Expose `put_bytes` and `dedup_bytes` through `status` to measure dedup per write directly.

## Blocked or unknown

- No historical timings or sizes: not recorded by the daemon.
- Dedup per write: counters exist but are not exposed.
- Garbage in the store (unreferenced blocks): not measurable from outside, so D is a lower bound.
- Mode (b) snapshot-native warm reuse: not enabled on this mount, not measured.
- The `git status` gate (g3) and the other `bench/gates.py` gates were not run.
- Whether the slowdown comes from the debug build, NFS client costs, or the core backend: untested.
