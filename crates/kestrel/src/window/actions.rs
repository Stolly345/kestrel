//! Every window action (`win.*`): menus, shortcuts and the command palette all trigger these.

use super::*;

impl Win {
    pub(super) fn actions(self: &Rc<Self>) {
        let home = || gio::File::for_path(glib::home_dir());
        self.act("new-tab", move |w| {
            let loc = w.current().and_then(|b| b.dir()).unwrap_or_else(home);
            w.add_tab(w.active.get(), Loc::Dir(loc), true).focus_view();
        });
        self.act("close-tab", |w| {
            let v = &w.panes[w.active.get()].view;
            if let Some(p) = v.selected_page() {
                v.close_page(&p);
            }
        });
        self.act("new-window", move |w| {
            let loc = w.current().map_or_else(|| Loc::Dir(home()), |b| b.loc());
            if let Some(app) = w.window.application().and_downcast::<adw::Application>() {
                Win::new(&app, vec![loc], vec![], false).window.present();
            }
        });
        self.act("back", |w| w.current().into_iter().for_each(|b| b.back()));
        self.act("forward", |w| w.current().into_iter().for_each(|b| b.forward()));
        self.act("up", |w| w.current().into_iter().for_each(|b| b.up()));
        self.act("reload", |w| w.current().into_iter().for_each(|b| b.reload()));
        self.act("edit-location", |w| w.current().into_iter().for_each(|b| b.edit_location()));
        self.act("search-here", |w| {
            if let Some(b) = w.current() {
                b.set_scope(Scope::Folder);
                w.update();
            }
            w.focus_search(None);
            w.show_suggestions();
        });
        self.act("search-everywhere", |w| {
            if let Some(b) = w.current() {
                b.set_scope(Scope::Everywhere);
                w.update();
            }
            w.focus_search(None);
            w.show_suggestions();
        });
        self.act("open", |w| {
            let Some(b) = w.current() else { return };
            for i in b.selected_infos() {
                b.open_info(&i);
            }
        });
        self.act("open-with", |w| {
            if let (Some(b), Some(f)) = (w.current(), w.sel().into_iter().next()) {
                b.launch(&f, true);
            }
        });
        self.act("open-tab", |w| {
            let Some(b) = w.current() else { return };
            for i in b.selected_infos().iter().filter(|i| i.file_type() == gio::FileType::Directory) {
                if let Some(f) = crate::browser::info_file(i) {
                    w.add_tab(w.active.get(), Loc::Dir(f), false);
                }
            }
        });
        self.act("open-other", |w| {
            let Some(d) = w.target_dir() else { return };
            if !w.panes[1].root.is_visible() {
                w.set_dual(true);
            }
            if let Some(o) = w.other() {
                o.go(Loc::Dir(d));
            }
        });
        self.act("copy", |w| w.ops.set_clipboard(&w.sel(), false));
        self.act("cut", |w| w.ops.set_clipboard(&w.sel(), true));
        for (name, link) in [("paste", false), ("paste-link", true)] {
            self.act(name, move |w| match w.current().and_then(|b| b.dir()) {
                Some(d) => w.ops.paste(d, link),
                None => w.ops.toast("Open a folder to paste into"),
            });
        }
        // Explorer's "Move to" / "Copy to": pick any folder
        for (name, op, title) in [("copy-to", Op::Copy, "Copy To"), ("move-to", Op::Move, "Move To")] {
            self.act(name, move |w| {
                let files = w.sel();
                let fd = gtk::FileDialog::builder().title(title).accept_label(title).modal(true).build();
                if let Some(d) = w.current().and_then(|b| b.dir()) {
                    fd.set_initial_folder(Some(&d));
                }
                let ops = w.ops.clone();
                fd.select_folder(Some(&w.window), gio::Cancellable::NONE, move |r| {
                    if let Ok(dest) = r {
                        ops.transfer(op, files.clone(), dest);
                    }
                });
            });
        }
        self.act("select-all", |w| w.current().into_iter().for_each(|b| _ = b.sel().select_all()));
        self.act("select-none", |w| w.current().into_iter().for_each(|b| _ = b.sel().unselect_all()));
        self.act("invert-selection", |w| {
            let Some(b) = w.current() else { return };
            let s = b.sel();
            let all = gtk::Bitset::new_range(0, s.n_items());
            let flipped = all.copy();
            flipped.subtract(&s.selection());
            s.set_selection(&flipped, &all);
        });
        for (name, op) in [("copy-other", Op::Copy), ("move-other", Op::Move)] {
            self.act(name, move |w| match w.other().and_then(|o| o.dir()) {
                Some(d) => w.ops.transfer(op, w.sel(), d),
                None => w.ops.toast("Turn on dual pane (F3) and open a folder on the other side"),
            });
        }
        self.act("rename", |w| {
            let sel = w.sel();
            if sel.len() > 1 {
                crate::rename::show(&w.ops, sel);
                return;
            }
            if w.current().is_some_and(|b| b.rename_inline()) {
                return;
            }
            if let Some(f) = sel.into_iter().next() {
                w.ops.rename(f);
            }
        });
        self.act("trash", |w| w.ops.trash(w.sel()));
        self.act("restore", |w| w.ops.restore(w.sel()));
        self.act("empty-trash", |w| w.ops.empty_trash());
        self.act("delete", |w| w.ops.delete(w.sel()));
        self.act("new-folder", |w| match w.current().and_then(|b| b.dir()) {
            Some(d) => w.ops.new_folder(d),
            None => w.ops.toast("Open a folder first"),
        });
        self.act("new-file", |w| match w.current().and_then(|b| b.dir()) {
            Some(d) => w.ops.new_file(d),
            None => w.ops.toast("Open a folder first"),
        });
        self.act("undo", |w| w.ops.undo());
        self.act("copy-path", |w| {
            let files = w.sel();
            let files = if files.is_empty() {
                w.current().and_then(|b| b.dir()).into_iter().collect()
            } else {
                files
            };
            let text = files
                .iter()
                .map(|f| f.path().map_or_else(|| f.uri().to_string(), |p| p.display().to_string()))
                .collect::<Vec<_>>()
                .join("\n");
            w.window.clipboard().set_text(&text);
            w.ops.toast("Path copied");
        });
        self.act("add-bookmark", |w| {
            // in a search, Ctrl+D saves the search instead
            if let Some(q) = w.current().and_then(|b| b.query()) {
                sidebar::save_search(&q);
                w.sidebar.refresh_searches();
                w.ops.toast("Search saved");
                return;
            }
            if let Some(d) = w.target_dir() {
                match sidebar::add_bookmark(&d) {
                    Ok(()) => w.sidebar.refresh_bookmarks(),
                    Err(e) => w.ops.toast(&e.to_string()),
                }
            }
        });
        self.act("terminal-panel", |w| w.term.toggle(w.current().and_then(|b| b.dir()).and_then(|d| d.path())));
        self.act("terminal", |w| {
            let Some(dir) = w.target_dir().and_then(|d| d.path()) else {
                w.ops.toast("Not a local folder");
                return;
            };
            let d = dir.display().to_string();
            let tries: [(&str, Vec<String>); 3] = [
                ("ptyxis", vec!["--new-window".into(), format!("--working-directory={d}")]),
                ("gnome-terminal", vec![format!("--working-directory={d}")]),
                ("xdg-terminal-exec", vec![]),
            ];
            if !tries
                .iter()
                .any(|(cmd, args)| std::process::Command::new(cmd).args(args).current_dir(&dir).spawn().is_ok())
            {
                w.ops.toast("No terminal found");
            }
        });
        self.act("context-menu", |w| w.current().into_iter().for_each(|b| b.context_menu()));
        self.act("other-pane", |w| {
            if w.panes[1].root.is_visible() {
                w.set_active(1 - w.active.get());
                w.current().into_iter().for_each(|b| b.focus_view());
            }
        });
        self.act("syntax", |w| {
            let d = adw::AlertDialog::new(Some("Search syntax"), None);
            let l = gtk::Label::builder()
                .label(SYNTAX)
                .css_classes(["monospace"])
                .xalign(0.0)
                .selectable(true)
                .build();
            d.set_extra_child(Some(&l));
            d.add_response("ok", "OK");
            d.present(Some(&w.window));
        });
        self.act("about", |w| {
            adw::AboutDialog::builder()
                .application_name("Kestrel")
                .application_icon("dev.kestrel.Kestrel")
                .version(env!("CARGO_PKG_VERSION"))
                .comments("Everything's instant search, Files' interface, for Linux.")
                .license_type(gtk::License::MitX11)
                .build()
                .present(Some(&w.window));
        });

        self.act("properties", |w| {
            let Some(b) = w.current() else { return };
            let infos = b.selected_infos();
            let ops = w.ops.clone();
            if !infos.is_empty() {
                crate::props::show(&w.window, infos, move |m| ops.toast(m));
                return;
            }
            // nothing selected: the folder itself, queried off the GTK thread
            let (Some(d), win) = (b.dir(), w.window.clone()) else { return };
            glib::spawn_future_local(async move {
                if let Ok(i) = d
                    .query_info_future(crate::browser::ATTRS, gio::FileQueryInfoFlags::NONE, glib::Priority::DEFAULT)
                    .await
                {
                    i.set_attribute_object("standard::file", &d);
                    crate::props::show(&win, vec![i], move |m| ops.toast(m));
                }
            });
        });
        self.act("palette", |w| crate::palette::show(&w.window));
        self.act("preferences", |w| {
            let (me, me2, ops) = (w.me.clone(), w.me.clone(), w.ops.clone());
            crate::prefs::show(
                &w.window,
                move || {
                    if let Some(w) = me.upgrade() {
                        w.set_hidden(crate::prefs::get().show_hidden);
                        // re-list so rows pick up display settings (time format)
                        w.panes.iter().flat_map(|p| p.browsers()).for_each(|b| b.reload());
                        w.refresh_preview();
                    }
                },
                move || {
                    if let Some(w) = me2.upgrade() {
                        w.panes.iter().flat_map(|p| p.browsers()).for_each(|b| b.apply_columns());
                    }
                },
                move |m| ops.toast(m),
            );
        });
        self.act("compress", |w| {
            let files = w.sel();
            let Some(dir) = files.first().and_then(|f| f.parent()) else { return };
            let default = match files.as_slice() {
                [one] => format!("{}.zip", one.basename().unwrap_or_default().display()),
                _ => "Archive.zip".into(),
            };
            let ops = w.ops.clone();
            w.ops
                .ask_name("Compress", "Create", &default, move |name| ops.compress(files.clone(), dir.clone(), name));
        });
        self.act("extract", |w| {
            for f in w.sel() {
                w.ops.extract(f, None);
            }
        });
        self.act("extract-to", |w| {
            let files = w.sel();
            let fd = gtk::FileDialog::builder().title("Extract To").accept_label("Extract Here").modal(true).build();
            if let Some(d) = w.current().and_then(|b| b.dir()).or_else(|| files.first().and_then(|f| f.parent())) {
                fd.set_initial_folder(Some(&d));
            }
            let ops = w.ops.clone();
            fd.select_folder(Some(&w.window), gio::Cancellable::NONE, move |r| {
                if let Ok(dest) = r {
                    for f in &files {
                        ops.extract(f.clone(), Some(dest.clone()));
                    }
                }
            });
        });
        self.act("browse-archive", |w| {
            if let Some(f) = w.sel().into_iter().next() {
                // GVfs' archive backend mounts it; Browser::show handles the mount
                w.navigate(crate::archive::archive_root(&f));
            }
        });
        let me = self.me.clone();
        let a = gio::SimpleAction::new("script", Some(glib::VariantTy::STRING));
        a.connect_activate(move |_, v| {
            let (Some(w), Some(script)) = (me.upgrade(), v.and_then(|v| v.get::<String>())) else {
                return;
            };
            let Some(dir) = w.current().and_then(|b| b.dir()) else { return };
            if let Err(e) = crate::scripts::run(std::path::Path::new(&script), &w.sel(), &dir) {
                w.ops.toast(&format!("Couldn't run the script: {e}"));
            }
        });
        self.window.add_action(&a);
        let me = self.me.clone();
        let a = gio::SimpleAction::new("tag", Some(glib::VariantTy::STRING));
        a.connect_activate(move |_, v| {
            let (Some(w), Some(t)) = (me.upgrade(), v.and_then(|v| v.get::<String>())) else {
                return;
            };
            if let Err(e) = crate::tags::toggle(&w.sel(), &t) {
                w.ops.toast(&format!("Couldn't tag: {e}"));
            }
        });
        self.window.add_action(&a);

        for (action, g) in [("chip-type", Group::Type), ("chip-date", Group::Date), ("chip-size", Group::Size)] {
            let me = self.me.clone();
            let a = gio::SimpleAction::new_stateful(action, Some(glib::VariantTy::STRING), &"".to_variant());
            a.connect_activate(move |_, v| {
                let (Some(w), Some(tok)) = (me.upgrade(), v.and_then(|v| v.get::<String>())) else {
                    return;
                };
                let q = query_ui::set_group(&w.bar.entry.text(), g, &tok);
                w.bar.entry.set_text(&q);
                w.bar.entry.set_position(-1);
            });
            self.window.add_action(&a);
        }
        // F6 / Shift+F6: jump between the three regions (search box, file list, sidebar)
        for (name, step) in [("focus-next-region", 1usize), ("focus-prev-region", 2)] {
            self.act(name, move |w| {
                let focus = GtkWindowExt::focus(&w.window);
                let inside = |r: &gtk::Widget| focus.as_ref().is_some_and(|f| f == r || f.is_ancestor(r));
                let regions: [gtk::Widget; 3] = [
                    w.bar.entry.clone().upcast(),
                    w.current().map_or_else(|| w.bar.entry.clone().upcast(), |b| b.root.clone().upcast()),
                    w.sidebar.root.clone().upcast(),
                ];
                let cur = regions.iter().position(inside).unwrap_or(0);
                match (cur + step) % 3 {
                    0 => {
                        w.focus_search(None);
                        w.show_suggestions();
                    }
                    1 => w.current().into_iter().for_each(|b| b.focus_view()),
                    _ => {
                        w.sidebar.root.child_focus(gtk::DirectionType::TabForward);
                    }
                }
            });
        }
        // Esc from the file list or filter bar (the search box handles its own Esc)
        self.act("clear-search", |w| {
            if w.current().is_some_and(|b| b.is_searching()) {
                w.bar.entry.set_text("");
                if let Some(b) = w.current() {
                    b.clear_search();
                    b.focus_view();
                }
            }
        });
        self.act("chip-clear", |w| {
            let mut q = w.bar.entry.text().to_string();
            for g in [Group::Type, Group::Date, Group::Size] {
                q = query_ui::set_group(&q, g, "");
            }
            w.bar.entry.set_text(&q);
        });

        // a checkbox in the menu; flipping it rewires folders + "Show in Folder" for this user
        let me = self.me.clone();
        let a = gio::SimpleAction::new_stateful("default-fm", None, &crate::filemanager1::is_default().to_variant());
        a.connect_activate(move |a, _| {
            let on = !a.state().and_then(|s| s.get::<bool>()).unwrap_or(false);
            let Some(w) = me.upgrade() else { return };
            match crate::filemanager1::make_default(on) {
                Ok(()) => {
                    a.set_state(&on.to_variant());
                    w.ops.toast(if on {
                        "Kestrel now opens folders and \u{201c}Show in Folder\u{201d}"
                    } else {
                        "Folders open in Files (Nautilus) again"
                    });
                }
                Err(e) => w.ops.toast(&format!("Couldn't change the default: {e}")),
            }
        });
        self.window.add_action(&a);

        // stateful
        let me = self.me.clone();
        let a = gio::SimpleAction::new_stateful("preview", None, &false.to_variant());
        a.connect_activate(move |a, _| {
            let on = !a.state().and_then(|s| s.get::<bool>()).unwrap_or(false);
            a.set_state(&on.to_variant());
            if let Some(w) = me.upgrade() {
                w.preview.root.set_visible(on);
                w.refresh_preview();
            }
        });
        self.window.add_action(&a);
        let me = self.me.clone();
        let a = gio::SimpleAction::new_stateful("select-mode", None, &false.to_variant());
        a.connect_activate(move |a, _| {
            let on = !a.state().and_then(|s| s.get::<bool>()).unwrap_or(false);
            a.set_state(&on.to_variant());
            if let Some(w) = me.upgrade() {
                w.select_mode.set(on);
                w.panes.iter().flat_map(|p| p.browsers()).for_each(|b| b.set_select_mode(on));
            }
        });
        self.window.add_action(&a);
        let me = self.me.clone();
        let a = gio::SimpleAction::new_stateful("show-hidden", None, &false.to_variant());
        a.connect_activate(move |a, _| {
            let on = !a.state().and_then(|s| s.get::<bool>()).unwrap_or(false);
            a.set_state(&on.to_variant());
            crate::prefs::update(|s| s.show_hidden = on);
            if let Some(w) = me.upgrade() {
                w.show_hidden.set(on);
                BROWSERS.with_borrow(|v| v.iter().for_each(|b| b.set_show_hidden(on)));
            }
        });
        self.window.add_action(&a);
        let me = self.me.clone();
        let a = gio::SimpleAction::new_stateful("dual", None, &false.to_variant());
        a.connect_activate(move |a, _| {
            let on = !a.state().and_then(|s| s.get::<bool>()).unwrap_or(false);
            me.upgrade().into_iter().for_each(|w| w.set_dual(on));
        });
        self.window.add_action(&a);
        let me = self.me.clone();
        let a = gio::SimpleAction::new_stateful("view", Some(glib::VariantTy::STRING), &"list".to_variant());
        a.connect_activate(move |a, v| {
            let Some(m) = v.and_then(|v| v.get::<String>()) else { return };
            a.set_state(&m.to_variant());
            if let Some(b) = me.upgrade().and_then(|w| w.current()) {
                b.set_mode(Mode::parse(&m));
                b.focus_view();
            }
        });
        self.window.add_action(&a);
    }
}
