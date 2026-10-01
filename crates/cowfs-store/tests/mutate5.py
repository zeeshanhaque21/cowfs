#!/usr/bin/env python3
"""Round-5 mutation driver. One textual mutation per recovery, lock and acknowledgement rule.

A mutant no test notices is a hole in the suite, so every case here must be killed.
Each mutant gets its own source tree and CARGO_TARGET_DIR, and a hard timeout: a timeout is
UNKNOWN, not killed, because nothing was proven about it.
"""
import json
import os
import shutil
import subprocess
import sys
import tarfile
import time

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__)))))
OUT = os.path.join(ROOT, "target", "mut5")
HARD_TIMEOUT = 900
SUITES = ["--features", "fault-injection"]
# The long crash and power-loss models are run once, at the end, not once per mutant: at 900 s a
# mutant of the whole store suite is indistinguishable from a timeout.
FAST = ["--test", "round5", "--test", "round4", "--test", "round3", "--test", "round2",
        "--test", "store", "--test", "durability", "--test", "integrity", "--test", "regress",
        "--test", "lock", "--test", "model", "--lib"]

# (label, file, old, new)
M = [
    ("D01 no high-water is raised after a roll or new_pack", "src/store.rs",
     "        wm.raise_next(next)?;\n        w.next_id = next;",
     "        w.next_id = next;"),
    ("D02 raise_next never fsyncs", "src/wm.rs",
     "        self.io.sync_file(&self.file, &self.path)?;\n        self.seq = seq;\n        self.next = next;",
     "        self.seq = seq;\n        self.next = next;"),
    ("D03 a checkpoint validates against any pack with the id", "src/store.rs",
     "&& nonces.get(id).is_some_and(|&w| w == nonce || w == 0)",
     "&& nonces.get(id).is_some()"),
    ("D05 the old pack is cut before the copy is durable", "src/store.rs",
     "            io.sync_file(&file, &path)?;\n            // The recovered records are durable in the new pack, so the old pack may now be cut.",
     "            if false { io.sync_file(&file, &path)?; }\n            // The recovered records are durable in the new pack, so the old pack may now be cut."),
    ("D06 pending-loss markers are never written", "src/store.rs",
     "        if !fresh.is_empty() {\n            ack::save(&io, &dir, fresh)?;",
     "        if false {\n            ack::save(&io, &dir, fresh)?;"),
    ("D09 close releases the lock before it flushes", "src/store.rs",
     "        let flushed = self.finish();\n        let released = self.release_lock();",
     "        let released = self.release_lock();\n        let flushed = self.finish();"),
    ("D10 drop releases the lock before the other handles", "src/store.rs",
     "    fn drop(&mut self) {\n        let _ = self.finish();\n    }",
     "    fn drop(&mut self) {\n        let _ = self.release_lock();\n        let _ = self.finish();\n    }"),
    ("D11 salvage trusts the header instead of the payload hash", "src/pack.rs",
     "let found = crate::record::verify(&header, &payload);",
     "let found = true;"),
    ("D12 an ack matches any nonce", "src/ack.rs",
     "        if !(e.nonce == nonce || e.nonce == 0)",
     "        if false"),
    ("D13 torn sidecars are never pruned", "src/store.rs",
     "    while all.len() > keep || (total > options.max_torn_sidecar_bytes && all.len() > 1) {",
     "    while false {"),
    ("D15 the tail is cut even when the sidecar cannot be written", "src/store.rs",
     "    let written = io\n        .write_at(&f, &path, 0, &buf)\n        .and_then(|()| io.sync_file(&f, &path));",
     "    let written = io.write_at(&f, &path, 0, &buf);"),
    ("D16 a rebuilt index is checkpointed as if it were trusted", "src/store.rs",
     "                rescan.insert(id);\n                last_damaged |= is_active;",
     "                last_damaged |= is_active;"),
    ("D17 a loss is treated as repaired without verifying elsewhere", "src/store.rs",
     "                    if !l.verified && !verify_at(&dir, x, l) {\n                        return false;\n                    }",
     "                    if false {\n                        return false;\n                    }"),
    ("D18 a missing synced pack is not recorded", "src/store.rs",
     "        for p in &recovery.missing_synced {\n            fresh.push(Entry {",
     "        for p in &[] as &[u32] {\n            fresh.push(Entry {"),
    # Round-5 rules.
    ("R01 the cut is unclassified but its marker is not saved", "src/store.rs",
     "                        ack::save(\n                            &io,\n                            &dir,\n                            vec![Entry {",
     "                        if false { ack::save(\n                            &io,\n                            &dir,\n                            vec![Entry {"),
    ("R02 a torn watermark slot does not stop the checkpoint being trusted", "src/store.rs",
     "            if packs_ok && entries_ok && !wm.uncertain() {",
     "            if packs_ok && entries_ok {"),
    ("R03 a pack above the mark is treated as an unpromised tail again", "src/store.rs",
     "                    if wm.uncertain() || known.contains(&id) {",
     "                    if false {"),
    ("R04 an acknowledged entry is written with a wildcard nonce", "src/store.rs",
     "                nonce: nonces.get(&c.pack).copied().unwrap_or(0),\n                state: ack::State::Acked,",
     "                nonce: 0,\n                state: ack::State::Acked,"),
    ("R05 a whole-pack ack is written even while the pack is on disk", "src/store.rs",
     "                .filter(|&&pack| !pack::pack_path(&self.dir, pack).exists())\n",
     ""),
    ("R06 the shutdown work runs twice, once without the lock", "src/store.rs",
     "        if self.finished.swap(true, Ordering::Relaxed) {\n            return Ok(());\n        }",
     "        if false {\n            return Ok(());\n        }"),
    ("R07 a record inside another record's payload is indexed", "src/pack.rs",
     "                if claimed.is_some_and(|c| cand > from && cand < c) {\n                    return Ok(Found::Nothing);\n                }",
     "                if false {\n                    return Ok(Found::Nothing);\n                }"),
    ("R08 an unreadable pack header is fatal again", "src/store.rs",
     "                    Err(_) => {\n                        // A torn or rotted header is not fatal: the records past it are still\n                        // scanned and the header is written again afterwards.\n                        head_lost.push(id);\n                    }",
     "                    Err(reason) => {\n                        return Err(Error::BadPack { path, reason });\n                    }"),
    ("R09 the lock refusal names no holder", "src/store.rs",
     "                    return Err(Error::Locked {\n                        dir,\n                        holder: lock_holder(&lock),\n                    });",
     "                    return Err(Error::Locked { dir, holder: None });"),
]


def fixture(tag):
    dest = os.path.join(OUT, tag, "source")
    if os.path.exists(os.path.join(OUT, tag, "source")):
        shutil.rmtree(dest)
    os.makedirs(dest)
    archive = subprocess.check_output(["git", "archive", "HEAD"], cwd=ROOT)
    with tarfile.open(fileobj=__import__("io").BytesIO(archive)) as src:
        src.extractall(dest, filter="data")
    return dest


def main():
    os.makedirs(OUT, exist_ok=True)
    only = [a for a in sys.argv[1:] if not a.startswith("-")]
    hard = next((int(a.split("=")[1]) for a in sys.argv[1:] if a.startswith("--timeout=")), HARD_TIMEOUT)
    report = os.path.join(OUT, "results.jsonl")
    for label, rel, old, new in M:
        tag = label.split()[0]
        if only and tag not in only:
            continue
        dest = fixture(tag)
        path = os.path.join(dest, "crates/cowfs-store", rel)
        src = open(path).read()
        row = {"mutant": label, "state": "invalid", "rc": None, "seconds": 0}
        if old not in src:
            row["state"] = "pattern-not-found"
            print(json.dumps(row), flush=True)
            with open(report, "a") as f:
                f.write(json.dumps(row) + "\n")
            continue
        open(path, "w").write(src.replace(old, new, 1))
        env = dict(os.environ, CARGO_TARGET_DIR=os.path.join(OUT, tag, "build"))
        cmd = ["cargo", "test", "-j4", "-p", "cowfs-store", *SUITES, *FAST]
        t0 = time.time()
        try:
            r = subprocess.run(cmd, cwd=dest, env=env, capture_output=True, text=True, timeout=hard)
            rc, out = r.returncode, r.stdout
        except subprocess.TimeoutExpired as e:
            rc, out = None, (e.stdout or b"").decode() if isinstance(e.stdout, bytes) else (e.stdout or "")
        row["seconds"] = round(time.time() - t0)
        row["rc"] = rc
        names = sorted({ln.split()[1] for ln in out.splitlines() if ln.startswith("test ") and "FAILED" in ln})
        if rc is None:
            row["state"] = "unknown-timeout"
        elif rc != 0:
            row["state"] = "killed" if names else "killed-build-or-panic"
            row["killed_by"] = names[:5]
        else:
            row["state"] = "survived"
        open(os.path.join(OUT, tag, "test.log"), "w").write(out)
        print(json.dumps(row), flush=True)
        with open(report, "a") as f:
            f.write(json.dumps(row) + "\n")
            f.flush()


if __name__ == "__main__":
    main()
