//! Search mode of a tab: entering and leaving it, running queries against the index, scope.

use super::*;

impl Browser {
    // ── search ──

    /// Show results for `text` (empty clears the search). Called on every keystroke.
    pub fn set_search(&self, text: &str) {
        let text = text.trim();
        if text.is_empty() {
            self.clear_search();
            return;
        }
        if !self.is_searching() {
            // park the folder view: remember where it was scrolled
            self.saved_scroll
                .set((self.list_scroll.vadjustment().value(), self.grid_scroll.vadjustment().value()));
            self.list.set_model(Some(&self.search_sel));
            self.grid.set_model(Some(&self.search_sel));
            self.dir_col.set_visible(true);
            self.git_col.set_visible(false);
            // results start in Best Match order: no column shows as sorted until one is clicked
            if let Some(s) = self.list.sorter().and_downcast::<gtk::ColumnViewSorter>() {
                *self.folder_sort.borrow_mut() = Some((s.primary_sort_column(), s.primary_sort_order()));
                self.sort_quietly(None, gtk::SortType::Ascending);
            }
        }
        *self.query.borrow_mut() = Some(text.to_string());
        *self.hl.borrow_mut() = crate::query_ui::highlight_terms(text);
        self.update_apps();
        self.run_search();
    }

    pub fn set_scope(&self, s: Scope) {
        if self.scope.replace(s) != s && self.is_searching() {
            self.update_apps();
            self.run_search();
        }
        self.update_locbar();
    }

    /// Esc: back to the folder exactly as it was.
    pub fn clear_search(&self) {
        self.end_search(true);
        self.show_folder_state();
        self.changed();
    }

    pub(super) fn end_search(&self, restore_scroll: bool) {
        if !self.is_searching() {
            return;
        }
        *self.query.borrow_mut() = None;
        self.search.cancel();
        self.update_apps();
        self.list.set_model(Some(&self.dir_sel));
        self.grid.set_model(Some(&self.dir_sel));
        self.dir_col.set_visible(false);
        self.git_col.set_visible(self.git.borrow().is_some());
        if let Some((col, order)) = self.folder_sort.take() {
            self.sort_quietly(col.as_ref(), order);
        }
        if restore_scroll {
            let (l, g) = self.saved_scroll.get();
            let (ls, gs) = (self.list_scroll.clone(), self.grid_scroll.clone());
            glib::idle_add_local_once(move || {
                ls.vadjustment().set_value(l);
                gs.vadjustment().set_value(g);
            });
        }
        self.update_locbar();
        self.update_status();
    }

    /// The query the daemon sees: the user's text plus the scope term.
    pub(super) fn effective_query(&self) -> Option<String> {
        let q = crate::tags::resolve_query(&self.query()?);
        Some(match self.scope.get().term(&self.dir.borrow()) {
            Some(t) => format!("{q} {t}"),
            None => q,
        })
    }

    pub(super) fn run_search(&self) {
        let Some(q) = self.effective_query() else { return };
        *self.search_state.borrow_mut() = SearchState::Running;
        self.update_status();
        self.update_locbar();
        self.changed();
        let (sort, desc) = self.sort_key();
        let prefer = if sort == Sort::Relevance {
            self.query().map(|u| crate::picks::prefer(&u)).unwrap_or_default()
        } else {
            Vec::new()
        };
        let me = self.me.clone();
        self.search.run(&q, sort, desc, prefer, move |r| {
            let Some(b) = me.upgrade() else { return };
            if !b.is_searching() {
                return;
            }
            match r {
                Ok(o) => {
                    *b.search_state.borrow_mut() = SearchState::Done(o);
                    if o.total == 0 {
                        let hint = if b.scope.get() == Scope::Everywhere {
                            "Try fewer words, or see Search Syntax in the menu."
                        } else {
                            "Nothing here. Switch the scope to Everywhere to search the whole disk."
                        };
                        b.show_status("system-search-symbolic", "No results", hint);
                    } else {
                        b.show_view();
                    }
                    b.apply_pending_select();
                }
                // keep the last good results on screen; the status bar shows what's wrong
                Err(SearchError::Query(e)) => *b.search_state.borrow_mut() = SearchState::BadQuery(e),
                Err(SearchError::Indexing) => {
                    b.show_status(
                        "content-loading-symbolic",
                        "Indexing your files…",
                        "The search service just started and is reading your disks. Results will appear here on their own.",
                    );
                    *b.search_state.borrow_mut() = SearchState::Indexing;
                    // try again shortly, unless the user has moved on
                    let (me, q) = (b.me.clone(), b.query());
                    glib::timeout_add_local_once(std::time::Duration::from_millis(1500), move || {
                        if let Some(b) = me.upgrade()
                            && b.query() == q
                        {
                            b.run_search();
                        }
                    });
                }
                Err(SearchError::Down(_)) => {
                    b.show_status(
                        "network-offline-symbolic",
                        "Search is off",
                        "Kestrel's search service isn't running, so nothing can be found yet.",
                    );
                    b.status.set_child(Some(
                        &gtk::Button::builder()
                            .label("Start Search Service")
                            .action_name("win.start-service")
                            .halign(gtk::Align::Center)
                            .css_classes(["pill", "suggested-action"])
                            .build(),
                    ));
                    *b.search_state.borrow_mut() = SearchState::Down;
                }
            }
            b.update_status();
        });
    }

    /// The Apps row above whole-disk results: up to four installed apps whose name, a word of
    /// it, its initials or its keywords fit the query (the quick window's matcher), ones you've
    /// opened for this query first. Hidden for folder-scoped searches and for queries that use
    /// the search syntax, which are about files.
    pub(super) fn update_apps(&self) {
        let bar = &self.apps_bar;
        while let Some(c) = bar.first_child() {
            bar.remove(&c);
        }
        let q = self.query().unwrap_or_default();
        let wanted = self.scope.get() == Scope::Everywhere && crate::launcher::looks_like_app_query(&q);
        let rows = if wanted {
            let bonus: std::collections::HashMap<String, i32> = crate::picks::prefer(&q).into_iter().collect();
            let bonus = |k: &str| bonus.get(k).copied().unwrap_or(0) / 10;
            crate::launcher::apps(&q.trim().to_lowercase(), &crate::launcher::installed_apps(), &bonus, 4)
        } else {
            Vec::new()
        };
        bar.set_visible(!rows.is_empty());
        if rows.is_empty() {
            return;
        }
        bar.append(&gtk::Label::builder().label("Apps").css_classes(["app-strip-label"]).build());
        for r in rows {
            let crate::launcher::Act::App(info) = r.act else { continue };
            let inner = gtk::Box::builder().spacing(8).build();
            inner.append(&gtk::Image::builder().gicon(&r.icon).pixel_size(24).build());
            inner.append(&gtk::Label::builder().label(&r.title).ellipsize(gtk::pango::EllipsizeMode::End).max_width_chars(24).build());
            let b = gtk::Button::builder()
                .child(&inner)
                .tooltip_text(&r.subtitle)
                .css_classes(["flat", "app-chip"])
                .build();
            b.update_property(&[
                gtk::accessible::Property::Label(&format!("Open {}", r.title)),
                gtk::accessible::Property::Description(&r.subtitle),
            ]);
            let me = self.me.clone();
            b.connect_clicked(move |b| {
                let Some(br) = me.upgrade() else { return };
                if let Some(q) = br.query() {
                    crate::picks::record(&q, &crate::launcher::app_key(&info));
                }
                let ctx = gtk::prelude::WidgetExt::display(b).app_launch_context();
                if let Err(e) = info.launch(&[], Some(&ctx)) {
                    br.statusbar.set_label(&format!("Couldn't start {}: {}", info.name(), e.message()));
                }
            });
            bar.append(&b);
        }
    }

    fn sort_quietly(&self, col: Option<&gtk::ColumnViewColumn>, order: gtk::SortType) {
        self.sorting_quietly.set(true);
        self.list.sort_by_column(col, order);
        self.sorting_quietly.set(false);
    }

    /// The column clicked, or Best Match when none is.
    pub(super) fn sort_key(&self) -> (Sort, bool) {
        let Some(s) = self.list.sorter().and_downcast::<gtk::ColumnViewSorter>() else {
            return (Sort::Relevance, false);
        };
        let key = s
            .primary_sort_column()
            .and_then(|c| self.cols.iter().find(|(k, _)| *k == c).map(|(_, s)| *s))
            .unwrap_or(Sort::Relevance);
        (key, s.primary_sort_order() == gtk::SortType::Descending)
    }
}
