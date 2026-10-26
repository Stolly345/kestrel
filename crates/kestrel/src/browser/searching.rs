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
        }
        *self.query.borrow_mut() = Some(text.to_string());
        *self.hl.borrow_mut() = crate::query_ui::highlight_terms(text);
        self.run_search();
    }

    pub fn set_scope(&self, s: Scope) {
        if self.scope.replace(s) != s && self.is_searching() {
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
        self.list.set_model(Some(&self.dir_sel));
        self.grid.set_model(Some(&self.dir_sel));
        self.dir_col.set_visible(false);
        self.git_col.set_visible(self.git.borrow().is_some());
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
        let q = self.query()?;
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
        let me = self.me.clone();
        self.search.run(&q, sort, desc, move |r| {
            let Some(b) = me.upgrade() else { return };
            if !b.is_searching() {
                return;
            }
            let daemon_ok = !matches!(r, Err(SearchError::Down(_)));
            match r {
                Ok(o) => {
                    *b.search_state.borrow_mut() = SearchState::Done(o);
                    if o.total == 0 {
                        let hint = if b.scope.get() == Scope::Everywhere {
                            "Try fewer words, or open the ? in the search box for syntax."
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
                Err(SearchError::Down(e)) => {
                    b.show_status(
                        "network-offline-symbolic",
                        "Search is off",
                        "Kestrel's search service isn't running. Press Start Service above to turn it on.",
                    );
                    *b.search_state.borrow_mut() = SearchState::Down(e);
                }
            }
            b.update_status();
            let cb = b.on_daemon.borrow().clone();
            if let Some(cb) = cb {
                cb(daemon_ok);
            }
        });
    }

    pub(super) fn sort_key(&self) -> (Sort, bool) {
        let Some(s) = self.list.sorter().and_downcast::<gtk::ColumnViewSorter>() else {
            return (Sort::Name, false);
        };
        let key = s
            .primary_sort_column()
            .and_then(|c| self.cols.iter().find(|(k, _)| *k == c).map(|(_, s)| *s))
            .unwrap_or(Sort::Name);
        (key, s.primary_sort_order() == gtk::SortType::Descending)
    }
}
