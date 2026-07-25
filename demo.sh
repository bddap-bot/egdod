#!/usr/bin/env bash
# Stands up a controller and an agent on this one machine and exercises
# everything egdod v0 claims: approval, the three primitives, the ssh recipe,
# and the reachability canary. Unattended, hermetic (no relay, no DNS, no
# network beyond loopback), and it cleans up after itself.
#
# Needs: bash, cargo (or a prebuilt binary in EGDOD_BIN), sshd, ssh, ssh-keygen,
# sha256sum, head. Runs unprivileged; if passwordless sudo happens to be
# available it additionally proves exec-as-root with a second agent.
set -euo pipefail

cd "$(dirname "$0")"
BIN=${EGDOD_BIN:-${CARGO_TARGET_DIR:-target}/debug/egdod}
[ -x "$BIN" ] || cargo build
BIN=$(readlink -f "$BIN")

DEMO=$(mktemp -d "${TMPDIR:-/tmp}/egdod-demo.XXXXXX")
STATE=$DEMO/controller
SSHD=$(command -v sshd)
SSHD_PORT=${EGDOD_DEMO_SSHD_PORT:-2299}
PIDS=()

cleanup() {
  for p in "${PIDS[@]:-}"; do [ -n "$p" ] && kill "$p" 2>/dev/null || true; done
  [ -f "$DEMO/sshd/sshd.pid" ] && kill "$(cat "$DEMO/sshd/sshd.pid")" 2>/dev/null || true
  # Backstop: anything still running that was started against this run's unique
  # scratch directory, including processes reparented to init.
  pkill -f "$DEMO" 2>/dev/null || true
  sudo -n pkill -f "$DEMO" 2>/dev/null || true
  rm -rf "$DEMO"
}
trap cleanup EXIT

say() { printf '\n=== %s\n' "$*"; }
ctl() { "$BIN" controller --state-dir "$STATE" "$@"; }

# Bounded waits only: an unattended demo must fail rather than hang.
wait_for() { # wait_for <seconds> <shell-condition...>
  local limit=$1; shift
  for _ in $(seq $((limit * 4))); do
    if eval "$@"; then return 0; fi
    sleep 0.25
  done
  echo "TIMEOUT waiting for: $*" >&2
  return 1
}

say "0. versions"
"$BIN" --version
ssh -V 2>&1 | head -1

say "1. controller init — the node id is the only thing an image needs, and it is public"
NODE_ID=$(ctl init)
echo "node id: $NODE_ID"
ls -l "$STATE"

say "2. controller serve (hermetic: --no-relay, so nothing leaves this machine)"
# The UDP port is pinned so the agent can be given a fixed --direct address that
# survives the controller restart in step 11 — and so it needs no DNS at all.
DIRECT=127.0.0.1:${EGDOD_DEMO_PORT:-45777}
serve_up() {
  # Started directly rather than through the ctl() wrapper: a backgrounded shell
  # function leaves $! pointing at a subshell, and step 11 needs to kill the real
  # controller.
  "$BIN" controller --state-dir "$STATE" serve --no-relay --bind "$DIRECT" \
    --probe-interval 10 --probe-timeout 5 >>"$DEMO/serve.log" 2>&1 &
  SERVE_PID=$!
  PIDS+=("$SERVE_PID")
  wait_for 30 "[ -S '$STATE/control.sock' ] && [ -s '$STATE/status.json' ]"
}
serve_up
echo "controller direct address: $DIRECT"

say "3. reachability canary — the controller dials its own node id from a fresh key"
wait_for 30 "grep -q '\"dialable\": true' '$STATE/status.json'"
ctl status

say "4. agent starts, dials out, and is held pending (nothing is served to it)"
"$BIN" agent --controller "$NODE_ID" --key-file "$DEMO/agent.key" \
  --no-relay --direct "$DIRECT" >"$DEMO/agent.log" 2>&1 &
PIDS+=($!)
wait_for 30 "grep -q pubkey '$DEMO/pending.json' 2>/dev/null || ctl pending --json > '$DEMO/pending.json'; grep -q pubkey '$DEMO/pending.json'"
ctl pending
AGENT=$(grep -o '"pubkey": "[0-9a-f]*"' "$DEMO/pending.json" | head -1 | sed 's/.*"\([0-9a-f]*\)"$/\1/')
echo "agent pubkey: $AGENT"

say "5. an unapproved agent gets nothing"
if ctl exec "$AGENT" -- /bin/sh -c 'echo should-not-run' >"$DEMO/refused.out" 2>&1; then
  echo "DEFECT: an unapproved agent was served" >&2
  exit 1
fi
cat "$DEMO/refused.out"
if grep -q should-not-run "$DEMO/refused.out"; then
  echo "DEFECT: the command ran anyway" >&2
  exit 1
fi
echo "refused, as required — the controller holds no session for an unapproved key"

say "6. approve by public key (scriptable), and watch the session appear"
ctl approve "$AGENT"
cat "$STATE/approved"
wait_for 30 "ctl status --json | grep -q '$AGENT'"
ctl status

say "7. PRIMITIVE exec — stdout and stderr stay separate, exit status comes back"
set +e
ctl exec "$AGENT" -- /bin/sh -c 'echo to-stdout; echo to-stderr >&2; exit 7' \
  >"$DEMO/exec.out" 2>"$DEMO/exec.err"
code=$?
set -e
echo "exit status: $code (expected 7)"
echo "stdout file: $(cat "$DEMO/exec.out")"
echo "stderr file: $(grep to-stderr "$DEMO/exec.err")"
[ "$code" = 7 ] || { echo "DEFECT: wrong exit status" >&2; exit 1; }
grep -qx to-stdout "$DEMO/exec.out"
grep -qx to-stderr "$DEMO/exec.err"

say "8. PRIMITIVE copy — 64 MiB up, verified on the target, then back down"
head -c 67108864 /dev/urandom >"$DEMO/up.bin"
chmod 640 "$DEMO/up.bin"
AGENT_PID=$(pgrep -n -f "$BIN agent --controller" || true)
ctl push "$AGENT" "$DEMO/up.bin" "$DEMO/target/up.bin"
ctl exec "$AGENT" -- sha256sum "$DEMO/target/up.bin"
ctl pull "$AGENT" "$DEMO/target/up.bin" "$DEMO/down.bin"
sha256sum "$DEMO/up.bin" "$DEMO/down.bin"
[ "$(sha256sum <"$DEMO/up.bin")" = "$(sha256sum <"$DEMO/down.bin")" ] \
  || { echo "DEFECT: round trip corrupted the file" >&2; exit 1; }
echo "mode on the target: $(stat -c %a "$DEMO/target/up.bin") (source was 640)"
[ "$(stat -c %a "$DEMO/target/up.bin")" = 640 ]
# A streaming copy must not have cost the agent 64 MiB of RAM.
[ -n "$AGENT_PID" ] && echo "agent peak RSS after a 64 MiB round trip: $(grep VmHWM "/proc/$AGENT_PID/status")"

say "9. RECIPE ssh — host key generated and fetched over the egdod channel"
mkdir -p "$DEMO/sshd"
cat >"$DEMO/sshd/sshd_config" <<EOF
Port $SSHD_PORT
ListenAddress 127.0.0.1
HostKey $DEMO/sshd/ssh_host_ed25519_key
AuthorizedKeysFile $DEMO/target-authorized_keys
PidFile $DEMO/sshd/sshd.pid
StrictModes no
UsePAM no
PasswordAuthentication no
KbdInteractiveAuthentication no
EOF
# Nothing here is pre-created: the recipe generates the host key with `exec`,
# installs the authorized key with `copy`, starts sshd with `exec`, and reaches
# it through `forward`. Note StrictHostKeyChecking=yes + BatchMode=yes inside.
ctl ssh "$AGENT" \
  --user "$(id -un)" \
  --authorized-keys "$DEMO/target-authorized_keys" \
  --host-key "$DEMO/sshd/ssh_host_ed25519_key.pub" \
  --keygen-arg=ssh-keygen --keygen-arg=-q --keygen-arg=-t --keygen-arg=ed25519 \
  --keygen-arg=-N --keygen-arg="" --keygen-arg=-f \
  --keygen-arg="$DEMO/sshd/ssh_host_ed25519_key" \
  --sshd-arg="$SSHD" --sshd-arg=-f --sshd-arg="$DEMO/sshd/sshd_config" \
  --target-addr "127.0.0.1:$SSHD_PORT" \
  -- hostname
echo "known_hosts the controller wrote:"
cat "$STATE/ssh/known_hosts"

say "10. PRIMITIVE forward — a local port onto the target's sshd, proven by its banner"
"$BIN" controller --state-dir "$STATE" forward "$AGENT" 0 "127.0.0.1:$SSHD_PORT" >"$DEMO/fwd.log" 2>&1 &
PIDS+=($!)
wait_for 20 "grep -q forwarding '$DEMO/fwd.log'"
FWD=$(sed -n 's/^forwarding 127.0.0.1:\([0-9]*\).*/\1/p' "$DEMO/fwd.log" | head -1)
cat "$DEMO/fwd.log"
exec 3<>"/dev/tcp/127.0.0.1/$FWD"
head -1 <&3
exec 3<&-

say "11. approval survives a controller restart, and the agent redials on its own"
kill "$SERVE_PID"; wait "$SERVE_PID" 2>/dev/null || true
# A killed controller leaves its socket and status file behind; clearing them is
# how this script can tell the new controller apart from the dead one's remains.
rm -f "$STATE/control.sock" "$STATE/status.json"
serve_up
wait_for 60 "ctl status --json | grep -q '$AGENT'"
echo "reconnected without being approved again:"
ctl exec "$AGENT" -- /bin/sh -c 'echo still-here'

say "12. exec as root (only if this machine offers passwordless sudo)"
if sudo -n true 2>/dev/null; then
  sudo -n "$BIN" agent --controller "$NODE_ID" --key-file "$DEMO/root-agent.key" \
    --no-relay --direct "$DIRECT" >"$DEMO/root-agent.log" 2>&1 &
  PIDS+=($!)
  wait_for 30 "grep -q 'agent pubkey' '$DEMO/root-agent.log'"
  ROOT_AGENT=$(sed -n 's/^agent pubkey: //p' "$DEMO/root-agent.log" | head -1)
  ctl approve "$ROOT_AGENT"
  wait_for 30 "ctl status --json | grep -q '$ROOT_AGENT'"
  echo "uid seen by exec: $(ctl exec "$ROOT_AGENT" -- id -u)"
  # Reads a root-only file without printing any of it.
  ctl exec "$ROOT_AGENT" -- head -c 0 /etc/shadow && echo "/etc/shadow is readable: exec really is root"
  sudo -n kill "$(pgrep -f "$BIN agent --controller $NODE_ID --key-file $DEMO/root-agent.key")" 2>/dev/null || true
else
  echo "skipped: no passwordless sudo here"
fi

say "12b. what the controller does when it CANNOT be dialled"
# Forced, not simulated away: a zero-length probe deadline makes every canary
# fail, which is the same code path as a controller that has published itself
# and is nevertheless unreachable. Separate state dir so the real one is untouched.
UND=$DEMO/undialable
"$BIN" controller --state-dir "$UND" init >/dev/null
"$BIN" controller --state-dir "$UND" serve --no-relay --bind 127.0.0.1:0 \
  --probe-interval 1 --probe-timeout 0 --restart-after 2 >"$DEMO/undialable.log" 2>&1 &
PIDS+=($!)
wait_for 60 "[ -s '$UND/status.json' ] && grep -q '\"endpoint_restarts\": 1' '$UND/status.json'"
grep -m3 -E "UNDIALABLE|rebuilding" "$DEMO/undialable.log"
"$BIN" controller --state-dir "$UND" status
kill "${PIDS[-1]}" 2>/dev/null || true

say "13. final controller status"
ctl status --json

say "14. controller log (paths reported for every session, canary results)"
grep -E 'agent connected|routed a session|reachability|UNDIALABLE|pending' "$DEMO/serve.log" | tail -12

# Everything above is hermetic. This last phase is the real-world configuration —
# public relay, address lookup over DNS, the agent dialling nothing but a node id —
# and therefore needs working internet, so it is opt-in.
if [ "${EGDOD_DEMO_RELAY:-0}" = 1 ]; then
  say "15. relay + discovery: the agent knows only the node id"
  R=$DEMO/relay
  RSTATE=$R/controller
  mkdir -p "$R"
  RNODE=$("$BIN" controller --state-dir "$RSTATE" init)
  "$BIN" controller --state-dir "$RSTATE" serve --probe-interval 15 --probe-timeout 20 \
    >"$R/serve.log" 2>&1 &
  PIDS+=($!)
  wait_for 60 "[ -S '$RSTATE/control.sock' ]"
  wait_for 120 "[ -s '$RSTATE/status.json' ] && grep -q '\"dialable\": true' '$RSTATE/status.json'"
  echo "canary in discovery mode (dialled its own node id through n0 DNS + relay):"
  grep -E '"probe_mode"|"dialable"|"last_probe_path"' "$RSTATE/status.json"
  "$BIN" agent --controller "$RNODE" --key-file "$R/agent.key" >"$R/agent.log" 2>&1 &
  PIDS+=($!)
  wait_for 60 "grep -q 'agent pubkey' '$R/agent.log'"
  RAGENT=$(sed -n 's/^agent pubkey: //p' "$R/agent.log" | head -1)
  "$BIN" controller --state-dir "$RSTATE" approve "$RAGENT"
  wait_for 90 "'$BIN' controller --state-dir '$RSTATE' status --json | grep -q '$RAGENT'"
  "$BIN" controller --state-dir "$RSTATE" exec "$RAGENT" -- /bin/sh -c 'echo hello-over-the-internet'
  "$BIN" controller --state-dir "$RSTATE" status
fi

say "demo complete: all primitives, the recipe, and the canary exercised"
