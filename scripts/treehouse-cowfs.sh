#!/bin/sh
# Run treehouse with its pool on the live cowfs mount (see docs/live-treehouse.md).
# Refuses to run when the mount or the control daemon is absent, so a pool is never
# silently created on the underlying native directory.
#
# Usage: treehouse-cowfs.sh <treehouse args...>      e.g. get --lease --json --no-fetch
# Override with: COWFS_HOME (default $HOME/.cowfs), COWFS_MOUNT, COWFS_SOCKET, COWFS_BIN, TREEHOUSE_BIN.
set -eu
case "${1:-}" in
-h | --help | "")
    sed -n '2,7p' "$0" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
esac
project=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
home=${COWFS_HOME:-$HOME/.cowfs}
mount=${COWFS_MOUNT:-$home/mnt}
socket=${COWFS_SOCKET:-$home/sock/daemon.sock}
cowfs=${COWFS_BIN:-$project/spikes/nfs-loopback/out/live/bin/cowfs}
treehouse=${TREEHOUSE_BIN:-$(command -v treehouse || echo "$HOME/.local/bin/treehouse")}
if ! /sbin/mount | /usr/bin/grep -F " on $mount (nfs," >/dev/null; then
    echo "cowfs is not mounted at $mount; refusing native-filesystem fallback" >&2
    exit 1
fi
"$cowfs" --socket "$socket" --timeout 5 snapshot list >/dev/null
export TREEHOUSE_APFS_SHARING=off
exec "$treehouse" --root "$mount/base" "$@"
