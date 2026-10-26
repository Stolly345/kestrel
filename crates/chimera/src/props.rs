//! Properties dialog.

use crate::browser::{ATTRS, info_file};
use crate::preview::perms;
use adw::prelude::*;
use gtk::{gio, glib};

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

pub fn show(parent: &impl IsA<gtk::Widget>, infos: Vec<gio::FileInfo>, toast: impl Fn(&str) + 'static) {
    let page = adw::PreferencesPage::new();
    let g = adw::PreferencesGroup::new();
    page.add(&g);
    let title;
    match infos.as_slice() {
        [one] => {
            let Some(f) = info_file(one) else { return };
            // re-query for times/owner the listing didn't ask for
            let info = f
                .query_info(
                    &format!("{ATTRS},time::*,owner::*,unix::*,access::*"),
                    gio::FileQueryInfoFlags::NONE,
                    gio::Cancellable::NONE,
                )
                .unwrap_or_else(|_| one.clone());
            title = info.display_name().to_string();
            let ct = info.content_type().map(|c| c.to_string()).unwrap_or_default();
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
                if let Some(p) = f.path() {
                    glib::spawn_future_local(async move {
                        let q = crate::search::under_query(&p);
                        let r = gio::spawn_blocking(move || crate::search::stats(&q)).await.ok().flatten();
                        size_row.set_subtitle(&r.map_or("Needs chimera-indexd".into(), |(n, b)| format!("{} for {n} items", glib::format_size(b))));
                    });
                }
            }
            if info.file_type() == gio::FileType::SymbolicLink || info.is_symlink() {
                action_row(
                    &g,
                    "Link Target",
                    info.symlink_target().map(|p| p.display().to_string()).unwrap_or_default().as_str(),
                );
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
                    sw.connect_active_notify(move |sw| {
                        let m = if sw.is_active() { mode | ((mode & 0o444) >> 2) } else { mode & !0o111 };
                        if let Err(e) = f2.set_attribute_uint32("unix::mode", m, gio::FileQueryInfoFlags::NONE, gio::Cancellable::NONE) {
                            toast(e.message());
                        }
                    });
                    p.add(&sw);
                }
            }
            if let Some(app) = gio::AppInfo::default_for_type(&ct, false) {
                action_row(&g, "Opens With", &app.display_name());
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
