#!/usr/bin/env bash
set -euo pipefail

[ $# -eq 2 ] || { echo "usage: verify-stick.sh <image> <device>" >&2; exit 2; }
IMG=$1
DEV=$2

ISZ=$(stat -c%s "$IMG")
sudo blockdev --flushbufs "$DEV"
sudo cmp -n "$ISZ" "$IMG" "$DEV" || { echo "MISMATCH: read-back span differs from the image" >&2; exit 1; }
echo "STICK OK: $DEV holds the image, verified byte-for-byte over its $ISZ-byte span"
