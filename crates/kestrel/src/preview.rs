//! Preview pane: picture, video/audio player, text, rendered Markdown, PDF/Office pages,
//! document thumbnail, or folder totals.

use crate::browser::{fmt_date, info_file, set_icon};
use crate::thumbs;
use adw::prelude::*;
use gtk::{gio, glib};

pub struct Preview {
    pub root: gtk::ScrolledWindow,
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
/// Pages rendered for a PDF or Office document; the rest is one double-click away.
const PAGES: u32 = 20;

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

/// PNGs of a document's first pages, cached under ~/.cache/kestrel/preview by path, size and
/// mtime. LibreOffice and Poppler parse hostile files, so both run in the archive sandbox
/// (no network, no home, only the cache folder writable).
// ponytail: cache is never pruned; add an LRU sweep if ~/.cache/kestrel/preview grows large
fn render_pages(path: &std::path::Path, office: bool, stamp: (i64, i64)) -> Option<Vec<std::path::PathBuf>> {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (path, stamp).hash(&mut h);
    let dir = glib::user_cache_dir().join("kestrel/preview").join(format!("{:016x}", h.finish()));
    if !dir.join("done").exists() {
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).ok()?;
        let pdf = if office {
            let mut c = crate::fileops::sandboxed("soffice", path, &dir);
            c.arg(format!("-env:UserInstallation=file://{}/profile", dir.display()))
                .args(["--headless", "--norestore", "--convert-to", "pdf", "--outdir"])
                .arg(&dir)
                .arg(path);
            let _ = c.output();
            let _ = std::fs::remove_dir_all(dir.join("profile"));
            dir.join(format!("{}.pdf", path.file_stem()?.to_string_lossy()))
        } else {
            path.to_path_buf()
        };
        let mut c = crate::fileops::sandboxed("pdftoppm", &pdf, &dir);
        c.args(["-scale-to-x", "640", "-scale-to-y", "-1", "-png", "-f", "1", "-l", &PAGES.to_string()])
            .arg(&pdf)
            .arg(dir.join("page"));
        let ok = c.output().is_ok_and(|o| o.status.success());
        if office {
            let _ = std::fs::remove_file(&pdf);
        }
        if !ok {
            let _ = std::fs::remove_dir_all(&dir);
            return None;
        }
        std::fs::write(dir.join("done"), "").ok()?;
    }
    // pdftoppm zero-pads page numbers to the same width, so name order is page order
    let mut pages: Vec<_> = std::fs::read_dir(&dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "png"))
        .collect();
    pages.sort();
    Some(pages).filter(|p| !p.is_empty())
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
        let root = gtk::ScrolledWindow::builder()
            .child(&body)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .width_request(260)
            .visible(false)
            .build();
        root.add_css_class("preview-pane");
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
            // the thumbnail (or icon) stands in while the pages render
            let img = gtk::Image::builder().pixel_size(128).build();
            set_icon(&img, info);
            img.set_pixel_size(128);
            let note = gtk::Label::builder().label("Rendering preview…").css_classes(["dim-label"]).build();
            visual.append(&img);
            visual.append(&note);
            let pages = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(8).build();
            visual.append(&pages);
            let office = is_office(&ct);
            let stamp = (info.size(), info.modification_date_time().map_or(0, |d| d.to_unix()));
            let (serial, me_serial) = (self.serial.get(), self.serial.clone());
            glib::spawn_future_local(async move {
                let r = gio::spawn_blocking(move || render_pages(&path, office, stamp)).await.ok().flatten();
                if me_serial.get() != serial {
                    return;
                }
                match r {
                    Some(files) => {
                        img.set_visible(false);
                        note.set_visible(false);
                        for f in files {
                            let pic = gtk::Picture::for_filename(&f);
                            pic.set_can_shrink(true);
                            pic.set_content_fit(gtk::ContentFit::Contain);
                            pic.add_css_class("card");
                            pages.append(&pic);
                        }
                    }
                    None if office => note.set_label("Office previews need LibreOffice (libreoffice-writer, -calc, -impress)"),
                    None => note.set_label("No preview available"),
                }
                note.set_wrap(true);
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
        let tags = crate::tags::names(crate::tags::of_info(info)).collect::<Vec<_>>().join(", ");
        row(&grid, 6, "Tags", &tags);
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
                    let q = crate::search::under_query(&p);
                    let r = gio::spawn_blocking(move || crate::search::stats(&q)).await.ok().flatten();
                    if me_serial.get() == serial {
                        lbl.set_label(&match r {
                            Some((n, b)) => format!("{n} items inside, {}", glib::format_size(b)),
                            None => "Folder size needs kestrel-indexd".into(),
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
            let text = read_head(&p);
            let tv = gtk::TextView::builder()
                .editable(false)
                .monospace(true)
                .wrap_mode(gtk::WrapMode::None)
                .top_margin(8)
                .left_margin(8)
                .bottom_margin(8)
                .right_margin(8)
                .build();
            tv.buffer().set_text(&text);
            tv.add_css_class("card");
            self.body
                .append(&gtk::ScrolledWindow::builder().child(&tv).height_request(360).vexpand(true).build());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
