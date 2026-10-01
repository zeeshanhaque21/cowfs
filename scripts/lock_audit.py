#!/usr/bin/env python3
"""Regenerate the lock-audit table in docs/v1-core.md from the source.

`tests/critic2b.rs::every_lock_site_is_in_the_audit_table` fails if a function that takes a lock is
missing from the table, so the table cannot rot. Run this after touching a lock site:

    python3 scripts/lock_audit.py            # print the table
    python3 scripts/lock_audit.py --write    # rewrite the table in docs/v1-core.md
"""
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SRC = ROOT / "crates/cowfs-core/src"
DOC = ROOT / "docs/v1-core.md"
HEADER = "| Site | Locks held together | Order |"
MARKER = "| `Inner::flush_snapshot`, `Inner::barrier` |"

# What each lock means, for the "order" column.
ORDER = {
    "sc.ns": "1",
    "sc.flush": "1",
    "sc.q": "1",
    "st.wr": "2",
    "st.rd": "2",
    "st.try_read": "2",
    "nodes": "2",
    "dents": "2",
    "aliases": "leaf",
    "handles": "leaf",
    "root_time": "leaf",
    "pressure": "leaf",
    "unsynced": "leaf",
    "last_error": "leaf",
    "virt_lock": "leaf",
    "gens": "leaf",
    "snap.": "3",
    "blocks.get": "3",
    "blocks.put": "3",
}
GROUPS = [
    (re.compile(r"\bsc\.ns\b"), "sc.ns"),
    (re.compile(r"\bsc\.flush\b"), "sc.flush"),
    (re.compile(r"\bsc\.q\b|\bq\.lk\(\)|\.q\.lk\(\)"), "sc.q"),
    (re.compile(r"\bst\.wr\(\)|\.file\.as_mut\(\)|nlink = |attr\.\w+ = "), "st.wr"),
    (re.compile(r"\bst\.rd\(\)|\.st\.rd\(\)"), "st.rd"),
    (re.compile(r"\bst\.try_read\(\)|\.st\.try_read\(\)"), "st.try_read"),
    (re.compile(r"\bnodes\b"), "nodes"),
    (re.compile(r"\bdents\b"), "dents"),
    (re.compile(r"\baliases\b"), "aliases"),
    (re.compile(r"\bhandles\b"), "handles"),
    (re.compile(r"\broot_time\b"), "root_time"),
    (re.compile(r"\bpressure\b"), "pressure"),
    (re.compile(r"\bunsynced\b"), "unsynced"),
    (re.compile(r"\blast_error\b"), "last_error"),
    (re.compile(r"\bvirt_lock\b"), "virt_lock"),
    (re.compile(r"\bgens\b"), "gens"),
    (re.compile(r"\bsnap\."), "snap."),
    (re.compile(r"\bblocks\.get\b|\bblocks\.put\b"), "blocks"),
]
TAKES = re.compile(r"\.rd\(\)|\.wr\(\)|\.lk\(\)|\.try_read\(\)|\.try_write\(\)|\.try_lock\(\)|snap\.|blocks\.get|blocks\.put")


def fns(src: str):
    out = []
    for m in re.finditer(r"\bfn (\w+)", src):
        start = src.index("{", m.end())
        depth, i, started = 0, start, False
        while i < len(src):
            c = src[i]
            if c == "{":
                depth += 1
                started = True
            elif c == "}":
                depth -= 1
                if started and depth == 0:
                    break
            i += 1
        out.append((m.group(1), src[m.end() : i]))
    return out


def describe(body: str) -> str:
    kinds = []
    for rx, name in GROUPS:
        if rx.search(body) and name not in kinds:
            kinds.append(name)
    if not kinds:
        return "leaf | 1"
    order = []
    for k in kinds:
        o = ORDER.get(k, "?")
        if o not in order:
            order.append(o)
    return ", ".join(kinds) + " | " + " then ".join(sorted(set(order), key=lambda x: (x == "?", x)))


def table() -> str:
    rows = []
    for path in sorted(SRC.glob("*.rs")):
        src = path.read_text()
        for name, body in fns(src):
            if not TAKES.search(body):
                continue
            locks, order = describe(body).rsplit(" | ", 1)
            rows.append(f"| `{path.stem}::{name}` | {locks} | {order} |")
    return "\n".join([HEADER, "|---|---|---|", *rows])


if __name__ == "__main__":
    t = table()
    if "--write" in sys.argv:
        doc = DOC.read_text()
        head = doc[: doc.index(MARKER)]
        rest = doc[doc.index(MARKER) :]
        # keep the prose after the table (the paragraph starting with a blank line and text)
        lines = rest.split("\n")
        # lines[0] is the first old row; drop it plus every other row, and the old header
        tail = [l for l in lines[1:] if not l.startswith(("| `", "| Site |", "|---|---|---|"))]
        tail = "\n".join(tail).lstrip("\n")
        tail = tail[tail.index("\n\n") :] if "\n\n" in tail else tail
        DOC.write_text(head + t + "\n\n" + tail.lstrip("\n"))
        print(f"wrote {len(t.splitlines()) - 2} rows")
    else:
        print(t)