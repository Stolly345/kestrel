//! Properties dialog.

use crate::browser::{ATTRS, info_file};
use crate::preview::perms;
use adw::prelude::*;
use gtk::{gio, glib};
use std::sync::atomic::{AtomicU64, Ordering};

/// A "Calculate" button on the Size row: totals the folder in the background (a running count while
/// it works, Cancel to stop), for folders the index doesn't cover, such as network shares.
fn add_calculate(row: &adw::ActionRow, folder: gio::File) {
    let b = gtk::Button::builder().label("Calculate").valign(gtk::Align::Center).build();
    row.add_suffix(&b);
    let running: std::rc::Rc<std::cell::RefCell<Option<gio::Cancellable>>> = Default::default();
    let row = row.downgrade();
    b.connect_clicked(move |b| {
        if let Some(c) = running.take() {
            c.cancel();
            return;
        }
        let Some(row) = row.upgrade() else { return };
        let c = gio::Cancellable::new();
        *running.borrow_mut() = Some(c.clone());
        b.set_label("Cancel");
        let counts = std::sync::Arc::new((AtomicU64::new(0), AtomicU64::new(0)));
        let (f, n, c2) = (folder.clone(), counts.clone(), c.clone());
        let work = gio::spawn_blocking(move || walk_size(&f, &n.0, &n.1, &c2));
        let (r2, n2) = (row.downgrade(), counts.clone());
        let tick = glib::timeout_add_local(std::time::Duration::from_millis(200), move || {
            let Some(r) = r2.upgrade() else { return glib::ControlFlow::Break };
            r.set_subtitle(&format!(
                "Counting… {} items, {}",
                n2.0.load(Ordering::Relaxed),
                glib::format_size(n2.1.load(Ordering::Relaxed))
            ));
            glib::ControlFlow::Continue
        });
        let (b, running) = (b.clone(), running.clone());
        glib::spawn_future_local(async move {
            let done = work.await.unwrap_or(false);
            tick.remove();
            running.take();
            let (n, bytes) = (counts.0.load(Ordering::Relaxed), counts.1.load(Ordering::Relaxed));
            if done {
                row.set_subtitle(&format!("{} for {n} items", glib::format_size(bytes)));
                b.set_visible(false);
            } else {
                row.set_subtitle(&format!("Stopped at {n} items, {}", glib::format_size(bytes)));
                b.set_label("Calculate");
            }
        });
    });
}

/// Adds up everything under `root` (apparent sizes, links not followed), counting as it goes.
/// Unreadable folders are skipped. Returns false if cancelled. Blocking.
pub(crate) fn walk_size(root: &gio::File, items: &AtomicU64, bytes: &AtomicU64, c: &gio::Cancellable) -> bool {
    let mut todo = vec![root.clone()];
    while let Some(dir) = todo.pop() {
        let Ok(e) = dir.enumerate_children(
            "standard::name,standard::type,standard::size",
            gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
            Some(c),
        ) else {
            if c.is_cancelled() {
                return false;
            }
            continue;
        };
        while let Ok(Some(i)) = e.next_file(Some(c)) {
            items.fetch_add(1, Ordering::Relaxed);
            bytes.fetch_add(i.size().max(0) as u64, Ordering::Relaxed);
            if i.file_type() == gio::FileType::Directory {
                todo.push(dir.child(i.name()));
            }
        }
        if c.is_cancelled() {
            return false;
        }
    }
    true
}

fn action_row(group: &adw::PreferencesGroup, title: &str, value: &str) -> adw::ActionRow {
    let r = adw::ActionRow::builder()
        .title(title)
        .subtitle(value)
        .subtitle_selectable(true)
        .css_classes(["property"])
        .build();
    group.add(&r);
    r
}

fn when(info: &gio::FileInfo, attr: &str) -> String {
    let secs = info.attribute_uint64(attr);
    if secs == 0 {
        return String::new();
    }
    glib::DateTime::from_unix_local(secs as i64)
        .ok()
        .and_then(|d| d.format("%A %e %B %Y, %H:%M").ok())
        .map(|s| s.to_string())
        .unwrap_or_default()
}

/// MD5, SHA-1 and SHA-256 of the file, read once in 1 MB chunks.
pub fn hashes(path: &std::path::Path, read: &std::sync::atomic::AtomicU64) -> std::io::Result<[String; 3]> {
    use std::io::Read;
    let kinds = [glib::ChecksumType::Md5, glib::ChecksumType::Sha1, glib::ChecksumType::Sha256];
    let mut sums = kinds.map(|k| glib::Checksum::new(k).unwrap());
    let mut f = std::fs::File::open(path)?;
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        sums.iter_mut().for_each(|s| s.update(&buf[..n]));
        read.fetch_add(n as u64, std::sync::atomic::Ordering::Relaxed);
    }
    Ok(sums.map(|s| s.string().unwrap().to_string()))
}

/// Calculated on request (a 3 GB ISO takes a while); paste a published hash to compare.
fn checksums(path: std::path::PathBuf) -> adw::PreferencesGroup {
    use std::sync::atomic::{AtomicU64, Ordering};
    let g = adw::PreferencesGroup::builder()
        .title("Checksums")
        .description("Paste a published checksum to check a download")
        .build();
    let calc = gtk::Button::builder().label("Calculate").valign(gtk::Align::Center).build();
    g.set_header_suffix(Some(&calc));
    let rows: Vec<adw::ActionRow> = ["MD5", "SHA-1", "SHA-256"]
        .into_iter()
        .map(|k| {
            let r = adw::ActionRow::builder()
                .title(k)
                .subtitle_selectable(true)
                .css_classes(["property", "monospace"])
                .visible(false)
                .build();
            g.add(&r);
            r
        })
        .collect();
    let verify = adw::EntryRow::builder().title("Compare with").build();
    let verdict = gtk::Image::new();
    verify.add_suffix(&verdict);
    g.add(&verify);
    let sums: std::rc::Rc<std::cell::RefCell<Option<[String; 3]>>> = Default::default();
    let check = {
        let (sums, verify, verdict) = (sums.clone(), verify.clone(), verdict.clone());
        move || {
            let want = verify.text().trim().to_lowercase();
            let (icon, tip) = match sums.borrow().as_ref() {
                _ if want.is_empty() => ("", ""),
                None => ("", ""),
                Some(s) if s.contains(&want) => ("emblem-ok-symbolic", "Matches"),
                Some(_) => ("dialog-warning-symbolic", "Doesn't match any of these checksums"),
            };
            verdict.set_icon_name(Some(icon).filter(|i| !i.is_empty()));
            verdict.set_tooltip_text(Some(tip));
            verdict.set_css_classes(if icon.starts_with("emblem") { &["success"] } else { &["error"] });
        }
    };
    let run = {
        let (calc, rows, sums, check) = (calc.clone(), rows.clone(), sums.clone(), check.clone());
        std::rc::Rc::new(move || {
            if sums.borrow().is_some() || !calc.is_sensitive() {
                return;
            }
            calc.set_sensitive(false);
            let size = std::fs::metadata(&path).map_or(0, |m| m.len()).max(1);
            let read = std::sync::Arc::new(AtomicU64::new(0));
            let (r2, c2) = (read.clone(), calc.clone());
            let tick = glib::timeout_add_local(std::time::Duration::from_millis(200), move || {
                c2.set_label(&format!("{}%", r2.load(Ordering::Relaxed) * 100 / size));
                glib::ControlFlow::Continue
            });
            let (path, calc, rows, sums, check) = (path.clone(), calc.clone(), rows.clone(), sums.clone(), check.clone());
            glib::spawn_future_local(async move {
                // ponytail: keeps reading if the dialog closes mid-way; add a cancel flag if that bites
                let r = gio::spawn_blocking(move || hashes(&path, &read)).await;
                tick.remove();
                match r {
                    Ok(Ok(h)) => {
                        for (row, v) in rows.iter().zip(&h) {
                            row.set_subtitle(v);
                            row.set_visible(true);
                        }
                        *sums.borrow_mut() = Some(h);
                        calc.set_visible(false);
                        check();
                    }
                    Ok(Err(e)) => calc.set_label(&crate::browser::human_io(&e)),
                    Err(_) => calc.set_label("Failed"),
                }
            });
        })
    };
    let r = run.clone();
    calc.connect_clicked(move |_| r());
    verify.connect_changed(move |e| {
        if !e.text().trim().is_empty() {
            run();
        }
        check();
    });
    g
}

/// Open Properties. One item is re-queried for the times/owner/permissions the listing didn't
/// ask for, off the GTK thread (a stalled network mount must not freeze the window).
pub fn show(parent: &impl IsA<gtk::Widget>, infos: Vec<gio::FileInfo>, toast: impl Fn(&str) + 'static) {
    let parent = parent.as_ref().clone();
    let [one] = infos.as_slice() else {
        build(&parent, infos, toast);
        return;
    };
    let Some(f) = info_file(one) else { return };
    let one = one.clone();
    glib::spawn_future_local(async move {
        let info = f
            .query_info_future(
                &format!("{ATTRS},time::*,owner::*,unix::*,access::*"),
                gio::FileQueryInfoFlags::NONE,
                glib::Priority::DEFAULT,
            )
            .await
            .unwrap_or(one);
        info.set_attribute_object("standard::file", &f);
        build(&parent, vec![info], toast);
    });
}

fn build(parent: &gtk::Widget, infos: Vec<gio::FileInfo>, toast: impl Fn(&str) + 'static) {
    let page = adw::PreferencesPage::new();
    let g = adw::PreferencesGroup::new();
    page.add(&g);
    let title;
    match infos.as_slice() {
        [one] => {
            let Some(f) = info_file(one) else { return };
            let info = one.clone();
            title = info.display_name().to_string();
            let ct = crate::browser::content_type(&info).map(|c| c.to_string()).unwrap_or_default();
            action_row(&g, "Name", &title);
            action_row(&g, "Type", &format!("{} ({ct})", gio::content_type_get_description(&ct)));
            action_row(
                &g,
                "Location",
                &f.parent()
                    .map(|p| p.path().map_or_else(|| p.uri().to_string(), |x| x.display().to_string()))
                    .unwrap_or_default(),
            );
            let size_row = action_row(&g, "Size", &glib::format_size(info.size() as u64));
            if info.file_type() == gio::FileType::Directory {
                size_row.set_subtitle("Counting…");
                let (row, folder) = (size_row.clone(), f.clone());
                glib::spawn_future_local(async move {
                    // instant from the index; network shares and other unindexed folders get a
                    // Calculate button instead, since adding them up means reading every file's entry
                    let r = match folder.path() {
                        Some(p) => gio::spawn_blocking(move || crate::search::folder_stats(&p))
                            .await
                            .unwrap_or(Err("Counting failed")),
                        None => Err("Not in the search index"),
                    };
                    match r {
                        Ok((n, b)) => row.set_subtitle(&format!("{} for {n} items", glib::format_size(b))),
                        Err(why) => {
                            row.set_subtitle(why);
                            add_calculate(&row, folder);
                        }
                    }
                });
            }
            if let Some(t) = crate::browser::link_target(&info) {
                action_row(&g, "Link Target", &t.display().to_string());
            }
            let t = adw::PreferencesGroup::builder().title("Times").build();
            page.add(&t);
            for (k, a) in [("Modified", "time::modified"), ("Accessed", "time::access"), ("Created", "time::created")] {
                let v = when(&info, a);
                if !v.is_empty() {
                    action_row(&t, k, &v);
                }
            }
            let p = adw::PreferencesGroup::builder().title("Permissions").build();
            page.add(&p);
            action_row(
                &p,
                "Owner",
                &format!(
                    "{} / {}",
                    info.attribute_string("owner::user").unwrap_or_default(),
                    info.attribute_string("owner::group").unwrap_or_default()
                ),
            );
            if info.has_attribute("unix::mode") {
                let mode = info.attribute_uint32("unix::mode");
                action_row(&p, "Mode", &perms(mode));
                if info.file_type() == gio::FileType::Regular && f.is_native() {
                    let sw = adw::SwitchRow::builder()
                        .title("Allow running as a program")
                        .active(mode & 0o111 != 0)
                        .sensitive(info.boolean("access::can-write"))
                        .build();
                    let f2 = f.clone();
                    let toast = std::rc::Rc::new(toast);
                    sw.connect_active_notify(move |sw| {
                        let m = if sw.is_active() { mode | ((mode & 0o444) >> 2) } else { mode & !0o111 };
                        let (f2, toast) = (f2.clone(), toast.clone());
                        glib::spawn_future_local(async move {
                            let r = gio::spawn_blocking(move || {
                                f2.set_attribute_uint32("unix::mode", m, gio::FileQueryInfoFlags::NONE, gio::Cancellable::NONE)
                                    .map_err(|e| crate::browser::human_gio(&e))
                            })
                            .await;
                            if let Ok(Err(e)) = r {
                                toast(&e);
                            }
                        });
                    });
                    p.add(&sw);
                }
            }
            if let Some(app) = gio::AppInfo::default_for_type(&ct, false) {
                action_row(&g, "Opens With", &app.display_name());
            }
            if info.file_type() == gio::FileType::Regular
                && let Some(path) = f.path()
            {
                page.add(&checksums(path));
            }
        }
        many => {
            title = format!("{} items", many.len());
            let bytes: u64 = many.iter().filter(|i| i.file_type() != gio::FileType::Directory).map(|i| i.size() as u64).sum();
            action_row(&g, "Selection", &title);
            action_row(&g, "Size of files", &glib::format_size(bytes));
        }
    }
    let tv = adw::ToolbarView::new();
    tv.add_top_bar(&adw::HeaderBar::builder().title_widget(&adw::WindowTitle::new(&title, "Properties")).build());
    tv.set_content(Some(&page));
    adw::Dialog::builder()
        .child(&tv)
        .content_width(460)
        .content_height(560)
        .build()
        .present(Some(parent));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walks_folder_sizes() {
        let d = std::env::temp_dir().join(format!("kestrel-walk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("a/b")).unwrap();
        std::fs::write(d.join("one"), [0u8; 100]).unwrap();
        std::fs::write(d.join("a/two"), [0u8; 20]).unwrap();
        std::fs::write(d.join("a/b/three"), [0u8; 3]).unwrap();
        std::os::unix::fs::symlink(&d, d.join("a/loop")).unwrap(); // not followed
        let (n, b) = (AtomicU64::new(0), AtomicU64::new(0));
        let root = gio::File::for_path(&d);
        assert!(walk_size(&root, &n, &b, &gio::Cancellable::new()));
        // one, a, a/two, a/b, a/b/three, a/loop
        assert_eq!(n.load(Ordering::Relaxed), 6);
        assert!(b.load(Ordering::Relaxed) >= 123);
        let c = gio::Cancellable::new();
        c.cancel();
        assert!(!walk_size(&root, &AtomicU64::new(0), &AtomicU64::new(0), &c));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn hashes_of_abc() {
        let p = std::env::temp_dir().join(format!("kestrel-hash-{}", std::process::id()));
        std::fs::write(&p, "abc").unwrap();
        let read = std::sync::atomic::AtomicU64::new(0);
        let h = super::hashes(&p, &read).unwrap();
        assert_eq!(h[0], "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(h[1], "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(h[2], "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(read.into_inner(), 3);
        std::fs::remove_file(p).unwrap();
    }
}
