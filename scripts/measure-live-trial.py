#!/usr/bin/env python3
"""Point-in-time storage and matched-build measurement of a live cowfs mount.

Subcommands (all take --run ID, which names one unique run directory):

  init       create the run directories exclusively and a marker in each
  status     append one store/daemon snapshot to the run's JSONL
  walk       bounded read-only traversal of the live mount: apparent bytes, hardlinks, errors
  build      clone, fetch, smoke, then timed reps of clean / no-op / edit builds across arms
  bg         sample store growth while this script does nothing else
  summarize  print per-phase medians, ranges and ratios from the JSONL

Nothing here cleans, kills or writes outside RUN dirs. The only deletion is the target
directory inside an arm directory that carries this run's marker file.

Raw records go to NATIVE_BASE/metrics.jsonl, appended, flushed and fsynced per record.
"""

import argparse
import hashlib
import json
import os
import re
import statistics
import subprocess
import sys
import threading
import time
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
NATIVE_PARENT = Path(
    os.environ.get(
        "LIVE_TRIAL_NATIVE", "/Users/zeeshanhaque/Projects/cowfs/bench/out/live-trial-native"
    )
)
MOUNT = Path(os.environ.get("LIVE_TRIAL_MOUNT", "/Users/zeeshanhaque/.cowfs/mnt"))
STORE = Path(os.environ.get("LIVE_TRIAL_STORE", "/Users/zeeshanhaque/.cowfs/store"))
SOCK = os.environ.get("LIVE_TRIAL_SOCK", "/Users/zeeshanhaque/.cowfs/sock/daemon.sock")
CLI = os.environ.get(
    "LIVE_TRIAL_CLI",
    "/Users/zeeshanhaque/Projects/cowfs/spikes/nfs-loopback/out/live/bin/cowfs",
)
SHA = "ab99868ba36d12fa1cb4ba35738bf71ee8fede63"
EDIT_TARGET = "crates/cowfs-vfs-path/src/cookies.rs"
PACKAGE = "cowfs-daemon"
BIN = "debug/cowfs-daemon"
MARKER = ".live-trial-owned"
QUIET_LOAD1 = 8.0  # half the 16 cores: above this a timing is labelled loaded
NOISE_BAND = 0.10  # native-vs-native median ratio must be within 1 +/- this
ARMS = ["nativeA", "cowfs", "nativeB"]


class Run:
    def __init__(self, run_id: str):
        if not re.fullmatch(r"[A-Za-z0-9._-]+", run_id):
            raise SystemExit("bad run id")
        self.id = run_id
        self.native = NATIVE_PARENT / run_id
        self.cow = REPO / "bench" / "out" / "live-trial" / run_id
        self.jsonl = self.native / "metrics.jsonl"
        self.cargo_home = self.native / "cargo-home"
        self.arm_dir = {
            "nativeA": self.native / "a",
            "nativeB": self.native / "b",
            "cowfs": self.cow / "c",
        }

    def owned(self, p: Path) -> bool:
        p = p.resolve()
        for base in (self.native, self.cow):
            if (base / MARKER).is_file() and str(p).startswith(str(base.resolve()) + os.sep):
                return True
        return False


def emit(run: Run, rec: dict):
    rec = {"ts": time.strftime("%Y-%m-%dT%H:%M:%S%z"), "run": run.id, **rec}
    with open(run.jsonl, "a") as fh:
        fh.write(json.dumps(rec, sort_keys=True) + "\n")
        fh.flush()
        os.fsync(fh.fileno())


def sh(cmd, cwd=None, env=None, timeout=3600):
    t = time.monotonic()
    p = subprocess.run(
        cmd, cwd=cwd, env=env, capture_output=True, text=True, timeout=timeout, check=False
    )
    return p, time.monotonic() - t


def checked(cmd, **kw):
    p, _ = sh(cmd, **kw)
    if p.returncode != 0:
        raise SystemExit(f"FAIL {cmd}\n{p.stdout[-2000:]}\n{p.stderr[-2000:]}")
    return p


# ---- point-in-time host and store state ------------------------------------------------


def daemon_pid():
    try:
        return int(Path.home().joinpath(".cowfs/daemon.pid").read_text().strip())
    except (OSError, ValueError):
        return None


def proc(pid):
    if not pid:
        return None
    p, _ = sh(["ps", "-o", "pcpu=,rss=,etime=", "-p", str(pid)])
    f = p.stdout.split()
    if len(f) != 3:
        return None
    return {"pid": pid, "pcpu": float(f[0]), "rss_kb": int(f[1]), "etime": f[2]}


def host():
    p, _ = sh(["ps", "-A", "-o", "pcpu=,comm="])
    rows = []
    for line in p.stdout.splitlines():
        a = line.split(None, 1)
        if len(a) == 2:
            try:
                rows.append((float(a[0]), os.path.basename(a[1])))
            except ValueError:
                pass
    rows.sort(reverse=True)
    l1, l5, l15 = os.getloadavg()
    return {
        "load1": l1,
        "load5": l5,
        "load15": l15,
        "ps_pcpu_sum": round(sum(r[0] for r in rows), 1),
        "ps_top3": [[r[0], r[1][:30]] for r in rows[:3]],
        "daemon": proc(daemon_pid()),
    }


def store_status():
    p, _ = sh([CLI, "--socket", SOCK, "--timeout", "5", "--json", "status"], timeout=20)
    try:
        return json.loads(p.stdout)
    except ValueError:
        return {"error": (p.stdout + p.stderr)[-200:]}


def store_files():
    out = {"files": {}, "pack_apparent": 0, "pack_allocated": 0, "packs": 0}
    for d in (STORE, STORE / "store", STORE / "store" / "packs"):
        try:
            for e in os.scandir(d):
                if e.is_file(follow_symlinks=False):
                    st = e.stat(follow_symlinks=False)
                    if d.name == "packs":
                        out["packs"] += 1
                        out["pack_apparent"] += st.st_size
                        out["pack_allocated"] += st.st_blocks * 512
                    else:
                        out["files"][e.name] = [st.st_size, st.st_blocks * 512]
        except OSError as e:
            out.setdefault("errors", []).append(str(e))
    out["physical_apparent"] = out["pack_apparent"] + sum(v[0] for v in out["files"].values())
    out["physical_allocated"] = out["pack_allocated"] + sum(v[1] for v in out["files"].values())
    return out


def fs_of(path: Path):
    p, _ = sh(["df", "-P", str(path)])
    last = p.stdout.strip().splitlines()[-1].split()
    m, _ = sh(["mount"])
    on = last[-1]
    typ = next((l for l in m.stdout.splitlines() if f" on {on} (" in l), "")
    return {"df_source": last[0], "mounted_on": on, "mount_line": typ}


def cmd_init(run: Run, a):
    for base in (run.native, run.cow):
        base.mkdir(parents=True, exist_ok=False)
        (base / MARKER).write_text(f"{run.id}\n")
    for d in run.arm_dir.values():
        d.mkdir(exist_ok=False)
    therm, _ = sh(["pmset", "-g", "therm"])
    emit(
        run,
        {
            "kind": "meta",
            "repo": str(REPO),
            "sha": SHA,
            "arms": {k: str(v) for k, v in run.arm_dir.items()},
            "fs": {k: fs_of(v) for k, v in run.arm_dir.items()},
            "ncpu": os.cpu_count(),
            "rustc": checked(["rustc", "-V"]).stdout.strip(),
            "cargo": checked(["cargo", "-V"]).stdout.strip(),
            "therm": therm.stdout.strip(),
            "quiet_load1": QUIET_LOAD1,
            "noise_band": NOISE_BAND,
            "cargo_env": {k: v for k, v in os.environ.items() if k.startswith(("CARGO", "RUST"))},
            "daemon_cmd": sh(["ps", "-o", "command=", "-p", str(daemon_pid())])[0].stdout.strip(),
        },
    )
    print("run dirs:", run.native, run.cow)


def cmd_status(run: Run, a):
    emit(run, {"kind": "status", "label": a.label, "host": host(), "store": store_status(),
               "files": store_files()})


def cmd_bg(run: Run, a):
    end = time.monotonic() + a.seconds
    while time.monotonic() < end:
        emit(run, {"kind": "bg", "label": a.label, "host": host(), "store": store_status()})
        time.sleep(a.every)


# ---- bounded read-only tree walk --------------------------------------------------------


def classify(rel: str) -> str:
    parts = rel.split(os.sep)
    for tag in (".git", "node_modules", "target"):
        if tag in parts:
            return tag
    return "other"


def walk_root(root: Path, deadline: float, seen: set, prog: list, exclude: Path):
    acc = {"files": 0, "dirs": 0, "symlinks": 0, "other": 0, "apparent": 0, "allocated": 0,
           "hardlink_dupes": 0, "hardlink_dupe_bytes": 0, "vanished": 0, "errors": {},
           "by_class": {}, "complete": True}
    stack = [(root, "other")]
    while stack:
        d, cls = stack.pop()
        if time.monotonic() > deadline:
            acc["complete"] = False
            break
        try:
            it = list(os.scandir(d))
        except FileNotFoundError:
            acc["vanished"] += 1
            continue
        except OSError as e:
            acc["errors"][str(e.errno)] = acc["errors"].get(str(e.errno), 0) + 1
            continue
        acc["dirs"] += 1
        for e in it:
            prog[0] += 1
            try:
                st = e.stat(follow_symlinks=False)
            except FileNotFoundError:
                acc["vanished"] += 1
                continue
            except OSError as ex:
                acc["errors"][str(ex.errno)] = acc["errors"].get(str(ex.errno), 0) + 1
                continue
            mode = st.st_mode & 0o170000
            if mode == 0o040000:
                if Path(e.path) == exclude:
                    continue
                c = cls if cls != "other" else classify(e.name)
                stack.append((Path(e.path), c))
            elif mode == 0o100000:
                key = (st.st_dev, st.st_ino)
                bc = acc["by_class"].setdefault(cls, [0, 0])
                if st.st_nlink > 1 and key in seen:
                    acc["hardlink_dupes"] += 1
                    acc["hardlink_dupe_bytes"] += st.st_size
                    continue
                if st.st_nlink > 1:
                    seen.add(key)
                acc["files"] += 1
                acc["apparent"] += st.st_size
                acc["allocated"] += st.st_blocks * 512
                bc[0] += 1
                bc[1] += st.st_size
            elif mode == 0o120000:
                acc["symlinks"] += 1
            else:
                acc["other"] += 1
    return acc


def cmd_walk(run: Run, a):
    base = Path(a.root)
    exclude = REPO / "bench" / "out" / "live-trial"
    roots = []
    for e in sorted(os.scandir(base), key=lambda e: e.name):
        if e.name == ".treehouse":
            for pool in sorted(os.scandir(e.path), key=lambda x: x.name):
                if pool.is_dir(follow_symlinks=False):
                    slots = [s for s in os.scandir(pool.path) if s.is_dir(follow_symlinks=False)]
                    roots += [Path(s.path) for s in sorted(slots, key=lambda x: x.name)] or [Path(pool.path)]
        else:
            roots.append(Path(e.path))
    before = {"host": host(), "store": store_status()}
    emit(run, {"kind": "walk_start", "base": str(base), "roots": len(roots), "budget_s": a.budget,
               "exclude": str(exclude), **before})
    prog = [0]
    stop = threading.Event()

    def watchdog():
        last, t_last = -1, time.monotonic()
        while not stop.wait(5):
            if prog[0] != last:
                last, t_last = prog[0], time.monotonic()
            elif time.monotonic() - t_last > a.no_progress:
                emit(run, {"kind": "walk_abort", "reason": "no progress", "entries": prog[0]})
                os._exit(3)

    threading.Thread(target=watchdog, daemon=True).start()
    deadline = time.monotonic() + a.budget
    seen: set = set()
    done = skipped = 0
    for r in roots:
        if time.monotonic() > deadline:
            skipped += 1
            continue
        t = time.monotonic()
        acc = walk_root(r, deadline, seen, prog, exclude)
        emit(run, {"kind": "walk_root", "root": str(r.relative_to(base)), "secs": round(time.monotonic() - t, 2), **acc})
        done += 1
    stop.set()
    emit(run, {"kind": "walk_end", "roots_walked": done, "roots_not_started": skipped,
               "entries": prog[0], "after": {"host": host(), "store": store_status()}})
    print(f"walk: {done} roots walked, {skipped} not started, {prog[0]} entries")


# ---- matched builds ----------------------------------------------------------------------


def build_env(run: Run, arm: str):
    env = dict(os.environ)
    env["CARGO_HOME"] = str(run.cargo_home)
    env["CARGO_TARGET_DIR"] = str(run.arm_dir[arm] / "corpus-target")
    env["CARGO_TERM_COLOR"] = "never"
    for k in ("CARGO_INCREMENTAL", "RUSTC_WRAPPER", "RUSTFLAGS"):
        env.pop(k, None)
    return env


def corpus(run: Run, arm: str) -> Path:
    return run.arm_dir[arm] / "corpus"


def setup_arm(run: Run, arm: str):
    c = corpus(run, arm)
    if not (c / ".git").exists():
        checked(["git", "clone", "--no-hardlinks", "--quiet", str(REPO), str(c)])
        checked(["git", "-C", str(c), "checkout", "--quiet", "--detach", SHA])
    head = checked(["git", "-C", str(c), "rev-parse", "HEAD"]).stdout.strip()
    if head != SHA:
        raise SystemExit(f"{arm}: HEAD {head} != {SHA}")
    ls = checked(["git", "-C", str(c), "ls-files", "-s"]).stdout
    lock = hashlib.sha256((c / "Cargo.lock").read_bytes()).hexdigest()
    return {"head": head, "manifest_sha256": hashlib.sha256(ls.encode()).hexdigest(),
            "files": ls.count("\n"), "cargo_lock_sha256": lock}


def parse_finished(text: str):
    """Seconds from cargo's `Finished ... in 1.23s` or `in 2m 03s`. None if absent."""
    m = re.search(r"Finished .*? in ([^\n]*)", text)
    if not m:
        return None
    total, seen = 0.0, False
    for value, unit in re.findall(r"([0-9]+(?:\.[0-9]+)?)\s*(m|s)\b", m.group(1)):
        seen = True
        total += float(value) * (60.0 if unit == "m" else 1.0)
    return total if seen else None


def cargo_build(run: Run, arm: str):
    cmd = ["cargo", "build", "--offline", "--locked", "-p", PACKAGE, "-j", "4"]
    p, secs = sh(cmd, cwd=corpus(run, arm), env=build_env(run, arm))
    return {"cmd": " ".join(cmd), "rc": p.returncode, "wall_s": round(secs, 3),
            "compiling": len(re.findall(r"^\s*Compiling ", p.stderr, re.M)),
            "cargo_s": parse_finished(p.stderr),
            "stderr_tail": p.stderr[-300:] if p.returncode else ""}


def validate_bin(run: Run, arm: str):
    b = run.arm_dir[arm] / "corpus-target" / BIN
    p, _ = sh([str(b), "--help"], timeout=60)
    return {"bin_bytes": b.stat().st_size if b.exists() else None, "help_rc": p.returncode,
            "help_sha256": hashlib.sha256((p.stdout + p.stderr).encode()).hexdigest()[:16],
            "help_has_store_flag": "--store" in (p.stdout + p.stderr)}


def tree_bytes(path: Path):
    n = b = 0
    for dp, _, fs in os.walk(path):
        for f in fs:
            try:
                st = os.lstat(os.path.join(dp, f))
            except OSError:
                continue
            n += 1
            b += st.st_size
    return {"files": n, "apparent": b}


def phase(run: Run, rep, arm, name, fn):
    h0, s0 = host(), store_status()
    t0 = time.strftime("%H:%M:%S")
    res = fn()
    h1, s1 = host(), store_status()
    emit(run, {"kind": "phase", "rep": rep, "arm": arm, "phase": name, "start": t0,
               "host_before": h0, "host_after": h1, "store_before": s0, "store_after": s1, **res})
    flag = "" if res["rc"] == 0 else "  FAILED"
    print(f"rep{rep} {arm:8s} {name:6s} {res['wall_s']:8.2f}s compiling={res['compiling']} "
          f"load1={h0['load1']:.1f}->{h1['load1']:.1f}{flag}", flush=True)
    if res["rc"] != 0:
        raise SystemExit(f"build failed: {res['stderr_tail']}")


def clean_target(run: Run, arm: str):
    t = run.arm_dir[arm] / "corpus-target"
    if not t.exists():
        return
    # verification before the irreversible step: the run's own marker must sit above it.
    # Explicit if/raise, not assert: python -O strips assert and would remove the guard.
    if not run.owned(t):
        raise SystemExit(f"refusing to remove {t}: no run marker above it")
    checked(["rm", "-rf", str(t)])
    if t.exists():
        raise SystemExit(f"{t} survived rm -rf")


def edit_leaf(run: Run, arm: str, rep: int):
    f = corpus(run, arm) / EDIT_TARGET
    checked(["git", "-C", str(corpus(run, arm)), "checkout", "--quiet", "--", EDIT_TARGET])
    f.write_text(f.read_text() + f"\n// live-trial edit rep {rep}\n")


def cmd_build(run: Run, a):
    setups = {}
    for arm in ARMS:
        setups[arm] = setup_arm(run, arm)
    if len({(v["head"], v["manifest_sha256"], v["cargo_lock_sha256"]) for v in setups.values()}) != 1:
        raise SystemExit("arms differ in source")
    emit(run, {"kind": "setup", "arms": setups})
    checked(["cargo", "fetch", "--locked", "--manifest-path", str(corpus(run, "nativeA") / "Cargo.toml")],
            env=build_env(run, "nativeA"))
    if a.smoke:
        for arm in ARMS:
            clean_target(run, arm)
            phase(run, 0, arm, "smoke", lambda arm=arm: {**cargo_build(run, arm), **validate_bin(run, arm)})
        return
    for rep in range(1, a.reps + 1):
        order = ARMS[(rep - 1) % 3:] + ARMS[: (rep - 1) % 3]
        for arm in order:
            clean_target(run, arm)
            phase(run, rep, arm, "clean", lambda: {**cargo_build(run, arm), **validate_bin(run, arm)})
            phase(run, rep, arm, "noop", lambda: cargo_build(run, arm))
            edit_leaf(run, arm, rep)
            phase(run, rep, arm, "edit", lambda: {**cargo_build(run, arm), **validate_bin(run, arm)})
            emit(run, {"kind": "target_size", "rep": rep, "arm": arm,
                       **tree_bytes(run.arm_dir[arm] / "corpus-target")})
            checked(["git", "-C", str(corpus(run, arm)), "checkout", "--quiet", "--", EDIT_TARGET])


# ---- summary ------------------------------------------------------------------------------


def med(xs):
    return statistics.median(xs) if xs else None


def cmd_summarize(run: Run, a):
    recs = [json.loads(l) for l in open(run.jsonl)]
    ph = [r for r in recs if r["kind"] == "phase" and r["phase"] != "smoke"]
    out = {}
    for name in ("clean", "noop", "edit"):
        by = {arm: {r["rep"]: r["wall_s"] for r in ph if r["arm"] == arm and r["phase"] == name}
              for arm in ARMS}
        loads = [max(r["host_before"]["load1"], r["host_after"]["load1"])
                 for r in ph if r["phase"] == name]
        reps = sorted(set(by["nativeA"]) & set(by["nativeB"]) & set(by["cowfs"]))
        nat = {k: (by["nativeA"][k] + by["nativeB"][k]) / 2 for k in reps}
        ratios = [by["cowfs"][k] / nat[k] for k in reps]
        nvn = [by["nativeB"][k] / by["nativeA"][k] for k in reps]
        added = [by["cowfs"][k] - nat[k] for k in reps]
        out[name] = {
            "reps": len(reps),
            "wall_s": {arm: {"median": med(list(v.values())), "min": min(v.values(), default=None),
                              "max": max(v.values(), default=None)} for arm, v in by.items()},
            "cowfs_over_native_ratio": {"median": med(ratios), "min": min(ratios, default=None), "max": max(ratios, default=None)},
            "nativeB_over_nativeA_ratio": {"median": med(nvn), "min": min(nvn, default=None), "max": max(nvn, default=None)},
            "cowfs_added_s": {"median": med(added), "min": min(added, default=None), "max": max(added, default=None)},
            "max_load1": max(loads, default=None),
        }
    quiet = all(v["max_load1"] is not None and v["max_load1"] <= QUIET_LOAD1 for v in out.values())
    nvn_ok = all(v["nativeB_over_nativeA_ratio"]["median"] is not None
                 and abs(v["nativeB_over_nativeA_ratio"]["median"] - 1) <= NOISE_BAND for v in out.values())
    out["validity"] = {"quiet_host": quiet, "native_native_within_band": nvn_ok,
                       "verdict": "VALID" if quiet and nvn_ok else "PROVISIONAL: no gate verdict"}
    print(json.dumps(out, indent=1, sort_keys=True))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--run", required=True)
    sub = ap.add_subparsers(dest="cmd", required=True)
    sub.add_parser("init")
    s = sub.add_parser("status"); s.add_argument("--label", default="")
    s = sub.add_parser("bg"); s.add_argument("--label", default="bg"); s.add_argument("--seconds", type=int, default=60); s.add_argument("--every", type=int, default=5)
    s = sub.add_parser("walk"); s.add_argument("--root", default=str(MOUNT / "base")); s.add_argument("--budget", type=int, default=300); s.add_argument("--no-progress", type=int, default=90)
    s = sub.add_parser("build"); s.add_argument("--smoke", action="store_true"); s.add_argument("--reps", type=int, default=3)
    sub.add_parser("summarize")
    a = ap.parse_args()
    run = Run(a.run)
    if a.cmd != "init" and not run.jsonl.exists():
        raise SystemExit("run not initialised")
    {"init": cmd_init, "status": cmd_status, "bg": cmd_bg, "walk": cmd_walk, "build": cmd_build,
     "summarize": cmd_summarize}[a.cmd](run, a)


if __name__ == "__main__":
    main()
