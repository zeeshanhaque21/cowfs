import errno
import json
import os
from pathlib import Path
import subprocess
import sys


root = Path(sys.argv[1])
path = root / "readonly-owner"
results = {}
fd = os.open(path, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o444)
try:
    assert os.write(fd, b"created readonly\n") == len(b"created readonly\n")
    os.fsync(fd)
    results["a_create_write"] = "ok"
finally:
    os.close(fd)
assert path.stat().st_mode & 0o777 == 0o444
assert path.read_bytes() == b"created readonly\n"
try:
    fd = os.open(path, os.O_WRONLY)
except OSError as error:
    results["b_reopen"] = errno.errorcode[error.errno]
else:
    os.close(fd)
    results["b_reopen"] = "ok"
os.chmod(path, 0o644)
fd = os.open(path, os.O_WRONLY)
try:
    assert os.write(fd, b"writable\n") == len(b"writable\n")
    os.fsync(fd)
    results["d_chmod_reopen"] = "ok"
finally:
    os.close(fd)
assert path.stat().st_mode & 0o777 == 0o644
assert path.read_bytes().startswith(b"writable\n")
repo = root / "git-readonly"
repo.mkdir()
env = dict(os.environ, GIT_CONFIG_GLOBAL="/dev/null", GIT_CONFIG_SYSTEM="/dev/null",
           GIT_AUTHOR_NAME="test", GIT_AUTHOR_EMAIL="test@example.com",
           GIT_COMMITTER_NAME="test", GIT_COMMITTER_EMAIL="test@example.com")
try:
    for args in [["init", "-q"], ["add", "file"], ["commit", "-qm", "test"],
                 ["repack", "-ad"], ["fsck", "--strict"]]:
        if args[0] == "add":
            (repo / "file").write_text("small commit\n")
        subprocess.run(["git", *args], cwd=repo, env=env, check=True,
                       capture_output=True, text=True, timeout=60)
    packs = list((repo / ".git/objects/pack").glob("*.pack"))
    assert packs
    assert all(pack.stat().st_mode & 0o222 == 0 for pack in packs)
    results["e_git"] = "ok"
except subprocess.CalledProcessError as error:
    results["e_git"] = error.stderr.strip()
print(json.dumps(results, sort_keys=True), flush=True)
if "--check" in sys.argv:
    expected = {
        "a_create_write": "ok", "b_reopen": "EACCES",
        "d_chmod_reopen": "ok", "e_git": "ok",
    }
    for key, value in expected.items():
        assert results[key] == value, results
