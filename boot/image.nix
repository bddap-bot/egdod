{ controllerNodeId ? "0000000000000000000000000000000000000000000000000000000000000000"
, direct ? ""
, relay ? ""
, dhcp ? false
, ip ? "10.0.2.15"
, gw ? "10.0.2.2"
, mask ? "255.255.255.0"
}:
assert direct != "" -> relay == "";
let
  pkgs = import ../nixpkgs.nix;
  agent = import ../musl.nix;

  shim = pkgs.fetchurl {
    url = "https://snapshot.debian.org/file/41c37e9daf9c9166ad3c98db4943f35b16e7ef77";
    sha256 = "993868c31cca3eab054a0c51c124a4a8b3904f49fb002feb4b09cb9415200e74";
    name = "shim-signed_1.51+16.1-2_amd64.deb";
  };
  grub = pkgs.fetchurl {
    url = "https://snapshot.debian.org/file/737d3c9d7fc52f29640904817f951b4fe847c9d0";
    sha256 = "da8bb31308a3682a7d9343fc42bf7f603ad85bb5483eeb3ece23e2f1be2e9bbd";
    name = "grub-efi-amd64-signed_1+2.12+9+deb13u2_amd64.deb";
  };
  kernel = pkgs.fetchurl {
    url = "https://snapshot.debian.org/file/4c3fb904f2cfac385f09702d8f2471b61871dfcb";
    sha256 = "9ea27ad6d5d6e360f532e7faa49f653c4df870e98bf5552f93ee233eb280a9b8";
    name = "linux-image-6.12.96+deb13-amd64_6.12.96-1_amd64.deb";
  };

  krel = "6.12.96+deb13-amd64";
  dialArg = if direct != "" then " egdod.direct=${direct} egdod.norelay"
            else if relay != "" then " egdod.relay=${relay}"
            else "";
  netArg = if dhcp then " egdod.dhcp"
           else " egdod.ip=${ip} egdod.gw=${gw} egdod.mask=${mask}";
  cmdline = "console=tty0 console=ttyS0,115200 egdod.controller=${controllerNodeId}"
    + netArg + " egdod.mods=/e1000.ko,/efivarfs.ko" + dialArg;
in
pkgs.stdenv.mkDerivation {
  name = "egdod-stick";
  dontUnpack = true;
  nativeBuildInputs = [ pkgs.dpkg pkgs.mtools pkgs.dosfstools pkgs.xz pkgs.cpio pkgs.gzip pkgs.pkgsStatic.stdenv.cc ];

  buildPhase = ''
    set -euo pipefail
    mkdir x
    dpkg-deb -x ${shim} x/shim
    dpkg-deb -x ${grub} x/grub
    dpkg-deb -x ${kernel} x/kern

    SHIM=x/shim/usr/lib/shim/shimx64.efi.signed
    GRUB=x/grub/usr/lib/grub/x86_64-efi-signed/grubx64.efi.signed
    VMLINUZ=x/kern/boot/vmlinuz-${krel}
    MODS=x/kern/usr/lib/modules/${krel}/kernel

    x86_64-unknown-linux-musl-gcc -static -O2 -o init ${./init.c}

    mkdir -p rootfs
    cp init rootfs/init
    cp ${agent}/bin/egdod rootfs/egdod
    mkdir -p rootfs/bin
    cp ${pkgs.pkgsStatic.busybox}/bin/busybox rootfs/bin/busybox
    cp ${./udhcpc.script} rootfs/bin/udhcpc.script
    chmod +x rootfs/bin/udhcpc.script
    xz -dc "$MODS/drivers/net/ethernet/intel/e1000/e1000.ko.xz" > rootfs/e1000.ko
    xz -dc "$MODS/fs/efivarfs/efivarfs.ko.xz" > rootfs/efivarfs.ko
    ( cd rootfs && find . -print0 | cpio --null -H newc -o 2>/dev/null | gzip -9 ) > initrd.img

    mkdir -p esp/EFI/BOOT esp/EFI/debian
    cp "$SHIM"    esp/EFI/BOOT/BOOTX64.EFI
    cp "$GRUB"    esp/EFI/BOOT/grubx64.efi
    cp "$VMLINUZ" esp/vmlinuz
    cp initrd.img esp/initrd.img

    cat > cfg <<CFG
    set timeout=1
    serial --unit=0 --speed=115200
    terminal_input serial console
    terminal_output serial console
    menuentry "egdod" {
      linux /vmlinuz ${cmdline}
      initrd /initrd.img
    }
    CFG
    sed 's/^    //' cfg > esp/EFI/debian/grub.cfg

    dd if=/dev/zero of=esp.img bs=1M count=96 status=none
    mkfs.vfat esp.img >/dev/null
    ( cd esp && find . -type d ! -name . -printf '%P\n' ) | while read -r d; do mmd -i esp.img "::$d"; done
    ( cd esp && find . -type f -printf '%P\n' ) | while read -r f; do mcopy -i esp.img "esp/$f" "::$f"; done
  '';

  installPhase = ''
    mkdir -p $out
    cp esp.img $out/esp.img
    cp initrd.img $out/initrd.img
    printf '%s\n' "${cmdline}" > $out/cmdline.txt
  '';
}
