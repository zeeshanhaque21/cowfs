#!/usr/bin/env python3
"""Regenerate the lock-audit table in docs/v1-core.md from the source.

CI runs `--check` (the lint job), so the table cannot rot when the source changes. Run this after
touching a lock site:

    python3 scripts/lock_audit.py            # print the table
    python3 scripts/lock_audit.py --write    # rewrite the table in docs/v1-core.md
    python3 scripts/lock_audit.py --check    # exit 1 if the table in the doc is not the generated one
"""
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SRC = ROOT / "crates/cowfs-core/src"
DOC = ROOT / "docs/v1-core.md"
HEADER = "| Site | Locks held together | Order |"
# the generated table: header, separator, then rows, until a blank line
HEADER_MARK = "| Site | Locks held together | Order |"

# What each lock means, for the "order" column.
ORDER = {
    "target": "0",
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
    (re.compile(r"\block_target\b|\bswap_targets\b"), "target"),
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
TAKES = re.compile(r"\block_target\(|\.rd\(\)|\.wr\(\)|\.lk\(\)|\.try_read\(\)|\.try_write\(\)|\.try_lock\(\)|snap\.|blocks\.get|blocks\.put")


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
            row = f"| `{path.stem}::{name}` | {locks} | {order} |"
            if row not in rows:
                rows.append(row)
    return "\n".join([HEADER, "|---|---|---|", *rows])


if __name__ == "__main__":
    t = table()
    if "--write" in sys.argv:
        lines = DOC.read_text().split("\n")
        i = lines.index(HEADER_MARK)
        j = i + 2
        while j < len(lines) and lines[j].startswith("|"):
            j += 1
        DOC.write_text("\n".join(lines[:i] + t.split("\n") + lines[j:]))
        print(f"wrote {len(t.splitlines()) - 2} rows")
    elif "--check" in sys.argv:
        lines = DOC.read_text().split("\n")
        i = lines.index(HEADER_MARK)
        j = i + 2
        while j < len(lines) and lines[j].startswith("|"):
            j += 1
        if "\n".join(lines[i:j]) != t:
            sys.exit("docs/v1-core.md lock-audit table is stale: run python3 scripts/lock_audit.py --write")
        print(f"lock-audit table is current ({len(t.splitlines()) - 2} rows)")
    else:
        print(t)