# egdod v0 — what was observed, and what was not

Branch `impl/opus`. Everything in §3 is the verbatim output of one `./demo.sh`
run on one machine (NixOS, x86_64, kernel 6.18.39, iroh 1.0.3, rustc from the
nixpkgs pinned in `shell.nix`), with only the run's scratch directory shortened
to `$DEMO`. Where a statement is an inference rather than something seen, it is
in §6 or labelled inline.

Reproduce: `nix-shell --run ./demo.sh` (`EGDOD_DEMO_RELAY=1` adds the last phase,
which needs internet).

---

## 1. Design in one page

**Identity is the iroh NodeId, and nothing else.** The agent generates a keypair
on first run and uses it as its iroh secret key, so the key the controller
approves is the key QUIC has already authenticated by the time a connection
exists. There is no application-level handshake to get wrong, no capability
token, no shared secret in the image. The image carries the controller's NodeId,
which is public.

**`serve` is a dumb proxy.** The long-lived controller authorises the agent and
then does nothing but copy bytes between a unix socket in the state directory
and a QUIC stream. Every short-lived command (`exec`, `push`, `pull`, `forward`,
`ssh`) speaks the request protocol itself, so that protocol has exactly one
implementation on each side and `ssh` is a *composition* of the primitives, not a
fourth primitive with privileged access to internals.

**One request per stream.** The stream is the request's channel; end of request
is stream FIN. `exec` frames stdout and stderr separately; `push`/`pull` send an
integrity header (mode, length, sha256) then the raw body, which is verified into
a staging file and renamed into place only if length and digest match; `forward`
acks and then the stream is a raw pipe.

---

## 2. `cargo test`, run in the foreground

```
$ nix-shell --run 'cargo test'
   Compiling egdod v0.1.0 (/home/bot/.cache/botq-wt/1700/repo)
    Finished `test` profile [unoptimized + debuginfo] target(s) in 12.65s
     Running unittests src/lib.rs

running 15 tests
test net::tests::path_classification ... ok
test net::tests::relay_addr_is_never_reported_as_direct ... ok
test proto::tests::missing_and_unreadable_are_distinct_replies ... ok
test net::tests::relay_choice_flags ... ok
test pipe::tests::splices_both_directions_and_propagates_eof ... ok
test proto::tests::msg_roundtrip_and_framing ... ok
test state::tests::approved_list_tolerates_comments_and_junk ... ok
test state::tests::approval_is_by_pubkey_and_survives_restart ... ok
test agent::tests::agent_key_is_stable_across_runs ... ok
test proto::tests::truncated_transfer_is_rejected ... ok
test proto::tests::corrupt_transfer_leaves_no_destination ... ok
test state::tests::key_is_stable_and_private ... ok
test proto::tests::overlong_transfer_is_cut_off ... ok
test proto::tests::copy_verifies_digest_and_preserves_mode ... ok
test net::tests::probe_of_a_nonexistent_endpoint_fails ... ok

test result: ok. 15 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 6.03s

     Running unittests src/main.rs
running 0 tests
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

   Doc-tests egdod
running 0 tests
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

`cargo clippy --all-targets` printed no warnings on the same tree. There is no
`unsafe` in this crate.

---

## 3. `./demo.sh` — the transcript

Note what the script itself filters, since the transcript inherits it: step 14 is
`grep -E 'agent connected|routed a session|reachability|UNDIALABLE|pending'` over
the controller log, tailed to the last 12 lines — it is a sample of the log, not
the whole log. Everything else below is complete.

```
=== 0. versions
egdod 0.1.0
OpenSSH_10.4p1, OpenSSL 3.6.3 9 Jun 2026

=== 1. controller init — the node id is the only thing an image needs, and it is public
egdod: controller key at $DEMO/controller/controller.key — bake the node id above into images; it is public
node id: 27deff44bffcbe13735271eed23d2687b1c2418dde8a3cb731a70f1f5475433e
total 4
-rw------- 1 bot users 32 Jul 24 23:33 controller.key

=== 2. controller serve (hermetic: --no-relay, so nothing leaves this machine)
controller direct address: 127.0.0.1:45777

=== 3. reachability canary — the controller dials its own node id from a fresh key
node id:   27deff44bffcbe13735271eed23d2687b1c2418dde8a3cb731a70f1f5475433e
dialable:  yes
probe:     Direct mode, last ok 1784961211
restarts:  0
agent:     none connected

=== 4. agent starts, dials out, and is held pending (nothing is served to it)
dee72fa1f6ba8eebce889ddafb2dcf3f3e111de18f426caca00dc7c04346a538  attempts=1 first_seen=1784961212 last_seen=1784961212
agent pubkey: dee72fa1f6ba8eebce889ddafb2dcf3f3e111de18f426caca00dc7c04346a538

=== 5. an unapproved agent gets nothing
Error: no agent is connected
Error: no agent is connected
Error: no agent is connected
Error: no agent is connected
exec, push, pull and forward all refused — the controller holds no session for an unapproved key

=== 6. approve by public key (scriptable), and watch the session appear
approved dee72fa1f6ba8eebce889ddafb2dcf3f3e111de18f426caca00dc7c04346a538
dee72fa1f6ba8eebce889ddafb2dcf3f3e111de18f426caca00dc7c04346a538
node id:   27deff44bffcbe13735271eed23d2687b1c2418dde8a3cb731a70f1f5475433e
dialable:  yes
probe:     Direct mode, last ok 1784961211
restarts:  0
agent:     dee72fa1f6ba8eebce889ddafb2dcf3f3e111de18f426caca00dc7c04346a538 via direct-local (127.0.0.1:57211)

=== 7. PRIMITIVE exec — stdout and stderr stay separate, exit status comes back
exit status: 7 (expected 7)
stdout file: to-stdout
stderr file: to-stderr

=== 8. PRIMITIVE copy — 64 MiB up, verified on the target, then back down
egdod: session with dee72fa1… via direct-local (127.0.0.1:57211)
pushed $DEMO/up.bin -> $DEMO/target/up.bin (67108864 bytes, sha256 d0fa2fedd0ac3ab12bef4d27cdb5b9e8996fb33d0ddd48688df2349cf666e2cb)
egdod: session with dee72fa1… via direct-local (127.0.0.1:57211)
d0fa2fedd0ac3ab12bef4d27cdb5b9e8996fb33d0ddd48688df2349cf666e2cb  $DEMO/target/up.bin
egdod: session with dee72fa1… via direct-local (127.0.0.1:57211)
pulled $DEMO/target/up.bin -> $DEMO/down.bin (67108864 bytes, sha256 d0fa2fedd0ac3ab12bef4d27cdb5b9e8996fb33d0ddd48688df2349cf666e2cb)
d0fa2fedd0ac3ab12bef4d27cdb5b9e8996fb33d0ddd48688df2349cf666e2cb  $DEMO/up.bin
d0fa2fedd0ac3ab12bef4d27cdb5b9e8996fb33d0ddd48688df2349cf666e2cb  $DEMO/down.bin
mode: source 640, on the target 640, pulled back 640
agent peak RSS after a 64 MiB round trip: VmHWM:	   46584 kB

=== 9. RECIPE ssh — host key generated and fetched over the egdod channel
egdod: session with dee72fa1… via direct-local (127.0.0.1:57211)
egdod: session with dee72fa1… via direct-local (127.0.0.1:57211)
egdod: installed an authorized key at $DEMO/target-authorized_keys
egdod: session with dee72fa1… via direct-local (127.0.0.1:57211)
egdod: no host key on the target; generating one with ["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", "$DEMO/sshd/ssh_host_ed25519_key"]
egdod: session with dee72fa1… via direct-local (127.0.0.1:57211)
egdod: session with dee72fa1… via direct-local (127.0.0.1:57211)
egdod: session with dee72fa1… via direct-local (127.0.0.1:57211)
2026-07-25T06:33:48.080795Z  WARN egdod::controller: forwarded connection failed: agent could not connect to 127.0.0.1:2299: connecting to 127.0.0.1:2299: Connection refused (os error 111)
egdod: no sshd answering; starting it with ["…/openssh-10.4p1/bin/sshd", "-f", "$DEMO/sshd/sshd_config"]
egdod: session with dee72fa1… via direct-local (127.0.0.1:57211)
egdod: session with dee72fa1… via direct-local (127.0.0.1:57211)
egdod: sshd is up: SSH-2.0-OpenSSH_10.4
egdod: known_hosts entry from the authenticated channel: [127.0.0.1]:46553 ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAINcIiD7MHfJHE8Q6s0eWnuMe22YyCricygx4C990poPr
egdod: session with dee72fa1… via direct-local (127.0.0.1:57211)
bothouse
known_hosts the controller wrote:
[127.0.0.1]:46553 ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAINcIiD7MHfJHE8Q6s0eWnuMe22YyCricygx4C990poPr

=== 10. PRIMITIVE forward — a local port onto the target's sshd, proven by its banner
forwarding 127.0.0.1:41097 -> 127.0.0.1:2299 on the target (ctrl-c to stop)
SSH-2.0-OpenSSH_10.4

=== 11. approval survives a controller restart, and the agent redials on its own
reconnected without being approved again:
egdod: session with dee72fa1… via direct-local (127.0.0.1:39920)
still-here

=== 12. exec as root (only if this machine offers passwordless sudo)
approved 76fb2e8995e042f3748d32fd0fa35091d30d201d36f29d47c44f36cf2c7d3447
egdod: session with 76fb2e89… via direct-local (127.0.0.1:53203)
uid seen by exec: 0
egdod: session with 76fb2e89… via direct-local (127.0.0.1:53203)
/etc/shadow is readable: exec really is root

=== 12b. what the controller does when it CANNOT be dialled
egdod: controller key at $DEMO/undialable/controller.key — bake the node id above into images; it is public
2026-07-25T06:34:32.832737Z ERROR egdod::controller: UNDIALABLE: cannot reach our own node id (probe timed out after 0ns). A target booting now would not find us. failures=1
2026-07-25T06:34:36.842564Z ERROR egdod::controller: UNDIALABLE: cannot reach our own node id (probe timed out after 0ns). A target booting now would not find us. failures=2
2026-07-25T06:34:36.842913Z ERROR egdod::controller: rebuilding the iroh endpoint: no dialer has been able to reach us
node id:   c5e0bb5e98c865e20838e115afb22c8597f72bf13ae954189c86ebba7dc65978
dialable:  NO — probe timed out after 0ns
probe:     Direct mode, last ok never
restarts:  1
agent:     none connected

=== 13. final controller status
{
  "node_id": "27deff44bffcbe13735271eed23d2687b1c2418dde8a3cb731a70f1f5475433e",
  "pid": 2133743,
  "updated_unix": 1784961269,
  "direct_addrs": [ "127.0.0.1:45777" ],
  "dialable": true,
  "probe_mode": "direct",
  "last_probe_ok_unix": 1784961268,
  "last_probe_path": "direct-local",
  "last_probe_error": null,
  "consecutive_probe_failures": 0,
  "endpoint_restarts": 0,
  "sessions": [
    { "agent": "76fb2e89…", "path": "direct-local", "remote": "127.0.0.1:53203", "since_unix": 1784961269 },
    { "agent": "dee72fa1…", "path": "direct-local", "remote": "127.0.0.1:39920", "since_unix": 1784961264 }
  ]
}

=== 14. controller log (paths reported for every session, canary results)
INFO egdod::controller: routed a session agent=dee72fa1… path=direct-local remote=127.0.0.1:57211
INFO egdod::controller: reachability confirmed: a stranger can dial us path=direct-local mode=Direct
INFO egdod::controller: reachability confirmed: a stranger can dial us path=direct-local mode=Direct
INFO egdod::controller: reachability confirmed: a stranger can dial us path=direct-local mode=Direct
INFO egdod::controller: reachability confirmed: a stranger can dial us path=direct-local mode=Direct
INFO egdod::controller: agent connected agent=dee72fa1… path=direct-local remote=127.0.0.1:39920
INFO egdod::controller: routed a session agent=dee72fa1… path=direct-local remote=127.0.0.1:39920
WARN egdod::controller: unapproved agent held pending; nothing served agent=76fb2e89…
INFO egdod::controller: reachability confirmed: a stranger can dial us path=direct-local mode=Direct
INFO egdod::controller: agent connected agent=76fb2e89… path=direct-local remote=127.0.0.1:53203
INFO egdod::controller: routed a session agent=76fb2e89… path=direct-local remote=127.0.0.1:53203
INFO egdod::controller: routed a session agent=76fb2e89… path=direct-local remote=127.0.0.1:53203

=== 15. relay + discovery: the agent knows only the node id
egdod: controller key at $DEMO/relay/controller/controller.key — bake the node id above into images; it is public
canary in discovery mode (dialled its own node id through n0 DNS + relay):
  "dialable": true,
  "probe_mode": "discovery",
  "last_probe_path": "relayed",
approved da7cd766a959aafab3324b5a38d80698a21e672a511d560d1ec59565dc8de471
egdod: session with da7cd766… via direct-lan (192.168.1.141:46802)
hello-over-the-internet
node id:   75a4844b4dca92539f8bfd9548b02a6092004b42ad16e1b28f88d1e324d4ce40
dialable:  yes
probe:     Discovery mode, last ok 1784961279
restarts:  0
agent:     da7cd766… via relayed (https://usw1-1.relay.n0.iroh.link./)

=== demo complete: all primitives, the recipe, and the canary exercised
```

Exit status 0.

---

## 4. PROVED — observed in that run

- **exec.** `sh -c 'echo to-stdout; echo to-stderr >&2; exit 7'` put `to-stdout`
  in the controller's stdout and `to-stderr` in its stderr — two separate files,
  each asserted to contain its own line *and* not to contain the other's — and
  the controller exited 7.
- **exec as root.** A second agent started under `sudo` reported `id -u` = 0 and
  successfully read `/etc/shadow` (0 bytes of it: the demo proves the access
  without printing the contents). The unprivileged agent used everywhere else
  executes as its own uid; this step is what makes "as root" a result rather than
  an assumption about deployment.
- **copy, both directions, with integrity and mode.** 64 MiB of `/dev/urandom`
  pushed, hashed *on the target* by `exec sha256sum`, pulled back: three
  identical digests, and mode 640 on the source, on the target, and on the file
  pulled back.
- **copy is streaming, not buffered.** The agent's peak RSS after moving 64 MiB
  twice was 46 MB — less than the file. That is what "handles files larger than
  RAM" reduces to on a machine where a bigger test is impractical.
- **forward.** A controller-side TCP listener carried the target's sshd banner
  (`SSH-2.0-OpenSSH_10.4`) back through the QUIC connection the agent dialled.
- **Approve-before-anything, for all three primitives.** Before approval the
  agent shows up in `pending`/`pending --json`, and `exec`, `push`, `pull` and a
  connection through `forward` were all refused (four `Error: no agent is
  connected` lines); the demo additionally asserts that no file appeared on
  either side and that the forwarded socket carried no bytes. The controller
  registers no session at all for an unapproved key.
- **Approval is by public key, scriptable, and survives restart.** `approve
  <pubkey>` appends to `<state-dir>/approved`. Step 11 killed the controller,
  started a fresh one on the same state directory, and the agent redialled and
  was served with no second approval. Also unit-tested at the file level,
  including through a reopened state directory.
- **The agent keeps trying.** Same step: it survived the controller vanishing and
  returning, untouched by a human, and before that it kept redialling through the
  whole pending period.
- **Every session reports its path.** Each command prints `egdod: session with
  <agent> via <path> (<addr>)`; the controller logs the same on connect and on
  every routed stream; `status`/`status --json` carries it per session. Values
  observed in this run: `direct-local`, `direct-lan`, `relayed`.
- **The ssh recipe, composed from the three primitives.** In one invocation it
  pulled the target's `authorized_keys` (absent), pushed one containing the
  controller's key (copy); pulled the host key (absent), generated it with
  `ssh-keygen` (exec), pulled it (copy); found nothing answering on the target's
  sshd port — the `Connection refused` warning is that probe going through the
  forward — started sshd (exec), waited for its banner; wrote `known_hosts` from
  the key that arrived over the authenticated channel; and ran ssh with
  `-o StrictHostKeyChecking=yes -o BatchMode=yes -o IdentitiesOnly=yes`, which
  printed `bothouse`. That connect could not have prompted and could not have
  accepted on trust.
- **The controller reacts when it cannot be dialled.** Step 12b forces every
  probe to fail with a zero-length deadline and shows the whole reaction chain:
  `ERROR UNDIALABLE …` twice, `dialable: NO` in `status`, and the endpoint
  rebuilt after `--restart-after` failures (`restarts: 1`). Note precisely what
  this proves: the *reaction*, not the detection — a zero deadline elapses before
  a packet is sent. Detection itself is proved in the other direction, by the
  canary succeeding: `reachability confirmed: a stranger can dial us`, from a
  throwaway endpoint with a fresh key, every probe interval. It was not shown
  that the rebuild *restores* dialability, because nothing here could make the
  loopback path fail for real.
- **Relay and discovery, for real.** Step 15 ran a controller on the public n0
  relays with DNS-based address lookup; its canary resolved and dialled its own
  NodeId through them — `probe_mode: discovery`, `last_probe_path: relayed`,
  which is a genuine end-to-end publish→resolve→dial — and an agent given nothing
  but the NodeId connected and ran a command. In the hermetic steps the canary
  runs in `Direct` mode, which is handed the controller's own addresses and by
  construction proves nothing about discovery; `status` says which mode it used.
- **No secret in the image.** The agent is started with a NodeId, a key path and
  addressing flags — no secret. It generates its own key on first run and prints
  the public half to stdout (`agent pubkey: …`), which is what a serial console
  captures and what gets approved.
- **The controller's state is one directory.** `--state-dir` holds the key (mode
  600, asserted by a test), `approved`, `pending.json`, `status.json`,
  `control.sock` and the ssh material the recipe manufactures. Nothing else is
  written except where a `pull` is explicitly told to write, nothing needs root.

Observed outside `demo.sh`, in the same session:

- **The agent runs with a literally empty environment.** `env -i egdod agent
  --controller <id> --key-file … --no-relay --direct 127.0.0.1:45799` connected
  and executed a command by absolute path; `/proc/<pid>/environ` was empty. So
  the agent needs no `PATH`, `HOME`, `XDG_*`, `SSL_CERT_FILE` or anything else to
  run — only the argv it is given.
- **A static musl binary builds.** `nix-build musl.nix` produces a `static-pie
  linked` 26,110,768-byte `egdod`, and `env -i ./egdod --version` runs. **It does
  not work — see §5.**

## 5. FAILED — found, root-caused, not fixed

**The musl build panics on the first datagram it receives.** Not this crate:
iroh's UDP layer unconditionally sets `SO_TIMESTAMPNS` and then decodes the
resulting `SCM_TIMESTAMPNS` control message as a `libc::timespec` (8-byte
aligned) while musl's `cmsghdr` is 4-byte aligned, so `noq-udp`'s alignment
assertion fires:

```
thread 'tokio-rt-worker' panicked at noq-udp-1.1.0/src/cmsg/mod.rs:81:5:
assertion failed: align_of::<T>() <= align_of::<C>()
  11: noq_udp::cmsg::decode::<libc::unix::timespec, libc::new::musl::sys::socket::cmsghdr, …>
  12: <noq_udp::imp::UdpSocketState>::recv
  15: <iroh::socket::transports::ip::IpTransport>::poll_recv
```

Reproduced by running the built binary as an agent against a working controller.
`noq-udp 1.1.0` is the newest published version (`cargo update` changes nothing).
It affects both roles under musl and neither under glibc; the fix belongs
upstream at `noq-udp/src/unix.rs:784` (skip the `SCM_TIMESTAMPNS` arm on musl, or
read it unaligned). I did **not** vendor a patched copy of iroh's UDP crate into
this repo — that trade is worse than reporting it. `musl.nix` stays because the
build itself is correct and starts working the moment upstream does.

So the spec's "single static binary (musl)" is delivered as a build and a running
CLI, but **not as a working agent today**, and everything in §4 was proved with
the glibc build.

## 6. INFERRED — believed, with the reason, not observed here

- **Dial-out from behind hostile NAT.** Both peers were on one machine, so
  holepunching and relay fallback were only exercised in the easy case. The prior
  art in `bddap/bothouse` (`hatch/RESULTS.md`) measured exactly this shape —
  outbound-only-NAT guest, baked NodeId, public relay — on the same iroh
  mechanism, which is why I expect it to hold.
- **Throughput over a relay.** Never measured here; the 64 MiB copy went over
  loopback. The prior art's ~3–4 MB/s through a public relay is the number to
  assume, with more on a direct path.
- **The relay session upgrading to a direct path.** In step 15 the `exec` printed
  `direct-lan` while the session snapshot said `relayed`. The likely explanation
  is that the connection started relayed and holepunched to the LAN path, and the
  snapshot is only refreshed each canary tick — but no before/after was captured,
  so this is a reconstruction from the code, not an observation.
- **`direct-wan` classification.** Unit-tested against synthetic addresses
  (loopback, RFC1918, link-local, ULA, a public v4), but no session in this run
  travelled a direct WAN path.
- **Behaviour as PID 1 in an initramfs.** The agent assumes no shell, no
  populated `/etc`, no dbus, no NetworkManager, and does run under `env -i` — but
  it was not booted as init inside an initramfs, and the binary that would run
  there is the musl one from §5.
- **Running on aarch64, or on a phone.** The controller needs no root, no systemd
  and no fixed paths beyond the `/proc/<pid>` liveness check noted in §8 — but it
  was only ever run on x86_64 Linux.
- **A file genuinely larger than RAM.** 64 MiB at 46 MB peak RSS shows the copy
  streams; nothing at 100 GB was attempted.

## 7. NOT TESTED — stated plainly

- Two agents on *different* machines. Every agent here was a local process.
- Concurrent commands to one agent. The protocol is one request per stream and
  streams are independent, but nothing drove them in parallel.
- `forward` to a non-loopback address on the target. Only `127.0.0.1:<sshd>` was
  forwarded, though the parameter is an arbitrary `addr:port`.
- The recipe's default paths (`/root/.ssh/authorized_keys`, `/etc/ssh/…`, `sshd`
  from `PATH`, port 22), and therefore the bare, spec-shaped `egdod controller
  ssh <agent>`. The demo overrides all of them so it can run unprivileged and
  leave nothing behind on the host. The defaults are plain clap defaults, so I
  expect that invocation to work on a real root target — expect, not observed.
- The default state directory (`$XDG_STATE_HOME/egdod`); every command in the
  demo passes `--state-dir`.
- The dial-failure backoff doubling to 30 s. Only the fast-retry paths ran.
- `PathKind::Unknown` never occurred; it exists so that an unclassifiable path is
  not rounded up to "direct", and no run produced one.
- Recovery from an `approved`/`pending.json` corrupted by something other than
  egdod, and from a truncated controller key (the agent replaces an unusable key,
  the controller refuses — neither branch was exercised end to end).
- IPv6-only environments, and a relay reached by IP-literal URL with DNS really
  broken. The flags exist and parse, and `--direct` removes the need for a
  resolver, but no test took DNS away.
- Very long-lived sessions, and a controller restarted mid-`push`.
- A cancelled `exec` does not kill the process it started on the target; that
  process leaks there until the agent exits. Known, not fixed.
- Revocation. Removing a key from `approved` stops the *next* connection; a live
  session survives until it drops. v0 has no `revoke`, and the spec does not ask
  for one.
- A setuid file through `copy` (the mode is passed through unmasked, see §10).

---

## 8. What the agent needs from its environment

This list is what the code actually requires, checked against the glibc build.

- **Nothing in the environment.** Observed: `env -i egdod agent …` runs with an
  empty `/proc/<pid>/environ`. No `/etc`, no shell, no dbus, no NetworkManager,
  no `$HOME`. TLS to the relay uses bundled roots, not `/etc/ssl`.
- **Entropy.** The key is generated with `getrandom(2)`, which *blocks until the
  kernel pool is seeded*. As PID 1 in an early initramfs that can mean waiting.
- **A writable path for its key** (`--key-file`, default
  `/var/lib/egdod/agent.key`). Missing parents are created (and, only when this
  call creates them, restricted to 0700). On a tmpfs initramfs the key is new
  every boot, so the agent needs approving every boot — a property of the medium,
  and the reason the flag exists.
- **A writable destination directory for `push`.** The copy primitive creates
  missing parents and stages the file next to its destination, so a read-only
  rootfs cannot receive files.
- **A UDP socket and a route out.** No inbound reachability, no port forward, no
  DHCP option, no PXE.
- **How a relay is reached without DNS**, in order:
  - `--direct <addr:port>` (repeatable) — the controller's socket address, which
    also switches off NodeId lookup entirely. Pair it with `serve --bind
    <addr:port>` so the port survives a controller restart. On its own this still
    leaves relay *hostnames* to resolve, so for a genuinely DNS-free target
    combine it with `--no-relay` or an IP-literal `--relay`.
  - `--relay https://<ip-literal>/` — a relay with no hostname to resolve.
  - `--no-relay` — no relay at all; only `--direct` addresses.
  - The default (neither flag) uses n0's public relays and DNS lookup. Note that
    with no `/etc/resolv.conf` iroh falls back to Google's public resolvers
    (8.8.8.8 / 8.8.4.4) — a bare target with the default configuration will
    therefore talk to a third party. Use the explicit flags if that matters.
- **For `exec`:** the argv runs directly, not through a shell, and `PATH` comes
  from the agent's own environment — under `env -i` there is none, so pass
  absolute paths. This is deliberate: a target with no userland has no shell to
  split a command line, which is also why `--sshd-arg`/`--keygen-arg` are
  repeatable rather than one string.
- **For the ssh recipe only:** an `sshd` and `ssh-keygen` on the target, plus
  `ssh` and `ssh-keygen` on the controller. The primitives need none of that.

## 9. Deviations from the spec's command surface, and why

The spec's surface is implemented as written. Additions and refinements:

- `egdod controller status [--json]` — the spec requires the controller to
  "expose the state" of its own reachability. This is that, plus the live
  sessions and their paths; the same data sits in `<state-dir>/status.json` for a
  program that would rather read a file. It warns when the process that wrote the
  file is gone, so a dead controller cannot look healthy.
- `serve --relay/--no-relay/--bind/--probe-interval/--probe-timeout/--restart-after`.
  `--no-relay` and `--bind` are what make the demo hermetic and repeatable; the
  probe knobs let it show the undialable reaction in seconds. Defaults match the
  spec's intent: n0 relays, a probe every 60 s, rebuild after 2 failures.
- `agent --key-file` — the agent must persist its identity somewhere.
- `controller ssh` flags (`--user`, `--authorized-keys`, `--host-key`,
  `--sshd-arg`, `--keygen-arg`, `--target-addr`, `--local-port`), all with
  defaults matching the spec's implied root/`/etc/ssh` layout.
- **The `<agent>` argument** accepts a full public key, any unique prefix of one,
  or `any` when exactly one agent is connected. Ambiguity is an error, never a
  guess.
- **`PathKind`** is `direct-local`, `direct-lan`, `direct-wan`, `relayed`,
  `unknown`. The spec names three; `direct-wan` is split out rather than
  labelling an internet-routed direct path as LAN, and `unknown` exists so a path
  this code cannot classify is never rounded up to "direct". Only the *selected*
  iroh path is reported: a relay path and a not-yet-selected direct candidate are
  routinely open at once, and reporting the wrong one is the defect the spec
  names.

## 10. Smaller decisions worth recording

- **Approval is a file, re-read on every connection attempt.** So `approve` needs
  no running controller, takes effect without a restart, and survives one. The
  pending list keeps only the 256 most recent keys: the NodeId is public, anyone
  can dial with a fresh key, and an unbounded list is a disk-filling primitive
  against a controller that might be a phone.
- **The canary answers any dialer** on a separate ALPN, with a fixed 11-byte
  constant, only after the dialer sends the matching 10-byte ping. It has to look
  like a stranger to prove anything, and the answer reveals nothing that dialing
  the socket did not already reveal.
- **The integrity header precedes the payload.** The sender reads the file twice
  (hash, then send) rather than sending a trailer, which would need a second
  framing layer around the bulk stream. Memory stays O(1) either way.
- **Received data is staged in a file created `O_EXCL` under a random name** next
  to its destination and renamed only after length and digest check out, with the
  write flushed and fsynced first. The agent is usually root, so a predictable
  staging name in a directory a local user can write to would be a write-anywhere
  primitive via a symlink.
- **`pull` distinguishes "not there" from "could not read it".** The wire type
  has a separate `Missing` reply for `NotFound`. Anything that reads, modifies
  and writes back a remote file — the ssh recipe and `authorized_keys` — would
  otherwise erase what it failed to read.
- **The file mode is passed through exactly, including setuid bits.** That is
  what "preserving mode" means, with a consequence worth naming: a compromised
  agent could make a `pull` land a setuid file on the controller. It would be
  owned by the controller's own user, so it grants that user nothing new.
- **The controller refuses to replace an unusable key; the agent replaces one.**
  A controller has an operator and its key is the NodeId baked into every image;
  a screenless target has neither, and a key truncated by a power cut on first
  boot must not brick it.
