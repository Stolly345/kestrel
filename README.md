# Kestrel

Everything-style instant whole-disk search inside a Files-style file manager, for Ubuntu
(GNOME, GTK4/libadwaita). Written in Rust, shipped as a `.deb`. MIT licence.

- **kestrel** — the file manager: full-width search box, tabs, dual pane, details/grid/column
  views, preview pane (images, video, text, Markdown, PDF and Office pages, archive contents),
  git status, colour tags, command palette, terminal panel (Ctrl+`), batch rename, checksums,
  right-click scripts (Nautilus scripts folder), extract/compress for 70 archive formats in a
  sandbox, browse archives as folders. Can be the default file manager.
- **kestrel-indexd** — root daemon that indexes every local file system (~2 million entries in
  about 5 s), kept live with fanotify; results are filtered to what the asking user may read.
- **ks** — command-line search, like Everything's `es.exe` (`ks --syntax` for the query language).

## Install

```
cargo deb -p kestrel
sudo apt install ./target/debian/kestrel_*_amd64.deb
```

apt lists anything missing and asks once. Launch Kestrel once afterwards: it takes over as the
default file manager.

## Dependencies

**Runtime** (declared in the `.deb`, installed by apt):

| Package | For |
|---|---|
| libgtk-4-1, libadwaita-1-0, libglib2.0-0t64, libpango-1.0-0, libgdk-pixbuf-2.0-0, libgraphene-1.0-0, zlib1g | the app (picked up automatically) |
| libvte-2.91-gtk4-0 | terminal panel |
| bubblewrap | sandbox for extracting and for rendering document previews |
| tar, gzip, xz-utils, bzip2, zstd | tarballs |
| gvfs | Trash, Recent, Network |
| xdg-utils | default-file-manager hookup |
| pkexec | "Start Service" and editing the index exclude list |

7-Zip is bundled (`/usr/lib/kestrel/7zz`, 26.05): Ubuntu's 7zip lags on decoder security fixes
and can't read RAR. Update it monthly with `scripts/update-7zip.sh VERSION`.

**Recommended** (installed by default, skip with `--no-install-recommends`):

| Package | For |
|---|---|
| gvfs-backends | browsing inside archives, network places (SFTP, SMB…) |
| poppler-utils | PDF previews |
| libreoffice-writer, libreoffice-calc, libreoffice-impress | Word/Excel/PowerPoint/OpenDocument previews |
| ncompress | `.tar.z` / `.taz` |
| xdg-terminal-exec | "Open in Terminal" with your chosen terminal |

**Build:**

```
sudo apt install libgtk-4-dev libadwaita-1-dev libgit2-dev libpoppler-glib-dev \
  libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev libvte-2.91-gtk4-dev pkg-config
cargo install cargo-deb
```

**Rebuilding the archive test samples** (only when adding or changing a format):

```
sudo apt install zip rar arj gcab xar wimtools msitools rpm genisoimage squashfs-tools \
  xorriso qemu-utils android-sdk-libsparse-utils ncompress zstd cpio
```

## Tests

```
KESTREL_7ZZ=$PWD/packaging/7zip/7zz cargo test --workspace
```

- `every_format_extracts` extracts a real sample of all 70 archive formats through the same
  sandbox as Extract Here, checks the contents and that the preview listing matches. Samples
  live in `crates/kestrel/tests/fixtures/archives/` (sources and licences in its README) and are
  built by `scripts/make-archive-fixtures.sh`. A new extension fails the test until it has a sample.
- `scripts/smoke-live.sh` (run as yourself; it uses sudo only for a test daemon): live index
  updates, per-user permission filtering, excludes, drives mounting and unmounting. Run it after
  any change to `kestrel-indexd` or `kestrel-index`.

## Layout

| Path | What |
|---|---|
| `crates/kestrel` | the GTK app |
| `crates/kestrel-indexd` | the indexing daemon |
| `crates/kestrel-index` | index, scanner and query language shared by both |
| `crates/ks` | command-line search |
| `packaging/` | .desktop file, icon, systemd unit, bundled 7-Zip, Debian maintainer scripts |
| `docs/extract-formats.md` | every extractable format |
| `PRD.md` | requirements and release log |
| `PLAN.md` | engineering notes |
