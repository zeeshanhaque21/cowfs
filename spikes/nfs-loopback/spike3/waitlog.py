"""usage: waitlog.py <log> <pid> <max_minutes> <noprogress_minutes>; exits on ALL DONE, failure text, dead pid, or no progress"""
import os, sys, time
log, pid, mx, npg = sys.argv[1], int(sys.argv[2]), float(sys.argv[3]) * 60, float(sys.argv[4]) * 60
t0 = time.time()
def alive():
    try: os.kill(pid, 0); return True
    except OSError: return False
while True:
    txt = open(log, errors="replace").read()
    if "WRAPPER EXIT rc=0" in txt: print("DONE"); break
    bad = [l for l in txt.splitlines() if any(k in l for k in ("FAILED rc", "EXCEEDED", "Traceback", "ok=False", "SERVER DIED", "WRAPPER EXIT rc=2"))]
    if bad: print("FAILED:", bad[-1]); break
    if not alive(): print("PROCESS EXITED without ALL DONE"); break
    if time.time() - os.path.getmtime(log) > npg: print("NO PROGRESS"); break
    if time.time() - t0 > mx: print("STILL RUNNING (max wait reached)"); break
    time.sleep(10)
print("\n".join(txt.splitlines()[-int(os.environ.get("TAIL", "12")):]))
