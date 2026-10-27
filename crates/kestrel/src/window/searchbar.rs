//! The header search box: suggestions, filter chips, and running the query in the current tab.

use super::*;

impl Win {
    pub(super) fn set_entry(&self, text: &str) {
        self.bar.setting.set(true);
        self.bar.entry.set_text(text);
        self.bar.entry.set_position(-1);
        self.bar.setting.set(false);
    }

    /// Put the cursor in the search box (launch, Ctrl+F, typing in a file list).
    pub fn focus_search(&self, append: Option<&str>) {
        self.bar.entry.grab_focus();
        if let Some(t) = append {
            let pos = self.bar.entry.text().chars().count() as i32;
            let mut p = pos;
            self.bar.entry.insert_text(t, &mut p);
        }
        self.bar.entry.set_position(-1);
    }

    /// Under the search box: recent searches when it's empty, completions while typing a modifier.
    pub(super) fn show_suggestions(&self) {
        let e = &self.bar.entry;
        let text = e.text().to_string();
        // complete the last word; the cursor position isn't updated yet when `changed` fires
        let at_end = true;
        let (title, rows): (&str, Vec<(String, String, String)>) = if text.is_empty() {
            let h = sidebar::read_lines("history");
            ("Recent searches", h.into_iter().take(10).map(|q| (q.clone(), q, String::new())).collect())
        } else if at_end {
            let s = query_ui::complete(&text);
            (
                "Tab to complete",
                s.into_iter()
                    .take(10)
                    .map(|s| (query_ui::apply(&text, &s.insert), s.label, s.detail.to_string()))
                    .collect(),
            )
        } else {
            ("", vec![])
        };
        *self.bar.suggestions.borrow_mut() = if text.is_empty() {
            vec![]
        } else {
            rows.iter().map(|r| r.0.clone()).collect()
        };
        // focus sits on the GtkText inside the SearchEntry (has_focus() is false), and FOCUS_WITHIN
        // isn't set yet when focus was given at launch, so ask the window what has focus
        let focused = e.root().and_then(|r| r.focus()).is_some_and(|f| f.is_ancestor(e));
        if rows.is_empty() || !focused {
            self.bar.history.popdown();
            return;
        }
        let list = gtk::ListBox::builder().css_classes(["navigation-sidebar"]).build();
        for (full, label, detail) in &rows {
            let b = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            b.append(&gtk::Label::builder().label(label).xalign(0.0).hexpand(true).css_classes(["monospace"]).build());
            if !detail.is_empty() {
                b.append(&gtk::Label::builder().label(detail).css_classes(["dim-label", "caption"]).build());
            }
            let r = gtk::ListBoxRow::builder().child(&b).build();
            r.set_widget_name(full);
            list.append(&r);
        }
        let me = self.me.clone();
        let completing = !text.is_empty();
        list.connect_row_activated(move |_, r| {
            if let Some(w) = me.upgrade() {
                w.take_suggestion(&r.widget_name(), !completing);
            }
        });
        let c = gtk::Box::new(gtk::Orientation::Vertical, 4);
        c.append(
            &gtk::Label::builder()
                .label(title)
                .xalign(0.0)
                .css_classes(["dim-label", "caption-heading"])
                .margin_start(8)
                .build(),
        );
        c.append(&list);
        self.bar.history.set_child(Some(&c));
        self.bar.history.popup();
    }

    /// Put a suggestion in the box; recent searches run straight away, completions keep you typing.
    pub(super) fn take_suggestion(&self, full: &str, run: bool) {
        self.bar.history.popdown();
        let e = &self.bar.entry;
        e.set_text(full);
        e.grab_focus();
        e.set_position(-1);
        if run {
            self.commit_search();
        }
    }

    /// Chip labels/state from the query text; chips show while the current tab is searching.
    pub(super) fn sync_chips(&self) {
        let text = self.bar.entry.text();
        for (g, b) in &self.bar.chip_btns {
            let (label, active) = query_ui::chip_label(&text, *g);
            b.set_label(&label);
            if active {
                b.add_css_class("chip-active");
            } else {
                b.remove_css_class("chip-active");
            }
            let action = match g {
                Group::Type => "chip-type",
                Group::Date => "chip-date",
                Group::Size => "chip-size",
            };
            if let Some(a) = self.window.lookup_action(action).and_downcast::<gio::SimpleAction>() {
                a.set_state(&query_ui::current(&text, *g).unwrap_or_default().to_variant());
            }
        }
        let searching = self.current().is_some_and(|b| b.is_searching()) || !text.is_empty();
        self.bar.chips.set_reveal_child(searching);
    }

    /// Every keystroke (after the entry's 200 ms debounce).
    pub(super) fn on_search_changed(&self) {
        if let Some(b) = self.current() {
            b.set_search(&self.bar.entry.text());
        }
    }

    /// Enter: remember the query and move into the results.
    pub(super) fn commit_search(&self) {
        if let Some(id) = self.bar.debounce.take() {
            id.remove();
        }
        let text = self.bar.entry.text().to_string();
        if let Some(b) = self.current() {
            b.set_search(&text);
            if !text.trim().is_empty() {
                sidebar::remember_search(text.trim());
                b.focus_view();
            }
        }
    }
}
