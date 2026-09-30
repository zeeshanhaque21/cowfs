#!/bin/sh
# usage: sync.sh <local dir> <vm subdir under /home/zeeshanhaque/cowfs-spike3>; runs rsync inside the VM from the Mac path
set -e
S=$(cd "$1" && pwd)
orb -m cowfs-spike3 sh -c "mkdir -p /home/zeeshanhaque/cowfs-spike3/$2 && rsync -a --delete --exclude target --exclude out $S/ /home/zeeshanhaque/cowfs-spike3/$2/"
