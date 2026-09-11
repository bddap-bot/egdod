{ pkgs ? import ./nixpkgs.nix, static ? false }:
let
  platform = if static then pkgs.pkgsCross.musl64.rustPlatform else pkgs.rustPlatform;
  # Prose, the demo harness, the boot image and repo metadata are not build
  # inputs: including them made every doc commit invalidate the (minutes-long)
  # cross build.
  src = pkgs.lib.cleanSourceWith {
    src = ./.;
    filter = path: type:
      let base = baseNameOf (toString path);
      in pkgs.lib.cleanSourceFilter path type
         && base != "target" && base != "boot" && base != "demo.sh"
         && base != "test-map.json" && base != ".gitignore"
         && !(pkgs.lib.hasSuffix ".md" base) && !(pkgs.lib.hasSuffix ".nix" base);
  };
in
platform.buildRustPackage ({
  pname = if static then "egdod-static" else "egdod";
  version = "0.1.0";
  inherit src;
  cargoLock.lockFile = ./Cargo.lock;
  nativeBuildInputs = [ pkgs.cmake pkgs.perl ];
  doCheck = false;
} // pkgs.lib.optionalAttrs static { RUSTFLAGS = "-C target-feature=+crt-static"; })
