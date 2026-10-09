#!/usr/bin/env python3
"""One-command, validated quiet-host run of gates g1/g2 (cargo) and g3 (git status), macOS arm.

Tracker g1 = harness gates g1 (clean build) + g2 (edit rebuild); tracker g2 = harness gate g3 (git status).

  python3 bench/g12_run.py --run-id ID                 # full gate run, 5 reps, scale 100, gates g1,g2,g3
  python3 bench/g12_run.py --run-id ID --sample        # tiny plumbing check, NEVER a gate result

Steps: take the CPU lock, build the daemon with `cargo build --release --message-format=json` and refuse any
artifact whose reported profile is unoptimised, has debug assertions, or is not under a `release` directory
(the digest is recorded and re-compared before the launch and around every cowfs arm), measure an idle
baseline of this host, start a PRIVATE release daemon (own store, socket, mount), run bench/gates.py in the order
native1, cowfs1, native2, cowfs2 (the run-pair.sh interleave) with a quiet-host check before each arm,
validate provenance, load and the native-native noise floor, run bench/compare.py for both pairs,
write OUT/verdict.json, stop the daemon. Per-arm quiet check is foreign CPU (ps pcpu outside the driver's and daemon's process trees) against baseline p95 + a chosen margin;
load1 only gates the baseline and the settle between arms, since the arm's own build raises it.
Exit 0 valid PASS, 1 valid FAIL, 2 INVALID, unmeasurable, or any recorded problem (also in --sample).
A non-sample run must use gates g1,g2,g3, scale 100, reps >= 5, --baseline-window >= 300 and --load-cap <= 4.0, else it is refused up front.

The quiet bar is derived from the baseline measured in this run:
  load1 limit = min(baseline p95 load1 + 1.0, --load-cap); idle floor = baseline median CPU idle - 10 points;
  foreign CPU limit = baseline p95 foreign CPU + 50 points. The +1.0, -10 and +50 margins are chosen, not measured.
The baseline itself is refused if its p95 load1 exceeds --load-cap, its p95 foreign CPU exceeds 100, or foreign cargo/rustc/cc1/ld run.
CPU of kernel_task, Spotlight and fseventsd (INDUCED, chosen) is reported per arm but not gated.
"""

import argparse
import hashlib
import json
import os
import re
import signal
import statistics
import subprocess
import sys
import threading
import time
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
BENCH = REPO / "bench"
LOCK = Path(os.environ.get("COWFS_BENCH_CPU_LOCK", "/Users/zeeshanhaque/Projects/cowfs/spikes/nfs-loopback/out/cpu.lock"))
NOISE_BAND = 0.10  # native-vs-native median ratio, same band as scripts/measure-live-trial.py
FOREIGN = re.compile(r"\b(cargo|rustc|cc1|ld)\b")
LOAD_CAP_MAX = 4.0  # guess: 25 percent of 16 logical cores, not measured
FOREIGN_MARGIN = 50.0  # chosen, not measured: half a core of extra foreign CPU (ps pcpu points) over the baseline p95
FOREIGN_BASE_CAP = 100.0  # chosen: a baseline with more than one core of foreign CPU is not idle
INDUCED = {"kernel_task", "mds", "mds_stores", "mdworker_shared", "fseventsd"}  # chosen, not measured: CPU the arm's own file I/O induces
PLATEAU = 0.5  # chosen: max load1 spread over 60 s that counts as settled
COOL_TIMEOUT = 600  # chosen: seconds to let load1 decay after the release build
MIN_GATES, MIN_SCALE, MIN_REPS, MIN_BASELINE = {"g1", "g2", "g3"}, 100, 5, 300


def sh(cmd, **kw):
    return subprocess.run(cmd, capture_output=True, text=True, check=False, **kw)


# ---- pure validation (unit tested, no I/O) ------------------------------------------------


def param_problems(gates, scale, reps, load_cap, sample, baseline_window=300):
    """A non-sample run is a gate run: refuse toy or weakened settings up front."""
    if sample:
        return []
    out = []
    if not MIN_GATES <= set(gates):
        out.append(f"gates {sorted(gates)} must include {sorted(MIN_GATES)} (g1 and g2 are both needed for a verdict)")
    if scale < MIN_SCALE:
        out.append(f"scale {scale} < {MIN_SCALE}")
    if reps < MIN_REPS:
        out.append(f"reps {reps} < {MIN_REPS}")
    if baseline_window < MIN_BASELINE:
        out.append(f"--baseline-window {baseline_window} < {MIN_BASELINE}")
    if load_cap > LOAD_CAP_MAX:
        out.append(f"--load-cap {load_cap} > {LOAD_CAP_MAX}")
    return out


def profile_problems(art):
    """`art` is the compiler-artifact message cargo reported for a bin: refuse an unoptimised or debug-assert build."""
    prof, exe = art.get("profile") or {}, str(art.get("executable") or "")
    out = []
    if str(prof.get("opt_level")) in ("0", "None", ""):
        out.append(f"opt_level {prof.get('opt_level')!r}: not an optimised build")
    if prof.get("debug_assertions") is not False:
        out.append("debug_assertions on or unknown")
    if Path(exe).parent.name != "release":
        out.append(f"{exe} is not under a release directory")
    return out


def digest_problems(path, want):
    got = hashlib.sha256(Path(path).read_bytes()).hexdigest() if Path(path).is_file() else None
    return [] if got == want else [f"digest of {path} is {got}, recorded at build {want}"]


def arm_problems(alive, kind, source, dev, native_dev, digest_bad):
    out = []
    if not alive:
        out.append("daemon not running")
    if kind != "nfs" or not str(source).startswith("localhost:/cowfs-"):
        out.append(f"arm is not the cowfs NFS export (type {kind}, source {source})")
    if dev == native_dev:
        out.append("arm shares st_dev with the native root")
    return out + digest_bad


def mount_entry(path, table):
    """(source, type, mountpoint) of the longest mount point containing path, from `mount` output."""
    best, p = None, str(Path(path).resolve())
    for line in table.splitlines():
        m = re.match(r"(.+?) on (.+) \(([^,)]+)", line)
        if m and (p == m[2] or p.startswith(m[2].rstrip("/") + "/")) and (best is None or len(m[2]) > len(best[2])):
            best = (m[1], m[3], m[2])
    return best or (None, None, None)


def stale_lock(owner_text, alive):
    m = re.search(r"pid (\d+)", owner_text or "")
    return bool(m) and not alive(int(m[1]))


def pid_alive(pid):
    if pid <= 0:
        return False
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def parse_ps(text):
    """`ps -A -o pid=,ppid=,pcpu=,comm=` rows -> [(pid, ppid, pcpu, comm)]."""
    rows = []
    for line in text.splitlines():
        f = line.split(None, 3)
        if len(f) == 4 and f[0].isdigit() and f[1].isdigit():
            try:
                rows.append((int(f[0]), int(f[1]), float(f[2]), f[3]))
            except ValueError:
                pass
    return rows


def foreign_cpu(rows, roots):
    """pcpu outside the process trees rooted at `roots` (the driver, the daemon), split into
    (foreign, induced, top3): `induced` is the INDUCED set (kernel NFS client, Spotlight, fseventsd), which the arm's own
    I/O provokes and which is reported but not gated; top3 are the largest gated contributors.

    ps pcpu is a decaying average, a heuristic; it is only ever compared with the same metric taken in the baseline.
    """
    kids = {}
    for pid, ppid, _, _ in rows:
        kids.setdefault(ppid, []).append(pid)
    own, todo = set(), list(roots)
    while todo:
        pid = todo.pop()
        if pid not in own:
            own.add(pid)
            todo += kids.get(pid, [])
    other = [(c, os.path.basename(comm)) for pid, _, c, comm in rows if pid not in own]
    gated = sorted((x for x in other if x[1] not in INDUCED), reverse=True)
    return sum(c for c, _ in gated), sum(c for c, n in other if n in INDUCED), [(n, c) for c, n in gated[:3]]


def foreign_problems(samples, limit):
    """An arm is sound when it has samples and their p95 foreign CPU is within the baseline-derived limit."""
    if not samples:
        return ["no foreign-CPU samples taken during the arm"]
    if p95(samples) > limit:
        return [f"foreign CPU p95 {p95(samples):.0f} > limit {limit:.0f} (max {max(samples):.0f}, n {len(samples)})"]
    return []


def sample_foreign(roots):
    return foreign_cpu(parse_ps(sh(["ps", "-A", "-o", "pid=,ppid=,pcpu=,comm="]).stdout), roots)  # (foreign, induced, top3)


def ratio_table(nat, cow, gates):
    """Per-gate median cowfs/native from two lists of rep rows."""
    out = {}
    for g in gates:
        a = [r["wall_s"] for r in nat if r["gate"] == g]
        b = [r["wall_s"] for r in cow if r["gate"] == g]
        out[g] = round(statistics.median(b) / statistics.median(a), 3) if a and b and statistics.median(a) else None
    return out


# ---- host quiet gate ----------------------------------------------------------------------


def window(secs, roots=None):
    """Sample load1 (and foreign CPU when `roots` is given) every 5 s and CPU idle via top; returns (loads, idles, foreign)."""
    n = max(2, secs // 5)
    top = subprocess.Popen(["top", "-l", str(n + 1), "-s", "5", "-n", "0"], stdout=subprocess.PIPE, text=True)
    loads, foreign = [], []
    for _ in range(n):
        time.sleep(5)
        loads.append(os.getloadavg()[0])
        if roots:
            foreign.append(sample_foreign(roots))
    out = top.communicate()[0]
    idles = [float(m) for m in re.findall(r"CPU usage:.*?([\d.]+)% idle", out)][1:]  # first top sample is since boot
    return loads, idles, foreign


def foreign_procs(own_pid=None):
    hits = []
    for r in sh(["ps", "-A", "-o", "pid=,comm="]).stdout.splitlines():
        pid, _, comm = r.strip().partition(" ")
        if pid.isdigit() and int(pid) != own_pid and FOREIGN.search(os.path.basename(comm)):
            hits.append(os.path.basename(comm))
    return sorted(set(hits))


def p95(xs):
    xs = sorted(xs)
    return xs[min(len(xs) - 1, int(0.95 * len(xs)))]


def mount_table():
    return sh(["mount"]).stdout


def fstype(path):
    return mount_entry(path, mount_table())[1]


def cool_down(cap, timeout, getload=lambda: os.getloadavg()[0], sleep=time.sleep, clock=time.monotonic):
    """Wait for a load1 plateau: 7 polls 10 s apart, all <= cap and spread < PLATEAU (so the decay from the build is over).

    Returns (settled, last_load)."""
    end, hist = clock() + timeout, []
    while True:
        hist = (hist + [getload()])[-7:]
        if len(hist) == 7 and max(hist) <= cap and max(hist) - min(hist) < PLATEAU:
            return True, hist[-1]
        if clock() >= end:
            return False, hist[-1]
        sleep(10)


class Quiet:
    """The quiet bar, derived from a baseline measured in this run.

    load1 limit = baseline p95 + 1.0 (the margin is chosen, not measured), capped at --load-cap.
    foreign CPU limit = baseline p95 of ps pcpu outside the driver's own trees + FOREIGN_MARGIN (chosen).
    load1 is used for the baseline and between-arm settling only; during an arm only foreign CPU counts,
    because the arm's own `cargo build -j4` legitimately raises load1.
    """

    def __init__(self, args):
        self.args = args
        self.limit = None
        self.foreign_limit = None
        self.idle_floor = None
        self.base = None

    def baseline(self):
        loads, idles, fsamples = window(self.args.baseline_window, [os.getpid()])
        fcpu = [f[0] for f in fsamples]
        foreign = foreign_procs()
        self.base = {"window_s": self.args.baseline_window, "load1_p95": p95(loads), "load1_max": max(loads),
                     "cpu_idle_median": statistics.median(idles) if idles else None, "foreign": foreign,
                     "foreign_cpu_p95": p95(fcpu), "foreign_cpu_max": max(fcpu)}
        ok = (self.base["load1_p95"] <= self.args.load_cap and not foreign and bool(idles)
              and self.base["foreign_cpu_p95"] <= FOREIGN_BASE_CAP)
        self.limit = min(self.base["load1_p95"] + 1.0, self.args.load_cap)
        self.foreign_limit = self.base["foreign_cpu_p95"] + FOREIGN_MARGIN
        self.idle_floor = (self.base["cpu_idle_median"] or 0) - 10
        self.base["limit"], self.base["foreign_limit"], self.base["idle_floor"] = self.limit, self.foreign_limit, self.idle_floor
        return ok

    def check(self, own_pid):
        """Between-arm settle: True when load1, CPU idle and foreign processes look like the baseline; retries for --wait."""
        deadline = time.time() + self.args.wait
        while True:
            loads, idles, _ = window(60)
            foreign = foreign_procs(own_pid)
            why = {"load1_max": max(loads), "cpu_idle_median": statistics.median(idles) if idles else None, "foreign": foreign}
            if max(loads) <= self.limit and idles and why["cpu_idle_median"] >= self.idle_floor and not foreign:
                return True, why
            if time.time() > deadline:
                return False, why
            time.sleep(10)


class ArmSampler(threading.Thread):
    """Samples foreign CPU every 5 s while an arm runs."""

    def __init__(self, roots, sample=sample_foreign, period=5):
        super().__init__(daemon=True)
        self.roots, self.sample, self.period, self.samples = roots, sample, period, []
        self.stop_ev = threading.Event()

    def run(self):
        self.samples.append(self.sample(self.roots))  # one at the start, so a short arm still has data
        while not self.stop_ev.wait(self.period):
            self.samples.append(self.sample(self.roots))

    def finish(self):
        self.stop_ev.set()
        self.join(10)
        self.samples.append(self.sample(self.roots))
        return self.samples


# ---- build and daemon ---------------------------------------------------------------------


def build_release():
    """Build with --release and return {name: artifact message}; refuse anything profile_problems rejects."""
    p = sh(["cargo", "build", "--release", "--locked", "-p", "cowfs-daemon", "-p", "cowfs-cli", "-j", "4",
            "--message-format=json"], cwd=REPO)
    if p.returncode:
        raise SystemExit(f"release build failed: {p.stderr[-1500:]}")
    arts = {}
    for line in p.stdout.splitlines():
        try:
            m = json.loads(line)
        except ValueError:
            continue
        if m.get("reason") == "compiler-artifact" and m.get("executable") and m["target"]["name"] in ("cowfs-daemon", "cowfs"):
            arts[m["target"]["name"]] = m
    for name in ("cowfs-daemon", "cowfs"):
        if name not in arts:
            raise SystemExit(f"cargo reported no {name} executable")
        bad = profile_problems(arts[name])
        if bad:
            raise SystemExit(f"{name} is not a release build: {bad}")
    return arts


class Daemon:
    def __init__(self, out, args, arts):
        self.dir = out / "daemon"
        self.store, self.mnt, self.log = self.dir / "store", self.dir / "mnt", self.dir / "daemon.log"
        self.sock = Path.home() / ".cowfs" / "sock" / f"g12-{args.run_id}.sock"  # AF_UNIX path limit is 104 bytes
        self.bin, self.cli = Path(arts["cowfs-daemon"]["executable"]), Path(arts["cowfs"]["executable"])
        self.sha = hashlib.sha256(self.bin.read_bytes()).hexdigest()
        self.proc = None

    def start(self):
        self.store.mkdir(parents=True)
        self.mnt.mkdir()
        self.sock.parent.mkdir(parents=True, exist_ok=True)
        self.mnt = self.mnt.resolve()
        self.proc = subprocess.Popen(
            [str(self.bin), "--store", str(self.store), "--mount", str(self.mnt), "--socket", str(self.sock), "--backend", "core"],
            stdout=open(self.log, "w"), stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL)  # noqa: SIM115
        for _ in range(120):
            if self.proc.poll() is not None:
                raise SystemExit(f"daemon died: {self.log.read_text()[-1500:]}")
            if fstype(self.mnt) == "nfs" and self.sock.exists():
                break
            time.sleep(1)
        else:
            raise SystemExit("daemon mount timeout")
        p = sh([str(self.cli), "--socket", str(self.sock), "snapshot", "create", "base"])
        if p.returncode:
            raise SystemExit(f"snapshot create base failed: {p.stdout}{p.stderr}")
        return self.mnt / "base" / "g12"

    def prov(self):
        argv = sh(["ps", "-o", "command=", "-p", str(self.proc.pid)]).stdout.strip()
        return {"pid": self.proc.pid, "argv": argv, "bin": str(self.bin), "sha256": self.sha,
                "launched_from_built_binary": argv.startswith(str(self.bin)) and "--backend core" in argv,
                "mount_fstype": fstype(self.mnt)}

    def arm_state(self, native_dev):
        """Liveness, NFS provenance and binary digest right now; the list is empty when the cowfs arm is sound."""
        src, kind, _ = mount_entry(self.mnt, mount_table())
        base = self.mnt / "base"
        dev = os.stat(base).st_dev if base.exists() else None
        alive = self.proc.poll() is None
        probs = arm_problems(alive, kind, src, dev, native_dev, digest_problems(self.bin, self.sha))
        return {"alive": alive, "fstype": kind, "source": src, "st_dev": dev, "problems": probs}

    def stop(self):
        """SIGTERM after an argv check; returns problems. The socket is only removed once the daemon is gone."""
        out = []
        if self.proc and self.proc.poll() is None:
            if str(self.bin) in sh(["ps", "-o", "command=", "-p", str(self.proc.pid)]).stdout:
                self.proc.send_signal(signal.SIGTERM)
                try:
                    self.proc.wait(60)
                except subprocess.TimeoutExpired:
                    return [f"daemon {self.proc.pid} ignored SIGTERM for 60 s, left running for the operator"]
        if self.proc and fstype(self.mnt) == "nfs" and "cowfs-" in str(mount_entry(self.mnt, mount_table())[0]):
            out.append(f"mount still present after stop: {self.mnt}")
        self.sock.unlink(missing_ok=True)
        Path(str(self.sock) + ".lock").unlink(missing_ok=True)
        return out


# ---- shared CPU lock ----------------------------------------------------------------------


def acquire_lock(wait=900):
    LOCK.parent.mkdir(parents=True, exist_ok=True)
    end = time.time() + wait
    while True:
        try:
            LOCK.mkdir()
            return
        except FileExistsError:
            owner = LOCK / "owner"
            text = owner.read_text() if owner.exists() else None
            if text is None and time.time() - LOCK.stat().st_mtime > 60:
                text = "pid 0"  # an owner-less lock older than a minute: its creator died between mkdir and write
            if text is not None and stale_lock(text, pid_alive):
                # rename is atomic: of several waiters exactly one wins, the others get FileNotFoundError.
                # ponytail: a waiter that renames a lock re-created in between puts it back below; narrow window remains.
                grave = LOCK.with_name(f"{LOCK.name}.stale.{os.getpid()}.{time.monotonic_ns()}")
                try:
                    os.rename(LOCK, grave)
                except OSError:
                    continue
                if not stale_lock((grave / "owner").read_text() if (grave / "owner").exists() else "pid 0", pid_alive):
                    try:
                        os.rename(grave, LOCK)
                    except OSError:
                        pass
                    continue
                (grave / "owner").unlink(missing_ok=True)
                grave.rmdir()
                continue
            if time.time() > end:
                raise SystemExit(f"cpu lock still held after {wait} s: {LOCK}")
            time.sleep(10)


def release_lock():
    for step in ((LOCK / "owner").unlink, LOCK.rmdir):
        try:
            step()
        except OSError:
            pass


# ---- driver -------------------------------------------------------------------------------


def reps_of(path):
    rows = [json.loads(line) for line in Path(path).read_text().splitlines() if line.strip()]
    return rows[0], [r for r in rows[1:] if r.get("kind") == "rep"]


def newest(label):
    pat = re.compile(re.escape(label) + r"-\d{8}-\d{6}\.jsonl")
    return sorted((p for p in (BENCH / "out").glob(f"{label}-*.jsonl") if pat.fullmatch(p.name)), key=lambda p: p.stat().st_mtime)[-1]


def run(args, out, verdict):
    env = dict(os.environ, COWFS_BENCH_SCALE=str(args.scale))
    gl = args.gates.split(",")
    acquire_lock(900)
    d = None
    labels = []
    try:
        (LOCK / "owner").write_text(f"g12_run pid {os.getpid()} {time.ctime()}\n")
        arts = build_release()
        d = Daemon(out, args, arts)
        verdict["build"] = {n: {"executable": a["executable"], "profile": a["profile"]} for n, a in arts.items()}
        # the build's decaying load must not leak into the baseline window
        settled, load = cool_down(args.load_cap, 0 if args.sample else COOL_TIMEOUT)
        verdict["cool_down"] = {"settled": settled, "load1": load}
        if not settled and not args.sample:
            verdict["problems"].append(f"load1 {load:.1f} did not fall to {args.load_cap} within {COOL_TIMEOUT} s after the release build")
            return "INVALID"
        q = Quiet(args)
        ok = q.baseline()
        verdict["idle_baseline"] = q.base
        if not ok:
            verdict["problems"].append("idle baseline refused: p95 load1 or foreign CPU above cap, foreign build processes, or no CPU samples")
            if not args.sample:
                return "INVALID"
        croot = d.start()
        verdict["daemon"] = d.prov()
        if not verdict["daemon"]["launched_from_built_binary"] or verdict["daemon"]["mount_fstype"] != "nfs":
            verdict["problems"].append("daemon was not launched from the built release binary on an NFS mount")
            return "INVALID"
        native_dev = os.stat(out).st_dev
        verdict["arms"] = []
        for tag, root in (("n1", out / "native"), ("c1", croot), ("n2", out / "native"), ("c2", croot)):
            label = f"g12-{args.run_id}-{tag}"
            good, why = q.check(d.proc.pid)
            rec = {"arm": tag, "quiet": why, "quiet_ok": good}
            verdict["arms"].append(rec)
            if not good and not args.sample:
                verdict["problems"].append(f"{label}: host not quiet: {why}")
                break
            if tag.startswith("c"):
                rec["pre"] = d.arm_state(native_dev)
                if rec["pre"]["problems"]:
                    verdict["problems"].append(f"{label}: cowfs arm unsound before run: {rec['pre']['problems']}")
                    break
            elif fstype(out) == "nfs":
                verdict["problems"].append(f"{label}: native root is on the cowfs mount")
                break
            sampler = ArmSampler([os.getpid(), d.proc.pid])
            sampler.start()
            r = subprocess.run([sys.executable, str(BENCH / "gates.py"), "--root", str(root), "--label", label,
                                "--reps", str(args.reps), "--gates", args.gates], env=env)
            samples = sampler.finish()
            vals = [x[0] for x in samples]
            peak = max(samples, key=lambda x: x[0], default=(None, None, []))
            rec["foreign_cpu"] = {"n": len(vals), "p95": p95(vals) if vals else None, "max": peak[0], "top3_at_peak": peak[2],
                                  "limit": q.foreign_limit, "induced_max": max((x[1] for x in samples), default=None)}
            bad = foreign_problems(vals, q.foreign_limit)
            verdict["problems"] += [f"{label}: {b}" for b in bad]
            if bad and not args.sample:
                break  # a disturbed arm is not worth the hours of the remaining ones
            if tag.startswith("c"):
                rec["post"] = d.arm_state(native_dev)
                if rec["post"]["problems"]:
                    verdict["problems"].append(f"{label}: cowfs arm unsound after run: {rec['post']['problems']}")
                    break
            if r.returncode:
                verdict["problems"].append(f"{label}: gates.py rc {r.returncode}")
                break
            labels.append(label)
    finally:
        if d is not None:
            try:
                verdict["problems"] += d.stop()
            except Exception as e:  # noqa: BLE001
                verdict["problems"].append(f"daemon stop failed: {e}")
        release_lock()
    if len(labels) < 4:
        return "INVALID"
    files = {k: newest(f"g12-{args.run_id}-{k}") for k in ("n1", "c1", "n2", "c2")}
    rows = {k: reps_of(f)[1] for k, f in files.items()}
    for k, rs in rows.items():
        for g in gl:
            if sum(1 for r in rs if r["gate"] == g) != args.reps:
                verdict["problems"].append(f"{k} {g}: rep count != {args.reps}")
    verdict["native_native"] = ratio_table(rows["n1"], rows["n2"], gl)
    for g, ratio in verdict["native_native"].items():
        if ratio is None or abs(ratio - 1) > NOISE_BAND:
            verdict["problems"].append(f"{g}: native-native ratio {ratio} outside 1 +/- {NOISE_BAND}")
    verdict["cowfs_over_native"] = {"pair1": ratio_table(rows["n1"], rows["c1"], gl), "pair2": ratio_table(rows["n2"], rows["c2"], gl)}
    codes = []
    for i, (nat, cow, nf) in enumerate((("n1", "c1", "n2"), ("n2", "c2", "n1"))):
        p = sh([sys.executable, str(BENCH / "compare.py"), "--native", str(files[nat]), "--cowfs", str(files[cow]),
                "--noise-floor", str(files[nf])])
        (out / f"compare-{i + 1}.txt").write_text(p.stdout + p.stderr)
        codes.append(p.returncode)
    verdict["compare_rc"] = codes
    if any(c in (2, 3) for c in codes):
        verdict["problems"].append(f"compare.py rc {codes}")
    if args.sample:
        return "SAMPLE, NOT A GATE RESULT"
    if verdict["problems"]:
        return "INVALID"
    return "PASS" if codes == [0, 0] else "FAIL"


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--run-id", required=True)
    ap.add_argument("--sample", action="store_true", help="scale 1, gates g3, 1 rep, 20 s baseline, quiet bar informational")
    ap.add_argument("--reps", type=int)
    ap.add_argument("--gates")
    ap.add_argument("--scale", type=int)
    ap.add_argument("--baseline-window", type=int)
    ap.add_argument("--load-cap", type=float, default=LOAD_CAP_MAX, help="25 percent of 16 logical cores (a guess)")
    ap.add_argument("--wait", type=int, default=900, help="seconds to wait for a quiet host before each arm")
    args = ap.parse_args()
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(143))  # run the finally blocks: lock and daemon cleanup
    if not re.fullmatch(r"[A-Za-z0-9._-]+", args.run_id):
        raise SystemExit("bad run id")
    s = args.sample
    args.reps = args.reps or (1 if s else MIN_REPS)
    args.gates = args.gates or ("g3" if s else "g1,g2,g3")
    args.scale = args.scale or (1 if s else MIN_SCALE)
    args.baseline_window = args.baseline_window or (20 if s else 300)
    if s:
        args.wait = 0  # the quiet bar is informational in a sample
    out = BENCH / "out" / "g12" / args.run_id
    out.mkdir(parents=True)  # exclusive: a repeated run id is refused
    verdict = {"run_id": args.run_id, "sample": s, "NOT_A_GATE_RESULT": s, "gates": args.gates, "reps": args.reps,
               "scale": args.scale, "load_cap": args.load_cap, "baseline_window": args.baseline_window, "wait": args.wait, "problems": [], "result": "INVALID"}
    try:
        verdict["problems"] += param_problems(args.gates.split(","), args.scale, args.reps, args.load_cap, s, args.baseline_window)
        verdict["result"] = "INVALID" if verdict["problems"] else run(args, out, verdict)
    except BaseException as e:  # noqa: BLE001  verdict.json is always written; exit 2 never collides with a valid FAIL (1)
        verdict["problems"].append(f"{type(e).__name__}: {e}")
        verdict["result"] = "INVALID"
    (out / "verdict.json").write_text(json.dumps(verdict, indent=1, default=str) + "\n")
    print(json.dumps({k: verdict[k] for k in ("run_id", "sample", "NOT_A_GATE_RESULT", "result", "problems")}, default=str))
    if verdict["problems"] or verdict["result"] == "INVALID":
        return 2
    return 0 if verdict["result"] in ("PASS", "SAMPLE, NOT A GATE RESULT") else 1


if __name__ == "__main__":
    raise SystemExit(main())
