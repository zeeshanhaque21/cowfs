#!/bin/sh
# Run one command as a single foreground invocation while holding a wave resource lock.
#
# usage: locked-run.sh LOCK_PATH COMMAND [ARGS...]
#
# The lock recipe is the one in docs/ready-wave-dispatch.md, unchanged: a nonblocking flock on
# LOCK_PATH retried for 600 s, then exit 75. The python parent stays alive until the child exits,
# so the lock is held for the whole command and released only when it is done.
#
# A busy lane is a blocker, not permission to run unlocked. Exit 75 means exactly that.
set -eu

LOCK=${1:?usage: locked-run.sh LOCK_PATH COMMAND [ARGS...]}
shift
[ "$#" -gt 0 ] || {
    echo "usage: locked-run.sh LOCK_PATH COMMAND [ARGS...]" >&2
    exit 2
}

exec python3 -c 'import fcntl,os,subprocess,sys,time; p=sys.argv[1]; os.makedirs(os.path.dirname(p),exist_ok=True); f=open(p,"a"); until=time.monotonic()+600
while True:
 try: fcntl.flock(f,fcntl.LOCK_EX|fcntl.LOCK_NB); break
 except BlockingIOError:
  if time.monotonic()>until: print("resource lane busy; blocked",file=sys.stderr); sys.exit(75)
  time.sleep(.25)
sys.exit(subprocess.run(sys.argv[2:]).returncode)' "$LOCK" "$@"
