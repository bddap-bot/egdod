# Build environment: rustc (with the musl std for the static agent binary) plus
# a musl cross-linker for ring's C bits. Pinned so the demo is reproducible.
let
  nixpkgs = fetchTarball {
    url = "https://github.com/NixOS/nixpkgs/archive/d6c71932130818840fc8fe9509cf50be8c64634f.tar.gz";
    sha256 = "1klgyhj98j3gfsql5sn9rapyx62qk5g8adk5zh9mnc4d0fj61gdr";
  };
  rust-overlay = import (fetchTarball {
    url = "https://github.com/oxalica/rust-overlay/archive/a163bd01a6e11ac58d4f8d11669da550b5afbbc6.tar.gz";
    sha256 = "19d7dz714klb1cz9l64w9sf7krwlk5fm349z8b409071c1ajb6p6";
  });
  pkgs = import nixpkgs { overlays = [ rust-overlay ]; };
  rust = pkgs.rust-bin.stable.latest.default.override {
    targets = [ "x86_64-unknown-linux-musl" ];
  };
  musl-gcc = pkgs.pkgsCross.musl64.buildPackages.gcc;
in
pkgs.mkShell {
  # NOTE: musl-gcc must NOT go in nativeBuildInputs — its setup hook overrides
  # stdenv's CC/CXX/AR and poisons host builds. Referenced by store path only.
  nativeBuildInputs = with pkgs; [ rust pkg-config perl openssh ];
  # ring compiles C for the musl target; point cc at the cross toolchain.
  CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER = "${musl-gcc}/bin/x86_64-unknown-linux-musl-gcc";
  CC_x86_64_unknown_linux_musl = "${musl-gcc}/bin/x86_64-unknown-linux-musl-gcc";
}
