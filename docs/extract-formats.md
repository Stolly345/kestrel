# What Kestrel can extract

Right-click → **Extract Here** or **Extract To…** (also in the command palette, Ctrl+Shift+P) appears for
any file ending in one of the extensions below. Extraction runs in a sandbox (no network, home hidden,
only the destination writable) using the 7-Zip bundled with Kestrel (`/usr/lib/kestrel/7zz`); tarballs use
the system `tar`, sandboxed the same way. Encrypted archives ask for the password.

The list lives in code as `ARCHIVE_EXTS` in `crates/kestrel/src/fileops.rs`; keep the two in step.

## Archives

| Extension | Format | Notes |
|---|---|---|
| .7z | 7-Zip | password-protected archives supported |
| .zip, .zipx | ZIP | password-protected archives supported (ZipCrypto and AES) |
| .z01, .001 | split ZIP (.z01, .z02 … .zip) / split archive (.7z.001, .zip.001 …) | the other parts must sit next to it; the .zip of a split ZIP works too |
| .rar, .r00 | RAR 4 and RAR 5, including multi-volume (.part1.rar …, or .rar + .r00, .r01 …) | encrypted archives supported; any part extracts the whole set |
| .cab | Microsoft Cabinet | |
| .arj | ARJ | |
| .lzh, .lha | LHA / LZH | |
| .cpio | cpio | |
| .xar | XAR | |
| .wim, .swm, .esd | Windows Imaging (install.wim / install.esd) | |
| .chm | Compiled HTML Help | extracts the HTML pages and images |

## Tarballs

| Extension | Format |
|---|---|
| .tar | plain tar |
| .tar.gz, .tgz, .tpz | gzip-compressed tar |
| .tar.xz, .txz | xz-compressed tar |
| .tar.zst, .tzst | Zstandard-compressed tar |
| .tar.bz2, .tbz2, .tbz | bzip2-compressed tar |
| .tar.z, .taz | compress (.Z) tar |
| .ova | Open Virtualization Appliance (a tar of the VM's disks and settings) |

## Single compressed files

These hold one file; extracting gives you that file back.

| Extension | Format |
|---|---|
| .gz, .gzip | gzip |
| .xz | xz |
| .bz2, .bzip2 | bzip2 |
| .zst | Zstandard |
| .lzma | LZMA |
| .z | Unix compress |

## Packages and installers

| Extension | Format | Notes |
|---|---|---|
| .deb, .udeb | Debian / Ubuntu package | gives `data.tar`; extract that again for the files |
| .rpm | Red Hat / Fedora package | gives a `.cpio`; extract that again for the files |
| .msi, .msp | Windows Installer | gives the installer's internal streams and cabinets |
| .apk | Android app | |
| .jar | Java archive | |
| .war | Java web application | |
| .whl | Python wheel | |
| .xpi | Firefox add-on | |
| .crx | Chrome extension | |
| .ipa | iOS app | |
| .appx | Windows app package | |
| .pkg | macOS installer package (XAR) | |

## Books and comics

| Extension | Format |
|---|---|
| .epub | e-book |
| .cbz | comic book (ZIP of images) |

## Disk and file-system images

| Extension | Format | Notes |
|---|---|---|
| .iso | CD/DVD image (ISO 9660, Joliet, UDF) | |
| .img | raw disk image | works when it holds a file system 7-Zip reads (FAT, NTFS, ext2/3/4, ISO, APFS, HFS) |
| .udf | UDF image | |
| .dmg | macOS disk image | |
| .vhd, .vhdx | Hyper-V / Virtual PC disk | |
| .vmdk | VMware disk | |
| .vdi | VirtualBox disk | |
| .qcow, .qcow2 | QEMU disk | |
| .squashfs | SquashFS (snaps, live-CD root file systems) | |
| .cramfs | CramFS | |
| .hfs, .hfsx | Mac HFS / HFS+ volume | |
| .simg | Android sparse image | |

## Not offered, on purpose

7-Zip can also open these, but Kestrel doesn't show Extract on them, because they're documents or
programs you normally just open:

- Office and OpenDocument files (.docx, .xlsx, .pptx, .odt, .ods) are ZIPs inside; the preview pane
  shows their pages instead.
- Old Office files (.doc, .xls, .ppt) are compound files.
- Windows programs and installers (.exe, .dll, .sys), including self-extracting and NSIS installers.
- Linux and macOS programs (ELF, Mach-O), UEFI firmware, and object files.
- Flash (.swf, .flv), Base64 and Intel HEX files, and checksum files (.sha256, .md5).

To look inside a ZIP-based one (.docx and so on), rename a copy to .zip and extract that.

## Not supported

- .tar.lz, .lz (lzip), .tar.lz4, .lz4, .sit (StuffIt) and .ace: the bundled 7-Zip can't read these.
