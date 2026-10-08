#!/usr/bin/env python3
"""Re-derive the document's tables from a run's own record.

    tabulate.py CASES_JSONL [--markdown]

Every figure the verification document quotes for a measured run comes from here, from the run's
cases.jsonl, rather than from anybody's memory of it. A pointer to the wrong run, or a table typed
from the terminal output of a different run, is exactly the class of defect this exists to remove.

    tabulate.py bench/out/ready-g4/repair-batch2/cases.jsonl --markdown
"""

import argparse
import json
import os
import sys


def load(path):
    rows = []
    with open(path) as f:
        for line in f:
            line = line.strip()
            if line:
                rows.append(json.loads(line))
    return rows


def collect(rows):
    meta = next((r for r in rows if r["kind"] == "meta"), {})
    verdict = next((r for r in reversed(rows) if r["kind"] == "verdict"), {})
    compares = [r for r in rows if r["kind"] == "compare"]
    restarts = [r for r in rows if r["kind"] == "restart"]
    cases = [r for r in rows if r["kind"] == "case"]
    return meta, verdict, compares, restarts, cases


def digest(value):
    return (value or "-")[:12]


def table(compares):
    out = ["| mode | seed | ops | native sha256 | cowfs sha256 | st_dev n/c | fstype n/c |"
           " stream | divergence | status |",
           "| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |"]
    for c in compares:
        dev, fst = c.get("data_st_dev", {}), c.get("data_fstype", {})
        div = c.get("first_stream_divergence")
        if div is None:
            where = "none" if c.get("ops_stream_match") else "not located"
        else:
            where = "%s: %s vs %s" % (div["index"], div["native"], div["cowfs"])
        out.append("| %s | %s | %s | %s | %s | %s | %s | %s | %s | %s |"
                   % (c["mode"], c["seed"], c.get("ops_requested"),
                      digest(c["data_sha256"].get("native")), digest(c["data_sha256"].get("cowfs")),
                      "%s/%s" % (dev.get("native"), dev.get("cowfs")),
                      "%s/%s" % (fst.get("native"), fst.get("cowfs")),
                      "same" if c.get("ops_stream_match") else "differs",
                      where, c.get("status")))
    return "\n".join(out)


def render(path, markdown):
    """The whole report as text, for a document to paste and for a test to read."""
    rows = load(path)
    meta, verdict, compares, restarts, cases = collect(rows)
    label = os.path.basename(os.path.dirname(path)) or os.path.basename(path)
    lines = []
    if markdown:
        lines.append("Evidence directory: `bench/out/ready-g4/%s`, from `%s`." % (label, path))
        lines.append("")
        lines.append("| | |")
        lines.append("| --- | --- |")
        lines.append("| status | %s |" % verdict.get("status"))
        lines.append("| exit code recorded in the run | %s |" % verdict.get("exit_code", "not recorded"))
        lines.append("| cases | %s |" % verdict.get("cases"))
        lines.append("| pairs | %s passed, %s failed, %s unmeasurable, %s invalid |"
              % (verdict.get("pairs_passed"), verdict.get("pairs_failed"),
                 verdict.get("pairs_unmeasurable"), verdict.get("pairs_invalid")))
        lines.append("| fsx binary sha256 | %s |" % meta.get("fsx", {}).get("sha256"))
        lines.append("| native root | %s |" % json.dumps(meta.get("roots", {}).get("native")))
        lines.append("| cowfs root | %s |" % json.dumps(meta.get("roots", {}).get("cowfs")))
        for row in restarts:
            lines.append("| restart leg | exit %s, generation %s to %s, %d file(s) read back, %d problem(s) |"
                  % (row.get("exit"),
                     (row.get("generation_before") or {}).get("pid"),
                     (row.get("generation_after") or {}).get("pid"),
                     len(row.get("rehashed") or {}), len(row.get("problems") or [])))
        bytes_ = verdict.get("bytes") or {}
        if bytes_:
            lines.append("| bytes written per arm | %s, against a per-arm total of %s |"
                  % (json.dumps(bytes_.get("written_per_arm")), bytes_.get("budget_per_arm")))
            lines.append("| accounting | %s |" % bytes_.get("accounting"))
            lines.append("| coverage | %s of %s cases per arm, partial %s |"
                  % ((bytes_.get("coverage") or {}).get("planned_cases_per_arm"),
                     (bytes_.get("coverage") or {}).get("declared_cases_per_arm"),
                     (bytes_.get("coverage") or {}).get("partial")))
        lines.append("")
        lines.append(table(compares))
        lines.append("")
        for heading, key in (("Failures", "failures"), ("Unmeasurable", "unmeasurable"),
                             ("Invalid", "invalid"), ("Capability gaps", "capability_gaps")):
            values = verdict.get(key) or []
            if values:
                lines.append("## %s" % heading)
                lines.append("")
                for value in values:
                    lines.append("- %s" % value)
                lines.append("")
        return "\n".join(lines) + "\n"

    return json.dumps({"evidence": path, "status": verdict.get("status"),
                       "exit_code": verdict.get("exit_code"), "cases": verdict.get("cases"),
                       "pairs": {k: verdict.get(k) for k in
                                 ("pairs_passed", "pairs_failed", "pairs_unmeasurable",
                                  "pairs_invalid")},
                       "fsx_sha256": meta.get("fsx", {}).get("sha256"),
                       "roots": meta.get("roots"), "bytes": verdict.get("bytes"),
                       "restarts": [{"exit": r.get("exit"),
                                     "pid": (r.get("generation_before") or {}).get("pid"),
                                     "pid_after": (r.get("generation_after") or {}).get("pid"),
                                     "rehashed": len(r.get("rehashed") or {}),
                                     "problems": r.get("problems")} for r in restarts],
                       "compares": [{"mode": c["mode"], "seed": c["seed"],
                                     "ops": c.get("ops_requested"), "status": c.get("status"),
                                     "stream_match": c.get("ops_stream_match"),
                                     "divergence": c.get("first_stream_divergence"),
                                     "native_sha256": digest(c["data_sha256"].get("native")),
                                     "cowfs_sha256": digest(c["data_sha256"].get("cowfs"))}
                                    for c in compares]}, indent=2, sort_keys=True)


def report(path, markdown):
    print(render(path, markdown))
    return 0


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__,
                                formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("cases_jsonl")
    p.add_argument("--markdown", action="store_true")
    args = p.parse_args(argv)
    if not os.path.exists(args.cases_jsonl):
        print("no such record: %s" % args.cases_jsonl, file=sys.stderr)
        return 2
    return report(args.cases_jsonl, args.markdown)


if __name__ == "__main__":
    sys.exit(main())
