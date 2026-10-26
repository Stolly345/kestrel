# Kestrel — Everything's engine + Files' UX, for Linux

Target: Ubuntu 26.04 / GNOME / ext4. Rust. Ships as one `.deb`.
Scope (decided 2026-10-25): **all** Everything technical features + **all** Files UX features.
Index: whole system, root daemon, results filtered per caller. Local disks only in v1 (no SMB/NAS).

## Crates (cargo workspace)

| crate | binary | job |
|---|---|---|
| `kestrel-index` | lib | in-memory index, walker, query language, matcher, sort, live reconcile, wire protocol (`proto`, JSON lines) |
| `kestrel-indexd` | `kestrel-indexd` | root systemd service: fanotify live updates, socket server, permission filter, HTTP server |
| `ks` | `ks` | CLI client, `es.exe` equivalent |
| `kestrel` | `kestrel` | GTK4 + libadwaita app: search + file manager |

## Phases

### 1. Index core (`kestrel-index`) — DONE 2026-10-25
- Entry = `{name_off, parent: u32, size, mtime, mode, uid, gid, kind}`; names in one big arena (Everything-style, target <100 B/file).
- Parallel first scan with `jwalk`, one per mount, `-xdev`, skip pseudo-fs (proc, sys, dev, run, snap squashfs).
- Pre-sorted index arrays per column (name, path, size, mtime, ext) → instant re-sort.
- Query language (Everything syntax): words AND, `|` OR, `!` NOT, `"quotes"`, `*`/`?` wildcards, `regex:`, `case:`, `path:`, `ext:a;b`, `size:>1gb`, `dm:today|lastweek|2026-10`, `dc:`, `file:`, `folder:`, `parent:`, `depth:`, `dupe:`, `empty:`, plus macros (`audio:`, `video:`, `doc:`, `pic:`, `zip:`, `exe:`).
- Parallel match with `rayon`; streaming first N results.
- ~~Snapshot to disk~~ skipped: boot rescan is 2.4 s warm. Add if cold-boot scan proves slow.
- **Check:** unit tests for parser + matcher; bench on real `/` (target: full scan <10 s for ~2M files, query <50 ms).

### 2. Daemon (`kestrel-indexd`) — BUILT 2026-10-25, fanotify path awaiting root smoke test
- Excludes: `/etc/kestrel/excludes` (default `/timeshift`): 5.42M → 1.95M entries, 2.4 s, ~232 MB.
- `fanotify` `FAN_MARK_FILESYSTEM` + `FAN_REPORT_DFID_NAME` per local fs → create/delete/rename/modify applied live.
  - Overflow (`FAN_Q_OVERFLOW`) → rescan that subtree.
- Unix socket `/run/kestrel/sock`; `SO_PEERCRED` → caller uid + groups.
- **Permission filter:** a path is returned only if the caller could `stat` it: every ancestor dir needs `x` for caller (mode bits + POSIX ACLs read at index time). Default-deny on anything unreadable. This is the security boundary — gets its own tests.
- systemd unit, hardened (`ProtectHome=read-only`, `CapabilityBoundingSet=CAP_SYS_ADMIN CAP_DAC_READ_SEARCH`, `NoNewPrivileges`).
- Optional HTTP server (Everything "ETP/HTTP"), localhost-only by default, off by default.

### 3. CLI (`ks`) — DONE 2026-10-25
- `ks <query>` with `-n`, `-s/--sort`, `--json`, `-0`, `--size --dm` columns, exit 1 on no match. Good enough for `ks ext:pdf invoice | fzf`.

### 4. App shell (`kestrel`) — DONE 2026-10-25
- `AdwTabView` tabs (drag out to new window, pin, restore session).
- Dual pane (`GtkPaned`), each pane an independent tab set.
- Views: details (`GtkColumnView`, virtualized), grid, column/Miller.
- Omnibar: breadcrumb path ⇄ type a path ⇄ type a query (instant index search, Everything mode).
- Sidebar: places, drives, tags, recent, bookmarks.
- File ops through **GIO** (copy/move/trash/undo, progress) → gets archives, FTP/SFTP, SMB, MTP from GVfs for free.

### 5. Files-specific UX — DONE 2026-10-25
- Preview pane: images, text/code (syntax highlight), PDF (poppler), audio/video (gstreamer), markdown.
- Thumbnails: freedesktop thumbnail cache (`~/.cache/thumbnails`), shared with Nautilus.
- Git: branch in omnibar, per-file status column, (later) ahead/behind — `git2`.
- Color tags: stored in `user.chimera.tags` xattr; indexed by daemon → `tag:red` query.
- Command palette (Ctrl+Shift+P), all actions + keybindings.
- Properties dialog, open-with, terminal here, compress/extract.

### 6. Packaging — DONE 2026-10-25 (`target/debian/kestrel_0.1.0-1_amd64.deb`, 1.5 MB)
- `cargo-deb` on the workspace: binaries, `.desktop`, icon, systemd unit, `postinst` (create `/var/lib/kestrel`, enable service).
- Depends: `libgtk-4-1`, `libadwaita-1-0`, `libgit2`, `libpoppler-glib8`, gstreamer base.

### 7. Extras (Everything parity) — DONE 2026-10-25
- Content search (`content:`) — slow path, reads files, opt-in.
- Duplicate finder UI (`dupe:size;name`, then hash).
- Search history, saved filters, bookmarks of queries.

## Known ceilings (decide later)
- Network mounts: no fanotify events → not indexed in v1.
- btrfs subvolumes / overlayfs: untested; Ubuntu default is ext4.
- fanotify needs kernel ≥5.9 (have 7.0).

## Needs Alex (sudo)
```
sudo apt install libgtk-4-dev libadwaita-1-dev libgit2-dev libpoppler-glib-dev \
  libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev libvte-2.91-gtk4-dev pkg-config
```
Runtime, recommended, build and test-sample dependencies: README.md.

## As built (2026-10-25)
- Query extras: `content:` (permission-checked BEFORE reading, 64 MB cap), `dupe:` `sizedupe:` `namesizedupe:`,
  `depth:`, `empty:`, `parent:`, `under:`, `tag:`.
- HTTP: `kestrel-indexd --http 127.0.0.1:8899 --http-user alex` (searches as that user; root refused; no downloads).
  The packaged unit has `PrivateNetwork=yes` + `RestrictAddressFamilies=AF_UNIX`; enabling HTTP needs a drop-in
  (`systemctl edit kestrel-indexd`) clearing both and adding the flags to ExecStart.
- GUI saves state in `~/.config/kestrel/` (session.json, history, searches); bookmarks shared with Nautilus.
- Headless checks: `GDK_BACKEND=broadway BROADWAY_DISPLAY=:7 kestrel --screenshot out.png [--select NAME] [--action NAME[::target]] PATH`
  (run `gtk4-broadwayd :7` first; dialogs land in `out.png.dialog.png`).
- Tests: 41 as of 0.9.14 (`KESTREL_7ZZ=$PWD/packaging/7zip/7zz cargo test --workspace`), incl. all 70 archive
  formats. `scripts/smoke-live.sh` (run as yourself; sudo only for its test daemon) covers fanotify live updates.

## Known ceilings
- ~~ACLs ignored~~ — fixed in M4: directory ACLs read at scan time, file ACLs before `content:` reads.
- Moved folders are removed + rescanned (no FAN_RENAME pairing).
- ~~ASCII-only case folding~~ — Unicode lowercasing since M4 (ß is not folded to ss); non-UTF-8 names shown lossy.
- No syntax highlighting in text preview (install `libgtksourceview-5-dev` to add it); no custom tag names.
- ~~No conflict dialog~~ — Skip / Keep Both / Replace since 0.4.0 (also for extract since 0.9.10); a failed item
  no longer stops a batch (0.9.13).
- Not click-tested since 0.9.10: Broadway can't run inside the Claude sandbox (unix sockets blocked).
