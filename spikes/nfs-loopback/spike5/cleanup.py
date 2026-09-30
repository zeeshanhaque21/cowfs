#!/usr/bin/env python3
"""usage: cleanup.py SIDE - SIGKILL leftover fixture pids recorded for SIDE, after verifying the command line"""
import glob, json, os, subprocess, sys

S = os.path.abspath(os.path.join(os.path.dirname(__file__), "../out/spike5"))
for f in glob.glob(f"{S}/pids-{sys.argv[1]}-*.json"):
    for p in json.load(open(f)):
        cmd = subprocess.run(["ps", "-o", "stat=,command=", "-p", str(p)], capture_output=True, text=True).stdout.strip()
        if cmd and not cmd.startswith("Z") and ("sleep 600" in cmd or "fixtures.py" in cmd):
            os.kill(p, 9)
            print("killed", p, cmd[:80])
