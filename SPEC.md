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
egdod controller join [--iface <dev>]           # join the access point a target hosts for this key
egdod controller ble <agent> --network <ssid> <psk>
egdod link <nodeid> [--json | --hostapd <dev> | --supplicant]
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

## The stick image: compatibility over compactness

USB media are cheap and the target is unknown, so the image optimises for booting on whatever it
meets, never for size. It carries the complete Debian kernel module set — not a curated subset —
and the full non-free firmware set, pinned by hash like the kernel, and `init` loads drivers by
walking every device's modalias through modprobe rather than from a list. Size is never a reason
to drop a driver, a firmware blob, or a module tree; `PROOF.md` records what the image weighs as a
fact, not as a cost to reduce. `egdod.mods=` on the command line names only what a modalias cannot
express (`efivarfs`).

## Getting onto a network: wired, baked station, derived access point, BLE

v0 assumed a cable. The stick now brings one link up, in this order, with one code path
(`boot/init.c`), and dials exactly the same way over whichever it gets:

1. **Wired.** Every non-wireless device is brought up; the first with a carrier within a bounded
   wait (10 s) is configured — static if the command line carries `egdod.ip=`, DHCP otherwise.
2. **Station on a baked network.** `boot/write-stick.sh` bakes a `wpa_supplicant.conf` into the
   initramfs at write time (`boot/networks.sh`: the writing host's active wireless profile, plus
   any `--network SSID PSK`, several allowed). With no wired carrier, the wireless device joins the
   first baked network in range (30 s bound), takes DHCP, and dials as over Ethernet. The
   credentials sit on the stick in plaintext, the same trade an installer stick with a preseed makes.
3. **Derived access point.** With no wired carrier and no baked network in range, the target hosts
   an access point itself and the controller comes to it. Everything about that link is a function
   of the controller node id the image already carries, so both ends compute it and nothing is
   typed:

   ```
   h(label) = SHA-256(label ‖ node id as 32 raw bytes)
   SSID       = "egdod-" ‖ hex(h("egdod-link-ssid")[0..4])
   PSK        = h("egdod-link-psk")            (raw 256-bit WPA2 PSK, 64 hex; hostapd wpa_psk=)
   a          = h("egdod-link-addr")
   target     = 169.254.(1 + a[0] mod 254).(1 + a[1] mod 254)
   controller = 169.254.(same third octet).(1 + a[2] mod 254), bumped once if it collides
   port       = 49152 + (a[3] ‖ a[4] as big-endian u16) mod 16384
   prefix     = /16
   ```

   `egdod link <node-id>` prints all of it (`--json`, `--hostapd IFACE`, `--supplicant`);
   `egdod controller join --iface DEV` joins the access point for the controller's own key and
   holds it while `serve` runs bound to that port. The agent on the target dials
   `controller:port` directly, so the direction of the protocol is unchanged: the agent dials, the
   controller listens, approval is by public key exactly as on any other link. The PSK grants a
   link and nothing else — anyone with the public node id can compute it, which puts them where
   anyone on the target's LAN already is. The same holds in the other direction: anyone can host
   an access point with the derived name and key, and a controller that `join`s it has handed
   that stranger a link to itself and nothing more — the controller serves only agents it has
   approved, and `join` is a deliberate act on a network the operator chose to stand next to.

   The first thing worth doing over that link is handing the target a real network: `push` a
   `wpa_supplicant.conf` to `/wpa_supplicant.conf` on the target. `init` sees the file land, tears
   the access point down, joins as a station, takes DHCP, and relaunches the agent with the
   image's normal dial settings; the approved key survives, so the session resumes without a
   second approval.

   Vector, for an independent controller to check itself against: node id
   `ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c` gives SSID `egdod-dd18e097`,
   target `169.254.200.131`, controller `169.254.200.129`, port `58263`.

Wireless needs the vendor firmware the kernel would otherwise fetch from a distro: the image carries
Debian's non-free set for Intel, Atheros, Realtek, Broadcom, MediaTek and Marvell Libertas radios
(`boot/image.nix`, pinned by hash like the kernel), the wireless and Ethernet driver modules with
their dependencies, and loads drivers by modalias at boot. A USB Ethernet or wireless dongle whose
driver and firmware are in that set is the zero-code fallback for a machine whose built-in radio is
not.

### BLE is only the first hop

Bluetooth Low Energy is the last link fallback, after wired carrier, a baked station network and the
derived access point. It carries the target and controller node ids and one
`wpa_supplicant.conf`; it never carries an egdod session. Once the file arrives, the same init loop
stops Bluetooth, joins that network through the existing station path, takes DHCP and launches the
same IP dial as every other link.

The target advertises the fixed egdod GATT service with local name `egdod-` followed by the first
eight hexadecimal digits of its node id. `egdod controller ble <agent-node-id> --network SSID PSK`
scans for that service, connects only to the requested node id and proves both identities before it
sends credentials. Each side signs the protocol label, both node ids and its ephemeral X25519 key
with its iroh Ed25519 identity; the target accepts only the controller id baked into the image, and
the controller accepts only the agent id named on its command line. The shared X25519 secret and
the signed transcript derive a ChaCha20-Poly1305 key, so the network credentials are neither clear
text nor writable by a radio that lacks the controller key. Payloads are bounded at 64 KiB and a
credential file is installed atomically at mode 0600.

BlueZ supplies the radio and GATT implementation on both ends. The initramfs carries `bluetoothd`,
the D-Bus daemon it uses and their pinned closures; the existing binary supplies the GATT target and
controller subcommands. Those processes exist only while provisioning. The session agent remains
the same static executable and has no Bluetooth, D-Bus or second lifecycle: possessing the BLE link
grants no command, file or tunnel access, and approval by the agent's public key still gates all
three primitives after the ordinary IP dial.

## Explicit non-goals for v0

No OS installer, no disko or partitioning, no web UI. Image building, Secure Boot, the
target-hosted access point and BLE credential bootstrap were later increments and are now in (see
above and `boot/`).
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
