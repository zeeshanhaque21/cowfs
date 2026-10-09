#!/bin/bash
# usage: iso2.sh OUTN "CASES" TMO CB  (password on stdin)
W=/mnt/docs/Projects/cowfs-g5; read -r P
( trap '' HUP; printf '%s\n' "$P" | sudo -S -p "" env OUTN=$1 CASES="$2" TMO=$3 CB=$4 bash $W/rootloop.sh ) </dev/null >$W/logs/iso2.log 2>&1 &
echo launched
