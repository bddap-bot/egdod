# INTEGRATION

Two implementations were written independently against `SPEC.md`, on `impl/kimi`
and `impl/opus`. Both branches remain; each carries its own `PROOF.md` and an
independent grade (`grade-kimi.md`, `grade-opus.md`). This file records what was
chosen and why. `PROOF.md` on this branch was re-earned from scratch — no claim
in it was inherited from either branch.

Both were read in full and both demos were run before deciding. The grades were
useful and were not trusted where they could be checked cheaply.

## The base: `impl/opus`

Judged on the spec's non-negotiables, not on style. Both implementations get the
shape right — dial-out only, no secret in the image, nothing needed at the
target, a serve daemon that is a dumb byte proxy so every command is an ordinary
client and `ssh` cannot become a privileged fourth primitive. They diverge on
what happens when something is wrong. `impl/opus` fails closed on an unreadable
approved list and bounds its pending list at 256 entries, where `impl/kimi`
resets its backoff on rejection and re-knocks at roughly 1 Hz forever into an
unbounded directory of pending files — and the node id is public by design, so
that is a disk-fill primitive against a controller the spec says may be a phone.
On property 5 it makes the anti-defect unrepresentable rather than merely
handled: it classifies only the *selected* path and returns `Unknown` rather
than rounding an unrecognised path up to "direct", where `impl/kimi` falls back
to an arbitrary path when none is selected. In the required ssh recipe it keeps
`Missing` distinct from `Err` on the wire, so a failed read of the target's
`authorized_keys` cannot be mistaken for an empty one — `impl/kimi` maps every
pull failure to `String::new()` and then writes the result back, which silently
destroys a target's existing root keys. And on the undialable-controller
requirement it does not merely log: it rebuilds the endpoint, and records whether
the probe exercised discovery or was handed the answer, so the reader knows what
the result is worth.

## Taken from `impl/kimi`

- **The demo that leaves loopback by default.** Both branches exercised the
  public relay, and both recorded it — an earlier draft of this file said
  `impl/opus` never ran its relay phase, which was simply false: its `PROOF.md`
  contains the transcript. The real difference is narrower. `impl/kimi` ran
  those phases by default and observed a relayed→direct upgrade as an event;
  `impl/opus` gated its phase behind `EGDOD_DEMO_RELAY=1`, so anyone running the
  script as written gets no relay coverage at all, and its account of the same
  upgrade is a reconstruction ("the likely explanation is…") rather than an
  observation. A phase that is off unless someone remembers a variable will
  eventually stop being run. Steps 15-17 are now on by default, and
  `EGDOD_DEMO_OFFLINE=1` prints what skipping them costs.
- **An end-to-end test inside `cargo test`.** `impl/opus` left `controller.rs`,
  `ssh.rs` and `main.rs` — about half the source — covered only by `demo.sh`,
  which needs sshd and a network. `tests/integration.rs` takes `impl/kimi`'s
  idea and its coverage list, rewritten against this API and driven through the
  control socket, so routing, the gate and the three primitives break the build
  rather than the demo.
- **Watching the path for the life of a session.** `impl/kimi`'s agent tracked
  path changes and printed them. `impl/opus` re-derived the path live for every
  routed command — that part was never stale — but its `status.json` snapshot
  refreshed only on the canary tick, so a session that holepunched (or fell back
  to the relay) a minute ago could still read as its first label there.
  `net::path_changes` is one implementation of the detection used by both roles.
- **`status` exiting non-zero when undialable.** The spec's intended operator is
  a program on a handset; it should not have to parse prose.
- **Running the static binary instead of describing it.** `impl/kimi`'s musl
  phase ran `file` on an artifact it never executed, and passed when the
  artifact was absent. Step 18 now runs it — and the abort that surfaced
  (noq-udp's cmsg alignment assert, egdod#3) is fixed by the vendored patch in
  `vendor/noq-udp/`; the step now requires the static binary to be approved and
  serve an exec.

## Rejected

From `impl/kimi`: the line-oriented control socket (typed messages are better
where both ends ship together); bounding a copy by the size `stat` reported,
which "verifies" a truncated read of a file whose size lies; a staging path made
with `with_extension`, which collides for any two files sharing a stem and is a
predictable target written as root; and its unbounded pending directory.

From `impl/opus`, kept but fixed rather than inherited: `exec` drained its output
pumps for a fixed two seconds *per pump* after the command exited and then
aborted them, silently truncating anything that outran the link while still
reporting `Exit(0)`; the drain now gives up only when the output side is
genuinely idle, is capped at fifteen seconds so an orphaned writer cannot hold
it forever, and a cut-off stream sends an explicit `Truncated` frame first. Its agent also loaded its key *before* the
retry loop and propagated the error, so a target whose key path was not yet
writable exited instead of retrying — a direct violation of property 7 on a
machine with nobody to restart it. The duplicate `set_mode` is gone.

Neither implementation's approval gate is checked per request; both check at
connect. Revocation therefore does not reach a live session. The spec does not
ask for revocation and it was left alone rather than half-built.

## Still missing against the spec

- **Nothing has been tested across two machines.** Both ends of every session
  were on one host, so NAT traversal — the reason the agent dials out — rests on
  the prior art rather than on this code.
- **No revocation**, and no way to signal or kill a running `exec`.
- **Nothing has run on aarch64**, so property 6 is respected by construction and
  unverified by observation.

## Booting a machine off a stick

`SPEC.md` puts image building out of scope for v0, so v0 is the protocol proven
on a machine that already booted. The boot itself is now built and watched, in
`boot/` and in `PROOF.md`. The pieces the earlier account listed as missing are there:
a kernel and an initramfs with the agent as `init`; the static binary that
survives a datagram; every driver Debian builds for that kernel plus the
non-free firmware they ask for, loaded by modalias, and both ways to bring the
link up, static and DHCP; and a stick image, produced by a nix derivation from a distro-signed
chain pinned by hash. `boot/run-boot.sh` boots it under OVMF with Secure Boot
enforcing, the agent dials out and is approved and runs as root, and the
initramfs receives a root filesystem over the wire and `switch_root`s into it.
`boot/write-stick.sh` writes the image to a real device with byte-for-byte
readback.

What is left is not in this repo's control: a boot on a physical Secure-Boot
machine, whose firmware is not OVMF, and a session across two genuinely separate
networks. The stick is written for that test; until it runs, "boots under Secure
Boot" means "boots under OVMF with Microsoft keys".

### Booting on Intel Macs

Intel Macs from 2012–2017 have no Secure Boot and show the shim as `EFI Boot` in
the Option-key startup picker. T2 models need Startup Security Utility configured
for external media and `No Security`; 2006–2007 models with 32-bit EFI are out of
scope. Apple USB Ethernet uses the in-kernel `asix` driver and Thunderbolt
Ethernet uses `tg3`. Broadcom wireless firmware is only partly covered by the
non-free set in the image, so wired Ethernet is the proven path on these machines.

## Onto a network without a cable

The v0 stick assumed Ethernet. A laptop with no port and no keyboard needs the
stick to find a network by itself, and there were two ways to give it one:
bake the credentials in, or have the target host an access point and take the
credentials over it. Both are in, in that order of preference, for a reason
that is about the controller rather than the target: a controller that joins
the target's access point gives up its own upstream — a laptop with one radio
then has no internet, and a phone reaches the target only from an app bound to
that network while its default route stays on cellular. So the baked network is
the primary path and the derived access point is the fallback for a target
that finds nothing it knows.

The trade the primary path makes is stated plainly: the network's SSID and PSK
sit on the stick in plaintext, in the initramfs. That is the same class of
artifact as an installer stick carrying a preseed file with a wifi password,
and it is why `write-stick.sh` bakes them at write time rather than the image
build carrying them — the image in the nix store stays credential-free
(`SPEC.md` property 1 is about the controller's secret; a stick with a wifi
password on it is a provisioning object to be treated like one). Whoever
writes the stick chooses what goes on it: the writing host's own network by
default, any number of `--network SSID PSK` besides.

The fallback needs no decision from anyone: the SSID, PSK, addresses and port
are all derived from the controller's public node id, so the controller
computes them from the key it already trusts and the target computes them from
the id it already carries. Joining that access point buys a link and nothing
else; approval by public key gates everything, exactly as on any network. It
was tempting to hand the credentials over as a new verb; instead the first
approved command is a plain `push` of a `wpa_supplicant.conf`, which the
target's init treats as the same file `write-stick.sh` would have baked. One
file format, one join path, one network bring-up in `boot/init.c`: wired, then
baked station, then access point, then BLE.

BLE adds no second agent or session path. The target advertises `egdod-` plus
the first eight hexadecimal digits of its node id. The controller supplies the
expected target id, an SSID and a PSK file:

```sh
egdod controller ble <agent-node-id> --network <ssid> --psk-file <path>
```

The peers prove the controller and target keys before an encrypted credential
frame can be installed. The target then stops Bluetooth and feeds the resulting
`wpa_supplicant.conf` back into the existing station transition. Its subsequent
IP dial and public-key approval are unchanged; proximity grants network
provisioning, never an egdod capability.

Wireless on unknown hardware needs firmware the kernel would otherwise fetch
from a distro. The image carries Debian's non-free set for Intel, Atheros,
Realtek, Broadcom, MediaTek and Marvell Libertas radios, compressed, and loads
drivers by modalias at boot instead of the fixed module list the wired stick
had. For a machine whose built-in radio is outside that set, the zero-code
fallback is a USB Ethernet adapter or a USB wireless dongle whose driver and
firmware are in it (`cdc_ether`, `r8152`, `ath9k_htc`, `rtl8xxxu`, `mt7921u`
and the like are all present). `PROOF.md` records what the set costs in bytes
and which drivers were actually exercised, which is fewer than it covers.

## A controller that is always serving

The written image names exactly one node id, so a persistent controller serves
it: `egdod controller serve` over the key the image was built against, kept
running by the host it lives on. Keys that land in `pending` are surfaced for
approval by that host, not by this repo. `default.nix` builds the host binary
that serves; `musl.nix` is the same derivation built statically for the target.
