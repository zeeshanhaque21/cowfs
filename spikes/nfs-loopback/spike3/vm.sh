#!/bin/sh
# usage: vm.sh <command...>  run inside the OrbStack VM cowfs-spike3
exec orb -m cowfs-spike3 "$@"
