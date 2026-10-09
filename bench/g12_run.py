#!/usr/bin/env python3
"""One-command, validated quiet-host run of gates g1/g2 (cargo) and g3 (git status), macOS arm.

Tracker g1 = harness gates g1 (clean build) + g2 (edit rebuild); tracker g2 = harness gate g3 (git status).

  python3 bench/g12_run.py --run-id ID                 # full gate run, 5 reps, scale 100, gates g1,g2,g3
  python3 bench/g12_run.py --run-id ID --sample        # tiny plumbing check, NEVER a gate result

Steps: refuse a debug or missing daemon, take the CPU lock, measure an idle baseline of this host,
start a PRIVATE release daemon (own store, socket, mount), run bench/gates.py in the order
native1, cowfs1, native2, cowfs2 (the run-pair.sh interleave) with a quiet-host check before each arm,
validate provenance, load and the native-native noise floor, run bench/compare.py for both pairs,
write OUT/verdict.json, stop the daemon. Exit 0 valid PASS, 1 valid FAIL, 2 INVALID or unmeasurable.

The quiet bar is derived from the baseline measured in this run:
  limit = min(baseline p95 load1 + 1.0, --load-cap); idle floor = baseline median CPU idle - 10 points.
The baseline itself is refused if its p95 load1 exceeds --load-cap or foreign cargo/rustc/cc1/ld run.
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
import time
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
BENCH = REPO / "bench"
LOCK = Path(os.environ.get("COWFS_BENCH_CPU_LOCK", "/Users/zeeshanhaque/Projects/cowfs/spikes/nfs-loopback/out/cpu.lock"))
NOISE_BAND = 0.10  # native-vs-native median ratio, same band as scripts/measure-live-trial.py
FOREIGN = re.compile(r"\b(cargo|rustc|cc1|ld)\b")


def sh(cmd, **kw):
    return subprocess.run(cmd, capture_output=True, text=True, check=False, **kw)


def window(secs):
    """Sample load1 every 5 s and CPU idle via top for `secs`; returns (loads, idles)."""
    n = max(2, secs // 5)
    top = subprocess.Popen(["top", "-l", str(n + 1), "-s", "5", "-n", "0"], stdout=subprocess.PIPE, text=True)
    loads = []
    for _ in range(n):
        time.sleep(5)
        loads.append(os.getloadavg()[0])
    out = top.communicate()[0]
    idles = [float(m) for m in re.findall(r"CPU usage:.*?([\d.]+)% idle", out)][1:]  # first top sample is since boot
    return loads, idles


def foreign_procs(own_pid=None):
    rows = sh(["ps", "-A", "-o", "pid=,comm="]).stdout.splitlines()
    hits = []
    for r in rows:
        pid, _, comm = r.strip().partition(" ")
        if pid.isdigit() and int(pid) != own_pid and FOREIGN.search(os.path.basename(comm)):
            hits.append(os.path.basename(comm))
    return sorted(set(hits))


def p95(xs):
    xs = sorted(xs)
    return xs[min(len(xs) - 1, int(0.95 * len(xs)))]


def fstype(path):
    """Filesystem type of the longest mount point containing path, from the kernel mount table."""
    best, kind = "", None
    p = str(Path(path).resolve())
    for line in sh(["mount"]).stdout.splitlines():
        m = re.match(r".* on (.+) \(([^,)]+)", line)
        if m and (p == m[1] or p.startswith(m[1].rstrip("/") + "/")) and len(m[1]) > len(best):
            best, kind = m[1], m[2]
    return kind


class Quiet:
    def __init__(self, args):
        self.args = args
        self.limit = None
        self.idle_floor = None
        self.base = None

    def baseline(self):
        loads, idles = window(self.args.baseline_window)
        foreign = foreign_procs()
        self.base = {"window_s": self.args.baseline_window, "load1_p95": p95(loads), "load1_max": max(loads),
                     "cpu_idle_median": statistics.median(idles) if idles else None, "foreign": foreign}
        ok = self.base["load1_p95"] <= self.args.load_cap and not foreign and idles
        self.limit = min(self.base["load1_p95"] + 1.0, self.args.load_cap)
        self.idle_floor = (self.base["cpu_idle_median"] or 0) - 10
        self.base["limit"], self.base["idle_floor"] = self.limit, self.idle_floor
        return ok

    def check(self, own_pid):
        """True when this host looks like the baseline; retries for --wait seconds."""
        deadline = time.time() + self.args.wait
        while True:
            loads, idles = window(60)
            foreign = foreign_procs(own_pid)
            why = {"load1_max": max(loads), "cpu_idle_median": statistics.median(idles) if idles else None, "foreign": foreign}
            if max(loads) <= self.limit and idles and why["cpu_idle_median"] >= self.idle_floor and not foreign:
                return True, why
            if time.time() > deadline:
                return False, why
            time.sleep(10)


class Daemon:
    def __init__(self, out, args):
        self.dir = out / "daemon"
        self.store, self.mnt, self.log = self.dir / "store", self.dir / "mnt", self.dir / "daemon.log"
        self.sock = Path.home() / ".cowfs" / "sock" / f"g12-{args.run_id}.sock"  # AF_UNIX path limit is 104 bytes
        self.bin = REPO / "target" / "release" / "cowfs-daemon"
        self.cli = REPO / "target" / "release" / "cowfs"
        self.proc = None

    def start(self):
        for b in (self.bin, self.cli):
            if not b.is_file():
                raise SystemExit(f"missing {b}: cargo build --release --locked -p cowfs-daemon -p cowfs-cli")
        self.store.mkdir(parents=True)
        self.mnt.mkdir()
        self.sock.parent.mkdir(parents=True, exist_ok=True)
        self.mnt = self.mnt.resolve()
        self.proc = subprocess.Popen(
            [str(self.bin), "--store", str(self.store), "--mount", str(self.mnt), "--socket", str(self.sock), "--backend", "core"],
            stdout=open(self.log, "w"), stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL)
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
        return {"pid": self.proc.pid, "argv": argv, "bin": str(self.bin), "sha256": hashlib.sha256(self.bin.read_bytes()).hexdigest(),
                "release": "/target/release/" in str(self.bin) and argv.startswith(str(self.bin)) and "--backend core" in argv,
                "mount_fstype": fstype(self.mnt)}

    def stop(self):
        if self.proc and self.proc.poll() is None:
            argv = sh(["ps", "-o", "command=", "-p", str(self.proc.pid)]).stdout
            if str(self.bin) in argv:  # verify before the kill
                self.proc.send_signal(signal.SIGTERM)
                try:
                    self.proc.wait(60)
                except subprocess.TimeoutExpired:
                    print(f"daemon {self.proc.pid} ignored SIGTERM for 60 s, left running for the operator", file=sys.stderr)
        self.sock.unlink(missing_ok=True)
        Path(str(self.sock) + ".lock").unlink(missing_ok=True)


def reps_of(path):
    rows = [json.loads(line) for line in Path(path).read_text().splitlines() if line.strip()]
    return rows[0], [r for r in rows[1:] if r.get("kind") == "rep"]


def newest(label):
    return sorted((BENCH / "out").glob(f"{label}-*.jsonl"), key=lambda p: p.stat().st_mtime)[-1]


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--run-id", required=True)
    ap.add_argument("--sample", action="store_true", help="scale 1, gates g3, 1 rep, 20 s baseline, quiet bar informational")
    ap.add_argument("--reps", type=int)
    ap.add_argument("--gates")
    ap.add_argument("--scale", type=int)
    ap.add_argument("--baseline-window", type=int)
    ap.add_argument("--load-cap", type=float, default=4.0, help="25 percent of 16 logical cores")
    ap.add_argument("--wait", type=int, default=900, help="seconds to wait for a quiet host before each arm")
    args = ap.parse_args()
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(143))  # run the finally blocks: lock and daemon cleanup
    if not re.fullmatch(r"[A-Za-z0-9._-]+", args.run_id):
        raise SystemExit("bad run id")
    s = args.sample
    args.reps = args.reps or (1 if s else 5)
    args.gates = args.gates or ("g3" if s else "g1,g2,g3")
    args.scale = args.scale or (1 if s else 100)
    args.baseline_window = args.baseline_window or (20 if s else 300)
    if s:
        args.wait = 0  # the quiet bar is informational in a sample
    out = BENCH / "out" / "g12" / args.run_id
    out.mkdir(parents=True)  # exclusive: a repeated run id is refused
    env = dict(os.environ, COWFS_BENCH_SCALE=str(args.scale))
    verdict = {"run_id": args.run_id, "sample": s, "NOT_A_GATE_RESULT": s, "gates": args.gates, "reps": args.reps,
               "scale": args.scale, "problems": []}

    def finish(code):
        (out / "verdict.json").write_text(json.dumps(verdict, indent=1, default=str) + "\n")
        print(json.dumps({k: verdict[k] for k in ("run_id", "sample", "NOT_A_GATE_RESULT", "result", "problems")}, default=str))
        return code

    try:
        LOCK.parent.mkdir(parents=True, exist_ok=True)
        t_end = time.time() + 900
        while True:
            try:
                LOCK.mkdir()
                break
            except FileExistsError:
                if time.time() > t_end:
                    raise SystemExit(f"cpu lock still held after 15 minutes: {LOCK}")
                time.sleep(10)
        (LOCK / "owner").write_text(f"g12_run {os.getpid()} {time.ctime()}\n")
        q, d = Quiet(args), Daemon(out, args)
        try:
            ok = q.baseline()
            verdict["idle_baseline"] = q.base
            if not ok:
                verdict["problems"].append("idle baseline refused: p95 load1 above cap, foreign build/daemon processes, or no CPU samples")
                if not s:
                    verdict["result"] = "INVALID"
                    return finish(2)
            croot = d.start()
            verdict["daemon"] = d.prov()
            if not verdict["daemon"]["release"] or verdict["daemon"]["mount_fstype"] != "nfs":
                verdict["problems"].append("daemon is not the release core backend on an NFS mount")
            if fstype(out / "native") == "nfs":
                verdict["problems"].append("native root is on NFS")
            labels = []
            for label, root in (("n1", out / "native"), ("c1", croot), ("n2", out / "native"), ("c2", croot)):
                label = f"g12-{args.run_id}-{label}"
                good, why = q.check(d.proc.pid)
                verdict.setdefault("pre_arm_quiet", []).append({label: why, "ok": good})
                if not good and not s:
                    verdict["problems"].append(f"{label}: host not quiet: {why}")
                    break
                r = subprocess.run([sys.executable, str(BENCH / "gates.py"), "--root", str(root), "--label", label,
                                    "--reps", str(args.reps), "--gates", args.gates], env=env)
                if r.returncode:
                    verdict["problems"].append(f"{label}: gates.py rc {r.returncode}")
                    break
                labels.append(label)
        finally:
            d.stop()
            LOCK_OWNER = LOCK / "owner"
            LOCK_OWNER.unlink(missing_ok=True)
            LOCK.rmdir()
        if len(labels) < 4:
            verdict["result"] = "INVALID"
            return finish(2)
        files = {k: newest(f"g12-{args.run_id}-{k}") for k in ("n1", "c1", "n2", "c2")}
        gl = args.gates.split(",")
        for k, f in files.items():
            _, reps = reps_of(f)
            for g in gl:
                if sum(1 for r in reps if r["gate"] == g) != args.reps:
                    verdict["problems"].append(f"{k} {g}: rep count != {args.reps}")
            hot = [r for r in reps if max(r["load1_before"], r["load1_after"]) > q.limit]
            if hot:
                verdict["problems"].append(f"{k}: {len(hot)} reps above load1 limit {q.limit:.2f}")
        for g in gl:
            m = lambda k: statistics.median(r["wall_s"] for r in reps_of(files[k])[1] if r["gate"] == g)  # noqa: E731
            ratio = m("n2") / m("n1")
            verdict.setdefault("native_native", {})[g] = round(ratio, 3)
            if abs(ratio - 1) > NOISE_BAND:
                verdict["problems"].append(f"{g}: native-native ratio {ratio:.3f} outside 1 +/- {NOISE_BAND}")
        codes = []
        for i, (nat, cow, nf) in enumerate((("n1", "c1", "n2"), ("n2", "c2", "n1"))):
            p = sh([sys.executable, str(BENCH / "compare.py"), "--native", str(files[nat]), "--cowfs", str(files[cow]), "--noise-floor", str(files[nf])])
            (out / f"compare-{i + 1}.txt").write_text(p.stdout + p.stderr)
            codes.append(p.returncode)
        verdict["compare_rc"] = codes
        if s:
            verdict["result"] = "SAMPLE, NOT A GATE RESULT"
            return finish(0 if all(c in (0, 1) for c in codes) else 2)
        if verdict["problems"] or any(c in (2, 3) for c in codes):
            verdict["result"] = "INVALID"
            return finish(2)
        verdict["result"] = "PASS" if codes == [0, 0] else "FAIL"
        return finish(0 if codes == [0, 0] else 1)
    except SystemExit as e:
        verdict["problems"].append(str(e))
        verdict["result"] = "INVALID"
        return finish(2)


if __name__ == "__main__":
    raise SystemExit(main())
