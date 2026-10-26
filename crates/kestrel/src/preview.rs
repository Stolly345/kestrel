//! Preview pane: picture, video/audio player, text, rendered Markdown, PDF/Office pages,
//! document thumbnail, or folder totals.

use crate::browser::{fmt_date, info_file, set_icon};
use crate::thumbs;
use adw::prelude::*;
use gtk::{gio, glib};

pub struct Preview {
    pub root: gtk::Box,
    body: gtk::Box,
    /// Bumped on every show(); late async results for an older selection are dropped.
    serial: std::rc::Rc<std::cell::Cell<u64>>,
}

fn row(grid: &gtk::Grid, n: i32, k: &str, v: &str) {
    if v.is_empty() {
        return;
    }
    grid.attach(
        &gtk::Label::builder().label(k).xalign(1.0).yalign(0.0).css_classes(["dim-label"]).build(),
        0,
        n,
        1,
        1,
    );
    grid.attach(
        &gtk::Label::builder()
            .label(v)
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .selectable(true)
            .hexpand(true)
            .build(),
        1,
        n,
        1,
        1,
    );
}

pub fn perms(mode: u32) -> String {
    let bits = ["r", "w", "x"];
    (0..9).map(|i| if mode & (1 << (8 - i)) != 0 { bits[i % 3] } else { "-" }).collect::<String>() + &format!("  ({:o})", mode & 0o7777)
}

const TEXT_LIMIT: usize = 256 * 1024;

/// Word, Excel, PowerPoint, OpenDocument and RTF: LibreOffice turns them into a PDF first.
fn is_office(ct: &str) -> bool {
    [
        "application/msword",
        "application/rtf",
        "text/rtf",
        "application/vnd.ms-excel",
        "application/vnd.ms-powerpoint",
        "application/vnd.ms-word",
        "application/vnd.openxmlformats-officedocument.",
        "application/vnd.oasis.opendocument.",
    ]
    .iter()
    .any(|t| ct.starts_with(t))
}

/// A document to page through: its PDF (LibreOffice's conversion, for Office files) and page
/// count. Pages render one at a time as they're viewed, into `dir`
/// (~/.cache/kestrel/preview/<hash of path, size, mtime>). LibreOffice and Poppler parse hostile
/// files, so both run in the archive sandbox (no network, no home, only `dir` writable).
struct Doc {
    pdf: std::path::PathBuf,
    dir: std::path::PathBuf,
    pages: u32,
}

fn cache_root() -> std::path::PathBuf {
    glib::user_cache_dir().join("kestrel/preview")
}

const CACHE_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(30 * 86400);
const CACHE_MAX_BYTES: u64 = 512 << 20;

/// Drop document caches not viewed for 30 days, then the least recently viewed until the
/// whole cache is under 512 MB.
fn prune(root: &std::path::Path, max_age: std::time::Duration, max_bytes: u64) {
    let Ok(rd) = std::fs::read_dir(root) else { return };
    let now = std::time::SystemTime::now();
    let mut dirs: Vec<(std::time::SystemTime, u64, std::path::PathBuf)> = rd
        .flatten()
        .filter_map(|e| {
            let used = e.metadata().ok()?.modified().ok()?;
            // flat: doc.pdf, pages, p<N>.png
            let bytes = std::fs::read_dir(e.path())
                .ok()?
                .flatten()
                .filter_map(|f| f.metadata().ok())
                .map(|m| m.len())
                .sum();
            Some((used, bytes, e.path()))
        })
        .collect();
    dirs.sort(); // oldest first
    let mut total: u64 = dirs.iter().map(|d| d.1).sum();
    for (used, bytes, path) in dirs {
        if now.duration_since(used).unwrap_or_default() < max_age && total <= max_bytes {
            continue;
        }
        if std::fs::remove_dir_all(&path).is_ok() {
            total -= bytes;
        }
    }
}

/// The archive's contents as a summary line and a path per line (folders end in /).
fn archive_text(entries: &[crate::archive::Entry]) -> (String, String) {
    const SHOW: usize = 1000;
    let (dirs, files): (Vec<_>, Vec<_>) = entries.iter().partition(|e| e.dir);
    let bytes: u64 = files.iter().map(|e| e.size).sum();
    let s = |n: usize, w: &str| if n == 1 { format!("1 {w}") } else { format!("{n} {w}s") };
    let summary = format!(
        "{}, {} · {} unpacked",
        s(files.len(), "file"),
        s(dirs.len(), "folder"),
        glib::format_size(bytes)
    );
    let mut list: Vec<String> = entries
        .iter()
        .take(SHOW)
        .map(|e| if e.dir { format!("{}/", e.path) } else { e.path.clone() })
        .collect();
    if entries.len() > SHOW {
        list.push(format!("…and {} more", entries.len() - SHOW));
    }
    (summary, list.join("\n"))
}

fn open_doc(path: &std::path::Path, office: bool, stamp: (i64, i64)) -> Option<Doc> {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (path, stamp).hash(&mut h);
    let dir = cache_root().join(format!("{:016x}", h.finish()));
    std::fs::create_dir_all(&dir).ok()?;
    // mtime = last viewed, which is what prune() goes by
    let _ = std::fs::File::open(&dir).and_then(|f| f.set_modified(std::time::SystemTime::now()));
    let pdf = if office {
        let pdf = dir.join("doc.pdf");
        if !pdf.exists() {
            let mut c = crate::archive::sandboxed("soffice", path, &dir);
            c.arg(format!("-env:UserInstallation=file://{}/profile", dir.display()))
                .args(["--headless", "--norestore", "--convert-to", "pdf", "--outdir"])
                .arg(&dir)
                .arg(path);
            let _ = c.output();
            let _ = std::fs::remove_dir_all(dir.join("profile"));
            std::fs::rename(dir.join(format!("{}.pdf", path.file_stem()?.to_string_lossy())), &pdf).ok()?;
        }
        pdf
    } else {
        path.to_path_buf()
    };
    let count = dir.join("pages");
    let pages = match std::fs::read_to_string(&count).ok().and_then(|s| s.trim().parse().ok()) {
        Some(n) => n,
        None => {
            let out = crate::archive::sandboxed("pdfinfo", &pdf, &dir).arg(&pdf).output().ok()?;
            let n: u32 = String::from_utf8_lossy(&out.stdout)
                .lines()
                .find_map(|l| l.strip_prefix("Pages:"))?
                .trim()
                .parse()
                .ok()?;
            let _ = std::fs::write(&count, n.to_string());
            n
        }
    };
    Some(Doc { pdf, dir, pages }).filter(|d| d.pages > 0)
}

/// Page `n` (1-based) as a PNG, rendered on first view.
fn render_page(doc: &Doc, n: u32) -> Option<std::path::PathBuf> {
    let png = doc.dir.join(format!("p{n}.png"));
    if !png.exists() {
        // render under a temporary name: a half-written PNG must never be shown
        let tmp = doc.dir.join(format!("tmp{n}-{:?}", std::thread::current().id()).replace(['(', ')'], ""));
        let mut c = crate::archive::sandboxed("pdftoppm", &doc.pdf, &doc.dir);
        c.args([
            "-singlefile",
            "-scale-to-x",
            "1000",
            "-scale-to-y",
            "-1",
            "-png",
            "-f",
            &n.to_string(),
            "-l",
            &n.to_string(),
        ])
        .arg(&doc.pdf)
        .arg(&tmp);
        let ok = c.output().is_ok_and(|o| o.status.success());
        let made = tmp.with_extension("png");
        if !ok || std::fs::rename(&made, &png).is_err() {
            let _ = std::fs::remove_file(&made);
            return None;
        }
    }
    Some(png)
}

/// One page at a time: arrows, "3 / 12", and the scroll wheel over the page flips pages.
/// Holds its widgets weakly so the handlers that own it don't keep them alive.
struct Pager {
    doc: std::sync::Arc<Doc>,
    cur: std::cell::Cell<u32>,
    pic: glib::WeakRef<gtk::Picture>,
    label: glib::WeakRef<gtk::Label>,
    prev: glib::WeakRef<gtk::Button>,
    next: glib::WeakRef<gtk::Button>,
    /// placeholder (icon + "Rendering preview…"), hidden once the first page is in
    waiting: glib::WeakRef<gtk::Box>,
    note: glib::WeakRef<gtk::Label>,
    serial: u64,
    me_serial: std::rc::Rc<std::cell::Cell<u64>>,
}

impl Pager {
    /// Show page `n`; false if it's out of range (so a scroll there can scroll the pane instead).
    fn go(self: &std::rc::Rc<Self>, n: u32) -> bool {
        if n < 1 || n > self.doc.pages || n == self.cur.get() {
            return false;
        }
        self.cur.set(n);
        if let (Some(l), Some(p), Some(x)) = (self.label.upgrade(), self.prev.upgrade(), self.next.upgrade()) {
            l.set_label(&format!("{n} / {}", self.doc.pages));
            p.set_sensitive(n > 1);
            x.set_sensitive(n < self.doc.pages);
        }
        let (me, doc) = (self.clone(), self.doc.clone());
        glib::spawn_future_local(async move {
            let png = gio::spawn_blocking(move || render_page(&doc, n)).await.ok().flatten();
            if me.cur.get() != n || me.me_serial.get() != me.serial {
                return; // flipped on, or another file selected, while this page rendered
            }
            match (png, me.pic.upgrade()) {
                (Some(png), Some(pic)) => {
                    pic.set_filename(Some(&png));
                    if let Some(w) = me.waiting.upgrade() {
                        w.set_visible(false);
                    }
                }
                _ => {
                    if let Some(note) = me.note.upgrade() {
                        note.set_label("Couldn't render this page");
                    }
                }
            }
        });
        true
    }
}

/// Markdown as Pango markup: headings, emphasis, code, links, lists, quotes, tables as rows.
fn md_markup(src: &str) -> String {
    use pulldown_cmark::{Event as E, HeadingLevel as H, Options, Parser, Tag as T, TagEnd as TE};
    let esc = |t: &str| glib::markup_escape_text(t).to_string();
    let (mut out, mut lists) = (String::new(), Vec::<Option<u64>>::new());
    let opts = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    for ev in Parser::new_ext(src, opts) {
        match ev {
            E::Start(T::Heading { level, .. }) => out.push_str(match level {
                H::H1 => "<span size='xx-large' weight='bold'>",
                H::H2 => "<span size='x-large' weight='bold'>",
                H::H3 => "<span size='large' weight='bold'>",
                _ => "<span weight='bold'>",
            }),
            E::End(TE::Heading(_)) => out.push_str("</span>\n\n"),
            E::End(TE::Paragraph) => out.push_str("\n\n"),
            E::Start(T::Emphasis) | E::Start(T::BlockQuote(_)) => out.push_str("<i>"),
            E::End(TE::Emphasis) => out.push_str("</i>"),
            E::End(TE::BlockQuote(_)) => out.push_str("</i>"),
            E::Start(T::Strong) => out.push_str("<b>"),
            E::End(TE::Strong) => out.push_str("</b>"),
            E::Start(T::Strikethrough) => out.push_str("<s>"),
            E::End(TE::Strikethrough) => out.push_str("</s>"),
            E::Start(T::CodeBlock(_)) => out.push_str("<tt>"),
            E::End(TE::CodeBlock) => out.push_str("</tt>\n"),
            E::Code(t) => out.push_str(&format!("<tt>{}</tt>", esc(&t))),
            E::Start(T::Link { dest_url, .. }) => out.push_str(&format!("<a href=\"{}\">", esc(&dest_url))),
            E::End(TE::Link) => out.push_str("</a>"),
            E::Start(T::List(first)) => {
                // a nested list starts on its own line, not after the parent item's text
                if !lists.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
                lists.push(first);
            }
            E::End(TE::List(_)) => {
                lists.pop();
                if lists.is_empty() {
                    out.push('\n');
                }
            }
            E::Start(T::Item) => {
                out.push_str(&"    ".repeat(lists.len().saturating_sub(1)));
                match lists.last_mut() {
                    Some(Some(n)) => {
                        out.push_str(&format!("{n}. "));
                        *n += 1;
                    }
                    _ => out.push_str("• "),
                }
            }
            E::End(TE::Item) if !out.ends_with('\n') => out.push('\n'),
            E::TaskListMarker(done) => out.push_str(if done { "☑ " } else { "☐ " }),
            E::End(TE::TableCell) => out.push_str("  │  "),
            E::End(TE::TableHead) | E::End(TE::TableRow) => out.push('\n'),
            // raw HTML shows as written rather than vanishing
            E::Text(t) | E::Html(t) | E::InlineHtml(t) => out.push_str(&esc(&t)),
            E::SoftBreak => out.push(' '),
            E::HardBreak => out.push('\n'),
            E::Rule => out.push_str("────────────\n\n"),
            _ => {}
        }
    }
    out.trim_end().to_string()
}

fn read_head(p: &std::path::Path) -> String {
    use std::io::Read;
    let mut buf = Vec::new();
    if let Ok(f) = std::fs::File::open(p) {
        let _ = f.take(TEXT_LIMIT as u64).read_to_end(&mut buf);
    }
    String::from_utf8_lossy(&buf).into_owned()
}

impl Preview {
    pub fn new() -> Preview {
        let body = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_start(16)
            .margin_end(16)
            .margin_top(16)
            .margin_bottom(16)
            .build();
        let scroller = gtk::ScrolledWindow::builder()
            .child(&body)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .build();
        // its own titled panel (sidebar shade, border, header) so it never reads as part of the file list
        let header = gtk::CenterBox::builder()
            .start_widget(&gtk::Label::builder().label("Preview").css_classes(["heading"]).build())
            .end_widget(
                &gtk::Button::builder()
                    .icon_name("window-close-symbolic")
                    .css_classes(["flat", "circular"])
                    .tooltip_text("Close Preview (Alt+P)")
                    .action_name("win.preview")
                    .valign(gtk::Align::Center)
                    .build(),
            )
            .css_classes(["preview-header"])
            .build();
        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .width_request(260)
            .visible(false)
            .css_classes(["preview-pane"])
            .build();
        root.append(&header);
        root.append(&scroller);
        crate::label_icon_buttons(root.upcast_ref());
        std::thread::spawn(|| prune(&cache_root(), CACHE_MAX_AGE, CACHE_MAX_BYTES));
        Preview {
            root,
            body,
            serial: Default::default(),
        }
    }

    fn clear(&self) {
        while let Some(c) = self.body.first_child() {
            self.body.remove(&c);
        }
    }

    pub fn show(&self, infos: &[gio::FileInfo], git: Option<char>) {
        self.serial.set(self.serial.get() + 1);
        self.clear();
        match infos {
            [] => {
                self.body.append(
                    &adw::StatusPage::builder()
                        .icon_name("view-reveal-symbolic")
                        .title("No selection")
                        .description("Select a file to preview it")
                        .build(),
                );
            }
            [info] => self.one(info, git),
            many => {
                let bytes: u64 = many.iter().filter(|i| i.file_type() != gio::FileType::Directory).map(|i| i.size() as u64).sum();
                self.body.append(
                    &adw::StatusPage::builder()
                        .icon_name("edit-select-all-symbolic")
                        .title(format!("{} items", many.len()))
                        .description(glib::format_size(bytes).as_str())
                        .build(),
                );
            }
        }
    }

    fn one(&self, info: &gio::FileInfo, git: Option<char>) {
        let Some(file) = info_file(info) else { return };
        let ct = info.content_type().map(|c| c.to_string()).unwrap_or_default();
        let is_dir = info.file_type() == gio::FileType::Directory;

        // the big visual
        let visual = gtk::Box::builder().orientation(gtk::Orientation::Vertical).halign(gtk::Align::Fill).build();
        if ct.starts_with("image/") && file.is_native() {
            let pic = gtk::Picture::for_file(&file);
            pic.set_can_shrink(true);
            pic.set_content_fit(gtk::ContentFit::Contain);
            pic.set_height_request(260);
            visual.append(&pic);
        } else if (ct.starts_with("video/") || ct.starts_with("audio/")) && file.is_native() {
            let v = gtk::Video::for_file(Some(&file));
            v.set_autoplay(false);
            v.set_height_request(if ct.starts_with("video/") { 220 } else { 60 });
            visual.append(&v);
        } else if (ct == "application/pdf" || is_office(&ct))
            && let Some(path) = file.path()
        {
            // the icon stands in while the document opens and the first page renders
            let waiting = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(6).build();
            let img = gtk::Image::builder().pixel_size(128).build();
            set_icon(&img, info);
            img.set_pixel_size(128);
            let note = gtk::Label::builder().label("Rendering preview…").wrap(true).css_classes(["dim-label"]).build();
            waiting.append(&img);
            waiting.append(&note);
            let pic = gtk::Picture::builder()
                .can_shrink(true)
                .content_fit(gtk::ContentFit::Contain)
                .css_classes(["card"])
                .build();
            let btn = |icon: &str, tip: &str| {
                gtk::Button::builder()
                    .icon_name(icon)
                    .tooltip_text(tip)
                    .css_classes(["flat", "circular"])
                    .sensitive(false)
                    .build()
            };
            let (prev, next) = (btn("go-previous-symbolic", "Previous page"), btn("go-next-symbolic", "Next page"));
            let label = gtk::Label::builder().css_classes(["dim-label", "numeric"]).build();
            let nav = gtk::CenterBox::builder()
                .start_widget(&prev)
                .center_widget(&label)
                .end_widget(&next)
                .visible(false)
                .build();
            // arrows above the page: pages differ in height, and the buttons must not move under
            // the pointer while clicking through
            visual.append(&nav);
            visual.append(&waiting);
            visual.append(&pic);
            let office = is_office(&ct);
            let stamp = (info.size(), info.modification_date_time().map_or(0, |d| d.to_unix()));
            let (serial, me_serial) = (self.serial.get(), self.serial.clone());
            glib::spawn_future_local(async move {
                let doc = gio::spawn_blocking(move || open_doc(&path, office, stamp)).await.ok().flatten();
                if me_serial.get() != serial {
                    return;
                }
                let Some(doc) = doc else {
                    note.set_label(if office {
                        "Office previews need LibreOffice (libreoffice-writer, -calc, -impress)"
                    } else {
                        "No preview available"
                    });
                    return;
                };
                nav.set_visible(doc.pages > 1);
                let pager = std::rc::Rc::new(Pager {
                    doc: std::sync::Arc::new(doc),
                    cur: Default::default(),
                    pic: pic.downgrade(),
                    label: label.downgrade(),
                    prev: prev.downgrade(),
                    next: next.downgrade(),
                    waiting: waiting.downgrade(),
                    note: note.downgrade(),
                    serial,
                    me_serial,
                });
                let p = pager.clone();
                prev.connect_clicked(move |_| {
                    p.go(p.cur.get() - 1);
                });
                let p = pager.clone();
                next.connect_clicked(move |_| {
                    p.go(p.cur.get() + 1);
                });
                // wheel/touchpad over the page flips pages; past either end the pane scrolls
                let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL | gtk::EventControllerScrollFlags::DISCRETE);
                let p = pager.clone();
                scroll.connect_scroll(move |_, _, dy| {
                    let n = p.cur.get();
                    let to = if dy > 0.0 { n + 1 } else { n.saturating_sub(1) };
                    if p.go(to) { glib::Propagation::Stop } else { glib::Propagation::Proceed }
                });
                pic.add_controller(scroll);
                pager.go(1);
            });
        } else if thumbs::can_thumbnail(info) {
            let pic = gtk::Picture::builder()
                .can_shrink(true)
                .content_fit(gtk::ContentFit::Contain)
                .height_request(300)
                .build();
            let img = gtk::Image::builder().pixel_size(128).build();
            set_icon(&img, info);
            img.set_pixel_size(128);
            visual.append(&img);
            visual.append(&pic);
            let (info, serial, me_serial) = (info.clone(), self.serial.get(), self.serial.clone());
            glib::spawn_future_local(async move {
                let wanted = || me_serial.get() == serial;
                if let Some(p) = thumbs::get(&info, thumbs::Size::XLarge, wanted).await
                    && me_serial.get() == serial
                {
                    pic.set_filename(Some(&p));
                    img.set_visible(false);
                }
            });
        } else {
            let img = gtk::Image::builder().pixel_size(128).build();
            set_icon(&img, info);
            img.set_pixel_size(128);
            visual.append(&img);
        }
        self.body.append(&visual);
        self.body.append(
            &gtk::Label::builder()
                .label(info.display_name().as_str())
                .wrap(true)
                .wrap_mode(gtk::pango::WrapMode::WordChar)
                .css_classes(["title-3"])
                .justify(gtk::Justification::Center)
                .selectable(true)
                .build(),
        );
        if file.is_native() && !is_dir {
            // tag dots act on the previewed file itself (not "the selection", which a click on
            // the list or a refresh may have changed)
            let picker = std::rc::Rc::new(std::cell::OnceCell::<gtk::Box>::new());
            let (p, f) = (picker.clone(), file.clone());
            let row = crate::tags::picker(move |t| {
                if let Err(e) = crate::tags::toggle(std::slice::from_ref(&f), t) {
                    eprintln!("kestrel: tagging failed: {e}");
                }
                if let Some(row) = p.get() {
                    crate::tags::set_picker(row, crate::tags::read(&f));
                }
            });
            crate::tags::set_picker(&row, crate::tags::of_info(info));
            let _ = picker.set(row.clone());
            self.body.append(&row);
        }

        let grid = gtk::Grid::builder().column_spacing(12).row_spacing(6).build();
        let desc = gio::content_type_get_description(&ct);
        row(&grid, 0, "Type", &desc);
        let size_lbl = if is_dir {
            String::new()
        } else {
            glib::format_size(info.size() as u64).to_string()
        };
        row(&grid, 1, "Size", &size_lbl);
        row(&grid, 2, "Modified", &fmt_date(info));
        let loc = file
            .parent()
            .map(|p| p.path().map_or_else(|| p.uri().to_string(), |x| x.display().to_string()))
            .unwrap_or_default();
        row(&grid, 3, "Location", &loc);
        if info.has_attribute("unix::mode") {
            row(&grid, 4, "Permissions", &perms(info.attribute_uint32("unix::mode")));
        }
        row(&grid, 5, "Owner", info.attribute_string("owner::user").as_deref().unwrap_or(""));
        let git_s = match git {
            Some('M') => "Modified",
            Some('A') => "Added",
            Some('?') => "Untracked",
            Some('D') => "Deleted",
            Some('R') => "Renamed",
            Some('U') => "Conflict",
            _ => "",
        };
        row(&grid, 7, "Git", git_s);
        self.body.append(&grid);

        if is_dir {
            // folder totals straight from the index: instant even for huge trees
            if let Some(p) = file.path() {
                let (serial, me_serial) = (self.serial.get(), self.serial.clone());
                let lbl = gtk::Label::builder().label("Counting…").css_classes(["dim-label"]).build();
                self.body.append(&lbl);
                glib::spawn_future_local(async move {
                    let r = gio::spawn_blocking(move || crate::search::folder_stats(&p))
                        .await
                        .unwrap_or(Err("Counting failed"));
                    if me_serial.get() == serial {
                        lbl.set_label(&match r {
                            Ok((n, b)) => format!("{n} items inside, {}", glib::format_size(b)),
                            Err(why) => why.into(),
                        });
                    }
                });
            }
        } else if gio::content_type_is_a(&ct, "text/markdown")
            && let Some(p) = file.path()
        {
            let lbl = gtk::Label::builder()
                .use_markup(true)
                .label(md_markup(&read_head(&p)))
                .wrap(true)
                .wrap_mode(gtk::pango::WrapMode::WordChar)
                .xalign(0.0)
                .selectable(true)
                .margin_start(12)
                .margin_end(12)
                .margin_top(12)
                .margin_bottom(12)
                .build();
            let card = gtk::Box::builder().css_classes(["card"]).build();
            card.append(&lbl);
            self.body.append(&card);
        } else if gio::content_type_is_a(&ct, "text/plain")
            && let Some(p) = file.path()
        {
            self.body.append(&text_card(&read_head(&p)));
        } else if crate::archive::archive_ext(&info.name().to_string_lossy()).is_some()
            && let Some(p) = file.path().and_then(|p| p.canonicalize().ok())
        {
            let lbl = gtk::Label::builder()
                .label("Reading archive…")
                .xalign(0.0)
                .wrap(true)
                .css_classes(["dim-label"])
                .build();
            self.body.append(&lbl);
            let (body, serial, me_serial) = (self.body.clone(), self.serial.get(), self.serial.clone());
            glib::spawn_future_local(async move {
                let r = gio::spawn_blocking(move || {
                    let scratch = cache_root().join("list");
                    std::fs::create_dir_all(&scratch).map_err(|e| e.to_string())?;
                    crate::archive::list_archive(&p, &scratch).map(|e| archive_text(&e))
                })
                .await
                .unwrap_or_else(|_| Err("Couldn't read this archive".into()));
                if me_serial.get() != serial {
                    return;
                }
                match r {
                    Ok((summary, list)) => {
                        lbl.set_label(&summary);
                        body.append(&text_card(&list));
                    }
                    Err(e) => lbl.set_label(&e),
                }
            });
        }
    }
}

/// Monospace, read-only, scrollable text in a card.
fn text_card(text: &str) -> gtk::ScrolledWindow {
    let tv = gtk::TextView::builder()
        .editable(false)
        .monospace(true)
        .wrap_mode(gtk::WrapMode::None)
        .top_margin(8)
        .left_margin(8)
        .bottom_margin(8)
        .right_margin(8)
        .build();
    tv.buffer().set_text(text);
    tv.add_css_class("card");
    gtk::ScrolledWindow::builder().child(&tv).height_request(360).vexpand(true).build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prune_drops_stale_then_oldest() {
        let root = std::env::temp_dir().join(format!("kestrel-prune-{}", std::process::id()));
        let day = std::time::Duration::from_secs(86400);
        let now = std::time::SystemTime::now();
        for (name, age, bytes) in [("old", 40, 1), ("mid", 5, 600), ("new", 1, 600)] {
            std::fs::create_dir_all(root.join(name)).unwrap();
            std::fs::write(root.join(name).join("p1.png"), vec![0u8; bytes]).unwrap();
            std::fs::File::open(root.join(name)).unwrap().set_modified(now - day * age).unwrap();
        }
        prune(&root, day * 30, 1000);
        let left: Vec<_> = ["old", "mid", "new"].into_iter().filter(|n| root.join(n).exists()).collect();
        assert_eq!(left, ["new"]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn markdown_is_valid_pango() {
        let md = "# T <x>\n\nSome **bold** & *it* `c<d>` [link](http://a?b=1&c=2)\n\n- a\n  1. one\n  2. two\n- [x] done\n\n> quote\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n```\nfn x() {}\n```\n---\n";
        let m = md_markup(md);
        // <a href> is GtkLabel's extension to Pango markup; check the rest parses
        let (pre, post) = m.split_once("<a href=").unwrap();
        let plain = format!("{pre}{}", post.split_once('>').unwrap().1.replacen("</a>", "", 1));
        gtk::pango::parse_markup(&plain, '\0').unwrap();
        assert!(m.contains("<b>bold</b>") && m.contains("2. two") && m.contains("☑") && m.contains("&lt;x&gt;"));
        assert!(is_office("application/vnd.openxmlformats-officedocument.wordprocessingml.document"));
        assert!(!is_office("application/pdf"));
    }
}
