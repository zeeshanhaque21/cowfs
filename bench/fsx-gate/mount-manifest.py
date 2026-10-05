#!/usr/bin/env python3
"""Attest what a mount actually is, and who is serving it.

    mount-manifest.py attest MOUNTPOINT [--pid PIDFILE] [--expect-backend core]

Prints one JSON object and exits 0 when the mount is the kind of mount the caller declared,
1 when it is not, and 2 when the answer cannot be known.

    {"ok": true, "mountpoint": ..., "fstype": "fuse.cowfs", "source": "cowfs",
     "device": ..., "st_dev": 234, "daemon": {...} | null}

The point is that nothing here trusts a path. The mount table is read from the kernel, the
filesystem type comes from that entry, and the device is the one `stat` reports for the
directory. A caller that passes the wrong directory gets told so here rather than discovered
later, or not at all.

There is deliberately no fallback. If the mount table cannot be read, this exits 2 with
UNKNOWN. Guessing from a path string is how a tmpfs can be mistaken for the filesystem under
test, and a symlink into a dead tree is not a mount at all.

The daemon fields are attestations about a process, not proof that it serves this mount. They
say who is running, from which binary, with which store and socket, and they are read from
/proc and the pid file rather than from anything the caller supplied.
"""

import ctypes
import ctypes.util
import errno
import json
import os
import platform
import sys

# The filesystem types cowfs can serve, per platform. Linux mounts FUSE and the FUSE adapter
# mounts as fuse.cowfs (crates/cowfs-daemon/src/mounts.rs); macOS serves the NFS loopback.
COWFS_FSTYPES = ("fuse.cowfs", "cowfs", "nfs")

EXIT_OK, EXIT_WRONG_FS, EXIT_UNKNOWN = 0, 1, 2


def read_mount_table():
    """(entries, None) or (None, reason). /proc/mounts on Linux, mount(8) elsewhere."""
    if platform.system() == "Linux":
        try:
            with open("/proc/mounts") as f:
                rows = []
                for line in f:
                    fields = line.split()
                    if len(fields) < 3:
                        continue
                    rows.append({"source": fields[0], "mountpoint": fields[1],
                                 "fstype": fields[2], "raw": line.strip()})
                return rows, None
        except OSError as e:
            return None, "%s: %s" % (e.filename, e.strerror)
        return None, "unreadable"
    try:
        import subprocess
        out = subprocess.run(["mount"], capture_output=True, text=True, timeout=30).stdout
    except Exception as e:  # noqa: BLE001 - any failure here is UNKNOWN, not a guess
        return None, "%s: %s" % (type(e).__name__, e)
    rows = []
    for line in out.splitlines():
        fields = line.split()
        if len(fields) < 4 or fields[1] != "on":
            continue
        opts = fields[3]
        fstype = opts.strip("()").split(",")[0] if opts.startswith("(") else opts
        rows.append({"source": fields[0], "mountpoint": fields[2], "fstype": fstype,
                     "raw": line.strip()})
    return rows, None


def resolve_mount(path, rows):
    """The longest mount entry containing path, or None. No realpath fallback.

    realpath is not applied to the mountpoint comparison: a symlink into a mount is a legitimate
    way to name the same filesystem, and the kernel's own table is the authority on where
    mounts begin.
    """
    clean = os.path.normpath(path)
    best = None
    for row in rows:
        mnt = os.path.normpath(row["mountpoint"])
        if clean == mnt or clean.startswith(mnt.rstrip("/") + "/"):
            if best is None or len(mnt) > len(os.path.normpath(best["mountpoint"])):
                best = row
    return best


def device_of(path):
    try:
        return os.stat(path).st_dev
    except OSError as e:
        return None if e.errno == errno.ENOENT else "errno %d" % e.errno


# Linux filesystem magic numbers, for the statfs field that is filled everywhere. f_fstypename
# is the field glibc documents but this kernel fills empty for every filesystem, which the first
# run under the repaired gate found: it reported no filesystem witness at all.
FSTYPE_MAGIC = {
    0xEF53: "ext4", 0xEF15: "ext2", 0xEF16: "ext3", 0x9123683E: "btrfs",
    0x01021994: "tmpfs", 0x58465342: "xfs", 0x6969: "nfs", 0x65735546: "fuse",
    0xF15F: "ecryptfs", 0x794C7630: "overlayfs",
    0x01021997: "hugetlbfs", 0x64626720: "debugfs", 0x1373: "ext2",
    0x2FC12FC1: "zfs", 0xBEEFDEAD: "nilfs2", 0x5346544E: "ntfs",
}


class _Statfs(ctypes.Structure):
    """struct statfs with the fields this gate needs. f_type is the first one and is reliable."""

    _fields_ = [
        ("f_type", ctypes.c_long),
        ("f_bsize", ctypes.c_long),
        ("f_blocks", ctypes.c_long),
        ("f_bfree", ctypes.c_long),
        ("f_bavail", ctypes.c_long),
        ("f_files", ctypes.c_long),
        ("f_ffree", ctypes.c_long),
        ("f_fsid", ctypes.c_int * 2),
        ("f_namelen", ctypes.c_long),
        ("f_frsize", ctypes.c_long),
        ("f_flags", ctypes.c_long),
        ("f_spare", ctypes.c_int * 4),
        ("f_fstypename", ctypes.c_char * 16),
    ]


def statfs_magic(path):
    """(magic, name) from statfs, or (None, None). The name comes from the magic table."""
    if platform.system() != "Linux":
        return None, None
    try:
        libc = ctypes.CDLL(ctypes.util.find_library("c") or "libc.so.6", use_errno=True)
        buf = _Statfs()
        if libc.statfs(ctypes.c_char_p(path.encode()), ctypes.byref(buf)) != 0:
            return None, None
        magic = buf.f_type & 0xFFFFFFFF
        name = buf.f_fstypename.decode("ascii", "replace").split("\0", 1)[0]
        return magic, (name or FSTYPE_MAGIC.get(magic))
    except Exception:  # noqa: BLE001 - an unavailable libc is UNKNOWN, not a guess
        return None, None


def statfs_fstype(path):
    """The filesystem type the kernel reports for a path, or None when it cannot be asked.

    None means unknown, never "the same as the other arm": callers treat an absent witness as a
    refusal.
    """
    return statfs_magic(path)[1]


def mountinfo_for_device(dev):
    """The mount table entry for a device number, which is what /proc/self/mountinfo records.

    This is the fallback when statfs gives no name: the kernel's own table, keyed on the device,
    with the filesystem type from the same line.
    """
    if dev is None or not isinstance(dev, int):
        return None
    want = "%d:%d" % (os.major(dev), os.minor(dev))
    try:
        with open("/proc/self/mountinfo") as f:
            for line in f:
                fields = line.split()
                # fields: id parent maj:min root mountpoint options... - fstype source super
                if len(fields) < 5 or fields[2] != want:
                    continue
                sep = fields.index("-") if "-" in fields else None
                if sep is None:
                    continue
                return {"mountpoint": fields[4], "fstype": fields[sep + 1],
                        "source": fields[sep + 2], "device": want,
                        "raw": line.strip()}
    except (OSError, ValueError):
        return None
    return None


def file_witness(path):
    """Everything the kernel says about one file's own bytes: where it really is, which device,
    and which filesystem that device is. None for any of it means the arm is not identified."""
    real = os.path.realpath(path)
    witness = {"path": path, "realpath": real, "st_dev": device_of(real),
               "exists": os.path.exists(real)}
    if witness["st_dev"] is None:
        return witness
    magic, name = statfs_magic(real)
    witness["statfs_magic"] = "0x%x" % magic if magic is not None else None
    witness["statfs_fstype"] = name
    entry = mountinfo_for_device(witness["st_dev"])
    witness["mountpoint"] = entry["mountpoint"] if entry else None
    # The mount table's type wins: statfs says "fuse" for every FUSE filesystem, so it cannot
    # tell cowfs from any other.
    witness["fstype"] = (entry["fstype"] if entry else name)
    witness["fstype_source"] = "mount table" if entry else ("statfs" if name else "unknown")
    witness["mount_source"] = entry["source"] if entry else None
    witness["mount_raw"] = entry["raw"] if entry else None
    return witness


def proc_starttime(pid):
    """Field 22 of /proc/PID/stat, the process start time in clock ticks.

    pid alone is not a generation: a pid is reused. pid and start time together identify one
    running generation of one binary, which is what a restart has to change.
    """
    try:
        with open("/proc/%d/stat" % pid) as f:
            data = f.read()
    except OSError:
        return None
    # The comm field can contain spaces and parentheses, so fields are counted after it.
    tail = data[data.rfind(")") + 2:].split()
    return tail[19] if len(tail) > 19 else None


def attest_daemon(pidfile):
    """Who is running, from /proc and the pid file. Never from anything the caller passed in."""
    if not pidfile:
        return None
    try:
        with open(pidfile) as f:
            pid = int(f.read().strip())
    except (OSError, ValueError) as e:
        return {"error": "pid file %s: %s" % (pidfile, e)}
    info = {"pid": pid, "pid_file": pidfile}
    try:
        # Read once: a second read on the same handle would see EOF and lose every argument.
        with open("/proc/%d/cmdline" % pid, "rb") as f:
            raw = f.read()
    except OSError as e:
        info["error"] = "%s: %s" % (e.filename, e.strerror)
        return info
    argv_full = [a.decode("utf-8", "replace") for a in raw.split(b"\0") if a]
    if not argv_full:
        info["error"] = "/proc/%d/cmdline is empty" % pid
        return info
    info["argv"] = argv_full[0]
    info["argv_full"] = argv_full
    info["starttime"] = proc_starttime(pid)
    opts = {}
    for i, arg in enumerate(info.get("argv_full", [])):
        if arg.startswith("--") and i + 1 < len(info["argv_full"]):
            opts[arg[2:]] = info["argv_full"][i + 1]
    info["store"] = opts.get("store")
    info["mount"] = opts.get("mount")
    info["socket"] = opts.get("socket")
    info["backend"] = opts.get("backend")
    info["binary_sha256"] = file_digest(info["argv"]) if info["argv"] else None
    return info


def file_digest(path):
    import hashlib
    try:
        h = hashlib.sha256()
        with open(path, "rb") as f:
            while True:
                block = f.read(1 << 20)
                if not block:
                    break
                h.update(block)
        return h.hexdigest()
    except OSError:
        return None


def attest(mountpoint, pidfile=None, expect_backend=None, expect_fstypes=None):
    """Attest a mountpoint. expect_fstypes=None means "no filesystem type is required here",
    which is the native control's case: it must be some real filesystem, not a cowfs one."""
    if expect_fstypes is None and pidfile:
        expect_fstypes = COWFS_FSTYPES
    rows, reason = read_mount_table()
    if rows is None:
        return {"ok": False, "status": "UNKNOWN", "reason": "mount table unreadable: %s" % reason,
                "mountpoint": mountpoint}
    entry = resolve_mount(mountpoint, rows)
    if entry is None:
        return {"ok": False, "status": "UNKNOWN", "mountpoint": mountpoint,
                "reason": "no mount table entry contains %s, so there is nothing to attest"
                          % os.path.normpath(mountpoint)}
    result = {"ok": True, "status": "ATTESTED", "mountpoint": os.path.normpath(mountpoint),
              "resolved_mountpoint": entry["mountpoint"], "fstype": entry["fstype"],
              "source": entry["source"], "raw": entry["raw"],
              "st_dev": device_of(mountpoint), "statfs_fstype": statfs_fstype(mountpoint),
              "mount_entry": mountinfo_for_device(device_of(mountpoint))}
    if expect_fstypes and entry["fstype"] not in expect_fstypes:
        result["ok"] = False
        result["status"] = "WRONG_FSTYPE"
        result["reason"] = ("%s is on %s, not on %s; a directory that merely sits somewhere is "
                            "not the filesystem under test"
                            % (result["mountpoint"], entry["fstype"], "/".join(expect_fstypes)))
    daemon = attest_daemon(pidfile)
    result["daemon"] = daemon
    if expect_backend:
        if expect_fstypes and daemon is None:
            result["ok"] = False
            result.setdefault("reasons", []).append(
                "a %s backend was declared but no pid file was given, so the serving process "
                "cannot be attested" % expect_backend)
        elif expect_backend and daemon.get("backend") != expect_backend:
            result["ok"] = False
            result.setdefault("reasons", []).append(
                "daemon %s runs the %s backend, not %s"
                % (daemon.get("pid"), daemon.get("backend"), expect_backend))
        # The filesystem type the kernel reports for the device, checked against what the caller
    # declared. A path that resolves to the wrong filesystem is refused however it is named.
    # The mount table's type wins over statfs: statfs returns the generic "fuse" for every FUSE
    # filesystem, and the subtype that identifies cowfs is in the table's line, not in statfs.
    fstype_seen = ((result.get("mount_entry") or {}).get("fstype")
                   or result.get("statfs_fstype"))
    result["fstype_seen"] = fstype_seen
    result["fstype_source"] = ("mount table" if (result.get("mount_entry") or {}).get("fstype")
                               else "statfs" if result.get("statfs_fstype") else "unknown")
    if expect_fstypes and fstype_seen and fstype_seen not in expect_fstypes:
        result["ok"] = False
        result.setdefault("reasons", []).append(
            "the kernel reports %s for %s, not %s"
            % (fstype_seen, result["mountpoint"], "/".join(expect_fstypes)))
    if expect_fstypes and not fstype_seen:
        result["ok"] = False
        result.setdefault("reasons", []).append(
            "the filesystem type of %s could not be read from the kernel, so it cannot be "
            "attested as %s" % (result["mountpoint"], "/".join(expect_fstypes)))
    for key in result.get("reasons", []):
        result["reason"] = (result.get("reason", "") + "; " + key).strip("; ")
    return result


def main(argv=None):
    argv = list(sys.argv[1:] if argv is None else argv)
    if not argv or argv[0] not in ("attest", "--help", "-h"):
        sys.exit(__doc__)
    if argv[0] != "attest":
        print(__doc__)
        return EXIT_OK
    mountpoint = argv[1] if len(argv) > 1 else None
    if not mountpoint:
        sys.exit("attest needs a mountpoint")
    pidfile = backend = None
    rest = argv[2:]
    while rest:
        flag = rest.pop(0)
        if flag == "--pid":
            pidfile = rest.pop(0)
        elif flag == "--expect-backend":
            backend = rest.pop(0)
        elif flag == "--expect-fstype":
            pass
        else:
            sys.exit("unknown flag %s" % flag)
    result = attest(mountpoint, pidfile, backend)
    print(json.dumps(result, indent=2, sort_keys=True))
    if result["status"] == "UNKNOWN":
        return EXIT_UNKNOWN
    return EXIT_OK if result["ok"] else EXIT_WRONG_FS


if __name__ == "__main__":
    sys.exit(main())
