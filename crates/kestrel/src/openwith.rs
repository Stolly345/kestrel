//! Open With: Kestrel's own app chooser, with Windows' "Always use this app" as a switch, and Set
//! Default App (the same list, no opening). Defaults are saved where GNOME Files saves them
//! (~/.config/mimeapps.list), so every app agrees.

use crate::fileops::Ops;
use adw::prelude::*;
use gtk::{gio, glib};
use std::rc::Rc;

/// The file's type for choosing an app. An empty file sniffs as "application/x-zerosize", which no
/// app claims, so that one goes by its name (a new `notes.md` is Markdown).
pub fn file_type(f: &gio::File, info: Option<&gio::FileInfo>) -> glib::GString {
    let sniffed = info.and_then(|i| i.content_type()).filter(|t| t != "application/x-zerosize");
    sniffed.unwrap_or_else(|| gio::content_type_guess(Some(&f.basename().unwrap_or_default()), None::<&[u8]>).0)
}

/// Open in the default app for the file's type; with none set, ask (show).
pub fn open_default(ops: &Rc<Ops>, f: &gio::File) {
    let ct = file_type(f, None);
    match gio::AppInfo::default_for_type(&ct, false) {
        Some(app) => launch(ops, &app, &[f.clone()]),
        None => show(ops, vec![f.clone()], None, false),
    }
}

fn launch(ops: &Rc<Ops>, app: &gio::AppInfo, files: &[gio::File]) {
    let ctx = ops.toasts.display().app_launch_context();
    if let Err(e) = app.launch(files, Some(&ctx)) {
        ops.toast(&crate::browser::human_gio(&e));
    }
}

/// The chooser: apps for the type, the current default first and picked. `set_default`: Set
/// Default App (only saves the choice); otherwise Open With, with "Always use" off by default.
pub fn show(ops: &Rc<Ops>, files: Vec<gio::File>, info: Option<&gio::FileInfo>, set_default: bool) {
    let Some(first) = files.first() else { return };
    let ct = file_type(first, info);
    let what = gio::content_type_get_description(&ct);
    let current = gio::AppInfo::default_for_type(&ct, false);
    let mut apps: Vec<gio::AppInfo> = current.iter().cloned().collect();
    for a in gio::AppInfo::all_for_type(&ct) {
        if !apps.iter().any(|b| b.equal(&a)) {
            apps.push(a);
        }
    }
    // ponytail: nothing registered for the type: every app, the way Windows offers "More apps"
    if apps.is_empty() {
        apps = gio::AppInfo::all().into_iter().filter(|a| a.should_show()).collect();
        apps.sort_by_key(|a| a.display_name().to_lowercase());
    }

    // one click picks, a double-click (or Enter) also confirms
    let list = gtk::ListBox::builder()
        .css_classes(["boxed-list"])
        .selection_mode(gtk::SelectionMode::Single)
        .activate_on_single_click(false)
        .build();
    for a in &apps {
        let row = adw::ActionRow::builder().title(glib::markup_escape_text(&a.display_name())).activatable(true).build();
        let icon = gtk::Image::builder().pixel_size(32).build();
        icon.set_from_gicon(&a.icon().unwrap_or_else(|| gio::ThemedIcon::new("application-x-executable").upcast()));
        row.add_prefix(&icon);
        if current.as_ref().is_some_and(|c| c.equal(a)) {
            row.set_subtitle("Default");
        }
        list.append(&row);
    }
    list.select_row(list.row_at_index(0).as_ref());
    let scroll = gtk::ScrolledWindow::builder()
        .child(&list)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .max_content_height(340)
        .build();
    let body = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12).build();
    body.append(&scroll);
    let always = adw::SwitchRow::builder().title("Always use for this file type").subtitle(what.as_str()).build();
    if !set_default {
        let opts = gtk::ListBox::builder().css_classes(["boxed-list"]).selection_mode(gtk::SelectionMode::None).build();
        opts.append(&always);
        body.append(&opts);
    }

    let name = first.basename().unwrap_or_default().display().to_string();
    let (heading, ok) = if set_default {
        ("Default App".to_string(), "Set as Default")
    } else if files.len() == 1 {
        (format!("Open \u{201c}{name}\u{201d} With"), "Open")
    } else {
        (format!("Open {} Files With", files.len()), "Open")
    };
    let d = adw::AlertDialog::builder().heading(heading).extra_child(&body).build();
    if set_default {
        d.set_body(&what);
    }
    d.add_responses(&[("cancel", "Cancel"), ("ok", ok)]);
    d.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
    d.set_default_response(Some("ok"));
    d.set_close_response("cancel");
    // double-click / Enter on an app: same as the button
    let dw = d.downgrade();
    list.connect_row_activated(move |l, r| {
        l.select_row(Some(r));
        if let Some(d) = dw.upgrade() {
            d.emit_by_name::<()>("response", &[&"ok"]);
            d.close();
        }
    });
    let ops2 = ops.clone();
    d.connect_response(Some("ok"), move |_, _| {
        let Some(app) = list.selected_row().and_then(|r| apps.get(r.index() as usize).cloned()) else { return };
        if set_default || always.is_active() {
            match app.set_as_default_for_type(&ct) {
                Ok(()) => ops2.toast(&format!("{} is now the default for {what}", app.display_name())),
                Err(e) => ops2.toast(&crate::browser::human_gio(&e)),
            }
        }
        if !set_default {
            launch(&ops2, &app, &files);
        }
    });
    d.present(ops.window().as_ref());
}
