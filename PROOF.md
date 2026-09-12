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
bash boot/run-boot.sh                                            # boot under Secure Boot, end to end
EGDOD_BOOT_DHCP=1 bash boot/run-boot.sh                          # same, bringing the link up by DHCP
```

`demo.sh` steps 0-14 are hermetic. Steps 15-17 use n0's public relay and DNS.
Steps 12 and 17 need passwordless sudo; without it they announce they were
skipped. `EGDOD_DEMO_OFFLINE=1` skips 15-17 and prints what that costs.

## PROVED

### `cargo test` — 23 tests, green

```
running 22 tests
test net::tests::relay_addr_is_never_reported_as_direct ... ok
test net::tests::path_classification ... ok
test net::tests::relay_choice_flags ... ok
test pipe::tests::splices_both_directions_and_propagates_eof ... ok
test proto::tests::missing_and_unreadable_are_distinct_replies ... ok
test proto::tests::declared_over_the_ceiling_is_refused_unwritten ... ok
test proto::tests::msg_roundtrip_and_framing ... ok
test proto::tests::free_space_is_checked_before_anything_is_created ... ok
test proto::tests::lying_stream_is_cut_at_the_announced_length ... ok
test proto::tests::exec_output_past_the_ceiling_is_cut_and_reported ... ok
test proto::tests::exec_output_under_the_ceiling_arrives_whole_with_its_status ... ok
test agent::tests::agent_key_is_stable_across_runs ... ok
test ssh::tests::civil_dates_match_the_calendar ... ok
test ssh::tests::line_carries_options_and_utc_expiry ... ok
test ssh::tests::rerun_replaces_by_blob_and_keeps_strangers ... ok
test state::tests::approved_list_tolerates_comments_and_junk ... ok
test proto::tests::truncated_transfer_is_rejected ... ok
test proto::tests::corrupt_transfer_leaves_no_destination ... ok
test state::tests::key_is_stable_and_private ... ok
test state::tests::approval_is_by_pubkey_and_survives_restart ... ok
test proto::tests::copy_verifies_digest_and_preserves_mode ... ok
test net::tests::probe_of_a_nonexistent_endpoint_fails ... ok

test result: ok. 22 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 6.07s

     Running tests/integration.rs
running 1 test
test controller_and_agent_over_iroh ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 6.63s
```

`cargo clippy --all-targets` emits nothing but its own progress:

```
    Checking egdod v0.1.0 (/home/bot/.cache/botq-wt/3645)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.29s
```

The integration test is a real iroh
connection, not a mock: a controller serving, an agent dialling it by node id,
the gate refusing exec, push, pull *and* a connection through a forward to a
live listener before approval and serving them after, a 5 MiB round trip, the
same file refused under a 1 MiB `--max-bytes` with nothing staged, a
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

### A lying target cannot fill the controller

`pull` takes both the length and the digest from the target, and an approved
target is untrusted hardware, so the receiver carries its own ceiling.
`recv_file_body` — the one implementation behind `push` on the target and
`pull` on the controller — refuses a declaration over the ceiling before any
directory, staging file or byte exists; checks the staging filesystem's free
space (`statvfs` on the destination's directory) before the first read; and
cuts a stream that outruns its declared length, unlinking the staging file.
`pull --max-bytes` defaults to 256 MiB. The ssh recipe pulls `authorized_keys`
and the host key under a fixed 1 MiB and no longer reads either whole: the
merge streams line by line into the file it pushes back, and the host key is
read as one line.

Watched, in the `cargo test` run above:

- `declared_over_the_ceiling_is_refused_unwritten` — a 5-byte declaration
  under a 4-byte ceiling, with a body whose digest matches: refused, the
  directory stays empty, and the body is still unread on the stream
  afterwards. Remove the ceiling check and the transfer succeeds.
- `free_space_is_checked_before_anything_is_created` — a 2^60-byte
  declaration into a directory made unwritable: refused with the free-space
  message, nothing created, stream unread. Create the staging file before the
  check instead and the error becomes a permission failure.
- `lying_stream_is_cut_at_the_announced_length` — 4 bytes declared, a
  megabyte streamed: the sender gets through at most four 4 KiB writes before
  the receiver stops reading; no destination, no staging file.
- `copy_verifies_digest_and_preserves_mode` — a 200,000-byte transfer with the
  ceiling set exactly to its length still verifies its digest and mode.
- `tests/integration.rs` — the 5 MiB round-trip file pulled again over the
  real iroh session under a 1 MiB ceiling: refused on the declaration, no
  destination, no staging file in the directory.

`exec` is the other door: `exec_capture` keeps a command's whole output in
memory, and the ssh recipe captures two commands (host key generation, the
sshd start). `recv_exec_output` — the one receiver for exec frames, behind the
terminal command and the capture alike — takes a ceiling on the bytes accepted
across stdout and stderr: the first frame that would cross it is dropped
unwritten and the stream with it, reported as an error rather than a shorter
result. The terminal command passes `u64::MAX`, since it holds nothing; the
recipe's two captures pass 64 KiB, enough for the few lines either command
reports and small enough to matter on a handset.

Watched, in the same run:

- `exec_output_past_the_ceiling_is_cut_and_reported` — 1 KiB frames
  alternating stdout and stderr, streamed without end under a 4 KiB ceiling:
  the receiver returns the ceiling error holding at most 4 KiB across both
  sinks, and the sender gets fewer than 64 frames through before its writes
  fail. Remove the ceiling check and the receiver drains all 1024 frames and
  fails only for the missing exit status; count stdout alone and it holds
  8 KiB; drop frames past the ceiling instead of failing and the error never
  comes.
- `exec_output_under_the_ceiling_arrives_whole_with_its_status` — 25 bytes
  over three frames on both sinks under a ceiling of exactly 25: both buffers
  arrive whole and the exit status is 3. Make the check strict and it fails
  on the last frame.

Each test was falsified against its own mutation in a scratch checkout, named
in the landing commit: removing the guarded line turns the test red for the
stated reason, restoring it turns it green.

### The ssh recipe: first connect, no trust on first use

Nothing is pre-created. The recipe generates the target's host key with `exec`,
installs the authorized key with `copy` (pull, merge, push — an unreadable file
is distinguished from a missing one, so it cannot erase what it failed to read),
starts sshd with `exec`, reaches it through `forward`, and writes `known_hosts`
from a host key that crossed the already-authenticated egdod channel.

```
egdod: installed at .../target-authorized_keys: from="127.0.0.1,::1",expiry-time="202609012000Z" ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOuR9rCaf7hKATaMTgX3VKcISnXPN9yjjaB7DiN6WWd1 egdod-controller
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

**The line the recipe leaves behind is harmless by construction.** Nothing in
egdod removes it (the recipe has no teardown; on a real target it outlives the
session), so the line itself is pinned: `from="127.0.0.1,::1"` means sshd
accepts the key only from the target's own loopback — which is where the
`forward` exits — and `expiry-time` (default 10 minutes, `--key-ttl-secs`)
retires it by the target's clock. A rerun replaces the line by key blob rather
than appending. Step 9b runs the recipe twice and then offers the same key from
this host's LAN address; sshd refuses it (egdod#7):

```
target authorized_keys after two runs of the recipe:
from="127.0.0.1,::1",expiry-time="202609012000Z" ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOuR9rCaf7hKATaMTgX3VKcISnXPN9yjjaB7DiN6WWd1 egdod-controller
from 192.168.1.141 with the same key: bot@127.0.0.1: Permission denied (publickey).
```

sshd's side of that refusal, from the journal:
`Authentication tried for bot with correct key but not from a permitted host (host=192.168.1.141, ip=192.168.1.141, required=127.0.0.1,::1)`.
The controls: the same connect without `from=` on the line succeeds, and a
line whose `expiry-time` lies ten minutes in the past is refused with
`entry expired`.

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
a `setsid`-detached janitor that waits for the script's pid (paired with its
start time) to disappear and reaps whatever is left, so the guarantee does not
depend on this shell surviving. Its first version killed itself: its own argv
carried the scratch path, so its `pkill -f` matched the janitor before the
`rm -rf` line ran, and the earlier transcript here — "procs 0, sshd 0", no
"scratch removed" — was consistent with that without anyone noticing (egdod#7).
The path now reaches it through the environment. The step-12 root agent runs
under a root-side `timeout 300` as the backstop for the kill the janitor cannot
survive either (a cgroup kill takes both). Tested by `SIGKILL`ing the script
while the root agent is up, which no trap can catch:

```
demo.sh pid=2709593, in step 12, scratch /tmp/nix-shell-2709448-2653668780/egdod-demo.kvcVQ7
root agent up (uid 0):
2712513 .../sudo -n timeout 300 .../egdod agent --controller … --key-file /tmp/.../egdod-demo.kvcVQ7/root-agent.key --no-relay --direct 127.0.0.1:45777
2712518 timeout 300 .../egdod agent --controller … --key-file /tmp/.../egdod-demo.kvcVQ7/root-agent.key --no-relay --direct 127.0.0.1:45777
2712519 .../egdod agent --controller … --key-file /tmp/.../egdod-demo.kvcVQ7/root-agent.key --no-relay --direct 127.0.0.1:45777
sshd on :2299 before: 1
20:03:37Z SIGKILL sent to demo.sh (no trap can run)
after 2 s:
  scratch dir exists:   no
  egdod agents left:    0
  sshd on :2299:        0
  procs naming scratch: 0
  janitor left:         0
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
/nix/store/vjr7zx783n1f5dghf3vyl55xpm1d2cfi-...-musl-0.1.0/bin/egdod: ELF 64-bit LSB pie executable, x86-64, version 1 (SYSV), static-pie linked, not stripped
approved e9cdd8b67a602150f1500d833b63a388adefb5256645d1dbf9a87ab385d8e3c5
the static binary connected, was approved, and served an exec:
  static-agent-served-exec

=== demo complete: every claim above was exercised, including the static agent
```

The regression is pinned twice: step 18 turns a returning abort back into a
GAP, and the vendored crate carries unit tests (`cargo test --manifest-path
vendor/noq-udp/Cargo.toml --lib`) that decode and encode through a musl-shaped
align-4 cmsghdr with a deliberately misaligned `timespec` payload — verified to
fail against the upstream code and pass against the patch.

### The boot: Secure Boot on, dial-out, exec as root, and switch_root

`boot/run-boot.sh` was watched carrying the whole arc on one host, with the
agent inside a virtual machine and the controller outside it. `boot/image.nix`
builds the stick image from a distro-signed chain pinned by sha256 — Debian's
Microsoft-signed shim, Debian-signed grub and `vmlinuz` 6.12.96 — plus an
unsigned initramfs whose PID 1 is `boot/init.c`, which launches the static musl
agent. The image boots under OVMF with the Microsoft keys enrolled.

Secure Boot was enforcing, watched from three sides at once. The firmware and
shim: the boot reached grub and the kernel only through the signed chain (a
byte-flipped shim is refused by the firmware, a byte-flipped grub by shim — the
controls proven in the prior art this rides on). The kernel, on the serial
console: `EFI stub: UEFI Secure Boot is enabled`, `Kernel is locked down from
EFI Secure Boot`, `secureboot: Secure boot enabled`, and the Debian Secure Boot
CA loaded into the integrity keyring. And the firmware's own record, read from
inside the booted guest through efivarfs: the `SecureBoot` variable is
`06 00 00 00 01` — value `01`.

```
init: egdod PID 1 up, launching agent
agent pubkey: 033e2c67119981f7f51f1e9e040f6b356514cb26de46304903d7337b1a31d74d
...
uid seen by exec: 0
Linux (none) 6.12.96+deb13-amd64 #1 SMP PREEMPT_DYNAMIC Debian 6.12.96-1 x86_64 GNU/Linux
egdod: session with 033e2c67... via direct-lan (192.168.1.141:58356)
```

e1000 came up (statically, and separately by DHCP against qemu's own server —
`lease of 10.0.2.15 obtained`), the agent dialed out and printed its key on the
serial console, an unapproved agent was served nothing, and after approval the
controller ran commands as root inside the guest: `id -u` returned `0`, `uname`
and the baked command line came back, and the session reported a direct path,
never relayed.

Then the received-OS handoff, watched end to end: the controller pushed a
rootfs tarball (sha256 verified by the copy primitive), an `exec` mounted a
tmpfs and unpacked into it and armed `/switch.req`, and PID 1 moved that tmpfs
to `/` and exec'd the init inside it. The received init announced itself on the
serial console — `NEWROOT-INIT: switch_root landed; the received OS is PID 1
now` — and read `/proc/version` to show the same kernel, no kexec. The move is
`MS_MOVE` plus `chroot`, the one root-replacement lockdown permits: `pivot_root`
`EINVAL`s on an initramfs and `kexec` of an unsigned kernel is refused
`ENOKEY`, both established in the prior art.

Finally the image was written to a USB stick and read back: the sha256 of the
written span equals the sha256 of the image.

**Not watched here.** The vendor firmware half — this was OVMF, not a laptop's
own UEFI, whose `db` contents, Fast Boot and removable-media handling vary; the
stick settles that only when it is moved to real hardware and booted. And the
relay half of the stick's own configuration: the written stick is set for DHCP
and finds its controller by node id through the public relay, but only the
`--no-relay --direct` path and a DHCP lease were observed in the VM — the
relay dial-in on real hardware is the next thing to watch, not a thing watched.

### Drivers by modalias, every module and every firmware blob, and a relayed dial from the stick

The first stick written for a real machine booted its signed chain, brought the
kernel up under Secure Boot, started the agent, and then dialed nothing: the
image loaded exactly two modules named on its command line, `e1000` and
`efivarfs`, and the laptop's Ethernet chip was neither, so `udhcpc` never bound,
`/etc/resolv.conf` never existed, and every dial failed with `No addressing
information available`. A list of drivers is the wrong shape for an image meant
for unknown hardware.

The image now carries the complete Debian module set for its kernel — all of
`/lib/modules/6.12.96+deb13-amd64/kernel`, with `modules.dep`, `modules.alias`
and `modules.order` generated at build time — and the full non-free firmware set
from the same Debian snapshot (27 packages, pinned by hash like the kernel),
each blob compressed with `xz --check=crc32` so the kernel's firmware loader
reads it in place. `init` loads drivers by walking every device's `modalias`
under `/sys/bus/{pci,usb,sdio,platform,virtio}` through `modprobe`, again after
each second of the carrier wait so late-enumerating USB devices are caught, and
again before every re-link; `egdod.mods=` on the command line names only what a
modalias cannot express (`efivarfs`). The kernel's own module requests
(`/sbin/modprobe` for `crypto-ccm(aes)` when a wireless key is installed, for
instance) reach the same busybox.

What the image weighs, as built (`sizes.txt` in the derivation output):

| part | size |
|---|---|
| `/lib/modules` (complete, `.ko.xz` as Debian ships them) | 104 MB |
| `/lib/firmware` (27 non-free packages, xz-compressed in place) | 436 MB |
| `/bin` (busybox, wpa_supplicant, hostapd, static) | 23 MB |
| `initrd.img` | 537 MB |
| `esp.img` | 618 MB |

`boot/run-boot.sh` was watched twice with a NIC the old image could not have
driven and no module named anywhere:

- `EGDOD_BOOT_NIC=virtio-net-pci EGDOD_BOOT_RELAY=1`: the image carried no
  `egdod.direct` and no relay URL, the controller served through the public
  relay bound to loopback so nothing but the relay could reach it, and the
  target had to find it by node id. On the serial console:

```
EFI stub: UEFI Secure Boot is enabled.
init: link wired eth0
udhcpc: lease of 10.0.2.15 obtained from 10.0.2.2, lease time 86400
init: egdod PID 1 up, launching agent
egdod::agent: egdod agent starting controller=4d1ff39dea4aec04fa3899e5ad0a0ed3d87d14a5ff233564c3da5b349a16b321
agent pubkey: b600794aea0c4e8feb0da55c174ff63639ecbb81c39de95c0467d108ec3ad744
egdod::agent: connected to controller via relayed (https://usw1-1.relay.n0.iroh.link./)
egdod::agent: controller has not approved b600794aea0c4e8feb0da55c174ff63639ecbb81c39de95c0467d108ec3ad744 yet; retrying
egdod::agent: connected to controller via relayed (https://usw1-1.relay.n0.iroh.link./)
init: switch_root into /newroot exec /sbin/init
NEWROOT-INIT: switch_root landed; the received OS is PID 1 now
```

  And the controller's published record, resolved from this host while the
  target was dialing:

```
_iroh.jwx988xkjmsyj6tau8144noq4xc84fff9htuk3gd5jpujgosscoo.dns.iroh.link descriptive text "relay=https://usw1-1.relay.n0.iroh.link./"
egdod::controller: reachability confirmed: a stranger can dial us path=relayed mode=Discovery
egdod::controller: agent connected agent=b600794aea0c4e8feb0da55c174ff63639ecbb81c39de95c0467d108ec3ad744 path=relayed remote=https://usw1-1.relay.n0.iroh.link./
```

- `EGDOD_BOOT_NIC=e1000e`: the same image, a second driver found by modalias,
  DHCP, the direct dial, exec as root, Secure Boot on, switch_root.

```
e1000e: Intel(R) PRO/1000 Network Driver
e1000e: Copyright(c) 1999 - 2015 Intel Corporation.
e1000e 0000:00:01.0: Interrupt Throttling Rate (ints/sec) set to dynamic conservative mode
e1000e 0000:00:01.0 0000:00:01.0 (uninitialized): registered PHC clock
e1000e 0000:00:01.0 eth0: (PCI Express:2.5GT/s:Width x1) 52:54:00:12:34:56
e1000e 0000:00:01.0 eth0: Intel(R) PRO/1000 Network Connection
e1000e 0000:00:01.0 eth0: MAC: 3, PHY: 8, PBA No: 000000-000
init: 1 wired device(s), waiting up to 10s for a carrier
e1000e 0000:00:01.0 eth0: NIC Link is Up 1000 Mbps Full Duplex, Flow Control: Rx/Tx
init: link wired eth0
```

Which drivers were *exercised*: `e1000`, `e1000e`, `virtio_net` (this section),
`mac80211_hwsim` (below). The set *covers* every driver Debian builds for this
kernel; "covers" is an inference about the machine in front of you, "exercised"
is what was watched.

### No cable: a baked network, and the access point the target hosts when there is none

Both were watched under qemu with `mac80211_hwsim`, three virtual radios in one
VM: one stays with the target, the other two are moved into a network namespace
where a rig — an overlay `/init` that prepares the namespace, then `exec`s the
stick's own `init` — plays the other side. The target's `init` is the product
binary from the image, unchanged; the rig only supplies what a second machine
would: a radio hosting a network with DHCP, and a controller. The VM has no
wired device at all (`-nic none`).

**Station on a baked network** (`EGDOD_BOOT_LINK=station`). The rig hosts
`egdod-proof` on its first radio and serves DHCP on it; `boot/networks.sh
--no-host --network egdod-proof …` produced the same `wpa_supplicant.conf`
`write-stick.sh` would bake, and `boot/bake.sh` appended it to the image's
initrd the way `write-stick.sh` does. On the serial console:

```
RIG: hwsim radios: phy0 phy1 phy2; phy0 stays with the target, the rest go to the rig's netns
init: 0 wired device(s), waiting up to 10s for a carrier
RIG: netns radios: ap=wlan1 join=wlan2
RIG: hostapd: wlan1: AP-ENABLED  
RIG: proof network egdod-proof up on wlan1, controller serving on 0.0.0.0:52847
init: wlan0 scanning for a baked network, up to 30s
wlan0: CTRL-EVENT-CONNECTED - Connection to 02:00:00:00:01:00 completed [id=0 id_str=]
init: link station wlan0
udhcpc: lease of 10.99.0.10 obtained from 10.99.0.1, lease time 864000
init: egdod PID 1 up, launching agent
agent pubkey: db5016fcf9850b99d7e4f9ebc6d8ae060bde8e353b5932454bba45639d84e0b1
egdod::agent: connected to controller via direct-lan (10.99.0.1:52847)
RIG: agent pending over the baked network: db5016fcf9850b99d7e4f9ebc6d8ae060bde8e353b5932454bba45639d84e0b1
RIG: unapproved exec refused
egdod::agent: connected to controller via direct-lan (10.99.0.1:52847)
RIG: exec as root over the baked network: uid 0
RIG: target addresses: lo 127.0.0.1/8 wlan0 10.99.0.10/24 
RIG: station OK
```

**The access point** (`EGDOD_BOOT_LINK=ap`). Nothing baked, so the target
finds no wired carrier, no baked network in range, and hosts the derived access
point. The rig's controller `join`s it (`egdod controller join --iface wlan2`,
wpa_supplicant plus the derived link-local address), holds the agent pending,
refuses an unapproved exec, approves, runs as root over the access point, and
then `push`es a `wpa_supplicant.conf` for `egdod-proof` as the first approved
command. The target's `init` sees the file land, tears the access point down,
joins `egdod-proof` as a station, takes a lease, relaunches the agent with the
image's dial settings, and the same approved key is served again — this time
from a `10.99.0.0/24` address:

```
init: 0 wired device(s), waiting up to 10s for a carrier
RIG: hostapd: wlan1: AP-ENABLED  
RIG: proof network egdod-proof up on wlan1, controller serving on 0.0.0.0:49925
wlan0: AP-ENABLED 
init: link access-point egdod-294a8681 on wlan0 as 169.254.71.136; the controller dials in at 169.254.71.186:49925
init: egdod PID 1 up, launching agent
agent pubkey: 913f3646ec3343416928cc0959dc83163a70d06694e779268b9b177859d75dad
wlan0: AP-STA-CONNECTED 02:00:00:00:02:00
egdod::agent: connected to controller via direct-lan (169.254.71.186:49925)
RIG: agent pending over the target's access point: 913f3646ec3343416928cc0959dc83163a70d06694e779268b9b177859d75dad
RIG: unapproved exec refused
egdod::agent: connected to controller via direct-lan (169.254.71.186:49925)
RIG: exec as root over the target's access point: uid 0
RIG: target addresses: lo 127.0.0.1/8 wlan0 169.254.71.136/16 
RIG: join: egdod: joined egdod-294a8681 on wlan2 as 169.254.71.186/16; the target dials 169.254.71.186:49925. Ctrl-C to leave.
RIG: pushing credentials for egdod-proof as the first approved command
init: network credentials received over the access point
init: 0 wired device(s), waiting up to 10s for a carrier
init: wlan0 scanning for a baked network, up to 30s
wlan0: CTRL-EVENT-CONNECTED - Connection to 02:00:00:00:01:00 completed [id=0 id_str=]
init: link station wlan0
udhcpc: lease of 10.99.0.10 obtained from 10.99.0.1, lease time 864000
agent pubkey: 913f3646ec3343416928cc0959dc83163a70d06694e779268b9b177859d75dad
egdod::agent: connected to controller via direct-lan (10.99.0.1:49925)
RIG: exec as root over the credentialed network: uid 0
RIG: target addresses: lo 127.0.0.1/8 wlan0 10.99.0.10/24 
RIG: target holds a 10.99.0.0/24 lease: it left its access point for egdod-proof
RIG: ap OK
```

The derivation the two sides agreed on is `egdod link <node-id>` (`SPEC.md`),
pinned by `link::tests::known_vector` and checked by hand with `sha256sum` over
the label and the raw id.

Serial logs for both runs are the artifacts `boot-station-serial.log` and
`boot-ap-serial.log`.

## What the agent needs from its environment

`SPEC.md` property 4 asks for this to be written down here, and made explicit on
the command line where possible. The agent needs:

- **A working network interface, already up and addressed.** Nothing in the
  agent brings a link up, loads a driver, or speaks DHCP; on the stick that
  something is `boot/init.c`, which loads drivers by modalias and runs `udhcpc`
  before launching the agent.
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
- `controller ssh --key-options/--key-ttl-secs` — the options and lifetime on
  the installed authorized_keys line (defaults `from="127.0.0.1,::1"`, 600 s);
  the spec does not describe that line, so this is an addition, not a change.
  An off-target `--target-addr` needs `--key-options` changed to match.

## NOT PROVED

### Not tested at all

- **Two machines.** Both ends of every session above were on `bothouse`. NAT
  traversal between genuinely separate networks — the property the whole design
  exists for — is untested here. The prior art (`bddap/bothouse`,
  `hatch/RESULTS.md`) measured it working over this transport; this codebase has
  not.
- **`direct-wan`.** Three of the four labels were produced. A direct
  internet-routed path needs two hosts.
- **A boot on real hardware.** The image boots under OVMF with Secure Boot on,
  the agent runs as PID 1, and the switch_root handoff works (see PROVED above),
  but a laptop's own firmware is not OVMF. The stick is written; the boot on a
  physical Secure-Boot machine is the outstanding test.
- **A real radio.** Every wireless proof ran on `mac80211_hwsim`, whose driver
  needs no firmware and whose radios never lose each other. Whether a laptop's
  Intel, Realtek or Broadcom chip comes up from the firmware set, associates,
  and holds an access point is unwatched; the physical stick settles it.
- **A controller that actually needs the derived access point.** In the proof
  the controller's radio and the target's are two hwsim radios in one VM;
  `egdod controller join` was exercised there, on a station with no other
  network. A laptop losing its upstream when it joins the target — the reason
  the access point is the fallback — is a design argument, not an observation.
- **Baking from a host's own profile.** `networks.sh` was exercised only with
  `--no-host --network …`; the NetworkManager and wpa_supplicant readers have
  not run against a live profile.
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
- **Many agents.** One or two at a time. The pending list's 256-entry bound has
  never been reached.
- **Revocation.** Removing a key from `approved` does not end a live session,
  and nothing tests what happens if you try.

## INFERRED

Beliefs with reasons and no observation behind them. Each is a candidate for the
next round of proving.

- **The firmware set covers the radios it names.** `PROOF.md` lists the drivers
  exercised (`e1000`, `e1000e`, `virtio_net`, `mac80211_hwsim`); every other
  driver in the tree is present and its firmware alongside, and the kernel's
  in-place xz firmware load is a documented path, but no such driver has been
  watched requesting a blob from this initramfs.
- **The credential handoff on a real controller.** The proof pushed the file
  from a controller on the same hwsim medium; over a real radio the access
  point disappears under the controller the moment `init` acts, which is the
  intended order (join, then serve the target's redial) and untested.
- **exec's drain behaves under a genuinely slow controller.** The three shapes
  that matter were measured (see PROVED above), but all of them on loopback. The
  case the drain is really designed for — a controller reading slowly enough
  that the writer stays blocked mid-send — was reasoned about, not reproduced.
- **The endpoint rebuild fixes a genuinely undialable controller.** Step 12b
  proves the *reaction* — detection, the loud log, the rebuild. It does not
  prove the rebuild restores dialability, because the failure was forced with a
  zero deadline rather than by an unreachable network.
- **The free-space check on a filesystem that keeps no block accounting.**
  `statvfs` reports zero blocks on ramfs and on a plain initramfs, so the check
  returns early there rather than refusing every transfer; reasoned from the
  kernel's `simple_statfs`, not observed — no test here mounts a ramfs. The
  choice of `f_bfree` over `f_bavail` is reasoning too: on the target the
  writer is root and the reserve is its to use; on the controller it
  over-admits by that reserve and leaves the refusing to ENOSPC mid-stream,
  which already unlinks the staging file.
- **An initramfs agent needs approving once per boot.** The agent's key lives at
  `--key-file`; on a tmpfs that is gone at reboot, so a new key is generated and
  the target arrives pending again. Nothing secret is lost — the cost is a
  re-approval. Now observed rather than reasoned: each boot in `run-boot.sh`
  printed a fresh pubkey that had to be approved.
- **The canary's same-host blind spot.** In `Direct` mode the probe is handed
  the controller's own addresses and dials over loopback, so it proves the socket
  answers and nothing about whether a stranger elsewhere could reach it;
  `probe_mode` is in `status.json` precisely so a reader knows which claim they
  are being given. In `Discovery` mode the probe really did leave through n0's
  DNS and relay — but even there the dialer was on the same host as the
  controller.
