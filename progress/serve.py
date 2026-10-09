#!/usr/bin/env python3
"""Serves progress/index.html and a live status.json.

status.json is progress/plan.json with live data overlaid from the running
dedup spike (out/full/slots.jsonl, run.pid, report.json).
Usage: serve.py [port]   (binds 127.0.0.1)
"""
import json, os, subprocess, sys, threading, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
SPIKE_OUT = os.path.join(ROOT, "spikes", "dedup-corpus", "out", "full")
TREEHOUSE = os.path.expanduser("~/.treehouse")
GIB = 2**30
CLOSED = {"done"}

PROJECT = os.path.expanduser("~/Projects/context-mode")
_sizes = {}
_sizes_ready = False


def _du_kb(path):
    r = subprocess.run(["du", "-sk", path], capture_output=True, text=True)
    try:
        return int(r.stdout.split()[0])
    except (IndexError, ValueError):
        return 0


def _measure_sizes():
    global _sizes_ready
    _sizes["proj:" + os.path.basename(PROJECT) + "/-"] = _du_kb(PROJECT) * 1024
    for pool in sorted(os.listdir(TREEHOUSE)):
        p = os.path.join(TREEHOUSE, pool)
        if not os.path.isdir(p):
            continue
        for s in sorted(os.listdir(p)):
            if os.path.isdir(os.path.join(p, s)):
                _sizes[f"{pool}/{s}"] = _du_kb(os.path.join(p, s)) * 1024
    _sizes_ready = True


def total_slots():
    n = 1
    for pool in os.listdir(TREEHOUSE):
        p = os.path.join(TREEHOUSE, pool)
        if os.path.isdir(p):
            n += sum(1 for s in os.listdir(p) if os.path.isdir(os.path.join(p, s)))
    return n


def pid_alive(path):
    try:
        pid = int(open(path).read().strip())
        os.kill(pid, 0)
        return True
    except (OSError, ValueError):
        return False


def spike1_live(item):
    rows = []
    try:
        with open(os.path.join(SPIKE_OUT, "slots.jsonl")) as f:
            for line in f:
                line = line.strip()
                if line:
                    try:
                        rows.append(json.loads(line))
                    except json.JSONDecodeError:
                        pass
    except OSError:
        pass
    total = total_slots()
    done = len(rows)
    raw = sum(r["raw_bytes"] for r in rows)
    comp = sum(r["new_comp"] for r in rows)
    uraw = sum(r["new_raw"] for r in rows)
    pf = sum(r["perfile_comp_bytes"] for r in rows)
    secs = sum(r["secs"] for r in rows)
    errs = sum(r["read_errors"] + r["walk_errors"] for r in rows)
    changed = sum(r["changed_during_read"] for r in rows)
    alive = pid_alive(os.path.join(SPIKE_OUT, "run.pid"))
    finished = os.path.exists(os.path.join(SPIKE_OUT, "report.json"))
    if finished:
        item["state"], item["done"] = "done", True
    elif alive:
        item["state"] = "running"
    elif done:
        item["state"] = "failed"
        item["detail"] += "\nRun process is not alive and no report.json exists. Resume with --resume."
    if done:
        eta = ""
        frac = done / total
        if _sizes_ready:
            all_bytes = sum(_sizes.values())
            done_bytes = sum(_sizes.get(r["name"], 0) for r in rows)
            frac = done_bytes / max(all_bytes, 1)
            if alive and secs > 0 and done < total:
                eta = f", ETA about {int((all_bytes - done_bytes) / (done_bytes / secs) / 60)} min (approx, from du sizes)"
        else:
            eta = ", sizing slots for ETA"
        item["numbers"] = (
            f"{done}/{total} slots, {frac * 100:.0f}% of bytes, {raw / GIB:.1f} GiB raw, {errs} errors, {changed} changed mid-read{eta}\n"
            f"running totals (order-dependent until the run finishes): "
            f"per-file zstd {raw / max(pf, 1):.2f}x, CDC raw {raw / max(uraw, 1):.2f}x, CDC + zstd {raw / max(comp, 1):.2f}x"
        )
        item["progress"] = frac
    return item


def build_status():
    plan = json.load(open(os.path.join(HERE, "plan.json")))
    closed = slices = 0
    for ph in plan["phases"]:
        for it in ph["items"]:
            if it.get("live") == "spike1":
                spike1_live(it)
            if it.get("issue"):
                it["issue_url"] = f"https://github.com/zeeshanhaque21/cowfs/issues/{it['issue']}"
            it["done"] = it.get("done") or it["state"] in CLOSED
            slices += 1
            closed += it["done"]
    plan["totals"] = {"closed": closed, "slices": slices}
    plan["updated"] = time.strftime("%Y-%m-%d %H:%M:%S", time.localtime(os.path.getmtime(os.path.join(HERE, "plan.json"))))
    return plan


class H(BaseHTTPRequestHandler):
    def _send(self, code, ctype, body):
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Cache-Control", "no-store")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        path = self.path.split("?")[0]
        if path == "/status.json":
            try:
                self._send(200, "application/json", json.dumps(build_status()).encode())
            except Exception as e:
                self._send(500, "text/plain", str(e).encode())
        elif path in ("/", "/index.html"):
            self._send(200, "text/html; charset=utf-8", open(os.path.join(HERE, "index.html"), "rb").read())
        elif path in ("/graph", "/graph.html"):
            self._send(200, "text/html; charset=utf-8", open(os.path.join(HERE, "graph.html"), "rb").read())
        else:
            self._send(404, "text/plain", b"not found")

    def log_message(self, *a):
        pass


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8780
    ThreadingHTTPServer(("127.0.0.1", port), H).serve_forever()
