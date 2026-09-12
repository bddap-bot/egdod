#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

MODE=${EGDOD_BOOT_LINK:-wired}
case "$MODE" in wired|station|ap) ;; *) echo "EGDOD_BOOT_LINK must be wired, station or ap" >&2; exit 2;; esac

WORK=$(mktemp -d "${TMPDIR:-/tmp}/egdod-boot.XXXXXX")
SERIAL=$WORK/serial.log
STATE=$WORK/controller
PIDS=()
cleanup() {
  for p in "${PIDS[@]:-}"; do kill "$p" 2>/dev/null || true; done
  [ -s "$SERIAL" ] && save_serial
  rm -rf "$WORK"
}
trap cleanup EXIT

say() { printf '\n=== %s\n' "$*"; }
clean_serial() { sed 's/\x1b\[[0-9;]*[a-zA-Z]//g; s/\x1b[=>]//g' "$SERIAL" | tr -d '\r'; }
save_serial() {
  if [ -n "${BOTQ_ARTIFACTS_DIR:-}" ]; then
    clean_serial > "$BOTQ_ARTIFACTS_DIR/boot-$MODE-serial.log"
    echo "serial log saved to $BOTQ_ARTIFACTS_DIR/boot-$MODE-serial.log"
  fi
}

say "building qemu + OVMF (Microsoft keys) and the native controller"
QEMU=$(nix-build --no-out-link nixpkgs.nix -A qemu)/bin/qemu-system-x86_64
OVMF=$(nix-build --no-out-link nixpkgs.nix -A OVMFFull.fd)/FV
CODE=$OVMF/OVMF_CODE.ms.fd
VARS_SRC=$OVMF/OVMF_VARS.ms.fd
[ -r "$CODE" ] && [ -r "$VARS_SRC" ] || { echo "OVMF Microsoft-key firmware not found" >&2; exit 1; }
BIN=$(nix-shell --run 'cargo build --release -q >&2 && echo "${CARGO_TARGET_DIR:-target}/release/egdod"')
BIN=$(readlink -f "$BIN")

say "controller init"
"$BIN" controller --state-dir "$STATE" init >"$WORK/init.out"
NODE_ID=$(grep -oE '[0-9a-f]{64}' "$WORK/init.out" | head -1)
echo "controller node id: $NODE_ID"
PORT=${EGDOD_BOOT_PORT:-52847}
if [ "$MODE" = ap ]; then
  PORT=$("$BIN" link "$NODE_ID" | sed -n 's/^port=//p')
  echo "derived link:"; "$BIN" link "$NODE_ID"
fi

boot() {
  for attempt in 1 2 3; do
    start_vm "$@"
    if wait_serial "EFI stub: UEFI Secure Boot is enabled" 30 >/dev/null; then return 0; fi
    kill "$QEMU_PID" 2>/dev/null || true
    wait "$QEMU_PID" 2>/dev/null || true
    echo "firmware attempt $attempt did not reach the kernel: $(clean_serial | grep -aoE "BdsDxe: [^\r]*" | tail -1)"
  done
  echo "DEFECT: the firmware never started the kernel" >&2; exit 1
}

start_vm() {
  cp "$VARS_SRC" "$WORK/vars.fd"; chmod +w "$WORK/vars.fd"
  : > "$SERIAL"
  say "booting under qemu/OVMF with Secure Boot ON"
  "$QEMU" \
    -machine q35,accel=kvm:tcg -cpu max -m 4096 \
    -drive if=pflash,format=raw,readonly=on,file="$CODE" \
    -drive if=pflash,format=raw,file="$WORK/vars.fd" \
    -drive format=raw,file="$1",if=ide,snapshot=on \
    "${@:2}" \
    -display none -vga none -serial "file:$SERIAL" -no-reboot >"$WORK/qemu.log" 2>&1 &
  QEMU_PID=$!
  PIDS+=("$QEMU_PID")
}

wait_serial() {
  local pattern=$1 tries=$2 line
  for _ in $(seq 1 "$tries"); do
    kill -0 "$QEMU_PID" 2>/dev/null || { echo "qemu exited early" >&2; clean_serial | tail -40 >&2; return 1; }
    line=$(clean_serial 2>/dev/null | grep -aE "$pattern" | head -1 || true)
    [ -n "$line" ] && { printf '%s\n' "$line"; return 0; }
    sleep 2
  done
  return 1
}

if [ "$MODE" != wired ]; then
  say "building the stick image dialing the rig's controller inside the VM (10.99.0.1:$PORT)"
  IMG=$(nix-build --no-out-link boot/image.nix --argstr controllerNodeId "$NODE_ID" --argstr direct "10.99.0.1:$PORT")
  cat "$IMG/sizes.txt"

  say "baking the hwsim proof rig into a copy of the image"
  OV=$WORK/overlay
  mkdir -p "$OV/bin" "$OV/rig" "$OV/rig-state"
  cp boot/rig.sh "$OV/init"
  zcat "$IMG/initrd.img" | ( cd "$OV" && cpio -i --quiet --to-stdout init ) > "$OV/init.egdod"
  chmod +x "$OV/init" "$OV/init.egdod"
  cp "$(nix-build --no-out-link nixpkgs.nix -A pkgsStatic.iw)/bin/iw" "$OV/bin/iw"
  cp "$(nix-build --no-out-link nixpkgs.nix -A pkgsStatic.hostapd)/bin/hostapd" "$OV/bin/hostapd"
  echo "$PORT" > "$OV/rig/port"
  echo "$MODE" > "$OV/rig/mode"
  echo egdod-proof > "$OV/rig/proof-ssid"
  echo proof-network-psk > "$OV/rig/proof-psk"
  cp "$STATE/controller.key" "$OV/rig-state/controller.key"
  boot/networks.sh --no-host --network "$(cat "$OV/rig/proof-ssid")" "$(cat "$OV/rig/proof-psk")" > "$OV/rig/proof.conf"
  if [ "$MODE" = station ]; then
    cp "$OV/rig/proof.conf" "$OV/wpa_supplicant.conf"
    echo "--- baked networks:"; cat "$OV/wpa_supplicant.conf"
  fi
  boot/bake.sh "$IMG/esp.img" "$WORK/esp.img" "$OV"

  boot "$WORK/esp.img" -nic none
  say "watching the rig (no wired device in this VM; two hwsim radios in the rig's netns, one with the target)"
  if ! wait_serial "RIG: ($MODE OK|DEFECT)" 150; then
    echo "DEFECT: the rig reached no verdict" >&2; clean_serial | tail -40 >&2; save_serial; exit 1
  fi
  say "rig and init lines from the serial console:"
  clean_serial | grep -aE "^(RIG:|init:|agent pubkey)" | head -40
  save_serial
  clean_serial | grep -aq "RIG: $MODE OK" || { echo "DEFECT on the serial console" >&2; exit 1; }
  echo
  echo "BOOT OK ($MODE): Secure Boot on, no wired device, the target reached the controller over wireless and was served as root."
  exit 0
fi

z32() {
  local hex=$1 alpha=ybndrfg8ejkmcpqxot1uwisza345h769 out="" acc=0 bits=0 i
  for ((i = 0; i < ${#hex}; i += 2)); do
    acc=$(( (acc << 8) | 16#${hex:i:2} )); bits=$((bits + 8))
    while ((bits >= 5)); do bits=$((bits - 5)); out+=${alpha:$(( (acc >> bits) & 31 )):1}; acc=$(( acc & ((1 << bits) - 1) )); done
  done
  ((bits > 0)) && out+=${alpha:$(( (acc << (5 - bits)) & 31 )):1}
  printf '%s\n' "$out"
}

DIAL_ARGS=(--argstr direct "10.0.2.2:$PORT")
if [ "${EGDOD_BOOT_RELAY:-}" = 1 ]; then
  say "controller serve on the host through the public relay, discoverable by node id; bound to loopback so the only way in is the relay (no --direct in the image)"
  "$BIN" controller --state-dir "$STATE" serve --bind "127.0.0.1:$PORT" --probe-interval 10 --probe-timeout 5 >"$WORK/serve.log" 2>&1 &
  DIAL_ARGS=()
else
  say "controller serve on the host (no relay, bound to 0.0.0.0:$PORT)"
  "$BIN" controller --state-dir "$STATE" serve --no-relay --bind "0.0.0.0:$PORT" \
    --probe-interval 10 --probe-timeout 5 >"$WORK/serve.log" 2>&1 &
fi
PIDS+=($!)
sleep 2

say "building the stick image for this controller (Secure Boot chain, pinned)"
NET_ARGS=()
if [ "${EGDOD_BOOT_STATIC:-}" = 1 ]; then NET_ARGS=(--argstr ip 10.0.2.15 --argstr gw 10.0.2.2); echo "network: static"; else echo "network: DHCP"; fi
IMG=$(nix-build --no-out-link boot/image.nix \
  --argstr controllerNodeId "$NODE_ID" "${DIAL_ARGS[@]}" "${NET_ARGS[@]}")
echo "image: $IMG/esp.img"
cat "$IMG/sizes.txt"

NIC=${EGDOD_BOOT_NIC:-e1000}
echo "NIC model: $NIC"
boot "$IMG/esp.img" -netdev user,id=n0 -device "$NIC,netdev=n0"

say "waiting for the agent to boot and print its public key"
AGENT=$(wait_serial '^agent pubkey: [0-9a-f]+' 120 | sed 's/^agent pubkey: //') \
  || { echo "no agent pubkey on serial within timeout" >&2; clean_serial | tail -40 >&2; exit 1; }
echo "agent pubkey seen on serial: $AGENT"

if [ "${EGDOD_BOOT_RELAY:-}" = 1 ]; then
  say "the published record, resolved from this host"
  host -t TXT "_iroh.$(z32 "$NODE_ID").dns.iroh.link" || true
fi

say "before approval: the controller serves the agent nothing"
for _ in $(seq 1 30); do
  "$BIN" controller --state-dir "$STATE" pending --json 2>/dev/null | grep -q "$AGENT" && break
  sleep 2
done
"$BIN" controller --state-dir "$STATE" pending --json 2>/dev/null | grep -q "$AGENT" \
  || { echo "DEFECT: agent connected but never appeared as pending" >&2; exit 1; }
echo "agent is connected and held pending"
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
echo "--- SecureBoot EFI variable, read from inside the guest firmware (attributes then value; 01 = on):"
"$BIN" controller --state-dir "$STATE" exec "$AGENT" -- /bin/busybox od -An -tx1 /sys/firmware/efi/efivars/SecureBoot-8be4df61-93ca-11d2-aa0d-00e098032b8c >"$WORK/sb" 2>/dev/null || true
cat "$WORK/sb"
SBVAL=$(tr -d ' \n' < "$WORK/sb" | tail -c2)
[ "$SBVAL" = 01 ] || { echo "DEFECT: SecureBoot value is '$SBVAL', not 01 — Secure Boot not enforcing" >&2; exit 1; }
echo "SecureBoot value byte is 01: firmware reports Secure Boot on"
say "the link and the dial, as the target reported them on its console:"
clean_serial | grep -aE "^(init: |egdod: |udhcpc: lease)" | head -12
if [ "${EGDOD_BOOT_RELAY:-}" = 1 ]; then
  clean_serial | grep -aq "connected to controller via relayed" \
    || { echo "DEFECT: no relayed dial was observed on the console" >&2; exit 1; }
  echo "relayed dial observed on the target console"
fi

say "receive a root filesystem over the wire and switch_root into it"
STATIC_CC=$(nix-build --no-out-link nixpkgs.nix -A pkgsStatic.stdenv.cc)/bin/x86_64-unknown-linux-musl-gcc
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
wait_serial "NEWROOT-INIT: switch_root landed" 30 >/dev/null \
  || { echo "DEFECT: switch_root marker never appeared on serial" >&2; clean_serial | tail -20 >&2; exit 1; }
echo "switch_root confirmed on serial:"
clean_serial | grep -a "NEWROOT-INIT" | head -4

say "controller serve log (path label, agent connect):"
grep -aE 'routed a session|agent connected|reachability|UNDIALABLE' "$WORK/serve.log" | tail -8 || true

say "Secure Boot markers from the guest kernel (serial console):"
clean_serial | grep -aE "Secure Boot is enabled|locked down from EFI|secureboot: Secure boot|Debian Secure Boot CA|e1000 .* eth0|init: |agent pubkey|switch_root|NEWROOT-INIT" | head -14

save_serial
echo
echo "BOOT OK: Secure Boot on, agent dialed out, controller approved and ran as root in the VM."
