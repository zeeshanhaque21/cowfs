#!/usr/bin/env python3
"""Compare two cowfs criterion-2 arms and rule on the criterion.

Usage: compare.py --native NATIVE.jsonl [NATIVE2.jsonl ...] --cowfs COWFS.jsonl
                  [--noise-floor NOISE.jsonl] [--budget-add 1.0]

Every rep of every gate is read, not just the median, and the median, min and
max of the per-rep ratios are printed alongside the rep counts.

Every input file, the noise-floor file included, is validated first, and any failure
exits 3 with no comparison printed. A file needs exactly one valid meta record before
its first rep, whose counts.big_bytes is an integer multiple of 1 MiB, at least 1 MiB,
and equal to the value its scale implies when the meta records a scale, and at least
one rep. Every rep, whatever its gate, needs a recognised gate, an integer rep index,
a finite non-bool non-negative wall_s, and a load1_before and load1_after that are
numeric, non-bool, and either NaN (what the harness writes when getloadavg fails) or
finite and non-negative. Infinity, -Infinity and finite negative values are all
refused, in every input slot. Every
g5 rep additionally needs integer (not bool, not float) bytes, written_bytes and
read_bytes that all equal meta big_bytes; the read_matches flag is not consulted. There
is no legacy exemption: rows with only a flag, and files whose meta or reps break the
rules above, are invalid, never a performance verdict.

g5 is all or nothing across the inputs. If any input has g5 reps, every input must,
else exit 3 naming the ones that do not. If no input has g5 reps, a scoped comparison
still runs and is judged per gate as before, and the output prints `g5   not run (no
input has g5 reps)` instead of a g5 line, never a g5 pass or a throughput claim. So
g1/g2/g3-only comparisons keep working, and a g5 result can never be certified without
its byte counts. A comparison in which no gate is present in both the native and the
cowfs arm is refused (exit 3), not printed as a zero-gate pass. This is a
byte-accounting check on the harness's own output, not provenance: it cannot tell a
hand-written but internally consistent file from a real run.

Partial gate coverage is reported, never silent (issue #80). A gate that some input
requested, or that has reps in one arm only, is listed with the arm that has no data
for it and the inputs that asked for it, so a dropped gate reads as a dropped gate
rather than as an absent line. The same block prints one `coverage {...}` line, the
machine-readable form of the same facts. The aggregate verdict carries its own scope,
naming how many of the six gates were compared and which were not, so a partial result
never reads as though every production gate was exercised. Coverage changes no verdict
and no exit code: a matched g1-only or g1/g3-only comparison is still 0, and the gates
it did not run are still not evidence about anything.

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

sys.path.insert(0, str(Path(__file__).resolve().parent))
import gates  # noqa: E402

RATIO_BAR = 1.5
LOAD_CEILING = 30.0
LOAD_SKEW = 2.0
GATES = ["g1", "g2", "g3", "g4", "g5", "g6"]


def rows_of(path):
    out = []
    for line in Path(path).read_text().splitlines():
        line = line.strip()
        if not line:
            continue
        row = json.loads(line)
        if isinstance(row, dict):
            out.append(row)
    return out


def load(paths):
    meta = None
    reps = []
    for path in paths:
        for row in rows_of(path):
            if row.get("kind") == "meta":
                meta = meta or row
            elif row.get("kind") == "rep":
                reps.append(row)
    if not reps:
        raise SystemExit(f"no reps in {paths}")
    return meta, reps


def inputs_of(paths):
    """One entry per input file: what it calls itself, what it asked for, what it recorded."""
    out = []
    for path in paths:
        rows = rows_of(path)
        meta = next((r for r in rows if r.get("kind") == "meta"), {})
        label = meta.get("label")
        wanted = meta.get("gates")
        counts = {}
        for row in rows:
            if row.get("kind") == "rep" and row.get("gate") in GATES:
                counts[row["gate"]] = counts.get(row["gate"], 0) + 1
        out.append({
            "label": label if isinstance(label, str) and label else path,
            "requested": {g for g in wanted if g in GATES} if isinstance(wanted, list) else None,
            "counts": counts,
        })
    return out


def requested_by(inputs, gate):
    """Reps recorded under each label that asked for the gate, summed over inputs sharing a label.

    gates.py appends a timestamp to a label rather than uniquifying it, so one label can be
    two files. Keyed per file that would report one file's count as the label's; summing
    keeps the number equal to the reps those inputs actually recorded. Only inputs whose own
    meta asked for the gate are counted, so a file that merely shares the label is never
    named as a requester.
    """
    out = {}
    for i in inputs:
        if i["requested"] is None or gate not in i["requested"]:
            continue
        out[i["label"]] = out.get(i["label"], 0) + i["counts"].get(gate, 0)
    return out


def coverage_gaps(native_inputs, cowfs_inputs):
    """Every known gate with no comparison: the arm missing its data, and who asked for it."""
    out = []
    for gate in GATES:
        n = sum(i["counts"].get(gate, 0) for i in native_inputs)
        c = sum(i["counts"].get(gate, 0) for i in cowfs_inputs)
        if n and c:
            continue
        out.append({
            "gate": gate,
            "missing_in": "cowfs" if n else ("native" if c else "any input"),
            "reps": {"native": n, "cowfs": c},
            "requested_by": requested_by([*native_inputs, *cowfs_inputs], gate),
        })
    return out


def gap_reason(gap):
    gate, missing = gap["gate"], gap["missing_in"]
    where = "no data in any input" if missing == "any input" else f"the {missing} arm has no {gate} data"
    have = ", ".join(f"{arm} has {n} reps" for arm, n in gap["reps"].items() if n)
    asked = ", ".join(f"{label} requested it and recorded {n} reps"
                      for label, n in sorted(gap["requested_by"].items()))
    return f"{gate}  not compared: {where}" + (f", {have}" if have else "") + \
           f"; {asked or 'requested by no input'}"


def scope_text(compared):
    rest = [g for g in GATES if g not in compared]
    text = f"compared {len(compared)} of {len(GATES)} ({' '.join(compared) or 'none'})"
    return f"{text}, not compared {' '.join(rest)}" if rest else text


def is_int(v):
    return isinstance(v, int) and not isinstance(v, bool)


def is_num(v):
    try:
        return isinstance(v, (int, float)) and not isinstance(v, bool) and math.isfinite(v)
    except OverflowError:
        return False


def is_nonneg_load(v):
    """NaN (getloadavg failed) or a finite value >= 0; a number too large for a float is not a load."""
    if isinstance(v, bool) or not isinstance(v, (int, float)):
        return False
    try:
        return math.isnan(v) or (math.isfinite(v) and v >= 0)
    except OverflowError:
        return False


def meta_problem(meta):
    """Why a meta record cannot anchor g5 byte counts, or None."""
    counts = meta.get("counts")
    if not isinstance(counts, dict):
        return "meta has no counts"
    big = counts.get("big_bytes")
    unit = gates.UNIT["big_bytes"]
    if not is_int(big) or big < gates.MIN_BYTES or big % unit:
        return f"meta counts big_bytes {big!r} is not an integer multiple of {unit} that is at least {gates.MIN_BYTES}"
    if "scale" in meta:
        scale = meta["scale"]
        if not is_num(scale):
            return f"meta scale {scale!r} is not a finite number"
        try:
            want = gates.scaled_bytes("big_bytes", scale)
        except (OverflowError, ValueError):
            return f"meta scale {scale!r} implies no representable byte count"
        if big != want:
            return f"meta scale {scale} implies big_bytes {want}, meta says {big}"
    return None


def g5_problem(row, meta_bytes):
    """Why a g5 rep is not a valid read-back, or None. Integers decide; read_matches is never consulted."""
    m = row.get("metrics")
    if not isinstance(m, dict):
        return "no metrics"
    got = {k: m.get(k) for k in ("bytes", "written_bytes", "read_bytes")}
    for k, v in got.items():
        if not is_int(v):
            return f"{k} {v!r} is missing or not an integer"
    if not all(v == meta_bytes for v in got.values()):
        return f"expected {got['bytes']} written {got['written_bytes']} read {got['read_bytes']} are not all meta big_bytes {meta_bytes}"
    return None


def file_problems(path):
    """Every reason this result file cannot be trusted, plus its g5 rep count."""
    try:
        text = Path(path).read_text()
    except OSError as e:
        return [f"{path}: unreadable: {e}"], 0
    out = []
    meta_bytes = None
    metas = 0
    g5 = 0
    reps = 0
    for n, line in enumerate(text.splitlines(), 1):
        if not line.strip():
            continue
        try:
            row = json.loads(line)
        except ValueError:
            out.append(f"{path}:{n}: not JSON")
            continue
        if not isinstance(row, dict):
            out.append(f"{path}:{n}: not a JSON object")
            continue
        kind = row.get("kind")
        if kind == "meta":
            metas += 1
            if metas > 1:
                out.append(f"{path}:{n}: more than one meta record")
                continue
            why = meta_problem(row)
            if why:
                out.append(f"{path}:{n}: {why}")
            else:
                meta_bytes = row["counts"]["big_bytes"]
        elif kind == "rep":
            if metas == 0:
                out.append(f"{path}:{n}: rep before the meta record")
            reps += 1
            if row.get("gate") not in GATES:
                out.append(f"{path}:{n}: rep gate {row.get('gate')!r} is not one of {GATES}")
            if not is_int(row.get("rep")):
                out.append(f"{path}:{n}: rep index {row.get('rep')!r} is not an integer")
            if not is_num(row.get("wall_s")) or row["wall_s"] < 0:
                out.append(f"{path}:{n}: rep wall_s {row.get('wall_s')!r} is missing, not a finite number, or negative")
            for k in ("load1_before", "load1_after"):
                v = row.get(k)
                if not is_nonneg_load(v):
                    out.append(f"{path}:{n}: rep {k} {v!r} is missing, not a number, or out of range (NaN or finite >= 0)")
            if row.get("gate") == "g5":
                g5 += 1
                why = g5_problem(row, meta_bytes) if meta_bytes is not None else "no valid meta to check against"
                if why:
                    out.append(f"{path}:{n}: g5 rep {row.get('rep')}: {why}")
    if not metas:
        out.append(f"{path}: no meta record")
    if not reps:
        out.append(f"{path}: no rep records")
    return out, g5


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
    bad = []
    g5n = {}
    for path in [*args.native, args.cowfs, *([args.noise_floor] if args.noise_floor else [])]:
        why, n = file_problems(path)
        bad += why
        g5n[path] = n
    if any(g5n.values()):
        bad += [f"{p}: no g5 reps while other inputs have g5" for p, n in g5n.items() if not n]
    if not bad:
        native_gates = set().union(*[set(by_gate(load([p])[1])) for p in args.native])
        cowfs_gates = set(by_gate(load([args.cowfs])[1]))
        if not native_gates & cowfs_gates:
            bad.append("inputs: no gate is present in both the native and the cowfs arm, nothing to compare")
    if bad:
        for line in bad:
            print(f"INVALID {line}", file=sys.stderr)
        print("RESULT: invalid, g5 byte accounting is not verifiable", file=sys.stderr)
        return 3
    _, na = load(args.native)
    _, nb = load([args.cowfs])
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
        if math.isnan(la) or math.isnan(lb):
            # max() and min() are not symmetric on NaN, so test the arms rather than the peak:
            # max(quiet_finite, NaN) returns the quiet number and would launder the gap.
            print(f"UNMEASURABLE: no finite load1 was recorded (native {la:.1f}, "
                  f"cowfs {lb:.1f}), so the ceiling {LOAD_CEILING} cannot be checked")
            unmeasurable += 1
            continue
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

    native_inputs = inputs_of(args.native)
    cowfs_inputs = inputs_of([args.cowfs])
    gaps = coverage_gaps(native_inputs, cowfs_inputs)
    compared = [g for g in GATES if g not in {x["gate"] for x in gaps}]
    print()
    print(f"gate coverage  {len(compared)} of {len(GATES)} compared ({' '.join(compared) or 'none'})")
    for gap in gaps:
        if gap["reps"]["native"] or gap["reps"]["cowfs"] or gap["requested_by"]:
            print(f"  {gap_reason(gap)}")
    print("coverage " + json.dumps({"compared": compared, "gates_known": len(GATES), "not_compared": gaps}))

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
        thin = [g for g in GATES if g in ga and g not in gc]
        if thin:
            print(f"  noise floor has no {' '.join(thin)} data, which the native arm ran")
    else:
        print()
        print("noise floor: not measured. Pass --noise-floor, or use run-pair.sh, "
              "which runs the native arm twice around the cowfs arms.")

    print()
    if not any(g5n.values()):
        print("g5   not run (no input has g5 reps)")
    scope = scope_text(compared)
    if unmeasurable:
        print(f"RESULT: {unmeasurable} gate(s) unmeasurable, {fails} failed  scope: {scope}")
        return 2
    print(f"RESULT: {'PASS' if fails == 0 else f'FAIL ({fails})'}  scope: {scope}")
    return 1 if fails else 0


if __name__ == "__main__":
    raise SystemExit(main())
