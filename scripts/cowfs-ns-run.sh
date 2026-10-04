#!/bin/sh
# Run a command in a private Linux mount namespace in which --src also appears at --canonical,
# so two slots at different real paths build artifacts that embed one identical absolute path.
#
# usage: cowfs-ns-run.sh --src DIR --canonical DIR [--ns-mode auto|unprivileged|privileged] -- CMD [ARG...]
#
# The caller's mounts are never touched. Everything happens in a namespace that unshare creates
# with propagation already private, and the command replaces this script through exec, so the
# command's exit code and the signal that killed it reach the caller unchanged.
#
# Fails closed. With no namespace, or off Linux, nothing runs and the exit code is 77 with an
# UNMEASURABLE line on stderr, because a build that quietly ran at its raw path would produce
# artifacts that look comparable and are not.
set -eu

EXIT_USAGE=2
EXIT_UNMEASURABLE=77
prog=${0##*/}

usage() {
  printf 'usage: %s --src DIR --canonical DIR [--ns-mode auto|unprivileged|privileged] -- CMD [ARG...]\n' "$prog" >&2
}

die_usage() {
  usage
  printf '%s: %s\n' "$prog" "$*" >&2
  exit $EXIT_USAGE
}

unmeasurable() {
  printf '%s: UNMEASURABLE: %s\n' "$prog" "$*" >&2
  exit $EXIT_UNMEASURABLE
}

# Resolved once: unshare has to exec this script again inside the namespace, and $0 is a bare
# name rather than a path when the script was found through PATH.
self=$0
case $self in
/*) ;;
*/*) self=$PWD/$self ;;
*)
  self=$(command -v -- "$self") ||
    unmeasurable "cannot find this script to re-exec it inside the namespace"
  ;;
esac

# The two payload modes. unshare runs one of them inside the new namespace.
case ${1:-} in
__probe)
  # Reports this process's mount namespace id. The caller compares it with its own, which is the
  # only way to know a namespace was really created rather than assumed.
  readlink /proc/self/ns/mnt
  exit 0
  ;;
__inner)
  shift
  [ $# -ge 3 ] || exit $EXIT_USAGE
  inner_src=$1
  inner_canonical=$2
  shift 2
  if [ "${1:-}" = -- ]; then shift; fi
  [ $# -ge 1 ] || exit $EXIT_USAGE
  mount --rbind -- "$inner_src" "$inner_canonical" ||
    unmeasurable "cannot bind $inner_src onto $inner_canonical"
  # --rbind carries the source's propagation flags across, and a shared mount would send this
  # namespace's mounts back to the caller's peers.
  mount --make-private "$inner_canonical" ||
    unmeasurable "cannot make $inner_canonical private"
  cd "$inner_canonical" || unmeasurable "cannot enter $inner_canonical"
  inner_here=$(pwd -P)
  [ "$inner_here" = "$inner_canonical" ] ||
    unmeasurable "$inner_canonical resolves to $inner_here, so paths recorded in a build would not be the canonical ones"
  exec "$@"
  ;;
esac

src=
canonical=
ns_mode=auto

while [ $# -gt 0 ]; do
  case $1 in
  --src)
    [ $# -ge 2 ] || die_usage "--src needs a directory"
    src=$2
    shift 2
    ;;
  --canonical)
    [ $# -ge 2 ] || die_usage "--canonical needs a directory"
    canonical=$2
    shift 2
    ;;
  --ns-mode)
    [ $# -ge 2 ] || die_usage "--ns-mode needs a value"
    ns_mode=$2
    shift 2
    ;;
  --)
    shift
    break
    ;;
  -h | --help)
    usage
    exit 0
    ;;
  *)
    die_usage "unknown argument: $1"
    ;;
  esac
done

[ $# -ge 1 ] || die_usage "no command after --"
[ -n "$src" ] || die_usage "--src is required"
[ -n "$canonical" ] || die_usage "--canonical is required"
case $ns_mode in
auto | unprivileged | privileged) ;;
*) die_usage "--ns-mode must be auto, unprivileged or privileged, not: $ns_mode" ;;
esac

case $src in
/*) ;;
*) die_usage "--src must be an absolute path, not: $src" ;;
esac
case $canonical in
/*) ;;
*) die_usage "--canonical must be an absolute path, not: $canonical" ;;
esac
[ -d "$src" ] || die_usage "--src is not a directory: $src"
[ -d "$canonical" ] ||
  die_usage "--canonical is not an existing directory: $canonical"
case $canonical/ in
"$src"/*) die_usage "--canonical $canonical is inside --src $src" ;;
esac
case $src/ in
"$canonical"/*) die_usage "--src $src is inside --canonical $canonical" ;;
esac

[ "$(uname -s)" = Linux ] ||
  unmeasurable "mount namespaces need Linux, this is $(uname -s), so the command was not run at its raw path"

parent_ns=$(readlink /proc/self/ns/mnt) ||
  unmeasurable "cannot read /proc/self/ns/mnt, so no namespace can be verified"

# ns_run how MODE PROGRAM ARG... puts the namespace flags between PROGRAM and its argv and runs
# it. argv only: no command string is ever built out of caller input. "exec" hands the process
# over so the command inherits this one's status, signals and terminal.
ns_run() {
  how=$1
  mode=$2
  runner=$3
  shift 3
  # The flags go after the program name, because unshare reads them as its own.
  case $mode in
  unprivileged) set -- "$runner" --user --map-root-user --mount --propagation private "$@" ;;
  privileged) set -- "$runner" --mount --propagation private "$@" ;;
  esac
  if [ "$how" = exec ]; then
    exec "$@"
  fi
  "$@"
}

case $ns_mode in
auto) candidates="unprivileged privileged" ;;
*) candidates=$ns_mode ;;
esac

chosen=
note=
for candidate in $candidates; do
  # unshare's own error and the probe's id share one stream, so a refusal keeps its reason.
  note=$(ns_run run "$candidate" unshare -- "$self" __probe 2>&1) || true
  case $note in
  'mnt:['*)
    if [ "$note" != "$parent_ns" ]; then
      chosen=$candidate
      break
    fi
    note="ns-mode $candidate reported the caller's own mount namespace, so no isolation"
    ;;
  '')
    note="ns-mode $candidate produced no mount namespace id"
    ;;
  *)
    note="ns-mode $candidate: $note"
    ;;
  esac
done

[ -n "$chosen" ] ||
  unmeasurable "no private mount namespace, nothing was run. ${note:-no reason reported}"

printf '%s: mount namespace ready (%s), %s at %s\n' "$prog" "$chosen" "$src" "$canonical" >&2

ns_run exec "$chosen" unshare -- "$self" __inner "$src" "$canonical" -- "$@"