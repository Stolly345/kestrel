//! Preview pane: picture, video/audio player, text, document thumbnail, or folder totals.

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
                            None => "Folder size needs chimera-indexd".into(),
                        });
                    }
                });
            }
        } else if gio::content_type_is_a(&ct, "text/plain")
            && let Some(p) = file.path()
        {
            let mut buf = Vec::new();
            use std::io::Read;
            if let Ok(f) = std::fs::File::open(&p) {
                let _ = f.take(TEXT_LIMIT as u64).read_to_end(&mut buf);
            }
            let text = String::from_utf8_lossy(&buf);
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
