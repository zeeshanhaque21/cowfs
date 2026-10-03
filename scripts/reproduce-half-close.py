import argparse
import json
import os
from pathlib import Path
import subprocess
import time

parser = argparse.ArgumentParser()
parser.add_argument("--runs", type=int, default=200)
parser.add_argument("--seconds", type=float, default=180)
parser.add_argument("--output", type=Path, required=True)
parser.add_argument("--binary", type=Path)
args = parser.parse_args()
args.output.parent.mkdir(parents=True, exist_ok=True)
started = time.monotonic()
with args.output.open("x", buffering=1) as output:
    for run in range(args.runs):
        if time.monotonic() - started >= args.seconds:
            print(f"time bound reached after {run} passing runs", flush=True)
            break
        command = ([str(args.binary),
                    "m1_half_close_means_no_more_requests_not_cancel", "--exact"]
                   if args.binary else
                   ["cargo", "test", "--locked", "-p", "cowfs-ctl", "--test", "regress",
                    "m1_half_close_means_no_more_requests_not_cancel", "--", "--exact"])
        result = subprocess.run(
            ["rtk", "proxy", *command], capture_output=True, text=True, timeout=30)
        record = {"run": run + 1, "exit": result.returncode,
                  "stdout": result.stdout, "stderr": result.stderr}
        output.write(json.dumps(record) + "\n")
        output.flush()
        os.fsync(output.fileno())
        if result.returncode:
            print(json.dumps(record), flush=True)
            raise SystemExit(1)
    else:
        print(f"{args.runs} runs passed; failure not reproduced", flush=True)
