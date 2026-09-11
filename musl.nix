# The agent's deployment target has no distro: no dynamic loader to speak of, no
# populated /etc, no shell. `nix-build musl.nix` produces the binary that is
# supposed to run there, so the claim can be checked with `file` rather than
# believed.
import ./default.nix { static = true; }
