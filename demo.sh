#!/usr/bin/env bash
# egdod v0 demo: one machine, both roles, everything exercised unattended.
# Run inside the project's nix-shell:  nix-shell --run './demo.sh'
#
# What it proves, in order:
#   direct-local session with path reporting
#   approve-before-anything (unapproved agent gets NOTHING)
#   approval survives a controller restart
#   exec (stdout/stderr separate, remote exit status)
#   copy both directions, 64 MiB streamed, sha256 + mode verified
#   forward a TCP port (real bytes through the tunnel)
#   the ssh recipe: first connect under StrictHostKeyChecking=yes BatchMode=yes
#   a relayed/discovery session (bare NodeId, no hints) with path reporting
#   controller self-reachability status
set -euo pipefail

DEMO_DIR="${EGDOD_DEMO_DIR:-$(pwd)/demo-state}"
CTL="$DEMO_DIR/ctl"
AGT="$DEMO_DIR/agt"
TARGET_DIR="${CARGO_TARGET_DIR:-$(pwd)/target}"
BIN="$TARGET_DIR/debug/egdod"
SSH_PATH="/run/current-system/sw/bin:/usr/bin:/bin"

say()  { printf '\n=== %s ===\n' "$*"; }
fail() { printf 'DEMO FAIL: %s\n' "$*" >&2; exit 1; }

pkill_demo() { sudo pkill -f "egdod agent" 2>/dev/null || true; pkill -f "egdod controller serve" 2>/dev/null || true; }
trap pkill_demo EXIT
pkill_demo
rm -rf "$DEMO_DIR"
mkdir -p "$CTL" "$AGT"

say "build (debug)"
cargo build 2>&1 | tail -1
[ -x "$BIN" ] || fail "no binary at $BIN"

say "controller init"
NODE_ID="$("$BIN" controller init --state-dir "$CTL")"
echo "controller NodeId: $NODE_ID"
[ -f "$CTL/controller.key" ] || fail "controller key not created"

say "serve (controller)"
"$BIN" controller serve --state-dir "$CTL" >"$DEMO_DIR/serve.log" 2>&1 &
PORT=""
for _ in $(seq 1 30); do
  PORT="$(grep 'direct addr' "$DEMO_DIR/serve.log" 2>/dev/null \
    | grep -m1 -oE '[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+:[0-9]+' | cut -d: -f2 || true)"
  [ -n "$PORT" ] && break
  sleep 1
done
[ -n "$PORT" ] || fail "no direct addr in serve.log"
echo "controller port: $PORT"

say "agent dials (direct hint, no relay) — as root via sudo"
sudo env "PATH=$SSH_PATH" "$BIN" agent --controller "$NODE_ID" \
  --direct "127.0.0.1:$PORT" --no-relay --state-dir "$AGT" \
  >>"$DEMO_DIR/agent.log" 2>&1 &
AGENT_PK=""
for _ in $(seq 1 60); do
  AGENT_PK="$("$BIN" controller pending --state-dir "$CTL" --json | jq -r '.pending[0].pubkey // empty')"
  [ -n "$AGENT_PK" ] && break
  sleep 0.5
done
[ -n "$AGENT_PK" ] || fail "agent never appeared in pending"
echo "pending agent: $AGENT_PK"

say "unapproved agent gets nothing"
if "$BIN" controller exec "$AGENT_PK" --state-dir "$CTL" -- true 2>/dev/null; then
  fail "exec succeeded on an unapproved agent"
else
  echo "exec correctly refused before approval"
fi

say "approve"
"$BIN" controller approve "$AGENT_PK" --state-dir "$CTL"
for _ in $(seq 1 60); do
  "$BIN" controller exec "$AGENT_PK" --state-dir "$CTL" -- true 2>/dev/null && break
  sleep 0.5
done
"$BIN" controller exec "$AGENT_PK" --state-dir "$CTL" -- true || fail "agent never registered after approval"
echo "agent registered"

say "exec: stdout/stderr separate, remote exit status"
set +e
OUT="$("$BIN" controller exec "$AGENT_PK" --state-dir "$CTL" -- \
  sh -c 'echo to-stdout; echo to-stderr >&2; exit 3' 2>"$DEMO_DIR/stderr.txt")"
CODE=$?
set -e
[ "$CODE" = 3 ] || fail "exit status $CODE != 3"
[ "$OUT" = "to-stdout" ] || fail "stdout mismatch: $OUT"
[ "$(cat "$DEMO_DIR/stderr.txt")" = "to-stderr" ] || fail "stderr mismatch"
echo "exit=3, stdout/stderr cleanly separated"

say "copy: push 64 MiB, verify on target, pull back, compare"
dd if=/dev/urandom of="$DEMO_DIR/up.bin" bs=1M count=64 status=none
chmod 751 "$DEMO_DIR/up.bin"
"$BIN" controller push "$AGENT_PK" "$DEMO_DIR/up.bin" /root/egdod-demo.bin --state-dir "$CTL"
LOCAL_SUM="$(sha256sum "$DEMO_DIR/up.bin" | cut -d' ' -f1)"
REMOTE_SUM="$("$BIN" controller exec "$AGENT_PK" --state-dir "$CTL" -- sha256sum /root/egdod-demo.bin | cut -d' ' -f1)"
[ "$LOCAL_SUM" = "$REMOTE_SUM" ] || fail "sha256 mismatch on target"
REMOTE_MODE="$("$BIN" controller exec "$AGENT_PK" --state-dir "$CTL" -- stat -c %a /root/egdod-demo.bin)"
[ "$REMOTE_MODE" = "751" ] || fail "mode not preserved (got $REMOTE_MODE)"
"$BIN" controller pull "$AGENT_PK" /root/egdod-demo.bin "$DEMO_DIR/down.bin" --state-dir "$CTL"
[ "$(sha256sum "$DEMO_DIR/down.bin" | cut -d' ' -f1)" = "$LOCAL_SUM" ] || fail "sha256 mismatch on pull"
echo "64 MiB round trip: sha256 identical, mode 751 preserved"

say "forward: local port -> target sshd (real banner bytes)"
"$BIN" controller forward "$AGENT_PK" 127.0.0.1:12223 127.0.0.1:22 --state-dir "$CTL" \
  >"$DEMO_DIR/forward.log" 2>&1 &
FWD_PID=$!
sleep 1
BANNER="$(exec 3<>/dev/tcp/127.0.0.1/12223 && head -c 7 <&3; exec 3<&- || true)"
[ "$BANNER" = "SSH-2.0" ] || fail "no sshd banner through the tunnel (got: $BANNER)"
echo "tunnel carried: $BANNER…"
kill "$FWD_PID" 2>/dev/null || true

say "ssh recipe (exec+copy+forward; strict, batch, first connect)"
SSH_OUT="$("$BIN" controller ssh "$AGENT_PK" --state-dir "$CTL" -- 'hostname; id -u')"
echo "$SSH_OUT"
grep -q '^0$' <<<"$SSH_OUT" || fail "ssh did not return uid 0"
grep -q ' ssh-ed25519 ' "$CTL/ssh/known_hosts" || fail "known_hosts not written from in-band host key"
echo "first ssh under StrictHostKeyChecking=yes BatchMode=yes succeeded"

say "path report (direct phase)"
grep -m2 'path:' "$DEMO_DIR/agent.log" || fail "no path report from agent"
grep -m2 'path:' "$DEMO_DIR/serve.log" || fail "no path report from controller"

say "relayed/discovery phase (bare NodeId, no hints)"
sudo pkill -f "egdod agent"; sleep 1
sudo env "PATH=$SSH_PATH" "$BIN" agent --controller "$NODE_ID" --state-dir "$AGT" \
  >>"$DEMO_DIR/agent-relay.log" 2>&1 &
# Bare-NodeId dialing through public discovery+relay is observed, not
# exec-gated: same-host path flap (the host's own docker-bridge addrs get
# discovered and raced against the relay) makes the session unstable, which
# is an iroh-path-selection property of running BOTH ends on one NAT-less
# host, not of the link. What we assert: it connects and reports its path.
ok=""
for _ in $(seq 1 120); do
  grep -q 'connected to controller' "$DEMO_DIR/agent-relay.log" && { ok=1; break; }
  sleep 1
done
[ -n "$ok" ] || fail "bare-NodeId dial never connected over discovery/relay"
grep -m3 'path' "$DEMO_DIR/agent-relay.log" || fail "no path report in relayed session"

say "forced-relay phase (--relay): exec over a relayed session"
sudo pkill -f "egdod agent"; sleep 1
RELAY_URL="$(grep -m1 'controller relay:' "$DEMO_DIR/serve.log" | awk '{print $NF}')"
[ -n "$RELAY_URL" ] || fail "no relay URL in serve.log"
echo "relay: $RELAY_URL"
sudo env "PATH=$SSH_PATH" "$BIN" agent --controller "$NODE_ID" --relay "$RELAY_URL" \
  --state-dir "$AGT" >>"$DEMO_DIR/agent-relay2.log" 2>&1 &
ok=""
for _ in $(seq 1 90); do
  "$BIN" controller exec "$AGENT_PK" --state-dir "$CTL" -- true 2>/dev/null && { ok=1; break; }
  sleep 1
done
[ -n "$ok" ] || fail "agent did not register over forced relay"
"$BIN" controller exec "$AGENT_PK" --state-dir "$CTL" -- hostname || fail "exec over relayed session failed"
grep -m2 'path:' "$DEMO_DIR/agent-relay2.log" || fail "no path report in forced-relay session"
grep -m1 'relayed' "$DEMO_DIR/agent-relay2.log" || fail "forced-relay session did not report itself relayed"

say "approval + agent survive controller restart (relayed path)"
pkill -f "egdod controller serve"; sleep 1
"$BIN" controller serve --state-dir "$CTL" >"$DEMO_DIR/serve2.log" 2>&1 &
ok=""
for _ in $(seq 1 120); do
  "$BIN" controller exec "$AGENT_PK" --state-dir "$CTL" -- true 2>/dev/null && { ok=1; break; }
  sleep 1
done
[ -n "$ok" ] || fail "agent did not reconnect to restarted controller"
echo "reconnected with no re-approval: approval survived the restart"

say "controller self-reachability"
for _ in $(seq 1 30); do
  [ -f "$CTL/reachability.json" ] && break
  sleep 1
done
"$BIN" controller status --state-dir "$CTL" --json | jq '{reachability, approved, pending}'

say "no DNS, no network but loopback (initramfs conditions)"
pkill -f "egdod controller serve"; sudo pkill -f "egdod agent"; sleep 1
sudo unshare -n --mount bash -c '
  mount --bind /dev/null /etc/resolv.conf
  ip link set lo up
  export RUST_LOG=off
  D="'"$DEMO_DIR"'/nodns"; B="'"$BIN"'"
  mkdir -p "$D"
  "$B" controller serve --state-dir "$D/ctl" --no-relay > "$D/serve.log" 2>&1 &
  for _ in $(seq 1 20); do grep -q "direct addr" "$D/serve.log" 2>/dev/null && break; sleep 0.5; done
  ID=$("$B" controller init --state-dir "$D/ctl")
  PORT=$(grep -m1 -oE "[0-9.]+:[0-9]+" "$D/serve.log" | cut -d: -f2)
  "$B" agent --controller "$ID" --direct "127.0.0.1:$PORT" --no-relay --state-dir "$D/agt" > "$D/agent.log" 2>&1 &
  PK=""
  for _ in $(seq 1 40); do PK=$(ls "$D/ctl/pending" 2>/dev/null | head -1); [ -n "$PK" ] && break; sleep 0.5; done
  [ -n "$PK" ] || { echo "no-DNS: agent never knocked"; exit 1; }
  "$B" controller approve "$PK" --state-dir "$D/ctl" >/dev/null
  ok=""
  for _ in $(seq 1 40); do "$B" controller exec "$PK" --state-dir "$D/ctl" -- hostname 2>/dev/null && { ok=1; break; }; sleep 0.5; done
  [ -n "$ok" ] || { echo "no-DNS: exec failed"; exit 1; }
  grep -m1 "path:" "$D/agent.log"
  kill %1 %2 2>/dev/null; wait 2>/dev/null
' || fail "no-DNS phase failed"
echo "agent dialed, was approved, and execd with /etc/resolv.conf masked out"

say "static musl agent binary"
MUSL_BIN="$TARGET_DIR/x86_64-unknown-linux-musl/release/egdod"
if [ -x "$MUSL_BIN" ]; then
  file "$MUSL_BIN" | grep -qE 'statically linked|static-pie' && echo "static musl binary present: $MUSL_BIN" \
    || fail "musl binary is not static: $(file "$MUSL_BIN")"
else
  echo "(musl binary not built in this run — build with: env -u RUSTC_WRAPPER cargo build --release --target x86_64-unknown-linux-musl)"
fi

say "DEMO PASS"
