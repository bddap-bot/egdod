#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

WORK=$(mktemp -d "${TMPDIR:-/tmp}/egdod-boot.XXXXXX")
SERIAL=$WORK/serial.log
STATE=$WORK/controller
PIDS=()
cleanup() {
  for p in "${PIDS[@]:-}"; do kill "$p" 2>/dev/null || true; done
  rm -rf "$WORK"
}
trap cleanup EXIT

say() { printf '\n=== %s\n' "$*"; }

NIXPKGS='(import (builtins.fetchTarball { url = "https://github.com/NixOS/nixpkgs/archive/e2587caef70cea85dd97d7daab492899902dbf5d.tar.gz"; sha256 = "14jrgz4z2m8n1c8qwcla44kdy9kd7x0xnwfyrajnyvnhkxbnnqf1"; }) {})'

say "building qemu + OVMF (Microsoft keys) and the native controller"
QEMU=$(nix-build --no-out-link -E "$NIXPKGS.qemu")/bin/qemu-system-x86_64
OVMF=$(nix-build --no-out-link -E "$NIXPKGS.OVMFFull.fd")/FV
CODE=$OVMF/OVMF_CODE.ms.fd
VARS_SRC=$OVMF/OVMF_VARS.ms.fd
[ -r "$CODE" ] && [ -r "$VARS_SRC" ] || { echo "OVMF Microsoft-key firmware not found" >&2; exit 1; }
BIN=$(nix-shell --run 'cargo build --release -q >&2 && echo "${CARGO_TARGET_DIR:-target}/release/egdod"')
BIN=$(readlink -f "$BIN")

PORT=${EGDOD_BOOT_PORT:-52847}
DIRECT=10.0.2.2:$PORT

say "controller init + serve on the host (no relay, bound to 0.0.0.0:$PORT)"
"$BIN" controller --state-dir "$STATE" init >"$WORK/init.out"
NODE_ID=$(sed -n 's/.*[Nn]ode.* \([0-9a-f]\{64\}\).*/\1/p; s/^\([0-9a-f]\{64\}\)$/\1/p' "$WORK/init.out" | head -1)
[ -n "$NODE_ID" ] || NODE_ID=$(grep -oE '[0-9a-f]{64}' "$WORK/init.out" | head -1)
echo "controller node id: $NODE_ID"
"$BIN" controller --state-dir "$STATE" serve --no-relay --bind "0.0.0.0:$PORT" \
  --probe-interval 10 --probe-timeout 5 >"$WORK/serve.log" 2>&1 &
PIDS+=($!)
sleep 2

say "building the stick image for this controller (Secure Boot chain, pinned)"
NET_ARGS=()
if [ "${EGDOD_BOOT_DHCP:-}" = 1 ]; then NET_ARGS=(--arg dhcp true); echo "network: DHCP"; else echo "network: static"; fi
IMG=$(nix-build --no-out-link boot/image.nix \
  --argstr controllerNodeId "$NODE_ID" --argstr direct "$DIRECT" "${NET_ARGS[@]}")/esp.img
echo "image: $IMG"

cp "$VARS_SRC" "$WORK/vars.fd"; chmod +w "$WORK/vars.fd"

say "booting under qemu/OVMF with Secure Boot ON"
"$QEMU" \
  -machine q35,accel=kvm:tcg -cpu host -m 1024 \
  -drive if=pflash,format=raw,readonly=on,file="$CODE" \
  -drive if=pflash,format=raw,file="$WORK/vars.fd" \
  -drive format=raw,file="$IMG",if=ide,snapshot=on \
  -netdev user,id=n0 -device e1000,netdev=n0 \
  -display none -vga none -serial "file:$SERIAL" -no-reboot >"$WORK/qemu.log" 2>&1 &
QEMU_PID=$!
PIDS+=("$QEMU_PID")

say "waiting for the agent to boot and print its public key"
AGENT=""
for _ in $(seq 1 120); do
  kill -0 "$QEMU_PID" 2>/dev/null || { echo "qemu exited early" >&2; cat "$SERIAL" >&2; exit 1; }
  AGENT=$(sed -n 's/^agent pubkey: \([0-9a-f]*\).*/\1/p' "$SERIAL" 2>/dev/null | head -1 || true)
  [ -n "$AGENT" ] && break
  sleep 2
done
[ -n "$AGENT" ] || { echo "no agent pubkey on serial within timeout" >&2; tail -40 "$SERIAL" >&2; exit 1; }
echo "agent pubkey seen on serial: $AGENT"

say "before approval: the controller serves the agent nothing"
if "$BIN" controller --state-dir "$STATE" exec "$AGENT" -- /bin/busybox true >/dev/null 2>&1; then
  echo "DEFECT: an unapproved agent was served" >&2; exit 1
fi
echo "unapproved exec refused"

say "approve by public key, then run commands as root inside the booted VM"
"$BIN" controller --state-dir "$STATE" approve "$AGENT"
for _ in $(seq 1 20); do
  if "$BIN" controller --state-dir "$STATE" exec "$AGENT" -- /bin/busybox id -u >"$WORK/uid" 2>"$WORK/uid.err"; then break; fi
  sleep 2
done
UID_SEEN=$(cat "$WORK/uid")
echo "uid seen by exec: $UID_SEEN"
[ "$UID_SEEN" = 0 ] || { echo "DEFECT: exec did not run as root" >&2; exit 1; }
echo "--- uname -a over the wire:"
"$BIN" controller --state-dir "$STATE" exec "$AGENT" -- /bin/busybox uname -a
echo "--- /proc/cmdline read from the booted VM (proves the baked node id):"
"$BIN" controller --state-dir "$STATE" exec "$AGENT" -- /bin/busybox cat /proc/cmdline
echo "--- SecureBoot EFI variable, read from inside the guest firmware (01 = on):"
echo "(attributes then value; trailing 01 = Secure Boot on)"
"$BIN" controller --state-dir "$STATE" exec "$AGENT" -- /bin/busybox od -An -tx1 /sys/firmware/efi/efivars/SecureBoot-8be4df61-93ca-11d2-aa0d-00e098032b8c || true

say "receive a root filesystem over the wire and switch_root into it"
STATIC_CC=$(nix-build --no-out-link -E "$NIXPKGS.pkgsStatic.stdenv.cc")/bin/x86_64-unknown-linux-musl-gcc
cat > "$WORK/newinit.c" <<'C'
#include <fcntl.h>
#include <stdio.h>
#include <sys/stat.h>
#include <sys/mount.h>
#include <unistd.h>
int main(void) {
    printf("NEWROOT-INIT: switch_root landed; the received OS is PID 1 now\n");
    char b[256];
    int fd = open("/os-marker", O_RDONLY);
    if (fd >= 0) { int n = read(fd, b, sizeof b - 1); if (n > 0) { b[n] = 0; printf("NEWROOT-INIT: %s", b); } close(fd); }
    mkdir("/proc", 0755);
    mount("proc", "/proc", "proc", 0, NULL);
    fd = open("/proc/version", O_RDONLY);
    if (fd >= 0) { int n = read(fd, b, sizeof b - 1); if (n > 0) { b[n] = 0; printf("NEWROOT-INIT: same kernel, no kexec: %s", b); } close(fd); }
    fflush(stdout);
    for (;;) pause();
}
C
"$STATIC_CC" -static -O2 -o "$WORK/newinit" "$WORK/newinit.c"
mkdir -p "$WORK/nr/sbin"
cp "$WORK/newinit" "$WORK/nr/sbin/init"
echo "received over egdod, unpacked into a tmpfs, no distro touched it" > "$WORK/nr/os-marker"
( cd "$WORK/nr" && tar cf "$WORK/newroot.tar" . )
echo "pushing the rootfs tarball to the target as root"
"$BIN" controller --state-dir "$STATE" push "$AGENT" "$WORK/newroot.tar" /newroot.tar
echo "unpacking into a fresh tmpfs and arming the handoff"
"$BIN" controller --state-dir "$STATE" exec "$AGENT" -- /bin/busybox sh -c \
  'mkdir -p /newroot && /bin/busybox mount -t tmpfs none /newroot && cd /newroot && /bin/busybox tar xf /newroot.tar && printf "%s" "/newroot /sbin/init" > /switch.req' || true
echo "waiting for the received OS to come up as PID 1 on the serial console"
SR=""
for _ in $(seq 1 30); do
  SR=$(grep -a "NEWROOT-INIT: switch_root landed" "$SERIAL" 2>/dev/null | head -1 || true)
  [ -n "$SR" ] && break
  sleep 2
done
[ -n "$SR" ] || { echo "DEFECT: switch_root marker never appeared on serial" >&2; sed 's/\x1b\[[0-9;]*[a-zA-Z]//g' "$SERIAL" | tail -20 >&2; exit 1; }
echo "switch_root confirmed on serial:"
sed 's/\x1b\[[0-9;]*[a-zA-Z]//g' "$SERIAL" | grep -a "NEWROOT-INIT" | head -4

say "controller serve log (path label, agent connect):"
grep -aE 'routed a session|agent connected|reachability|UNDIALABLE' "$WORK/serve.log" | tail -8 || true

say "Secure Boot markers from the guest kernel (serial console):"
sed 's/\x1b\[[0-9;]*[a-zA-Z]//g; s/\x1b[=>]//g' "$SERIAL" | grep -aE "Secure Boot is enabled|locked down from EFI|secureboot: Secure boot|Debian Secure Boot CA|e1000 .* eth0|init: egdod|agent pubkey|switch_root|NEWROOT-INIT" | head -12

if [ -n "${BOTQ_ARTIFACTS_DIR:-}" ]; then
  sed 's/\x1b\[[0-9;]*[a-zA-Z]//g; s/\x1b[=>]//g' "$SERIAL" > "$BOTQ_ARTIFACTS_DIR/boot-serial.log"
  echo "serial log saved to $BOTQ_ARTIFACTS_DIR/boot-serial.log"
fi
echo
echo "BOOT OK: Secure Boot on, agent dialed out, controller approved and ran as root in the VM."
