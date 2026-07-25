# Pinned so a build here is the build the grader gets; matches the nixpkgs the
# host machine already has in its store, so entering this shell downloads nothing.
let
  nixpkgs = builtins.fetchTarball {
    url = "https://github.com/NixOS/nixpkgs/archive/e2587caef70cea85dd97d7daab492899902dbf5d.tar.gz";
    sha256 = "14jrgz4z2m8n1c8qwcla44kdy9kd7x0xnwfyrajnyvnhkxbnnqf1";
  };
  pkgs = import nixpkgs { };
in
pkgs.mkShell {
  # openssh/coreutils are demo.sh dependencies, not build dependencies.
  nativeBuildInputs = with pkgs; [ cargo rustc clippy gcc pkg-config openssh ];
}
