let pkgs = import ../nixpkgs.nix; in
pkgs.bluez.overrideAttrs (old: {
  pname = "btvirt";
  patches = old.patches ++ [ ./btvirt-serial-acl.patch ];
  configureFlags = old.configureFlags ++ [ "--enable-testing" ];
  buildPhase = "make -j$NIX_BUILD_CORES emulator/btvirt";
  installPhase = "install -D emulator/btvirt $out/bin/btvirt";
  outputs = [ "out" ];
  postInstall = "";
  doCheck = false;
})
