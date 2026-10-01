#!/usr/bin/env python3
"""Mutation driver for the round-3 code. Each mutant is one source edit; a mutant that no test
notices is a hole in the suite. Run: python3 crates/cowfs-store/tests/mutate3.py [TAG ...]
"""
import os
import shutil
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__)))))
WORK = os.path.join(ROOT, "target", "mut3", "work")
TARGET = os.path.join(ROOT, "target", "mut3", "tgt")
SUITES = ["--test", "round3", "--test", "round2", "--test", "store", "--test", "durability"]

M = [
    ("N01 pack ids are reused after an acknowledgement",
     "src/store.rs",
     "            .max(acked.iter().map(|e| e.pack).max().map_or(0, |p| p + 1));",
     "            .max(0);"),
    ("N02 a stale checkpoint may validate against a different pack",
     "src/store.rs",
     "&& nonces.get(id).is_some_and(|&w| w == nonce || w == 0)",
     "&& nonces.get(id).is_some()"),
    ("N03 records move down in reverse order",
     "src/store.rs",
     "            for r in &relocated {",
     "            for r in relocated.iter().rev() {"),
    ("N04 every torn tail overwrites the first sidecar",
     "src/store.rs",
     "let n = (0u32..).find(|n| !torn_path(dir, pack, *n).exists()).unwrap();",
     "let n = 0u32;"),
    ("N05 the work bound refuses a candidate that exactly fits",
     "src/pack.rs",
     "    if *budget < u64::from(slen) {",
     "    if *budget <= u64::from(slen) {"),
    ("N06 a repairing put leaves the block in the damaged list",
     "src/store.rs",
     "                .remove(&id);",
     "                .get(&id).map(|_| ());"),
    ("N07 an acknowledgement does not sync first",
     "src/store.rs",
     "        self.sync()?;\n        let n = entries.len();",
     "        let n = entries.len();"),
    ("N08 a pack with a lost header is refused again",
     "src/store.rs",
     "                        head_lost.push(id);",
     "                        return Err(Error::BadPack { path, reason });"),
    ("N09 salvage trusts the header instead of the payload hash",
     "src/pack.rs",
     "                let found = crate::record::verify(&header, &payload);",
     "                let found = true;"),
    ("N10 sidecars are never pruned",
     "src/store.rs",
     "    while all.len() > keep || (total > options.max_torn_sidecar_bytes && all.len() > 1) {",
     "    while false {"),
    ("N11 a cut label is ignored, so a cut is read as corruption",
     "src/store.rs",
     "            Some(_) => sealed_len.get(&id).copied().unwrap_or(0),",
     "            Some(_) => 0,"),
    ("N12 an unclassifiable cut is not remembered",
     "src/store.rs",
     "                if recovery.watermark_missing {",
     "                if false {"),
    ("N13 an acknowledged region is still reported",
     "src/ack.rs",
     "    entries.iter().rev().any(|e| {\n        e.pack == pack\n            && e.state.accepted()",
     "    entries.iter().rev().any(|e| {\n        e.pack == pack\n            && true"),
    ("N14 salvage does not clear the rescan set, so its records are never checkpointed",
     "src/store.rs",
     "            self.rescan\n                .lock()\n                .unwrap_or_else(PoisonError::into_inner)\n                .clear();",
     ""),
]


def main():
    only = sys.argv[1:]
    os.makedirs(WORK, exist_ok=True)
    subprocess.run(
        ["rsync", "-a", "--delete", "--exclude", "target", "--exclude", ".git",
         os.path.join(ROOT, "crates") + "/", WORK + "/crates/"],
        check=True)
    subprocess.run(["cp", os.path.join(ROOT, "Cargo.toml"), WORK + "/Cargo.toml"], check=True)
    for extra in ("Cargo.lock", "rust-toolchain.toml"):
        src = os.path.join(ROOT, extra)
        if os.path.exists(src):
            subprocess.run(["cp", src, os.path.join(WORK, extra)], check=True)
    out = open(os.path.join(ROOT, "target", "mut3", "results.txt"), "a")
    for name, rel, a, b in M:
        tag = name.split()[0]
        if only and tag not in only:
            continue
        p = os.path.join(WORK, "crates/cowfs-store", rel)
        src = open(p).read()
        if a not in src:
            print(f"{name}: PATTERN NOT FOUND", flush=True)
            out.write(f"{name}: PATTERN NOT FOUND\n")
            continue
        open(p, "w").write(src.replace(a, b, 1))
        env = dict(os.environ, CARGO_TARGET_DIR=TARGET)
        r = subprocess.run(["cargo", "test", "-j4", "-p", "cowfs-store", *SUITES],
                           cwd=WORK, env=env, capture_output=True, text=True)
        killed = r.returncode != 0
        names = []
        for line in r.stdout.splitlines():
            if line.startswith("test ") and ("FAILED" in line or "failed" in line):
                names.append(line.split()[1])
        verdict = "KILLED by " + ", ".join(sorted(set(names))[:4]) if killed else "SURVIVED"
        if killed and not names:
            verdict = "KILLED (build or panic)"
        print(f"{name}: {verdict}", flush=True)
        out.write(f"{name}: {verdict}\n")
        open(p, "w").write(src)
    out.close()


if __name__ == "__main__":
    main()