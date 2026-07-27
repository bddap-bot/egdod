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
nix-build musl.nix -o /tmp/egdod-musl-result                     # the static agent
EGDOD_MUSL_BIN=/tmp/egdod-musl-result/bin/egdod nix-shell --run './demo.sh'
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

`cargo clippy --all-targets` emits nothing but its own progress:

```
    Checking egdod v0.1.0 (/home/bot/.cache/botq-wt/1703)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 4.47s
```

The integration test is a real iroh
connection, not a mock: a controller serving, an agent dialling it by node id,
the gate refusing exec, push, pull *and* a connection through a forward to a
live listener before approval and serving them after, a 5 MiB round trip, a
missing pull reported as missing and an unreadable one deliberately not, bytes
through a forwarded port, and the session's own report of its path.

### The three primitives, and the gate

An unapproved agent is recorded pending and given nothing. Not "no exec" —
nothing:

```
=== 5. an unapproved agent gets nothing
Error: no agent is connected
Error: no agent is connected
Error: no agent is connected
Error: no agent is connected
WARN egdod::controller: forwarded connection failed: no agent is connected
exec, push, pull and forward all refused — the controller holds no session for an unapproved key
```

The four `Error:` lines are the exec attempt (printed twice, before and after the
file attempts) plus push and pull; the script also asserts that no file appeared
on either side. The `WARN` is the forward: a TCP connection was made through the
listener and the controller declined to route it. All of them read `no agent is
connected` because the controller holds no session for an unapproved key at all
— the gate is upstream of routing rather than a check inside it.

What the demo cannot show is more than that refusal, because nothing is
listening on the target port at that stage: "no bytes came back" would also be
true of a working forward. The discriminating check is in
`tests/integration.rs`, where a live echo listener is stood up *before* approval
and a connection through the forward is required to fail:

```rust
assert!(
    c.read_exact(&mut buf).await.is_err(),
    "an unapproved agent carried traffic to a live service"
);
```

**exec** keeps the streams apart and returns the status:

```
=== 7. PRIMITIVE exec — stdout and stderr stay separate, exit status comes back
exit status: 7 (expected 7)
stdout file: to-stdout
stderr file: to-stderr
```

**copy**, 64 MiB each way, digest and mode intact, in bounded memory:

```
pushed .../up.bin -> .../target/up.bin (67108864 bytes, sha256 b39d7de898133e2b3076728ce332a7ec6135e17233e244643036977acc632bbe)
b39d7de898133e2b3076728ce332a7ec6135e17233e244643036977acc632bbe  .../target/up.bin
pulled .../target/up.bin -> .../down.bin (67108864 bytes, sha256 b39d7de898133e2b3076728ce332a7ec6135e17233e244643036977acc632bbe)
mode: source 640, on the target 640, pulled back 640
agent peak RSS after a 64 MiB round trip: VmHWM:	   46792 kB
```

46 MB of RSS for a 64 MiB round trip is the observation behind "handles files
larger than RAM": the file never lands in memory. That figure is the *agent's*
peak; the controller's boundedness is by construction (a 64 KiB chunk in
`proto.rs`) and was not measured.

**forward**, proved by the bytes of a real service on the far side:

```
=== 10. PRIMITIVE forward — a local port onto the target's sshd, proven by its banner
forwarding 127.0.0.1:39857 -> 127.0.0.1:2299 on the target (ctrl-c to stop)
SSH-2.0-OpenSSH_10.4
```

**Approval survives a restart.** Step 11 kills the controller, removes its socket
and status file, starts a new one, and the agent redials and is served with no
second approval:

```
=== 11. approval survives a controller restart, and the agent redials on its own
reconnected without being approved again:
egdod: session with 815ad383... via direct-local (127.0.0.1:34254)
still-here
```

**exec bounds its own drain** without truncating a healthy command. The three
shapes, timed against a live agent:

```
=== A: orphan that keeps writing
real 0m16.138s   rc=0  bytes=243494898   "stopped reading" reported
=== B: daemon that writes nothing (the sshd pattern)
real 0m2.016s    rc=0  complete-output
=== C: ordinary command
real 0m0.013s    rc=5  plain
```

A is the pathological case — a command that exits leaving a writer on its pipes
— and it terminates with the output marked as possibly incomplete rather than
hanging. B is what `exec sshd` looks like and is unaffected. C is unchanged.

**exec really is root** when the agent runs as root:

```
=== 12. exec as root (only if this machine offers passwordless sudo)
approved 998f6b34d6fcc4aec8d4c467c15b0226a1b570b52c4e6142cd26495db1b92b11
uid seen by exec: 0
/etc/shadow is readable: exec really is root
```

The second line is `head -c 0 /etc/shadow` succeeding — the file was opened, not
read, which is enough to show the privilege without putting a hash in a log.

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
egdod: known_hosts entry from the authenticated channel: [127.0.0.1]:43229 ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIBmSWK/Z4npqMI7tt4azR15KTk0VR8qcnxZj4R9mRfsA
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
egdod: session with 2d3dce2b... via direct-lan (192.168.1.141:47072)
hello-from-a-node-id-alone
```

The labels themselves, from the target's own log:

```
INFO egdod::agent: connected to controller path=relayed remote=https://usw1-1.relay.n0.iroh.link./
INFO egdod::agent: path changed was=relayed now=direct-lan remote=192.168.1.141:36572
```

The session started on the relay, said so, holepunched to a LAN-routed direct
path, and said that too. Three of the four labels were produced by this run:
`relayed` and `direct-lan` against real infrastructure, `direct-local` from the
loopback steps.

### No DNS, no interfaces but loopback

The rest of the script avoids DNS by accident, because it hands the agent a
`--direct` address. Step 17 removes the accident: a fresh network namespace with
nothing but `lo`, and `/etc/resolv.conf` bind-mounted to `/dev/null`.

```
=== 17. no DNS at all, and no network but loopback — initramfs conditions
resolv.conf is: 0 bytes
egdod: session with d6ca9ed5... via direct-local (127.0.0.1:56871)
exec-with-no-resolver
```

### The demo leaves nothing behind

It installs an ssh key and starts an sshd. On a machine that is somebody's
actual computer, a demo that leaves either behind is a backdoor, so every key
and file it creates lives under one `mktemp -d` scratch directory which is
removed on every exit path — and the removal is checked, not assumed. The last
thing a run prints is what it left:

```
=== cleanup: what this run left on the machine
/root/.ssh/authorized_keys: unchanged (a0370156d0e729127533fd7d30ac595a63787123e9f2b4f2cd48a77adc86429e)
scratch directory removed: /tmp/nix-shell-2909619-2289422015/egdod-demo.27D9m7
no egdod or sshd processes left from this run
nothing installed by this run remains on the machine
```

The real `/root/.ssh/authorized_keys` is never a target — the recipe's default
is overridden to a file under the scratch directory — but its digest is taken
before and after regardless, because "we passed a flag" is an argument and a
digest is evidence. Verified independently of the script, from outside it:

```
BEFORE: a0370156d0e729127533fd7d30ac595a63787123e9f2b4f2cd48a77adc86429e
AFTER:  a0370156d0e729127533fd7d30ac595a63787123e9f2b4f2cd48a77adc86429e
```

**An EXIT trap alone was not enough, and this was found the hard way.** The
script is normally run inside `nix-shell`; killing the wrapper leaves the script
orphaned but running, and a `SIGKILL` on the script itself runs no trap at all
— which would strand an sshd listening with a freshly installed key. There is
now a `setsid`-detached janitor that waits for the script's pid to disappear and
reaps whatever is left, so the guarantee does not depend on this shell
surviving. Tested by `SIGKILL`ing the script mid-run, which no trap can catch:

```
demo.sh pid=2907307 — SIGKILL (no trap can possibly run)
waiting for the janitor...
AFTER:  a0370156d0e729127533fd7d30ac595a63787123e9f2b4f2cd48a77adc86429e
leftover procs:   0
leftover sshd:    0
```

### No secret in the image

```
egdod: controller key at .../controller/controller.key — bake the node id above into images; it is public
node id: 7fc188fe4763d73a8855c769095becf64b323fc018fb8b50e6d66322913e62fa
total 4
-rw------- 1 bot users 32 Jul 25 05:40 controller.key
```

The agent's entire configuration is `--controller <node id>` plus addressing
flags. It generates its own keypair on first run and prints only the public
half. Nothing secret is given to it, so nothing secret can be taken from it.

### The static agent completes the whole loop (property 4)

This was the spec failure this file used to carry. Upstream `noq-udp` (iroh's
UDP layer) sets `SO_TIMESTAMPNS` unconditionally on Linux and decodes the
resulting `SCM_TIMESTAMPNS` control message as a `libc::timespec` through a
helper asserting `align_of::<timespec>() <= align_of::<cmsghdr>()`. glibc gives
`cmsghdr` 8-byte alignment and the assertion holds; musl gives it 4 and the
static binary aborted on the first datagram it received. Reproduced on demand
(2026-07-27) by running the unpatched static build against a blackhole
controller address and feeding its bound UDP port one datagram from
`/dev/udp`:

```
thread 'tokio-rt-worker' (291251) panicked at /build/cargo-vendor-dir/noq-udp-1.1.0/src/cmsg/mod.rs:81:5:
assertion failed: align_of::<T>() <= align_of::<C>()
...
timeout: the monitored command dumped core   # rc=134
```

The fix is the vendored `vendor/noq-udp/` (crates.io 1.1.0 with unaligned cmsg
reads/writes and the asserts removed — `VENDOR.md` there has the provenance and
the exact deviation), applied via `[patch.crates-io]`. The same one-datagram
experiment against the patched build: no panic, the process ran until killed.
That settles what the previous revision of this file could only infer — the
unaligned read is *sufficient*, not merely the first thing in the way.

Sufficiency for the actual job was then demonstrated end to end: `demo.sh`
step 18 no longer settles for "did not abort" — it requires the fresh musl key
to be held pending, approves it, and execs through the static binary:

```
=== 18. the static agent — the binary this is supposed to boot on
/nix/store/3ggrv26zpdy17dlbf1lb5m6a0q8d7waw-...-musl-0.1.0/bin/egdod: ELF 64-bit LSB pie executable, x86-64, version 1 (SYSV), static-pie linked, not stripped
approved e987a6cf1be7e5fbb30fc34536e25aac5307fe34b601cdba2aba677153810476
the static binary connected, was approved, and served an exec:
  static-agent-served-exec

=== demo complete: every claim above was exercised, including the static agent
```

The regression is pinned twice: step 18 turns a returning abort back into a
GAP, and the vendored crate carries unit tests (`cargo test --manifest-path
vendor/noq-udp/Cargo.toml --lib`) that decode and encode through a musl-shaped
align-4 cmsghdr with a deliberately misaligned `timespec` payload — verified to
fail against the upstream code and pass against the patch.

## What the agent needs from its environment

`SPEC.md` property 4 asks for this to be written down here, and made explicit on
the command line where possible. The agent needs:

- **A working network interface, already up and addressed.** Nothing in egdod
  brings a link up, loads a driver, or speaks DHCP. This is the largest
  unstated dependency: on a bare target something else must have configured the
  NIC before the agent can dial.
- **A writable path for `--key-file`** (default `/var/lib/egdod/agent.key`), and
  its parent directory creatable. On a tmpfs initramfs this means a new identity
  per boot.
- **Nothing else.** No `/etc`, no shell, no dbus, no resolver — step 17 runs it
  with `/etc/resolv.conf` masked and no interface but `lo`.

Reaching a relay without DNS is what `--relay https://<ip-literal>/` is for:
`RelayChoice::Urls` takes the URL verbatim and, combined with `--direct`, the
agent installs neither a publisher nor a resolver. The URL parsing is unit
tested; the *connection* is not — see "Not tested at all" below.

### Deviations from the command surface

`SPEC.md` asks for deviations to be recorded. All eight commands it lists exist
with exactly the argument shapes it gives. The additions, none of which replace
anything specified:

- `controller status [--json]` — the spec requires the undialable state to be
  *exposed*; this is the exposure, and it exits 2 when the answer is "nobody can
  dial me".
- `agent --key-file <path>` — property 6 says no fixed system paths, so the
  agent's one piece of state is nameable too.
- `controller serve --bind/--probe-interval/--probe-timeout/--restart-after` —
  pinning the UDP port is what lets a `--direct` hint survive a restart; the
  probe knobs exist so the demo can force the undialable case.
- `controller ssh --user/--authorized-keys/--host-key/--sshd-arg/--keygen-arg/
  --target-addr/--local-port` — the recipe has to name paths, and argv rather
  than a command string, on a target with no shell to split one; the defaults
  are the real ones (`root`, `/root/.ssh/authorized_keys`, `sshd`,
  `127.0.0.1:22`) and the flags exist so the demo can point it at an
  unprivileged sshd instead.

## NOT PROVED

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
  here describes a relayed transfer. Over the public relay the demo exercises
  `exec` only — copy, forward and the ssh recipe all ran on loopback.
- **Reaching a relay without DNS.** `--relay https://<ip-literal>/` is the
  documented answer and its URL parsing is unit tested, but no connection has
  been made that way. The no-DNS phase that *was* run used `--no-relay
  --direct`, which avoids the question rather than answering it.
- **A hostile agent.** An approved agent declares both the length and the digest
  of a file on `pull`, so an oversized transfer does not merely get written
  before a check fails — it succeeds. No cap, no free-space check, and on the
  ssh path the staged file is then read whole into controller memory. Approved
  targets are untrusted hardware by design, so this is a real hole.
- **Many agents.** One or two at a time. The pending list's 256-entry bound has
  never been reached.
- **Revocation.** Removing a key from `approved` does not end a live session,
  and nothing tests what happens if you try.

## INFERRED

Beliefs with reasons and no observation behind them. Each is a candidate for the
next round of proving.

- **exec's drain behaves under a genuinely slow controller.** The three shapes
  that matter were measured (see PROVED above), but all of them on loopback. The
  case the drain is really designed for — a controller reading slowly enough
  that the writer stays blocked mid-send — was reasoned about, not reproduced.
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
