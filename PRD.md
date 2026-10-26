# Kestrel — Product Requirements Document

| | |
|---|---|
| Owner | Alex Stoll |
| Status | Draft v1 — 2026-10-25 |
| Platform | Ubuntu 26.04, GNOME, Wayland, ext4 |
| Stack | Rust, GTK4 + libadwaita, root index daemon (fanotify), `.deb` |
| Supersedes | The scope sections of `PLAN.md` (that file stays as the engineering log) |

---

## 1. Problem

On Linux, finding a file is slow and lives in a separate place from managing files. Nautilus search is slow and only covers part of the disk. `locate` is hours out of date. FSearch is fast but is a separate app with no file management. On Windows, **Everything** made search instant and global, and **Files** made file management modern. There is no Linux app that does both.

## 2. Vision

**Search is the centre of the app, not a feature hidden inside it.**

You open Kestrel, and the cursor is already in one large search box in the middle of the top bar. Typing anything shows matching files from the whole disk within one keystroke. Browsing folders, previewing, tagging and moving files all happen around that search box. Search should never feel like a mode you switch into.

## 3. What went wrong in v0.1 (the build to date)

v0.1 proved the engine but got the product shape wrong.

| Area | v0.1 behaviour | Why it's wrong |
|---|---|---|
| Search bar | Hidden inside the path bar. It only appears after you click the path or start typing, and it swaps places with the breadcrumbs. | You can't see it and it isn't in the middle, so it doesn't feel like the main feature. Nothing tells you search exists. |
| Search vs. path | One field does both. Text that looks like a path navigates; anything else searches. | You can't tell which will happen. Typing `/src/main` might navigate or might search. |
| Scope | Search covers the whole disk unless you know Ctrl+F types a quoted `"path/"` prefix. | Nothing on screen shows the scope, and you can't change it with a click. |
| Results | A plain table in a tab, titled "Search: …". | Results don't look like a first-class view. There's no count up front, no filters and no matched-text highlighting. |
| Discoverability | The syntax is only in a dialog in the ⋮ menu. | Power features like `ext:`, `size:`, `dm:` and `dupe:` are invisible. |
| Live behaviour | Never tested with the root daemon. | Unknown whether the index really stays live. |

What carries forward unchanged: the index engine, the query language, the daemon, `ks`, file operations, tabs and dual pane, the preview pane, tags, Git status, packaging.

## 4. Users

- **Primary: Alex.** A power user who lives in the terminal and in GNOME. Has about 2 million files, plus project folders, NAS mounts and Timeshift snapshots. Wants instant search everywhere and a file manager good enough to replace Nautilus day to day.
- **Secondary:** other users of the same machine. They must only ever see files they could see themselves.

## 5. Goals and non-goals

### Goals
1. **G1, search first:** one prominent search box, centred, focused on launch, always visible.
2. **G2, instant:** results update on every keystroke, in under 50 ms on 2 million files.
3. **G3, always correct:** the index reflects changes on disk within 1 second, and survives reboots and event overflows.
4. **G4, safe:** results never reveal a filename or file contents the asking user couldn't access.
5. **G5, a real file manager:** good enough to set as the default and not go back to Nautilus.
6. **G6, installs cleanly:** one `.deb`, with no manual steps after `apt install`.

### Non-goals (v1)
- Windows or macOS builds; non-GNOME desktops beyond "it runs".
- Indexing network shares (SMB/NFS/SFTP) live. You can browse them, but they aren't indexed.
- Cloud sync, file versioning, or a built-in editor.
- Full-text indexing. `content:` stays a brute-force, opt-in search.

---

## 6. Experience design

### 6.1 Window layout (the main change)

```
┌──────────────────────────────────────────────────────────────────────────────────────┐
│ ‹  ›  ↑      ┌──────────────────────────────────────────────┐ [Everywhere ▾]   ▦ ≡ ⋮ │  ← header
│              │ 🔍  Search files…   ext:pdf  size:>1mb     ✕ │                        │
│              └──────────────────────────────────────────────┘                        │
├────────────┬─────────────────────────────────────────────────────────────┬────────────┤
│ Places     │ ⌂ › Projects › kestrel                          [tab][tab]+ │  Preview   │  ← location bar
│ Bookmarks  │─────────────────────────────────────────────────────────────│            │     per pane
│ Saved      │  Name              Folder            Size   Modified        │            │
│ searches   │  ...                                                        │            │
│ Tags       │                                                             │            │
│ Devices    │                                                             │            │
│            │  1,204 results · 3.1 GB · 12 ms           [Type▾][Date▾][Size▾] │         │  ← status + filters
└────────────┴─────────────────────────────────────────────────────────────┴────────────┘
```

- **The search box is the header's centre widget.** It's always visible: 480 px minimum, 720 px maximum, centred, and taller and more prominent than a normal entry.
- **The breadcrumb path moves out of the header** into a location bar at the top of each pane. With dual pane on, each pane has its own path. There's still only one search box, and it acts on the active pane.
- **A scope selector** sits next to the search box: *Everywhere* / *This folder* / *This folder only (no subfolders)*. It shows plainly what's being searched. *This folder* uses `under:` and *This folder only* uses `parent:`, so you never have to type those.
- **Focus on launch:** the search box has focus when a window opens. Esc clears it, a second Esc returns to the folder, and Down moves into the results.

### 6.2 Search ↔ browse model
- **Search box:** always searches and never navigates.
- **Going to a path:** Ctrl+L, or clicking the location bar, turns the location bar into an editable path field. Paths and URIs go there, not in the search box.
- **Empty search box:** you're browsing the current folder.
- **Non-empty search box:** you see results in the current tab. The tab title becomes the query, and the location bar shows the scope (e.g. "Everywhere", or "in Projects ›").
- **Clearing the search** returns to exactly the folder, scroll position and selection you had before.
- **Opening a folder from the results** clears the search and browses into it. That step goes into back/forward history.
- **Typing anywhere** in the file list (when no text field has focus) goes into the search box, as it does in Everything.

### 6.3 Results view
- **Columns:** Name (with matched text highlighted), Folder, Size, Modified, Type, plus Tags and Git when relevant. Sorting is done by the daemon, so it's instant at any size.
- **Live summary in the status bar:** "N results · total size · query time", or the query error in red with where it went wrong.
- **Filter chips** under the results: Type (Folders, Documents, Pictures, Audio, Video, Archives, Executables), Date modified, Size. A chip just adds or removes query terms, and the box shows the resulting text so it can be edited. Chips and syntax are two views of the same query.
- **Empty and error states:** "No results" offers to widen the scope. If the daemon is down, there's a clear banner with a *Start service* action (via `pkexec systemctl start kestrel-indexd`).
- **Large result sets:** a million hits scroll smoothly through lazy paging, which v0.1 already does.

### 6.4 Search assistance
- **Autocomplete:** typing a modifier name (`ex` → `ext:`, `dm:` → today/yesterday/…) offers completions in a popover under the box.
- **Recent and saved searches:** focusing the empty box shows the last 10 searches and your saved searches. Ctrl+D saves the current search, which then appears in the sidebar.
- **Syntax help:** a "?" button in the box opens a cheat sheet popover. It's a popover, not a modal dialog.
- **Debounce:** results update after a 75 ms typing pause. Pressing Enter commits the query to history.

### 6.5 File manager (kept from v0.1, polished)
- **Tabs:** drag tabs between panes and windows; session restore.
- **Dual pane** (F3): F7 copies and F8 moves to the other pane.
- **Views:** details, grid with thumbnails, columns.
- **Preview pane** (Space or Alt+P).
- **File operations:** copy, cut, paste interchangeable with Nautilus; trash; delete; rename; new folder; undo with a toast.
- **New:** a conflict dialog (Replace / Keep both / Skip, with "apply to all") instead of always auto-renaming.
- **New:** inline rename (F2 edits in place) instead of a dialog.
- **Context menu:** Open With, terminal here, tags, compress/extract, properties.
- **Git status column and branch;** colour tags (`tag:`); command palette (Ctrl+Shift+P).

---

## 7. Functional requirements

Priority: **P0** = v1 can't ship without it · **P1** = v1 should have it · **P2** = later.

### 7.1 Search UI
| ID | Requirement | Pri | Acceptance |
|---|---|---|---|
| S1 | Search box is the centred header widget, always visible | P0 | Visible in every state, including browse mode, dual pane, narrow windows (≥ 640 px) |
| S2 | Search box has focus when a window opens | P0 | Launching the app and typing `abc` shows results without a click |
| S3 | Search box never navigates; paths go through Ctrl+L / the location bar | P0 | Typing `/etc/hosts` in the search box searches for it |
| S4 | Scope selector: Everywhere / This folder / This folder only | P0 | Switching scope re-runs the query; the scope shows on screen at all times |
| S5 | Results update while typing (75 ms debounce), with no UI stalls | P0 | Typing at 10 chars/sec never freezes the window; search runs off the main thread |
| S6 | Esc clears the search and restores the previous folder and selection | P0 | Round trip is exact |
| S7 | Typing in the file list sends the text to the search box | P1 | |
| S8 | Status bar: count, total size, query time; parse errors inline | P0 | |
| S9 | Filter chips for type/date/size, kept in sync with the query text | P1 | Clicking "Pictures" adds `pic:`; deleting `pic:` un-highlights the chip |
| S10 | Matched-text highlighting in the Name column | P1 | |
| S11 | Autocomplete for modifiers and their values | P1 | |
| S12 | Recent searches when the box is empty and focused; saved searches (Ctrl+D) in the sidebar | P1 | |
| S13 | Syntax cheat sheet popover from a "?" button in the box | P1 | |
| S14 | Results sorted by any column instantly (daemon-side) | P0 | |
| S15 | "Daemon not running" banner with a start action | P0 | |

### 7.2 Index & daemon (exists; P0 items need verifying)
| ID | Requirement | Pri | Acceptance |
|---|---|---|---|
| D1 | Indexes all local filesystems at boot; ready within 10 s of a cold boot | P0 | Measure on Alex's laptop with `systemd-analyze`-style timing |
| D2 | Live updates through fanotify; changes searchable within 1 s | P0 | `scripts/smoke-live.sh` passes as root |
| D3 | Recovers from event-queue overflow with a background rescan; searches keep working | P0 | |
| D4 | Exclude list in `/etc/kestrel/excludes`, default `/timeshift`; editable from Preferences | P0 / P1 | |
| D5 | Results filtered per caller (mode bits); `content:` checks read permission before opening | P0 | Existing tests plus a root-run test |
| D6 | ACL-aware permission filter | P1 | |
| D7 | Saves the index to disk for a faster start, if D1 misses its target | P2 | |
| D8 | Optional HTTP search, off by default | P2 | Exists |
| D9 | Index removable drives when they're mounted; drop them when unmounted | P1 | |

### 7.3 Query language (exists)
AND, OR (`|`), NOT (`!`), quotes, wildcards, `regex:`, `case:`, `path:`, `file:`, `folder:`, `ext:`, `size:`, `dm:`, type macros, `tag:`, `parent:`, `under:`, `depth:`, `empty:`, `content:`, `dupe:`, `sizedupe:`, `namesizedupe:`.
- **P1:** `dc:` (date created), `da:` (date accessed), `len:` (name length), Unicode case-folding, `<grouping>`.

### 7.4 File management
| ID | Requirement | Pri |
|---|---|---|
| F1 | Tabs, dual pane, three views, session restore | P0 (exists) |
| F2 | Copy, move, trash, delete, rename, new folder, undo; clipboard compatible with Nautilus | P0 (exists) |
| F3 | Conflict dialog: Replace / Keep both / Skip / Apply to all | P0 |
| F4 | Inline rename in the list | P1 |
| F5 | Drag and drop onto folders in the list and onto sidebar items, not just the empty background | P1 |
| F6 | Preview pane: images, video/audio, text (with syntax highlighting), PDF, folder totals | P1 (mostly exists) |
| F7 | Git column and branch; colour tags; command palette; properties; archives | P1 (exists) |
| F8 | Can be set as the default file manager (`inode/directory`) without breaking "Show in Folder" from other apps (D-Bus `org.freedesktop.FileManager1`) | P1 |
| F9 | Custom tag names, not just the 7 fixed colours | P2 |

### 7.5 CLI
- `ks` mirrors Everything's `es.exe`. It exists; P1 is to add `--scope DIR` and `--count`.

---

## 8. Non-functional requirements

| Area | Target |
|---|---|
| Query latency (2M entries, name query) | p95 < 50 ms in the daemon; < 100 ms from keystroke to pixels |
| Live update latency | < 1 s from a file change to it being searchable |
| Daemon memory | < 300 MB for 2M entries |
| GUI cold start | < 400 ms to an interactive search box |
| UI responsiveness | No main-thread operation over 16 ms during typing or scrolling |
| Security | Filenames and contents never leak across users; the daemon runs hardened (current unit); HTTP is off by default |
| Accessibility | Every action reachable by keyboard; screen-reader labels on all icon buttons; works with GNOME large text and high contrast |
| Theming | Follows system light/dark and accent colour (libadwaita) |

---

## 9. Milestones

| M | Scope | Exit criteria |
|---|---|---|
| **M0: Verify** | Install the v0.1 `.deb`; run `smoke-live.sh`; measure cold boot | D1, D2, D3 pass or have bugs filed |
| **M1: Search-first shell** | S1–S6, S8, S14, S15; location bar moved into panes; search off the main thread | Alex can launch the app, type, and get results with no clicks |
| **M2: Search polish** | S7, S9–S13 | Filters and history usable without knowing the syntax |
| **M3: File manager parity** | F3–F6, F8 | Alex sets Kestrel as the default file manager for a week |
| **M4: Hardening** | D6, D9, query P1s, accessibility pass | No known permission leaks; keyboard-only run-through passes |

---

## 10. Success metrics
- Alex uses Kestrel instead of Nautilus for 2 weeks straight.
- Median time from launch to opening the file you were looking for is under 3 seconds.
- Zero results that should have been hidden, found by an automated multi-user test.
- `smoke-live.sh` and the full test suite pass on every release.

---

## 11. Decisions
- 2026-10-25: search box **centred in the header**; default scope **Everywhere**.

## 11a. Status log
- **M1 built (v0.2.0, 2026-10-25).** S1–S6, S8, S14, S15 done; location bar per tab; search on a worker thread.
  Verified by real clicks/keys (headless Chromium driving GTK's Broadway backend), which found and fixed:
  window controller freed after startup (every handler silently dead — the cause of "buttons don't work"),
  search box signals never firing, header min-width 1219 px clipping the menu/close buttons, panic on Up,
  palette ignoring Esc, rename dialog not focusing its entry and dropping the extension, Undo-from-trash taking
  61 s (GVfs D-Bus timeout; now reads ~/.local/share/Trash directly, 4 ms).
- **M0 findings (root daemon as installed):** 6.38M entries / peak 3.2 GB RAM — ~4.4M are root-only
  (Docker image layers). Default excludes now add `/var/lib/docker`, `/var/lib/containers`.
  Scan peak memory still needs work (D1).
- **v0.2.1:** first `smoke-live.sh` run as root — live modify/new-tree/delete pass; create/rename failed only because
  the script stopped waiting before the root scan finished (script fixed); "/root leak" was a bad test
  (`path:/root/` substring) — verified no leak with `under:/root`. Installed daemon after excludes: 1.97M entries;
  RSS 777 MB → 194 MB via shrink_to_fit + malloc_trim after scans. The other ~2.2 GB systemd reports is reclaimable
  kernel dentry/page cache from the walk.
- **M0 DONE (2026-10-25):** `smoke-live.sh` 10/10 as root on v0.2.1 — live create/rename/modify/new-tree/delete,
  per-user filtering (/root hidden from alex, visible to root), Timeshift + Docker excluded. Root scan 32 s,
  1.97M entries, 173 MB index.

## 12. Open questions (for Alex)
1. **Search box placement:** centred in the header (as drawn), or a full-width bar under the header like Everything's own window?
2. **Default scope:** should a new search default to *Everywhere* (Everything-style) or *This folder* (Files/Nautilus-style)?
3. **Opening a result:** when you open a folder from the results, should it replace the search in that tab, or open a new tab and keep the results?
4. **Default file manager:** should Kestrel take over `inode/directory` on install, or stay opt-in?
5. **Name:** keep "Kestrel", or rename before 1.0?
6. **Other users:** is this machine single-user in practice? If so, D6 (ACLs) can drop to P2.
- **M2 DONE (v0.3.0, 2026-10-25):** S7 (type in list → search box), S9 filter chips (Type/Date/Size, kept in sync
  with the query text both ways, Clear filters), S10 bold match highlighting in names, S11 autocomplete for
  modifiers and values (Tab accepts), S12 recent searches on focusing the empty box + saved searches,
  S13 syntax popover. Also: empty-valued modifiers (`ext:` mid-typing) are ignored instead of matching nothing;
  Esc clears a search from the file list / filter bar too. All verified with real clicks/keys.
- **Startup fix (v0.4.0):** daemon opens its socket immediately and answers `indexing` until the first scan lands;
  the app shows "Indexing your files…" and retries every 1.5 s (verified: results appeared by themselves after the
  42 s scan). `ks` prints "Still indexing".
- **M3 DONE except drag-and-drop verification (v0.4.0, 2026-10-25):** F3 conflict dialog (Replace / Keep Both /
  Skip; replaced items go to the trash and Undo restores them — click-tested), F4 rename in place (popover on the
  row/tile, stem preselected — click-tested), F5 drops onto folder rows/tiles and sidebar places/bookmarks/drives,
  Trash row trashes (NOT verifiable headlessly: Broadway has no DnD — needs a manual check on Wayland),
  F8 FileManager1 D-Bus service (`kestrel --filemanager1`; ShowItems tested over D-Bus) + "Default File Manager"
  menu toggle (user-level service file + `xdg-mime`; not exercised to avoid changing Alex's defaults).
  Also fixed: file shortcuts (Ctrl+C/V, Delete, F2…) and type-to-search died after navigating, because focus fell
  to the window; both now live on the window in the bubble phase.
- **M3 manual checks passed (2026-10-25):** drag-and-drop onto folders/sidebar/trash, and the Default File Manager toggle.
- **M4 built (v0.5.0, 2026-10-25):** D6 ACL-aware permission filter (dir ACLs at scan time, file ACLs before
  `content:` reads; end-to-end deny test), D9 drives indexed on mount / dropped on unmount (watch loop polls
  /proc/self/mounts; mount fds no longer held open, so unmount isn't blocked), query P1s `dc:` `da:` `len:`,
  Unicode lowercasing (é/É), `<…>` grouping incl. `!<…>`; `ks --scope DIR --count`; accessibility: icon buttons
  named from tooltips, labelled search/scope/location fields, F6/Shift+F6 region cycling, keyboard-only run-through.
  D9 needs root to verify: `scripts/smoke-live.sh` now mounts/unmounts a loop-ext4 "drive".
- **v0.5.1 (2026-10-25):** v0.5.0 broke live updates (O_PATH mount fd rejected by open_by_handle_at); fixed.
  **M4 verified:** `smoke-live.sh` 13/13 as root — live create/rename/modify/new-tree/delete, /root privacy,
  Timeshift + Docker excluded, drive indexed on mount, unmounts cleanly (not held busy), dropped on unmount.
  All milestones M0–M4 done.
- **v0.6.0 (2026-10-25): D7 + scan memory.** Snapshot at /var/lib/kestrel/index.bin (0600, StateDirectory 0700),
  saved on SIGTERM/shutdown and every 10 min if changed; loaded at start in ~0.25 s so search works immediately
  while a background rescan catches offline changes; damaged/other-version snapshots are rejected (validated
  links/offsets). jwalk replaced by a worker-pool walker that never waits on tree order: scan peak 959 MB → 294 MB
  and scan time 41 s → 20.6 s (2M entries). Boot peak with snapshot + rescan: 527 MB; steady ~257 MB.
  Smoke test uses its own snapshot path so it can't touch the service's.
- **v0.6.0 verified (2026-10-25):** smoke-live 13/13 as root (root scan now 10–20 s); on restart the service saved
  the snapshot (0.4 s), the new one loaded it (0.24 s) and served immediately, then rescanned (20.5 s);
  /var/lib/kestrel unreadable to Alex as intended. Steady RSS ~263 MB.
- **v0.6.1:** fixed search box eating the space and jumping the cursor to the start (window re-synced the trimmed
  query into the box mid-typing). **v0.7.0:** Preferences (☰ → Preferences, Ctrl+,): style, default view,
  hidden files, reopen tabs, default scope, type-to-search, default file manager, index status, excluded folders
  (D4 — applied via pkexec + service restart), Rebuild Index. Settings in ~/.config/kestrel/settings.json.
  Apply/Rebuild need Alex's password, so not click-tested.
- **v0.7.1:** events that can't be resolved (not ESTALE; e.g. ENOMEM seen once during a cargo build) now schedule
  a repair rescan, rate-limited to one per 10 min, instead of being silently dropped. Smoke test checks it end to
  end via test-only env hooks (KESTREL_TEST_LOSE, KESTREL_LOST_GAP_SECS).
- **v0.7.2 (2026-10-25): "crash on clicking Trash".** Not a crash: the window froze 20–60 s on synchronous GVfs calls
  (folder title lookup on the main thread) because trash:/// checks every mount and the TV box's CIFS mount
  (/mnt/tvbox) was down ("Host is down" after 20 s); GNOME showed it as not responding. Fixes: folder titles never
  touch disk/GVfs; "Loading…" page for slow folders; Trash now reads ~/.local/share/Trash directly (instant) with
  Restore / Delete Permanently / Empty Trash (other drives' .Trash-uid not shown). Also: recent-searches popup no
  longer opens at launch; thumbnailer temp files under ~/.cache/thumbnails (AppArmor blocked papers in /tmp).
- **v0.7.2 installed (2026-10-25):** smoke-live 15/15 as root incl. lost-event repair; service restarted on the
  snapshot path.

- **v0.8.0 (2026-10-25): bundled 7-Zip + document previews.** Ubuntu's 7zip (26.00) carries none of 2026's
  decoder CVE fixes (incl. CVE-2026-14266 XZ heap overflow) and has no RAR codec, so the .deb now ships the official
  static 7zz 26.05 at /usr/lib/kestrel/7zz (+ License.txt; fetched and digest-checked against the GitHub release by
  `scripts/update-7zip.sh VERSION`; re-run monthly). Every extraction (7zz and tar) runs under bubblewrap: no network,
  read-only system, home/tmp/run/media hidden, only the archive and the temp output folder visible. Encrypted
  archives prompt for a password (sent over stdin); 7-Zip progress drives the header bar. Preview pane renders the
  first 20 pages of PDFs (pdftoppm) and Word/Excel/PowerPoint/ODF/RTF (LibreOffice → PDF), both in the same sandbox,
  cached in ~/.cache/kestrel/preview; Markdown renders formatted. Depends: bubblewrap (7zip dropped); Recommends:
  poppler-utils, libreoffice-writer/-calc/-impress. Not done: cancel button, per-file extract conflicts, archive
  listing in preview.
- **v0.9.0 (2026-10-25): renamed Chimera → Kestrel.** Binaries kestrel / kestrel-indexd / ks (was cx), app id
  dev.kestrel.Kestrel, /run/kestrel, /var/lib/kestrel, /etc/kestrel, ~/.config/kestrel, env KESTREL_*. The .deb
  Conflicts/Replaces chimera; postinst deletes the old root-owned /var/lib/chimera snapshot. First launch moves
  ~/.config/chimera over and re-points the default-file-manager hookup (D-Bus service + xdg-mime). Kept on purpose:
  the `user.chimera.tags` xattr, so existing colour tags survive.
- **v0.9.1 (2026-10-25):** sidebar toggle moved to the window's top-left corner (sidebar header; shown in the content
  header only while the sidebar is hidden), and F9 now actually toggles it (the tooltip promised it; never bound).
  New preview-pane toggle mirrors it top-right (Alt+P). smoke-live.sh refuses to run under sudo: Alex's 0.9.0 run
  reported "no /root for you" as FAIL only because the user-side queries ran as root.
- **v0.9.1 installed (2026-10-25):** smoke-live 15/15 as Alex (after the script learned to clear a root-owned stale
  log/socket left by a sudo run, which had made it test against a daemon that never started).
- **v0.9.2 (2026-10-25): document preview pages one at a time.** Multi-page documents were stacked into the pane and
  squeezed unreadable. Now: one page, "‹ 3 / 12 ›" above it (above, so the arrows don't move as page heights change),
  mouse wheel/touchpad over the page flips pages (past either end the pane scrolls as usual). No 20-page cap: pdfinfo
  gives the count and each page renders on first view (sandboxed pdftoppm, cached; Office converts to PDF once).
- **v0.9.3 (2026-10-25): tags rethought (Finder-style), preview panel, header + path-bar tweaks.** Tags were buried
  (sidebar section below Devices, a two-level right-click submenu of colour names). Now: a row of colour dots in the
  right-click menu (one click toggles on the selection; checks show tags every selected file has; Clear at the end),
  a dot row under the sidebar places (click = index search `tag:colour`), and dots under the name in the preview
  (acting on the previewed file). Fixed along the way: a file whose attributes change (tagged, saved) was replaced in
  place by GtkDirectoryList and silently dropped from the selection; selections now survive in-place updates. Preview
  pane is its own titled panel (sidebar shade, border, "Preview" header with close). Scope picker (Everywhere/This
  Folder) moved next to the View button. Path bar: clicking the current folder refreshes, clicking it again edits
  the path. New app icon: an original drawing of an American kestrel on a perch (packaging/dev.kestrel.Kestrel.svg).
  Modified column (and preview) shows time first with a relative date: "14:32 Today", "09:05 Yesterday",
  "18:40 12 Oct", "18:40 12 Oct 2024"; Preferences → Appearance → Time format: Follow System (GNOME clock-format) /
  24-hour / 12-hour, applied live. Sorting still uses the timestamp. Sidebar Tags row moved to the very top, above Home.
  Preferences → Browsing → File type names: Full ("Microsoft Excel Worksheet", GIO's description) / Short ("Excel":
  drops "Microsoft" and a trailing generic noun) / Extension ("XLSX", "TAR.GZ"); Type column sorts by what it shows.
  Unknown types ("text/x-foo type") show as "RS file" instead.
- **v0.9.4 (2026-10-26): column options.** Alex couldn't resize Type (the last column's only resize handle is its
  right edge, against the window border; dragging the gap to its left resizes Modified) or drag headers to reorder.
  Preferences → Columns: per column (Size, Modified, Type) a width (px), move left/right, show/hide, plus Reset;
  applied live to every open list. Widths dragged in the list (and any header reorder) are saved too. Name stays
  first and fills the rest; Folder (search results) and Git (repos) still show automatically.
- **v0.9.5 (2026-10-26): drag headers to reorder columns.** GTK 4.22's own reordering never starts: the column
  title's click gesture claims the press, which denies the view's drag gesture (`gtkcolumnviewtitle.c`
  `click_pressed_cb`; a bare Python ColumnView fails the same way). Kestrel turns GTK's off and watches raw
  capture-phase events: press on a Size/Modified/Type header, release over another one, and it moves there (saved).
  Name/Git/Folder can't be moved or dropped onto; a resize drag (width changed) never reorders; clicks still sort.
  New app icon: kestrel head on a yellow folder (Gemini-generated, background cut out, 512 px PNG embedded in the SVG).
- **v0.9.6 (2026-10-26): search box fills the header.** In a non-maximised window it sat off-centre: AdwHeaderBar
  centres its title between equal-width sides and the right side (scope, View, menu, preview, window buttons) is
  ~360 px vs ~120 px on the left, so GTK shoved it left. Alex: don't centre it, stretch it from the up arrow to the
  search-syntax (i) button. The whole header row is now one full-width title box (nav buttons, search, end buttons,
  gtk::WindowControls in place of the header bar's own); the search box takes all the space between.
- **v0.9.7 (2026-10-26): Extract To…, many more extractable formats.** Right-click (and palette) → Extract To…
  picks a folder (FileDialog, starts in the current folder) and runs the same sandboxed extract there. Extract was
  only offered for 10 extensions; it now covers everything the bundled 7-Zip reads that is an archive, package or
  disk image (~70 extensions: .iso .img .dmg .vhd(x) .vmdk .qcow2 .deb .rpm .msi .cab .wim .squashfs .apk …), from
  one list (`fileops::ARCHIVE_EXTS`) that also names the output folder. Full list: docs/extract-formats.md.
  Search box: Alex says it "doesn't resize at all" between windowed and maximised; on headless GNOME Shell (Wayland,
  1536 px virtual monitor, Alex's own config) the entry measures 393 px windowed (1200) and 669 px maximised, so
  still unexplained — needs a screenshot from Alex.
- **v0.9.8 (2026-10-26): every runtime dependency declared.** Depends adds gzip, xz-utils, bzip2, zstd (tarball
  decompressors tar calls), gvfs (trash://, recent://, network://), xdg-utils (default-file-manager hookup) and pkexec
  ("Start Service"); Recommends adds ncompress (.tar.z/.taz). `sudo apt install ./kestrel_*.deb` lists whatever a
  machine is missing and asks once (Y/n); `--no-install-recommends` skips the previews/terminal/ncompress extras.
- **v0.9.9 (2026-10-26): icon follows the icon theme; preview pane at start.** The icon's folder takes the colour of
  the active icon theme's own folder icon (its most common saturated colour: Yaru accents, Papirus, Adwaita…).
  packaging/icon/split.py splits the art into a tintable folder layer (grey shading, 128 = front face) and the bird;
  icon.rs tints, composites, and writes ~/.local/share/icons/hicolor/scalable/apps/dev.kestrel.Kestrel.svg (searched
  before /usr/share) at launch and on every gtk-icon-theme-name change; a grey/unknown folder removes it (packaged
  yellow shows). Beak and eye ring stay yellow (not connected to the folder). Preferences → Browsing → Preview pane
  at start: Remember Last State (default; session.json `preview`) / Always Show / Always Hide.
- **v0.9.10 (2026-10-26): cancel extract, extract conflicts, archive listing, preview cache pruning.** A Cancel
  button sits beside the progress bar while an archive extracts (7-Zip or tar); it SIGKILLs the outer bwrap, which
  takes the sandbox down (--die-with-parent), and the staging folder is removed. When the extracted item's name
  already exists, the copy/move conflict dialog asks Skip / Keep Both / Replace (Replace trashes the old one; Undo
  restores it); Cancel there = Skip. The preview pane lists an archive's contents (sandboxed `7zz l -slt`, or `tar -tv`
  for tarballs): "N files, M folders · size unpacked" and up to 1000 paths; encrypted headers say so.
  ~/.cache/kestrel/preview is pruned at launch: entries not viewed for 30 days go, then least recently viewed until
  under 512 MB (each open touches its folder's mtime). Unit-tested (cancel, both listings, prune); not click-tested —
  Broadway can't run inside the Claude sandbox this session.
