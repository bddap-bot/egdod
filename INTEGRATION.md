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

- **The demo that leaves loopback.** The decisive difference between the two
  branches was evidence, not code: `impl/kimi`'s demo exercised n0's public
  relay, discovery by bare node id, a relayed→direct path upgrade, and a no-DNS
  network namespace. `impl/opus` had a relay phase but behind
  `EGDOD_DEMO_RELAY=1`, so it never ran and its committed transcript is entirely
  `direct-local` — a phase that is off by default is a phase that does not
  exist. Steps 15-17 are now on by default, and `EGDOD_DEMO_OFFLINE=1` prints
  what skipping them costs.
- **An end-to-end test inside `cargo test`.** `impl/opus` left `controller.rs`,
  `ssh.rs` and `main.rs` — about half the source — covered only by `demo.sh`,
  which needs sshd and a network. `tests/integration.rs` is ported from
  `impl/kimi` and driven through the control socket, so routing, the gate and
  the three primitives break the build rather than the demo.
- **Watching the path for the life of a session.** `impl/kimi`'s agent tracked
  path changes and printed them; `impl/opus` labelled a session at connect and
  refreshed `status.json` only on the canary tick, so a session that holepunched
  (or fell back to the relay) a minute ago could still read as its first label.
  `net::path_changes` is one implementation of the detection used by both roles.
- **`status` exiting non-zero when undialable.** The spec's intended operator is
  a program on a handset; it should not have to parse prose.
- **Running the static binary instead of describing it.** `impl/kimi`'s musl
  phase ran `file` on an artifact it never executed, and was conditional on that
  artifact existing, so it could not fail. Step 18 now runs it — and it aborts.

## Rejected

From `impl/kimi`: the line-oriented control socket (typed messages are better
where both ends ship together); bounding a copy by the size `stat` reported,
which "verifies" a truncated read of a file whose size lies; a staging path made
with `with_extension`, which collides for any two files sharing a stem and is a
predictable target written as root; and its unbounded pending directory.

From `impl/opus`, kept but fixed rather than inherited: `exec` drained its output
pumps for a fixed two seconds after the command exited and then aborted them,
silently truncating anything that outran the link while still reporting
`Exit(0)`; the deadline now runs against a stall and a cut-off stream sends an
explicit `Truncated` frame first. Its agent also loaded its key *before* the
retry loop and propagated the error, so a target whose key path was not yet
writable exited instead of retrying — a direct violation of property 7 on a
machine with nobody to restart it. The duplicate `set_mode` is gone.

Neither implementation's approval gate is checked per request; both check at
connect. Revocation therefore does not reach a live session. The spec does not
ask for revocation and it was left alone rather than half-built.

## Still missing against the spec

- **Property 4 is not met.** The static musl agent builds and aborts on its
  first datagram — an alignment assertion in iroh's UDP layer that holds on
  glibc and not on musl. Root cause in `PROOF.md`; the fix is upstream, and
  vendoring a 3,550-line fork of a networking crate to force it was judged worse
  than saying so. Everything proved was proved with the glibc build.
- **Nothing has been tested across two machines.** Both ends of every session
  were on one host, so NAT traversal — the reason the agent dials out — rests on
  the prior art rather than on this code.
- **An approved agent can make the controller write unbounded data** by lying
  about a file's length on `pull`. Approved targets are untrusted hardware by
  design, so this is a real hole, not a theoretical one.
- **No revocation**, and no way to signal or kill a running `exec`.
- **Nothing has run on aarch64**, so property 6 is respected by construction and
  unverified by observation.

## How far from booting a machine off a stick

Further than the working demo suggests, and the missing pieces are mostly not
in this repo. `egdod` is the process that runs *after* something has already
brought a machine to the point of executing a Linux binary with a configured
network interface. To boot a stick you still need: a kernel and an initramfs
with this binary as `init`; a static binary that survives a datagram, which is
the gap above; NIC driver modules and something to bring the link up and get an
address, because nothing here does DHCP and the agent assumes an interface that
already works; and a stick image to put it all on. `SPEC.md` puts image
building, installers and partitioning out of scope for v0 deliberately — so the
honest statement is that v0 is the *protocol* proven on a machine that already
booted, and the stick is the next increment, not a remaining detail of this one.
