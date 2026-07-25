# egdod v0 — the contract

This is the normative spec. Two independent implementations are being written to it and will be
graded against it, so where this document is specific, be exactly that specific; where it is silent,
make the smallest defensible choice and record it in `PROOF.md`.

## What it is

One binary, two roles.

- **controller** — long-lived, runs where you are. Owns a secret key in a file. Its NodeId, derived
  from that key, is the *only* thing baked into a target image, and it is public.
- **agent** — boots on the bare target, typically as init inside an initramfs. Generates its own
  keypair on first run. Dials the controller's NodeId. Never listens for inbound connections.

Transport is [iroh](https://docs.rs/iroh) (QUIC). The agent dials out; the controller opens streams
back over that same connection. This direction is load-bearing: it is what makes one image work for
many machines behind arbitrary NAT.

## Non-negotiable properties

1. **No secret material in a published image.** The image carries a public NodeId and nothing else.
   A stolen stick must be worth nothing beyond the ability to knock on the door.
2. **No screen, no keyboard, no human at the target.** Any scheme requiring someone to read a code
   off the target's display, or type into it, is out of scope and will be graded as a failure.
   Approval happens on the controller.
3. **Approve-before-anything.** An unknown agent that dials in is recorded as *pending* and is given
   nothing — no exec, no files, no ports — until its public key is approved on the controller.
   Approval is by public key, must be scriptable (so an unattended controller can auto-approve a
   known key), and must survive controller restart.
4. **No distro, no userland.** The agent must run as a single static binary (musl) with no dbus, no
   NetworkManager, no shell, and no assumption that `/etc` is populated. Anything it genuinely needs
   from the environment must be documented in `PROOF.md` and, where possible, made explicit on the
   command line — including how a relay is reached without working DNS.
5. **Say which path you got.** Every session must report whether it is direct-local, direct-LAN, or
   relayed. A relayed connection silently masquerading as direct is a defect, not a detail.
6. **The controller is portable, including to a phone.** It must run unprivileged on aarch64 Linux
   with no systemd, no root, and no fixed system paths — all state under one directory named by
   `--state-dir` (default `$XDG_STATE_HOME/egdod`), so relocating a controller is copying a
   directory. The intended endpoint is a controller running in a terminal on a handset, driven by a
   program rather than a person, with no PC in the picture; nothing in v0 may foreclose that. So
   `pending` must also emit machine-readable output (`--json`).
7. **The agent never gives up.** It dials, retries with backoff, and keeps retrying forever; when an
   established connection drops it goes back to dialing. A controller may be undialable for minutes
   (see below), may be restarted, or may be a phone that fell asleep — none of which the target can
   observe, and all of which it must survive without a human touching it.

## The three primitives

Everything the controller can do reduces to these. Design them as the API; do not design ssh.

```
exec    run an argv on the target as root; stream stdout and stderr separately; return the exit status
copy    move a file in either direction, preserving mode; verify integrity; handle files larger than RAM
forward tunnel a local TCP listener on the controller to an arbitrary addr:port on the target
```

## Command surface

Shape it like this so the two implementations are comparable. Deviate only with a reason recorded in
`PROOF.md`.

```
egdod controller init                             # create the key; print the NodeId
egdod controller serve                            # accept agents; hold unapproved ones pending
egdod controller pending                          # list agents awaiting approval
egdod controller approve <pubkey>
egdod controller exec <agent> -- <argv...>
egdod controller push <agent> <local-path> <remote-path>
egdod controller pull <agent> <remote-path> <local-path>
egdod controller forward <agent> <local-port> <remote-addr:port>
egdod agent --controller <nodeid> [--relay <url> | --no-relay] [--direct <addr:port>]
```

## Required recipe: ssh, built on the primitives

Provide `egdod controller ssh <agent>` as a *thin composition* of the three primitives, not as a
special transport: install an authorized key, ensure an sshd is running, forward a local port to the
target's sshd, and write a `known_hosts` entry from a host-key fingerprint carried over the already
authenticated egdod channel. The first `ssh` must succeed under
`-o StrictHostKeyChecking=yes -o BatchMode=yes`; if it could have prompted or accepted on trust,
the recipe is wrong. (This has been observed working — see prior art.)

## Required: the controller must know when it is unreachable

Prior art hit this and it is the sharpest known hazard: a controller can come up reporting a healthy
relay and still be undialable by NodeId for minutes, while a target boots, dials, fails, and has no
screen with which to tell anyone. The controller must actively verify it is dialable and act when it
is not — republish, restart the endpoint, or at minimum log loudly and expose the state. A controller
that cannot tell the difference between "no targets today" and "nobody can find me" is incomplete.

## Explicit non-goals for v0

No OS installer, no disko or partitioning, no image building, no Secure Boot work, no BLE, no
target-hosted wifi AP, no web UI. Those are later increments and adding them will not earn credit.
No Android packaging either — property 6 constrains the design, it is not a v0 deliverable.

## Deliverables

- Rust, 2021+, dual MIT/Apache-2.0 (`LICENSE-MIT` and `LICENSE-APACHE` at the root).
- `cargo test` green, run in the foreground, with the output pasted in `PROOF.md`.
- A `demo.sh` that stands up a controller and an agent on one machine, approves the agent, and
  exercises all three primitives plus the ssh recipe, end to end, unattended.
- `PROOF.md`: a real transcript of that demo actually running, plus an explicit split between what
  you **proved** by observation and what you **inferred**. An unlabelled inference presented as
  fact is graded as a defect. State what you did not test.
- No `unsafe` without a comment justifying it.
- Comments explain **why**, never what the signature already says.

## Prior art you should read

`bddap/bothouse` contains a working proof of the hard part: `deck-control/src/bin/iroh-tunnel.rs`
implements dial/expose roles over iroh, and `hatch/RESULTS.md` records the measured result — an
outbound-only-NAT guest dialing a baked NodeId over a public relay, the controller opening streams
back to the guest's sshd, ssh exec plus file copy both directions, host keys delivered in band, and
throughput of roughly 3–4 MB/s through a public relay. Read both. Reusing that proven approach is
encouraged and is *not* what "no peeking" refers to.

## No peeking

A second implementation of this same spec is being written independently, on another branch. Do not
fetch it, read it, or look at its commits. Build your own. The graders will check.
