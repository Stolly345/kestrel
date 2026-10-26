//! Command palette (Ctrl+Shift+P): every action, filterable by typing.

use adw::prelude::*;
use gtk::{glib, glib::variant::ToVariant};

/// (label, action, target)
pub const COMMANDS: &[(&str, &str, Option<&str>)] = &[
    ("New tab", "win.new-tab", None),
    ("Close tab", "win.close-tab", None),
    ("New window", "win.new-window", None),
    ("Back", "win.back", None),
    ("Forward", "win.forward", None),
    ("Go up", "win.up", None),
    ("Reload", "win.reload", None),
    ("Go to location…", "win.edit-location", None),
    ("Search in this folder", "win.search-here", None),
    ("Search everywhere", "win.search-everywhere", None),
    ("View: details", "win.view", Some("list")),
    ("View: grid", "win.view", Some("grid")),
    ("View: columns", "win.view", Some("columns")),
    ("Toggle hidden files", "win.show-hidden", None),
    ("Toggle dual pane", "win.dual", None),
    ("Switch to other pane", "win.other-pane", None),
    ("Toggle preview pane", "win.preview", None),
    ("Open", "win.open", None),
    ("Open with…", "win.open-with", None),
    ("Open in new tab", "win.open-tab", None),
    ("Open in other pane", "win.open-other", None),
    ("Copy", "win.copy", None),
    ("Cut", "win.cut", None),
    ("Paste", "win.paste", None),
    ("Copy to other pane", "win.copy-other", None),
    ("Move to other pane", "win.move-other", None),
    ("Rename…", "win.rename", None),
    ("Move to trash", "win.trash", None),
    ("Delete permanently…", "win.delete", None),
    ("Undo", "win.undo", None),
    ("Restore from trash", "win.restore", None),
    ("Empty trash…", "win.empty-trash", None),
    ("New folder…", "win.new-folder", None),
    ("Copy path", "win.copy-path", None),
    ("Add bookmark", "win.add-bookmark", None),
    ("Open terminal here", "win.terminal", None),
    ("Terminal panel", "win.terminal-panel", None),
    ("Properties", "win.properties", None),
    ("Compress…", "win.compress", None),
    ("Extract here", "win.extract", None),
    ("Extract to…", "win.extract-to", None),
    ("Browse archive", "win.browse-archive", None),
    ("Tag: red", "win.tag", Some("red")),
    ("Tag: orange", "win.tag", Some("orange")),
    ("Tag: yellow", "win.tag", Some("yellow")),
    ("Tag: green", "win.tag", Some("green")),
    ("Tag: blue", "win.tag", Some("blue")),
    ("Tag: purple", "win.tag", Some("purple")),
    ("Tag: gray", "win.tag", Some("gray")),
    ("Clear tags", "win.tag", Some("none")),
    ("Next region (search / files / sidebar)", "win.focus-next-region", None),
    ("Preferences", "win.preferences", None),
    ("Search syntax", "win.syntax", None),
    ("About Kestrel", "win.about", None),
];

pub fn show(win: &adw::ApplicationWindow) {
    let app = win.application();
    let entry = gtk::SearchEntry::builder().placeholder_text("Type a command").hexpand(true).build();
    let list = gtk::ListBox::builder().css_classes(["navigation-sidebar"]).build();
    for (label, action, target) in COMMANDS {
        let detailed = match target {
            Some(t) => format!("{action}::{t}"),
            None => action.to_string(),
        };
        let accel = app
            .as_ref()
            .and_then(|a| a.accels_for_action(&detailed).first().cloned())
            .and_then(|a| gtk::accelerator_parse(&a))
            .map(|(k, m)| gtk::accelerator_get_label(k, m).to_string())
            .unwrap_or_default();
        let b = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        b.append(&gtk::Label::builder().label(*label).xalign(0.0).hexpand(true).build());
        b.append(&gtk::Label::builder().label(&accel).css_classes(["dim-label"]).build());
        let row = gtk::ListBoxRow::builder().child(&b).build();
        row.set_widget_name(&detailed);
        list.append(&row);
    }
    let e = entry.clone();
    list.set_filter_func(move |row| {
        let label = row
            .child()
            .and_then(|b| b.first_child())
            .and_downcast::<gtk::Label>()
            .map(|l| l.label().to_lowercase())
            .unwrap_or_default();
        e.text().to_lowercase().split_whitespace().all(|w| label.contains(w))
    });
    let l = list.clone();
    entry.connect_search_changed(move |_| {
        l.invalidate_filter();
        if let Some(r) = first_visible(&l) {
            l.select_row(Some(&r));
        }
    });

    let body = gtk::Box::new(gtk::Orientation::Vertical, 6);
    body.set_margin_start(12);
    body.set_margin_end(12);
    body.set_margin_top(12);
    body.set_margin_bottom(12);
    body.append(&entry);
    body.append(&gtk::ScrolledWindow::builder().child(&list).min_content_height(360).vexpand(true).build());
    let dialog = adw::Dialog::builder().child(&body).content_width(520).title("Commands").build();

    let run = {
        let (d, w) = (dialog.clone(), win.clone());
        move |row: &gtk::ListBoxRow| {
            let name = row.widget_name();
            let (action, target) = name.split_once("::").map_or((name.as_str(), None), |(a, t)| (a, Some(t)));
            d.close();
            // run after the dialog has gone so focus-sensitive actions hit the file view
            let (w, action, target) = (w.clone(), action.to_string(), target.map(|t| t.to_variant()));
            glib::idle_add_local_once(move || {
                let _ = WidgetExt::activate_action(&w, &action, target.as_ref());
            });
        }
    };
    let r = run.clone();
    list.connect_row_activated(move |_, row| r(row));
    // the search entry eats Esc (stop-search), so close the dialog from there
    let d = dialog.clone();
    entry.connect_stop_search(move |_| {
        d.close();
    });
    let l = list.clone();
    entry.connect_activate(move |_| {
        if let Some(row) = l.selected_row().or_else(|| first_visible(&l)) {
            run(&row);
        }
    });
    let k = gtk::EventControllerKey::new();
    let l = list.clone();
    k.connect_key_pressed(move |k, key, _, _| {
        let step = match key {
            gtk::gdk::Key::Down => 1,
            gtk::gdk::Key::Up => -1,
            _ => return glib::Propagation::Proceed,
        };
        let rows: Vec<gtk::ListBoxRow> = visible_rows(&l);
        let cur = l.selected_row().and_then(|s| rows.iter().position(|r| *r == s)).unwrap_or(0) as i32;
        if let Some(r) = rows.get((cur + step).clamp(0, rows.len() as i32 - 1) as usize) {
            l.select_row(Some(r));
            // focusing the row scrolls it into view; then keep typing in the entry
            r.grab_focus();
            if let Some(e) = k.widget().and_downcast::<gtk::SearchEntry>() {
                e.grab_focus();
                e.set_position(-1);
            }
        }
        glib::Propagation::Stop
    });
    entry.add_controller(k);
    dialog.present(Some(win));
    entry.grab_focus();
}

fn visible_rows(l: &gtk::ListBox) -> Vec<gtk::ListBoxRow> {
    let mut out = Vec::new();
    let mut c = l.first_child();
    while let Some(w) = c {
        c = w.next_sibling();
        if let Ok(r) = w.downcast::<gtk::ListBoxRow>()
            && r.is_child_visible()
        {
            out.push(r);
        }
    }
    out
}

fn first_visible(l: &gtk::ListBox) -> Option<gtk::ListBoxRow> {
    visible_rows(l).into_iter().next()
}
