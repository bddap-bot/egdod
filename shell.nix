# Pinned so a build here is the build the grader gets; matches the nixpkgs the
# host machine already has in its store, so entering this shell downloads nothing.
let
  pkgs = import ./nixpkgs.nix;
in
pkgs.mkShell {
  # openssh/coreutils are demo.sh dependencies, not build dependencies.
  nativeBuildInputs = with pkgs; [ cargo rustc clippy gcc pkg-config openssh ];
}
