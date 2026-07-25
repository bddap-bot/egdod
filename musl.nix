# The agent's deployment target has no distro: no dynamic loader to speak of, no
# populated /etc, no shell. `nix-build musl.nix` produces the binary that is
# supposed to run there, so the claim can be checked with `file` rather than
# believed.
let
  nixpkgs = builtins.fetchTarball {
    url = "https://github.com/NixOS/nixpkgs/archive/e2587caef70cea85dd97d7daab492899902dbf5d.tar.gz";
    sha256 = "14jrgz4z2m8n1c8qwcla44kdy9kd7x0xnwfyrajnyvnhkxbnnqf1";
  };
  pkgs = import nixpkgs { };
  musl = pkgs.pkgsCross.musl64;
  src = pkgs.lib.cleanSourceWith {
    src = ./.;
    filter = path: type:
      let base = baseNameOf (toString path);
      in pkgs.lib.cleanSourceFilter path type && base != "target" && base != "result";
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
