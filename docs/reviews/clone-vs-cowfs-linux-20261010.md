# Clone vs cowfs on Linux, 2026-10-10

Question: can plain OS copy-on-write clones (btrfs reflinks) or overlayfs replace the cowfs mount for many cheap workspaces of a Rust project?
Judged on real disk used, build time, and whether built files survive a different folder path.
A sibling run covers macOS; this is the Linux half.

## Setup

- Host: cachyos box, kernel 7.2.8-2-cachyos, x86_64 (the brief said arm; the box is x86_64), 16 threads, 54 GiB RAM.
- Filesystem: `/mnt/docs`, btrfs, options `rw,noatime,compress=zstd:1,ssd,discard=async,space_cache=v2`.
- Work dir: `/mnt/docs/cowfs-exp`, created for this run and deleted at the end (verified gone, no mount, no process left).
- Free disk at start 619.1 GB, at end 619.3 GB.
- Toolchain: cargo 1.99.0 (5f94df478 2026-08-27), rustc 1.99.0 (b940084d7 2026-09-28), LLVM 23.1.1, GNU coreutils cp 9.12.
- No `CARGO_*` or `RUST*` env, no cargo config, no sccache, target dir inside every tree.
- Project: fresh clone of `zeeshanhaque21/cowfs`, pinned commit `65a551957134c361c70fd7c843ef265a2327dc7c`.
- Base: that tree after one `cargo test --no-run --workspace` (dev profile): 36.4 s, 121 units, 15 workspace crates, 10.59 GB logical, about 3.5 GB on disk after zstd.
- Idle df noise before the run: 0 KB over 60 s.
- No foreign `cargo` or `rustc` process was seen at any step (the `foreign=` note in every row).
- Other agents' cowfs daemons were running (18 `cowfs serve` processes, not six); none was touched.

## Arms

- A, plain copy: `cp --reflink=never -a base slot-N`.
- B, reflink clone: `cp --reflink=always -a base slot-N`.
- C, cowfs over FUSE: release `cowfs` built from the pinned commit in a scratch copy, a private daemon with its own store, mount and socket under the work dir, `cowfs import base --name exp-base`, then `cowfs snapshot create slot-N --from exp-base`, builds in `<mount>/slot-N`.
- D, overlayfs: `unshare -Urm mount -t overlay overlay -o lowerdir=base,upperdir=up-N,workdir=wk-N,userxattr base`, mounted over the base path itself in a private mount namespace, one `unshare` call per slot per round.

Deviations from the brief, with reasons:

- `cp -R` was replaced by `cp -a` with an explicit reflink flag.
  coreutils 9.12 `cp` reflinks by default on btrfs, so bare `cp -R` would make arm A a second arm B (proven on a 50 MB file: default cp used 0 KB of df, `--reflink=never` used 48.8 MB).
  Bare `cp -R` also resets mtimes, which would make cargo rebuild for a timestamp reason and spoil the different-path test.
- Reflink proof: a 10 MB file reflinked left df unchanged and `btrfs filesystem du` showed 0 exclusive and 9.54 MiB shared.
- Unprivileged overlayfs works on this kernel (`userxattr`), including mounting over the lower directory's own path, so arm D ran.

Leaf crates for R2, by fewest workspace dependents from `cargo metadata` (ties broken by name), slot 1 to 8:
cowfs-cli (0), cowfs-treehouse (0), cowfs-daemon (1), cowfs-fuse (2), cowfs-nfs (2), cowfs-ctl (3), nfsserve (3), cowfs-core (5).

Edits:

- R2 appends `// exp slot K` to `crates/<leaf>/src/lib.rs`.
- R3 appends `/// exp` and `pub fn exp_slot_K() -> u64 { K }` to `crates/cowfs-store/src/lib.rs`.
- R4 runs `cargo clean` then the full build in slots 1 to 4.
- R6 runs `cargo test -p cowfs-snapname` in slots 1 to 3.

## Arm C was trimmed by lead decision

Arm C ran R1 with 8 slots, R2 with 8 slots, R3 with 2 slots, no R4, and R6 with 3 slots.
The lead first cut C to R3 with 3 slots and R4 with 2 slots, then cut further to stop after the in-flight R3 slot and skip R4.
The reason given: a separate metadata-write micro-benchmark on a cowfs mount measured about 100 ms per create, mkdir and rename and 21 ms per unlink (stat and reads were fine), so a full rebuild is tens of thousands of such operations, hours per slot.
That micro-benchmark is the lead's, not part of this run.
R3 slot 2 was in flight and finished; slot 3 never started.
All rows already measured are kept.

## Results

Times are wall seconds; the poll loop gives about 1 s resolution.
Disk is df on `/mnt/docs`, in GB (10^9 bytes, on disk after btrfs zstd), summed over the round's steps.

| Arm | R1 create (8) | R2 small edit (8) | R3 core edit | R4 clean rebuild (4) | R6 sanity (3) |
|---|---|---|---|---|---|
| A copy | 87.0 s, 28.08 GB | 59.8 s, 1.37 GB | 204.6 s (8), 4.85 GB | 204.3 s, -2.83 GB | 6.0 s, pass |
| B reflink | 8.0 s, 0.00 GB | 54.3 s, 4.48 GB | 203.6 s (8), 16.31 GB | 204.4 s, 4.33 GB | 6.0 s, pass |
| C cowfs | 79.2 s import + 8 x 1.0 s, 2.65 GB | 3393.2 s, 118.4 GB before gc | 3353.0 s (2), 92.1 GB before gc | not run | 7.0 s, pass |
| D overlay | 8.0 s, 0 GB | 22.1 s, 4.25 GB | 103.7 s (8), 14.95 GB | 204.9 s, 5.08 GB | 6.0 s, pass |

Per-slot seconds:

| Arm | R2 slots 1 to 8 | R3 per slot | R4 per slot |
|---|---|---|---|
| A | 2.0, 3.0, 5.1, 7.1, 6.1, 10.1, 7.1, 19.3 | 21.3 to 27.4 | 50.5 to 51.6 |
| B | 2.0, 2.0, 4.0, 5.0, 6.0, 10.1, 7.0, 18.2 | 22.3 to 27.4 | 50.6 to 51.6 |
| C | 106.2, 41.1, 372.9, 409.0, 256.6, 472.2, 283.0, 1452.2 | 1653.4, 1699.6 | not run |
| D | 2.0, 2.0, 2.0, 3.0, 2.0, 3.0, 2.0, 6.1 | 12.2 to 13.3 | 50.9 to 52.0 |

Units compiled per R2 slot were identical in every arm: 1, 1, 2, 3, 1, 4, 2, 5 of 121.
R3 compiled 7 units per slot and R4 121 units per slot in every arm that ran them.
All R6 runs passed (cowfs-snapname, 7 tests).

### cowfs store, before and after gc

`cowfs gc` ran after R2 and after each R3 slot; stored_bytes before and after:

| Point | stored_bytes | logical_bytes (raw) | gc time |
|---|---|---|---|
| after import | 2.67 GB | 9.56 GB | |
| after R2, before gc | 123.84 GB | 947.64 GB | |
| after R2 gc | 6.22 GB | 27.99 GB | 459.8 s, 109.5 GiB reclaimed |
| R3 slot 1, before gc | 67.29 GB | 504.89 GB | |
| R3 slot 1, after gc | 6.88 GB | 28.27 GB | 175.6 s |
| R3 slot 2, before gc | 68.54 GB | 509.55 GB | |
| R3 slot 2, after gc | 7.31 GB | 27.17 GB | 179.6 s |
| final | 7.33 GB | 27.21 GB | |

Import reported 21,592 files, 10.5 GiB, verified by hash, store 2.5 GiB (23.6% of the source); mtimes were preserved.
logical_bytes is reported raw; its value before gc (947 GB for 9 snapshots of about 10.6 GB) matches no obvious sum, so no ratio here uses it.
Almost all of C's growth before gc was garbage: about 15 GB per R2 slot and about 61 GB per R3 slot written, against about 0.5 GB live per R3 slot after gc.
Write amplification before gc against B on the same edit, per R2 slot by df: about 40x (slot 1, 4.58 vs 0.11 GB), 10x (slot 2), 40x (slot 3), 30x (slot 4).

### Different path (R5)

- Every R2 slot in A, B and C is at a new path, and every one rebuilt at most 5 of 121 units (at most 4.1%), the same counts as D, which builds at the base's own path.
- Wall time for those incremental builds was 2 to 19 s in A and B, against 36.4 s for the base build.
- So the more-than-50% trigger was not hit, and the `--remap-path-prefix` repeat was not required and not run.
- Cargo's output file hashes are the same at every path (for example `libcowfs_vfs-0309d08c289c419a.rlib` in every slot), which is why the build stays incremental.
- The bytes are not the same across paths. After R4, `libcowfs_vfs`, `libcowfs_snapname` and even third-party `libserde` differ between `a/slot-1`, `a/slot-2` and `b/slot-1`, and the workspace rlib embeds the slot path 29 times.
- Arm D sees the base's own path in every slot (the overlay sits over `base` in a private namespace).
  After R4, D's `libserde` was byte-identical to the base and across slots, but the workspace `libcowfs_vfs` still differed (cause not checked).
- Unverified hypothesis: D's R3 at 12 to 13 s against 21 to 27 s in A and B is a same-path benefit, perhaps rustc's incremental cache.
  Same edit and same unit count, but this was not profiled.

## Pre-registered rules, evaluated

Savings are against arm A (plain copy) as the do-nothing baseline.

1. **Clones reach at least 70% of cowfs's space saving across R2 to R4, and builds are not slower than cowfs: clones win.**
   - Speed half: met by a wide margin. C's R2 took 3393 s against B's 54 s and D's 22 s, and C's R3 per slot took 1653 to 1700 s against 22 to 27 s.
   - Disk half, measured at the end of R2 (8 slots in every arm), using A at 29.45 GB:

     | Arm | Disk at end of R2 | Saving against A |
     |---|---|---|
     | B | 4.48 GB | 24.97 GB |
     | D | 4.25 GB | 25.2 GB |
     | C after gc (store size 6.82 GB) | 6.82 GB | 22.6 GB |
     | C before gc | 121 GB | -91.6 GB |

     B's saving is 110% of C's after gc, so the 70% bar is met.
   - Disk half through R3, as a projection because C ran only 2 R3 slots. C's live store grew about 0.5 GB per R3 slot after gc, against B's 2.04 GB per slot and D's 1.87 GB per slot. Extrapolated to 8 slots:

     | Arm | Disk through R3 | Saving against A (34.30 GB) | Share of C's saving |
     |---|---|---|---|
     | C after gc | about 10.8 GB | about 23.5 GB | |
     | B | 20.79 GB | 13.5 GB | about 57% |
     | D | 19.20 GB | 15.1 GB | about 64% |

     So the disk half fails through R3 if cowfs is credited with gc after every build. Without gc, C uses more disk than plain copies.
   - R4 is missing for C, so "across R2 to R4" cannot be evaluated in full.
   - Net: clones win on build time outright. On disk they win against cowfs without frequent gc, and they fall short of 70% against cowfs with gc after every slot (projection).
2. **cowfs saves at least 2x more disk than clones in R4: cowfs wins that case.**
   - UNEVALUATED for cowfs, because R4 was not run for arm C by lead decision.
   - R4 numbers for the other arms (4 slots, df): A -2.83 GB (cargo clean freed more than the rebuild wrote), B +4.33 GB, D +5.08 GB, each about 51 s per slot.
   - Byte identity bears on this rule: outputs rebuilt at different paths differ byte for byte, so block-level dedup across slots at different paths would not catch them. At the same path (D), third-party rlibs were identical and a workspace rlib was not.
3. **A clone at a new path rebuilds more than 50% of units: the clone warm-cache benefit is mostly gone.**
   - Not triggered. At most 5 of 121 units (4.1%) rebuilt at a new path, in A, B and C alike. The warm cache survives the path change.

## Anomalies

- The first arm C attempt ran its daemon under SCHED_IDLE with the idle I/O class.
  It inherited this from ananicy-cpp's `sshd` rule (type BG_CPUIO: nice 16, sched idle, ioclass idle), while rustc ran at nice -4.
  I stopped it after 3 R2 slots, kept its rows as `Cidle`, discarded its store, and restarted the daemon under `systemd-run --user` (SCHED_OTHER, nice 0).
  Its partially built slot 4 has no row.
  The restart did not change the times: R2 slots 1 to 3 took 101.2, 43.1 and 366.9 s idle against 106.2, 41.1 and 372.9 s normal, so scheduling was not the cause of C's slowness.
- Cargo and rustc in every arm ran from the ssh session, so they carried the same ananicy classes in every arm.
- The load numbers in the notes column are the experiment's own: no foreign cargo or rustc ran.
  During C's R2 the load reached about 240 (it was 172 at one reading), with I/O pressure `full avg10` about 84% and the cowfs daemon the only busy process.
  Load was near idle (0.7 to 10) at the start of R3 slots while the mount was the bottleneck.
  The slowness is reported, not profiled or fixed.
- The step runner changed mid-arm C, after R2 slot 7.
  The no-progress stall rule became "10 minutes with no log growth and no daemon CPU progress", because a FUSE link step was silent for over 18 minutes while the daemon was busy.
  The disk floor was also moved into the poll loop, and a hold gate was added between slots.
  No step stalled or tripped the floor.
- `btrfs filesystem du` mixes logical and compressed bytes under zstd. For B after R1, "set shared" read 3.74 GB, about the compressed base, while A's "exclusive" equalled the logical size.
  So df is the primary disk number and `btrfs filesystem du` is in the raw CSV only.
- df lagged after the large gc frees: after R2's gc it showed 18.2 GB used against the post-import point, while the store was 6.82 GB.
  The gap shrank later without any action of mine, so for C after gc the store size is used, not df.
  /mnt/docs also holds other agents' trees, so df drift from them cannot be excluded.
- The R1 df figures for C and D in the raw CSV include frees from earlier teardown and gc. The C import cost (2.65 GB) is taken from the pre-import and post-import measurements.
- The overlay work dirs (`wk-N/work`) are owned by the namespace's mapped root, so `du` could not read them, and deleting them needed `unshare -Ur`.

## Not verified

- Rule 2 for cowfs (no R4 in C).
- C's R3 on 8 slots; the figures above extrapolate from 2 slots.
- C's on-disk size after btrfs compression: its store size is apparent bytes, so it is an upper bound.
- The cause of C's slowness and write amplification (out of scope by lead decision; the lead's micro-benchmark points at metadata-write latency).
- Why D's R3 is about 2x faster than A and B's, and why a workspace rlib differs even at the same path.

## Raw data

results.csv (per step):

```csv
arm,round,slot,seconds,compiling,ws_compiling,df_free_kb,rc,status,notes
base,R0,0,36.4,121,15,615644212,0,ok,load=0.96 foreign=0
A,R1,1,10.0,0,0,612133872,0,ok,load=11.60 foreign=0
A,R1,2,11.0,0,0,608623532,0,ok,load=10.79 foreign=0
A,R1,3,11.0,0,0,605113192,0,ok,load=9.50 foreign=0
A,R1,4,11.0,0,0,601602852,0,ok,load=8.80 foreign=0
A,R1,5,11.0,0,0,598092512,0,ok,load=8.51 foreign=0
A,R1,6,11.0,0,0,594582172,0,ok,load=8.25 foreign=0
A,R1,7,11.0,0,0,591071832,0,ok,load=7.70 foreign=0
A,R1,8,11.0,0,0,587561492,0,ok,load=6.98 foreign=0
A,R2,1,2.0,1,1,587541740,0,ok,leaf=cowfs-cli load=6.61 foreign=0
A,R2,2,3.0,1,1,587475076,0,ok,leaf=cowfs-treehouse load=6.61 foreign=0
A,R2,3,5.1,2,2,587383360,0,ok,leaf=cowfs-daemon load=6.08 foreign=0
A,R2,4,7.1,3,3,587232896,0,ok,leaf=cowfs-fuse load=7.04 foreign=0
A,R2,5,6.1,1,1,587089548,0,ok,leaf=cowfs-nfs load=12.08 foreign=0
A,R2,6,10.1,4,4,586844516,0,ok,leaf=cowfs-ctl load=12.47 foreign=0
A,R2,7,7.1,2,2,586652380,0,ok,leaf=nfsserve load=15.03 foreign=0
A,R2,8,19.3,5,5,586191184,0,ok,leaf=cowfs-core load=17.51 foreign=0
A,R3,1,26.3,7,7,585503928,0,ok,cowfs-store load=24.55 foreign=0
A,R3,2,26.3,7,7,584792668,0,ok,cowfs-store load=28.18 foreign=0
A,R3,3,25.3,7,7,584192136,0,ok,cowfs-store load=28.85 foreign=0
A,R3,4,25.4,7,7,583641072,0,ok,cowfs-store load=28.19 foreign=0
A,R3,5,27.4,7,7,582930532,0,ok,cowfs-store load=32.60 foreign=0
A,R3,6,25.3,7,7,582329488,0,ok,cowfs-store load=37.04 foreign=0
A,R3,7,27.3,7,7,581593368,0,ok,cowfs-store load=36.58 foreign=0
A,R3,8,21.3,7,7,581340656,0,ok,cowfs-store load=36.35 foreign=0
A,R4,1,50.6,121,15,582045796,0,ok,clean+full load=32.62 foreign=0
A,R4,2,51.6,121,15,582819224,0,ok,clean+full load=32.71 foreign=0
A,R4,3,51.6,121,15,583473620,0,ok,clean+full load=34.18 foreign=0
A,R4,4,50.5,121,15,584169280,0,ok,clean+full load=32.47 foreign=0
A,R6,1,2.0,3,1,584157940,0,ok,snapname load=29.16 foreign=0
A,R6,2,2.0,3,1,584146604,0,ok,snapname load=27.23 foreign=0
A,R6,3,2.0,3,1,584135268,0,ok,snapname load=27.23 foreign=0
B,R1,1,1.0,0,0,584135268,0,ok,load=21.34 foreign=0
B,R1,2,1.0,0,0,584135268,0,ok,load=21.34 foreign=0
B,R1,3,1.0,0,0,584135268,0,ok,load=19.79 foreign=0
B,R1,4,1.0,0,0,584135268,0,ok,load=19.79 foreign=0
B,R1,5,1.0,0,0,584135268,0,ok,load=19.79 foreign=0
B,R1,6,1.0,0,0,584135268,0,ok,load=19.79 foreign=0
B,R1,7,1.0,0,0,584135268,0,ok,load=18.20 foreign=0
B,R1,8,1.0,0,0,584135268,0,ok,load=18.20 foreign=0
B,R2,1,2.0,1,1,584024264,0,ok,leaf=cowfs-cli load=18.20 foreign=0
B,R2,2,2.0,1,1,583859744,0,ok,leaf=cowfs-treehouse load=16.83 foreign=0
B,R2,3,4.0,2,2,583480184,0,ok,leaf=cowfs-daemon load=16.83 foreign=0
B,R2,4,5.0,3,3,582973896,0,ok,leaf=cowfs-fuse load=15.80 foreign=0
B,R2,5,6.0,1,1,582531532,0,ok,leaf=cowfs-nfs load=18.06 foreign=0
B,R2,6,10.1,4,4,581846972,0,ok,leaf=cowfs-ctl load=16.85 foreign=0
B,R2,7,7.0,2,2,581345256,0,ok,leaf=nfsserve load=16.13 foreign=0
B,R2,8,18.2,5,5,579652776,0,ok,leaf=cowfs-core load=19.72 foreign=0
B,R3,1,26.3,7,7,577386512,0,ok,cowfs-store load=23.16 foreign=0
B,R3,2,27.4,7,7,575058104,0,ok,cowfs-store load=28.05 foreign=0
B,R3,3,25.3,7,7,573029352,0,ok,cowfs-store load=27.46 foreign=0
B,R3,4,24.3,7,7,571068048,0,ok,cowfs-store load=31.13 foreign=0
B,R3,5,26.3,7,7,568739084,0,ok,cowfs-store load=28.78 foreign=0
B,R3,6,25.3,7,7,566710320,0,ok,cowfs-store load=31.75 foreign=0
B,R3,7,26.4,7,7,564382180,0,ok,cowfs-store load=35.48 foreign=0
B,R3,8,22.3,7,7,563338656,0,ok,cowfs-store load=37.52 foreign=0
B,R4,1,51.6,121,15,562215096,0,ok,clean+full load=32.32 foreign=0
B,R4,2,50.6,121,15,561206784,0,ok,clean+full load=32.85 foreign=0
B,R4,3,50.6,121,15,560041136,0,ok,clean+full load=29.14 foreign=0
B,R4,4,51.6,121,15,559007844,0,ok,clean+full load=31.18 foreign=0
B,R6,1,2.0,3,1,558996508,0,ok,snapname load=33.02 foreign=0
B,R6,2,2.0,3,1,558985172,0,ok,snapname load=30.37 foreign=0
B,R6,3,2.0,3,1,558973836,0,ok,snapname load=30.37 foreign=0
Cidle,R1,0,78.2,0,0,556324312,0,ok,import base 14.44
Cidle,R1,1,1.0,0,0,556324304,0,ok,load=4.12 foreign=0
Cidle,R1,2,1.0,0,0,556324304,0,ok,load=4.12 foreign=0
Cidle,R1,3,1.0,0,0,556324304,0,ok,load=4.12 foreign=0
Cidle,R1,4,1.0,0,0,556324292,0,ok,load=4.12 foreign=0
Cidle,R1,5,1.0,0,0,556324296,0,ok,load=4.43 foreign=0
Cidle,R1,6,1.0,0,0,556324296,0,ok,load=4.43 foreign=0
Cidle,R1,7,1.0,0,0,556324296,0,ok,load=4.43 foreign=0
Cidle,R1,8,1.0,0,0,556324296,0,ok,load=4.43 foreign=0
Cidle,R2,1,101.2,1,1,551879576,0,ok,leaf=cowfs-cli load=4.43 foreign=0
Cidle,R2,2,43.1,1,1,550122440,0,ok,leaf=cowfs-treehouse load=32.03 foreign=0
Cidle,R2,3,366.9,2,2,535448608,0,ok,leaf=cowfs-daemon load=72.60 foreign=0
C,R1,0,79.2,0,0,556324452,0,ok,import base load=83.60
C,R1,1,1.0,0,0,556324448,0,ok,load=25.30 foreign=0
C,R1,2,1.0,0,0,556324448,0,ok,load=23.27 foreign=0
C,R1,3,1.0,0,0,556324448,0,ok,load=23.27 foreign=0
C,R1,4,1.0,0,0,556324452,0,ok,load=23.27 foreign=0
C,R1,5,1.0,0,0,556324452,0,ok,load=23.27 foreign=0
C,R1,6,1.0,0,0,556324452,0,ok,load=21.49 foreign=0
C,R1,7,1.0,0,0,556324452,0,ok,load=21.49 foreign=0
C,R1,8,1.0,0,0,556324448,0,ok,load=21.49 foreign=0
C,R2,1,106.2,1,1,551748272,0,ok,leaf=cowfs-cli load=21.49 foreign=0
C,R2,2,41.1,1,1,550090172,0,ok,leaf=cowfs-treehouse load=39.08 foreign=0
C,R2,3,372.9,2,2,535376192,0,ok,leaf=cowfs-daemon load=73.18 foreign=0
C,R2,4,409.0,3,3,520715992,0,ok,leaf=cowfs-fuse load=124.49 foreign=0
C,R2,5,256.6,1,1,511581940,0,ok,leaf=cowfs-nfs load=129.30 foreign=0
C,R2,6,472.2,4,4,495757368,0,ok,leaf=cowfs-ctl load=145.21 foreign=0
C,R2,7,283.0,2,2,486609420,0,ok,leaf=nfsserve load=96.24 foreign=0
C,R2,8,1452.2,5,5,437930444,0,ok,leaf=cowfs-core load=129.20 foreign=0
C,R2-gc,0,459.8,0,0,538129680,0,ok,gc
C,R3,1,1653.4,7,7,492622680,0,ok,cowfs-store load=0.70 foreign=0
C,R3-gc,1,175.6,0,0,538131468,0,ok,gc
C,R3,2,1699.6,7,7,491524744,0,ok,cowfs-store load=10.32 foreign=0
C,R3-gc,2,179.6,0,0,544214056,0,ok,gc
C,R6,1,3.0,3,1,544205372,0,ok,snapname load=8.32
C,R6,2,2.0,3,1,544204084,0,ok,snapname
C,R6,3,2.0,3,1,544200600,0,ok,snapname
D,R1,1,1.0,0,0,551515892,0,ok,load=5.47 foreign=0
D,R1,2,1.0,0,0,551515892,0,ok,load=5.47 foreign=0
D,R1,3,1.0,0,0,551515892,0,ok,load=5.47 foreign=0
D,R1,4,1.0,0,0,551515892,0,ok,load=5.03 foreign=0
D,R1,5,1.0,0,0,551515892,0,ok,load=5.03 foreign=0
D,R1,6,1.0,0,0,551515892,0,ok,load=5.03 foreign=0
D,R1,7,1.0,0,0,551515892,0,ok,load=5.03 foreign=0
D,R1,8,1.0,0,0,551515892,0,ok,load=5.03 foreign=0
D,R2,1,2.0,1,1,551408600,0,ok,leaf=cowfs-cli load=4.79 foreign=0
D,R2,2,2.0,1,1,551257912,0,ok,leaf=cowfs-treehouse load=4.79 foreign=0
D,R2,3,2.0,2,2,550900468,0,ok,leaf=cowfs-daemon load=4.79 foreign=0
D,R2,4,3.0,3,3,550419428,0,ok,leaf=cowfs-fuse load=4.72 foreign=0
D,R2,5,2.0,1,1,550003188,0,ok,leaf=cowfs-nfs load=4.72 foreign=0
D,R2,6,3.0,4,4,549306752,0,ok,leaf=cowfs-ctl load=4.51 foreign=0
D,R2,7,2.0,2,2,548840844,0,ok,leaf=nfsserve load=4.14 foreign=0
D,R2,8,6.1,5,5,547264076,0,ok,leaf=cowfs-core load=4.14 foreign=0
D,R3,1,12.2,7,7,545174344,0,ok,cowfs-store load=8.94 foreign=0
D,R3,2,13.2,7,7,543025816,0,ok,cowfs-store load=15.77 foreign=0
D,R3,3,12.2,7,7,541162884,0,ok,cowfs-store load=22.34 foreign=0
D,R3,4,13.2,7,7,539376012,0,ok,cowfs-store load=30.44 foreign=0
D,R3,5,13.2,7,7,537213032,0,ok,cowfs-store load=32.28 foreign=0
D,R3,6,13.3,7,7,535378144,0,ok,cowfs-store load=41.83 foreign=0
D,R3,7,13.2,7,7,533227032,0,ok,cowfs-store load=44.06 foreign=0
D,R3,8,13.2,7,7,532315432,0,ok,cowfs-store load=44.24 foreign=0
D,R4,1,52.0,121,15,531011920,0,ok,clean+full load=43.36 foreign=0
D,R4,2,50.9,121,15,529810548,0,ok,clean+full load=43.16 foreign=0
D,R4,3,50.9,121,15,528528592,0,ok,clean+full load=37.24 foreign=0
D,R4,4,51.1,121,15,527236148,0,ok,clean+full load=39.04 foreign=0
D,R6,1,2.0,3,1,527224812,0,ok,snapname load=33.85 foreign=0
D,R6,2,2.0,3,1,527213476,0,ok,snapname load=33.85 foreign=0
D,R6,3,2.0,3,1,527202136,0,ok,snapname load=31.22 foreign=0
```

disk.csv (after each round; for C also per slot and around gc; `btrfs_*` columns are raw `btrfs filesystem du -s --raw`):

```csv
arm,round,df_free_kb,btrfs_total,btrfs_excl,btrfs_set_shared,du_apparent,cowfs_stored;logical
base,R0,615644212,10593923072,10593923072,0,10573974357,
A,R1,587561492,84751482880,84751482880,0,84591794856,
A,R2,586191184,87097712640,87097712640,0,86902532968,
A,R3,581340656,95626588160,95626588160,0,95314784501,
A,R4,584169280,90460721152,90460721152,0,90224187223,
A,R6,584135268,90550140928,90550140928,0,90312783309,
B,R1,584135268,84751384576,0,3742420992,84591794856,
B,R2,579652776,87098023936,13849739264,3742404608,86902532998,
B,R3,563338656,95626272768,62850961408,2202083328,95314784910,
B,R4,559007844,90460684288,75548577792,2202083328,90224186860,
B,R6,558973836,90550104064,75637997568,2202083328,90312782931,
Cidle,pre-import,558973816,36864,36864,0,1056855,
Cidle,post-import,556324312,2695106560,2695106560,0,2702252805,
Cidle,post-import-2,556324312,2695106560,2695106560,0,2702252805,2668562974;9556345734
Cidle,R1,556324296,2695106560,2695106560,0,2702252805,2668562974;9556345734
Cidle,R2-partial-idle,532363980,27206590464,27206590464,0,27233743172,27166366365;199534240703
C,pre-import,558973788,16384,16384,0,1056855,0;0
C,post-import,556324448,2695258112,2695258112,0,2702459526,2668769695;9556714596
C,R1,556324448,2695258112,2695258112,0,2702459526,2668769695;9556714596
C,R2,437930444,123906416640,123906416640,0,123972040252,123837286677;947635648664
C,R2-postgc,538129680,6818369536,6818369536,0,6884052267,6219245441;27991056198
C,R3-s1,492622680,67904897024,67904897024,0,67952298385,67287487927;504888496969
C,R3-s1-postgc,538131468,7350407168,7350407168,0,7397789594,6878494626;28273850197
C,R3-s2,491524744,69029928960,69029928960,0,69057377565,68538078917;509554255884
C,R3-s2-postgc,544214056,7834275840,7834275840,0,7861751185,7312788892;27165685448
C,final,544200600,7848030208,7848030208,0,7875503890,7326541597;27213798461
D,R1,551515892,0,0,0,0,
D,R2,547264076,14970605568,12624220160,1054461952,14950683945,
D,R3,532315432,66746187776,56128761856,1346850816,66647177377,
D,R4,527236148,77419945984,71907135488,1330782208,77293477223,
D,R6,527202132,77509365760,71996555264,1330782208,77382070124,
A,final,527202132,90550140928,90550140928,0,90312783309,
B,final,527202132,90550104064,75637997568,2202083328,90312782931,
D,final,527202132,77509365760,71996555264,1330782208,77382070124,
```
