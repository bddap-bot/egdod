#!/usr/bin/env bash
set -euo pipefail

if [ $# -ne 3 ]; then
  echo "usage: bake.sh <esp-image> <out-image> <overlay-dir>" >&2
  echo "  copies the image and appends the overlay directory to its initrd as a second" >&2
  echo "  cpio archive: every file in it lands at the same path in the booted initramfs." >&2
  exit 2
fi
IMG=$1
OUT=$2
DIR=$3

[ -f "$IMG" ] || { echo "refuse: no image at $IMG" >&2; exit 1; }
[ -d "$DIR" ] || { echo "refuse: no overlay dir at $DIR" >&2; exit 1; }
T=$(mktemp -d "${TMPDIR:-/tmp}/egdod-bake.XXXXXX")
trap 'rm -rf "$T"' EXIT

cp "$IMG" "$OUT"
chmod u+w "$OUT"
mcopy -i "$OUT" ::initrd.img "$T/initrd.img"
( cd "$DIR" && find . -print0 | cpio --null -H newc -o 2>/dev/null | gzip -9 ) >> "$T/initrd.img"
mdel -i "$OUT" ::initrd.img
mcopy -i "$OUT" "$T/initrd.img" ::initrd.img
echo "baked $(cd "$DIR" && find . -type f | wc -l) file(s) into $OUT"
