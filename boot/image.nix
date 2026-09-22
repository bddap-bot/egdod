{ controllerNodeId ? "0000000000000000000000000000000000000000000000000000000000000000"
, direct ? ""
, relay ? ""
, ip ? ""
, gw ? ""
, mask ? "255.255.255.0"
}:
assert direct != "" -> relay == "";
let
  pkgs = import ../nixpkgs.nix;
  agent = import ../musl.nix;

  deb = hash: sha256: name: pkgs.fetchurl {
    url = "https://snapshot.debian.org/file/${hash}";
    inherit sha256 name;
  };
  shim = deb "41c37e9daf9c9166ad3c98db4943f35b16e7ef77"
    "993868c31cca3eab054a0c51c124a4a8b3904f49fb002feb4b09cb9415200e74"
    "shim-signed_1.51+16.1-2_amd64.deb";
  grub = deb "737d3c9d7fc52f29640904817f951b4fe847c9d0"
    "da8bb31308a3682a7d9343fc42bf7f603ad85bb5483eeb3ece23e2f1be2e9bbd"
    "grub-efi-amd64-signed_1+2.12+9+deb13u2_amd64.deb";
  kernel = deb "4c3fb904f2cfac385f09702d8f2471b61871dfcb"
    "9ea27ad6d5d6e360f532e7faa49f653c4df870e98bf5552f93ee233eb280a9b8"
    "linux-image-6.12.96+deb13-amd64_6.12.96-1_amd64.deb";
  firmware = [
    (deb "85379150472201870d36bfd51f00023b85b1301a" "0iqmmbwxk06gjicblmnmcnw33amr5w2a627vps5n5wwh8pda8hhi" "firmware-amd-graphics_20260810-1_bpo13+1_all.deb")
    (deb "463499eda2691243eef9c28ea8f85a0962f9d7bc" "1gwr6almz8nvz7n2mm77qf72p0v25qp5prizrb79bxm7hc4ah1y9" "firmware-atheros_20260810-1_bpo13+1_all.deb")
    (deb "d3df9e997036d640f53da207d71fe1d2857ac2aa" "1nhcz3xapgmzwhxpjnyq8zsrx5qfgqmg4n35wprv14nl71i4c2fk" "firmware-bnx2_20260810-1_bpo13+1_all.deb")
    (deb "4ef86ace45a124e259c253e8451df3f71ddff9bb" "1sv1vrlxc48v0y26hnjaz3y95i1mf0c6qv76ac904r2hp5fh37va" "firmware-bnx2x_20260810-1_bpo13+1_all.deb")
    (deb "9299d8e3381930b580e5f504d6d96db42b530a5a" "14qmrn2pz691d3a420ln44yw3mg5idi84lbd6sqipapbi998ql2k" "firmware-brcm80211_20260810-1_bpo13+1_all.deb")
    (deb "118addc6f03bbb7c129bf00fc1c49f5507454d6a" "0rhkzh33yr9h5wyv8ca0rn7g3gbznvxrp2h8g12nybrmf1pk3f8z" "firmware-cavium_20260810-1_bpo13+1_all.deb")
    (deb "7358fcf0d23136bec4c4d171adf11ed8a636cd06" "091mq6hg9v859j7drjqsm9rhr99pd5909q26q7zg4m7cqndpvd9i" "firmware-cirrus_20260810-1_bpo13+1_all.deb")
    (deb "d91df40245e7409124d0f0fe38872706449828e9" "079mikwvv9m5gfl4jmvl6sr9z247448hc460ms36r0qsvxaiyjal" "firmware-intel-graphics_20260810-1_bpo13+1_all.deb")
    (deb "d6ea6351359dcf659e6ce6c0a93278b74132dee3" "06yii115qw2iyh8q473i7l2pr83vf4xc936g08kb4sxiy02ixs1a" "firmware-intel-misc_20260810-1_bpo13+1_all.deb")
    (deb "c6fd0b910a86cbc1d457b526ae426dadd9281d80" "07bad9g2740iy64asp5plmbj593hkphi070dp4bqkgs5ylk6wccl" "firmware-intel-sound_20260810-1_bpo13+1_all.deb")
    (deb "8da88f8ee4c3fda256c39ac0390ae80da7969f97" "17pjdgbnmm5qxa6ccp4zmh2y12g6yz54yrlj8anbidr21m38pr9g" "firmware-ipw2x00_20260810-1_bpo13+1_all.deb")
    (deb "e4a0acdfc69c94f621d735cde01d3293fbdce789" "07r7wcfsdi003x2n4jl4p9pn022av4ww19f4n4bw9aawiwf9121l" "firmware-ivtv_20260810-1_bpo13+1_all.deb")
    (deb "2fdd2ca2674e16bcd5e5b6bd1c6e0f6d1a64d331" "1cnly2sxvsh1d98a91l3y46v808jcriz38q41agsiazb797ld3mi" "firmware-iwlwifi_20260810-1_bpo13+1_all.deb")
    (deb "d2eb289f56168081562f0ac85c0836595fd1ebbc" "02llfn1wvyn10n0s0x6rnni7dzqb41w0nf4b9qijg1ciq7i9nrbh" "firmware-libertas_20260810-1_bpo13+1_all.deb")
    (deb "2363c6ce217aa75998e1518a60b171ce31cdf524" "002qiw23c966h8q35y3g7bjzkm7cn4gs9zdnf9s8zq9v3y195dir" "firmware-marvell-prestera_20260810-1_bpo13+1_all.deb")
    (deb "f961627f3304a775515eb5ecd73651540e762fd9" "0y0spj7xj92cq47yqi2xj3xn8k2a7gxfnrqgdb9grw26flbapqb3" "firmware-mediatek_20260810-1_bpo13+1_all.deb")
    (deb "3ea668ddfde3375456a0b1025364f6dcbbf4433c" "1n21h1xgg2daff9208b2559lqxd1vcwndkgxq05jjpp592v0jyb4" "firmware-misc-nonfree_20260810-1_bpo13+1_all.deb")
    (deb "c51eb1624683bd3efc3af5d24b964e3a4b2a96c9" "1yb524px9zc2rf6bx8hi7s1lqlpqz2awidsaicbw21348jcz0djz" "firmware-myricom_20260810-1_bpo13+1_all.deb")
    (deb "fad98c8453277e9946559ffd25cd1f003e4950d6" "16fhf15vjk0iqcxz680gi6gi9g6b58g9ylg9igkpgkdyn2kdmyhi" "firmware-netronome_20260810-1_bpo13+1_all.deb")
    (deb "d35cb5be047dcf2e3423db8240d31eda09015aeb" "11k3v483pkwjv1i0r5bgqyj0n1v16vfrwr6nzb2cgi7y7j3kdhvj" "firmware-netxen_20260810-1_bpo13+1_all.deb")
    (deb "2be70f7b0b1a7787e11d4e7afdad0899e43dc5b7" "1z7pv9c4z6zk6rr0bc81xi0ys10g8qiky58qlx48dggb97dqi6qs" "firmware-nvidia-graphics_20260810-1_bpo13+1_all.deb")
    (deb "8252d00ba253374911306b76d2e5ba929370e525" "01kalpy0li6k5zkp6vw7wcp3mh7kzqyrs7dndzf52bsh27r9yj65" "firmware-qcom-soc_20260810-1_bpo13+1_all.deb")
    (deb "91bcefc942d5b4550abb6d2ff7d704f1688faca6" "01618zi8mhhgf743l7j6pzzfa1f4i41m5wb2ma9m6gd01qgi59bx" "firmware-qlogic_20260810-1_bpo13+1_all.deb")
    (deb "1956cf838bd38450c822a969b9ea218de0c24357" "0000vg8mlkzs9wfy81abh2l1npn4hzryjxy6cyzxrfg594f9k789" "firmware-realtek_20260810-1_bpo13+1_all.deb")
    (deb "617bbcc9a86ca1d5f4af075eaa7ef96e876f158e" "0cdws3r6db93bpfpjzp7q702a93cjjmsasy9mlc6giq607203mry" "firmware-samsung_20260810-1_bpo13+1_all.deb")
    (deb "401921d5410114742b7df8438ea2e0ee717ff2e5" "1bwjwflj6i2d816bc25g1y29ykjbjxqrp3dpriv1ha1q8gllsbbz" "firmware-siano_20260810-1_bpo13+1_all.deb")
    (deb "cefc7472f6f348f612c484231e07b86708b9cf23" "0q5nr3dwq13p0bqgzn45fgqg4vl4crkp0awybbn0nwwmm17zhlb5" "firmware-ti-connectivity_20260810-1_bpo13+1_all.deb")
  ];

  krel = "6.12.96+deb13-amd64";
  bluetoothClosure = pkgs.closureInfo { rootPaths = [ pkgs.bluez pkgs.dbus ]; };
  dialArg = if direct != "" then " egdod.direct=${direct} egdod.norelay"
            else if relay != "" then " egdod.relay=${relay}"
            else "";
  netArg = if ip != "" then " egdod.ip=${ip} egdod.mask=${mask}" + (if gw != "" then " egdod.gw=${gw}" else "")
           else "";
  cmdline = "console=tty0 console=ttyS0,115200 egdod.controller=${controllerNodeId} egdod.mods=efivarfs" + netArg + dialArg;
  drivers = pkgs.stdenv.mkDerivation {
    name = "egdod-drivers";
    dontUnpack = true;
    nativeBuildInputs = [ pkgs.dpkg pkgs.xz ];
    buildPhase = ''
    set -euo pipefail
    mkdir x
    dpkg-deb -x ${kernel} x/kern
    for f in ${toString firmware}; do dpkg-deb -x "$f" x/fw; done
    KMOD=x/kern/usr/lib/modules/${krel}
    mkdir -p rootfs/lib/modules/${krel} rootfs/lib/firmware

    cp -r "$KMOD/kernel" "$KMOD/modules.order" "$KMOD/modules.builtin" "rootfs/lib/modules/${krel}/"
    ${pkgs.pkgsStatic.busybox}/bin/busybox depmod -b rootfs ${krel}
    tr ' ' '\n' < "rootfs/lib/modules/${krel}/modules.dep" | sed 's/:$//' | grep -v '^$' | sort -u | while read -r m; do
      [ -n "$m" ] && [ -e "rootfs/lib/modules/${krel}/$m" ] || { echo "module tree is missing dependency $m" >&2; exit 1; }
    done
    for m in mac80211_hwsim e1000 iwlmvm ath9k ath11k_pci rtw88_8822ce rtw89_8852be mt7921e brcmfmac r8169 cdc_ether; do
      grep -q "/$m.ko.xz:" "rootfs/lib/modules/${krel}/modules.dep" || { echo "module tree lacks $m" >&2; exit 1; }
    done

    cp -r x/fw/usr/lib/firmware/. rootfs/lib/firmware/
    find rootfs/lib/firmware -type l -print0 | while IFS= read -r -d ''' l; do
      t=$(readlink -f "$l" || true)
      if [ -f "$t" ]; then printf '%s\0%s\0' "$l" "$(realpath --relative-to="$(dirname "$l")" "$t")"; fi
      [ -d "$t" ] || rm "$l"
    done > links
    find rootfs/lib/firmware -type f -print0 | xargs -0 -P "$NIX_BUILD_CORES" -n 32 xz -6 --check=crc32
    while IFS= read -r -d ''' l && IFS= read -r -d ''' t; do ln -s "$t.xz" "$l.xz"; done < links
    find rootfs/lib/firmware -xtype l -print -quit | grep -q . && { echo "dangling firmware symlink" >&2; exit 1; }

    du -sh rootfs/lib/modules rootfs/lib/firmware > sizes.txt
    '';
    installPhase = ''
    mkdir -p $out
    cp x/kern/boot/vmlinuz-${krel} $out/vmlinuz
    cp -r rootfs/lib $out/lib
    cp sizes.txt $out/
    '';
  };

  initrd = pkgs.stdenv.mkDerivation {
    name = "egdod-initrd";
    dontUnpack = true;
    nativeBuildInputs = [ pkgs.cpio pkgs.gzip pkgs.pkgsStatic.stdenv.cc ];
    buildPhase = ''
    set -euo pipefail
    x86_64-unknown-linux-musl-gcc -static -O2 -o init ${./init.c}
    mkdir -p rootfs/bin rootfs/nix/store rootfs/etc/dbus-1 rootfs/etc/bluetooth
    cp init rootfs/init
    cp ${agent}/bin/egdod rootfs/egdod
    cp ${pkgs.pkgsStatic.busybox}/bin/busybox rootfs/bin/busybox
    cp ${pkgs.pkgsStatic.wpa_supplicant}/bin/wpa_supplicant rootfs/bin/wpa_supplicant
    cp ${pkgs.pkgsStatic.hostapd}/bin/hostapd rootfs/bin/hostapd
    while read -r path; do cp -a "$path" rootfs/nix/store/; done < ${bluetoothClosure}/store-paths
    ln -s ${pkgs.bluez}/bin/bluetoothd rootfs/bin/bluetoothd
    ln -s ${pkgs.dbus}/bin/dbus-daemon rootfs/bin/dbus-daemon
    cp ${./dbus-system.conf} rootfs/etc/dbus-1/system.conf
    cp ${./bluetooth.conf} rootfs/etc/bluetooth/main.conf
    cp ${./passwd} rootfs/etc/passwd
    cp ${./group} rootfs/etc/group
    cp ${./udhcpc.script} rootfs/bin/udhcpc.script
    chmod +x rootfs/bin/udhcpc.script
    cp -r ${drivers}/lib rootfs/lib
    ( cd rootfs && find . -print0 | cpio --null -H newc -o 2>/dev/null | gzip -1 ) > initrd.img
    cat ${drivers}/sizes.txt > sizes.txt
    du -sh rootfs/bin rootfs/nix/store initrd.img >> sizes.txt
    '';
    installPhase = ''
    mkdir -p $out
    cp ${drivers}/vmlinuz $out/vmlinuz
    cp initrd.img sizes.txt $out/
    '';
  };
in
pkgs.stdenv.mkDerivation {
  name = "egdod-stick";
  dontUnpack = true;
  nativeBuildInputs = [ pkgs.dpkg pkgs.mtools pkgs.dosfstools ];

  buildPhase = ''
    set -euo pipefail
    mkdir x
    dpkg-deb -x ${shim} x/shim
    dpkg-deb -x ${grub} x/grub
    SHIM=x/shim/usr/lib/shim/shimx64.efi.signed
    GRUB=x/grub/usr/lib/grub/x86_64-efi-signed/grubx64.efi.signed


    mkdir -p esp/EFI/BOOT esp/EFI/debian
    cp "$SHIM"    esp/EFI/BOOT/BOOTX64.EFI
    cp "$GRUB"    esp/EFI/BOOT/grubx64.efi
    cp ${initrd}/vmlinuz esp/vmlinuz
    cp ${initrd}/initrd.img esp/initrd.img

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

    MB=$(( $(du -sm esp | cut -f1) + 64 ))
    dd if=/dev/zero of=esp.img bs=1M count="$MB" status=none
    mkfs.vfat -n EGDOD esp.img >/dev/null
    ( cd esp && find . -type d ! -name . -printf '%P\n' ) | while read -r d; do mmd -i esp.img "::$d"; done
    ( cd esp && find . -type f -printf '%P\n' ) | while read -r f; do mcopy -i esp.img "esp/$f" "::$f"; done
    cat ${initrd}/sizes.txt > sizes.txt
    du -sh esp.img >> sizes.txt
  '';

  installPhase = ''
    mkdir -p $out
    cp esp.img $out/esp.img
    ln -s ${initrd}/initrd.img $out/initrd.img
    cp sizes.txt $out/sizes.txt
    printf '%s\n' "${cmdline}" > $out/cmdline.txt
  '';
}
