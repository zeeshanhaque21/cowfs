#!/usr/bin/env python3
"""usage: srv.py start|mount|umount|stop  - spike5 private server on port 11115"""
import os, subprocess, sys, time

S = os.path.abspath(os.path.join(os.path.dirname(__file__), "../out/spike5"))
BIN, BACK, MNT, PIDF = f"{S}/bin/nfs-loopback-tuned", f"{S}/backing", f"{S}/mnt", f"{S}/server.pid"
PORT = 11115


def mounted():
    return MNT in subprocess.run(["mount"], capture_output=True, text=True).stdout


def mount():
    subprocess.run(["mount_nfs", "-o", f"locallocks,vers=3,tcp,rsize=131072,actimeo=120,port={PORT},mountport={PORT}",
                    "localhost:/", MNT], timeout=20, check=True)
    assert mounted()


def umount():
    if mounted() and subprocess.run(["umount", MNT], timeout=60).returncode != 0:
        print("plain umount failed (busy?)")
        return False
    return not mounted()


def start():
    log = open(f"{S}/server.log", "a")
    p = subprocess.Popen([BIN, "--root", BACK, "--port", str(PORT), "--hide-appledouble"],
                         stdout=log, stderr=log, stdin=subprocess.DEVNULL, start_new_session=True)
    open(PIDF, "w").write(str(p.pid))
    time.sleep(1.5)
    assert subprocess.run(["ps", "-p", str(p.pid)], capture_output=True).returncode == 0, "server died"
    mount()
    print("up pid", p.pid)


def stop():
    if mounted() and subprocess.run(["umount", MNT], timeout=60).returncode != 0:
        subprocess.run(["umount", "-f", MNT], timeout=60)
    assert not mounted(), "still mounted"
    pid = open(PIDF).read().strip()
    cmd = subprocess.run(["ps", "-p", pid, "-o", "command="], capture_output=True, text=True).stdout
    if BIN in cmd:
        os.kill(int(pid), 15)
        print("killed", pid)
    else:
        print("pid not ours / gone", pid, repr(cmd))


if __name__ == "__main__":
    {"start": start, "mount": mount, "umount": lambda: print(umount()), "stop": stop}[sys.argv[1]]()
