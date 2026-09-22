#!/bin/busybox sh
set -u
/bin/busybox --install -s /bin
PORT=$(cat /rig/port)
MODE=$(cat /rig/mode)
SSID=$(cat /rig/proof-ssid)
say() { echo "RIG: $*"; }
ctl() { /egdod controller --state-dir /rig-state "$@"; }

if [ "${1:-}" != ns ]; then
  mkdir -p /proc /sys /dev
  mount -t proc proc /proc
  mount -t sysfs sysfs /sys
  mount -t devtmpfs devtmpfs /dev
  modprobe mac80211_hwsim radios=3
  for _ in 1 2 3 4 5 6 7 8 9 10; do [ "$(ls /sys/class/ieee80211 2>/dev/null | wc -l)" -ge 3 ] && break; sleep 1; done
  PHYS=""
  for p in /sys/class/ieee80211/*; do
    case "$(readlink -f "$p/device")" in *mac80211_hwsim*) PHYS="$PHYS $(basename "$p")";; esac
  done
  set -- $PHYS
  [ $# -ge 3 ] || { say "DEFECT: only $# hwsim radios came up"; exit 1; }
  if [ "$MODE" = ble ]; then
    modprobe virtio_console
    modprobe hci_uart
    for _ in $(seq 1 20); do [ -e /dev/hvc0 ] && break; sleep 0.5; done
    btattach -N -B /dev/hvc0 -P h4 > /rig/btattach.log 2>&1 &
    for _ in $(seq 1 20); do [ -e /sys/class/bluetooth/hci0 ] && break; sleep 0.5; done
    [ -e /sys/class/bluetooth/hci0 ] || { say "DEFECT: btattach brought up no controller: $(tr '\n' ' ' < /rig/btattach.log)"; exit 1; }
    sleep 3
    btmon -w /rig/btmon.snoop > /rig/btmon.txt 2>&1 &
    say "bluetooth: $(ls /sys/class/bluetooth 2>/dev/null | tr '\n' ' ')from the virtio-serial controller$(dmesg | grep -a 'Bluetooth: hci' | tail -2 | sed 's/.*Bluetooth: /; /' | tr '\n' ' '); hwsim radios: $*, all in the rig's netns until credentials arrive"
  else
    say "hwsim radios: $*; $1 stays with the target, the rest go to the rig's netns"
    shift
  fi
  unshare -m -n sh /init ns &
  NS=$!
  for _ in $(seq 1 50); do [ -e /rig/ns-ready ] && break; sleep 0.1; done
  for p in "$@"; do iw phy "$p" set netns "$NS"; done
  ip link set hwsim0 netns "$NS"
  say "handing over to the stick's own init"
  exec /init.egdod
fi

mount -t sysfs sysfs /sys
touch /rig/ns-ready
ip link set lo up
for _ in 1 2 3 4 5 6 7 8 9 10; do [ "$(ls /sys/class/net | grep -c '^wlan')" -ge 2 ] && break; sleep 1; done
set -- $(ls /sys/class/net | grep '^wlan' | sort)
[ $# -ge 2 ] || { say "DEFECT: only $# radios reached the rig netns"; exit 1; }
AP=$1
JOIN=$2
say "netns radios: ap=$AP join=$JOIN"

cat > /rig/hostapd.conf <<EOF
interface=$AP
driver=nl80211
ssid=$SSID
hw_mode=g
channel=6
wpa=2
wpa_key_mgmt=WPA-PSK
rsn_pairwise=CCMP
wpa_passphrase=$(cat /rig/proof-psk)
EOF
cat > /rig/udhcpd.conf <<EOF
start 10.99.0.10
end 10.99.0.50
interface $AP
lease_file /rig/leases
option subnet 255.255.255.0
option router 10.99.0.1
EOF
hostapd /rig/hostapd.conf > /rig/hostapd.log 2>&1 &
for _ in $(seq 1 20); do grep -q "AP-ENABLED" /rig/hostapd.log && break; sleep 1; done
say "hostapd: $(grep -aE 'AP-ENABLED|Could not|failed' /rig/hostapd.log | tail -2 | tr '\n' ' ')"
ip addr add 10.99.0.1/24 dev "$AP"
ip link set "$AP" up
touch /rig/leases
udhcpd -f /rig/udhcpd.conf > /rig/udhcpd.log 2>&1 &
ctl serve --no-relay --bind "0.0.0.0:$PORT" --probe-interval 10 --probe-timeout 5 > /rig/serve.log 2>&1 &
say "proof network $SSID up on $AP, controller serving on 0.0.0.0:$PORT"

wait_pending() {
  for _ in $(seq 1 90); do
    AGENT=$(ctl pending --json 2>/dev/null | sed -n 's/.*"\([0-9a-f]\{64\}\)".*/\1/p' | head -1)
    [ -n "$AGENT" ] && return 0
    sleep 2
  done
  say "DEFECT: no agent dialed in over $1"
  tail -5 /rig/hostapd.log
  tail -5 /rig/join.log 2>/dev/null
  tail -10 /rig/serve.log
  return 1
}
prove_session() {
  for _ in $(seq 1 30); do
    if ctl exec "$AGENT" -- /bin/busybox id -u > /rig/uid 2>/dev/null; then break; fi
    sleep 2
  done
  [ "$(cat /rig/uid)" = 0 ] || { say "DEFECT: exec uid is '$(cat /rig/uid)', not 0"; return 1; }
  say "exec as root over $1: uid 0"
  say "target addresses: $(ctl exec "$AGENT" -- /bin/busybox ip -4 -o addr 2>/dev/null | tr -s ' ' | cut -d' ' -f2,4 | tr '\n' ' ')"
  say "session path: $(grep -a 'routed a session' /rig/serve.log | tail -1 | sed 's/.*egdod::controller: //')"
}

case "$MODE" in
  ap)
    ctl join --iface "$JOIN" > /rig/join.log 2>&1 &
    LINK="the target's access point";;
  ble)
    for _ in $(seq 1 200); do [ -s /wpa_supplicant.conf ] && break; sleep 1; done
    [ -s /wpa_supplicant.conf ] || {
      say "DEFECT: no credentials arrived over BLE"
      grep -aE 'Connection Complete|Disconnect|Reason:|ATT: |SMP: |L2CAP|Status:' /rig/btmon.txt | grep -av 'Status: Success' | tail -40 | sed 's/^/RIG-BTMON: /'
      exit 1
    }
    say "credentials landed over BLE (mode $(stat -c %a /wpa_supplicant.conf)); handing $JOIN to the target"
    iw phy "$(cat "/sys/class/net/$JOIN/phy80211/name")" set netns 1
    LINK="the network received over BLE";;
  *)
    LINK="the baked network";;
esac
wait_pending "$LINK" || exit 1
say "agent pending over $LINK: $AGENT"
if ctl exec "$AGENT" -- /bin/busybox true >/dev/null 2>&1; then say "DEFECT: unapproved agent served"; exit 1; fi
say "unapproved exec refused"
ctl approve "$AGENT"
prove_session "$LINK" || exit 1

if [ "$MODE" = ap ]; then
  say "join: $(grep -a 'joined' /rig/join.log | head -1)"
  say "pushing credentials for $SSID as the first approved command"
  ctl push "$AGENT" /rig/proof.conf /wpa_supplicant.conf || { say "DEFECT: push failed"; exit 1; }
  sleep 5
  prove_session "the credentialed network" || exit 1
fi
if [ "$MODE" = ap ] || [ "$MODE" = ble ]; then
  case "$(ctl exec "$AGENT" -- /bin/busybox ip -4 -o addr 2>/dev/null)" in
    *10.99.0.*) say "target holds a 10.99.0.0/24 lease on $SSID";;
    *) say "DEFECT: target is not on the credentialed network"; exit 1;;
  esac
fi
say "$MODE OK"
