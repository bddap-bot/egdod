#!/usr/bin/env bash
# Stands up a controller and an agent on this one machine and exercises
# everything egdod v0 claims: approval, the three primitives, the ssh recipe,
# the reachability canary, and — off loopback — discovery, a real relay, and
# the path labels. Unattended, and it cleans up after itself.
#
# Steps 0-14 are hermetic (no relay, no DNS, nothing but loopback). Steps 15-17
# use the public relay and n0's DNS, because "relayed" is a label loopback
# cannot produce; set EGDOD_DEMO_OFFLINE=1 to skip them and be told what was
# therefore not proved.
#
# Needs: bash, cargo (or a prebuilt binary in EGDOD_BIN), sshd, ssh, ssh-keygen,
# sha256sum, head. Runs unprivileged; passwordless sudo, if available, adds
# exec-as-root and the no-DNS namespace. EGDOD_MUSL_BIN points step 18 at the
# static build.
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

# This script installs an ssh key and starts an sshd. On a machine that is
# somebody's actual computer, a demo that leaves either behind is not a demo,
# it is a backdoor — so every key and file goes inside $DEMO and $DEMO is
# removed on EVERY exit path, success or failure or early abort, and the removal
# is then CHECKED rather than assumed.
#
# The real root authorized_keys is never a target: the recipe's default is
# overridden to a file under $DEMO (step 9). We record its digest anyway and
# compare afterwards, because "we passed a flag" is an argument and a digest is
# evidence.
ak_digest() { # the real root authorized_keys, however it is readable
  local f=/root/.ssh/authorized_keys
  if sudo -n test -r "$f" 2>/dev/null; then sudo -n sha256sum "$f" | cut -d' ' -f1
  elif [ -r "$f" ]; then sha256sum "$f" | cut -d' ' -f1
  else echo "absent-or-unreadable"; fi
}
AK_BEFORE=$(ak_digest)

reap() { # kill everything this run started, and delete its scratch
  [ -f "$DEMO/sshd/sshd.pid" ] && kill "$(cat "$DEMO/sshd/sshd.pid")" 2>/dev/null || true
  # Backstop: anything still running that was started against this run's unique
  # scratch directory, including processes reparented to init.
  pkill -f "$DEMO" 2>/dev/null || true
  sudo -n pkill -f "$DEMO" 2>/dev/null || true
  # The root-run phases leave root-owned files behind, so the unprivileged
  # removal is allowed to fail before the privileged one finishes the job.
  rm -rf "$DEMO" 2>/dev/null || sudo -n rm -rf "$DEMO" 2>/dev/null || true
}

# A trap is not enough. This script is normally run inside `nix-shell`, and
# killing that killed the whole process group without bash ever running its EXIT
# trap — which left an sshd listening with a freshly installed key on it. That
# is a backdoor, not a demo, so the guarantee cannot depend on this shell
# surviving. The janitor is `setsid`-detached, so a group kill misses it; it
# waits for this process to disappear and then reaps whatever is left.
setsid bash -c '
  while kill -0 '"$$"' 2>/dev/null; do sleep 1; done
  sleep 1
  [ -f "'"$DEMO"'/sshd/sshd.pid" ] && kill "$(cat "'"$DEMO"'/sshd/sshd.pid")" 2>/dev/null
  pkill -f "'"$DEMO"'" 2>/dev/null
  sudo -n pkill -f "'"$DEMO"'" 2>/dev/null
  rm -rf "'"$DEMO"'" 2>/dev/null || sudo -n rm -rf "'"$DEMO"'" 2>/dev/null
  exit 0
' >/dev/null 2>&1 </dev/null &
disown 2>/dev/null || true

CLEANED=0
cleanup() {
  [ "$CLEANED" = 1 ] && return 0
  CLEANED=1
  for p in "${PIDS[@]:-}"; do [ -n "$p" ] && kill "$p" 2>/dev/null || true; done
  reap

  printf '\n=== cleanup: what this run left on the machine\n'
  local bad=0
  local after; after=$(ak_digest)
  if [ "$after" = "$AK_BEFORE" ]; then
    echo "/root/.ssh/authorized_keys: unchanged ($AK_BEFORE)"
  else
    echo "DEFECT: /root/.ssh/authorized_keys CHANGED: $AK_BEFORE -> $after" >&2; bad=1
  fi
  if [ -e "$DEMO" ]; then
    echo "DEFECT: scratch directory survived: $DEMO" >&2
    ls -la "$DEMO" >&2 || true; bad=1
  else
    echo "scratch directory removed: $DEMO"
  fi
  local left; left=$(pgrep -af "$DEMO" 2>/dev/null || true)
  if [ -n "$left" ]; then echo "DEFECT: processes still running: $left" >&2; bad=1
  else echo "no egdod or sshd processes left from this run"; fi
  [ "$bad" = 0 ] && echo "nothing installed by this run remains on the machine"
}
# INT and TERM as well as EXIT: bash does not run an EXIT trap for an untrapped
# fatal signal, and ctrl-c during a 64 MiB copy is exactly when someone would
# most want the sshd and the installed key to go away. `cleanup` runs at most
# once, so the two traps overlapping is harmless.
trap 'exit 130' INT TERM
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
# Informational: `status` exits 2 when the controller is undialable, and a
# single transient probe failure must not abort a demo that is about to prove
# the canary works.
ctl status || true

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
# "Given nothing" means no files and no ports either, not just no exec.
echo unapproved-probe >"$DEMO/probe.bin"
if ctl push "$AGENT" "$DEMO/probe.bin" "$DEMO/target/probe.bin" >>"$DEMO/refused.out" 2>&1; then
  echo "DEFECT: an unapproved agent accepted a file" >&2; exit 1
fi
if ctl pull "$AGENT" "$DEMO/probe.bin" "$DEMO/stolen" >>"$DEMO/refused.out" 2>&1; then
  echo "DEFECT: an unapproved agent handed a file over" >&2; exit 1
fi
if [ -e "$DEMO/target/probe.bin" ] || [ -e "$DEMO/stolen" ]; then
  echo "DEFECT: a file crossed to or from an unapproved agent" >&2; exit 1
fi
# forward only routes when something connects, so it is tested by connecting.
# Note what this can and cannot show: nothing is listening on $SSHD_PORT yet, so
# "no bytes came back" would also be true of a *working* forward. What it does
# prove is the log line — the controller refused to route at all. The
# discriminating version of this check, against a live listener, is in
# tests/integration.rs, where standing one up costs nothing.
"$BIN" controller --state-dir "$STATE" forward "$AGENT" 0 "127.0.0.1:$SSHD_PORT" \
  >"$DEMO/refused-fwd.log" 2>&1 &
REFUSED_FWD=$!
PIDS+=("$REFUSED_FWD")
wait_for 20 "grep -q forwarding '$DEMO/refused-fwd.log'"
RP=$(sed -n 's/^forwarding 127.0.0.1:\([0-9]*\).*/\1/p' "$DEMO/refused-fwd.log" | head -1)
GOT=$(timeout 5 bash -c "exec 3<>/dev/tcp/127.0.0.1/$RP; head -c 16 <&3" || true)
kill "$REFUSED_FWD" 2>/dev/null || true
if [ -n "$GOT" ]; then echo "DEFECT: the forward carried data: $GOT" >&2; exit 1; fi
if ! grep -q 'no agent is connected' "$DEMO/refused-fwd.log"; then
  echo "DEFECT: the forward did not refuse to route" >&2; exit 1
fi
cat "$DEMO/refused.out"
grep -m1 'no agent is connected' "$DEMO/refused-fwd.log"
echo "exec, push, pull and forward all refused — the controller holds no session for an unapproved key"

say "6. approve by public key (scriptable), and watch the session appear"
ctl approve "$AGENT"
cat "$STATE/approved"
wait_for 30 "grep -q '$AGENT' '$STATE/status.json'"
ctl status || true

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
# and neither stream leaked into the other
if grep -q to-stderr "$DEMO/exec.out" || grep -q to-stdout "$DEMO/exec.err"; then
  echo "DEFECT: the two streams were mixed" >&2; exit 1
fi

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
echo "mode: source 640, on the target $(stat -c %a "$DEMO/target/up.bin"), pulled back $(stat -c %a "$DEMO/down.bin")"
[ "$(stat -c %a "$DEMO/target/up.bin")" = 640 ]
[ "$(stat -c %a "$DEMO/down.bin")" = 640 ]
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
wait_for 60 "grep -q '$AGENT' '$STATE/status.json' 2>/dev/null"
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
  wait_for 30 "grep -q '$ROOT_AGENT' '$STATE/status.json'"
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
wait_for 60 "[ -s '$UND/status.json' ] && grep -qE '\"endpoint_restarts\": [1-9]' '$UND/status.json'"
grep -m3 -E "UNDIALABLE|rebuilding" "$DEMO/undialable.log"
# `status` exits 2 on an undialable controller, which is the point of this step;
# under `set -e` that has to be caught rather than treated as the script failing.
set +e
"$BIN" controller --state-dir "$UND" status
UND_RC=$?
set -e
[ "$UND_RC" = 2 ] || {
  echo "DEFECT: an undialable controller's status exited $UND_RC, wanted 2" >&2; exit 1
}
echo "status exited 2 — a program can act on this without parsing prose"
kill "${PIDS[-1]}" 2>/dev/null || true

say "13. final controller status"
ctl status --json || true

say "14. controller log (paths reported for every session, canary results)"
grep -E 'agent connected|routed a session|reachability|UNDIALABLE|pending' "$DEMO/serve.log" | tail -12

# Everything above is hermetic, and hermetic is not the configuration this is
# for. The phases below are the real one — public relay, address lookup over DNS,
# the agent knowing nothing but a node id — so they need working internet. They
# run by DEFAULT: a phase that is off unless someone remembers a variable is a
# phase that never runs, and "relayed" is precisely the label that cannot be
# checked on loopback.
if [ "${EGDOD_DEMO_OFFLINE:-0}" = 1 ]; then
  say "15-17. SKIPPED (EGDOD_DEMO_OFFLINE=1)"
  echo "NOT EXERCISED: discovery, relay, and the relayed path label. This run"
  echo "proves nothing about how egdod behaves off loopback."
else
  R=$DEMO/relay
  RSTATE=$R/controller
  mkdir -p "$R"
  rctl() { "$BIN" controller --state-dir "$RSTATE" "$@"; }

  say "15. relay + discovery: the agent is given the node id and nothing else"
  RNODE=$(rctl init)
  rctl serve --probe-interval 15 --probe-timeout 20 >"$R/serve.log" 2>&1 &
  PIDS+=($!)
  wait_for 60 "[ -S '$RSTATE/control.sock' ]"
  wait_for 120 "grep -q '\"dialable\": true' '$RSTATE/status.json' 2>/dev/null"
  echo "canary in discovery mode (dialled its own node id through n0 DNS + relay):"
  grep -E '"probe_mode"|"dialable"|"last_probe_path"' "$RSTATE/status.json"
  # No --direct, no --relay: everything comes from resolving the node id.
  "$BIN" agent --controller "$RNODE" --key-file "$R/agent.key" >"$R/agent.log" 2>&1 &
  PIDS+=($!)
  wait_for 60 "grep -q 'agent pubkey' '$R/agent.log'"
  RAGENT=$(sed -n 's/^agent pubkey: //p' "$R/agent.log" | head -1)
  rctl approve "$RAGENT"
  wait_for 120 "grep -q '$RAGENT' '$RSTATE/status.json'"
  rctl exec "$RAGENT" -- /bin/sh -c 'echo hello-from-a-node-id-alone'
  rctl status || true

  say "16. the relayed label, and a session that stops being relayed"
  # Both ends are on one host, so holepunching always wins eventually and a
  # session cannot be pinned to the relay to be photographed. What can be shown
  # is the truth: the session *starts* relayed, says so, and says so again when
  # it changes. A label that never changed would be the defect.
  echo "what the target logged about its own path:"
  grep -E 'connected to controller|path changed' "$R/agent.log" || true
  if ! grep -qE 'path=relayed|was=relayed|"last_probe_path": "relayed"' \
      "$R/agent.log" "$RSTATE/status.json"; then
    echo "DEFECT: nothing in this run ever reported a relayed path, so the" >&2
    echo "        relayed/direct distinction went untested." >&2
    exit 1
  fi
  echo "relay in use: $(grep -hoE 'https://[a-z0-9.-]+\.relay\.n0\.iroh\.link\.?' \
    "$R/agent.log" "$RSTATE/status.json" 2>/dev/null | head -1)"

  say "17. no DNS at all, and no network but loopback — initramfs conditions"
  # The rest of this script gets away without DNS by accident (it hands the agent
  # a --direct address). This proves it: a fresh network namespace with nothing
  # but lo, and /etc/resolv.conf masked out, which is what an initramfs looks like.
  if sudo -n true 2>/dev/null; then
    sudo -n unshare -n --mount bash -c '
      set -e
      mount --bind /dev/null /etc/resolv.conf
      ip link set lo up
      B=$1; D=$2/nodns; mkdir -p "$D"
      ID=$("$B" controller --state-dir "$D/ctl" init)
      "$B" controller --state-dir "$D/ctl" serve --no-relay --bind 127.0.0.1:45888 \
        >"$D/serve.log" 2>&1 &
      for _ in $(seq 60); do [ -S "$D/ctl/control.sock" ] && break; sleep 0.25; done
      "$B" agent --controller "$ID" --key-file "$D/agent.key" \
        --no-relay --direct 127.0.0.1:45888 >"$D/agent.log" 2>&1 &
      for _ in $(seq 60); do grep -q "agent pubkey" "$D/agent.log" && break; sleep 0.25; done
      PK=$(sed -n "s/^agent pubkey: //p" "$D/agent.log" | head -1)
      "$B" controller --state-dir "$D/ctl" approve "$PK"
      for _ in $(seq 80); do
        "$B" controller --state-dir "$D/ctl" exec "$PK" -- /bin/echo ok >/dev/null 2>&1 && break
        sleep 0.25
      done
      echo "resolv.conf is: $(cat /etc/resolv.conf | wc -c) bytes"
      "$B" controller --state-dir "$D/ctl" exec "$PK" -- /bin/sh -c "echo exec-with-no-resolver"
      kill %1 %2 2>/dev/null || true
    ' _ "$BIN" "$DEMO"
  else
    echo "SKIPPED: needs passwordless sudo to create a network namespace"
  fi
fi

GAPS=0
say "18. the static agent — the binary this is supposed to boot on"
# Built separately (nix-build musl.nix); run here rather than merely inspected,
# because `file` says nothing about whether it works.
MUSL=${EGDOD_MUSL_BIN:-}
if [ -x "$MUSL" ]; then
  file "$MUSL"
  M=$DEMO/musl; mkdir -p "$M"
  # Not merely "does it stay alive": the fresh key must be held pending, get
  # approved, and serve an exec — the same loop every glibc phase above ran.
  # noq-udp's aligned cmsg reads used to abort this binary on its first
  # received datagram (egdod#3, vendor/noq-udp/VENDOR.md), which the align_of
  # branch below still detects if it ever comes back.
  "$MUSL" agent --controller "$NODE_ID" --key-file "$M/agent.key" \
    --no-relay --direct "$DIRECT" >"$M/agent.log" 2>&1 &
  PIDS+=("$!")
  set +e
  wait_for 15 "grep -q 'agent pubkey:' '$M/agent.log'"
  MUSL_PK=$(sed -n 's/^agent pubkey: //p' "$M/agent.log" | head -1)
  wait_for 20 "ctl pending --json | grep -q \"$MUSL_PK\""
  ctl approve "$MUSL_PK"
  # PENDING_RETRY is 5s; give the redial and the exec a few attempts.
  MUSL_OUT=
  for _ in 1 2 3 4 5 6; do
    MUSL_OUT=$(ctl exec "$MUSL_PK" -- /bin/sh -c 'echo static-agent-served-exec' 2>>"$M/exec.err")
    [ "$MUSL_OUT" = static-agent-served-exec ] && break
    sleep 5
  done
  set -e
  if grep -q 'align_of' "$M/agent.log"; then
    GAPS=1
    echo "GAP: the static binary aborted on a received datagram — the noq-udp"
    echo "     cmsg alignment bug (egdod#3) is back:"
    grep -m2 -E 'panicked|assertion' "$M/agent.log"
  elif [ "$MUSL_OUT" = static-agent-served-exec ]; then
    echo "the static binary connected, was approved, and served an exec:"
    echo "  $MUSL_OUT"
  else
    GAPS=1
    echo "GAP: the static binary ran but never served an exec; agent.log tail:"
    tail -5 "$M/agent.log"
  fi
else
  GAPS=1
  echo "not checked: set EGDOD_MUSL_BIN to the output of \`nix-build musl.nix\`"
  echo "GAP: SPEC.md property 4 wants a working static musl agent. See PROOF.md."
fi

if [ "$GAPS" = 0 ]; then
  say "demo complete: every claim above was exercised, including the static agent"
else
  say "demo complete: primitives, recipe, canary and relay all exercised — but see the GAP above"
  echo "egdod works on the machine you build it on. It does not yet run on the"
  echo "machine it is for. PROOF.md and INTEGRATION.md say exactly how far that is."
fi
