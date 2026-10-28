//! File operations through GIO, so local disks, SFTP/SMB/FTP, MTP and trash:// all behave the same.
//! Heavy work runs on a worker thread; the UI gets a progress bar, a toast and an Undo.

use crate::archive::*;
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

/// Outcome of a transfer: (src, dst) pairs done, paths of items replaced (now in the trash),
/// and "name: why" for each item that failed (the rest still went through).
pub struct Transferred {
    pub pairs: Vec<(gio::File, gio::File)>,
    pub replaced: Vec<PathBuf>,
    pub failed: Vec<String>,
}

pub(crate) struct Progress {
    pub(crate) done: AtomicU64,
    pub(crate) total: AtomicU64,
    /// Set by work that can stop midway (extracting): shows the Cancel button.
    pub(crate) can_cancel: AtomicBool,
    pub(crate) cancel: gio::Cancellable,
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

/// Copy or move `srcs` into `dest`. An item that fails (permissions, a locked file, a full
/// disk) is noted and skipped; the others still go through.
fn transfer(op: Op, srcs: &[gio::File], dest: &gio::File, policy: Conflict, p: &Progress) -> Transferred {
    p.total.store(srcs.iter().map(du).sum(), Ordering::Relaxed);
    let mut done = Vec::new();
    let mut replaced = Vec::new();
    let mut failed = Vec::new();
    for src in srcs {
        let name = src.basename().unwrap_or_default().to_string_lossy().into_owned();
        if dest.equal(src) || dest.has_prefix(src) {
            failed.push(format!("{name}: can't go inside itself"));
            continue;
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
                    if let Err(e) = target.trash(gio::Cancellable::NONE) {
                        failed.push(format!(
                            "{name}: couldn't move the existing one to the trash ({})",
                            crate::browser::human_gio(&e)
                        ));
                        continue;
                    }
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
        match r {
            Ok(()) => done.push((src.clone(), dst)),
            Err(e) => failed.push(format!("{name}: {}", crate::browser::human_gio(&e))),
        }
    }
    Transferred { pairs: done, replaced, failed }
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
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {}", dir.display(), crate::browser::human_io(&e)))?;
        let name = orig.file_name().unwrap_or_default().to_string_lossy().into_owned();
        let dest = unique_child(&gio::File::for_path(dir), &name);
        gio::File::for_path(item)
            .move_(
                &dest,
                gio::FileCopyFlags::NOFOLLOW_SYMLINKS | gio::FileCopyFlags::ALL_METADATA,
                gio::Cancellable::NONE,
                None,
            )
            .map_err(|e| format!("{name}: {}", crate::browser::human_gio(&e)))?;
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

/// What the person asked for when dropping: Shift held = move, Ctrl = copy, neither = let
/// drop_op decide. Read from the keys, not from the source's offered actions: a drag from the
/// desktop (or another app) offers only "copy", which made every such drop a copy.
pub fn drop_intent(t: &gtk::DropTarget) -> gdk::DragAction {
    let mods = t.current_event_state();
    if mods.contains(gdk::ModifierType::SHIFT_MASK) {
        gdk::DragAction::MOVE
    } else if mods.contains(gdk::ModifierType::CONTROL_MASK) {
        gdk::DragAction::COPY
    } else {
        gdk::DragAction::empty()
    }
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
        std::fs::rename(trash.join("files").join(stem), orig).map_err(|e| format!("{}: {}", orig.display(), crate::browser::human_io(&e)))?;
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
        .map_err(|e| crate::browser::human_gio(&e))?;
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
            .map_err(|e| crate::browser::human_gio(&e))?;
    }
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!("not in the trash any more: {}", missing.join(", ")))
    }
}

fn run_undo(u: Undo) -> Result<String, String> {
    let err = |e: glib::Error| crate::browser::human_gio(&e);
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
            Ok("Moved to trash".into())
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
    title: gtk::Label,
    progress: gtk::ProgressBar,
    /// "42% · about 2 min left"
    detail: gtk::Label,
    cancel: gtk::Button,
    undo: RefCell<Vec<Undo>>,
}

/// The archive unpacked in a hidden staging folder `tmp`: `src` still has to land in the
/// destination as `name`.
struct Unpacked {
    tmp: PathBuf,
    /// `name` already exists in the destination (checked on the worker)
    clash: bool,
    /// unsafe links 7-Zip refused to create
    skipped: usize,
    src: gio::File,
    name: String,
}

/// "42%", then "42% · about 2 min left" once there's enough history to guess from.
fn progress_text(f: f64, elapsed: std::time::Duration) -> String {
    let pct = format!("{:.0}%", f * 100.0);
    if f < 0.02 || f >= 1.0 || elapsed.as_secs() < 3 {
        return pct;
    }
    let left = (elapsed.as_secs_f64() * (1.0 - f) / f) as u64;
    let left = match left {
        0..10 => "a few seconds left".to_string(),
        10..60 => format!("{} s left", left / 5 * 5),
        60..5400 => format!("about {} min left", left.div_ceil(60)),
        _ => format!("about {} h left", (left as f64 / 3600.0).round()),
    };
    format!("{pct} · {left}")
}

fn plural(n: usize, one: &str) -> String {
    if n == 1 { format!("1 {one}") } else { format!("{n} {one}s") }
}

impl Ops {
    pub fn new(toasts: adw::ToastOverlay) -> Rc<Ops> {
        let progress = gtk::ProgressBar::builder().hexpand(true).valign(gtk::Align::Center).build();
        let title = gtk::Label::builder().ellipsize(gtk::pango::EllipsizeMode::Middle).max_width_chars(40).build();
        let detail = gtk::Label::builder().css_classes(["dim-label", "numeric"]).build();
        let cancel = gtk::Button::builder()
            .icon_name("process-stop-symbolic")
            .tooltip_text("Cancel")
            .css_classes(["flat", "circular"])
            .visible(false)
            .build();
        let bar = gtk::Box::builder()
            .spacing(12)
            .margin_start(12)
            .margin_end(6)
            .margin_top(4)
            .margin_bottom(4)
            .visible(false)
            .build();
        bar.append(&title);
        bar.append(&progress);
        bar.append(&detail);
        bar.append(&cancel);
        Rc::new(Ops {
            toasts,
            bar,
            title,
            progress,
            detail,
            cancel,
            undo: RefCell::default(),
        })
    }

    pub fn toast(&self, msg: &str) {
        self.toasts.add_toast(adw::Toast::new(msg));
    }

    pub(crate) fn toast_undo(&self, msg: &str, undo: Undo) {
        self.undo.borrow_mut().push(undo);
        self.toasts
            .add_toast(adw::Toast::builder().title(msg).button_label("Undo").action_name("win.undo").build());
    }

    pub(crate) fn window(&self) -> Option<gtk::Window> {
        self.toasts.root().and_downcast()
    }

    /// Run `work` on a worker thread with a live progress bar, then `then` on the main thread.
    pub(crate) fn job<T: Send + 'static>(
        self: &Rc<Self>,
        title: String,
        work: impl FnOnce(&Progress) -> T + Send + 'static,
        then: impl FnOnce(&Rc<Ops>, T) + 'static,
    ) {
        let p = Arc::new(Progress::default());
        let (bar, cancel, detail) = (self.progress.clone(), self.cancel.clone(), self.detail.clone());
        bar.set_fraction(0.0);
        detail.set_label("");
        self.title.set_label(&title);
        self.bar.set_visible(true);
        let start = std::time::Instant::now();
        let pc = p.clone();
        let clicked = cancel.connect_clicked(move |_| pc.cancel.cancel());
        let pw = p.clone();
        let tick = glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
            let (d, t) = (pw.done.load(Ordering::Relaxed), pw.total.load(Ordering::Relaxed));
            if t > 0 {
                let f = (d as f64 / t as f64).min(1.0);
                bar.set_fraction(f);
                detail.set_label(&progress_text(f, start.elapsed()));
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

    /// Quick file work (a rename, a new folder, an existence check) on a worker thread, then
    /// `then` back on the GTK thread: a stalled network mount waits there instead of freezing the
    /// window. No progress bar; long work goes through `job`.
    pub(crate) fn bg<T: Send + 'static>(self: &Rc<Self>, work: impl FnOnce() -> T + Send + 'static, then: impl FnOnce(&Rc<Ops>, T) + 'static) {
        let me = self.clone();
        glib::spawn_future_local(async move {
            match gio::spawn_blocking(work).await {
                Ok(v) => then(&me, v),
                Err(_) => me.toast("Operation crashed"),
            }
        });
    }

    /// Files dropped on `dest`: move or copy (see drop_op), decided off the GTK thread.
    pub fn drop_files(self: &Rc<Self>, actions: gdk::DragAction, files: Vec<gio::File>, dest: gio::File) {
        self.bg(
            move || (drop_op(actions, &files, &dest), files, dest),
            |me, (op, files, dest)| me.transfer(op, files, dest),
        );
    }

    /// Copy/move, asking once what to do if names already exist at the destination.
    pub fn transfer(self: &Rc<Self>, op: Op, srcs: Vec<gio::File>, dest: gio::File) {
        if srcs.is_empty() {
            return;
        }
        self.bg(
            move || (conflicts(&srcs, &dest), srcs, dest),
            move |me, (clash, srcs, dest)| {
                if clash.is_empty() {
                    me.run_transfer(op, srcs, dest, Conflict::KeepBoth);
                    return;
                }
                let (m, to) = (me.clone(), dest.clone());
                me.ask_conflict(&clash, &to, move |policy| {
                    if let Some(policy) = policy {
                        m.run_transfer(op, srcs.clone(), dest.clone(), policy);
                    }
                });
            },
        );
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
            format!("{} {}", if op == Op::Copy { "Copying" } else { "Moving" }, plural(srcs.len(), "item")),
            move |p| transfer(op, &srcs, &dest, policy, p),
            move |me, Transferred { pairs, replaced, failed }| {
                if !failed.is_empty() {
                    me.report_failures(if op == Op::Copy { "Copy" } else { "Move" }, pairs.len(), &failed);
                }
                if pairs.is_empty() {
                    if failed.is_empty() {
                        me.toast("Nothing to do");
                    }
                } else {
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
            },
        );
    }

    /// One dialog listing every item that failed, after the rest went through.
    fn report_failures(&self, what: &str, done: usize, failed: &[String]) {
        let heading = format!(
            "{} couldn\u{2019}t be {}",
            plural(failed.len(), "item"),
            if what == "Copy" { "copied" } else { "moved" }
        );
        let body = if done > 0 {
            format!("The other {} went through.", plural(done, "item"))
        } else {
            String::new()
        };
        let d = adw::AlertDialog::new(Some(&heading), Some(&body));
        let tv = gtk::TextView::builder()
            .editable(false)
            .wrap_mode(gtk::WrapMode::WordChar)
            .top_margin(6)
            .bottom_margin(6)
            .left_margin(6)
            .right_margin(6)
            .css_classes(["card"])
            .build();
        tv.buffer().set_text(&failed.join("\n"));
        d.set_extra_child(Some(
            &gtk::ScrolledWindow::builder()
                .child(&tv)
                .min_content_height(60)
                .max_content_height(240)
                .propagate_natural_height(true)
                .build(),
        ));
        d.add_response("ok", "OK");
        d.present(self.window().as_ref());
    }

    pub fn trash(self: &Rc<Self>, files: Vec<gio::File>) {
        if files.is_empty() {
            return;
        }
        self.job(
            format!("Moving {} to the Trash", plural(files.len(), "item")),
            move |_| {
                let mut done = Vec::new();
                for f in &files {
                    match f.trash(gio::Cancellable::NONE) {
                        Ok(()) => done.extend(f.path()),
                        Err(e) if e.matches(gio::IOErrorEnum::NotSupported) => {
                            return Err((done, "this drive has no trash; use Delete (Shift+Delete) instead".into()));
                        }
                        Err(e) => return Err((done, crate::browser::human_gio(&e))),
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
                format!("Deleting {}", plural(files.len(), "item")),
                move |_| {
                    for f in &files {
                        delete_tree(f).map_err(|e| crate::browser::human_gio(&e))?;
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
            format!("Restoring {}", plural(items.len(), "item")),
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
                "Emptying the Trash".to_string(),
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
                            r.map_err(|e| format!("{}: {}", p.display(), crate::browser::human_io(&e)))?;
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

    pub fn rename_to(self: &Rc<Self>, file: &gio::File, name: &str) {
        let (file, name) = (file.clone(), name.to_string());
        self.bg(
            move || {
                if file.parent().is_some_and(|d| d.child(&name).query_exists(gio::Cancellable::NONE)) {
                    return Err(format!("\u{201c}{name}\u{201d} already exists"));
                }
                file.set_display_name(&name, gio::Cancellable::NONE)
                    .map(|new| (file, new))
                    .map_err(|e| crate::browser::human_gio(&e))
            },
            |me, r| match r {
                Ok((old, new)) => me.undo.borrow_mut().push(Undo::Renamed(old, new)),
                Err(e) => me.toast(&e),
            },
        );
    }

    pub fn new_folder(self: &Rc<Self>, dir: gio::File) {
        let me = self.clone();
        self.ask_name("New Folder", "Create", "New Folder", move |name| {
            let f = dir.child(&name);
            me.bg(
                move || f.make_directory(gio::Cancellable::NONE).map(|_| f).map_err(|e| crate::browser::human_gio(&e)),
                |me, r| match r {
                    Ok(f) => me.undo.borrow_mut().push(Undo::Created(f)),
                    Err(e) => me.toast(&e),
                },
            );
        });
    }

    pub fn new_file(self: &Rc<Self>, dir: gio::File) {
        let me = self.clone();
        self.ask_name("New File", "Create", "New Document.txt", move |name| {
            let f = dir.child(&name);
            me.bg(
                move || f.create(gio::FileCreateFlags::NONE, gio::Cancellable::NONE).map(|_| f).map_err(|e| crate::browser::human_gio(&e)),
                |me, r| match r {
                    Ok(f) => me.undo.borrow_mut().push(Undo::Created(f)),
                    Err(e) => me.toast(&e),
                },
            );
        });
    }

    /// Symlinks to `srcs` in `dest` (Explorer's "Paste shortcut"); a taken name gets " (2)".
    pub fn link(self: &Rc<Self>, srcs: Vec<gio::File>, dest: gio::File) {
        self.bg(
            move || {
                let (mut made, mut failed) = (Vec::new(), Vec::new());
                for s in &srcs {
                    let (Some(name), Some(target)) = (s.basename(), s.path()) else { continue };
                    let l = unique_child(&dest, &name.to_string_lossy());
                    match l.make_symbolic_link(&target, gio::Cancellable::NONE) {
                        Ok(()) => made.push(Undo::Created(l)),
                        Err(e) => failed.push(crate::browser::human_gio(&e)),
                    }
                }
                (made, failed)
            },
            |me, (made, failed)| {
                if let Some(e) = failed.first() {
                    me.toast(e);
                } else {
                    me.toast(&format!("{} linked", plural(made.len(), "item")));
                }
                if !made.is_empty() {
                    me.undo.borrow_mut().push(Undo::Many(made));
                }
            },
        );
    }

    /// Zip/7z via 7z, tarballs via tar (format from the name's extension).
    pub fn compress(self: &Rc<Self>, files: Vec<gio::File>, dir: gio::File, name: String) {
        let names: Option<Vec<String>> = files.iter().map(|f| f.basename().map(|b| b.to_string_lossy().into_owned())).collect();
        let (Some(names), Some(cwd)) = (names, dir.path()) else {
            self.toast("Compressing needs local files");
            return;
        };
        let lower = name.to_lowercase();
        let zip = lower.ends_with(".zip") || lower.ends_with(".7z");
        if !zip && !is_tarball(&lower) {
            self.toast("Use .zip, .7z, .tar.gz, .tar.xz or .tar.zst");
            return;
        }
        let title = format!("Compressing {name}");
        self.job(
            title,
            move |_| {
                // picking a free name touches the folder: done here, off the GTK thread
                let dest = unique_child(&dir, &name);
                let dpath = dest.path().ok_or("not a local folder")?;
                let mut cmd = if zip {
                    let mut c = std::process::Command::new(seven_zip());
                    c.arg("a").arg("-bd").arg("--").arg(&dpath).args(&names);
                    c
                } else {
                    let mut c = std::process::Command::new("tar");
                    c.arg("-caf").arg(&dpath).arg("--").args(&names);
                    c
                };
                cmd.current_dir(cwd);
                run(cmd).map(|_| dest)
            },
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
        let stem_len = archive_ext(&name).map_or(name.len(), |e| name.len() - e.len());
        let tmp_name = format!(".{}.kestrel-extract", &name[..stem_len]);
        let final_name = name[..stem_len].to_string();
        let had_pw = pw.is_some();
        let dir2 = dir.clone();
        let retry = file.clone();
        self.job(
            format!("Extracting {name}"),
            move |p| -> Result<Unpacked, ExtractErr> {
                let other = |e: std::io::Error| ExtractErr::Other(crate::browser::human_io(&e));
                let tmp = unique_child(&dir2, &tmp_name);
                let tpath = tmp.path().ok_or(ExtractErr::Other("no path".into()))?;
                std::fs::create_dir(&tpath).map_err(other)?;
                let tpath = tpath.canonicalize().map_err(other)?;
                let skipped = match unpack(&path, &tpath, pw.as_deref(), p) {
                    Ok(n) => n,
                    Err(e) => {
                        let _ = std::fs::remove_dir_all(&tpath);
                        return Err(e);
                    }
                };
                let entries: Vec<_> = std::fs::read_dir(&tpath).map_err(other)?.flatten().collect();
                let (src, name) = match entries.as_slice() {
                    [one] => (gio::File::for_path(one.path()), one.file_name().to_string_lossy().into_owned()),
                    _ => (tmp.clone(), final_name),
                };
                let clash = dir2.child(&name).query_exists(gio::Cancellable::NONE);
                Ok(Unpacked {
                    tmp: tpath,
                    clash,
                    skipped,
                    src,
                    name,
                })
            },
            move |me, r| match r {
                Ok(u) if !u.clash => me.land(u, dir, Conflict::KeepBoth),
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
        let skipped = u.skipped;
        self.job(
            format!("Moving {} into place", u.name),
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
                            target
                                .trash(gio::Cancellable::NONE)
                                .map_err(|e| format!("{}: {}", u.name, crate::browser::human_gio(&e)))?;
                            replaced = target.path();
                            target
                        }
                    }
                };
                let r = u.src.move_(&dest, gio::FileCopyFlags::NOFOLLOW_SYMLINKS, gio::Cancellable::NONE, None);
                // the staging folder is gone if it was moved itself; otherwise it holds nothing we want
                let _ = std::fs::remove_dir_all(&u.tmp);
                r.map_err(|e| crate::browser::human_gio(&e))?;
                Ok(Some((dest, replaced)))
            },
            move |me, r| match r {
                Ok(None) => me.toast("Nothing extracted"),
                Ok(Some((dest, replaced))) => {
                    let mut msg = format!("Extracted {}", dest.basename().unwrap_or_default().display());
                    if skipped > 0 {
                        msg += &format!(", skipped {}", plural(skipped, "unsafe link"));
                    }
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
        self.job(
            "Undoing".to_string(),
            move |_| run_undo(u),
            |me, r| me.toast(&r.unwrap_or_else(|e| format!("Undo failed: {e}"))),
        );
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

    /// Paste the clipboard's files into `dest`; `link` makes symlinks to them instead.
    pub fn paste(self: &Rc<Self>, dest: gio::File, link: bool) {
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
            if link {
                me.link(files, dest);
            } else {
                me.transfer(op, files, dest);
            }
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

    #[test]
    fn drops_move_on_the_same_drive_unless_ctrl() {
        let d = tmp("dropop");
        let files = [d.child("src/a.txt")];
        let dest = d.child("dst");
        // no keys held: the source's offered actions don't matter, the drive does
        assert_eq!(drop_op(gdk::DragAction::empty(), &files, &dest), Op::Move);
        assert_eq!(drop_op(gdk::DragAction::COPY, &files, &dest), Op::Copy); // Ctrl
        assert_eq!(drop_op(gdk::DragAction::MOVE, &files, &dest), Op::Move); // Shift
        let _ = std::fs::remove_dir_all(d.path().unwrap());
    }

    fn exists(f: &gio::File) -> bool {
        f.query_exists(gio::Cancellable::NONE)
    }

    #[test]
    fn copy_never_overwrites_and_undo_trashes() {
        let d = tmp("copy");
        let p = Progress::default();
        let (src, dst) = (d.child("src"), d.child("dst"));
        let pairs = transfer(Op::Copy, &[src.child("a.txt"), src.child("sub")], &dst, Conflict::KeepBoth, &p).pairs;
        assert_eq!(pairs.len(), 2);
        assert!(exists(&dst.child("sub/b.txt")), "recursive copy");
        assert_eq!(p.total.load(Ordering::Relaxed), 6);
        let again = transfer(Op::Copy, &[src.child("a.txt")], &dst, Conflict::KeepBoth, &p).pairs;
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
        let pairs = transfer(Op::Move, &[src.child("sub")], &dst, Conflict::KeepBoth, &p).pairs;
        assert!(exists(&dst.child("sub/b.txt")) && !exists(&src.child("sub")));
        assert!(
            transfer(Op::Move, &[dst.child("sub")], &dst, Conflict::KeepBoth, &p).pairs.is_empty(),
            "move into own parent is a no-op"
        );
        let t = transfer(Op::Move, std::slice::from_ref(&dst), &dst.child("sub"), Conflict::KeepBoth, &p);
        assert!(t.pairs.is_empty() && t.failed[0].contains("inside itself"), "{:?}", t.failed);
        // one bad item doesn't stop the rest
        let t = transfer(Op::Copy, &[src.child("missing"), dst.child("sub/b.txt")], &src, Conflict::KeepBoth, &p);
        assert_eq!((t.pairs.len(), t.failed.len()), (1, 1), "{:?}", t.failed);
        assert!(t.failed[0].starts_with("missing:"));
        delete_tree(&t.pairs[0].1).unwrap();
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

        assert!(transfer(Op::Copy, &a, &dst, Conflict::Skip, &p).pairs.is_empty());
        assert_eq!(std::fs::read_to_string(base.join("dst/a.txt")).unwrap(), "old");

        let t = transfer(Op::Copy, &a, &dst, Conflict::KeepBoth, &p);
        assert_eq!(t.pairs[0].1.basename().unwrap().to_str(), Some("a (2).txt"));

        let t = transfer(Op::Copy, &a, &dst, Conflict::Replace, &p);
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
        assert_eq!(restore_items(std::slice::from_ref(&item)).unwrap(), 1);
        assert!(orig.exists(), "back where it was");
        assert!(!item.exists() && !trashinfo(&item).exists(), "trash entry and info both gone");
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn time_left() {
        assert_eq!(progress_text(0.5, std::time::Duration::from_secs(1)), "50%");
        assert_eq!(progress_text(0.25, std::time::Duration::from_secs(60)), "25% · about 3 min left");
        assert_eq!(progress_text(0.5, std::time::Duration::from_secs(20)), "50% · 20 s left");
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
}
