# Clone vs cowfs workspaces on macOS (2026-10-10)

Question: can plain OS copy-on-write clones replace the cowfs mount for many cheap workspaces of a Rust project?
Judged on real disk used, build time, and whether built files survive a different folder path.
A sibling agent ran the same experiment on Linux; this report covers the Mac only.

## Setup

- Project: fresh `git clone https://github.com/zeeshanhaque21/cowfs.git`, pinned at `65a551957134c361c70fd7c843ef265a2327dc7c`.
- Base: that tree after one `cargo test --no-run --workspace` (dev profile, target inside the tree).
- Base build: 18.8 s wall, 113 `Compiling` lines, `target/` 3.9 GiB (du), 4.85 GB of file bytes per `cowfs import`.
- Machine: Apple M3 Max, 16 cores, 128 GiB RAM, macOS 26.6.2 (25G83), APFS Data volume.
- Toolchain: rustc 1.99.0 (b940084d7 2026-09-28), cargo 1.99.0 (5f94df478 2026-08-27), no sccache, no RUSTFLAGS.
- Free disk at start: 251 GB (df -k, Data volume); at end 139 GB (other agents also wrote to this volume).
- Arm A: `cp -R base slot-N`.
- Arm B: `cp -cR base slot-N` (APFS clone).
- Arm C: a private cowfs daemon (`spikes/nfs-loopback/out/live/bin`, cowfs sha1 d0b87f05, cowfs-daemon sha1 9ce9aa36) copied to scratch, `--backend core`, NFS loopback adapter.
- The private daemon lived under `/private/tmp/claude-501/cfx`, not the session scratchpad, because the scratchpad path pushes the socket past the macOS 104-byte Unix socket limit.
- The daemon also refuses a socket directory that is not mode 0700.
- The live daemon (pid 10824, `~/.cowfs`) was never touched.
- Arm C base: `cowfs import base --name exp-base`, 27.1 s, verified by hash, stored_bytes 1.23 GB, df delta 1.21 GiB.
- Slots: A and B under the scratch dir, C at `<mnt>/slot-N` from `cowfs snapshot create slot-N --from exp-base`.
- All slots sit at a path different from the base, so every first build in a slot is a different-path build.
- Driver: one Python script, steps strictly sequential, one build at a time, CSV appended and fsynced per step.
- Disk: `sync; sleep 10; df -k /System/Volumes/Data` after every step; cowfs `stored_bytes`/`logical_bytes` after every arm C step; no `du` on clone arms.
- Idle noise: two df reads 60 s apart differed by 1.8 MB.
- APFS clone check: a 1 GiB random file cloned with `cp -c` moved df by -0.5 MB, a plain copy by 1.00 GiB.
- No local Time Machine snapshots existed (`tmutil listlocalsnapshots /` empty).
- The machine was not quiet: other agents and apps ran throughout, idle load average 2.2 to 4.

## Leaf crates for R2

Chosen by transitive reverse dependents in the workspace (`cargo metadata --no-deps`, self-edges dropped), lowest first, ties alphabetical.

| slot | crate | transitive dependents |
| --- | --- | --- |
| 1 | cowfs-cli | 0 |
| 2 | cowfs-treehouse | 0 |
| 3 | cowfs-daemon | 1 |
| 4 | cowfs-fuse | 2 |
| 5 | cowfs-nfs | 2 |
| 6 | cowfs-ctl | 3 |
| 7 | nfsserve | 3 |
| 8 | cowfs-core | 5 |

R3 appended `// exp slot K` to `crates/cowfs-store/src/lib.rs` in every slot (6 transitive dependents), on top of the R2 edit.

## Scope changes by lead decision

- Arm C R3 ran slots 1 to 3 only, not 8, by lead decision, because each slot took 21 to 26 minutes.
- Arm C R4 was not run at all, by lead decision.
- Lead's reason: a metadata-write micro-benchmark on the mount (run by the lead, not by me) measured about 100 ms per create/mkdir/rename and 21 ms per unlink, so a clean plus full rebuild would take hours per slot.
- My arm C R4 `cargo clean` in slot 1 had run about 9.5 minutes when I stopped it on that decision; it exited within 5 s of SIGTERM, and slot 1's target is now partially cleaned.
- Rule 2 is therefore UNEVALUATED for cowfs.
- Arm C R6 ran on slots 2 and 3 (slot 1 was partially cleaned).
- Arms A and B ran every round in full.

## Per-arm per-round summary

Disk is the sum of per-step settled df deltas inside each contiguous run, in GiB (positive means space used).
Arm C also shows the change in cowfs `stored_bytes` (decimal GB).
Seconds are the mean wall time per slot.

| round | A s/slot | A disk GiB | B s/slot | B disk GiB | C s/slot | C disk GiB | C stored GB |
| --- | --- | --- | --- | --- | --- | --- | --- |
| R1 create 8 slots | 12.0 (upper bound) | 36.80 | 8.0 (upper bound) | 0.10 | 2.0 (upper bound) | 0.03 | 0.000 |
| R2 leaf edit, 8 slots | 17.6 | 11.43 | 4.6 | 4.71 | 276.3 (+184.7 rerun) | 1.00 | 0.922 |
| R3 store edit | 9.5 (8 slots) | -3.95 | 10.9 (8 slots) | 16.39 | 1346.5 (3 slots) | 1.18 | 1.261 |
| R4 clean (4 slots) | 7.8 | -22.01 | 5.9 | -9.65 | not run | | |
| R4 full build (4 slots) | 16.9 | 15.04 | 16.7 | 15.03 | not run | | |
| R4 net | | -6.97 | | 5.38 | not run | | |
| R6 snapname tests | 2.2 PASS 3/3 | | 2.1 PASS 3/3 | | 23.3 PASS 2/2 | | |

Compiling lines per slot in R2: A 74 in every slot; B 1,1,2,1,3,4,4,5; C 1,1,2,1,3,4,4,5 (same as B).
Compiling lines per slot in R3: 7 in every slot of every arm.
Compiling lines in R4: 113 in every slot (A and B), equal to the base build.
Arm C R2 per slot (s): 63.5, 115.8, 170.1, 42.2, 396.2, 408.0, 449.9, 564.6 (failed, see anomalies) plus a 184.7 s rerun.
Arm C R3 per slot (s): 1238 (reconstructed, see anomalies), 1264.7, 1536.8.
1-minute load during arm C builds was 3.3 to 6.1, about the idle baseline, while A and B builds drove it to 12 to 29.
So the CPU sat mostly idle during cowfs builds and the mount was the bottleneck; I did not profile it.

Cumulative footprint over the 8 working slots (A and B), measured after each round, in GiB:

| after | A | B | C |
| --- | --- | --- | --- |
| R1 | 36.80 | 0.10 | 1.24 (1.21 import + 0.03) |
| R2 | 48.23 | 4.81 | 2.24 |
| R3 | 44.28 | 21.20 | 3.42 measured with 3 slots; about 5.39 if 8 slots scale linearly (assumption, not measured) |
| R4 | 37.31 | 26.58 | not run |

Final cowfs state: logical_bytes 9.62 GB, stored_bytes 3.42 GB, store directory 3.36 GiB (du, plain directory), 9 snapshots.

## Rule evaluation

Rule 1 (clones reach at least 70% of cowfs's space saving across R2 to R4, and builds are not slower than cowfs): NOT MET on space; met on speed.
- R4 is missing for cowfs, so I compare at the end of R3, with saving defined as footprint below arm A.
- Saving after R3: B 44.28 - 21.20 = 23.08 GiB; C 44.28 - 5.39 = 38.89 GiB (C's 8-slot figure is the linear estimate from 3 slots).
- B reaches 59% of cowfs's saving, under the 70% bar.
- Using only the measured 3-slot C R3 growth per slot (0.39 GiB) against B's (2.05 GiB per slot) gives the same picture: B grows about 5x faster in R3.
- On R2 alone (all 8 slots measured), B grew 4.71 GiB against C's 1.00 GiB.
- Speed: clones are far faster, R2 4.6 s vs 276 s per slot, R3 10.9 s vs 1347 s per slot (60x to 120x).
- Taken together, the rule as written is not met because of the space clause.

Rule 2 (cowfs saves at least 2x more disk than clones in R4): UNEVALUATED, cowfs R4 was not run by lead decision.
- For reference, B's R4 on 4 slots: clean freed 9.65 GiB, full build wrote 15.03 GiB, net +5.38 GiB (about 3.76 GiB written per full rebuild).
- A's R4 on 4 slots: clean freed 22.01 GiB, build wrote 15.04 GiB, net -6.97 GiB.

Rule 3 (a clone at a new path rebuilds more than 50% of units): NOT TRIGGERED for clones.
- B (APFS clone, new path): at most 5 of 113 units (4.4%) in R2, the edited crate and its dependents only.
- C (cowfs, new path): the same counts as B.
- A (plain `cp -R`, not a clone) rebuilt 74 of 113 (65%) in every slot, and the cause is mtimes, not the path.
- A no-edit build in a fresh `cp -R` slot also compiled 74 units; `cargo -v` gave 74 `Dirty ... the dependency X was rebuilt` reasons.
- `cp -R` sets every mtime to copy time, while `cp -cR` and `cowfs import` keep source mtimes (checked with `stat -f %m`).
- The remap repeat changed nothing: with the base rebuilt under one identical RUSTFLAGS string `--remap-path-prefix=<base>=/ws --remap-path-prefix=<slot-10>=/ws --remap-path-prefix=<slot-11>=/ws`, two fresh `cp -R` slots still compiled 76 of 113.
- A per-slot remap string would change RUSTFLAGS and force a full rebuild by itself, which is why one identical string was used.

## Anomalies

- cowfs ld EEXIST: in arm C R2 slot 8, `ld` failed with `open() failed, errno=17 (File exists)` for `target/debug/deps/swap_provenance_124-2afb039026b4edb5`, although that bare file was absent from a directory listing right after.
- One diagnostic rerun passed (184.7 s, 4 crates recompiled); it happened once in 14 arm C builds; slot 8's R2 cost is both runs.
- Stall-rule false positive: the "no log growth for 10 min" rule fired on arm C R3 slot 1 at 18:00 elapsed.
- ps then showed 15 to 16 rustc processes in U state with new pids still being spawned, so the build was progressing; cargo prints nothing while test targets compile and link.
- The driver did not kill it; the build completed on its own at about 20.6 minutes with no errors and 168 `Executable` lines.
- Its row is reconstructed: seconds from ps etime start (plus or minus 1 s) to the log mtime.
- From then on every cargo call used `--message-format=json-render-diagnostics`, so each finished unit writes a line; the rule did not fire again.
- R1 times for slots 1 to 8 are upper bounds quantized to 2 s (driver polling bug, fixed before R2); later precise `cp -R` creates took 22 to 35 s with the page cache colder.
- In the pre-run smoke test, a fresh snapshot was not listed and the first write got ENOENT, then worked seconds later (likely a cached negative lookup from an earlier `ls`); R1 saw 0 s visibility wait.
- Arm A R3 freed 3.95 GiB and arm A R4 clean freed more than the rebuild wrote, because incremental-compilation session dirs accumulated in R2 were pruned or deleted.
- `cowfs status` logical and stored bytes did not change during the 9.5-minute partial `cargo clean`; I did not run gc and did not verify why.
- Free space fell from 251 GB to 139 GB over the run, partly from other agents on the same volume, so long arm C windows carry more df noise than the short A and B steps; stored_bytes is the cleaner arm C number.

## What I could not verify

- Arm C R4 and rule 2, not run by lead decision.
- Arm C R3 for slots 4 to 8; the 8-slot C footprint after R3 is a linear estimate.
- The root cause of the ld EEXIST, and whether it reproduces.
- Why cowfs builds are 60x to 120x slower; the lead's metadata micro-benchmark is the leading explanation, and I did not profile.
- Whether `cp -pR` (mtime-preserving plain copy) would make arm A incremental; not tested.

## Teardown

- Before shutdown: `cowfs ps` showed no processes for all 9 snapshots, and `lsof -n <mnt>` completed with no matches.
- `cowfs shutdown` returned `{}`, the daemon exited within 5 s, and the private mount was gone; the live mount was still present.
- No `umount -f` was used.

## Raw CSV

Columns: arm, round, slot, seconds, compiling_count, df_free_kb, notes.
Extra rounds: R2rerun (diagnostic rerun), R5diag (no-edit build in a fresh `cp -R` slot), R5remap-base and R2remap (remap repeat), R4clean (the clean step of R4).

```csv
arm,round,slot,seconds,compiling_count,df_free_kb,notes
A,R1,start,0.00,,244071580,load1=2.50
A,R1,1,12.04,,239260328,rc=0 load1=2.35
A,R1,2,12.04,,234431460,rc=0 load1=2.55
A,R1,3,12.04,,229604336,rc=0 load1=2.80
A,R1,4,12.03,,224777484,rc=0 load1=2.77
A,R1,5,12.04,,219981608,rc=0 load1=2.94
A,R1,6,12.05,,215136676,rc=0 load1=2.60
A,R1,7,12.05,,210308748,rc=0 load1=2.25
A,R1,8,12.04,,205482424,rc=0 load1=2.33
A,R1,end,0.00,,205482316,load1=2.21
B,R1,start,0.00,,205482264,load1=2.40
B,R1,1,8.03,,205487784,rc=0 load1=2.68
B,R1,2,8.03,,205470932,rc=0 load1=2.73
B,R1,3,8.03,,205453872,rc=0 load1=2.79
B,R1,4,8.02,,205446948,rc=0 load1=2.64
B,R1,5,8.03,,205428728,rc=0 load1=2.44
B,R1,6,8.03,,205410564,rc=0 load1=2.47
B,R1,7,8.02,,205393736,rc=0 load1=2.82
B,R1,8,8.02,,205376792,rc=0 load1=3.02
B,R1,end,0.00,,205376568,load1=2.87
C,R1,start,0.00,,205376668,load1=2.58 logical=3710135686 stored=1228528323
C,R1,1,2.01,,205376456,rc=0 load1=2.74 visible_wait=0.00s logical=3710135686 stored=1228528323
C,R1,2,2.01,,205376404,rc=0 load1=2.85 visible_wait=0.00s logical=3710135686 stored=1228528323
C,R1,3,2.01,,205376344,rc=0 load1=2.81 visible_wait=0.00s logical=3710135686 stored=1228528323
C,R1,4,2.00,,205344500,rc=0 load1=2.53 visible_wait=0.00s logical=3710135686 stored=1228528323
C,R1,5,2.01,,205353420,rc=0 load1=2.41 visible_wait=0.00s logical=3710135686 stored=1228528323
C,R1,6,2.01,,205352896,rc=0 load1=2.66 visible_wait=0.00s logical=3710135686 stored=1228528323
C,R1,7,2.01,,205334244,rc=0 load1=2.66 visible_wait=0.00s logical=3710135686 stored=1228528323
C,R1,8,2.00,,205342292,rc=0 load1=2.72 visible_wait=0.00s logical=3710135686 stored=1228528323
C,R1,end,0.00,,205342328,load1=2.46 logical=3710135686 stored=1228528323
C,R2,start,0.00,,205336964,load1=2.03 logical=3710135686 stored=1228528323
C,R2,1,63.46,1,205307524,rc=0 load1=2.74 edit=cowfs-cli logical=3769399240 stored=1247319405
C,R2,end,0.00,,205307276,load1=2.24 logical=3769399240 stored=1247319405
A,R2,start,0.00,,205312980,load1=2.43
A,R2,1,17.95,74,203779336,rc=0 load1=9.85 edit=cowfs-cli
A,R2,2,17.50,74,202241996,rc=0 load1=22.65 edit=cowfs-treehouse
A,R2,3,17.55,74,200745676,rc=0 load1=21.24 edit=cowfs-daemon
A,R2,4,17.34,74,199233540,rc=0 load1=29.17 edit=cowfs-fuse
A,R2,5,17.43,74,197703036,rc=0 load1=24.55 edit=cowfs-nfs
A,R2,6,17.95,74,196293080,rc=0 load1=24.81 edit=cowfs-ctl
A,R2,7,17.98,74,194801192,rc=0 load1=20.55 edit=nfsserve
A,R2,8,17.39,74,193324860,rc=0 load1=28.37 edit=cowfs-core
A,R2,end,0.00,,193324476,load1=21.14
B,R2,start,0.00,,193324420,load1=18.50
B,R2,1,3.47,1,193219024,rc=0 load1=17.34 edit=cowfs-cli
B,R2,2,2.73,1,193070852,rc=0 load1=14.57 edit=cowfs-treehouse
B,R2,3,4.16,2,192686468,rc=0 load1=13.05 edit=cowfs-daemon
B,R2,4,1.17,1,192655452,rc=0 load1=12.66 edit=cowfs-fuse
B,R2,5,5.87,3,191789636,rc=0 load1=11.76 edit=cowfs-nfs
B,R2,6,5.50,4,191075324,rc=0 load1=14.15 edit=cowfs-ctl
B,R2,7,6.50,4,190142668,rc=0 load1=13.37 edit=nfsserve
B,R2,8,7.77,5,188388580,rc=0 load1=12.86 edit=cowfs-core
B,R2,end,0.00,,188388560,load1=9.86
C,R2,start,0.00,,188387588,load1=8.79 logical=3769399240 stored=1247319405
C,R2,2,115.75,1,188433364,rc=0 load1=4.18 edit=cowfs-treehouse logical=3885012655 stored=1292257617
C,R2,3,170.06,2,188350780,rc=0 load1=4.43 edit=cowfs-daemon logical=4098077441 stored=1365713825
C,R2,4,42.24,1,188343908,rc=0 load1=4.27 edit=cowfs-fuse logical=4121029745 stored=1374697651
C,R2,5,396.22,3,188185900,rc=0 load1=6.06 edit=cowfs-nfs logical=4559498642 stored=1528117241
C,R2,6,407.99,4,187981860,rc=0 load1=5.82 edit=cowfs-ctl logical=4937418008 stored=1675573016
C,R2,7,449.87,4,187671664,rc=0 load1=3.89 edit=nfsserve logical=5390164818 stored=1845258313
C,R2,8,564.63,5,187438648,rc=101 load1=3.86 edit=cowfs-core logical=6067796280 stored=2073995138
C,R2rerun,start,0.00,,187437352,load1=3.40 logical=6067796280 stored=2073995138
C,R2rerun,8,184.72,4,187361108,rc=0 load1=3.32 diagnostic rerun after failure logical=6300965305 stored=2150533480
C,R2rerun,end,0.00,,187361468,load1=3.08 logical=6300965305 stored=2150533480
A,R3,start,0.00,,187361192,load1=3.89
A,R3,1,9.48,7,187879060,rc=0 load1=7.44 edit=cowfs-store
A,R3,2,9.44,7,188398780,rc=0 load1=7.72 edit=cowfs-store
A,R3,3,9.40,7,188914612,rc=0 load1=13.47 edit=cowfs-store
A,R3,4,9.49,7,189431488,rc=0 load1=14.61 edit=cowfs-store
A,R3,5,9.62,7,189954380,rc=0 load1=17.77 edit=cowfs-store
A,R3,6,9.85,7,190472720,rc=0 load1=20.39 edit=cowfs-store
A,R3,7,9.66,7,190988944,rc=0 load1=18.11 edit=cowfs-store
A,R3,8,9.43,7,191507548,rc=0 load1=16.21 edit=cowfs-store
A,R3,end,0.00,,191507404,load1=12.40
B,R3,start,0.00,,191507420,load1=10.88
B,R3,1,11.15,7,189135116,rc=0 load1=12.21 edit=cowfs-store
B,R3,2,11.12,7,186695060,rc=0 load1=21.38 edit=cowfs-store
B,R3,3,10.42,7,184498920,rc=0 load1=22.15 edit=cowfs-store
B,R3,4,10.98,7,182055644,rc=0 load1=22.68 edit=cowfs-store
B,R3,5,11.28,7,179878416,rc=0 load1=25.15 edit=cowfs-store
B,R3,6,10.84,7,177717564,rc=0 load1=23.77 edit=cowfs-store
B,R3,7,11.29,7,175565456,rc=0 load1=20.55 edit=cowfs-store
B,R3,8,10.46,7,174326372,rc=0 load1=20.65 edit=cowfs-store
B,R3,end,0.00,,174324432,load1=15.20
C,R3,start,0.00,,174334608,load1=13.17 logical=6300965305 stored=2150533480
C,R3,1,1238.00,7,173894720,rc=? (no error lines; Executable lines present) stall-rule false positive: driver exited at 18:00 while rustc still progressing; build completed on its own; seconds from ps etime start (+-1s) to log mtime; df/status taken after completion logical=7529983450 stored=2602099876
A,R4,start,0.00,,173879112,load1=3.29
A,R4clean,1,7.78,,179648300,rc=0 load1=4.15
A,R4,1,18.00,113,175701980,rc=0 load1=20.82
A,R4clean,2,7.81,,181472104,rc=0 load1=14.69
A,R4,2,16.69,113,177531152,rc=0 load1=21.80
A,R4clean,3,7.71,,183299900,rc=0 load1=15.66
A,R4,3,16.29,113,179354760,rc=0 load1=23.21
A,R4clean,4,7.88,,185125448,rc=0 load1=15.32
A,R4,4,16.50,113,181189480,rc=0 load1=20.61
A,R4,end,0.00,,181189444,load1=15.33
B,R4,start,0.00,,181189292,load1=13.36
B,R4clean,1,6.30,,183664024,rc=0 load1=11.07
B,R4,1,16.57,113,179728084,rc=0 load1=19.92
B,R4clean,2,5.94,,182333988,rc=0 load1=13.99
B,R4,2,16.39,113,178386520,rc=0 load1=22.94
B,R4clean,3,5.95,,180949412,rc=0 load1=14.80
B,R4,3,16.24,113,177003568,rc=0 load1=24.35
B,R4clean,4,5.57,,179478652,rc=0 load1=17.39
B,R4,4,17.63,113,175542904,rc=0 load1=21.11
B,R4,end,0.00,,175542856,load1=15.97
A,R6,start,0.00,,175542708,load1=13.91
A,R6,1,2.32,3,175525500,"rc=0 PASS results=[('ok', '7', '0'), ('ok', '0', '0')]"
A,R6,2,2.14,3,175511116,"rc=0 PASS results=[('ok', '7', '0'), ('ok', '0', '0')]"
A,R6,3,2.14,3,175493972,"rc=0 PASS results=[('ok', '7', '0'), ('ok', '0', '0')]"
A,R6,end,0.00,,175493932,load1=7.61
B,R6,start,0.00,,175492976,load1=6.60
B,R6,1,2.14,3,175475816,"rc=0 PASS results=[('ok', '7', '0'), ('ok', '0', '0')]"
B,R6,2,2.00,3,175461316,"rc=0 PASS results=[('ok', '7', '0'), ('ok', '0', '0')]"
B,R6,3,2.01,3,175444272,"rc=0 PASS results=[('ok', '7', '0'), ('ok', '0', '0')]"
B,R6,end,0.00,,175438608,load1=4.34
A,R1,start,0.00,,175435504,load1=4.12
A,R1,9,22.09,,170676872,rc=0 load1=3.40
A,R1,end,0.00,,170677812,load1=3.27
A,R5diag,start,0.00,,170677528,load1=2.92
A,R5diag,9,17.59,74,169149964,rc=0 load1=9.86 no-edit fingerprint diag
A,R5diag,end,0.00,,169162524,load1=7.90
A,R1,start,0.00,,169145232,load1=5.83
A,R1,12,22.33,,164356500,rc=0 load1=4.92
A,R1,end,0.00,,164356440,load1=5.29
A,R5diag,start,0.00,,164354976,load1=5.18
A,R5diag,12,17.88,74,162813060,rc=0 load1=8.32 no-edit diag -v
A,R5diag,end,0.00,,162813344,load1=6.53
base,R5remap-base,base,20.07,113,158899572,rc=0 base rebuilt with identical REMAP_FLAGS
A,R1,start,0.00,,158900640,load1=6.92
A,R1,10,33.61,,149451140,rc=0 load1=4.52
A,R1,11,35.14,,140070416,rc=0 load1=3.75
A,R1,end,0.00,,140070448,load1=3.40
A,R2remap,start,0.00,,140069500,load1=3.19
A,R2remap,10,20.90,76,140200360,rc=0 load1=12.71 edit=cowfs-treehouse
A,R2remap,11,20.18,76,140338252,rc=0 load1=17.06 edit=cowfs-daemon
A,R2remap,end,0.00,,140320676,load1=12.75
C,R3,start,0.00,,140314892,load1=10.72 logical=7529983450 stored=2602099876
C,R3,2,1264.73,7,139954848,rc=0 load1=3.26 edit=cowfs-store logical=8587374922 stored=3012636201
C,R3,3,1536.82,7,139513248,rc=0 load1=3.97 edit=cowfs-store logical=9591754684 stored=3411713664
C,R3,end,0.00,,,driver stopped before slot 4 by lead decision (trim to 3 slots); slot 4 not edited
C,R4,start,0.00,,139513388,load1=3.94 logical=9591754684 stored=3411713664
C,R4clean,1,~575,,,ABORTED by lead decision: cargo clean in slot-1 SIGTERMed after ~9.5 min (exited within 5s); slot-1 target now partially cleaned; C R4 not run
C,R6,start,0.00,,139083248,load1=4.32 logical=9591754684 stored=3411713664
C,R6,2,23.71,3,139082788,"rc=0 PASS results=[('ok', '7', '0'), ('ok', '0', '0')]"
C,R6,3,22.94,3,139081880,"rc=0 PASS results=[('ok', '7', '0'), ('ok', '0', '0')]"
C,R6,end,0.00,,139081728,load1=3.59 logical=9615589253 stored=3420746473
```
