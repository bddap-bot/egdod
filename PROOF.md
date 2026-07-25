# egdod v0 — what was observed, and what was not

Branch `impl/opus`. Everything below is a transcript of a real run on one machine
(NixOS, x86_64, kernel 6.18.39, iroh 1.0.3, rustc from the pinned nixpkgs in
`shell.nix`), not a description of intended behaviour. Where something is
inferred rather than seen, it says so.

Reproduce with `nix-shell --run ./demo.sh` (add `EGDOD_DEMO_RELAY=1` for the last
phase, which needs internet).

---

## 1. Design in one page

**Identity is the iroh NodeId, and nothing else.** The agent generates a keypair
on first run and uses it as its iroh secret key, so the key the controller
approves is the key QUIC has already authenticated by the time a connection
exists. There is no application-level handshake to get wrong, no capability
token, no shared secret in the image. The image carries the controller's NodeId,
which is public.

**`serve` is a dumb proxy.** The long-lived controller authorises the agent,
then does nothing but copy bytes between a unix socket in the state directory and
a QUIC stream. Every short-lived command (`exec`, `push`, `pull`, `forward`,
`ssh`) speaks the request protocol itself. So the request protocol has exactly
one implementation on each side, and `ssh` is a *composition* of the primitives
rather than a fourth primitive with privileged access to internals.

**One request per stream.** The stream is the request's channel; end of request
is stream FIN. `exec` frames stdout and stderr separately; `push`/`pull` send an
integrity header (mode, length, sha256) and then the raw body, verified into a
staging file that is renamed into place only if length and digest match;
`forward` acks and then the stream is a raw pipe.

---

## 2. `cargo test`, run in the foreground

```
$ nix-shell --run 'cargo test'
   Compiling egdod v0.1.0 (/home/bot/.cache/botq-wt/1700/repo)
    Finished `test` profile [unoptimized + debuginfo] target(s) in 12.65s
     Running unittests src/lib.rs

running 14 tests
test net::tests::path_classification ... ok
test net::tests::relay_addr_is_never_reported_as_direct ... ok
test net::tests::relay_choice_flags ... ok
test pipe::tests::splices_both_directions_and_propagates_eof ... ok
test proto::tests::msg_roundtrip_and_framing ... ok
test state::tests::approved_list_tolerates_comments_and_junk ... ok
test state::tests::approval_is_by_pubkey_and_survives_restart ... ok
test agent::tests::agent_key_is_stable_across_runs ... ok
test proto::tests::truncated_transfer_is_rejected ... ok
test proto::tests::corrupt_transfer_leaves_no_destination ... ok
test proto::tests::overlong_transfer_is_cut_off ... ok
test state::tests::key_is_stable_and_private ... ok
test proto::tests::copy_verifies_digest_and_preserves_mode ... ok
test net::tests::probe_of_a_nonexistent_endpoint_fails ... ok

test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 6.03s

     Running unittests src/main.rs
running 0 tests
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

   Doc-tests egdod
running 0 tests
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

`cargo clippy --all-targets` is also clean. There is no `unsafe` in this crate.

---

## 3. `./demo.sh` — the real transcript

Long paths are the run's scratch directory (`$DEMO`); nothing else is elided.
`egdod: session with … via …` lines are the per-session path report, on stderr.

```
=== 0. versions
egdod 0.1.0
OpenSSH_10.4p1, OpenSSL 3.6.3 9 Jun 2026

=== 1. controller init — the node id is the only thing an image needs, and it is public
egdod: controller key at $DEMO/controller/controller.key — bake the node id above into images; it is public
node id: deadc1d4092da82176f2bceeb6822b488199f87e6e4c2e8456017a9c83aa8a99
total 4
-rw------- 1 bot users 32 Jul 24 23:10 controller.key

=== 2. controller serve (hermetic: --no-relay, so nothing leaves this machine)
controller direct address: 127.0.0.1:45777

=== 3. reachability canary — the controller dials its own node id from a fresh key
node id:   deadc1d4092da82176f2bceeb6822b488199f87e6e4c2e8456017a9c83aa8a99
dialable:  yes
probe:     Direct mode, last ok 1784959804
restarts:  0
agent:     none connected

=== 4. agent starts, dials out, and is held pending (nothing is served to it)
3681d94aaaed67f88160bde54f91d7eb76c3a02d4b35f730ae8ee4b9c1cc37f9  attempts=1 first_seen=1784959805 last_seen=1784959805
agent pubkey: 3681d94aaaed67f88160bde54f91d7eb76c3a02d4b35f730ae8ee4b9c1cc37f9

=== 5. an unapproved agent gets nothing
Error: no agent is connected
refused, as required — the controller holds no session for an unapproved key

=== 6. approve by public key (scriptable), and watch the session appear
approved 3681d94aaaed67f88160bde54f91d7eb76c3a02d4b35f730ae8ee4b9c1cc37f9
3681d94aaaed67f88160bde54f91d7eb76c3a02d4b35f730ae8ee4b9c1cc37f9
node id:   deadc1d4092da82176f2bceeb6822b488199f87e6e4c2e8456017a9c83aa8a99
dialable:  yes
probe:     Direct mode, last ok 1784959804
restarts:  0
agent:     3681d94aaaed67f88160bde54f91d7eb76c3a02d4b35f730ae8ee4b9c1cc37f9 via direct-local (127.0.0.1:56696)

=== 7. PRIMITIVE exec — stdout and stderr stay separate, exit status comes back
exit status: 7 (expected 7)
stdout file: to-stdout
stderr file: to-stderr

=== 8. PRIMITIVE copy — 64 MiB up, verified on the target, then back down
egdod: session with 3681d94a… via direct-local (127.0.0.1:56696)
pushed $DEMO/up.bin -> $DEMO/target/up.bin (67108864 bytes, sha256 78212f4b8dbc370a81bca06aa2843a068e537119564dd34abfab557e786ad9b7)
egdod: session with 3681d94a… via direct-local (127.0.0.1:56696)
78212f4b8dbc370a81bca06aa2843a068e537119564dd34abfab557e786ad9b7  $DEMO/target/up.bin
egdod: session with 3681d94a… via direct-local (127.0.0.1:56696)
pulled $DEMO/target/up.bin -> $DEMO/down.bin (67108864 bytes, sha256 78212f4b8dbc370a81bca06aa2843a068e537119564dd34abfab557e786ad9b7)
78212f4b8dbc370a81bca06aa2843a068e537119564dd34abfab557e786ad9b7  $DEMO/up.bin
78212f4b8dbc370a81bca06aa2843a068e537119564dd34abfab557e786ad9b7  $DEMO/down.bin
mode on the target: 640 (source was 640)
agent peak RSS after a 64 MiB round trip: VmHWM:	   46416 kB

=== 9. RECIPE ssh — host key generated and fetched over the egdod channel
egdod: session with 3681d94a… via direct-local (127.0.0.1:56696)
egdod: session with 3681d94a… via direct-local (127.0.0.1:56696)
egdod: installed an authorized key at $DEMO/target-authorized_keys
egdod: session with 3681d94a… via direct-local (127.0.0.1:56696)
egdod: no host key on the target; generating one with ["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", "$DEMO/sshd/ssh_host_ed25519_key"]
egdod: session with 3681d94a… via direct-local (127.0.0.1:56696)
egdod: session with 3681d94a… via direct-local (127.0.0.1:56696)
egdod: session with 3681d94a… via direct-local (127.0.0.1:56696)
WARN egdod::controller: forwarded connection failed: agent could not connect to 127.0.0.1:2299: Connection refused (os error 111)
egdod: no sshd answering; starting it with ["…/openssh-10.4p1/bin/sshd", "-f", "$DEMO/sshd/sshd_config"]
egdod: session with 3681d94a… via direct-local (127.0.0.1:56696)
egdod: session with 3681d94a… via direct-local (127.0.0.1:56696)
egdod: sshd is up: SSH-2.0-OpenSSH_10.4
egdod: known_hosts entry from the authenticated channel: [127.0.0.1]:37863 ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIEL2uI835lvHFEl5lfWNFdECf9jE2n1q6aQyNSekeqqM
egdod: session with 3681d94a… via direct-local (127.0.0.1:56696)
bothouse
known_hosts the controller wrote:
[127.0.0.1]:37863 ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIEL2uI835lvHFEl5lfWNFdECf9jE2n1q6aQyNSekeqqM

=== 10. PRIMITIVE forward — a local port onto the target's sshd, proven by its banner
forwarding 127.0.0.1:34161 -> 127.0.0.1:2299 on the target (ctrl-c to stop)
SSH-2.0-OpenSSH_10.4

=== 11. approval survives a controller restart, and the agent redials on its own
reconnected without being approved again:
egdod: session with 3681d94a… via direct-local (127.0.0.1:45728)
still-here

=== 12. exec as root (only if this machine offers passwordless sudo)
approved f389e291cd0d3948220e524039a246c78f746d9b58d39713c464182d609ec472
egdod: session with f389e291… via direct-local (127.0.0.1:42711)
uid seen by exec: 0
egdod: session with f389e291… via direct-local (127.0.0.1:42711)
/etc/shadow is readable: exec really is root

=== 12b. what the controller does when it CANNOT be dialled
ERROR egdod::controller: UNDIALABLE: cannot reach our own node id (probe timed out after 0ns). A target booting now would not find us. failures=1
ERROR egdod::controller: UNDIALABLE: cannot reach our own node id (probe timed out after 0ns). A target booting now would not find us. failures=2
ERROR egdod::controller: rebuilding the iroh endpoint: no dialer has been able to reach us
node id:   90e8d37363840b362d8ae061b4784fdb9bcfe98a9986a9bc08d399975c8c40f7
dialable:  NO — probe timed out after 0ns
probe:     Direct mode, last ok never
restarts:  1
agent:     none connected

=== 13. final controller status
{
  "node_id": "deadc1d4092da82176f2bceeb6822b488199f87e6e4c2e8456017a9c83aa8a99",
  "pid": 2068720,
  "updated_unix": 1784959862,
  "direct_addrs": [ "127.0.0.1:45777" ],
  "dialable": true,
  "probe_mode": "direct",
  "last_probe_ok_unix": 1784959861,
  "last_probe_path": "direct-local",
  "last_probe_error": null,
  "consecutive_probe_failures": 0,
  "endpoint_restarts": 0,
  "sessions": [
    { "agent": "f389e291…", "path": "direct-local", "remote": "127.0.0.1:42711", "since_unix": 1784959862 },
    { "agent": "3681d94a…", "path": "direct-local", "remote": "127.0.0.1:45728", "since_unix": 1784959857 }
  ]
}

=== 14. controller log (paths reported for every session, canary results)
INFO egdod::controller: routed a session agent=3681d94a… path=direct-local remote=127.0.0.1:56696
INFO egdod::controller: reachability confirmed: a stranger can dial us path=direct-local mode=Direct
INFO egdod::controller: reachability confirmed: a stranger can dial us path=direct-local mode=Direct
INFO egdod::controller: agent connected agent=3681d94a… path=direct-local remote=127.0.0.1:45728
INFO egdod::controller: routed a session agent=3681d94a… path=direct-local remote=127.0.0.1:45728
WARN egdod::controller: unapproved agent held pending; nothing served agent=f389e291…
INFO egdod::controller: reachability confirmed: a stranger can dial us path=direct-local mode=Direct
INFO egdod::controller: agent connected agent=f389e291… path=direct-local remote=127.0.0.1:42711
INFO egdod::controller: routed a session agent=f389e291… path=direct-local remote=127.0.0.1:42711

=== 15. relay + discovery: the agent knows only the node id
canary in discovery mode (dialled its own node id through n0 DNS + relay):
  "dialable": true,
  "probe_mode": "discovery",
  "last_probe_path": "relayed",
approved b20ad8aa1677902f22a969c296159d89ea0356427f926b67dc26d5aea93f2b22
egdod: session with b20ad8aa… via direct-lan (192.168.1.141:35235)
hello-over-the-internet
node id:   5c435c630ca154d53cc70dabd22970a98e30ead2e1cc8064bfb71bd750d88e4d
dialable:  yes
probe:     Discovery mode, last ok 1784959872
restarts:  0
agent:     b20ad8aa… via relayed (https://usw1-1.relay.n0.iroh.link./)

=== demo complete: all primitives, the recipe, and the canary exercised
```

Exit status 0.

---

## 4. PROVED — observed in the run above

- **exec.** `sh -c 'echo to-stdout; echo to-stderr >&2; exit 7'` produced
  `to-stdout` on the controller's stdout, `to-stderr` on its stderr — two
  separate files in the demo, so the separation is checked, not eyeballed — and
  the controller exited 7.
- **exec as root.** A second agent started under `sudo` reported `id -u` = 0 and
  successfully read `/etc/shadow` (0 bytes of it: the demo proves access without
  printing content). The unprivileged agent in the rest of the run executes as
  the agent's own uid; this step is what shows "as root" is real when the agent
  is root.
- **copy, both directions, with integrity and mode.** 64 MiB of `/dev/urandom`
  pushed, hashed *on the target* by `exec sha256sum`, pulled back, all three
  digests identical, mode 640 preserved.
- **copy is streaming, not buffered.** The agent's peak RSS after a 64 MiB round
  trip was 46 MB — under the size of the file it moved twice. That is what "handles
  files larger than RAM" reduces to on a machine where a bigger test is impractical.
- **forward.** A controller-side TCP listener carried the target's sshd banner
  (`SSH-2.0-OpenSSH_10.4`) back through the QUIC connection the agent dialled.
- **Approve-before-anything.** Before approval the agent appears in
  `pending`/`pending --json` and the controller holds *no session* for it, so
  `exec` fails outright; there is no code path from an unapproved connection to a
  request parser. The first `WARN unapproved agent held pending; nothing served`
  is the controller turning it away.
- **Approval is by public key, scriptable, and survives restart.** `approve
  <pubkey>` appends to `<state-dir>/approved`; step 11 killed the controller,
  started a fresh one against the same state directory, and the agent redialled
  and was served without being approved again. (Also unit-tested at the file
  level, including a reopened state directory.)
- **The agent never gives up.** Same step: the agent survived the controller
  disappearing and coming back with no human touching it. It also retried through
  the whole pending period in step 4–6.
- **Every session reports its path.** Every command prints `egdod: session with
  <agent> via <path> (<addr>)`; the controller logs the same for each connection
  and each routed stream; `status`/`status --json` carries it per session.
  Observed values in this run: `direct-local` (hermetic phase), `direct-lan` and
  `relayed` (relay phase).
- **The ssh recipe, composed from the three primitives.** In one run it: pulled
  the target's `authorized_keys` (absent), pushed it with the controller's key
  (copy); pulled the host key (absent), generated one with `ssh-keygen` (exec),
  pulled it again (copy); found nothing answering on the target's sshd port
  (forward + banner probe — see the `Connection refused` warning), started sshd
  (exec), waited for the banner; wrote `known_hosts` from the key that came over
  the authenticated channel; then ran ssh with `-o StrictHostKeyChecking=yes -o
  BatchMode=yes -o IdentitiesOnly=yes`, which printed `bothouse`. That connect
  could not have prompted and could not have accepted on trust.
- **The controller knows when it cannot be dialled.** The canary dials the
  controller's own NodeId from a *throwaway endpoint with a fresh key* — a
  stranger, not a self-check — every `--probe-interval`. Step 12b forces every
  probe to fail (zero-length deadline) and shows the whole reaction: `ERROR
  UNDIALABLE …`, `dialable: NO` in `status`, and the endpoint rebuilt after
  `--restart-after` failures (`restarts: 1`).
- **Relay and discovery, for real.** Step 15 ran a controller with the default
  public n0 relay and DNS-based address lookup; its canary resolved and dialled
  its own NodeId through them (`probe_mode: discovery`, `last_probe_path:
  relayed`), and an agent given nothing but the NodeId connected and ran a
  command. Its first path was the relay, and it upgraded to a direct LAN path
  (both peers were on this machine), which is why the `exec` line says
  `direct-lan` while the session snapshot still said `relayed`.
- **No secret in the image.** The agent is started with a NodeId and nothing
  else; it generates its own key on first run (`agent pubkey: …`, printed to
  stdout so a serial console can capture it) and that key is what gets approved.
- **The controller state is one directory.** `--state-dir` holds the key
  (mode 600, checked by a test), `approved`, `pending.json`, `status.json`,
  `control.sock` and the recipe's ssh material. Nothing is written outside it,
  nothing needs root, no fixed system path is used.
- **A static musl binary builds.** `nix-build musl.nix` produces a
  `static-pie linked` 26 MB `egdod`; `env -i ./egdod --version` and `env -i
  ./egdod agent --help` run with an entirely empty environment. **But see the
  failure below — it does not survive first contact with a packet.**

## 5. FAILED — found, root-caused, not fixed

**The musl build panics on the first datagram received.** Not my code: iroh's UDP
layer unconditionally sets `SO_TIMESTAMPNS` and then decodes the resulting
`SCM_TIMESTAMPNS` control message as a `libc::timespec`, which is 8-byte aligned,
while musl's `cmsghdr` is 4-byte aligned. `noq-udp`'s alignment assertion fires:

```
thread 'tokio-rt-worker' panicked at noq-udp-1.1.0/src/cmsg/mod.rs:81:5:
assertion failed: align_of::<T>() <= align_of::<C>()
  11: noq_udp::cmsg::decode::<libc::unix::timespec, libc::new::musl::sys::socket::cmsghdr, …>
  12: <noq_udp::imp::UdpSocketState>::recv
  15: <iroh::socket::transports::ip::IpTransport>::poll_recv
```

Reproduced on `noq-udp 1.1.0` (the newest published version; `cargo update`
changes nothing). It affects both roles under musl and neither under glibc, and
it is upstream: the fix belongs in `noq-udp` (`unix.rs:784`, either skip the
`SCM_TIMESTAMPNS` arm on musl or read it unaligned). I did **not** vendor a
patched copy of the dependency — forking iroh's UDP crate into this repo is a
worse trade than reporting it. `musl.nix` is kept because the build itself is
correct and will start working the moment upstream does.

So: **the spec's "single static binary (musl)" is delivered as a build and a
running CLI, but is not a working agent today.** Everything in §4 was proved with
the glibc build. Nobody should read the musl claim as tested-working.

## 6. INFERRED — believed, not observed here

Each of these is an inference with its reason, not a result:

- **Dial-out from behind hostile NAT.** Both peers were on one machine, so
  holepunching and relay fallback were exercised only in the easy case. The
  prior art (`bddap/bothouse`, `hatch/RESULTS.md`) measured exactly this shape —
  outbound-only-NAT guest, baked NodeId, public relay, ~3–4 MB/s — with the same
  iroh mechanism, which is why I expect it to hold. Inference, not a result of
  this run.
- **Throughput over a relay.** Never measured here; the 64 MiB copy went over
  loopback. Assume the prior art's ~3–4 MB/s over a public relay, and much more
  on a direct path.
- **`direct-wan` classification.** The classifier is unit-tested against
  synthetic addresses (loopback, RFC1918, link-local, ULA, a public v4), but no
  session in this run travelled a direct WAN path.
- **Behaviour as PID 1 in an initramfs.** The agent never assumes a shell, a
  populated `/etc`, dbus or NetworkManager, and runs under `env -i`; but it was
  not booted as init inside an initramfs here. See the environment list below
  for what it does need.
- **Running on aarch64, or on a phone.** The controller needs no root, no
  systemd and no fixed paths, and keeps all state under one directory — the
  properties that make it portable — but it was only run on x86_64 Linux.
- **A file genuinely larger than RAM.** 64 MiB with 46 MB peak RSS shows the
  copy streams; a 100 GB file was not attempted.

## 7. NOT TESTED — stated plainly

- Two agents from *different* machines; every agent here was a local process.
- Concurrent commands to the same agent (the protocol is one request per stream,
  and streams are independent, but no test drove them in parallel).
- The recipe's default paths (`/root/.ssh/authorized_keys`, `/etc/ssh/…`, `sshd`
  from `PATH`, port 22). The demo overrides all of them so that it can run
  unprivileged and leave no trace on the host; the *spec-shaped* invocation
  `egdod controller ssh <agent>` with no flags was never run.
- Recovery from a corrupt `pending.json`/`approved` file written by something
  other than egdod.
- IPv6-only environments; a relay reached over an IP-literal URL with DNS
  actually broken (the flag exists and parses, and `--direct` makes the resolver
  unnecessary, but no test removed DNS).
- Very long-lived sessions, or a controller restarted while a `push` is in
  flight.
- A cancelled `exec`: killing the controller command does not kill the process it
  started on the target. It leaks there until the agent exits. Known, not fixed.
- Revocation. Removing a key from `approved` stops the *next* connection; a live
  session survives until it drops. v0 has no `revoke` command and the spec does
  not ask for one.

---

## 8. What the agent needs from its environment

This is the whole list; anything not here is not assumed.

- **Nothing at all to start.** `env -i egdod agent --controller <nodeid> …` runs.
  No `/etc`, no shell, no dbus, no NetworkManager, no `$HOME`.
- **A writable path for its key** (`--key-file`, default `/var/lib/egdod/agent.key`);
  parent directories are created. On a tmpfs initramfs the key is regenerated
  per boot, so the agent needs approving again each boot — a property of the
  medium, and the reason the flag exists.
- **A UDP socket and a route.** No inbound reachability, no port forwarding, no
  DHCP option, no PXE.
- **How a relay is reached without DNS**, in order of preference:
  - `--direct <addr:port>` (repeatable) — the controller's socket address. This
    *also switches off DNS-based address lookup entirely*, so no resolver is
    consulted; pair it with `egdod controller serve --bind <addr:port>` so the
    port survives a controller restart.
  - `--relay https://<ip-literal>/` — an explicit relay URL with no hostname to
    resolve.
  - `--no-relay` — no relay at all; only `--direct` addresses are used.
  - The default (neither flag) uses n0's public relays and DNS-based lookup,
    which does need working DNS.
- **For `exec`:** the argv is executed directly, not through a shell, and `PATH`
  comes from the agent's own environment. With `env -i` there is no `PATH`, so
  give absolute paths. This is deliberate — a target with no userland has no
  shell to split a command line, which is also why `--sshd-arg`/`--keygen-arg`
  are repeatable rather than a string.
- **For the ssh recipe only:** an `sshd` and an `ssh-keygen` on the target, and
  an `ssh` client plus `ssh-keygen` on the controller. The recipe is a
  convenience built on the primitives; the primitives need none of it.

## 9. Deviations from the spec's command surface, and why

The spec's surface is implemented as written. Additions:

- `egdod controller status [--json]` — the spec requires the controller to
  "expose the state" of its own reachability (§"the controller must know when it
  is unreachable"). This is that, plus the live sessions and their paths. The
  same data is in `<state-dir>/status.json` for a program that would rather read
  a file. It warns when the process that wrote the file is gone, so a dead
  controller cannot look healthy.
- `serve --relay/--no-relay/--bind/--probe-interval/--probe-timeout/--restart-after`.
  `--no-relay` and `--bind` are what make the demo hermetic and repeatable (no
  internet, stable address); the probe knobs let the demo show the undialable
  reaction in seconds instead of minutes. Defaults match the spec's intent: n0
  relays, a probe every 60 s, rebuild after 2 consecutive failures.
- `agent --key-file` — the agent must persist its identity somewhere.
- `controller ssh` flags (`--user`, `--authorized-keys`, `--host-key`,
  `--sshd-arg`, `--keygen-arg`, `--target-addr`, `--local-port`). All have
  defaults matching the spec's implied root/`/etc/ssh` layout, so
  `egdod controller ssh <agent>` works as written; the flags exist so the demo
  can run unprivileged.
- `PathKind` has four direct/relayed values plus `unknown`: `direct-local`,
  `direct-lan`, `direct-wan`, `relayed`, `unknown`. The spec names three;
  `direct-wan` is split out of "direct-LAN" rather than labelling an
  internet-routed direct path as LAN, and `unknown` exists so a path this code
  cannot classify is never rounded up to "direct". Only the *selected* iroh path
  is reported — a relay path and a not-yet-selected direct candidate are often
  open at once, and reporting the wrong one is the defect the spec names.

## 10. Smaller decisions worth recording

- **Approval is a file, re-read on every connection.** So `approve` needs no
  running controller, takes effect without a restart, and survives one. The
  pending list is capped at the 256 most recent keys: the NodeId is public, so
  anyone can dial with a fresh key, and an unbounded list is a disk-filling
  primitive against a controller that might be a phone.
- **The canary answers any dialer** on a separate ALPN with a fixed 11-byte
  constant. It has to look like a stranger to prove anything, and the answer
  reveals nothing that dialing the socket did not already reveal.
- **Integrity header before the payload.** The sender reads the file twice
  (hash, then send) rather than sending a trailer, which would need a second
  framing layer around the bulk stream. Memory stays O(1) either way.
- **Received data is staged in a file created `O_EXCL` under a random name** next
  to the destination, and renamed only after length and digest check out. The
  agent is usually root, so a predictable staging name in a directory a local
  user can write would be a write-anywhere primitive via a symlink.
- **The file mode is preserved exactly, including setuid bits.** That is what
  "preserving mode" means, but note the consequence: a compromised agent can make
  a `pull` land a setuid file on the controller. It would be owned by the
  controller's own user, so it grants nothing that user did not have.
