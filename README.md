# egdod

*In the beginning the machine was without form, and void.*

`egdod` is the first process to wake on a machine that has no operating system. It boots from a
generic USB stick, dials out to a controller you keep somewhere else, and hands that controller
three powers over the bare metal: **run a command as root**, **move a file either direction**,
**forward a TCP port**. Everything else — installing an OS, pivoting into a root filesystem pushed
over the wire, standing up an sshd — is built out of those three.

Nothing secret is baked into the image, so one stick provisions any number of machines. The target
dials out, so it works from behind NAT with no port forwarding, no PXE server, no DHCP options, and
no infrastructure at all when the two machines share a cable.

The target never needs a screen, a keyboard, or a human standing next to it. Approval happens on
the controller.

## The name

Stolen from Neal Stephenson's *Fall; or, Dodge in Hell*, in which the first process to achieve
consciousness inside an empty simulated world names itself Egdod — Dodge, backwards — and sets
about shaping the Land that everyone arriving later will inhabit. Those chapters are written in a
deliberately mock-mythic register, pitched somewhere between Genesis and a systems manual, and we
have not entirely resisted the influence.

## Status

Early, and the first paragraph above is the intent rather than the achievement. The protocol works:
three primitives, approval by public key, and the ssh recipe, all over real iroh connections — and
separately, a target that knows only a node id finding its controller through a public relay. See
`PROOF.md`, which separates what was watched happening from what is merely believed, and is careful
about which of those ran where. What does not work yet is the boot: the static binary meant for a
machine with no userland aborts on its first packet, and there is no image to put it on. `SPEC.md`
is the contract; `INTEGRATION.md` says how two competing implementations became this one, and what
is still owed.

## Try it

```sh
nix-shell --run ./demo.sh          # stands up both sides here and exercises everything
```

By hand:

```sh
egdod controller init                                  # prints the node id; bake that into the image
egdod controller serve &                               # long-lived
egdod agent --controller <nodeid>                      # on the target; prints its own pubkey
egdod controller pending --json                        # the key waiting to be let in
egdod controller approve <agent-pubkey>
egdod controller exec <agent> -- uname -a
egdod controller ssh  <agent>
```

`nix-build musl.nix` cross-builds the static agent — but read the failure section
in `PROOF.md` before trusting it.

## License

Dual MIT / Apache-2.0, at your option.
