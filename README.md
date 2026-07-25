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

Early. See `SPEC.md` for what v0 is contracted to do, and `PROOF.md` for what has actually been
observed working rather than merely intended.

## License

Dual MIT / Apache-2.0, at your option.
