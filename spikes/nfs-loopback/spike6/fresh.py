#!/usr/bin/env python3
"""fresh.py : copy a built slot (cp -c -R -p) to a new path and rebuild with -v; count Fresh vs Compiling. Writes out/spike6/fresh.json"""
import json, os, re, shutil, subprocess, sys, time
sys.path.insert(0, os.path.dirname(__file__))
import build

OUT = build.OUT
F = f"{OUT}/fresh"
shutil.rmtree(F, ignore_errors=True)
os.makedirs(F)


def cargo(d, release, rustflags, extra_env=None):
    e = {k: v for k, v in os.environ.items() if not k.startswith(("CARGO_", "RUSTFLAGS")) or k == "CARGO_HOME"}
    if rustflags: e["RUSTFLAGS"] = rustflags
    e.update(extra_env or {})
    p = subprocess.run(["cargo", "build", "--frozen", "-v"] + (["--release"] if release else []), cwd=d, env=e, capture_output=True, text=True)
    return {"rc": p.returncode, "fresh": len(re.findall(r"^\s*Fresh ", p.stderr, re.M)), "compiling": len(re.findall(r"^\s*Compiling ", p.stderr, re.M))}


def case(crate, variant, release, flags_fn, extra_env=None):
    """flags_fn(slotdir) -> RUSTFLAGS for that slot"""
    src = f"{OUT}/pool/{crate}-{variant}/2"
    res = {}
    for label, mk in [("copy_new_flags", lambda d: flags_fn(d)), ("copy_old_flags", lambda d: flags_fn(src))]:
        d = f"{F}/{crate}-{variant}-{label}/9"
        os.makedirs(os.path.dirname(d))
        subprocess.run(["cp", "-c", "-R", "-p", src, d], check=True)
        res[label] = cargo(d, release, mk(d), extra_env)
    d = f"{F}/{crate}-{variant}-samepath/9"
    return res


if __name__ == "__main__":
    crate = sys.argv[1]
    remap = lambda d: f"--remap-path-prefix={d}=/w"
    none = lambda d: None
    cases = [("a", False, none, None), ("b", True, none, None), ("c", False, remap, None), ("d", True, remap, None),
             ("e0", False, none, {"CARGO_PROFILE_DEV_DEBUG": "0"}), ("g0", False, none, {"CARGO_PROFILE_DEV_SPLIT_DEBUGINFO": "off"})]
    out = {}
    build.lock()
    try:
        for v, rel, fl, ee in cases:
            if not os.path.isdir(f"{OUT}/pool/{crate}-{v}/2"): continue
            out[f"{crate}-{v}"] = case(crate, v, rel, fl, ee)
            print(crate, v, out[f"{crate}-{v}"], flush=True)
    finally:
        build.unlock()
    json.dump(out, open(f"{OUT}/fresh-{crate}.json", "w"), indent=1)
