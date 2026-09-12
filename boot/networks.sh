#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "usage: networks.sh [--no-host] [--network SSID PSK]... > wpa_supplicant.conf" >&2
  echo "  emits the networks a stick joins at boot: the network this host is on now" >&2
  echo "  (its NetworkManager or wpa_supplicant profile, read as root), plus each --network." >&2
  echo "  --no-host with no --network emits nothing: a wired-only stick." >&2
  exit 2
}

HOST=1
SSIDS=()
PSKS=()
while [ $# -gt 0 ]; do
  case "$1" in
    --no-host) HOST=0; shift ;;
    --network) [ $# -ge 3 ] || usage; SSIDS+=("$2"); PSKS+=("$3"); shift 3 ;;
    *) usage ;;
  esac
done

as_root() { if [ "$(id -u)" = 0 ]; then "$@"; else sudo -n "$@"; fi; }

host_networks() {
  if command -v nmcli >/dev/null; then
    nmcli -t -f TYPE,NAME connection show --active 2>/dev/null | while IFS=: read -r type name; do
      [ "$type" = 802-11-wireless ] || continue
      name=$(printf '%s' "$name" | sed 's/\\:/:/g')
      ssid=$(as_root nmcli -s -g 802-11-wireless.ssid connection show "$name" 2>/dev/null) || true
      psk=$(as_root nmcli -s -g 802-11-wireless-security.psk connection show "$name" 2>/dev/null) || true
      if [ -n "$ssid" ] && [ -n "$psk" ]; then printf '%s\n%s\n' "$ssid" "$psk"; fi
    done
  fi
  if command -v wpa_cli >/dev/null; then
    for iface in $(ls /sys/class/net 2>/dev/null); do
      [ -e "/sys/class/net/$iface/phy80211" ] || continue
      ssid=$(as_root wpa_cli -i "$iface" status 2>/dev/null | sed -n 's/^ssid=//p' | head -1) || true
      [ -n "$ssid" ] || continue
      for conf in /etc/wpa_supplicant.conf /etc/wpa_supplicant/*.conf; do
        [ -r "$conf" ] || as_root test -r "$conf" || continue
        as_root awk -v want="$ssid" '
          /^[ \t]*network=\{/ { inblk=1; s=""; p=""; next }
          inblk && /^[ \t]*ssid=/ { s=$0; sub(/^[ \t]*ssid=/, "", s); gsub(/^"|"$/, "", s) }
          inblk && /^[ \t]*psk=/ { p=$0; sub(/^[ \t]*psk=/, "", p); gsub(/^"|"$/, "", p) }
          inblk && /^[ \t]*\}/ { if (s == want && p != "") { print s; print p; exit } inblk=0 }
        ' "$conf"
      done
    done
  fi
}

emit() {
  local ssid=$1 psk=$2
  case "$ssid$psk" in *'"'*|*'\'*|*$'\n'*) echo "refuse: a quote, backslash or newline in the credentials for $ssid" >&2; exit 1;; esac
  printf 'network={\n\tssid=%s\n' "$(printf '%s' "$ssid" | od -An -tx1 | tr -d ' \n')"
  if [[ "$psk" =~ ^[0-9a-fA-F]{64}$ ]]; then printf '\tpsk=%s\n' "$psk"; else printf '\tpsk="%s"\n' "$psk"; fi
  printf '\tkey_mgmt=WPA-PSK\n}\n'
}

N=0
if [ "$HOST" = 1 ]; then
  while IFS= read -r ssid && IFS= read -r psk; do emit "$ssid" "$psk"; N=$((N + 1)); done < <(host_networks)
fi
for i in "${!SSIDS[@]}"; do emit "${SSIDS[$i]}" "${PSKS[$i]}"; N=$((N + 1)); done
[ "$N" -gt 0 ] || [ "$HOST" = 0 ] \
  || { echo "refuse: nothing to bake — this host is not on wireless; give --network SSID PSK, or --no-host for a wired-only stick" >&2; exit 1; }
echo "baked $N network(s)" >&2
