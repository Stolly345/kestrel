//! File operations through GIO, so local disks, SFTP/SMB/FTP, MTP and trash:// all behave the same.
//! Heavy work runs on a worker thread; the UI gets a progress bar, a toast and an Undo.

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Op {
    Copy,
    Move,
}

pub enum Undo {
    Moved(Vec<(gio::File, gio::File)>),
    Copied(Vec<gio::File>),
    Trashed(Vec<PathBuf>),
    Renamed(gio::File, gio::File),
    Created(gio::File),
    /// Several steps, undone in order (a replace: undo the transfer, then restore what it replaced).
    Many(Vec<Undo>),
}

/// What to do when an item with the same name is already at the destination.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Conflict {
    KeepBoth,
    Skip,
    /// The existing item goes to the trash first, so Undo can bring it back.
    Replace,
}

/// Outcome of a transfer: (src, dst) pairs done, and paths of items replaced (now in the trash).
pub struct Transferred {
    pub pairs: Vec<(gio::File, gio::File)>,
    pub replaced: Vec<PathBuf>,
}

struct Progress {
    done: AtomicU64,
    total: AtomicU64,
    /// Set by work that can stop midway (extracting): shows the Cancel button.
    can_cancel: AtomicBool,
    cancel: gio::Cancellable,
}

impl Default for Progress {
    fn default() -> Self {
        Progress {
            done: AtomicU64::default(),
            total: AtomicU64::default(),
            can_cancel: AtomicBool::default(),
            cancel: gio::Cancellable::new(),
        }
    }
}

/// Run `wait` (which waits for process `pid`) with the Cancel button able to kill it. SIGKILL
/// to the outer bwrap takes the sandbox down with it (--die-with-parent).
fn killable<T>(p: &Progress, pid: u32, wait: impl FnOnce() -> T) -> T {
    p.can_cancel.store(true, Ordering::Relaxed);
    let id = p.cancel.connect_cancelled(move |_| unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGKILL);
    });
    let r = wait();
    if let Some(id) = id {
        p.cancel.disconnect_cancelled(id);
    }
    p.can_cancel.store(false, Ordering::Relaxed);
    r
}

const NOFOLLOW: gio::FileQueryInfoFlags = gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS;

fn is_dir(f: &gio::File) -> bool {
    f.query_file_type(NOFOLLOW, gio::Cancellable::NONE) == gio::FileType::Directory
}

fn children(f: &gio::File) -> Result<Vec<gio::File>, glib::Error> {
    let e = f.enumerate_children("standard::name", NOFOLLOW, gio::Cancellable::NONE)?;
    let mut out = Vec::new();
    while let Some(info) = e.next_file(gio::Cancellable::NONE)? {
        out.push(f.child(info.name()));
    }
    Ok(out)
}

fn du(f: &gio::File) -> u64 {
    let Ok(info) = f.query_info("standard::type,standard::size", NOFOLLOW, gio::Cancellable::NONE) else {
        return 0;
    };
    if info.file_type() == gio::FileType::Directory {
        children(f).map(|c| c.iter().map(du).sum()).unwrap_or(0)
    } else {
        info.size() as u64
    }
}

/// `dir/name`, or `dir/name (2).ext`, `(3)`… if taken. Never overwrites.
pub fn unique_child(dir: &gio::File, name: &str) -> gio::File {
    let first = dir.child(name);
    if !first.query_exists(gio::Cancellable::NONE) {
        return first;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(p) if p > 0 => (&name[..p], &name[p..]),
        _ => (name, ""),
    };
    (2..)
        .map(|n| dir.child(format!("{stem} ({n}){ext}")))
        .find(|f| !f.query_exists(gio::Cancellable::NONE))
        .unwrap()
}

fn copy_tree(src: &gio::File, dst: &gio::File, p: &Progress) -> Result<(), glib::Error> {
    if is_dir(src) {
        dst.make_directory(gio::Cancellable::NONE)?;
        for c in children(src)? {
            copy_tree(&c, &dst.child(c.basename().unwrap()), p)?;
        }
        Ok(())
    } else {
        let base = p.done.load(Ordering::Relaxed);
        src.copy(
            dst,
            gio::FileCopyFlags::NOFOLLOW_SYMLINKS | gio::FileCopyFlags::ALL_METADATA,
            gio::Cancellable::NONE,
            Some(&mut |cur, _| p.done.store(base + cur as u64, Ordering::Relaxed)),
        )
    }
}

pub fn delete_tree(f: &gio::File) -> Result<(), glib::Error> {
    if is_dir(f) {
        for c in children(f)? {
            delete_tree(&c)?;
        }
    }
    f.delete(gio::Cancellable::NONE)
}

/// Copy or move `srcs` into `dest`. Returns (src, dst) pairs actually done.
fn transfer(op: Op, srcs: &[gio::File], dest: &gio::File, policy: Conflict, p: &Progress) -> Result<Transferred, String> {
    p.total.store(srcs.iter().map(du).sum(), Ordering::Relaxed);
    let mut done = Vec::new();
    let mut replaced = Vec::new();
    for src in srcs {
        let name = src.basename().unwrap_or_default().to_string_lossy().into_owned();
        if dest.equal(src) || dest.has_prefix(src) {
            return Err(format!("Can't put \u{201c}{name}\u{201d} inside itself"));
        }
        if op == Op::Move && src.parent().is_some_and(|d| d.equal(dest)) {
            continue;
        }
        let target = dest.child(&name);
        // copying into its own folder always makes "name (2)", like Nautilus
        let same_dir = src.parent().is_some_and(|d| d.equal(dest));
        let dst = if !target.query_exists(gio::Cancellable::NONE) {
            target
        } else if same_dir {
            unique_child(dest, &name)
        } else {
            match policy {
                Conflict::KeepBoth => unique_child(dest, &name),
                Conflict::Skip => continue,
                Conflict::Replace => {
                    target.trash(gio::Cancellable::NONE).map_err(|e| format!("{name}: {}", e.message()))?;
                    replaced.extend(target.path());
                    target
                }
            }
        };
        let r = match op {
            Op::Copy => copy_tree(src, &dst, p),
            Op::Move => match src.move_(
                &dst,
                gio::FileCopyFlags::NOFOLLOW_SYMLINKS | gio::FileCopyFlags::ALL_METADATA,
                gio::Cancellable::NONE,
                None,
            ) {
                // directory across filesystems
                Err(e) if e.matches(gio::IOErrorEnum::WouldRecurse) => copy_tree(src, &dst, p).and_then(|_| delete_tree(src)),
                r => r,
            },
        };
        r.map_err(|e| format!("{name}: {}", e.message()))?;
        done.push((src.clone(), dst));
    }
    Ok(Transferred { pairs: done, replaced })
}

/// The home trash (freedesktop spec): files/ holds the items, info/NAME.trashinfo where each came from.
pub fn home_trash() -> PathBuf {
    glib::user_data_dir().join("Trash")
}

/// Is this a file sitting in the home trash's files/ folder?
pub fn in_home_trash(f: &gio::File) -> bool {
    f.path().is_some_and(|p| p.parent() == Some(&home_trash().join("files")))
}

fn trashinfo(item: &Path) -> PathBuf {
    home_trash()
        .join("info")
        .join(format!("{}.trashinfo", item.file_name().unwrap_or_default().to_string_lossy()))
}

/// Where a trashed item came from, from its .trashinfo (percent-encoded Path=).
pub fn trash_origin(item: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(trashinfo(item)).ok()?;
    let raw = text.lines().find_map(|l| l.strip_prefix("Path="))?.trim();
    Some(PathBuf::from(glib::Uri::unescape_string(raw, None::<&str>)?.as_str()))
}

/// Put trashed items back where they came from (as "name (2)" if that spot is taken now).
fn restore_items(items: &[PathBuf]) -> Result<usize, String> {
    let mut n = 0;
    for item in items {
        let orig = trash_origin(item).ok_or_else(|| format!("{}: no record of where it came from", item.display()))?;
        let dir = orig.parent().ok_or("bad original path")?;
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let name = orig.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let dest = unique_child(&gio::File::for_path(dir), &name);
        gio::File::for_path(item)
            .move_(
                &dest,
                gio::FileCopyFlags::NOFOLLOW_SYMLINKS | gio::FileCopyFlags::ALL_METADATA,
                gio::Cancellable::NONE,
                None,
            )
            .map_err(|e| format!("{name}: {}", e.message()))?;
        let _ = std::fs::remove_file(trashinfo(item));
        n += 1;
    }
    Ok(n)
}

fn same_fs(a: &gio::File, b: &gio::File) -> bool {
    let id = |f: &gio::File| {
        f.query_info("id::filesystem", gio::FileQueryInfoFlags::NONE, gio::Cancellable::NONE)
            .ok()
            .and_then(|i| i.attribute_string("id::filesystem"))
    };
    id(a).is_some() && id(a) == id(b)
}

/// What a drop does: Shift forces move, Ctrl copy; otherwise move within a filesystem, copy across.
pub fn drop_op(actions: gdk::DragAction, files: &[gio::File], dest: &gio::File) -> Op {
    if actions == gdk::DragAction::MOVE {
        Op::Move
    } else if actions == gdk::DragAction::COPY {
        Op::Copy
    } else if files.first().is_some_and(|f| same_fs(f, dest)) {
        Op::Move
    } else {
        Op::Copy
    }
}

/// Names in `srcs` that already exist in `dest` (excluding copies into their own folder).
pub fn conflicts(srcs: &[gio::File], dest: &gio::File) -> Vec<String> {
    srcs.iter()
        .filter(|s| !s.parent().is_some_and(|d| d.equal(dest)))
        .filter_map(|s| s.basename())
        .filter(|n| dest.child(n).query_exists(gio::Cancellable::NONE))
        .map(|n| n.to_string_lossy().into_owned())
        .collect()
}

/// Restore from the home trash directly (freedesktop trash spec: files/NAME + info/NAME.trashinfo).
/// Going through trash:/// from a worker thread hit a 60 s D-Bus timeout on every undo.
/// Returns the paths it couldn't find there (trashed from another drive).
fn restore_from_home_trash(origs: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    let trash = glib::user_data_dir().join("Trash");
    let mut newest: std::collections::HashMap<&PathBuf, (String, String)> = Default::default();
    for e in std::fs::read_dir(trash.join("info")).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(stem) = name.strip_suffix(".trashinfo") else { continue };
        let Ok(text) = std::fs::read_to_string(e.path()) else { continue };
        let field = |k: &str| text.lines().find_map(|l| l.strip_prefix(k)).map(str::trim).map(String::from);
        let (Some(path), date) = (field("Path="), field("DeletionDate=").unwrap_or_default()) else {
            continue;
        };
        let Some(path) = glib::Uri::unescape_string(&path, None::<&str>) else {
            continue;
        };
        if let Some(orig) = origs.iter().find(|o| o.as_os_str() == path.as_str())
            && newest.get(orig).is_none_or(|(d, _)| date > *d)
        {
            newest.insert(orig, (date, stem.to_string()));
        }
    }
    let mut rest = Vec::new();
    for orig in origs {
        let Some((_, stem)) = newest.get(orig) else {
            rest.push(orig.clone());
            continue;
        };
        if orig.symlink_metadata().is_ok() {
            return Err(format!("{} already exists", orig.display()));
        }
        std::fs::rename(trash.join("files").join(stem), orig).map_err(|e| format!("{}: {e}", orig.display()))?;
        let _ = std::fs::remove_file(trash.join("info").join(format!("{stem}.trashinfo")));
    }
    Ok(rest)
}

/// Put back what we trashed. The trash can hold several items from the same original path
/// (trashed at different times); take the most recently deleted one for each.
fn restore_from_trash(origs: &[PathBuf]) -> Result<(), String> {
    let origs = restore_from_home_trash(origs)?;
    if origs.is_empty() {
        return Ok(());
    }
    let origs = origs.as_slice();
    let trash = gio::File::for_uri("trash:///");
    let e = trash
        .enumerate_children("standard::name,trash::orig-path,trash::deletion-date", NOFOLLOW, gio::Cancellable::NONE)
        .map_err(|e| e.to_string())?;
    // orig path -> (deletion date, name in trash); ISO-8601 dates sort as strings
    let mut newest: std::collections::HashMap<PathBuf, (String, PathBuf)> = Default::default();
    while let Ok(Some(info)) = e.next_file(gio::Cancellable::NONE) {
        let Some(orig) = info.attribute_byte_string("trash::orig-path") else {
            continue;
        };
        let orig = PathBuf::from(orig.as_str());
        if !origs.contains(&orig) {
            continue;
        }
        let date = info.attribute_string("trash::deletion-date").map(|d| d.to_string()).unwrap_or_default();
        if newest.get(&orig).is_none_or(|(d, _)| date > *d) {
            newest.insert(orig, (date, info.name()));
        }
    }
    let mut missing = Vec::new();
    for orig in origs {
        let Some((_, name)) = newest.get(orig) else {
            missing.push(orig.display().to_string());
            continue;
        };
        let dest = gio::File::for_path(orig);
        if dest.query_exists(gio::Cancellable::NONE) {
            return Err(format!("{} already exists", orig.display()));
        }
        trash
            .child(name)
            .move_(&dest, gio::FileCopyFlags::NOFOLLOW_SYMLINKS, gio::Cancellable::NONE, None)
            .map_err(|e| e.to_string())?;
    }
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!("not in the trash any more: {}", missing.join(", ")))
    }
}

fn run_undo(u: Undo) -> Result<String, String> {
    let err = |e: glib::Error| e.message().to_string();
    match u {
        Undo::Moved(pairs) => {
            for (src, dst) in pairs.iter().rev() {
                dst.move_(
                    src,
                    gio::FileCopyFlags::NOFOLLOW_SYMLINKS | gio::FileCopyFlags::ALL_METADATA,
                    gio::Cancellable::NONE,
                    None,
                )
                .map_err(err)?;
            }
            Ok("Move undone".into())
        }
        // the originals still exist, so copies are deleted outright; trashing them would also
        // shadow a replaced original with the same path in the trash
        Undo::Copied(dsts) => {
            for d in dsts {
                delete_tree(&d).map_err(err)?;
            }
            Ok("Copy undone".into())
        }
        Undo::Created(f) => {
            f.trash(gio::Cancellable::NONE).map_err(err)?;
            Ok("Folder moved to trash".into())
        }
        Undo::Renamed(old, new) => {
            new.move_(&old, gio::FileCopyFlags::NOFOLLOW_SYMLINKS, gio::Cancellable::NONE, None)
                .map_err(err)?;
            Ok("Rename undone".into())
        }
        Undo::Trashed(origs) => restore_from_trash(&origs).map(|_| "Restored from trash".into()),
        Undo::Many(steps) => {
            let mut last = String::new();
            for u in steps {
                last = run_undo(u)?;
            }
            Ok(last)
        }
    }
}

pub struct Ops {
    pub toasts: adw::ToastOverlay,
    /// Shown under the header while a job runs: the progress bar, plus Cancel when the job can stop.
    pub bar: gtk::Box,
    progress: gtk::ProgressBar,
    cancel: gtk::Button,
    undo: RefCell<Vec<Undo>>,
}

/// Every extension Kestrel offers Extract on: what the bundled 7-Zip reads (`7zz i`) that is
/// really an archive, package or disk image. Documents and programs 7-Zip can also open (.docx,
/// .exe, .dll, ELF) are left out so Extract isn't offered on every Word file. Compound
/// extensions (.tar.gz) must come before their tail (.gz): the first match names the output folder.
/// Keep in sync with docs/extract-formats.md.
pub const ARCHIVE_EXTS: &[&str] = &[
    // tarballs (system tar, sandboxed)
    ".tar.gz",
    ".tar.xz",
    ".tar.zst",
    ".tar.bz2",
    ".tar.z",
    ".tgz",
    ".tpz",
    ".txz",
    ".tzst",
    ".tbz2",
    ".tbz",
    ".taz",
    ".tar",
    ".ova",
    // archives
    ".7z",
    ".zip",
    ".zipx",
    ".rar",
    ".cab",
    ".arj",
    ".lzh",
    ".lha",
    ".cpio",
    ".xar",
    ".wim",
    ".swm",
    ".esd",
    ".chm",
    ".z01",
    ".001",
    ".r00",
    // single-file compression
    ".gz",
    ".gzip",
    ".xz",
    ".bz2",
    ".bzip2",
    ".zst",
    ".lzma",
    ".z",
    // books and comics (zip inside)
    ".epub",
    ".cbz",
    // packages and installers
    ".deb",
    ".udeb",
    ".rpm",
    ".msi",
    ".msp",
    ".apk",
    ".jar",
    ".war",
    ".whl",
    ".xpi",
    ".crx",
    ".ipa",
    ".appx",
    ".pkg",
    // disk and file-system images
    ".iso",
    ".img",
    ".dmg",
    ".udf",
    ".vhd",
    ".vhdx",
    ".vmdk",
    ".vdi",
    ".qcow",
    ".qcow2",
    ".squashfs",
    ".cramfs",
    ".hfs",
    ".hfsx",
    ".simg",
];

/// The `ARCHIVE_EXTS` entry `name` ends with, if any.
pub fn archive_ext(name: &str) -> Option<&'static str> {
    let lower = name.to_lowercase();
    ARCHIVE_EXTS.iter().copied().find(|e| lower.ends_with(e))
}

fn is_tarball(lower: &str) -> bool {
    [
        ".tar", ".tar.gz", ".tgz", ".tpz", ".tar.xz", ".txz", ".tar.zst", ".tzst", ".tar.bz2", ".tbz2", ".tbz", ".tar.z", ".taz", ".ova",
    ]
    .iter()
    .any(|e| lower.ends_with(e))
}

/// Run a command; on failure return its last stderr line.
fn run(mut cmd: std::process::Command) -> Result<(), String> {
    let out = cmd.stdin(std::process::Stdio::null()).output().map_err(|e| e.to_string())?;
    check(out)
}

fn check(out: std::process::Output) -> Result<(), String> {
    if out.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("failed").trim().to_string())
    }
}

/// The 7-Zip bundled in the .deb. Ubuntu's 7zip package lags months behind on decoder CVEs and
/// is built without RAR. `KESTREL_7ZZ` points a dev build at packaging/7zip/7zz.
fn seven_zip() -> String {
    std::env::var("KESTREL_7ZZ").unwrap_or_else(|_| "/usr/lib/kestrel/7zz".into())
}

/// `prog` in a bubblewrap sandbox: no network, a read-only system, home/tmp/run/removable
/// media hidden, only `archive` (read-only) and `out` visible. 7-Zip fixes several
/// code-execution bugs in its decoders a year; a hostile archive can at worst write junk into
/// `out`, and no entry can land outside it (the kernel enforces what a zip-slip check would).
pub(crate) fn sandboxed(prog: &str, archive: &Path, out: &Path) -> std::process::Command {
    let mut c = std::process::Command::new("bwrap");
    c.args(["--ro-bind", "/", "/", "--dev", "/dev", "--proc", "/proc"]);
    for hide in ["/home", "/root", "/tmp", "/var/tmp", "/run", "/media", "/mnt", "/srv"] {
        if Path::new(hide).is_dir() {
            c.args(["--tmpfs", hide]);
        }
    }
    let p = Path::new(prog);
    if p.is_absolute() {
        c.arg("--ro-bind").arg(p).arg(p);
    }
    // every volume of a split archive, or 7-Zip sees only the first part
    for v in volumes(archive) {
        c.arg("--ro-bind").arg(&v).arg(&v);
    }
    c.arg("--bind").arg(out).arg(out);
    c.args(["--unshare-all", "--die-with-parent", "--new-session", "--", prog]);
    c
}

/// A split archive's name without its volume suffix ("v.7z" for v.7z.003, "x" for x.part2.rar,
/// x.r00, x.z01, x.zip), or None if `name` doesn't look like a volume.
fn volume_stem(name: &str) -> Option<&str> {
    let (stem, ext) = name.rsplit_once('.')?;
    let ext = ext.to_ascii_lowercase();
    let numbered = |p: &str| ext.strip_prefix(p).is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
    if ext == "rar" {
        // x.part2.rar → x
        return Some(match stem.rsplit_once('.') {
            Some((s, part)) if part.len() > 4 && part[..4].eq_ignore_ascii_case("part") && part[4..].bytes().all(|b| b.is_ascii_digit()) => s,
            _ => stem,
        });
    }
    (numbered("") || numbered("z") || numbered("r") || ext == "zip" || ext == "zipx").then_some(stem)
}

/// `archive` plus the other volumes next to it (same stem, any volume suffix). Just `archive`
/// for an ordinary file.
pub fn volumes(archive: &Path) -> Vec<PathBuf> {
    let mut all = vec![archive.to_path_buf()];
    let (Some(name), Some(dir)) = (archive.file_name().and_then(|n| n.to_str()), archive.parent()) else {
        return all;
    };
    let Some(stem) = volume_stem(name) else { return all };
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let n = e.file_name();
            let Some(n) = n.to_str() else { continue };
            if n != name && n.len() > stem.len() && n.starts_with(stem) && n.as_bytes()[stem.len()] == b'.' && volume_stem(n) == Some(stem) {
                all.push(dir.join(n));
            }
        }
    }
    all
}

/// The volume 7-Zip must be pointed at: old-style RAR sets (x.rar, x.r00, x.r01…) start at x.rar.
pub fn first_volume(archive: &Path) -> PathBuf {
    let name = archive.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    if let Some((stem, ext)) = name.rsplit_once('.')
        && ext.len() > 1
        && ext.as_bytes()[0].eq_ignore_ascii_case(&b'r')
        && ext[1..].bytes().all(|b| b.is_ascii_digit())
    {
        let first = archive.with_file_name(format!("{stem}.rar"));
        if first.exists() {
            return first;
        }
    }
    archive.to_path_buf()
}

/// One entry of an archive listing.
pub struct Entry {
    pub path: String,
    pub size: u64,
    pub dir: bool,
}

/// What's inside `archive` (canonical path), listed in the sandbox; `scratch` is the empty
/// writable dir the sandbox needs. Tarballs go through tar (7-Zip would show the one .tar inside).
pub fn list_archive(archive: &Path, scratch: &Path) -> Result<Vec<Entry>, String> {
    let archive = &first_volume(archive);
    let name = archive.file_name().unwrap_or_default().to_string_lossy().to_lowercase();
    let tar = is_tarball(&name);
    let mut c = if tar {
        let mut c = sandboxed("tar", archive, scratch);
        c.arg("-tvf").arg(archive);
        c
    } else {
        let mut c = sandboxed(&seven_zip(), archive, scratch);
        c.args(["l", "-slt", "-p", "--"]).arg(archive);
        c
    };
    let out = c.stdin(std::process::Stdio::null()).output().map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() {
        let msg = format!("{text}{}", String::from_utf8_lossy(&out.stderr)).to_lowercase();
        return Err(if msg.contains("encrypted") || msg.contains("password") {
            "Encrypted archive".into()
        } else {
            "Couldn't read this archive".into()
        });
    }
    Ok(if tar { parse_tar_tv(&text) } else { parse_7z_slt(&text) })
}

/// `tar -tv` lines: "drwxr-xr-x user/group 0 2026-10-26 08:25 d/sub/".
fn parse_tar_tv(text: &str) -> Vec<Entry> {
    text.lines()
        .filter_map(|l| {
            let mut rest = l;
            let mut tok = Vec::new();
            for _ in 0..5 {
                rest = rest.trim_start();
                let end = rest.find(char::is_whitespace)?;
                tok.push(&rest[..end]);
                rest = &rest[end..];
            }
            let path = rest.trim_start().split(" -> ").next()?.trim_end_matches('/');
            Some(Entry {
                path: path.into(),
                size: tok[2].parse().unwrap_or(0),
                dir: tok[0].starts_with('d'),
            })
        })
        .filter(|e| !e.path.is_empty())
        .collect()
}

/// `7zz l -slt` blocks ("Path = …", "Folder = +", "Size = …") after the "----------" line.
fn parse_7z_slt(text: &str) -> Vec<Entry> {
    let Some((_, body)) = text.split_once("\n----------\n") else {
        return Vec::new();
    };
    body.split("\n\n")
        .filter_map(|block| {
            let field = |k: &str| block.lines().find_map(|l| l.strip_prefix(k)?.strip_prefix(" = "));
            Some(Entry {
                path: field("Path")?.to_string(),
                size: field("Size").and_then(|s| s.parse().ok()).unwrap_or(0),
                dir: field("Folder") == Some("+") || field("Attributes").is_some_and(|a| a.starts_with('D')),
            })
        })
        .collect()
}

/// Why extracting failed: a missing/wrong password (ask for one), the Cancel button, or anything else.
enum ExtractErr {
    Password,
    Cancelled,
    Other(String),
}

/// The archive unpacked in a hidden staging folder `tmp`: `src` still has to land in the
/// destination as `name`.
struct Unpacked {
    tmp: PathBuf,
    src: gio::File,
    name: String,
}

/// Extract `archive` into `out` with the sandboxed 7-Zip, feeding its percentage to the
/// progress bar. The password goes over stdin, not argv, so other users can't read it from /proc.
fn run_7z(archive: &Path, out: &Path, pw: Option<&str>, p: &Progress) -> Result<(), ExtractErr> {
    use std::io::{Read, Write};
    use std::process::Stdio;
    let mut cmd = sandboxed(&seven_zip(), archive, out);
    cmd.args(["x", "-y", "-bso0", "-bse1", "-bsp1"]).arg(format!("-o{}", out.display()));
    // -p with no value: a password-protected archive fails at once instead of prompting
    if pw.is_none() {
        cmd.arg("-p");
    }
    // everything after -- is a file name or filter, so switches must come first
    cmd.arg("--").arg(archive).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    let mut child = cmd.spawn().map_err(|e| ExtractErr::Other(e.to_string()))?;
    if let (Some(mut si), Some(pw)) = (child.stdin.take(), pw) {
        let _ = writeln!(si, "{pw}");
    }
    p.total.store(100, Ordering::Relaxed);
    let mut so = child.stdout.take().unwrap();
    let (mut buf, mut msgs) = ([0u8; 4096], String::new());
    let ok = killable(p, child.id(), || {
        while let Ok(n @ 1..) = so.read(&mut buf) {
            // progress redraws in place with backspaces: "  42% 3 - name"
            for seg in String::from_utf8_lossy(&buf[..n]).split(['\x08', '\r']) {
                match seg.trim_start().split_once('%').and_then(|(n, _)| n.parse::<u64>().ok()) {
                    Some(pct) => p.done.store(pct, Ordering::Relaxed),
                    None => msgs.push_str(seg),
                }
            }
        }
        child.wait().is_ok_and(|s| s.success())
    });
    if p.cancel.is_cancelled() {
        Err(ExtractErr::Cancelled)
    } else if ok {
        p.done.store(100, Ordering::Relaxed); // small archives finish before any percentage prints
        Ok(())
    } else if msgs.contains("Wrong password") {
        Err(ExtractErr::Password)
    } else {
        let last = msgs.lines().map(str::trim).filter(|l| !l.is_empty() && *l != "ERRORS:").last();
        Err(ExtractErr::Other(last.unwrap_or("failed").to_string()))
    }
}

fn plural(n: usize, one: &str) -> String {
    if n == 1 { format!("1 {one}") } else { format!("{n} {one}s") }
}

impl Ops {
    pub fn new(toasts: adw::ToastOverlay) -> Rc<Ops> {
        let progress = gtk::ProgressBar::builder()
            .css_classes(["osd"])
            .hexpand(true)
            .valign(gtk::Align::Center)
            .build();
        let cancel = gtk::Button::builder()
            .icon_name("process-stop-symbolic")
            .tooltip_text("Cancel")
            .css_classes(["flat", "circular"])
            .visible(false)
            .build();
        let bar = gtk::Box::builder().spacing(6).visible(false).build();
        bar.append(&progress);
        bar.append(&cancel);
        Rc::new(Ops {
            toasts,
            bar,
            progress,
            cancel,
            undo: RefCell::default(),
        })
    }

    pub fn toast(&self, msg: &str) {
        self.toasts.add_toast(adw::Toast::new(msg));
    }

    fn toast_undo(&self, msg: &str, undo: Undo) {
        self.undo.borrow_mut().push(undo);
        self.toasts
            .add_toast(adw::Toast::builder().title(msg).button_label("Undo").action_name("win.undo").build());
    }

    fn window(&self) -> Option<gtk::Window> {
        self.toasts.root().and_downcast()
    }

    /// Run `work` on a worker thread with a live progress bar, then `then` on the main thread.
    fn job<T: Send + 'static>(self: &Rc<Self>, work: impl FnOnce(&Progress) -> T + Send + 'static, then: impl FnOnce(&Rc<Ops>, T) + 'static) {
        let p = Arc::new(Progress::default());
        let (bar, cancel) = (self.progress.clone(), self.cancel.clone());
        bar.set_fraction(0.0);
        self.bar.set_visible(true);
        let pc = p.clone();
        let clicked = cancel.connect_clicked(move |_| pc.cancel.cancel());
        let pw = p.clone();
        let tick = glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
            let (d, t) = (pw.done.load(Ordering::Relaxed), pw.total.load(Ordering::Relaxed));
            if t > 0 {
                bar.set_fraction(d as f64 / t as f64);
            } else {
                bar.pulse();
            }
            cancel.set_visible(pw.can_cancel.load(Ordering::Relaxed));
            glib::ControlFlow::Continue
        });
        let me = self.clone();
        glib::spawn_future_local(async move {
            let r = gio::spawn_blocking(move || work(&p)).await;
            tick.remove();
            me.cancel.disconnect(clicked);
            me.cancel.set_visible(false);
            me.bar.set_visible(false);
            match r {
                Ok(v) => then(&me, v),
                Err(_) => me.toast("Operation crashed"),
            }
        });
    }

    /// Copy/move, asking once what to do if names already exist at the destination.
    pub fn transfer(self: &Rc<Self>, op: Op, srcs: Vec<gio::File>, dest: gio::File) {
        if srcs.is_empty() {
            return;
        }
        let clash = conflicts(&srcs, &dest);
        if clash.is_empty() {
            self.run_transfer(op, srcs, dest, Conflict::KeepBoth);
            return;
        }
        let (me, to) = (self.clone(), dest.clone());
        self.ask_conflict(&clash, &to, move |policy| {
            if let Some(policy) = policy {
                me.run_transfer(op, srcs.clone(), dest.clone(), policy);
            }
        });
    }

    /// Skip / Keep Both / Replace for names in `clash` that already exist in `dest`; None on Cancel.
    fn ask_conflict(&self, clash: &[String], dest: &gio::File, done: impl Fn(Option<Conflict>) + 'static) {
        let folder = dest.basename().map_or_else(|| dest.uri().to_string(), |b| b.to_string_lossy().into_owned());
        let heading = match clash {
            [one] => format!("\u{201c}{one}\u{201d} already exists"),
            many => format!("{} items already exist", many.len()),
        };
        let mut body = format!("in \u{201c}{folder}\u{201d}.");
        if clash.len() > 1 {
            body += "\n\n";
            body += &clash.iter().take(5).cloned().collect::<Vec<_>>().join("\n");
            if clash.len() > 5 {
                body += &format!("\n…and {} more", clash.len() - 5);
            }
        }
        body += "\n\nReplaced items go to the trash, so Undo can bring them back.";
        let d = adw::AlertDialog::new(Some(&heading), Some(&body));
        d.add_responses(&[("cancel", "Cancel"), ("skip", "Skip"), ("keep", "Keep Both"), ("replace", "Replace")]);
        d.set_response_appearance("replace", adw::ResponseAppearance::Destructive);
        d.set_default_response(Some("keep"));
        d.set_close_response("cancel");
        d.connect_response(None, move |_, r| {
            done(match r {
                "skip" => Some(Conflict::Skip),
                "keep" => Some(Conflict::KeepBoth),
                "replace" => Some(Conflict::Replace),
                _ => None,
            })
        });
        d.present(self.window().as_ref());
    }

    fn run_transfer(self: &Rc<Self>, op: Op, srcs: Vec<gio::File>, dest: gio::File, policy: Conflict) {
        self.job(
            move |p| transfer(op, &srcs, &dest, policy, p),
            move |me, r| match r {
                Ok(t) if t.pairs.is_empty() => me.toast("Nothing to do"),
                Ok(Transferred { pairs, replaced }) => {
                    let mut msg = format!("{} {}", if op == Op::Copy { "Copied" } else { "Moved" }, plural(pairs.len(), "item"));
                    if !replaced.is_empty() {
                        msg += &format!(", replaced {}", replaced.len());
                    }
                    let main = if op == Op::Copy {
                        Undo::Copied(pairs.into_iter().map(|(_, d)| d).collect())
                    } else {
                        Undo::Moved(pairs)
                    };
                    let undo = if replaced.is_empty() {
                        main
                    } else {
                        Undo::Many(vec![main, Undo::Trashed(replaced)])
                    };
                    me.toast_undo(&msg, undo);
                }
                Err(e) => me.toast(&e),
            },
        );
    }

    pub fn trash(self: &Rc<Self>, files: Vec<gio::File>) {
        if files.is_empty() {
            return;
        }
        self.job(
            move |_| {
                let mut done = Vec::new();
                for f in &files {
                    match f.trash(gio::Cancellable::NONE) {
                        Ok(()) => done.extend(f.path()),
                        Err(e) => return Err((done, e.message().to_string())),
                    }
                }
                Ok(done)
            },
            |me, r| match r {
                Ok(done) => me.toast_undo(&format!("{} moved to trash", plural(done.len(), "item")), Undo::Trashed(done)),
                Err((done, e)) => {
                    if !done.is_empty() {
                        me.undo.borrow_mut().push(Undo::Trashed(done));
                    }
                    me.toast(&format!("Couldn't trash: {e}"));
                }
            },
        );
    }

    /// Permanent delete, after confirmation.
    pub fn delete(self: &Rc<Self>, files: Vec<gio::File>) {
        if files.is_empty() {
            return;
        }
        let what = if files.len() == 1 {
            format!("\u{201c}{}\u{201d}", files[0].basename().unwrap_or_default().display())
        } else {
            plural(files.len(), "item")
        };
        let d = adw::AlertDialog::new(Some(&format!("Permanently delete {what}?")), Some("This can't be undone."));
        d.add_responses(&[("cancel", "Cancel"), ("delete", "Delete")]);
        d.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        d.set_default_response(Some("cancel"));
        let me = self.clone();
        d.connect_response(None, move |_, r| {
            if r != "delete" {
                return;
            }
            let files = files.clone();
            me.job(
                move |_| {
                    for f in &files {
                        delete_tree(f).map_err(|e| e.message().to_string())?;
                        // deleting from the trash: drop its .trashinfo too
                        if in_home_trash(f)
                            && let Some(p) = f.path()
                        {
                            let _ = std::fs::remove_file(trashinfo(&p));
                        }
                    }
                    Ok::<_, String>(files.len())
                },
                |me, r| match r {
                    Ok(n) => me.toast(&format!("Deleted {}", plural(n, "item"))),
                    Err(e) => me.toast(&format!("Couldn't delete: {e}")),
                },
            );
        });
        d.present(self.window().as_ref());
    }

    pub fn restore(self: &Rc<Self>, files: Vec<gio::File>) {
        let items: Vec<PathBuf> = files.iter().filter(|f| in_home_trash(f)).filter_map(|f| f.path()).collect();
        if items.is_empty() {
            return;
        }
        self.job(
            move |_| restore_items(&items),
            |me, r| match r {
                Ok(n) => me.toast(&format!("Restored {}", plural(n, "item"))),
                Err(e) => me.toast(&format!("Couldn't restore: {e}")),
            },
        );
    }

    pub fn empty_trash(self: &Rc<Self>) {
        let d = adw::AlertDialog::new(
            Some("Empty the trash?"),
            Some("Everything in the trash is deleted for good. This can't be undone."),
        );
        d.add_responses(&[("cancel", "Cancel"), ("empty", "Empty Trash")]);
        d.set_response_appearance("empty", adw::ResponseAppearance::Destructive);
        d.set_default_response(Some("cancel"));
        let me = self.clone();
        d.connect_response(None, move |_, r| {
            if r != "empty" {
                return;
            }
            me.job(
                |_| {
                    let mut n = 0;
                    for sub in ["files", "info", "expunged"] {
                        for e in std::fs::read_dir(home_trash().join(sub)).into_iter().flatten().flatten() {
                            let p = e.path();
                            let r = if p.is_dir() && !p.is_symlink() {
                                std::fs::remove_dir_all(&p)
                            } else {
                                std::fs::remove_file(&p)
                            };
                            r.map_err(|err| format!("{}: {err}", p.display()))?;
                            n += usize::from(sub == "files");
                        }
                    }
                    Ok::<_, String>(n)
                },
                |me, r| match r {
                    Ok(n) => me.toast(&format!("Trash emptied ({})", plural(n, "item"))),
                    Err(e) => me.toast(&format!("Couldn't empty the trash: {e}")),
                },
            );
        });
        d.present(self.window().as_ref());
    }

    /// Ask for a name, pre-selecting it up to the extension like Nautilus does.
    pub fn ask_name(&self, title: &str, ok: &str, initial: &str, done: impl Fn(String) + 'static) {
        let d = adw::AlertDialog::new(Some(title), None);
        let entry = gtk::Entry::builder().text(initial).activates_default(true).build();
        d.set_extra_child(Some(&entry));
        d.add_responses(&[("cancel", "Cancel"), ("ok", ok)]);
        d.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
        d.set_default_response(Some("ok"));
        d.set_close_response("cancel");
        let e = entry.clone();
        d.connect_response(None, move |_, r| {
            let name = e.text().trim().to_string();
            if r == "ok" && !name.is_empty() && !name.contains('/') {
                done(name);
            }
        });
        // focusing an entry selects everything; once the dialog has focused it, select just the stem
        let stem = initial.rfind('.').filter(|&p| p > 0).unwrap_or(initial.len());
        let n = initial[..stem].chars().count() as i32;
        let fc = gtk::EventControllerFocus::new();
        let done = std::cell::Cell::new(false);
        fc.connect_enter(move |fc| {
            if done.replace(true) {
                return;
            }
            if let Some(e) = fc.widget().and_downcast::<gtk::Entry>() {
                glib::idle_add_local_once(move || e.select_region(0, n));
            }
        });
        entry.add_controller(fc);
        d.present(self.window().as_ref());
        // AdwAlertDialog focuses its default button; we want to type straight away
        d.set_focus(Some(&entry));
    }

    pub fn rename(self: &Rc<Self>, file: gio::File) {
        let old = file.basename().unwrap_or_default().to_string_lossy().into_owned();
        let me = self.clone();
        let initial = old.clone();
        self.ask_name("Rename", "Rename", &initial, move |name| {
            if name != old {
                me.rename_to(&file, &name);
            }
        });
    }

    pub fn rename_to(&self, file: &gio::File, name: &str) {
        if file.parent().is_some_and(|d| d.child(name).query_exists(gio::Cancellable::NONE)) {
            self.toast(&format!("\u{201c}{name}\u{201d} already exists"));
            return;
        }
        match file.set_display_name(name, gio::Cancellable::NONE) {
            Ok(new) => self.undo.borrow_mut().push(Undo::Renamed(file.clone(), new)),
            Err(e) => self.toast(e.message()),
        }
    }

    pub fn new_folder(self: &Rc<Self>, dir: gio::File) {
        let me = self.clone();
        self.ask_name("New Folder", "Create", "New Folder", move |name| {
            let f = dir.child(&name);
            match f.make_directory(gio::Cancellable::NONE) {
                Ok(()) => me.undo.borrow_mut().push(Undo::Created(f)),
                Err(e) => me.toast(e.message()),
            }
        });
    }

    /// Zip/7z via 7z, tarballs via tar (format from the name's extension).
    pub fn compress(self: &Rc<Self>, files: Vec<gio::File>, dir: gio::File, name: String) {
        let names: Option<Vec<String>> = files.iter().map(|f| f.basename().map(|b| b.to_string_lossy().into_owned())).collect();
        let (Some(names), Some(cwd)) = (names, dir.path()) else {
            self.toast("Compressing needs local files");
            return;
        };
        let dest = unique_child(&dir, &name);
        let Some(dpath) = dest.path() else { return };
        let lower = name.to_lowercase();
        let mut cmd = if lower.ends_with(".zip") || lower.ends_with(".7z") {
            let mut c = std::process::Command::new(seven_zip());
            c.arg("a").arg("-bd").arg("--").arg(&dpath).args(&names);
            c
        } else if is_tarball(&lower) {
            let mut c = std::process::Command::new("tar");
            c.arg("-caf").arg(&dpath).arg("--").args(&names);
            c
        } else {
            self.toast("Use .zip, .7z, .tar.gz, .tar.xz or .tar.zst");
            return;
        };
        cmd.current_dir(cwd);
        self.job(
            move |_| run(cmd).map(|_| dest),
            |me, r| match r {
                Ok(dest) => me.toast_undo(&format!("Created {}", dest.basename().unwrap_or_default().display()), Undo::Created(dest)),
                Err(e) => me.toast(&format!("Compress failed: {e}")),
            },
        );
    }

    /// Extract into `into` (None: next to the archive). A single top-level entry lands directly;
    /// otherwise a folder named after the archive holds everything (no spilling 500 files into Downloads).
    pub fn extract(self: &Rc<Self>, file: gio::File, into: Option<gio::File>) {
        self.extract_pw(file, None, into);
    }

    fn extract_pw(self: &Rc<Self>, file: gio::File, pw: Option<String>, into: Option<gio::File>) {
        let (Some(path), Some(dir)) = (
            file.path().and_then(|p| p.canonicalize().ok()).map(|p| first_volume(&p)),
            into.clone().or_else(|| file.parent()),
        ) else {
            self.toast("Extracting needs a local file");
            return;
        };
        if dir.path().is_none() {
            self.toast("Extracting needs a local folder");
            return;
        }
        let name = file.basename().unwrap_or_default().to_string_lossy().into_owned();
        let lower = name.to_lowercase();
        let stem_len = archive_ext(&name).map_or(name.len(), |e| name.len() - e.len());
        let tmp = unique_child(&dir, &format!(".{}.kestrel-extract", &name[..stem_len]));
        let final_name = name[..stem_len].to_string();
        let had_pw = pw.is_some();
        let retry = file.clone();
        self.job(
            move |p| -> Result<Unpacked, ExtractErr> {
                let other = |e: std::io::Error| ExtractErr::Other(e.to_string());
                let tpath = tmp.path().ok_or(ExtractErr::Other("no path".into()))?;
                std::fs::create_dir(&tpath).map_err(other)?;
                let tpath = tpath.canonicalize().map_err(other)?;
                let r = if is_tarball(&lower) {
                    let mut c = sandboxed("tar", &path, &tpath);
                    c.arg("-xf").arg(&path).arg("-C").arg(&tpath);
                    c.stdin(std::process::Stdio::null())
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::piped());
                    match c.spawn() {
                        Ok(child) => match killable(p, child.id(), || child.wait_with_output()) {
                            _ if p.cancel.is_cancelled() => Err(ExtractErr::Cancelled),
                            Ok(out) => check(out).map_err(ExtractErr::Other),
                            Err(e) => Err(other(e)),
                        },
                        Err(e) => Err(other(e)),
                    }
                } else {
                    run_7z(&path, &tpath, pw.as_deref(), p)
                };
                if let Err(e) = r {
                    let _ = std::fs::remove_dir_all(&tpath);
                    return Err(e);
                }
                let entries: Vec<_> = std::fs::read_dir(&tpath).map_err(other)?.flatten().collect();
                let (src, name) = match entries.as_slice() {
                    [one] => (gio::File::for_path(one.path()), one.file_name().to_string_lossy().into_owned()),
                    _ => (tmp.clone(), final_name),
                };
                Ok(Unpacked { tmp: tpath, src, name })
            },
            move |me, r| match r {
                Ok(u) if !dir.child(&u.name).query_exists(gio::Cancellable::NONE) => me.land(u, dir, Conflict::KeepBoth),
                Ok(u) => {
                    let (m, d) = (me.clone(), dir.clone());
                    let clash = [u.name.clone()];
                    let u = RefCell::new(Some(u));
                    me.ask_conflict(&clash, &dir, move |policy| {
                        if let Some(u) = u.take() {
                            m.land(u, d.clone(), policy.unwrap_or(Conflict::Skip));
                        }
                    });
                }
                Err(ExtractErr::Password) => {
                    let title = if had_pw { "Wrong Password" } else { "Password Required" };
                    let m = me.clone();
                    me.ask_password(title, &name, move |pw| m.extract_pw(retry.clone(), Some(pw), into.clone()));
                }
                Err(ExtractErr::Cancelled) => me.toast("Extract cancelled"),
                Err(ExtractErr::Other(e)) => me.toast(&format!("Extract failed: {e}")),
            },
        );
    }

    /// Move an unpacked archive from its staging folder into `dir`, resolving a name clash
    /// with `policy` (Skip throws the unpacked copy away).
    fn land(self: &Rc<Self>, u: Unpacked, dir: gio::File, policy: Conflict) {
        self.job(
            move |_| -> Result<Option<(gio::File, Option<PathBuf>)>, String> {
                let target = dir.child(&u.name);
                let mut replaced = None;
                let dest = if !target.query_exists(gio::Cancellable::NONE) {
                    target
                } else {
                    match policy {
                        Conflict::KeepBoth => unique_child(&dir, &u.name),
                        Conflict::Skip => {
                            let _ = std::fs::remove_dir_all(&u.tmp);
                            return Ok(None);
                        }
                        Conflict::Replace => {
                            target.trash(gio::Cancellable::NONE).map_err(|e| format!("{}: {}", u.name, e.message()))?;
                            replaced = target.path();
                            target
                        }
                    }
                };
                let r = u.src.move_(&dest, gio::FileCopyFlags::NOFOLLOW_SYMLINKS, gio::Cancellable::NONE, None);
                // the staging folder is gone if it was moved itself; otherwise it holds nothing we want
                let _ = std::fs::remove_dir_all(&u.tmp);
                r.map_err(|e| e.message().to_string())?;
                Ok(Some((dest, replaced)))
            },
            |me, r| match r {
                Ok(None) => me.toast("Nothing extracted"),
                Ok(Some((dest, replaced))) => {
                    let mut msg = format!("Extracted {}", dest.basename().unwrap_or_default().display());
                    let undo = match replaced {
                        Some(old) => {
                            msg += ", replaced 1";
                            Undo::Many(vec![Undo::Created(dest), Undo::Trashed(vec![old])])
                        }
                        None => Undo::Created(dest),
                    };
                    me.toast_undo(&msg, undo);
                }
                Err(e) => me.toast(&format!("Extract failed: {e}")),
            },
        );
    }

    fn ask_password(&self, title: &str, archive: &str, done: impl Fn(String) + 'static) {
        let d = adw::AlertDialog::new(Some(title), Some(&format!("\u{201c}{archive}\u{201d} is encrypted")));
        let entry = gtk::PasswordEntry::builder().show_peek_icon(true).activates_default(true).build();
        d.set_extra_child(Some(&entry));
        d.add_responses(&[("cancel", "Cancel"), ("ok", "Extract")]);
        d.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
        d.set_default_response(Some("ok"));
        d.set_close_response("cancel");
        let e = entry.clone();
        d.connect_response(None, move |_, r| {
            if r == "ok" {
                done(e.text().to_string());
            }
        });
        d.present(self.window().as_ref());
        d.set_focus(Some(&entry));
    }

    pub fn undo(self: &Rc<Self>) {
        let Some(u) = self.undo.borrow_mut().pop() else {
            self.toast("Nothing to undo");
            return;
        };
        // gio::File is thread-safe; the closure only moves Files/PathBufs
        self.job(move |_| run_undo(u), |me, r| me.toast(&r.unwrap_or_else(|e| format!("Undo failed: {e}"))));
    }

    /// Clipboard in the formats Nautilus reads and writes, so copy/paste works between them.
    pub fn set_clipboard(&self, files: &[gio::File], cut: bool) {
        let Some(display) = gdk::Display::default() else { return };
        let uris: Vec<String> = files.iter().map(|f| f.uri().to_string()).collect();
        let gnome = format!("{}\n{}", if cut { "cut" } else { "copy" }, uris.join("\n"));
        let list = gdk::FileList::from_array(files);
        let text = files
            .iter()
            .map(|f| f.path().map_or_else(|| f.uri().to_string(), |p| p.display().to_string()))
            .collect::<Vec<_>>()
            .join("\n");
        let provider = gdk::ContentProvider::new_union(&[
            gdk::ContentProvider::for_bytes("x-special/gnome-copied-files", &glib::Bytes::from_owned(gnome.into_bytes())),
            gdk::ContentProvider::for_value(&list.to_value()),
            gdk::ContentProvider::for_bytes("text/plain;charset=utf-8", &glib::Bytes::from_owned(text.into_bytes())),
        ]);
        let _ = display.clipboard().set_content(Some(&provider));
        self.toast(&format!("{} {}", plural(files.len(), "item"), if cut { "cut" } else { "copied" }));
    }

    pub fn paste(self: &Rc<Self>, dest: gio::File) {
        let Some(display) = gdk::Display::default() else { return };
        let cb = display.clipboard();
        let me = self.clone();
        glib::spawn_future_local(async move {
            let (stream, _) = match cb
                .read_future(&["x-special/gnome-copied-files", "text/uri-list"], glib::Priority::DEFAULT)
                .await
            {
                Ok(r) => r,
                Err(_) => {
                    me.toast("Nothing to paste");
                    return;
                }
            };
            let mut bytes = Vec::new();
            loop {
                match stream.read_bytes_future(65536, glib::Priority::DEFAULT).await {
                    Ok(b) if !b.is_empty() => bytes.extend_from_slice(&b),
                    _ => break,
                }
            }
            let text = String::from_utf8_lossy(&bytes);
            let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).peekable();
            let op = match lines.peek() {
                Some(&"cut") => {
                    lines.next();
                    Op::Move
                }
                Some(&"copy") => {
                    lines.next();
                    Op::Copy
                }
                _ => Op::Copy,
            };
            let files: Vec<gio::File> = lines.map(gio::File::for_uri).collect();
            me.transfer(op, files, dest);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> gio::File {
        let d = std::env::temp_dir().join(format!("kestrel-ops-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("src/sub")).unwrap();
        std::fs::create_dir_all(d.join("dst")).unwrap();
        std::fs::write(d.join("src/a.txt"), "aaaa").unwrap();
        std::fs::write(d.join("src/sub/b.txt"), "bb").unwrap();
        gio::File::for_path(d)
    }

    fn exists(f: &gio::File) -> bool {
        f.query_exists(gio::Cancellable::NONE)
    }

    #[test]
    fn copy_never_overwrites_and_undo_trashes() {
        let d = tmp("copy");
        let p = Progress::default();
        let (src, dst) = (d.child("src"), d.child("dst"));
        let pairs = transfer(Op::Copy, &[src.child("a.txt"), src.child("sub")], &dst, Conflict::KeepBoth, &p)
            .unwrap()
            .pairs;
        assert_eq!(pairs.len(), 2);
        assert!(exists(&dst.child("sub/b.txt")), "recursive copy");
        assert_eq!(p.total.load(Ordering::Relaxed), 6);
        let again = transfer(Op::Copy, &[src.child("a.txt")], &dst, Conflict::KeepBoth, &p).unwrap().pairs;
        assert_eq!(again[0].1.basename().unwrap().to_str(), Some("a (2).txt"), "second copy gets a new name");
        assert_eq!(std::fs::read_to_string(dst.child("a.txt").path().unwrap()).unwrap(), "aaaa");
        delete_tree(&d).unwrap();
        assert!(!exists(&d));
    }

    #[test]
    fn move_and_undo() {
        let d = tmp("move");
        let p = Progress::default();
        let (src, dst) = (d.child("src"), d.child("dst"));
        let pairs = transfer(Op::Move, &[src.child("sub")], &dst, Conflict::KeepBoth, &p).unwrap().pairs;
        assert!(exists(&dst.child("sub/b.txt")) && !exists(&src.child("sub")));
        assert!(
            transfer(Op::Move, &[dst.child("sub")], &dst, Conflict::KeepBoth, &p).unwrap().pairs.is_empty(),
            "move into own parent is a no-op"
        );
        let err = transfer(Op::Move, &[dst.clone()], &dst.child("sub"), Conflict::KeepBoth, &p).err().unwrap();
        assert!(err.contains("inside itself"), "{err}");
        run_undo(Undo::Moved(pairs)).unwrap();
        assert!(exists(&src.child("sub/b.txt")) && !exists(&dst.child("sub")), "undo moves it back");

        let renamed = src.child("a.txt").set_display_name("z.txt", gio::Cancellable::NONE).unwrap();
        run_undo(Undo::Renamed(src.child("a.txt"), renamed)).unwrap();
        assert!(exists(&src.child("a.txt")));
        delete_tree(&d).unwrap();
    }

    #[test]
    fn trash_then_restore() {
        // uses the real trash (tmpfs has none), under ~/.cache, cleaned up after
        let d = glib::user_cache_dir().join(format!("kestrel-trash-test-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("t.txt");
        std::fs::write(&p, "x").unwrap();
        let f = gio::File::for_path(&p);
        f.trash(gio::Cancellable::NONE).unwrap();
        assert!(!p.exists());
        let t = std::time::Instant::now();
        let r = restore_from_trash(std::slice::from_ref(&p));
        eprintln!("restore took {:?}: {r:?}", t.elapsed());
        assert!(r.is_ok(), "{r:?}");
        assert!(p.exists());
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn conflict_policies() {
        // under ~/.cache: Replace trashes, and tmpfs has no trash
        let base = glib::user_cache_dir().join(format!("kestrel-conflict-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("src")).unwrap();
        std::fs::create_dir_all(base.join("dst")).unwrap();
        std::fs::write(base.join("src/a.txt"), "new").unwrap();
        std::fs::write(base.join("dst/a.txt"), "old").unwrap();
        let (src, dst) = (gio::File::for_path(base.join("src")), gio::File::for_path(base.join("dst")));
        let a = [src.child("a.txt")];
        let p = Progress::default();
        assert_eq!(conflicts(&a, &dst), ["a.txt"]);
        assert!(conflicts(&a, &src).is_empty(), "same folder is never a conflict");

        assert!(transfer(Op::Copy, &a, &dst, Conflict::Skip, &p).unwrap().pairs.is_empty());
        assert_eq!(std::fs::read_to_string(base.join("dst/a.txt")).unwrap(), "old");

        let t = transfer(Op::Copy, &a, &dst, Conflict::KeepBoth, &p).unwrap();
        assert_eq!(t.pairs[0].1.basename().unwrap().to_str(), Some("a (2).txt"));

        let t = transfer(Op::Copy, &a, &dst, Conflict::Replace, &p).unwrap();
        assert_eq!(std::fs::read_to_string(base.join("dst/a.txt")).unwrap(), "new");
        assert_eq!(t.replaced, [base.join("dst/a.txt")]);
        // undo: copy removed, the replaced original back from the trash
        run_undo(Undo::Many(vec![
            Undo::Copied(t.pairs.into_iter().map(|(_, d)| d).collect()),
            Undo::Trashed(t.replaced),
        ]))
        .unwrap();
        assert_eq!(std::fs::read_to_string(base.join("dst/a.txt")).unwrap(), "old");
        assert_eq!(std::fs::read_to_string(base.join("src/a.txt")).unwrap(), "new", "source untouched");
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn trash_view_restore() {
        // the trash view's Restore: read the .trashinfo, move the item back, drop the info file
        let d = glib::user_cache_dir().join(format!("kestrel-rtest-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let orig = d.join("put-me-back.txt");
        std::fs::write(&orig, "x").unwrap();
        gio::File::for_path(&orig).trash(gio::Cancellable::NONE).unwrap();
        let item = std::fs::read_dir(home_trash().join("files"))
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .find(|p| trash_origin(p).as_deref() == Some(orig.as_path()))
            .expect("trashed item has a .trashinfo pointing back");
        assert!(in_home_trash(&gio::File::for_path(&item)));
        assert_eq!(restore_items(&[item.clone()]).unwrap(), 1);
        assert!(orig.exists(), "back where it was");
        assert!(!item.exists() && !trashinfo(&item).exists(), "trash entry and info both gone");
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn unique_names() {
        let d = tmp("uniq");
        let src = d.child("src");
        assert_eq!(unique_child(&src, "new.txt").basename().unwrap().to_str(), Some("new.txt"));
        assert_eq!(unique_child(&src, "a.txt").basename().unwrap().to_str(), Some("a (2).txt"));
        assert_eq!(unique_child(&src, "sub").basename().unwrap().to_str(), Some("sub (2)"));
        delete_tree(&d).unwrap();
    }

    /// Sandboxed bundled 7-Zip: extracts, asks for a password, and can't write outside `out`.
    /// Needs KESTREL_7ZZ=packaging/7zip/7zz (scripts/update-7zip.sh fetches it); skipped otherwise.
    #[test]
    fn archive_exts_compound_first() {
        // a later entry ending in an earlier one could never match (".tar.gz" after ".gz")
        for (i, a) in ARCHIVE_EXTS.iter().enumerate() {
            for b in &ARCHIVE_EXTS[i + 1..] {
                assert!(!b.ends_with(a), "{b} is shadowed by {a}");
            }
        }
        assert_eq!(archive_ext("Photos.TAR.GZ"), Some(".tar.gz"));
        assert_eq!(archive_ext("disk.iso"), Some(".iso"));
        assert_eq!(archive_ext("report.docx"), None);
    }

    #[test]
    fn volume_sets() {
        let d = std::env::temp_dir().join(format!("kestrel-vol-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        for n in [
            "a.7z.001",
            "a.7z.002",
            "a.7z",
            "b.part1.rar",
            "b.part2.rar",
            "b.txt",
            "c.rar",
            "c.r00",
            "c.r01",
            "s.zip",
            "s.z01",
            "s.zip.txt",
            "solo.zip",
        ] {
            std::fs::write(d.join(n), "").unwrap();
        }
        let set = |n: &str| {
            let mut v: Vec<String> = volumes(&d.join(n))
                .iter()
                .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
                .collect();
            v.sort();
            v
        };
        assert_eq!(set("a.7z.002"), ["a.7z.001", "a.7z.002"]);
        assert_eq!(set("b.part1.rar"), ["b.part1.rar", "b.part2.rar"]);
        assert_eq!(set("c.r00"), ["c.r00", "c.r01", "c.rar"]);
        assert_eq!(set("s.zip"), ["s.z01", "s.zip"]);
        assert_eq!(set("solo.zip"), ["solo.zip"]);
        assert_eq!(set("b.txt"), ["b.txt"]);
        assert_eq!(first_volume(&d.join("c.r01")), d.join("c.rar"));
        assert_eq!(first_volume(&d.join("a.7z.002")), d.join("a.7z.002"));
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn sandboxed_7z() {
        let Ok(z) = std::env::var("KESTREL_7ZZ") else { return };
        let d = std::env::temp_dir().join(format!("kestrel-7z-{}", std::process::id()));
        let out = d.join("out");
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(d.join("a.txt"), "hi").unwrap();
        let mk = |args: &[&str]| assert!(std::process::Command::new(&z).current_dir(&d).args(args).output().unwrap().status.success());
        mk(&["a", "plain.zip", "a.txt"]);
        mk(&["a", "-psecret", "-mhe", "enc.7z", "a.txt"]);
        let p = Progress::default();
        let x = |arc: &str, pw: Option<&str>| run_7z(&d.join(arc), &out, pw, &p);
        assert!(x("plain.zip", None).is_ok());
        assert_eq!(std::fs::read_to_string(out.join("a.txt")).unwrap(), "hi");
        assert!(matches!(x("enc.7z", None), Err(ExtractErr::Password)));
        assert!(matches!(x("enc.7z", Some("nope")), Err(ExtractErr::Password)));
        assert!(x("enc.7z", Some("secret")).is_ok());
        assert_eq!(p.done.load(Ordering::Relaxed), 100);
        // Cancel kills the sandbox (a cancel before it starts kills it at once)
        let c = Progress::default();
        c.cancel.cancel();
        assert!(matches!(run_7z(&d.join("plain.zip"), &out, None, &c), Err(ExtractErr::Cancelled)));
        assert!(!c.can_cancel.load(Ordering::Relaxed));
        // listings, in the sandbox
        std::fs::create_dir(d.join("sub")).unwrap();
        std::fs::write(d.join("sub/b.txt"), "yo!").unwrap();
        mk(&["a", "tree.zip", "sub"]);
        assert!(
            std::process::Command::new("tar")
                .current_dir(&d)
                .args(["-czf", "tree.tar.gz", "sub"])
                .status()
                .unwrap()
                .success()
        );
        for arc in ["tree.zip", "tree.tar.gz"] {
            let l = list_archive(&d.join(arc), &out).unwrap();
            assert_eq!(l.len(), 2, "{arc}");
            assert!(l.iter().any(|e| e.path == "sub" && e.dir), "{arc}");
            assert!(l.iter().any(|e| e.path == "sub/b.txt" && !e.dir && e.size == 3), "{arc}");
        }
        assert_eq!(list_archive(&d.join("enc.7z"), &out).err().as_deref(), Some("Encrypted archive"));
        // split archives: the sandbox must see every part (incompressible data, so it really splits)
        let mut big = vec![0u8; 3 << 20];
        let mut x: u64 = 1;
        for b in big.iter_mut() {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            *b = x as u8;
        }
        std::fs::write(d.join("sub/big.bin"), &big).unwrap();
        mk(&["a", "-v1m", "vol.7z", "sub"]);
        assert!(d.join("vol.7z.004").exists());
        let mut zips = vec!["vol.7z.001"];
        if std::process::Command::new("zip")
            .current_dir(&d)
            .args(["-q", "-r", "-s", "1m", "sz.zip", "sub"])
            .status()
            .is_ok_and(|s| s.success())
        {
            zips.extend(["sz.zip", "sz.z01"]);
        }
        for arc in zips {
            let o = d.join(format!("out-{arc}"));
            std::fs::create_dir(&o).unwrap();
            assert!(run_7z(&d.join(arc), &o, None, &p).is_ok(), "{arc}");
            assert_eq!(std::fs::read(o.join("sub/big.bin")).unwrap(), big, "{arc}");
        }
        // the sandbox itself: writes outside `out` are refused, home is hidden
        let mut c = sandboxed("/bin/sh", &d.join("plain.zip"), &out);
        c.args(["-c", &format!("echo x > {}/escaped; ls $HOME", d.display())]);
        assert!(!c.output().unwrap().status.success());
        assert!(!d.join("escaped").exists());
        std::fs::remove_dir_all(&d).unwrap();
    }
}
