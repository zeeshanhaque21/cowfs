#!/usr/bin/env python3
"""usage: srv.py start|stop  - private seeded-bench server on port 11112"""
import os, subprocess, sys, time

S = os.path.abspath(os.path.join(os.path.dirname(__file__), "../out/seeded"))
BIN, BACK, MNT, PIDF = f"{S}/nfs-loopback-bin", f"{S}/backing", f"{S}/mnt", f"{S}/server.pid"


def mounted():
    return MNT in subprocess.run(["mount"], capture_output=True, text=True).stdout


def start():
    os.makedirs(BACK, exist_ok=True)
    os.makedirs(MNT, exist_ok=True)
    log = open(f"{S}/server.log", "a")
    p = subprocess.Popen([BIN, "--root", BACK, "--port", "11112", "--hide-appledouble", *sys.argv[2:]],
                         stdout=log, stderr=log, stdin=subprocess.DEVNULL, start_new_session=True)
    open(PIDF, "w").write(str(p.pid))
    time.sleep(1.5)
    assert subprocess.run(["ps", "-p", str(p.pid)], capture_output=True).returncode == 0, "server died"
    subprocess.run(["mount_nfs", "-o", "locallocks,vers=3,tcp,rsize=131072,actimeo=120,port=11112,mountport=11112",
                    "localhost:/", MNT], timeout=20, check=True)
    assert mounted()
    print("up pid", p.pid)


def stop():
    if mounted() and subprocess.run(["umount", MNT], timeout=60).returncode != 0:
        subprocess.run(["umount", "-f", MNT], timeout=60)
    assert not mounted(), "still mounted"
    pid = open(PIDF).read().strip()
    cmd = subprocess.run(["ps", "-p", pid, "-o", "command="], capture_output=True, text=True).stdout
    if f"{S}/nfs-loopback-bin" in cmd:
        os.kill(int(pid), 15)
        print("killed", pid)
    else:
        print("pid not ours / gone", pid, repr(cmd))


{"start": start, "stop": stop}[sys.argv[1]]()
