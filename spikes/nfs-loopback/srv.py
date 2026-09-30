#!/usr/bin/env python3
"""Restart the loopback server and remount.

usage: srv.py [mount_opts_extra] [server args...]
Base opts: locallocks,vers=3,tcp,port=11111,mountport=11111 plus extra (default rsize=131072,actimeo=120).
"""
import os, subprocess, sys, time

HERE = os.path.dirname(os.path.abspath(__file__))
MNT = os.path.join(HERE, "out/mnt")
PIDF = os.path.join(HERE, "out/server.pid")


def mounted():
    return MNT in subprocess.run(["mount"], capture_output=True, text=True).stdout


def main():
    extra = sys.argv[1] if len(sys.argv) > 1 else "rsize=131072,actimeo=120"
    sargs = sys.argv[2:]
    if mounted():
        if subprocess.run(["umount", MNT], timeout=30).returncode != 0:
            subprocess.run(["umount", "-f", MNT], timeout=30)
    if mounted():
        sys.exit("still mounted")
    if os.path.exists(PIDF):
        pid = open(PIDF).read().strip()
        cmd = subprocess.run(["ps", "-p", pid, "-o", "command="], capture_output=True, text=True).stdout
        if "nfs-loopback --root" in cmd:
            os.kill(int(pid), 15)
            for _ in range(50):
                if subprocess.run(["ps", "-p", pid], capture_output=True).returncode != 0:
                    break
                time.sleep(0.1)
            else:
                sys.exit(f"server {pid} did not exit")
    log = open(os.path.join(HERE, "out/server.log"), "a")
    p = subprocess.Popen([os.environ.get("NFS_BIN", os.path.join(HERE, "target/release/nfs-loopback")), "--root", os.path.join(HERE, "out/backing"),
                          "--port", "11111", "--hide-appledouble", *sargs],
                         stdout=log, stderr=log, stdin=subprocess.DEVNULL, start_new_session=True)
    open(PIDF, "w").write(str(p.pid))
    time.sleep(1.5)
    if p.poll() is not None:
        sys.exit("server died")
    opts = "locallocks,vers=3,tcp,port=11111,mountport=11111," + extra
    subprocess.run(["mount_nfs", "-o", opts, "localhost:/", MNT], timeout=20, check=True)
    if not mounted():
        sys.exit("mount failed")
    print(f"pid {p.pid} opts {opts}")


if __name__ == "__main__":
    main()
