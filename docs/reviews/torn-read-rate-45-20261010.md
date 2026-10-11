# Torn cached reads: cowfs FUSE mount against native btrfs (#45)

Date: 2026-10-10.
Machine: cachyos box, Linux 7.2.8-2-cachyos, 16 CPUs, native arm on the btrfs volume at /mnt/docs (nvme0n1p3).
Repo head measured: 65a5519.
Scratch directory /mnt/docs/Projects/cowfs-torn-measure, deleted after each session (main series, then the staggered rerun); no mount or process left behind (checked with `mount` and `pgrep`).

## Question

Earlier work (PR 249, PR 281) showed cached reads tear on tmpfs, btrfs and ext4, and that O_DIRECT readers of a cowfs mount saw 0 tears in about 78M reads (the tracker figure; the PR 249 mount in coherence.rs uses `file_flush_bytes` of 4096, not this run's mount).
It never measured whether the tear rate with ordinary cached reads is higher on a cowfs FUSE mount than on the native disk.
This note measures that, and decides whether to force direct I/O.

## Method

Workload: the shape of `crates/cowfs-fuse/tests/coherence.rs`, rewritten as a small C driver so both arms run the identical binary against a plain path.
3 writers `pwrite` 4096-byte uniform buffers, 3 readers `pread` the same ranges with ordinary buffered reads (no O_DIRECT), 10 s per run, 1 MiB file prefilled with byte 1.
Slot for iteration i of thread t is `(i*7+t) % nslots`, value `t*60 + i%50 + 2`, as in coherence.rs.
A read is torn when its 4096 bytes are not all equal.
After the writers stop, every slot is read again at rest; all 60 runs had 0 torn slots at rest.

Three write patterns:

- `aligned`: 16 slots at `s*4096` (the coherence.rs pattern).
- `cross4k`: 16 slots at `s*4096 + 2048`, so every write and read crosses a 4 KiB page boundary.
- `crosschunk`: 3 slots at `c*256KiB - 2048` for c = 1..3, so every write crosses a cowfs chunk boundary.
  The real chunker (`cowfs_store::chunks`) cuts the prefilled file at exactly 256, 512, 768 and 1024 KiB, and still does with each of 5 sample values (2, 62, 122, 51, 200) written into all 3 straddling slots (scratch program, text below).
  The workload writes up to 150 distinct values and different slots hold different values at once; those images were not checked.

Arms, alternated by rotating the order every rep:

1. `native`: btrfs directory on /mnt/docs.
2. `cowfs`: a real `Core` mounted through `cowfs_fuse::Mount`, store also on /mnt/docs, with `cowfs_core::Options::default()` and `MountOptions::default()`, the same options `cowfs-daemon` uses (`daemon.rs` open_handler, `mounts.rs`).
   Fresh store and mount per run.
3. `paced` (supplementary control): native btrfs with each writer paced by `clock_nanosleep` absolute deadlines to the cowfs arm's per-writer write rate from a pilot (9400, 8800 and 8000 writes/s for the three patterns).
   Added because the cowfs arm writes about 15x slower than native, so tears per read on its own mostly measures write speed.
   In this series all three paced writers shared one deadline clock, so they fired in phase.
4. `paced_staggered` (rerun after the main series, not interleaved with it): the same paced arm with writer t's first deadline offset by t/3 of a period, to rule out the in-phase confound.

5 reps per arm per pattern: 45 runs in the main series plus 15 staggered runs, every run reported below, none dropped.
Machine quiet: CPU idle 91 to 97 percent over 2 s before each run (37 of 45 samples at 96).
About six unrelated `cowfs` daemons from other work were running at 5 to 10 percent of one CPU each; they were left alone.

## Results

Cells are tears / reads = tears per million reads.

| Pattern | Arm | Run 1 | Run 2 | Run 3 | Run 4 | Run 5 | Median | Min to max | Median reads | Median writes |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| aligned | native | 64323 / 63.2M = 1017.1 | 50995 / 53.8M = 948.3 | 72147 / 59.2M = 1218.6 | 48984 / 52.7M = 928.6 | 43387 / 52.7M = 823.6 | 948.3 | 823.6 to 1218.6 | 53.8M | 3965K |
| aligned | cowfs | 8397 / 96.1M = 87.4 | 7306 / 83.6M = 87.4 | 7654 / 81.5M = 93.9 | 7420 / 84.4M = 87.9 | 7477 / 83.4M = 89.6 | 87.9 | 87.4 to 93.9 | 83.6M | 261K |
| aligned | paced | 1036 / 84.8M = 12.2 | 1287 / 79.0M = 16.3 | 900 / 79.4M = 11.3 | 974 / 80.0M = 12.2 | 1047 / 80.2M = 13.1 | 12.2 | 11.3 to 16.3 | 80.0M | 282K |
| cross4k | native | 89477 / 48.6M = 1841.3 | 52046 / 49.3M = 1055.4 | 83473 / 48.4M = 1723.7 | 92363 / 49.7M = 1858.4 | 90670 / 48.7M = 1862.6 | 1841.3 | 1055.4 to 1862.6 | 48.7M | 4167K |
| cross4k | cowfs | 9596 / 74.9M = 128.2 | 8536 / 65.6M = 130.1 | 8514 / 66.2M = 128.5 | 8713 / 66.2M = 131.7 | 8320 / 65.8M = 126.4 | 128.5 | 126.4 to 131.7 | 66.2M | 261K |
| cross4k | paced | 1053 / 78.7M = 13.4 | 1027 / 75.2M = 13.7 | 924 / 75.1M = 12.3 | 772 / 75.2M = 10.3 | 1518 / 76.0M = 20.0 | 13.4 | 10.3 to 20.0 | 75.2M | 264K |
| crosschunk | native | 5550199 / 44.0M = 126046.7 | 5604354 / 43.6M = 128412.3 | 5633640 / 44.1M = 127752.6 | 5502205 / 43.4M = 126810.8 | 5467392 / 43.7M = 124985.2 | 126810.8 | 124985.2 to 128412.3 | 43.7M | 2754K |
| crosschunk | cowfs | 55229 / 60.8M = 908.0 | 55823 / 62.0M = 900.2 | 55404 / 59.9M = 924.6 | 53169 / 61.2M = 869.4 | 54246 / 61.2M = 886.9 | 900.2 | 869.4 to 924.6 | 61.2M | 260K |
| crosschunk | paced | 781274 / 65.3M = 11972.3 | 727891 / 64.9M = 11215.0 | 722307 / 65.1M = 11094.1 | 711640 / 64.8M = 10979.6 | 736406 / 65.0M = 11332.9 | 11215.0 | 10979.6 to 11972.3 | 65.0M | 240K |

Reading the table:

Staggered paced rerun (writers offset by t/3 of a period), same cell format:

| Pattern | Run 1 | Run 2 | Run 3 | Run 4 | Run 5 | Median | Min to max | Median reads | Writes |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| aligned | 709 / 84.9M = 8.4 | 805 / 84.4M = 9.5 | 963 / 76.8M = 12.5 | 1034 / 75.8M = 13.6 | 804 / 76.1M = 10.6 | 10.6 | 8.4 to 13.6 | 76.8M | 282K |
| cross4k | 558 / 79.0M = 7.1 | 898 / 79.1M = 11.4 | 554 / 71.0M = 7.8 | 561 / 70.6M = 7.9 | 583 / 71.1M = 8.2 | 7.9 | 7.1 to 11.4 | 71.1M | 264K |
| crosschunk | 716101 / 68.1M = 10522.1 | 755340 / 67.7M = 11149.2 | 699285 / 61.7M = 11331.9 | 738332 / 62.0M = 11916.9 | 703650 / 61.0M = 11534.5 | 11331.9 | 10522.1 to 11916.9 | 62.0M | 240K |

- Same workload, cowfs against native btrfs: the cowfs mount tears less often per million reads in every pattern, by about 11x for `aligned` (87.9 against 948.3), 14x for `cross4k` (128.5 against 1841.3) and 141x for `crosschunk` (900.2 against 126810.8), at the median.
  The spreads do not overlap in any pattern.
  The main reason is that cowfs writes are about 15x slower, so there are far fewer write windows for a reader to land in.
- Matched write rate (paced control): for `aligned` and `cross4k` the cowfs mount tears about 7x and 10x more per million reads than in-phase paced btrfs (87.9 against 12.2, 128.5 against 13.4), and about 8x and 16x more than staggered paced btrfs (against 10.6 and 7.9), with non-overlapping spreads and similar read counts.
  Staggering the writers did not raise the paced rate, so the in-phase deadlines do not explain the gap.
  Inference, not instrumented: each FUSE write leaves a wider tear window in the page cache than a btrfs write does.
  The paced arm did about 8 percent more writes than cowfs for `aligned`, which would push its rate up, not down.
- For `crosschunk` the order flips: paced btrfs tears about 12x more than cowfs (11215.0 in phase, 11331.9 staggered, against 900.2), so phase is not the cause there either.
  Only 3 slots make this pattern far more contended than the others, so compare it only within the pattern, not against `cross4k`.
  Hypothesis, not checked: the 256 KiB offsets are also likely large-folio boundaries in the btrfs page cache, so a native write there updates two folios in separate steps, while a 4 KiB-crossing write inside one large folio does not; native btrfs shows about 3 tears per write in this pattern.
- Crossing a cowfs chunk boundary does not raise the cowfs rate relative to native in any arm.
- At rest, 0 torn slots in all 60 runs, on both filesystems.

## Verdict

With the same unthrottled workload, cached reads tear less often per million reads on a cowfs FUSE mount than on native btrfs (11x, 14x and 141x less for the three patterns); but at a matched write rate, which is closer to an application whose write rate is set by itself and not by the filesystem, cowfs tears about 7x to 16x more for page-aligned and page-crossing writes (about 90 to 130 per million reads against about 8 to 13), and still about 12x less for the chunk-crossing pattern.
Do not force direct I/O (FOPEN_DIRECT_IO): the tear exists on the native disk too, so no application can rely on buffered-read atomicity on Linux either way, and forcing direct I/O would make cowfs stricter than the native filesystems at the cost of the page cache, shared mmap and throughput, to bring a rate of roughly 90 to 900 per million reads to zero rather than to fix a cowfs fault.

The higher per-write rate is still a page-cache tear: O_DIRECT readers saw 0 tears through the PR 249 coherence.rs mount (a `Core` with `file_flush_bytes` 4096), which shows the daemon serves each READ atomically; O_DIRECT was not rerun against this run's default-options mount.
Its mechanism in the kernel FUSE write path was not investigated here; if it matters, that is the next measurement, not a direct I/O switch.

## Not verified

- Whether the straddling block was actually split across two stored chunks during a run.
  With a 1 MiB file and the default 4 MiB `file_flush_bytes`, writes may sit in the dirty overlay until the background flusher runs; buffered reads are served from the page cache either way.
- The kernel mechanism behind the wider per-write window on FUSE (for example page locking across the WRITE round trip, or uptodate clearing); not instrumented.
- The crosschunk inversion in the paced arms is reported, not explained; the large-folio explanation above is a hypothesis.
- The staggered paced rerun ran after the main series, not interleaved with the other arms.
- One machine, one kernel (7.2.8), btrfs only for the native arm; ext4, tmpfs and XFS were not rerun.
- The cowfs arm mounts a `Core` snapshot view through `cowfs_fuse::Mount` with the daemon's default options, not through the full `cowfs serve` daemon.
- The paced rates come from the pilot, so the paced arm matches the cowfs arm's write rate to within about 8 percent, not exactly.

## Driver text

All files lived only in the scratch directory and were deleted with it.
`coremount.rs` and `cuts.rs` were dropped into `crates/cowfs-fuse/examples/` and `crates/cowfs-store/examples/` of a scratch clone and built with `cargo build --release`; no production crate changed.
`tear.c` below is the final version; the main series ran without the `stagger` block (the first `if (rate)` in `writer`), which was added for the staggered rerun only.

### `tear.c`

```c
// Scratch only (issue #45 rate measurement). Same shape as crates/cowfs-fuse/tests/coherence.rs:
// W writers of whole 4 KiB uniform buffers, R readers of the same ranges with ordinary buffered
// pread (no O_DIRECT), slot = (i*7+t) % nslots, value = t*60 + i%50 + 2, prefill 1.
// usage: tear <file> <aligned|cross4k|crosschunk> <seconds> [writers=3] [readers=3] [writes/s/writer]
#define _GNU_SOURCE
#include <fcntl.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>
#include <sys/prctl.h>

#define LEN 4096
#define FILESZ (1 << 20)
static const char *path;
static long slots[16];
static int nslots;
static atomic_int stop;
static atomic_long reads, writes, tears;
static long rate;
static int nwriters;  // writes per second per writer, 0 = unpaced

static int xopen(int fl) {
  int fd = open(path, fl);
  if (fd < 0) { perror("open"); exit(2); }
  return fd;
}

static void *writer(void *a) {
  long t = (long)a, w = 0;
  int fd = xopen(O_RDWR);
  char buf[LEN];
  struct timespec next;
  clock_gettime(CLOCK_MONOTONIC, &next);
  if (rate) {  // stagger writers by t/W of a period so paced writers do not fire in phase
    next.tv_nsec += (1000000000L / rate) * t / nwriters;
    if (next.tv_nsec >= 1000000000L) { next.tv_sec++; next.tv_nsec -= 1000000000L; }
  }
  for (long i = 0; !atomic_load_explicit(&stop, memory_order_relaxed); i++) {
    if (rate) {  // absolute deadlines, so a late write does not push every later one back
      next.tv_nsec += 1000000000L / rate;
      if (next.tv_nsec >= 1000000000L) { next.tv_sec++; next.tv_nsec -= 1000000000L; }
      clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME, &next, 0);
    }
    memset(buf, (int)(t * 60 + i % 50 + 2), LEN);
    if (pwrite(fd, buf, LEN, slots[(i * 7 + t) % nslots]) != LEN) { perror("pwrite"); exit(2); }
    w++;
  }
  atomic_fetch_add(&writes, w);
  close(fd);
  return 0;
}

static void *reader(void *a) {
  long r = (long)a, n = 0, torn = 0;
  int fd = xopen(O_RDONLY);
  char buf[LEN];
  for (long i = 0; !atomic_load_explicit(&stop, memory_order_relaxed); i++) {
    if (pread(fd, buf, LEN, slots[(i * 7 + r) % nslots]) != LEN) { perror("pread"); exit(2); }
    n++;
    if (memcmp(buf, buf + 1, LEN - 1)) torn++;  // uniform iff every byte equals its neighbour
  }
  atomic_fetch_add(&reads, n);
  atomic_fetch_add(&tears, torn);
  close(fd);
  return 0;
}

int main(int argc, char **argv) {
  if (argc < 4) { fprintf(stderr, "usage\n"); return 2; }
  path = argv[1];
  const char *pat = argv[2];
  int secs = atoi(argv[3]), W = argc > 4 ? atoi(argv[4]) : 3, R = argc > 5 ? atoi(argv[5]) : 3;
  rate = argc > 6 ? atol(argv[6]) : 0;
  nwriters = W;
  prctl(PR_SET_TIMERSLACK, 1UL);
  if (!strcmp(pat, "aligned")) { nslots = 16; for (int s = 0; s < 16; s++) slots[s] = s * 4096L; }
  else if (!strcmp(pat, "cross4k")) { nslots = 16; for (int s = 0; s < 16; s++) slots[s] = s * 4096L + 2048; }
  else if (!strcmp(pat, "crosschunk")) { nslots = 3; for (int c = 1; c <= 3; c++) slots[c - 1] = c * 262144L - 2048; }
  else { fprintf(stderr, "bad pattern\n"); return 2; }

  int fd = open(path, O_RDWR | O_CREAT | O_TRUNC, 0644);
  if (fd < 0) { perror("create"); return 2; }
  char *pre = malloc(FILESZ);
  memset(pre, 1, FILESZ);
  if (pwrite(fd, pre, FILESZ, 0) != FILESZ || fsync(fd)) { perror("prefill"); return 2; }

  pthread_t th[64];
  for (long t = 0; t < W; t++) pthread_create(&th[t], 0, writer, (void *)t);
  for (long r = 0; r < R; r++) pthread_create(&th[W + r], 0, reader, (void *)r);
  sleep(secs);
  atomic_store(&stop, 1);
  for (int k = 0; k < W + R; k++) pthread_join(th[k], 0);

  // At rest, no writer running: every slot must be uniform (corruption check, not the measurement).
  int rest_torn = 0;
  char buf[LEN];
  for (int s = 0; s < nslots; s++) {
    if (pread(fd, buf, LEN, slots[s]) != LEN) { perror("rest pread"); return 2; }
    if (memcmp(buf, buf + 1, LEN - 1)) rest_torn++;
  }
  close(fd);
  unlink(path);
  long rd = atomic_load(&reads), tr = atomic_load(&tears);
  printf("RESULT pattern=%s secs=%d writers=%d readers=%d rate=%ld reads=%ld writes=%ld tears=%ld per_m_reads=%.3f rest_torn=%d\n",
         pat, secs, W, R, rate, rd, atomic_load(&writes), tr, rd ? tr * 1e6 / rd : 0.0, rest_torn);
  return rest_torn ? 1 : 0;
}
```

### `run.sh`

```bash
#!/bin/bash
# Scratch only (issue #45 rate measurement). usage: run.sh <secs> <rep-number> <patterns...>
# Rotates arm order by rep number over native, cowfs, paced.
set -u
D=/mnt/docs/Projects/cowfs-torn-measure
BIN=$D/target/release/examples/coremount
SECS=$1; REPS=$2; shift 2
mkdir -p $D/native $D/cw

native() { echo "ARM=native rep=$1 $(timeout $((SECS + 60)) $D/tear $D/native/f $2 $SECS)"; }
# Native btrfs with writers paced to the cowfs arm's per-writer write rate (pilot-derived).
rate_of() { case $1 in aligned) echo 9400;; cross4k) echo 8800;; crosschunk) echo 8000;; esac; }
paced() { echo "ARM=paced rep=$1 $(timeout $((SECS + 60)) $D/tear $D/native/f $2 $SECS 3 3 $(rate_of $2))"; }

cowfs() {
  local w=$D/cw/r$1-$2 pid ok=0
  mkdir -p $w/mnt
  $BIN $w/store $w/mnt > $w/log 2>&1 &
  pid=$!
  for _ in $(seq 100); do
    grep -q READY $w/log && { ok=1; break; }
    kill -0 $pid 2>/dev/null || break
    sleep 0.2
  done
  if [ $ok = 1 ]; then
    echo "ARM=cowfs rep=$1 $(timeout $((SECS + 60)) $D/tear $w/mnt/f $2 $SECS)"
  else
    echo "ARM=cowfs rep=$1 MOUNT_FAILED $(tr '\n' ' ' < $w/log)"
  fi
  kill -TERM $pid 2>/dev/null
  for _ in $(seq 50); do kill -0 $pid 2>/dev/null || break; sleep 0.2; done
  if kill -0 $pid 2>/dev/null; then echo "WARN pid $pid survived TERM, KILL"; kill -KILL $pid; fi
  wait $pid 2>/dev/null
  if mount | grep -qF " $w/mnt "; then echo "WARN still mounted, fusermount3 -u"; fusermount3 -u $w/mnt; fi
  rm -rf $w
}

# CPU idle percent over 2 s, from /proc/stat.
idle() { local a b; a=($(head -1 /proc/stat)); sleep 2; b=($(head -1 /proc/stat))
  local t=0; for i in 1 2 3 4 5 6 7 8; do t=$((t + b[i] - a[i])); done
  echo $((100 * (b[4] - a[4]) / t)); }

for rep in $REPS; do
  for p in "$@"; do
    arms=(native cowfs paced)
    for k in 0 1 2; do
      a=${arms[$(((k + rep) % 3))]}
      sleep 3; echo "LOAD $(cut -d' ' -f1-3 /proc/loadavg) idle=$(idle)"
      $a $rep $p
    done
  done
done
```

### `paced2.sh`

```bash
#!/bin/bash
# Scratch only: paced native arm rerun with writers staggered by t/3 of a period. usage: paced2.sh <rep>
D=/mnt/docs/Projects/cowfs-torn-measure
for p in aligned cross4k crosschunk; do
  case $p in aligned) r=9400;; cross4k) r=8800;; crosschunk) r=8000;; esac
  sleep 3
  echo "ARM=paced_staggered rep=$1 $(timeout 70 $D/tear $D/native/f $p 10 3 3 $r)"
done
```

### `coremount.rs`

```rust
//! Scratch only (issue #45 rate measurement): mounts a real `Core` with production default
//! `Options` at <mnt>, store in <store>, prints READY, waits for a signal.
use std::sync::Arc;

fn main() {
    let mut a = std::env::args().skip(1);
    let (store, mnt) = (a.next().expect("store"), a.next().expect("mnt"));
    cowfs_fuse::Mount::install_signal_cleanup().expect("signal handlers");
    let core = cowfs_core::Core::open(&store, cowfs_core::Options::default()).expect("core");
    core.create_snapshot("main").expect("snapshot");
    let view = core.snapshot_view("main").expect("view");
    let _m = cowfs_fuse::Mount::new(Arc::new(view), &mnt, cowfs_fuse::MountOptions::default())
        .expect("mount");
    println!("READY");
    loop {
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
}
```

### `cuts.rs`

```rust
//! Scratch only: where the real chunker cuts a 1 MiB file of PREFILL bytes, with and without one
//! 4 KiB uniform write straddling the 256 KiB mark.
fn cuts(img: &[u8]) -> Vec<usize> {
    let mut at = 0;
    cowfs_store::chunks(img).map(|c| { at += c.len(); at }).collect()
}
fn main() {
    let mut img = vec![1u8; 1 << 20];
    println!("prefill: {:?}", cuts(&img));
    for v in [2u8, 62, 122, 51, 200] {
        for c in 1..4usize {
            let o = c * 262144 - 2048;
            img[o..o + 4096].fill(v);
        }
        println!("v={v} at 3 straddling slots: {:?}", cuts(&img));
    }
}
```
