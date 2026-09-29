#!/usr/bin/env python3
"""usage: detach.py <logfile> <cmd...>  runs cmd detached, prints pid; writes '<logfile>.done rc' on exit."""
import subprocess, sys

log = sys.argv[1]
sh = " ".join(f"'{a}'" for a in sys.argv[2:]) + f" > '{log}' 2>&1; echo $? > '{log}.done'"
p = subprocess.Popen(["sh", "-c", sh], stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                     stderr=subprocess.DEVNULL, start_new_session=True)
print(p.pid)
