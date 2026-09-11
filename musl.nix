# The agent's deployment target has no distro: no dynamic loader to speak of, no
# populated /etc, no shell. `nix-build musl.nix` produces the binary that is
# supposed to run there, so the claim can be checked with `file` rather than
# believed.
let
  pkgs = import ./nixpkgs.nix;
  musl = pkgs.pkgsCross.musl64;
  # Prose, the demo harness, and repo metadata are not build inputs: including
  # them made every doc commit invalidate this (minutes-long) cross build.
  src = pkgs.lib.cleanSourceWith {
    src = ./.;
    filter = path: type:
      let base = baseNameOf (toString path);
      in pkgs.lib.cleanSourceFilter path type
         && base != "target" && base != "result"
         && base != "demo.sh" && base != "test-map.json" && base != ".gitignore"
         && !(pkgs.lib.hasSuffix ".md" base);
  };
in
musl.rustPlatform.buildRustPackage {
  pname = "egdod-static";
  version = "0.1.0";
  inherit src;
  cargoLock.lockFile = ./Cargo.lock;
  nativeBuildInputs = [ pkgs.cmake pkgs.perl ];
  doCheck = false;
  RUSTFLAGS = "-C target-feature=+crt-static";
}
