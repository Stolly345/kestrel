//! The column (Miller) view: one list per folder level, side by side.

use super::*;

impl Browser {
    // ── column (Miller) view ──

    pub(super) fn miller_reset(&self, root: &gio::File) {
        while let Some(c) = self.miller.first_child() {
            self.miller.remove(&c);
        }
        *self.miller_sel.borrow_mut() = None;
        self.miller_add(root.clone(), 0);
    }

    pub(super) fn miller_add(&self, dir: gio::File, depth: usize) {
        self.miller_truncate(depth);
        let dl = gtk::DirectoryList::new(Some(ATTRS), Some(&dir));
        let filt = gtk::FilterListModel::new(Some(dl), Some(self.filter.clone()));
        let sorter = gtk::CustomSorter::new(|a, b| {
            let (a, b) = (info_of(a), info_of(b));
            cmp(
                (!is_dirlike(a), a.display_name().to_lowercase()),
                (!is_dirlike(b), b.display_name().to_lowercase()),
            )
        });
        let sorted = gtk::SortListModel::new(Some(filt), Some(sorter));
        let sel = gtk::SingleSelection::builder().model(&sorted).autoselect(false).can_unselect(true).build();
        sel.set_selected(gtk::INVALID_LIST_POSITION);
        let f = gtk::SignalListItemFactory::new();
        f.connect_setup(|_, item| {
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            row.append(&gtk::Image::builder().pixel_size(16).build());
            row.append(
                &gtk::Label::builder()
                    .xalign(0.0)
                    .hexpand(true)
                    .ellipsize(gtk::pango::EllipsizeMode::Middle)
                    .build(),
            );
            row.append(&gtk::Image::from_icon_name("go-next-symbolic"));
            item.downcast_ref::<gtk::ListItem>().unwrap().set_child(Some(&row));
        });
        f.connect_bind(|_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().unwrap();
            let (Some(row), Some(info)) = (item.child(), item.item().and_downcast::<gio::FileInfo>()) else {
                return;
            };
            let img = row.first_child().and_downcast::<gtk::Image>().unwrap();
            img.set_widget_name("");
            set_icon(&img, &info);
            img.set_pixel_size(16);
            crate::thumbs::load_into(&img, &info);
            img.next_sibling().and_downcast::<gtk::Label>().unwrap().set_text(&info.display_name());
            row.last_child().unwrap().set_opacity(if is_dirlike(&info) { 0.5 } else { 0.0 });
        });
        let lv = gtk::ListView::new(Some(sel.clone()), Some(f));
        lv.add_css_class("navigation-sidebar");
        let me = self.me.clone();
        sel.connect_selected_item_notify(move |s| {
            let (Some(b), Some(info)) = (me.upgrade(), s.selected_item().and_downcast::<gio::FileInfo>()) else {
                return;
            };
            let Some(f) = info_file(&info) else { return };
            *b.miller_sel.borrow_mut() = Some(info.clone());
            if is_dirlike(&info) {
                b.miller_add(nav_target(&info).unwrap_or(f), depth + 1);
            } else {
                b.miller_truncate(depth + 1);
            }
            b.notify_select();
        });
        let me = self.me.clone();
        lv.connect_activate(move |lv, pos| {
            let (Some(b), Some(info)) = (me.upgrade(), lv.model().and_then(|m| m.item(pos)).and_downcast::<gio::FileInfo>()) else {
                return;
            };
            b.open_info(&info);
        });
        let col = gtk::ScrolledWindow::builder()
            .child(&lv)
            .width_request(240)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .css_classes(["miller-col"])
            .build();
        self.miller.append(&col);
        // keep the newest column in view
        let sc = self.miller_scroll.clone();
        glib::idle_add_local_once(move || {
            let a = sc.hadjustment();
            a.set_value(a.upper() - a.page_size());
        });
    }

    /// Drop columns from `depth` rightwards.
    pub(super) fn miller_truncate(&self, depth: usize) {
        let mut c = self.miller.first_child();
        let mut i = 0;
        while let Some(w) = c {
            c = w.next_sibling();
            if i >= depth {
                self.miller.remove(&w);
            }
            i += 1;
        }
    }
}
