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
- **v0.9.11 (2026-10-26): split archives work; more ZIP-based formats.** The sandbox only exposed the clicked file,
  so split 7z failed ("Unexpected end of archive") and split ZIP wrote files with missing data; `sandboxed()` now
  read-only binds every volume with the same stem (`fileops::volumes`: .001…, .z01…/.zip, .partN.rar, .rar/.rNN).
  Old-style RAR parts (.r00…) extract from the .rar (`first_volume`). Extract now offered on .war .whl .crx .epub
  .cbz .r00. Office/OpenDocument stay preview-only (no Extract item).
- **v0.9.12 (2026-10-26): visible progress, ISO extract fix, type-aware search.** Jobs show a bottom row: what's
  running ("Extracting x.iso"), a progress bar, "42% · about 2 min left", Cancel (was a thin OSD line under the
  header nobody saw). Ubuntu ISOs contain `ubuntu -> .`; 7-Zip refuses it as "Dangerous link path" and exits 2, so
  Kestrel deleted a complete 2.8 GB extraction and toasted a progress fragment ("0M Scan …"). Now refused links count
  as skipped ("Extracted x, skipped 1 unsafe link") and real failures show 7-Zip's ERROR line (`fileops::outcome`).
  Search: a bare `.deb` ranks .deb files first (no more vnd.efi.iso.xml above the ISOs); bare `pdf`, `word`, `excel`,
  `powerpoint`, `spreadsheet`, `presentation` also match files of that type and rank them first (`budget excel` finds
  Budget.xlsx). Ranking is daemon-side (`Query::boost`); smoke-live 15/15 as Alex on 0.9.12.
- **v0.9.13 (2026-10-26): checksums, archive browsing, drive space, links, scripts, batch rename, terminal.**
  Properties → Checksums (MD5/SHA-1/SHA-256 on request, paste to compare). Double-click opens zip/7z/rar/tarballs as
  folders (GVfs archive://, needs gvfs-backends; packages, books, disk images still open in their apps). Sidebar
  free-space bars (File System + drives; red past 90 %). Symlink badge in the list; status bar shows "→ target" or
  "N hard links". Right-click → Scripts runs ~/.local/share/nautilus/scripts the Nautilus way (args, cwd,
  NAUTILUS_SCRIPT_* env). Copy/move no longer stops at the first failure: the rest go through, one dialog lists the
  failures. Batch rename (Rename with several selected): find/replace or regex with $1, case, numbering, live
  preview with clash checks, swaps/chains handled, one Undo. Terminal panel (Ctrl+`, F4 is other-pane): vte4 0.10;
  views cd the shell (only at its prompt, terminal not focused; Ctrl+U + leading-space cd), the shell's cwd (polled
  from /proc, any shell) moves the view; `exit` closes it. New dependency: libvte-2.91-gtk4-0 (auto via shlibs);
  build needs libvte-2.91-gtk4-dev. Unit-tested (40); not click-tested.
- **Archive test automation (2026-10-26, after 0.9.13).** `scripts/make-archive-fixtures.sh` builds a sample for all
  70 ARCHIVE_EXTS (committed under crates/kestrel/tests/fixtures/archives, ~780 KB in git; sources/licences in its
  README); `fileops::every_format_extracts` extracts each through `unpack()` (the sandboxed core of Extract Here,
  factored out), checks hello.txt where the format holds it, and checks every listed file came out. Fails on an
  extension with no sample. Verified it catches the 0.9.11 split-archive bug when that fix is reverted. 70/70 pass.
- **v0.9.14 (2026-10-26): preview listing fixes found by the format test.** The per-format report showed the
  preview pane listing nothing for .xz .bz2 .zst .lzma .z .deb .udeb .rpm (7-Zip gives a lone stream no `Path`; it
  is now named like Extract names it: innermost layer minus its extension, e.g. data.tar.xz → data.tar) and
  counting XAR/.pkg folders as files (an entry with entries under it is a folder). every_format_extracts now
  requires preview count == extracted count and checks hello.txt inside disk images too; prints a per-format
  report with `-- --nocapture`. 70/70.
- **Maintenance 2026-10-26.** Bundled 7-Zip 26.05 is the latest release (2026-10-21); re-fetched through
  scripts/update-7zip.sh, digest-verified, byte-identical. Rust crates: all at newest compatible versions, no
  newer majors for any direct dependency. Build-dependency list corrected (readelf -d): build-essential,
  pkg-config, libgtk-4-dev, libadwaita-1-dev, libvte-2.91-gtk4-dev, zlib1g-dev; libgit2 is vendored and the
  libgit2/poppler/gstreamer -dev packages were never needed. Every package named in README exists in 26.04.
- **Licence + GitHub README (2026-10-26).** Dual-licensed MIT OR Apache-2.0 (Alex's choice; LICENSE-MIT,
  LICENSE-APACHE, shipped in /usr/share/doc/kestrel). README rewritten for GitHub (badges, screenshots slots in
  docs/screenshots/, mermaid architecture, features, syntax, shortcuts, dependencies, limitations, credits). Not
  published yet: Alex reviews first; screenshots still to be taken.
- **v0.9.22 (2026-10-26): README screenshots.** scripts/screenshots.sh makes docs/screenshots/{kestrel,search,
  preview}.png from the real app on a made-up home ("maya": PDFs made by LibreOffice, sparse ISOs, a git
  project) with its own index and a thumbnail warm-up pass. App changes it needed: folders under home show as
  `~/…` in the Folder column and the preview's Location (`browser::tilde`; Properties keeps the full path);
  `--select` / Show in Folder in a search tab waits for the results; thumbnails get a faint outline so a white
  PDF page still reads at 16 px. (A /home/maya bubblewrap overlay was tried first: Kestrel's own PDF sandbox
  can't nest inside it.)
  Markdown preview shows the pictures a file uses when they're local files beside it (`md_page`: markdown
  images and HTML <img>, sized from their shape); web images such as badges are left out on purpose.
- **v0.9.21 (2026-10-26): folder sizes on network shares.** A share's sidebar bar is its disk's totals
  (both NAS shares report the same 3.9 TB volume; that's what a share can report, and it answers "will
  this fit", so it stays). For a folder's own size, Properties now shows Calculate on folders the index
  doesn't cover (network shares, tmpfs, excluded): `props::walk_size` totals it in the background with a
  running count, and Cancel stops it. Checked on the My Cloud share against an independent count.
  Network page (network.rs), like GNOME Files': Connected (mounted shares, with Disconnect), Previous
  (GTK's ~/.config/gtk-4.0/servers, shared with Files, with Forget; plus online-account drives), Available
  on Current Network (network:///, DNS-SD / WS-Discovery) and Connect to Server. network:/// alone listed only
  the last group. The servers file is read/written by hand: glib-rs 0.22 BookmarkFile panicked on a NULL
  from C and crashed the app. feature-test kills the wsdd that GVfs network browsing leaves behind.
- **v0.9.20 (2026-10-26): music player.** Pressing Play on some MP3s (Alex's "01 Peaceful Slumber.mp3")
  killed the app: GtkMediaFile always gives GStreamer 1.28's playbin3 a video sink, and decodebin3 then
  aborts the process (`mq_slot_handle_stream_start: assertion failed: (collection)`; reproduced with
  gst-launch playbin3 + video-sink, fine with flags=audio). Music now plays through Kestrel's own playbin
  (gstreamer-rs, flags audio+soft-volume): previous / next through the audio files in the folder, a seek
  bar with elapsed / total, repeat off / folder / song, and the next song starts when one ends. WAV
  confirmed. New build dep libgstreamer1.0-dev. GIO "created without standard::content-type" criticals
  fixed with `browser::content_type`. Feature test: music (MP3 + WAV; FT_SONG=file adds a real one).
- **v0.9.19 (2026-10-26): drops move.** A drop decided copy vs move from the source's offered actions,
  treating "COPY only" as Ctrl held; drags from the desktop and other apps offer only COPY, so they always
  copied. Now `fileops::drop_intent` reads the keys (Shift move, Ctrl copy) and otherwise drop_op moves on
  the same filesystem, copies across, as GNOME Files does. Alex confirmed dropping from the desktop works.
- **v0.9.18 (2026-10-26): thumbnails, whiteboards, drag.** PDF thumbnails: papers-thumbnailer's AppArmor
  profile only lets it write /tmp/gnome-desktop-*, so Kestrel's scratch file in ~/.cache never got written
  and no PDF ever got a thumbnail from Kestrel (pages showed only where GNOME Files had made one). Now
  `glib::file_open_tmp("gnome-desktop-kestrel-XXXXXX.png")`, and the details and column views make missing
  thumbnails like the grid (load_into). Nextcloud Whiteboard / Excalidraw drawings (.whiteboard, .excalidraw)
  render in the preview (whiteboard.rs: scene JSON -> SVG; colours sanitised) and get a MIME type
  (packaging/kestrel-mime.xml -> application/vnd.excalidraw+json, "Whiteboard drawing"). Drag and drop:
  pressing on a file started a rubber-band selection, so files couldn't be dragged; rubber band now only from
  empty space (drag_beats_rubberband). The XTest drop can't complete on bare Xvfb: FT_DND=1 on a desktop.
- **v0.9.17 (2026-10-26, in progress): previews and feature test.** Images decode off the GTK thread
  (glycin, then gdk-pixbuf; BMP was blank before; "No preview for this image format" otherwise). Music:
  cover art, title/artist/album/length/bitrate (lofty), player created only on Play (GStreamer aborted the app
  headless when every selected song pre-rolled). Plain-words error under audio/video that can't play;
  gstreamer libav/good/bad/ugly Recommended (24 codecs checked). Markdown renders as a page in the top slot
  like PDF/Office; raw HTML tags stripped. Kiwix .zim preview (zim.rs: header, binary search to namespace M,
  zstd/xz clusters via the system tools; 16 real files incl. 110 GB Wikipedia < 1 s). Mouse back/forward
  buttons. Space opens the preview (the list views ate it as select-toggle). `--select` waits for the folder
  to load. scripts/feature-test.py: drives the real app through AT-SPI + XTest, ~50 user-level tests.
- **v0.9.16 (2026-10-26): plain-words errors, search in the UI check.** Every error that reaches the
  screen goes through `browser::human_gio`/`human_io` ("You don't have permission", "The disk is full",
  "That drive or share isn't connected"; uncommon ones keep GIO's text, minus "(os error N)"); trash on a
  drive without one says to use Delete (Shift+Delete). Search-off state reworded ("Search is off", no daemon
  name, systemctl line or socket path). ui-check.sh runs its own index service as the user
  (`--no-watch`, new `KESTREL_ROOT` = one folder) and adds 7 screens: search, type ranking, picture filter
  in grid, content search, no results, service down, a folder you can't open (OK_ERR lets a scenario
  expect an error page). 35/35.
- **v0.9.15 (2026-10-26): review fixes.** (1) Index service: caller groups from SO_PEERGROUPS at connect time (was
  /proc/<pid>/status, a pid-reuse race); per-uid (32) and total (256) connection caps, 64 KB request lines (also
  for --http); unit gets MemoryHigh=1G, MemoryMax=3G, TasksMax=256; `content:` opens files with the caller's
  fsuid/fsgid/groups (per-thread setfsuid + raw setgroups; fails closed), so the kernel enforces permissions even
  when the index's mode is stale — unit needs CAP_SETUID/CAP_SETGID. smoke-live.sh gains "content: finds my own
  file", "content: root's readable file" and "content: kernel says no (stale index)". Daemon tests 1 → 3.
  (2) packaging/THIRD-PARTY-LICENSES.txt (scripts/third-party-licenses.py): licences of all 79 crates + C libgit2,
  shipped in /usr/share/doc/kestrel. (3) No blocking GIO on the GTK thread: transfer conflict check, drop
  copy/move decision, rename, new folder, Properties query + chmod switch, location bar, extract/compress name
  checks, batch-rename folder listings (which also fixes clash detection: gio::File compares by identity, so
  same-folder names were never matched). (4) scripts/ui-check.sh: real app under Xvfb with a demo home, screens
  to target/ui-check/. (5) archive.rs split out of fileops.rs; browser/ (mod, format, cells, searching, miller)
  and window/ (mod, actions, searchbar); clippy clean on all targets. 46 tests.
  Smoke run on 0.9.15 found a real (old) race: a full rescan only sees drives mounted when it starts, so a drive
  unmounted during it came back and one mounted during it vanished. Now every rescan is followed by a mount
  reconcile (`install()`), and the watch loop reconciles once after the first scan. smoke-live.sh reordered (its
  new lost-event file had triggered the repair rescan early); ui-check.sh feeds /dev/null to xvfb-run (its stty
  stopped the run with SIGTTOU under `timeout`).
  First ui-check run (23 screens) found: every view logged a GIO CRITICAL asking non-links for their link target
  (now `browser::link_target`); double-click/Browse Archive on .7z failed ("does not support seeking": GNOME's
  archive backend can't do 7z, single streams, disk images; `archive::browsable` from mounting all 70 samples,
  those open normally instead); `--select` and FileManager1.ShowItems did nothing in columns view (now selects in
  the first column); folder size said "0 items" for folders outside the index (now "Not in the search index");
  terminal panel was black in light mode (follows light/dark). smoke-live.sh waits for results, not rescan counts.
  Second run: Browse Archive mounted but then said "not mounted": GVfs names archive mounts by the file URI
  escaped twice; `archive_root` escaped once (broken since browse-archive was added). Inside an archive the tab
  and path bar now say "sample.zip" (`archive_source`), and the hidden-files filter no longer logs a CRITICAL
  there (archive listings have no is-hidden); archive mounts get no free-space bar (they report 100% full).
  Sidebar: File System and its free-space bar moved to the top, between Tags and Home (Alex's request).
- **v0.9.23 (2026-10-27): command bar, single window icon.** A toolbar under the header modelled on Windows 11
  Explorer's command bar: New ▾ (Folder) | Cut, Copy, Paste, Rename, Move to Trash | Extract, Restore, Empty
  Trash… (shown only while they apply: the button's visibility follows its action's sensitivity) | View ▾ (moved
  out of the header) | ⋯ (Undo, Copy Path, Compress, Delete Permanently, Add Bookmark, Open in Terminal,
  Properties, Preferences). Fix: with a title-button layout that starts with `icon:` the window icon showed
  twice (sidebar header + the content header's own WindowControls); the content header's start controls now
  show only while the sidebar is hidden. ui-check.sh uses such a layout so it stays caught. Not done: Sort menu
  (column headers sort), Share (no GNOME file-share), New → file, Select all/none/invert.
- **v0.9.24 (2026-10-28): the command bar does what Explorer's Home ribbon does.** Pin to sidebar; Copy ▾ (Copy
  Path); Paste ▾ (Paste as Link: symlinks, numbered on a clash, undoable — Explorer's "Paste shortcut"); Move to ▾
  / Copy to ▾ (Other Pane, Choose Folder…); Delete ▾ (Move to Trash, Delete Permanently); Rename; New ▾ (Folder,
  Empty File, also in the background menu); Archive group: Compress, Extract split button (Here / To… / Browse
  Archive) shown while an archive is selected; Open ▾ (Open, Open With, New Tab, Terminal); Properties; Select ▾
  (all Ctrl+A, none Ctrl+Shift+A, invert Ctrl+Shift+I). All new actions are in the palette. Icons with tooltips
  (Windows 11 style): with labels the bar needs ~1450 px; as icons ~1040 px, so the default window is now 1280
  wide and a narrower one scrolls the bar sideways. Not done: Easy access, Edit, History (Windows-only ideas).
