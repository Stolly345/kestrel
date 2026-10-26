#!/bin/bash
# Build one sample per extractable extension (fileops::ARCHIVE_EXTS) into
# crates/kestrel/tests/fixtures/archives/, for the `every_format_extracts` test.
# The samples are committed; re-run this only to add or rebuild formats.
#
# Every sample holds hello.txt ("Hello from Kestrel") and sub/data.txt, except where the format
# can't (packages hold their own layout, disk images hold a file system, CHM/LZH are downloaded).
# Tools: sudo apt install zip arj rar gcab xar wimtools msitools rpm genisoimage \
#   squashfs-tools xorriso qemu-utils android-sdk-libsparse-utils ncompress zstd cpio
set -u
here=$(cd "$(dirname "$0")" && pwd)
root=$(dirname "$here")
out=$root/crates/kestrel/tests/fixtures/archives
z7=${KESTREL_7ZZ:-$root/packaging/7zip/7zz}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$out"

p=$work/payload # the contents of every sample
mkdir -p "$p/sub"
printf 'Hello from Kestrel\n' >"$p/hello.txt"
printf 'Kestrel fixture data\n' >"$p/sub/data.txt"
big=$work/big # same plus 200 KB of noise, so split archives really split
cp -r "$p" "$big" && head -c 200000 /dev/urandom >"$big/sub/noise.bin"

made=0 skipped=()
# step EXT TOOL... -- COMMAND: run COMMAND (in $work) if every TOOL exists, else note the skip
step() {
    local ext=$1 t
    shift
    while [ "$1" != -- ]; do
        command -v "$1" >/dev/null || [ -x "$1" ] || { skipped+=("$ext (needs $1)"); t=skip; }
        shift
    done
    shift
    [ "${t:-}" = skip ] && return
    if (cd "$work" && eval "$@") >"$work/log" 2>&1; then
        made=$((made + 1))
    else
        skipped+=("$ext (failed: $(tail -1 "$work/log"))")
    fi
}
o() { echo "$out/$1"; } # path of sample $1

# rebuild everything except the downloaded samples (lzh, chm, oldrar)
find "$out" -maxdepth 1 \( -name 'sample.*' -o -name 'split.*' \) ! -name sample.lzh ! -name sample.chm -delete

# tarballs
step .tar tar -- 'tar -cf $(o sample.tar) -C $p .'
for e in .tar.gz .tgz .tpz; do step $e gzip -- "gzip -9nc $(o sample.tar) >$(o sample$e)"; done
for e in .tar.xz .txz; do step $e xz -- "xz -c $(o sample.tar) >$(o sample$e)"; done
for e in .tar.zst .tzst; do step $e zstd -- "zstd -qc $(o sample.tar) >$(o sample$e)"; done
for e in .tar.bz2 .tbz2 .tbz; do step $e bzip2 -- "bzip2 -c $(o sample.tar) >$(o sample$e)"; done
for e in .tar.z .taz; do step $e compress -- "compress -c $(o sample.tar) >$(o sample$e)"; done
step .ova tar -- 'cp $(o sample.tar) $(o sample.ova)'

# archives
step .7z "$z7" -- '(cd $p && $z7 a -bso0 $(o sample.7z) .)'
step .zip zip -- '(cd $p && zip -qr $(o sample.zip) .)'
step .zipx "$z7" -- '(cd $p && $z7 a -bso0 -tzip -mm=LZMA $(o sample.zipx) .)'
step .rar rar -- '(cd $p && rar a -idq -ma5 -r $(o sample.rar) .)'
step .z01 zip -- '(cd $big && zip -qr -s 64k $(o split.zip) .)'
step .001 "$z7" -- '(cd $big && $z7 a -bso0 -mx0 -v64k $(o split.7z) .)'
step .cab gcab -- '(cd $p && gcab -cz $(o sample.cab) hello.txt sub/data.txt)'
step .arj arj -- '(cd $p && arj a -r -y $(o sample.arj) hello.txt sub >/dev/null)'
step .cpio cpio -- '(cd $p && find . | cpio -o -H newc --quiet >$(o sample.cpio))'
step .xar xar -- '(cd $p && xar -cf $(o sample.xar) hello.txt sub)'
step .wim "$z7" -- '(cd $p && $z7 a -bso0 -twim $(o sample.wim) .)'
step .swm wimlib-imagex -- 'wimlib-imagex capture $p t.wim --no-acls >/dev/null && wimlib-imagex split t.wim $(o sample.swm) 1 >/dev/null'
step .esd wimlib-imagex -- 'wimlib-imagex capture $p $(o sample.esd) --solid --no-acls >/dev/null'

# single compressed files: hello.txt
for e in .gz .gzip; do step $e gzip -- "gzip -9nc $p/hello.txt >$(o sample$e)"; done
for e in .bz2 .bzip2; do step $e bzip2 -- "bzip2 -c $p/hello.txt >$(o sample$e)"; done
step .xz xz -- 'xz -c $p/hello.txt >$(o sample.xz)'
step .zst zstd -- 'zstd -qc $p/hello.txt >$(o sample.zst)'
step .lzma xz -- 'xz --format=lzma -c $p/hello.txt >$(o sample.lzma)'
step .z compress -- 'compress -c $p/hello.txt >$(o sample.z)'

# packages and installers
step .deb dpkg-deb -- 'mkdir -p pkg/DEBIAN pkg/usr/share/kestrel-fixture && cp -r $p/. pkg/usr/share/kestrel-fixture/ &&
    printf "Package: kestrel-fixture\nVersion: 1\nArchitecture: all\nMaintainer: Kestrel <kestrel@example.invalid>\nDescription: test fixture\n" >pkg/DEBIAN/control &&
    dpkg-deb --root-owner-group -Zxz --build pkg $(o sample.deb) >/dev/null'
step .udeb dpkg-deb -- 'cp $(o sample.deb) $(o sample.udeb)'
step .rpm rpmbuild -- 'mkdir -p rpm/SOURCES && cp $p/hello.txt rpm/SOURCES/ && cat >rpm/f.spec <<SPEC
Name: kestrel-fixture
Version: 1
Release: 1
Summary: test fixture
License: MIT
BuildArch: noarch
Source0: hello.txt
%description
test fixture
%install
mkdir -p %{buildroot}/usr/share/kestrel-fixture
cp %{SOURCE0} %{buildroot}/usr/share/kestrel-fixture/
%files
/usr/share/kestrel-fixture/hello.txt
SPEC
    rpmbuild -bb --define "_topdir $work/rpm" rpm/f.spec >/dev/null && cp rpm/RPMS/noarch/*.rpm $(o sample.rpm)'
# an MSI database (summary info) with hello.txt as a stream: what 7-Zip shows inside an .msi
step .msi msibuild -- 'msibuild $(o sample.msi) -s "Kestrel Fixture" && msibuild $(o sample.msi) -a hello.txt $p/hello.txt'
: <<'WIXL' # a full installer, if wixl is ever wanted instead: step .msi wixl -- 'cat >f.wxs <<WXS
<?xml version="1.0" encoding="utf-8"?>
<Wix xmlns="http://schemas.microsoft.com/wix/2006/wi">
  <Product Id="*" Name="Kestrel Fixture" Language="1033" Version="1.0.0" Manufacturer="Kestrel" UpgradeCode="5e4b0bd4-6c1a-4b9e-9a51-6a1f3c2f9a10">
    <Package InstallerVersion="200" Compressed="yes"/>
    <Media Id="1" Cabinet="data.cab" EmbedCab="yes"/>
    <Directory Id="TARGETDIR" Name="SourceDir"><Directory Id="ProgramFilesFolder"><Directory Id="D" Name="Kestrel Fixture">
      <Component Id="C" Guid="0b6f1f0e-3f6a-4f0e-9a8e-2b1c3d4e5f60"><File Id="F" Source="$p/hello.txt"/></Component>
    </Directory></Directory></Directory>
    <Feature Id="All" Level="1"><ComponentRef Id="C"/></Feature>
  </Product>
</Wix>
WXS
    wixl -o $(o sample.msi) f.wxs'
WIXL
# no Linux tool writes a patch; an .msp is the same OLE compound-file container as an .msi
step .msp msibuild -- 'cp $(o sample.msi) $(o sample.msp)'
zipped() { (cd "$p" && zip -qr "$(o "$1")" .); }
for e in .apk .jar .war .whl .xpi .ipa .appx .epub .cbz; do step $e zip -- "zipped sample$e"; done
step .crx python3 -- 'python3 -I $here/fixtures/crx.py $(o sample.zip) $(o sample.crx)'
step .pkg xar -- 'cp $(o sample.xar) $(o sample.pkg)'

# disk and file-system images
step .img mke2fs -- 'mke2fs -q -t ext2 -b 1024 -d $p $(o sample.img) 256'
step .iso xorriso -- 'xorriso -as mkisofs -quiet -R -J -o $(o sample.iso) $p'
step .udf genisoimage -- 'genisoimage -quiet -udf -R -o $(o sample.udf) $p'
for f in vpc:.vhd vhdx:.vhdx vmdk:.vmdk vdi:.vdi qcow:.qcow qcow2:.qcow2; do
    opts=; [ ${f%%:*} = vhdx ] && opts="-o log_size=1M,block_size=1M" # default log alone is 16 MB
    step ${f#*:} qemu-img -- "qemu-img convert -f raw -O ${f%%:*} $opts $(o sample.img) $(o sample${f#*:})"
done
step .squashfs mksquashfs -- 'mksquashfs $p $(o sample.squashfs) -noappend -quiet -all-root >/dev/null'
step .cramfs mkfs.cramfs -- 'mkfs.cramfs $p $(o sample.cramfs) >/dev/null'
# 7-Zip reads HFS+ (not classic HFS): cut it out of a hybrid ISO, once as HFS+ and once as HFSX
step .hfs xorriso -- 'xorriso -as mkisofs -quiet -hfsplus -o hyb.iso $p && python3 -I $here/fixtures/hfsplus.py hyb.iso $(o sample.hfs)'
step .hfsx xorriso -- 'python3 -I $here/fixtures/hfsplus.py hyb.iso $(o sample.hfsx) --hfsx'
step .dmg python3 -- 'python3 -I $here/fixtures/udif.py $(o sample.img) $(o sample.dmg)'
step .simg img2simg -- 'img2simg $(o sample.img) $(o sample.simg)'

# no Linux tool writes these: small real-world files, downloaded once and kept
fetch() { # fetch EXT URL: the file at URL becomes sample.EXT (uuencoded .uu files are decoded)
    local dst=$(o "sample$1")
    [ -s "$dst" ] && { made=$((made + 1)); return; }
    if curl -fsSL "$2" -o "$work/dl"; then
        case $2 in *.uu) python3 -I $here/fixtures/uudecode.py "$work/dl" "$dst" ;;
        *) mv "$work/dl" "$dst" ;; esac
        made=$((made + 1))
    else
        skipped+=("$1 (download failed: $2)")
    fi
}
# libarchive's LHA test file (BSD-2-Clause); PuTTY's manual (MIT)
fetch .lzh https://raw.githubusercontent.com/libarchive/libarchive/v3.8.10/libarchive/test/test_read_format_lha_lh6.lzh.uu
[ -s "$(o sample.lzh)" ] && cp "$(o sample.lzh)" "$(o sample.lha)" && made=$((made + 1))
fetch .chm https://the.earth.li/~sgtatham/putty/0.83/putty.chm
# RAR 7 can't write RAR4, and RAR5 sets are always .partN.rar: old-style .rar/.r00/.r01 comes from
# libarchive's RAR4 three-part set, relabelled (scripts/fixtures/oldrar.py)
if [ -s "$(o oldrar.r00)" ]; then
    made=$((made + 1))
else
    lib=https://raw.githubusercontent.com/libarchive/libarchive/v3.8.10/libarchive/test
    ok=1
    for i in 1 2 3; do
        curl -fsSL "$lib/test_rar_multivolume_single_file.part$i.rar.uu" -o "$work/r$i.uu" &&
            python3 -I "$here/fixtures/uudecode.py" "$work/r$i.uu" "$work/r$i.rar" || ok=
    done
    if [ -n "$ok" ] && python3 -I "$here/fixtures/oldrar.py" "$out/oldrar" "$work"/r{1,2,3}.rar; then
        made=$((made + 1))
    else
        skipped+=(".r00 (download failed)")
    fi
fi

echo "made $made samples in $out"
if [ ${#skipped[@]} -gt 0 ]; then
    printf 'skipped:\n'
    printf '  %s\n' "${skipped[@]}"
    exit 1
fi
