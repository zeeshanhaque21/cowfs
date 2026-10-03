#!/usr/bin/env python3
"""Compare two cowfs criterion-2 arms and rule on the criterion.

Usage: compare.py --native NATIVE.jsonl [NATIVE2.jsonl ...] --cowfs COWFS.jsonl
                  [--noise-floor NOISE.jsonl] [--budget-add 1.0]

Every rep of every gate is read, not just the median, and the median, min and
max of the per-rep ratios are printed alongside the rep counts.

A g5 rep whose read-back did not match what it wrote, or what counts() asked for,
makes the whole comparison invalid (exit 3): that throughput is of a short read.

Ratios are refused, not printed, when the machine was too loaded for them to
mean anything: load1 above 30 on either side, or the two arms more than 2x
apart. Two previous reviewers found timings unmeasurable at load 100 to 300,
so a ratio there is noise with a decimal point on it. The load numbers are
printed and the gate is marked unmeasurable instead.

With --noise-floor, the native arm is run against itself and the native-vs-native
ratio is printed as the noise floor. Without it, no ratio is a finding: run
run-pair.sh, which interleaves native, cowfs, native, cowfs, or report the
baseline yourself.

Criterion (docs/design.md, amended by issue #18):
  g1 clean cargo build      ratio <= 1.5 on every platform
  g2 warm edit-and-rebuild  ratio <= 1.5 off macOS, and on macOS the added
                            seconds over native must stay under --budget-add
  g3 git status             ratio <= 1.5 on every platform
  g4 walk and read          reported only
  g5 large write and read   reported only
  g6 metadata storm         reported only
"""

import argparse
import json
import math
import platform
import statistics
import sys
from pathlib import Path

RATIO_BAR = 1.5
LOAD_CEILING = 30.0
LOAD_SKEW = 2.0
GATES = ["g1", "g2", "g3", "g4", "g5", "g6"]


def load(paths):
    meta = None
    reps = []
    for path in paths:
        for line in Path(path).read_text().splitlines():
            line = line.strip()
            if not line:
                continue
            row = json.loads(line)
            if row.get("kind") == "meta":
                meta = meta or row
            elif row.get("kind") == "rep":
                reps.append(row)
    if not reps:
        raise SystemExit(f"no reps in {paths}")
    return meta, reps


def short_reads(reps):
    return [r for r in reps if r["gate"] == "g5" and not r["metrics"].get("read_matches")]


def by_gate(reps):
    out = {}
    for row in reps:
        out.setdefault(row["gate"], []).append(row)
    for rows in out.values():
        rows.sort(key=lambda r: r["rep"])
    return out


def median(rows):
    return statistics.median([r["wall_s"] for r in rows])


def spread(rows):
    walls = [r["wall_s"] for r in rows]
    return min(walls), max(walls)


def loads(rows):
    vals = [r["load1_before"] for r in rows] + [r["load1_after"] for r in rows]
    vals = [v for v in vals if not math.isnan(v)]
    return max(vals) if vals else float("nan")


def ratios(a, b):
    """Per-rep cowfs/native ratios, zipping rep index then falling back to index order."""
    pairs = []
    for i in range(min(len(a), len(b))):
        pairs.append(b[i]["wall_s"] / a[i]["wall_s"] if a[i]["wall_s"] else float("nan"))
    return [r for r in pairs if not math.isnan(r)]


def verdict(gate, med_a, med_b, mn, mx, on_macos, budget):
    if gate in ("g1", "g3"):
        ok = med_b <= RATIO_BAR * med_a
        return ("PASS" if ok else "FAIL"), f"bar {RATIO_BAR}x"
    if gate == "g2":
        if on_macos:
            add = med_b - med_a
            return ("PASS" if add < budget else "FAIL"), f"add < {budget:.2f}s (add {add:+.3f}s)"
        ok = med_b <= RATIO_BAR * med_a
        return ("PASS" if ok else "FAIL"), f"bar {RATIO_BAR}x"
    return "report", "no bar"


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--native", required=True, nargs="+", help="native arm JSONL file(s)")
    ap.add_argument("--cowfs", required=True, help="cowfs arm JSONL")
    ap.add_argument("--noise-floor", help="an independent native run, for the noise floor")
    ap.add_argument("--budget-add", type=float, default=1.0)
    args = ap.parse_args()

    on_macos = sys.platform == "darwin"
    _, na = load(args.native)
    _, nb = load([args.cowfs])
    bad = short_reads(na) + short_reads(nb)
    if bad:
        for r in bad:
            m = r["metrics"]
            print(f"INVALID g5 rep {r['rep']} of {r['label']}: expected {m.get('bytes')} "
                  f"written {m.get('written_bytes')} read {m.get('read_bytes')}", file=sys.stderr)
        print("RESULT: invalid, g5 did not read back what it wrote", file=sys.stderr)
        return 3
    ga, gb = by_gate(na), by_gate(nb)

    print(f"platform      {platform.platform()}")
    print(f"native arm    {', '.join(args.native)}")
    print(f"cowfs arm     {args.cowfs}")
    print(f"macOS         {on_macos}  (g2 uses an absolute budget on macOS)")
    print()

    print(f"{'gate':<5} {'n_nat':>5} {'n_cow':>5} {'native_s':>9} {'cowfs_s':>9} "
          f"{'med_x':>7} {'min_x':>7} {'max_x':>7} {'load':>6}  result")
    print("-" * 88)

    fails = 0
    unmeasurable = 0
    for gate in GATES:
        if gate not in ga or gate not in gb:
            continue
        a, b = ga[gate], gb[gate]
        ma, mb = median(a), median(b)
        _, _ = spread(a), spread(b)
        r = ratios(a, b)
        la, lb = loads(a), loads(b)
        peak = max(la, lb)
        skewed = max(la, lb) > LOAD_SKEW * max(1e-9, min(la, lb))
        med_r = statistics.median(r) if r else float("nan")
        mn = min(r) if r else float("nan")
        mx = max(r) if r else float("nan")
        if r:
            print(f"{gate:<5} {len(a):>5} {len(b):>5} {ma:>9.4f} {mb:>9.4f} "
                  f"{med_r:>7.3f} {mn:>7.3f} {mx:>7.3f} {peak:>6.1f}  ", end="")
        else:
            print(f"{gate:<5} {len(a):>5} {len(b):>5} {ma:>9.4f} {mb:>9.4f} "
                  f"{'-':>7} {'-':>7} {'-':>7} {peak:>6.1f}  ", end="")
        if peak > LOAD_CEILING or skewed or math.isnan(med_r):
            print(f"UNMEASURABLE: load1 peak {peak:.1f} "
                  f"(native {la:.1f}, cowfs {lb:.1f}, ceiling {LOAD_CEILING}, "
                  f"skew limit {LOAD_SKEW}x)")
            unmeasurable += 1
            continue
        status, why = verdict(gate, ma, mb, mn, mx, on_macos, args.budget_add)
        print(f"{status} ({why})")
        if status == "FAIL":
            fails += 1

    if args.noise_floor:
        _, nc = load([args.noise_floor])
        gc = by_gate(nc)
        print()
        print("noise floor, native arm against itself (a real ratio here is machine noise)")
        print(f"{'gate':<5} {'n':>5} {'run_a_s':>9} {'run_b_s':>9} {'ratio':>7} {'load':>6}")
        print("-" * 60)
        for gate in GATES:
            if gate not in ga or gate not in gc:
                continue
            a, c = ga[gate], gc[gate]
            ra = ratios(a, c)
            if not ra:
                continue
            peak = max(loads(a), loads(c))
            flag = "unmeasurable" if peak > LOAD_CEILING else ""
            print(f"{gate:<5} {min(len(a), len(c)):>5} {median(a):>9.4f} {median(c):>9.4f} "
                  f"{statistics.median(ra):>7.3f} {peak:>6.1f}  {flag}")
    else:
        print()
        print("noise floor: not measured. Pass --noise-floor, or use run-pair.sh, "
              "which runs the native arm twice around the cowfs arms.")

    print()
    if unmeasurable:
        print(f"RESULT: {unmeasurable} gate(s) unmeasurable, {fails} failed")
        return 2
    print(f"RESULT: {'PASS' if fails == 0 else f'FAIL ({fails})'}")
    return 1 if fails else 0


if __name__ == "__main__":
    raise SystemExit(main())
