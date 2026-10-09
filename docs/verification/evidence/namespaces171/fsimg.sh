#!/bin/bash
# usage: fsimg.sh up|down ext4|xfs   (sudo password on stdin, one line). sudo only for losetup/mkfs/mount/umount/chown on images under W.
set -eu
W=/mnt/docs/Projects/cowfs-171
FS=$2; IMG=$W/img/$FS.img; MNT=$W/mnt-$FS; DEVF=$W/img/$FS.dev
read -r P
S() { printf '%s\n' "$P" | sudo -S -p "" "$@"; }
case $1 in
up)
  mkdir -p "$W/img" "$MNT"
  [ ! -e "$IMG" ] || { echo "$IMG exists"; exit 1; }
  truncate -s 4G "$IMG"
  dev=$(S losetup --find --show "$IMG")
  echo "$dev" > "$DEVF"
  case $FS in
  ext4) S mkfs.ext4 -q -F "$dev" ;;
  xfs) S mkfs.xfs -q -f "$dev" ;;
  esac
  S mount "$dev" "$MNT"
  S chown "$(id -u):$(id -g)" "$MNT"
  echo "up: $dev -> $MNT"; findmnt -no FSTYPE,SOURCE,OPTIONS -T "$MNT"
  ;;
down)
  dev=$(cat "$DEVF")
  S umount "$MNT"
  S losetup -d "$dev"
  rm -f "$IMG" "$DEVF"; rmdir "$MNT"
  echo "down: $dev"
  ;;
esac
