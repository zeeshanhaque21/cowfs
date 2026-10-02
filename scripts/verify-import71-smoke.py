import argparse
import json
import os
from pathlib import Path
import subprocess
import time

parser = argparse.ArgumentParser()
parser.add_argument("--output", type=Path, required=True)
args = parser.parse_args()
out = args.output.resolve()
out.mkdir(mode=0o700, parents=True, exist_ok=False)
repo = Path(__file__).resolve().parents[1]
source = out / "source"
source.mkdir()
(source / ".git").mkdir()
(source / "empty-dir").mkdir()
files = {"binary": bytes(range(256)) * 4096, "zero": b"",
         "café.txt": b"hello\n", ".git/HEAD": b"ref: refs/heads/main\n"}
for name, data in files.items():
    (source / name).write_bytes(data)
os.symlink("binary", source / "link")
os.symlink("missing", source / "dangling")
before = {name: ((source / name).stat().st_mode,
                 (source / name).stat().st_mtime_ns,
                 (source / name).stat().st_ctime_ns) for name in files}
mount = out / "mount"
mount.mkdir()
socket = out / "control.sock"
binary = repo / "target/debug/cowfs"
daemon = repo / "target/debug/cowfs-daemon"


def cli(*words, success=True):
    p = subprocess.run([str(binary), "--socket", str(socket), "--timeout", "15",
                        "--json", *map(str, words)], capture_output=True, text=True, timeout=30)
    if success and p.returncode:
        raise RuntimeError(p.stdout + p.stderr)
    return p


with (out / "daemon.log").open("wb", buffering=0) as log:
    proc = subprocess.Popen([str(daemon), "--store", str(out / "store"),
                             "--mount", str(mount), "--socket", str(socket),
                             "--backend", "core"], stdout=log, stderr=log, start_new_session=True)
    (out / "daemon.pid").write_text(str(proc.pid) + "\n")
    try:
        deadline = time.monotonic() + 45
        while not socket.exists():
            if proc.poll() is not None:
                raise RuntimeError((out / "daemon.log").read_text(errors="replace"))
            if time.monotonic() > deadline:
                raise TimeoutError("private daemon did not start in 45 seconds")
            time.sleep(0.1)
        cli("snapshot", "list")
        first = cli("import", source, "--store-name", "sample")
        (out / "first.json").write_text(first.stdout)
        first_report = json.loads(first.stdout)
        assert first_report["verified"]
        assert first_report["source_root_hash"] == first_report["imported_root_hash"]
        imported = mount / "sample"
        for name, data in files.items():
            assert (imported / name).read_bytes() == data, name
            assert (source / name).read_bytes() == data, name
            stat = (source / name).stat()
            assert (stat.st_mode, stat.st_mtime_ns, stat.st_ctime_ns) == before[name], name
        assert os.readlink(imported / "link") == "binary"
        assert os.readlink(imported / "dangling") == "missing"
        assert (imported / "empty-dir").is_dir()
        duplicate = cli("import", source, "--store-name", "sample", success=False)
        assert duplicate.returncode != 0 and "already_exists" in duplicate.stdout + duplicate.stderr
        second = cli("import", source, "--store-name", "sample2")
        (out / "second.json").write_text(second.stdout)
        second_report = json.loads(second.stdout)
        assert second_report["verified"] and second_report["stored_bytes"] == 0
        assert second_report["imported_root_hash"] == first_report["imported_root_hash"]
        bad = out / "unsupported"
        bad.mkdir()
        os.mkfifo(bad / "pipe")
        refused = cli("import", bad, "--store-name", "bad", success=False)
        assert refused.returncode != 0 and "invalid_params" in refused.stdout + refused.stderr
        names = cli("snapshot", "list")
        (out / "snapshots.json").write_text(names.stdout)
        assert '"bad"' not in names.stdout
        result = {"readback": "passed", "source_file_bytes_and_metadata": "unchanged",
                  "symlinks_and_empty_directory": "passed", "duplicate": "refused",
                  "fifo": "refused", "first": first.stdout, "second": second.stdout}
        (out / "result.json").write_text(json.dumps(result, indent=2) + "\n")
        print(json.dumps({"result": "passed", "evidence": str(out)}), flush=True)
    finally:
        if proc.poll() is None:
            identity = subprocess.run(["ps", "-p", str(proc.pid), "-o", "command="],
                                      capture_output=True, text=True, timeout=5).stdout
            if str(daemon) not in identity or str(socket) not in identity:
                raise RuntimeError("private daemon identity mismatch; left running")
            proc.terminate()
            proc.wait(timeout=30)
