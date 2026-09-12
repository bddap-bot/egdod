#!/usr/bin/env bash
set -euo pipefail
HERE=$(dirname "$(readlink -f "$0")")

if [ $# -lt 2 ]; then
  echo "usage: write-stick.sh <esp-image> <device-by-id-path> [--no-host] [--network SSID PSK]..." >&2
  echo "  bakes the wireless networks the stick may join (networks.sh) into the image, then" >&2
  echo "  writes ONLY to the block device the by-id path resolves to, and only if it is an" >&2
  echo "  unmounted ~30G USB disk; refuses everything else." >&2
  exit 2
fi
SRC=$(readlink -f "$1")
BYID=$2
shift 2

[ -f "$SRC" ] || { echo "refuse: no image at $SRC" >&2; exit 1; }
DEV=$(readlink -f "$BYID" 2>/dev/null || true)
[ -n "$DEV" ] && [ -b "$DEV" ] || { echo "refuse: $BYID does not resolve to a block device" >&2; exit 1; }
NAME=$(basename "$DEV")

TRAN=$(lsblk -ndo TRAN "$DEV")
TYPE=$(lsblk -ndo TYPE "$DEV")
SIZE=$(lsblk -ndo SIZE "$DEV")
MNT=$(lsblk -rno MOUNTPOINTS "$DEV" | grep -v '^$' || true)

echo "resolved $BYID -> $DEV  (type=$TYPE tran=$TRAN size=$SIZE)"
[ "$TYPE" = disk ] || { echo "refuse: $DEV is $TYPE, not a whole disk" >&2; exit 1; }
[ "$TRAN" = usb ]  || { echo "refuse: $DEV transport is '$TRAN', not usb" >&2; exit 1; }
case "$SIZE" in 29.9G|30G|29.8G) ;; *) echo "refuse: $DEV is $SIZE, not the expected ~30G" >&2; exit 1;; esac
[ -z "$MNT" ] || { echo "refuse: $DEV has mounted partitions: $MNT" >&2; exit 1; }
case "$NAME" in
  sda*|sdb*|nvme*|zram*|dm-*) echo "refuse: $NAME is a system/data device by name" >&2; exit 1;;
esac

T=$(mktemp -d "${TMPDIR:-/tmp}/egdod-stick.XXXXXX")
trap 'rm -rf "$T"' EXIT
mkdir "$T/overlay"
"$HERE/networks.sh" "$@" > "$T/overlay/wpa_supplicant.conf"
[ -s "$T/overlay/wpa_supplicant.conf" ] || rm "$T/overlay/wpa_supplicant.conf"
"$HERE/bake.sh" "$SRC" "$T/esp.img" "$T/overlay"
IMG=$T/esp.img

ISZ=$(stat -c%s "$IMG")
echo "writing $ISZ bytes of $IMG to $DEV"
sudo dd if="$IMG" of="$DEV" bs=4M conv=fsync status=progress
sync

BLOCKS=$(( (ISZ + 4194303) / 4194304 ))
IMGH=$(sha256sum "$IMG" | cut -d' ' -f1)
BACKH=$(sudo dd if="$DEV" bs=4M count="$BLOCKS" 2>/dev/null | head -c "$ISZ" | sha256sum | cut -d' ' -f1)
echo "image sha256:    $IMGH"
echo "readback sha256: $BACKH"
[ "$IMGH" = "$BACKH" ] || { echo "MISMATCH: read-back span differs from the image" >&2; exit 1; }
echo "STICK OK: $DEV holds the image, verified byte-for-byte over its $ISZ-byte span"
