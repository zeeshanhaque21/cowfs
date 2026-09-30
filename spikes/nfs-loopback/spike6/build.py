#!/usr/bin/env python3
"""build.py <crate X|Y> <variant> <slot>... : create pool/<crate>-<variant>/<slot> from crate source and cargo build it, under the CPU lock."""
import json, os, shutil, subprocess, sys, time

ROOT = "/Users/zeeshanhaque/Projects/cowfs/spikes"
OUT = f"{ROOT}/nfs-loopback/out/spike6"
LOCK = f"{ROOT}/nfs-loopback/out/cpu.lock"
CRATES = {"X": f"{ROOT}/dedup-corpus", "Y": f"{ROOT}/nfs-loopback"}
# variant -> (cargo profile flags, env, extra cargo args); {slot} expands to the slot dir
VARIANTS = {
    "a": ([], {}, []),
    "b": (["--release"], {}, []),
    "c": ([], {"RUSTFLAGS": "--remap-path-prefix={slot}=/w"}, []),
    "d": (["--release"], {"RUSTFLAGS": "--remap-path-prefix={slot}=/w"}, []),
    "e0": ([], {"CARGO_PROFILE_DEV_DEBUG": "0"}, []),
    "e1": ([], {"CARGO_PROFILE_DEV_DEBUG": "line-tables-only"}, []),
    "f": ([], {}, ["--config", 'profile.dev.trim-paths="all"']),  # rejected by stable cargo 1.98
    "f2": ([], {"RUSTC_BOOTSTRAP": "1"}, ["-Ztrim-paths", "--config", 'profile.dev.trim-paths="all"']),
    "f3": (["--release"], {"RUSTC_BOOTSTRAP": "1"}, ["-Ztrim-paths", "--config", 'profile.release.trim-paths="all"']),
    "g0": ([], {"CARGO_PROFILE_DEV_SPLIT_DEBUGINFO": "off"}, []),
    "g1": ([], {"CARGO_PROFILE_DEV_SPLIT_DEBUGINFO": "packed"}, []),
    "i": ([], {"CARGO_INCREMENTAL": "0"}, []),
    "k": ([], {"RUSTFLAGS": "--remap-path-prefix={slot}=/w", "CARGO_INCREMENTAL": "0"}, []),
    "h": ([], {"RUSTFLAGS": "--remap-path-prefix={slot}=/w", "CARGO_PROFILE_DEV_DEBUG": "0"}, []),
}


def lock():
    t0 = time.time()
    while True:
        try:
            os.mkdir(LOCK)
            open(f"{LOCK}/owner", "w").write(f"spike6 pid={os.getpid()} {time.ctime()}\n")
            return
        except FileExistsError:
            if time.time() - t0 > 3000:
                sys.exit("cpu.lock wait > 50 min")
            time.sleep(10)


def unlock():
    shutil.rmtree(LOCK, ignore_errors=True)


def ensure_slot(crate, pool, slot):
    d = f"{OUT}/pool/{pool}/{slot}"
    if not os.path.isdir(d):
        os.makedirs(os.path.dirname(d), exist_ok=True)
        shutil.copytree(CRATES[crate], d, ignore=shutil.ignore_patterns("target", "out", "__pycache__", "validate.py"))
    return d


def build(d, variant, extra=(), flags_slot=None):
    prof, env, args = VARIANTS[variant]
    e = {k: v for k, v in os.environ.items() if not k.startswith(("CARGO_", "RUSTFLAGS")) or k in ("CARGO_HOME",)}
    e.update({k: v.format(slot=flags_slot or d) for k, v in env.items()})
    cmd = ["cargo", "build", "--frozen", "-v", *prof, *args, *extra]
    l0 = os.getloadavg()[0]
    t = time.time()
    p = subprocess.run(cmd, cwd=d, env=e, capture_output=True, text=True)
    r = {"dir": d, "variant": variant, "rc": p.returncode, "secs": round(time.time() - t, 1), "load1_before": round(l0, 1),
         "load1_after": round(os.getloadavg()[0], 1),
         "fresh": p.stderr.count("Fresh "), "compiling": p.stderr.count("Running `"), "tail": [l[:120] for l in p.stderr.strip().splitlines()[-1:]]}
    open(f"{d}.build.log", "a").write(" ".join(cmd) + "\n" + p.stderr + "\n")
    return r


if __name__ == "__main__":
    crate, variant, slots = sys.argv[1], sys.argv[2], sys.argv[3:]
    pool = f"{crate}-{variant}"
    dirs = [ensure_slot(crate, pool, s) for s in slots]
    lock()
    try:
        for d in dirs:
            r = build(d, variant)
            print(json.dumps(r))
            open(f"{OUT}/builds.jsonl", "a").write(json.dumps(r) + "\n")
            if r["rc"]:
                sys.exit(1)
    finally:
        unlock()
