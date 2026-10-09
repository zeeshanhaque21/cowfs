#!/usr/bin/env python3
"""Regenerate bench/pjdfstest-accepted-divergences.json from a g3 run's cases.jsonl.

    bench/derive_pjdfstest_divergences.py <run-dir-or-cases.jsonl> [--out FILE]

Every ordinal-worse position must be classified by RULES below, or this exits 2 and names the
unclassified ones: a new worse position is a decision, never an automatic waiver.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import pjdfstest as p

PATHCONF = "pathconf returned -1"
# (issue, reason, match, test or None for any case, n or None for any position). First rule wins.
# A pathconf rule is case-scoped by design: every worse position in a case whose stderr shows the
# pathconf failure is the same macOS client divergence.
RULES = [
    ("#204", "open O_WRONLY,O_NONBLOCK on a fifo with no reader answers EACCES where native answers "
             "ENXIO; a real defect tracked in #204, listed so the ordinal position is on record while "
             "the established regression still fails the gate",
     r"expected ENXIO, got EACCES", "open/17.t", 2),
    ("#109", "open O_RDONLY then unlink then fstat reports nlink 1 where native reports 0: macOS NFS "
             "silly-rename keeps nlink 1 for an open unlinked file (#109 symptom 2)",
     r"fstat 0 nlink', expected 0, got 1", "unlink/14.t", 4),
    ("#108", "pathconf(_PC_PATH_MAX) returns -1 on the macOS NFS client, so the suite cannot build its "
             "long path; accepted macOS client divergence (Zee decision 2026-10-09)",
     PATHCONF, None, None),
]


def load_arms(path: Path) -> dict:
    jsonl = path / "cases.jsonl" if path.is_dir() else path
    arms: dict = {}
    for line in jsonl.read_text().splitlines():
        record = json.loads(line)
        arms.setdefault(record["arm"], {})[record["test"]] = record
    return arms


def derive(arms: dict, derived_from: str) -> dict:
    import re
    entries, unclassified = [], []
    for row in p.ordinal_rows(arms)[0]:
        text = row["cowfs_detail"] + "\n" + row["cowfs_stderr"]
        for issue, why, match, test, n in RULES:
            if test in (None, row["test"]) and n in (None, row["n"]) and re.search(match, text):
                entries.append({"test": row["test"], "n": row["n"], "issue": issue,
                                "reason": why, "match": match})
                break
        else:
            unclassified.append(f"{row['test']} #{row['n']}")
    if unclassified:
        raise SystemExit(f"unclassified ordinal-worse positions, add a rule or fix them: "
                         f"{', '.join(unclassified)}")
    return {"version": 2, "derived_from": derived_from, "entries": entries}


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("run")
    ap.add_argument("--out", type=Path, default=p.ACCEPTED_DIVERGENCES)
    args = ap.parse_args()
    doc = derive(load_arms(Path(args.run)), f"{args.run} via bench/derive_pjdfstest_divergences.py")
    args.out.write_text(json.dumps(doc, indent=1) + "\n")
    print(f"wrote {len(doc['entries'])} entries to {args.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
