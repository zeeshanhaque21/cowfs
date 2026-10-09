#!/bin/bash
# Drive bench/g5_root.sh on the cachyos box. The sudo password is read ONCE from this
# script's stdin (start only), travels over ssh stdin, and is never in argv, a file or a log.
#   printf '%s\n' "$PW" | g5_box.sh start RUN CASES_FILE [ARMS]   # detached, one sudo
#   g5_box.sh poll RUN        # progress tail, exits 0 done, 1 aborted, 2 running, 3 dead
#   g5_box.sh fetch RUN DIR   # copy evidence (consoles, identities, meta) back
set -u
HOST=${G5_HOST:-zeeshan@100.122.64.51}
W=${G5_W:-/mnt/docs/Projects/cowfs-g5}
HERE=$(cd "$(dirname "$0")" && pwd)
cmd=${1:?}; run=${2:?}; OUT=$W/out/g5-$run
case $W in /mnt/docs/*) ;; *) echo "refusing: workspace $W is not under /mnt/docs" >&2; exit 1;; esac
case $cmd in
start)
  cases=${3:?}; arms=${4:-native cowfs control}
  # busy-box check: another run, a suite process, or a loaded box means stop
  busy=$(ssh "$HOST" "uptime; who | wc -l; echo \$(( \$(pgrep -cx 'fsstress|fsx|cowfs-daemon') + \$(pgrep -cf '^bash [^ ]*g5_root.sh') ))" 2>&1)
  echo "$busy" | head -3
  load=$(echo "$busy" | sed -n 's/.*load average: \([0-9.]*\).*/\1/p' | head -1)
  nproc_busy=$(echo "$busy" | tail -1)
  if [ "${nproc_busy:-0}" != 0 ] || [ "$(echo "${load:-0} > 4" | bc)" = 1 ]; then
    [ -n "${G5_FORCE:-}" ] || { echo "refusing: box looks busy (set G5_FORCE=1 to override)" >&2; exit 1; }
  fi
  scp -q "$HERE/g5_root.sh" "$cases" "$HOST:$W/" || exit 1
  ssh "$HOST" "mkdir -p $OUT && cp $W/$(basename "$cases") $OUT.cases && cd $W && cat > $OUT.launch.sh" <<'EOF' || exit 1
#!/bin/bash
read -r P
[ -n "$P" ] || { echo "empty password, not attempting sudo" >&2; exit 9; }
RUN_OUT=$1; shift
# one attempt only: a wrong password must not burn faillock tries
printf '%s\n' "$P" | sudo -S -p "" -v 2>/dev/null || { echo "sudo -v failed, stopping" >&2; exit 9; }
( printf '%s\n' "$P" | setsid sudo -S -p "" env "$@" >"$RUN_OUT.sudo.log" 2>&1 & echo $! > "$RUN_OUT.pid" )
EOF
  # the password goes to the launcher's stdin only; everything else is argv-safe text
  ssh "$HOST" "bash $OUT.launch.sh $OUT XFS=$W/ref/xfstests BIN=$W/target/release ARMS='$arms' COWFS_REV=${COWFS_REV:-} NOSHIM=${NOSHIM:-} TMO=${TMO:-300} bash $W/g5_root.sh $OUT $OUT.cases"
  ;;
poll)
  ssh "$HOST" "tail -n 5 $OUT/progress.txt 2>/dev/null; tail -n 3 $OUT.sudo.log 2>/dev/null | grep -v '^\$' ; uptime"
  st=$(ssh "$HOST" "if grep -q ALLDONE $OUT/progress.txt 2>/dev/null; then echo done; elif grep -q ABORT $OUT/progress.txt 2>/dev/null; then echo abort; elif pgrep -f '^bash $W/g5_root.sh $OUT ' >/dev/null; then echo run; else echo dead; fi")
  echo "STATE=$st"; case $st in done) exit 0;; abort) exit 1;; run) exit 2;; *) exit 3;; esac
  ;;
fetch)
  dest=${3:?}; mkdir -p "$dest"
  ssh "$HOST" "cd $OUT && tar -cf - --exclude=results --exclude=store --exclude='*.img' --exclude=main ." | tar -xf - -C "$dest"
  ;;
esac
