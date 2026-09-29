#!/usr/bin/env python3
"""usage: fixtures.py KIND SLOT PIDFILE - daemonize a lingering process shaped like KIND, write its pids (JSON list) to PIDFILE"""
import fcntl, json, os, signal, sys, time

kind, slot, pidfile = sys.argv[1:4]


def sleep_forever():
    while True:
        time.sleep(3600)


def body():
    pids = [os.getpid()]
    if kind == "cwd_root":
        os.chdir(slot)
        json.dump(pids, open(pidfile, "w"))
        os.execvp("sleep", ["sleep", "600"])
    if kind == "cwd_nested":
        os.chdir(f"{slot}/src/deep/er")
        json.dump(pids, open(pidfile, "w"))
        os.execvp("sleep", ["sleep", "600"])
    if kind == "fd_out":
        f = open(f"{slot}/held.txt")
        os.chdir("/")
    elif kind == "flock_out":
        f = open(f"{slot}/lockfile", "w")
        fcntl.flock(f, fcntl.LOCK_EX)
        os.chdir("/")
    elif kind == "trap":
        os.chdir(slot)
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
    elif kind in ("setsid_child", "setsid_child_out"):
        os.chdir(slot)
        c = os.fork()
        if c == 0:
            os.setsid()
            if kind == "setsid_child_out":
                os.chdir("/")
            sleep_forever()
        pids.append(c)
    json.dump(pids, open(pidfile, "w"))
    sleep_forever()


if os.fork():
    os._exit(0)
os.setsid()
if os.fork():
    os._exit(0)
fd = os.open(os.devnull, os.O_RDWR)
for n in (0, 1, 2):
    os.dup2(fd, n)
body()
