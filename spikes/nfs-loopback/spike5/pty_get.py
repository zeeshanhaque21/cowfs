#!/usr/bin/env python3
"""usage: pty_get.py SIDE - interactive `treehouse get`, leave a background sleep 613 in the slot, exit the subshell; treehouse must kill it and release the slot"""
import json, os, pty, select, subprocess, sys, time

S = os.path.abspath(os.path.join(os.path.dirname(__file__), "../out/spike5"))
side = sys.argv[1]
R = {"native": f"{S}/native/pool", "mount": f"{S}/mnt/pool"}[side]
env = {**os.environ, "HOME": f"{S}/home", "TREEHOUSE_ROOT": R, "SHELL": "/bin/sh", "PS1": "$ "}
pid, fd = pty.fork()
if pid == 0:
    os.chdir(f"{S}/repo")
    os.execvpe("treehouse", ["treehouse", "get", "--no-fetch", "--root", R], env)
buf = b""


def pump(sec):
    global buf
    end = time.time() + sec
    while time.time() < end:
        if select.select([fd], [], [], 0.3)[0]:
            try:
                d = os.read(fd, 65536)
            except OSError:
                return False
            if not d:
                return False
            buf += d
    return True


pump(6)
os.write(fd, b"pwd -P; sleep 613 &\n")
pump(2)
before = [l for l in subprocess.run(["ps", "-axo", "pid=,ppid=,command="], capture_output=True, text=True).stdout.splitlines() if l.split(None, 2)[-1] == "sleep 613"]
os.write(fd, b"exit\n")
t = time.time()
while pump(1) and time.time() - t < 30:
    pass
_, status = os.waitpid(pid, 0)
text = buf.decode(errors="replace")
st = subprocess.run(["treehouse", "status", "--root", R], cwd=f"{S}/repo", env=env, capture_output=True, text=True).stdout
left = [l for l in subprocess.run(["ps", "-axo", "pid=,ppid=,command="], capture_output=True, text=True).stdout.splitlines() if l.split(None, 2)[-1] == "sleep 613"]
res = dict(side=side, sleep613_before=before, exit=os.waitstatus_to_exitcode(status), transcript=text, status_after=st, sleep600_after=left)
json.dump(res, open(f"{S}/pty_get-{side}.json", "w"), indent=1)
print(json.dumps(res, indent=1))
