#!/bin/bash
# usage: run.sh LABEL N  [env assignments via ENV]  -> builds N times at fixed dir, hashes target tree
set -e; SHA="shasum -a 256"; command -v sha256sum >/dev/null && SHA=sha256sum
S=$(cd "$(dirname "$0")" && pwd); LABEL=$1; N=$2; shift 2
mkdir -p $S/out/$LABEL
for i in $(seq 1 $N); do
  rm -rf $S/proj/target
  (cd $S/proj && env "$@" cargo build --workspace 2>$S/out/$LABEL/build$i.err >/dev/null)
  (cd $S/proj/target && find . -type f ! -name '.cargo-lock' -print0 | sort -z | xargs -0 $SHA) > $S/out/$LABEL/$i.sha
done
