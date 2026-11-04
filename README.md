<div align="center">

<img src="packaging/dev.kestrel.Kestrel.svg" width="112" alt="Kestrel icon">

# Kestrel

**Instant whole-disk search, built into a file manager that feels at home on GNOME.**

Everything-style search for Linux: every file on every local disk, found as you type, kept
live by the kernel, and filtered to what you're allowed to see.

![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)
![Linux: Ubuntu 26.04+, Debian 13, Fedora, openSUSE, Arch](https://img.shields.io/badge/Linux-Ubuntu%2026.04%2B%20%C2%B7%20Debian%2013%20%C2%B7%20Fedora%20%C2%B7%20openSUSE%20%C2%B7%20Arch-E95420?logo=linux&logoColor=white)
![GTK4 + libadwaita](https://img.shields.io/badge/GTK4-libadwaita-4A86CF?logo=gnome&logoColor=white)
![Written in Rust](https://img.shields.io/badge/written%20in-Rust-B7410E?logo=rust&logoColor=white)
![70 archive formats, all tested](https://img.shields.io/badge/archive%20formats-70%2C%20all%20tested-2EA44F)

<img src="docs/screenshots/kestrel.png" width="900" alt="Kestrel searching the whole disk, with the preview pane open">

</div>

## Why Kestrel

On Windows, [Everything](https://www.voidtools.com/) finds any file by name the instant you
type. Linux has had pieces of that (`locate` is fast but goes stale; desktop search indexes
contents but is slow to answer "where is that file called…"), but never inside the file manager
you already use.

Kestrel pairs a small root service that indexes **every local file system** (about 2.1 million
files in 5 seconds, ~230 MB of memory) and keeps it **live through fanotify**, with a full GTK4
file manager whose search box asks it. A whole-disk search answers in tens of milliseconds, and
the results only ever include files you could find yourself.

**Why the bird?** A kestrel hovers in one spot above a whole field and still sees a single mouse
move in the grass. That's the job here: keep watch over the entire disk, notice every change as
it happens, and pick out the one file you want the moment you ask for it.

<table>
<tr>
<td width="50%"><img src="docs/screenshots/search.png" alt="Typing .iso lists ISO images first"></td>
<td width="50%"><img src="docs/screenshots/preview.png" alt="Preview pane paging through a PDF"></td>
</tr>
<tr>
<td align="center"><sub>Search understands types: <code>.iso</code>, <code>pdf</code>, <code>excel</code></sub></td>
<td align="center"><sub>Preview PDFs, Office documents, Markdown, media and archives</sub></td>
</tr>
<tr>
<td colspan="2"><img src="docs/screenshots/select.png" alt="The command bar, and Select mode with checkboxes ticked on two invoices"></td>
</tr>
<tr>
<td colspan="2" align="center"><sub>A command bar like Explorer's, and Select mode: tick the files you want</sub></td>
</tr>
</table>

## Features

**Search**
- Results while you type, across every local disk, USB drives included the moment they mount
- Everything's query language: wildcards, `|` OR, `!` NOT, `<groups>`, `regex:`, `size:`, `dm:`, `ext:`, `dupe:`, `content:`…
- **Best Match order**: the whole name beats its start, which beats the start of a word (`OwnerInvoice`, `old_report`), which beats anywhere. Your own folders (Desktop, Documents, Downloads, projects) rank up; caches, `node_modules`, build output and hidden folders rank down; this week's files edge ahead. Click a column header to sort by it instead
- **Learns what you open**: a result you opened for a query moves up the next time you type it (or a shorter or longer version of it). Kept in `~/.config/kestrel/picks.json`, never sent anywhere
- **Typing narrows**: when `repo` becomes `repor`, the service filters the last results instead of searching the whole disk again (5–7× faster per keystroke)
- **Apps too**: a whole-disk search for a name like `calc` or `vsc` shows matching apps in a row above the files (by name, word, initials or keywords); click one to start it
- Type-aware: `.deb` lists .deb files first; `pdf`, `word`, `excel`, `powerpoint` also find files of that type (`budget excel` finds `Budget.xlsx`)
- Scope switch (Everywhere / This folder / This folder only), filter chips, match highlighting, recent and saved searches, a syntax cheat sheet in the menu
- `ks`: the same search on the command line, like Everything's `es.exe`

**Quick Search** (`kestrel --quick`; turn on a shortcut in Preferences › Quick Search, Ctrl+Alt+Space by default)
- A small search window over whatever you're doing, like a launcher: apps (by name, word or initials), files from the whole-disk index, and GNOME Settings pages (`bluetooth`, `dark`, `display`), in one Best Match list
- Sums: `2^10 * 3`, `sqrt(2)/2`, `15% * 80`; Enter copies the answer
- Web searches: `g rust lifetimes`, `ddg`, `yt`, `gh`, `wiki`, or your own in `settings.json` (`web_searches`)
- System actions: `lock`, `sleep`, `log out`, `restart`, `shut down`, `bios` (restart into firmware setup); the ones that end your session ask for a second Enter
- Before you type: pinned items (drag to reorder), then recent ones. Right-click (or Shift+F10) a result: Open With, Open File Location, Copy Path, Pin to Top, Forget
- `?` lists the syntax and prefixes, each one Enter away. Ctrl+Enter shows a file in its folder, Ctrl+C copies its path, Tab searches inside a folder, Esc clears then hides; drag a file into another app
- Stays in the background after the first use, so the shortcut opens it instantly

**File manager**
- A command bar like Windows Explorer's, as icons (hover for the name and shortcut): New, Open, Undo, Redo, Pin, Cut, Copy, Paste, Rename, Compress, Extract, Delete, View. Pin, Compress, Extract and View also show their names from the default window size up. The buttons grow or shrink with the window to stay on one row, and only a narrow window takes two. Select (☑) and Properties (ⓘ) sit at the start of the path bar
- Select mode: a checkbox on every file, so you can tick exactly the ones you want (All / None / Invert too)
- Tabs, dual pane (F3), details / grid / column views, session restore
- Editable path bar (Ctrl+L), bookmarks shared with GNOME Files, Trash with restore
- A Network page like GNOME Files': connected shares, servers you've used (shared with Files), what's on your network, and Connect to Server
- New ▸ Folder, Shortcut, Text, Markdown, Word, Excel, PowerPoint or PDF: real blank documents, opened straight away in your default app for that type (change those with Open ▾ › Set Default App…)
- Open With lists the apps for a file's type, with *Always use for this file type*; Set Default App changes the default without opening anything. Saved where GNOME Files saves it, so every app agrees
- **Install apps by double-click**: a `.deb`, an `.rpm` or an AppImage opens a click-through installer (Next, Install, Open) that shows the app's own icon and name before anything is installed, and ends with a launcher in your apps and, if you like, on the Desktop. Packages install through apt, dnf or zypper with their dependencies; an AppImage needs no password: it goes to `~/Applications` with its icons, updates in place when you open a newer one, and can be uninstalled from the same screen. An `.rpm` on Ubuntu (or a `.deb` on Fedora) says what it's for instead. Right-click › Browse Archive still looks inside
- Open File Location on a search result: its folder, with it selected
- Tells you when a new version is out: once a day it asks GitHub for the latest release (nothing else is sent; turn it off in Preferences › System › Check for updates)
- Copy, move, rename, undo and redo; Paste as Link (symlinks); clipboard compatible with GNOME Files; drag and drop
- Name clashes ask *Skip / Keep Both / Replace*; one failed item never stops a batch
- Batch rename: find and replace (or regex), case changes, numbering, live preview
- Colour tags you can name (right-click a dot in the sidebar: red becomes *Taxes*, and `tag:taxes` finds them), git status column and branch, symlink badges, free-space bars on drives
- Command palette (Ctrl+Shift+P), terminal panel that follows the folder (Ctrl+\`)
- Properties with checksums (MD5, SHA-1, SHA-256) and paste-to-verify; folder sizes instantly from the index (also in the Size column), or on request (Calculate) for network shares
- Right-click scripts from `~/.local/share/nautilus/scripts`, run exactly as GNOME Files runs them
- Can be your default file manager, including "Show in Folder" from other apps

**Archives**
- Extract Here / Extract To… for [70 formats](docs/extract-formats.md): zip, 7z, rar, tarballs, cab, iso, dmg, vhd(x), qcow2, deb, rpm, msi, wim, squashfs…
- Split and multi-volume sets (`.001`, `.z01`, `.part1.rar`, `.r00`), password prompts, progress with time left, Cancel
- Double-click a zip, rar or tarball to browse it like a folder
- Every format is checked by an automated test

**Previews**
- Every image format your system can decode (PNG, JPEG, WebP, AVIF, HEIC, JPEG XL, SVG, BMP, TIFF, GIF, ICO, TGA, QOI…)
- Video and music in every common codec (MP3, WAV, FLAC, AAC, Opus…); music shows its cover art, title, artist and album, with previous / next through the folder, a seek bar and repeat
- Text, and Markdown rendered as a page like PDFs
- Kiwix `.zim` archives (offline Wikipedia and friends): title, description, article count
- Nextcloud Whiteboard and Excalidraw drawings (`.whiteboard`, `.excalidraw`), drawn
- Thumbnails for PDFs, documents, images and video in every view
- PDF, Word, Excel, PowerPoint and OpenDocument pages, one at a time
- Archive contents; folder totals (instant, from the index)

## How it works

```mermaid
flowchart LR
    subgraph root["kestrel-indexd (root, systemd)"]
        scan["scan every local file system"] --> index[("index in memory<br/>+ snapshot on disk")]
        fan["fanotify events"] --> index
    end
    index -- "results filtered by<br/>the caller's uid, groups, ACLs" --> sock(["/run/kestrel/sock"])
    sock --> app["Kestrel<br/>(GTK4 / libadwaita)"]
    sock --> ks["ks (CLI)"]
    app -- "extract, preview documents" --> bwrap["bubblewrap sandbox<br/>7-Zip · tar · LibreOffice · Poppler"]
```

- **The index** holds names, sizes, times, permissions and ACLs, never file contents. It is
  saved to `/var/lib/kestrel` (root only), so search works the moment you log in while a fresh
  scan runs in the background.
- **Live updates** come from fanotify, which reports changes for whole file systems: new,
  renamed and deleted files are searchable within a second. If the kernel ever drops events, the
  service notices and rescans.
- **Network shares** (SMB/CIFS, NFS) send no change events, so they're re-listed instead: every
  3 hours, and right away with `sudo systemctl reload kestrel-indexd`. A share that's offline (a
  laptop away from home) keeps its last listing, so you can still find what's on it. See
  [Indexing a NAS](#indexing-a-nas).
- **Privacy:** any user may ask, but every answer is filtered by the asker's own uid, groups and
  ACLs, which the kernel reports for the connection. Files inside someone else's private folder
  never appear, and `content:` opens files *as the asking user*, so the kernel itself enforces
  their permissions. Each user gets a bounded number of connections and request size, and the
  service runs under systemd memory and task limits.
- **Untrusted files are opened in a sandbox.** Archive extraction and document rendering run
  under bubblewrap: no network, your home folder hidden, only the output folder writable. A
  hostile archive can't write anywhere else, whatever paths it contains.
- **7-Zip is bundled** (the official 26.05 build, checked against its published SHA-256):
  Ubuntu's package trails on decoder security fixes and can't read RAR.

## Install

Download the package for your system from the
[latest release](https://github.com/Stolly345/kestrel/releases/latest) (64-bit x86 only), then:

| System | File | Install with |
| --- | --- | --- |
| Ubuntu 26.04, Debian 13 (trixie) or newer | `kestrel_<version>-1_amd64.deb` | `sudo apt install ./kestrel_*_amd64.deb` |
| Fedora 41 or newer | `kestrel-<version>-1.x86_64.rpm` | `sudo dnf install ./kestrel-*.x86_64.rpm` |
| openSUSE Tumbleweed | `kestrel-<version>-1.x86_64.rpm` | `sudo zypper install --allow-unsigned-rpm ./kestrel-*.x86_64.rpm` |
| Arch, Manjaro, EndeavourOS, CachyOS | none, see below | build the `kestrel-bin` package (below) |

Run the command in the folder you downloaded to (usually `cd ~/Downloads`); the `./` matters, it
tells apt/dnf/zypper you mean a file, not a package from the internet. They fetch everything else
Kestrel needs and ask once. The package starts the index service; to make Kestrel your default file
manager, switch it on in **Preferences**.

- **Arch and Arch-based:** the PKGBUILD in `packaging/aur` repackages the release `.deb`, so nothing
  compiles. Then turn the search service on once (Arch packages never do that themselves):

  ```sh
  sudo pacman -S --needed git base-devel
  git clone --depth 1 https://github.com/Stolly345/kestrel && cd kestrel/packaging/aur
  makepkg -si
  sudo systemctl enable --now kestrel-indexd
  ```

  Remove with `sudo pacman -R kestrel-bin`.
- **Use apt, not `dpkg -i`.** dpkg doesn't fetch dependencies, so it stops at the first missing one
  (on Debian usually `libvte-2.91-gtk4-0`). If that already happened, `sudo apt -f install` finishes it.
- **Not supported:** Debian 12 and Ubuntu before 26.04 (GTK, libadwaita and glibc are too old), Fedora 40
  and older, RHEL/Rocky/Alma 9, Arm. RHEL 10 is untested.
- **Remove:** `sudo apt remove kestrel` / `sudo dnf remove kestrel` / `sudo zypper remove kestrel`.

Build the packages yourself:

```sh
cargo build --release
cargo deb -p kestrel --no-build              # -> target/debian/kestrel_*_amd64.deb
cargo install cargo-generate-rpm             # once
cargo generate-rpm -p crates/kestrel         # -> target/generate-rpm/kestrel-*.x86_64.rpm
```

<details>
<summary><b>Dependencies</b></summary>

**Runtime** (declared in the package):

| Package | For |
|---|---|
| libgtk-4-1, libadwaita-1-0, GLib, Pango, gdk-pixbuf, graphene, zlib | the app |
| libvte-2.91-gtk4-0 | terminal panel |
| bubblewrap | sandbox for extracting and document previews |
| tar, gzip, xz-utils, bzip2, zstd | tarballs |
| gvfs | Trash, Recent, Network |
| xdg-utils | default file manager hookup |
| pkexec | starting the service, editing excluded folders |

**Recommended** (installed by default):

| Package | For |
|---|---|
| gvfs-backends | browsing inside archives; network places (SFTP, SMB…) |
| squashfs-tools | reading an AppImage's name and icon without running it |
| poppler-utils | PDF previews |
| libreoffice-writer, -calc, -impress | Word, Excel, PowerPoint and OpenDocument previews |
| gstreamer1.0-libav, -plugins-good, -bad, -ugly | playing every common audio and video codec |
| ncompress | `.tar.z` / `.taz` |
| xdg-terminal-exec | "Open in Terminal" with your chosen terminal |

**Build** (Rust 1.88 or newer):

```sh
sudo apt install build-essential pkg-config libgtk-4-dev libadwaita-1-dev \
  libvte-2.91-gtk4-dev libgstreamer1.0-dev zlib1g-dev
cargo install cargo-deb
```

libgit2 is compiled in; PDF pages use the `poppler-utils` commands and video uses GTK's own
GStreamer. Music plays through Kestrel's own small GStreamer player (audio only, so it needs
`libgstreamer1.0-dev` to build; at run time just the GStreamer that GNOME already has).

**Rebuilding archive test samples** (only when adding a format):

```sh
sudo apt install zip rar arj gcab xar wimtools msitools rpm genisoimage squashfs-tools \
  xorriso qemu-utils android-sdk-libsparse-utils ncompress zstd cpio
```

</details>

## Indexing a NAS

Connect to the share once (Network page, or Files), then press **Index…** next to it on Kestrel's
Network page. Give the share's user name and password, then your own password once, and its files
show up in your searches within a few minutes, like a local disk's. *Indexed Shares* on the same
page shows each share's progress ("Listing… 20,483 items so far", "182,200 items · listed today at
10:52", "Can't be reached"), and **Stop Indexing** there undoes it. This is SMB (Windows sharing), which nearly every NAS offers; it needs
the `cifs-utils` package, which the .deb and .rpm recommend.

What Index… does: it tries the user name and password first (a wrong one changes nothing), then
mounts the share at `/mnt/SERVER-SHARE` from `/etc/fstab` whenever something opens it, with the
password in a root-only file under `/etc/kestrel/shares`. Only you can open the mount, so its
files only show up in *your* searches. A share sends no change events, so Kestrel re-lists it
every 3 hours (`sudo systemctl reload kestrel-indexd` re-lists it now); when it's offline (a laptop
away from home) its last listing stays searchable.

By hand, or for NFS: any SMB/NFS share in `/etc/fstab` whose options include `x-kestrel` is
indexed, for example:

```sh
//NAS-ADDRESS/share /mnt/nas cifs credentials=/etc/nas.cred,uid=1000,gid=1000,dir_mode=0700,file_mode=0600,soft,noauto,x-systemd.automount,x-systemd.idle-timeout=30min,_netdev,nofail,x-kestrel 0 0
```

Shares without `x-kestrel` are never listed, so a big share behind a VPN isn't walked unasked.

## Search syntax

| Query | Finds |
|---|---|
| `invoice 2026` | names containing both words |
| `report \| summary` | either |
| `!tmp` | names without "tmp" |
| `*.rs` · `IMG_????.jpg` | wildcards (they match the whole name) |
| `.iso` · `excel` | that type first; type words also match the type |
| `ext:jpg;png` · `pic:` `audio:` `video:` `doc:` `zip:` `exe:` `diagram:` `cad:` `gis:` `data:` `cert:` `lib:` `code:` `font:` | by extension or kind |
| `size:>1gb` · `size:huge` | by size |
| `dm:today` · `dm:last2weeks` · `dm:2026-10` | by date modified (`dc:` created, `da:` accessed) |
| `under:~/Projects` · `parent:/etc` · `depth:<3` | by place |
| `folder:` · `file:` · `empty:` | folders, files, empty folders |
| `dupe:` · `sizedupe:` | same name / same size |
| `tag:red`, `tag:taxes` | colour tags, by colour or by the name you gave it |
| `content:"TODO"` | text inside files (slower; read permission checked first) |
| `regex:^\d{4}-` · `case:Readme` · `path:src/main` | regular expression, case-sensitive, full path |

Results come in Best Match order until you click a column header (`ks -s best` does the same on the command line).

Open **Search Syntax** from the menu (or the command palette, Ctrl+Shift+P) for the full cheat sheet, or run `ks --syntax`.

## Keyboard

| Keys | Action | Keys | Action |
|---|---|---|---|
| Ctrl+F | search this folder | Ctrl+Shift+F | search everywhere |
| Ctrl+L | edit location | Esc | clear search |
| Ctrl+T / Ctrl+W | new / close tab | Ctrl+N | new window |
| F3 | dual pane | F4 | switch pane |
| F7 / F8 | copy / move to other pane | Ctrl+1 / 2 / 3 | details / grid / columns |
| Space or Alt+P | preview pane | Ctrl+\` | terminal panel |
| F2 | rename (batch with several selected) | Alt+Return | properties |
| Delete / Shift+Delete | trash / delete | Ctrl+Z | undo |
| Ctrl+Shift+Z or Ctrl+Y | redo | Ctrl+Shift+N | new folder |
| Ctrl+A / Ctrl+Shift+A | select all / none | Ctrl+Shift+I | invert selection |
| Ctrl+Shift+P | command palette | Ctrl+, | preferences |
| Ctrl+H | hidden files | F9 | sidebar |

## Development

```sh
KESTREL_7ZZ=$PWD/packaging/7zip/7zz cargo test --workspace
```

- **`every_format_extracts`** extracts a real sample of all 70 archive formats through the same
  sandbox as Extract Here, checks the contents, and checks the preview lists exactly the files
  Extract produces. Samples live in `crates/kestrel/tests/fixtures/archives/` and are built by
  `scripts/make-archive-fixtures.sh`; a new extension fails the test until it has one.
  Per-format report: `cargo test -p kestrel every_format -- --nocapture`.
- **`scripts/ui-check.sh`** drives the real app through 37 screens under a virtual display
  (views, previews, dialogs, batch rename, terminal, search and its empty/off states, a folder
  you can't open, Extract and its conflict prompt, Trash, the command bar in one and two rows,
  Select mode) with a throwaway home folder and its
  own small search service, checks 5 results along the way, fails on any GTK warning or error
  page, and saves every screen to `target/ui-check/`.
- **`scripts/feature-test.py`** uses the app the way a person does: it clicks, types and presses
  shortcuts through the accessibility layer under a virtual display, then checks the result on disk
  and on screen. 65 tests cover navigation, tabs, windows, session restore, views, bookmarks, dual pane,
  copy/move/rename/undo/redo/trash/restore/delete and name clashes, New documents opening in their apps, Open With and Set Default App, Paste as Link, Select
  mode and its checkboxes, batch rename, tags, search (typing, type
  ranking, scopes, tags, content, saved searches, the query language), previews, properties and
  checksums, the palette, preferences, the terminal, Nautilus scripts, git status, opening files,
  Show in Folder and Open File Location, and installing a .deb or an AppImage by double-click (and an .rpm on Ubuntu being explained). Failures leave a screenshot and a dump of what was on screen in
  `target/feature-test/`. Needs `xvfb` and `python3-xlib`.
- **`scripts/screenshots.sh`** regenerates the screenshots above from the real app and a made-up
  home folder.
- **`scripts/smoke-live.sh`** (run as yourself; it uses sudo only for a test service) checks live
  updates, permission filtering, excluded folders, and drives mounting and unmounting. Run it
  after any change to `kestrel-indexd` or `kestrel-index`.
- **7-Zip updates:** check [ip7z/7zip releases](https://github.com/ip7z/7zip/releases) monthly
  and run `scripts/update-7zip.sh VERSION` (it verifies the download's SHA-256).
- **Licence notices:** after changing dependencies, run `python3 scripts/third-party-licenses.py`
  to refresh `packaging/THIRD-PARTY-LICENSES.txt` (installed with the package).

| Path | What |
|---|---|
| `crates/kestrel` | the GTK app |
| `crates/kestrel-indexd` | the index service |
| `crates/kestrel-index` | index, scanner and query language shared by both |
| `crates/ks` | command-line search |
| `packaging/` | desktop file, icon, systemd unit, bundled 7-Zip, Debian scripts |
| `docs/extract-formats.md` | every extractable format |
| `PRD.md` · `PLAN.md` | requirements and release log · engineering notes |

## Limitations

- Ubuntu 26.04, amd64, `.deb` only for now. The index service needs root (fanotify watches whole
  file systems), so it can't ship as a Flatpak or Snap.
- Network shares are indexed only when opted in from `/etc/fstab`, and only every 3 hours: they
  send no change events. Shares opened in Files (gvfs) can be browsed but aren't indexed.
- English only; no translations yet.
- Filenames that aren't valid UTF-8 are shown with replacement characters.

## License

Kestrel is licensed under either of the [Apache License 2.0](LICENSE-APACHE) or the
[MIT License](LICENSE-MIT), at your option.

Unless you explicitly state otherwise, any contribution you submit for inclusion in Kestrel, as
defined in the Apache-2.0 license, is dual licensed as above, without any additional terms.

Licences of the Rust crates and the C libgit2 compiled into Kestrel are collected in
[packaging/THIRD-PARTY-LICENSES.txt](packaging/THIRD-PARTY-LICENSES.txt).
**Font:** [Geologica](https://github.com/monokromskriftforlag/geologisk) (SIL Open Font License 1.1, installed as `/usr/share/doc/kestrel/Geologica-OFL.txt`).
**Bundled:** [7-Zip](https://www.7-zip.org/) by Igor Pavlov (GNU LGPL, with the unRAR licence
restriction for RAR code; see [packaging/7zip/License.txt](packaging/7zip/License.txt)).
**Test samples:** files from [libarchive](https://github.com/libarchive/libarchive)
(BSD-2-Clause) and the [PuTTY](https://www.chiark.greenend.org.uk/~sgtatham/putty/) manual (MIT);
see [the samples' README](crates/kestrel/tests/fixtures/archives/README.md).
