#!/usr/bin/env python3
"""usage: settings.py <set> [n] - git status timing per config on each layout, interleaved; probes fsmonitor first"""
import json, os, subprocess, sys, statistics as st
from lib import *

S = sys.argv[1]
N = int(sys.argv[2]) if len(sys.argv) > 2 else 5
CFG = {
    "default": {},
    "preload_off": {"core.preloadindex": "false"},
    "preload_on": {"core.preloadindex": "true"},
    "untracked_cache": {"core.untrackedCache": "true"},
    "untracked+split": {"core.untrackedCache": "true", "core.splitIndex": "true"},
    "manyFiles": {"feature.manyFiles": "true"},
    "checkstat_min": {"core.checkStat": "minimal"},
}
RUNS = f"{OUT}/settings_{S}.jsonl"


def probe(repo):
    r = {}
    r["fsmonitor_supported"] = git(repo, "fsmonitor--daemon", "status", check=False).stderr.strip()[:200]
    return r


def apply(repo, cfg):
    for k in ("core.preloadindex", "core.untrackedCache", "core.splitIndex", "feature.manyFiles", "core.checkStat", "core.fsmonitor"):
        git(repo, "config", "--unset-all", k, check=False)
    for k, v in cfg.items():
        git(repo, "config", k, v)
    if cfg.get("core.untrackedCache"):
        git(repo, "update-index", "--untracked-cache")
        git(repo, "update-index", "--force-untracked-cache")
    else:
        git(repo, "update-index", "--no-untracked-cache", check=False)
    git(repo, "status", "--porcelain")
    git(repo, "status", "--porcelain")


print({L: probe(paths(S, L)["repo"]) for L in "NMO"})
with cpulock():
    for rep in range(N):
        for L in (("N", "M", "O")[rep % 3:] + ("N", "M", "O")[:rep % 3]):
            repo = paths(S, L)["repo"]
            for name, cfg in CFG.items():
                apply(repo, cfg)
                t, r = timed(repo, "status", "--porcelain")
                assert r.returncode == 0 and r.stdout == "", (L, name, r.stderr, r.stdout[:100])
                record(RUNS, {"layout": L, "cfg": name, "rep": rep, "sec": t, "load": load()})
    for L in "NMO":
        apply(paths(S, L)["repo"], {})
d = {}
for l in open(RUNS):
    r = json.loads(l)
    d.setdefault(r["cfg"], {}).setdefault(r["layout"], []).append(r["sec"])
res = {c: {L: round(st.median(v), 4) for L, v in ls.items()} for c, ls in d.items()}
for c, m in res.items():
    print(f"{c:18s}", m, "M/N", round(m["M"] / m["N"], 2), "O/N", round(m["O"] / m["N"], 2))
json.dump(res, open(f"{OUT}/settings_summary_{S}.json", "w"), indent=1)
