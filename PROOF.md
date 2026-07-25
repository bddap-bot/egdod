# PROOF

What was observed, and what was not. Written fresh for the merged implementation
on `main` — no claim here is inherited from `impl/kimi` or `impl/opus`.
Everything below was run by the integrator on `bothouse` (NixOS, x86-64, glibc)
on 2026-07-25, in the foreground, from this tree.

Two rules govern this file. Anything under **PROVED** was seen happening and its
output is reproduced. Anything under **INFERRED** is a belief with a stated
reason and no observation behind it. An unlabelled inference is a defect, and
this repo has paid for that twice.

## How to reproduce

```
nix-shell --run 'cargo test'
nix-build musl.nix -o /tmp/egdod-musl                     # the static agent
EGDOD_MUSL_BIN=/tmp/egdod-musl/bin/egdod nix-shell --run './demo.sh'
```

`demo.sh` steps 0-14 are hermetic. Steps 15-17 use n0's public relay and DNS.
Steps 12 and 17 need passwordless sudo; without it they announce they were
skipped. `EGDOD_DEMO_OFFLINE=1` skips 15-17 and prints what that costs.

## PROVED

### `cargo test` — 16 tests, green

```
running 15 tests
test net::tests::path_classification ... ok
test net::tests::relay_addr_is_never_reported_as_direct ... ok
test proto::tests::missing_and_unreadable_are_distinct_replies ... ok
test net::tests::relay_choice_flags ... ok
test pipe::tests::splices_both_directions_and_propagates_eof ... ok
test proto::tests::msg_roundtrip_and_framing ... ok
test state::tests::approved_list_tolerates_comments_and_junk ... ok
test agent::tests::agent_key_is_stable_across_runs ... ok
test proto::tests::truncated_transfer_is_rejected ... ok
test state::tests::approval_is_by_pubkey_and_survives_restart ... ok
test proto::tests::overlong_transfer_is_cut_off ... ok
test proto::tests::corrupt_transfer_leaves_no_destination ... ok
test state::tests::key_is_stable_and_private ... ok
test proto::tests::copy_verifies_digest_and_preserves_mode ... ok
test net::tests::probe_of_a_nonexistent_endpoint_fails ... ok

test result: ok. 15 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 6.03s

     Running tests/integration.rs
running 1 test
test controller_and_agent_over_iroh ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 6.49s
```

`cargo clippy --all-targets` is clean. The integration test is a real iroh
connection, not a mock: a controller serving, an agent dialling it by node id,
the gate refusing all three primitives before approval and serving them after,
a 5 MiB round trip, bytes through a forwarded port, and the session's own report
of its path.

### The three primitives, and the gate

An unapproved agent is recorded pending and given nothing. Not "no exec" —
nothing:

```
=== 5. an unapproved agent gets nothing
Error: no agent is connected
Error: no agent is connected
Error: no agent is connected
Error: no agent is connected
exec, push, pull and forward all refused — the controller holds no session for an unapproved key
```

Those four are exec, push, pull, and a live TCP connection through a `forward`
listener; the script separately asserts no file appeared on either side and that
the forwarded socket carried zero bytes. The refusal reads `no agent is
connected` because the controller holds no session for an unapproved key at all
— the gate is upstream of routing rather than a check inside it.

**exec** keeps the streams apart and returns the status:

```
=== 7. PRIMITIVE exec — stdout and stderr stay separate, exit status comes back
exit status: 7 (expected 7)
stdout file: to-stdout
stderr file: to-stderr
```

**copy**, 64 MiB each way, digest and mode intact, in bounded memory:

```
pushed .../up.bin -> .../target/up.bin (67108864 bytes, sha256 f9992be2c4e9370f98c14ef7d1c3764922c4750161cef8149c08a0a5cb589202)
f9992be2c4e9370f98c14ef7d1c3764922c4750161cef8149c08a0a5cb589202  .../target/up.bin
pulled .../target/up.bin -> .../down.bin (67108864 bytes, sha256 f9992be2c4e9370f98c14ef7d1c3764922c4750161cef8149c08a0a5cb589202)
mode: source 640, on the target 640, pulled back 640
agent peak RSS after a 64 MiB round trip: VmHWM:	   47028 kB
```

47 MB of RSS for a 64 MiB round trip is the observation behind "handles files
larger than RAM": the file never lands in memory.

**forward**, proved by the bytes of a real service on the far side:

```
=== 10. PRIMITIVE forward — a local port onto the target's sshd, proven by its banner
forwarding 127.0.0.1:38343 -> 127.0.0.1:2299 on the target (ctrl-c to stop)
SSH-2.0-OpenSSH_10.4
```

**Approval survives a restart.** Step 11 kills the controller, removes its socket
and status file, starts a new one, and the agent redials and is served with no
second approval:

```
=== 11. approval survives a controller restart, and the agent redials on its own
reconnected without being approved again:
egdod: session with ea8268e3... via direct-local (127.0.0.1:49325)
still-here
```

**exec really is root** when the agent runs as root: `uid seen by exec: 0`, and
it read `/etc/shadow`.

### The ssh recipe: first connect, no trust on first use

Nothing is pre-created. The recipe generates the target's host key with `exec`,
installs the authorized key with `copy` (pull, merge, push — an unreadable file
is distinguished from a missing one, so it cannot erase what it failed to read),
starts sshd with `exec`, reaches it through `forward`, and writes `known_hosts`
from a host key that crossed the already-authenticated egdod channel.

```
egdod: installed an authorized key at .../target-authorized_keys
egdod: no host key on the target; generating one with ["ssh-keygen", "-q", "-t", "ed25519", ...]
egdod: no sshd answering; starting it with [".../openssh-10.4p1/bin/sshd", "-f", ...]
egdod: sshd is up: SSH-2.0-OpenSSH_10.4
egdod: known_hosts entry from the authenticated channel: [127.0.0.1]:35999 ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIEZLEL46XtebWI7lniYepMB4ZUzb8cC2+w23MB21Ee24
bothouse
```

`bothouse` is `hostname` run over that ssh session, under
`-o StrictHostKeyChecking=yes -o BatchMode=yes -o IdentitiesOnly=yes` with a
private `UserKnownHostsFile`. It could not have prompted, and it could not have
succeeded via the user's own keys or known_hosts.

### The controller knows when nobody can dial it

Step 12b runs a controller with a zero-length probe deadline, which fails the
canary through the same path as a controller that has published itself and is
nevertheless unreachable:

```
ERROR egdod::controller: UNDIALABLE: cannot reach our own node id (probe timed out after 0ns). A target booting now would not find us. failures=1
ERROR egdod::controller: UNDIALABLE: ... failures=2
ERROR egdod::controller: rebuilding the iroh endpoint: no dialer has been able to reach us
dialable:  NO — probe timed out after 0ns
probe:     Direct mode, last ok never
restarts:  1
status exited 2 — a program can act on this without parsing prose
```

Detected, logged loudly, exposed in `status.json`, acted on (the endpoint is
rebuilt), and answerable by a script through the exit code.

### Discovery, a real relay, and the path labels

This is the configuration egdod is for, and the part loopback cannot test. A
controller on n0's public relay, its canary in **discovery** mode — dialling its
own node id through pkarr publish and n0 DNS, from a throwaway endpoint with a
fresh key, as a stranger would:

```
canary in discovery mode (dialled its own node id through n0 DNS + relay):
  "dialable": true,
  "probe_mode": "discovery",
  "last_probe_path": "relayed",
```

An agent was then given the node id and *nothing else* — no relay URL, no
address — and reached it:

```
egdod: session with ad1aefaa... via direct-lan (172.18.0.1:41078)
hello-from-a-node-id-alone
```

The labels themselves, from the target's own log:

```
INFO egdod::agent: connected to controller path=relayed remote=https://usw1-1.relay.n0.iroh.link./
INFO egdod::agent: path changed was=relayed now=direct-lan remote=172.18.0.1:52821
```

The session started on the relay, said so, holepunched to a LAN-routed direct
path, and said that too. Three of the four labels — `relayed`, `direct-lan`,
`direct-local` — were produced by this run against real infrastructure.

### No DNS, no interfaces but loopback

The rest of the script avoids DNS by accident, because it hands the agent a
`--direct` address. Step 17 removes the accident: a fresh network namespace with
nothing but `lo`, and `/etc/resolv.conf` bind-mounted to `/dev/null`.

```
=== 17. no DNS at all, and no network but loopback — initramfs conditions
resolv.conf is: 0 bytes
egdod: session with 81829d1f... via direct-local (127.0.0.1:37292)
exec-with-no-resolver
```

### No secret in the image

```
egdod: controller key at .../controller/controller.key — bake the node id above into images; it is public
node id: 768ac1c98a987b941cd81a98cae121775248215bb40b720c43c8cdb13d56934a
total 4
-rw------- 1 bot users 32 Jul 25 05:01 controller.key
```

The agent's entire configuration is `--controller <node id>` plus addressing
flags. It generates its own keypair on first run and prints only the public
half. Nothing secret is given to it, so nothing secret can be taken from it.

## NOT PROVED — and one of these is a spec failure

### The static agent does not work. Property 4 is not met.

`nix-build musl.nix` produces a genuine static binary, and `--version` runs:

```
/tmp/egdod-musl-result/bin/egdod: ELF 64-bit LSB pie executable, x86-64, version 1 (SYSV), static-pie linked, not stripped
$ env -i ./egdod --version
egdod 0.1.0
```

It aborts on the first datagram it receives, so it has never completed a
connection. Reproduced three times, and `demo.sh` step 18 now runs it rather
than describing it:

```
GAP: the static binary aborts on its first datagram (rc=134):
thread 'tokio-rt-worker' panicked at /build/cargo-vendor-dir/noq-udp-1.1.0/src/cmsg/mod.rs:81:5:
assertion failed: align_of::<T>() <= align_of::<C>()
```

Root cause, read in the dependency's source rather than guessed at: `noq-udp`
(iroh's UDP layer) sets `SO_TIMESTAMPNS` unconditionally on Linux
(`src/unix.rs:140`) and decodes the resulting `SCM_TIMESTAMPNS` control message
as a `libc::timespec` (`src/unix.rs:784`), through a helper that asserts
`align_of::<timespec>() <= align_of::<cmsghdr>()` (`src/cmsg/mod.rs:81`). glibc
gives `cmsghdr` 8-byte alignment and the assertion holds; musl gives it 4 and it
does not. This is a bug in a dependency, not in egdod, and the fix belongs
upstream — an unaligned read instead of an aligned one. Working around it here
means vendoring a 3,550-line fork of a networking crate, which was judged worse
than saying this plainly.

Every claim under PROVED was therefore demonstrated on a machine with a distro
on it. The binary intended for a machine without one has never talked to
anything.

### Not tested at all

- **Two machines.** Both ends of every session above were on `bothouse`. NAT
  traversal between genuinely separate networks — the property the whole design
  exists for — is untested here. The prior art (`bddap/bothouse`,
  `hatch/RESULTS.md`) measured it working over this transport; this codebase has
  not.
- **`direct-wan`.** Three of the four labels were produced. A direct
  internet-routed path needs two hosts.
- **An actual boot.** No initramfs, no image, nothing started from a stick. See
  `INTEGRATION.md` for the distance.
- **aarch64, and the phone.** Property 6 constrains the design — all state under
  one `--state-dir`, `pending --json`, `status --json`, no root, no systemd, no
  fixed paths — and the design respects it, but nothing was built or run on
  aarch64 and no controller has run on a handset.
- **Throughput over a relay.** The 64 MiB round trip was loopback. No number
  here describes a relayed transfer.
- **A hostile agent.** An approved agent declares a file's length on `pull` and
  the controller writes up to that length before the digest check fails. No cap,
  no free-space check. Approved targets are untrusted hardware by design, so
  this is a real hole.
- **Many agents.** One or two at a time. The pending list's 256-entry bound has
  never been reached.
- **Revocation.** Removing a key from `approved` does not end a live session,
  and nothing tests what happens if you try.

## INFERRED

Beliefs with reasons and no observation behind them. Each is a candidate for the
next round of proving.

- **The `cmsghdr` alignment difference is the whole musl story.** The assertion,
  both struct definitions and the socket option were read in the dependency's
  source, and the failure reproduces exactly where that predicts. But no patched
  build was made, so it is not established that fixing the alignment is
  *sufficient* — only that it is the first thing in the way.
- **exec's stall deadline behaves under a slow link.** The deadline resets
  whenever a frame reaches the wire, and a stalled drain emits `Truncated`
  before the exit status. Reasoned about, not demonstrated: no test drives a
  command that outruns its link.
- **The endpoint rebuild fixes a genuinely undialable controller.** Step 12b
  proves the *reaction* — detection, the loud log, the rebuild. It does not
  prove the rebuild restores dialability, because the failure was forced with a
  zero deadline rather than by an unreachable network.
- **An initramfs agent needs approving once per boot.** The agent's key lives at
  `--key-file`; on a tmpfs that is gone at reboot, so a new key is generated and
  the target arrives pending again. Nothing secret is lost — the cost is a
  re-approval. Untested, because nothing has booted.
- **The canary's same-host blind spot.** In `Direct` mode the probe is handed
  the controller's own addresses and dials over loopback, so it proves the socket
  answers and nothing about whether a stranger elsewhere could reach it;
  `probe_mode` is in `status.json` precisely so a reader knows which claim they
  are being given. In `Discovery` mode the probe really did leave through n0's
  DNS and relay — but even there the dialer was on the same host as the
  controller.
