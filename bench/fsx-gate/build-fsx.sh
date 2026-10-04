#!/bin/sh
# Fetch the pinned fsx source, build it once, and record what that binary is.
#
# usage: build-fsx.sh WORKDIR [--no-fetch]
#
#   WORKDIR/src            the pinned clone (skipped with --no-fetch)
#   WORKDIR/build          ltp/fsx.c, src/{global.h,statx.h,config.h}, the fsx binary, usage.txt
#   WORKDIR/identity.json  upstream, ref, commit, per-file sha256, compiler, binary sha256, flags
#
# Both arms run WORKDIR/build/fsx. There is no second build, so "same version, same binary" is a
# fact about one file rather than a claim about two.
set -eu

WORK=${1:?usage: build-fsx.sh WORKDIR [--no-fetch]}
FETCH=${2:-}
HERE=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
GATE="$HERE/fsx-gate.json"
mkdir -p "$WORK"
WORK=$(CDPATH='' cd -- "$WORK" && pwd)

UPSTREAM=$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["tool"]["upstream"])' "$GATE")
REF=$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["tool"]["ref"])' "$GATE")
COMMIT=$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["tool"]["commit"])' "$GATE")
CONFIG_H="$HERE/$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["tool"]["config_header"])' "$GATE")"

if [ "$FETCH" != "--no-fetch" ]; then
    if [ ! -d "$WORK/src/.git" ]; then
        # An annotated tag does not clone as a commit, so peel it here and compare the commit.
        rm -rf "$WORK/src"
        GIT_TERMINAL_PROMPT=0 git clone --quiet --depth 1 --branch "$REF" "$UPSTREAM" "$WORK/src"
    fi
    GOT=$(git -C "$WORK/src" rev-parse "HEAD")
    if [ "$GOT" != "$COMMIT" ]; then
        echo "fetched $REF at $GOT, the gate pins $COMMIT" >&2
        exit 1
    fi
fi

mkdir -p "$WORK/build/src" "$WORK/build/ltp"
cp "$WORK/src/ltp/fsx.c" "$WORK/build/ltp/fsx.c"
cp "$WORK/src/src/global.h" "$WORK/build/src/global.h"
cp "$WORK/src/src/statx.h" "$WORK/build/src/statx.h"
cp "$CONFIG_H" "$WORK/build/src/config.h"

python3 - "$GATE" "$WORK/build" <<'PY'
import hashlib, json, sys
gate, build = sys.argv[1], sys.argv[2]
want = json.load(open(gate))["tool"]["files"]
for rel, digest in sorted(want.items()):
    got = hashlib.sha256(open(f"{build}/{rel}", "rb").read()).hexdigest()
    if got != digest:
        sys.exit(f"{rel}: sha256 {got}, the gate pins {digest}")
print("source sha256 verified:", len(want), "files")
PY

# The flags are xfstests' own for every C file, plus -I build/src so global.h and statx.h
# resolve next to config.h, plus the two headers ltp/fsx.c uses without including: <getopt.h>
# for getopt_long and <linux/kernel.h> for roundup. The source is not modified.
(cd "$WORK/build" && cc -std=gnu11 -funsigned-char -fno-strict-aliasing -Wall \
    -D_GNU_SOURCE -D_FILE_OFFSET_BITS=64 -O2 -I src \
    -include getopt.h -include linux/kernel.h \
    -o fsx ltp/fsx.c -lpthread)

# fsx prints its usage and exits 90 with no filename. That text is the only record of which
# features this binary actually compiled in, so it is kept rather than discarded.
set +e
"$WORK/build/fsx" > "$WORK/build/usage.txt" 2>&1
USAGE_EXIT=$?
set -e
if [ "$USAGE_EXIT" -ne 90 ]; then
    echo "fsx exited $USAGE_EXIT on its own usage text, expected 90" >&2
    exit 1
fi

CC_ID=$(cc --version | head -1)
CC_SHA=$(command -v cc)
python3 - "$GATE" "$WORK" "$COMMIT" "$REF" "$CC_ID" "$CC_SHA" <<'PY'
import hashlib, json, os, platform, re, subprocess, sys
gate, work, commit, ref, cc_id, cc_sha = sys.argv[1:7]
tool = json.load(open(gate))["tool"]
binary = os.path.join(work, "build", "fsx")
usage = open(os.path.join(work, "build", "usage.txt")).read()
flags = sorted(set(re.findall(r"(?m)^\t-([A-Za-z0-9_]+):", usage)))
identity = {
    "tool": "fsx",
    "upstream": tool["upstream"],
    "ref": ref,
    "commit": commit,
    "path": tool["path"],
    "file_sha256": tool["files"],
    "config_header_sha256": hashlib.sha256(
        open(os.path.join(work, "build", "src", "config.h"), "rb").read()).hexdigest(),
    "compile": tool["compile"],
    "compiler": cc_id,
    "cc_path": cc_sha,
    "binary_path": binary,
    "binary_sha256": hashlib.sha256(open(binary, "rb").read()).hexdigest(),
    "usage_exit": 90,
    "flags_in_this_binary": flags,
    "platform": platform.platform(),
}
json.dump(identity, open(os.path.join(work, "identity.json"), "w"), indent=2, sort_keys=True)
print("binary", identity["binary_sha256"])
print("flags", " ".join(flags))
PY
