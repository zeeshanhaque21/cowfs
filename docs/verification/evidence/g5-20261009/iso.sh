#!/bin/bash
W=/mnt/docs/Projects/cowfs-g5; read -r P
( trap '' HUP; printf '%s\n' "$P" | sudo -S -p "" bash $W/rootloop.sh ) </dev/null >$W/logs/iso.log 2>&1 &
echo launched
