#!/usr/bin/env bash
set -euo pipefail
HERE=$(dirname "$(readlink -f "$0")")

T=$(mktemp -d "${TMPDIR:-/tmp}/egdod-verify.XXXXXX")
LOOP=
cleanup() {
  rm -rf "$T"
  [ -z "$LOOP" ] || sudo -n losetup -d "$LOOP"
}
trap cleanup EXIT

ISZ=$(( 4194304 + 12345 ))
{ head -c $(( ISZ - 1 )) /dev/urandom; printf '\000'; } > "$T/esp.img"
truncate -s 16M "$T/disk"
LOOP=$(sudo -n losetup --find --show "$T/disk")
sudo -n dd if="$T/esp.img" of="$LOOP" bs=4M conv=fsync status=none
printf '\377' | sudo -n dd of="$LOOP" bs=1 seek="$ISZ" conv=notrunc,fsync status=none

"$HERE/verify-stick.sh" "$T/esp.img" "$LOOP" \
  || { echo "FAIL: a correct write was reported as wrong" >&2; exit 1; }

printf '\001' | sudo -n dd of="$LOOP" bs=1 seek=$(( ISZ - 1 )) conv=notrunc,fsync status=none
if "$HERE/verify-stick.sh" "$T/esp.img" "$LOOP"; then
  echo "FAIL: a write with a corrupted last byte was reported as correct" >&2
  exit 1
fi
echo "PASS: correct write accepted, corrupted write rejected ($ISZ-byte image)"
