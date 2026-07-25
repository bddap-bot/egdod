# PROOF — egdod v0

Everything under **PROVED** was observed in the runs pasted below on 2026-07-25
(bothouse, x86_64 NixOS, iroh 1.0.3, public n0 relay `usw1-1`). Everything under
**INFERRED** was not. The demo is `demo.sh`; reproduce with
`nix-shell --run './demo.sh'` (runs the agent as root via sudo; exercises all
three primitives, the ssh recipe, both path classes, restart survival,
self-reachability, and the no-DNS case).

## Design in one paragraph

One binary, two roles, per the spec. The agent's iroh endpoint key *is* its
identity — approval binds that public key, and QUIC already authenticates it,
so the capability layer the prior art (`iroh-tunnel`) used is dropped: same
security property, fewer moving parts. The controller `serve` owns the NodeId,
gates connections on `$state/approved/<pubkey>`, records unknowns to
`$state/pending/<pubkey>`, and bridges local CLI sessions onto agent
connections over a unix control socket — protocol knowledge lives only in the
CLI commands and the agent; serve routes. Approval/pending are plain files, so
approval survives restart by construction and an unattended controller can
auto-approve with `egdod controller approve <pubkey>` (or `mv`). The canary is
a second in-process endpoint that dials the controller's own NodeId with no
address hints — it exercises exactly what a booting target exercises — and
writes `reachability.json`; `egdod controller status` exposes it.

## `cargo test` (real output)

```
   Compiling egdod v0.1.0 (/home/bot/.cache/botq-wt/1699/repo)
    Finished `test` profile [unoptimized + debuginfo] target(s) in 5.56s
     Running unittests src/lib.rs (/home/bot/.cache/botq-wt/1699-target/debug/deps/egdod-266861af474c0201)

running 8 tests
test tests::cursor_strings_and_bounds ... ok
test tests::status_roundtrip ... ok
test tests::stream_hashed_rejects_short_stream ... ok
test tests::frames_roundtrip_and_eof ... ok
test tests::distinct_seeds_distinct_ids ... ok
test tests::key_file_roundtrip ... ok
test tests::classify_ip_labels ... ok
test tests::stream_hashed_moves_exact_bytes ... ok

test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.05s

     Running unittests src/main.rs (/home/bot/.cache/botq-wt/1699-target/debug/deps/egdod-83fb26381408547b)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running tests/integration.rs (/home/bot/.cache/botq-wt/1699-target/debug/deps/integration-295dddb3a36ef96e)

running 1 test
test end_to_end_loopback ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.68s

   Doc-tests egdod

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

The integration test (`tests/integration.rs`) stands up a real controller
endpoint and a real agent over loopback QUIC — pending, refusal before
approval, approval, exec stdout/stderr/exit-code separation, spawn-failure
127, a 5 MiB push/pull round trip with mode preserved, a forwarded TCP echo —
and asserts the session path label is `direct-*`.

## Demo transcript (real, complete, unedited)

```
=== build (debug) ===
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.29s

=== controller init ===
controller NodeId: 3373b2fc672da9b5c6c2aee796429ddaed23ce69414cdea2a11b06d9f873afd3

=== serve (controller) ===
controller port: 47890

=== agent dials (direct hint, no relay) — as root via sudo ===
pending agent: 28b2daeb0fcf09efb4281c02e57d78dec5d2d3014a1879f147009ce651e5a30c

=== unapproved agent gets nothing ===
exec correctly refused before approval

=== approve ===
approved 28b2daeb0fcf09efb4281c02e57d78dec5d2d3014a1879f147009ce651e5a30c (was pending)
agent registered

=== exec: stdout/stderr separate, remote exit status ===
exit=3, stdout/stderr cleanly separated

=== copy: push 64 MiB, verify on target, pull back, compare ===
pushed 67108864 bytes to /root/egdod-demo.bin
pulled 67108864 bytes from /root/egdod-demo.bin
64 MiB round trip: sha256 identical, mode 751 preserved

=== forward: local port -> target sshd (real banner bytes) ===
tunnel carried: SSH-2.0…

=== ssh recipe (exec+copy+forward; strict, batch, first connect) ===
egdod ssh: forward 127.0.0.1:35933 -> 127.0.0.1:22 on 28b2daeb0fcf09efb4281c02e57d78dec5d2d3014a1879f147009ce651e5a30c
bothouse
0
first ssh under StrictHostKeyChecking=yes BatchMode=yes succeeded

=== path report (direct phase) ===
egdod agent: connected to controller 3373b2fc672da9b5c6c2aee796429ddaed23ce69414cdea2a11b06d9f873afd3 — path: direct-local (127.0.0.1:47890)
egdod agent: connected to controller 3373b2fc672da9b5c6c2aee796429ddaed23ce69414cdea2a11b06d9f873afd3 — path: direct-local (127.0.0.1:47890)
egdod serve: agent 28b2daeb0fcf09efb4281c02e57d78dec5d2d3014a1879f147009ce651e5a30c connected — path: direct-local (127.0.0.1:46911)

=== relayed/discovery phase (bare NodeId, no hints) ===
egdod agent: connected to controller 3373b2fc672da9b5c6c2aee796429ddaed23ce69414cdea2a11b06d9f873afd3 — path: relayed (relay https://usw1-1.relay.n0.iroh.link./)
egdod agent: path changed: relayed (relay https://usw1-1.relay.n0.iroh.link./) -> direct-LAN (172.18.0.1:47890)

=== forced-relay phase (--relay): exec over a relayed session ===
relay: https://usw1-1.relay.n0.iroh.link./
bothouse
egdod agent: connected to controller 3373b2fc672da9b5c6c2aee796429ddaed23ce69414cdea2a11b06d9f873afd3 — path: relayed (relay https://usw1-1.relay.n0.iroh.link./)
egdod agent: connected to controller 3373b2fc672da9b5c6c2aee796429ddaed23ce69414cdea2a11b06d9f873afd3 — path: relayed (relay https://usw1-1.relay.n0.iroh.link./)

=== approval + agent survive controller restart (relayed path) ===
reconnected with no re-approval: approval survived the restart

=== controller self-reachability ===
{
  "reachability": {
    "checked_at": 1784974970,
    "ok": true,
    "path": "relayed (relay https://usw1-1.relay.n0.iroh.link./)"
  },
  "approved": 1,
  "pending": 0
}

=== no DNS, no network but loopback (initramfs conditions) ===
bothouse
egdod agent: connected to controller cb9f803f2091361b3f13bf66e401ef2d5c4fec8527385a06128721c5539d669e — path: direct-local (127.0.0.1:34671)
agent dialed, was approved, and execd with /etc/resolv.conf masked out

=== static musl agent binary ===
static musl binary present: /home/bot/.cache/botq-wt/1699-target/x86_64-unknown-linux-musl/release/egdod

=== DEMO PASS ===
```

The static binary was built with
`env -u RUSTC_WRAPPER cargo build --release --target x86_64-unknown-linux-musl`
(the wrapper unset is needed because cc-rs prefixes `RUSTC_WRAPPER` onto ring's
C compile and kache rejects a cross compiler's argv); `ldd` on the result
prints `statically linked` (25 MiB, unstripped). Zero `unsafe` in the crate.

## PROVED (observed in the pasted runs, or earlier runs of the same code)

- Agent dials out by baked NodeId; controller opens streams back over that
  same connection; all three primitives run over them. (Every demo phase.)
- Approve-before-anything: exec on an unapproved agent is refused; the agent
  is recorded in `pending` and `--json` emits it machine-readably.
- Approval survives controller restart with no re-approval (relayed phase,
  which is the restart-sensitive one: the agent re-found the restarted
  controller on its own).
- exec as root (the agent ran as root via sudo): stdout and stderr arrive on
  separate frames; the remote exit status (3, and 127 for spawn failure in the
  integration test) is returned.
- copy both directions: 64 MiB streamed, sha256 identical at both ends, mode
  751 preserved. Protocol appends a SHA-256 trailer verified before rename;
  transfer is chunked, never whole-file in RAM.
- forward: real bytes (`SSH-2.0` banner) through a local port to the target's
  sshd.
- ssh recipe: `mkdir`/`chmod` by exec, authorized_keys merged via pull+push
  (never clobbered — checked against this host's pre-existing file), sshd
  probed by exec, host key pulled over the egdod channel, known_hosts written
  from it, first connect succeeds under `StrictHostKeyChecking=yes
  BatchMode=yes` and returns `id -u` = 0.
- Path reporting, both ends, every session: `direct-local` on loopback,
  `relayed (relay https://usw1-1...)` when relayed, and the upgrade line
  `path changed: relayed -> direct-LAN` when holepunching succeeded mid-session.
- Controller self-reachability: `status --json` reported `ok: true` with the
  canary's observed path — and in a DNS-less network namespace the same canary
  reported the controller UNDIALABLE with precise per-service errors (run
  during development, not in the pasted transcript).
- No-DNS/no-userland operation: inside `unshare -n` with `/etc/resolv.conf`
  bind-mounted to /dev/null and only loopback up, the agent dialed via
  `--direct`, was approved, and execd — nothing but a kernel, one static
  binary, and UDP.
- The agent never gives up: it survived rejections, controller kills, and a
  controller restart across the demo; dial backoff is 1s doubling to 15s.

## INFERRED (not directly shown)

- Files larger than RAM: streaming is by construction (256 KiB chunks, no
  whole-file buffering anywhere) but the largest file actually moved was
  64 MiB.
- Throughput: not measured. The prior art measured 3–4 MB/s through the same
  public relay with the same transport shape; expect the same class.
- The ssh recipe on a *minimal* target: the demo target is a full NixOS with a
  running sshd. The recipe's ensure-sshd step (`sshd -t` → `ssh-keygen -A` →
  `sshd`) was not exercised against a target that lacked a config. A target
  with no openssh at all is out of the recipe's scope.
- `--relay <url>` with an IP-literal host as the no-DNS relay path: untested,
  and suspected NOT to work against n0's public relays (the wss certificate
  will not cover an IP literal). The proven no-DNS path is `--direct`; a
  self-hosted relay whose cert covers its IP should work (inference).
- The canary proves discovery-publish and relay reachability as seen from the
  controller's own host. A failure visible only from a *different* network
  (e.g. a NAT mapping lost while DNS publish stays fresh) would not be caught
  by it.
- The no-paths watchdog (agent declares the session dead after 15s with zero
  open paths) was added after observing one wedged session; the wedge was not
  reproduced deterministically, so the watchdog's trigger path is reasoned,
  not observed.
- Multi-agent concurrency: the registry's replace-vs-cleanup race (an evicted
  connection's cleanup removing the *new* registration) was observed once as a
  demo failure and fixed by tagging registrations with `stable_id`; the demo's
  rapid agent restarts exercise the overlap, but no dedicated many-agent test
  exists.

## Choices the spec left open (recorded per the contract)

- `egdod controller status [--json]` is an addition to the command surface:
  the spec requires the controller's reachability state be *exposed*; a file
  plus a reader command is that, made usable.
- Path labels: `direct-local`, `direct-LAN`, `relayed` per spec, plus a fourth
  `direct-public` for a direct path over a public IP — labeling that "LAN"
  would be the masquerade the spec forbids.
- `--state-dir` on every subcommand (controller default
  `$XDG_STATE_HOME/egdod`, agent default `/var/lib/egdod-agent`). In an
  initramfs, point the agent's at persistent storage or identity (and its
  approval) is per-boot — nothing in the image is secret, so the only cost of
  a per-boot key is re-approval.
- The agent needs from its environment, and nothing else: a Linux kernel with
  UDP, `getrandom(2)`, and (unless `--direct` is used) working DNS. No shell,
  no `/etc`, no dbus, no libc at runtime (static musl).
- The control socket is the trust boundary for local CLI commands; it lives
  under the state dir, which `init` creates mode 0700.

## Not tested

aarch64 controller build (property 6 is a design constraint — no fixed paths,
no systemd, all state under `--state-dir` — not a v0 deliverable). NAT-crossing
direct paths between two real machines. Hostile networks (TLS-intercepting
proxies, captive portals). Files > 64 MiB. More than one agent at a time.
ssh to a target whose sshd had to be started by the recipe.
