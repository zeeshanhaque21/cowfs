#!/usr/bin/env python3
"""Build benchmark: native APFS copy vs NFS mount copy, interleaved.

usage: bench.py <label> [runs] [sides] [incr]   sides default native,nfs; incr = only the one-line-edit rebuild
Appends rows to out/bench.csv: label,run,side,kind,seconds
"""
import os, shutil, subprocess, sys, time

HERE = os.path.dirname(os.path.abspath(__file__))
SIDES = {"native": os.path.join(HERE, "out/native"), "nfs": os.path.join(HERE, "out/mnt/build")}
LOG = open(os.path.join(HERE, "out/bench_cargo.log"), "a")


def cargo(d, *args):
    t = time.monotonic()
    r = subprocess.run(["cargo", "build", *args], cwd=d, stdout=LOG, stderr=subprocess.PIPE, timeout=600,
                       env={**os.environ, "CARGO_TERM_COLOR": "never"})
    dt = time.monotonic() - t
    LOG.write(r.stderr.decode())
    if r.returncode != 0 or b"Compiling dedup-corpus" not in r.stderr:
        sys.exit(f"cargo build {args} failed or did not compile in {d}")
    return dt


def check_bin(d, profile):
    r = subprocess.run([os.path.join(d, "target", profile, "dedup-corpus"), "--bogus"],
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=60)
    if r.returncode != 101:
        sys.exit(f"{d} {profile} binary exit {r.returncode}, want 101")


def clean(d):
    t = os.path.join(d, "target")
    if os.path.exists(t):
        shutil.rmtree(t)


def run_side(d, incr_only=False):
    out = {}
    if incr_only:
        subprocess.run(["cargo", "build"], cwd=d, stdout=LOG, stderr=LOG, timeout=600, check=True)
    else:
        clean(d)
        out["debug_clean"] = cargo(d)
        check_bin(d, "debug")
    src = os.path.join(d, "src/main.rs")
    orig = open(src, "rb").read()
    try:
        with open(src, "ab") as f:
            f.write(b"// bench edit\n")
        out["debug_incr"] = cargo(d)
        check_bin(d, "debug")
    finally:
        with open(src, "wb") as f:
            f.write(orig)
    if incr_only:
        return out
    clean(d)
    out["release_clean"] = cargo(d, "--release")
    check_bin(d, "release")
    return out


def main():
    label = sys.argv[1]
    runs = int(sys.argv[2]) if len(sys.argv) > 2 else 3
    sides = sys.argv[3].split(",") if len(sys.argv) > 3 else ["native", "nfs"]
    incr_only = len(sys.argv) > 4 and sys.argv[4] == "incr"
    csv = open(os.path.join(HERE, "out/bench.csv"), "a")
    for i in range(runs):
        for s in sides:
            for k, v in run_side(SIDES[s], incr_only).items():
                row = f"{label},{i},{s},{k},{v:.2f}"
                print(row, flush=True)
                csv.write(row + "\n")
                csv.flush()


if __name__ == "__main__":
    main()
