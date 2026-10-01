import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile

ROOT = Path(__file__).resolve().parents[3]
OUT = ROOT / "target" / "qa4"


def run(args, cwd=ROOT, env=None, timeout=600):
    return subprocess.run(
        ["rtk", "proxy", *args], cwd=cwd, env=env,
        stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
        text=True, timeout=timeout,
    )


def fixture(name, revision="HEAD"):
    dest = OUT / name / "source"
    dest.mkdir(parents=True, exist_ok=False)
    archive = subprocess.check_output(["rtk", "proxy", "git", "archive", revision], cwd=ROOT)
    with tarfile.open(fileobj=io.BytesIO(archive)) as src:
        src.extractall(dest, filter="data")
    return dest


def mutate(name, dest):
    store = dest / "crates/cowfs-store/src/store.rs"
    fsio = dest / "crates/cowfs-store/src/fsio.rs"
    wm = dest / "crates/cowfs-store/src/wm.rs"
    if name == "marker-fsync-dropped":
        text = fsio.read_text()
        old = "        self.sync_file(&f, &tmp)?;"
        assert text.count(old) == 1
        fsio.write_text(text.replace(old, '        if file_name != "ACKED" { self.sync_file(&f, &tmp)?; }'))
    elif name == "marker-after-truncate":
        text = store.read_text()
        start = text.index("                if recovery.watermark_missing")
        end = text.index("                save_torn(", start)
        block = text[start:end]
        text = text[:start] + text[end:]
        at = "                recovery.torn_tail_discarded += len - t;"
        assert text.count(at) == 1
        store.write_text(text.replace(at, at + "\n" + block))
    elif name == "highwater-fsync-dropped":
        text = wm.read_text()
        at = text.index("    pub(crate) fn raise_next")
        tail = text[at:]
        old = "        self.io.sync_file(&self.file, &self.path)?;"
        assert tail.count(old) == 1
        wm.write_text(text[:at] + tail.replace(old, ""))
    elif name == "highwater-after-create":
        text = store.read_text()
        text = text.replace("        wm.raise_next(next)?;", "")
        old = "        let file = Arc::new(create_pack(&self.io, &self.dir, id)?);"
        assert text.count(old) == 2
        text = text.replace(old, old + "\n        self.wm.lock().unwrap().raise_next(id + 1)?;")
        old = "                wm.raise_next(next_id)?;"
        assert text.count(old) == 1
        text = text.replace(old, "")
        old = "                let file = Arc::new(create_pack(&io, &dir, id)?);"
        assert text.count(old) == 1
        store.write_text(text.replace(old, old + "\n                wm.raise_next(next_id)?;"))
    else:
        raise ValueError(name)


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    names = ["marker-fsync-dropped", "marker-after-truncate", "highwater-fsync-dropped", "highwater-after-create"]
    if len(sys.argv) > 1:
        names = sys.argv[1:]
    for name in names:
        dest = fixture(name, "070ee93" if name == "before" else "HEAD")
        if name == "before":
            for path in ["Cargo.toml", "src/fsio.rs", "tests/round4.rs"]:
                rel = Path("crates/cowfs-store") / path
                (dest / rel).write_bytes((ROOT / rel).read_bytes())
        else:
            mutate(name, dest)
        env = {**os.environ, "CARGO_TARGET_DIR": str(OUT / name / "build")}
        result = run(["cargo", "test", "-j4", "-p", "cowfs-store", "--features", "fault-injection", "--test", "round4", "--", "--nocapture"], dest, env)
        (OUT / name / "test.log").write_text(result.stdout)
        broken_build = "could not compile" in result.stdout or "error[E" in result.stdout
        state = "invalid" if broken_build else "survived" if result.returncode == 0 else "killed"
        row = {"name": name, "state": state, "exit": result.returncode}
        with (OUT / "results.jsonl").open("a") as report:
            report.write(json.dumps(row) + "\n")
            report.flush()
        print(json.dumps(row), flush=True)
        if broken_build:
            raise RuntimeError(result.stdout[-3000:])


if __name__ == "__main__":
    main()
