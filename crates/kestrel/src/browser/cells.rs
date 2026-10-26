//! List and grid cells: the item factories, their gestures, drop targets and icons.

use super::*;

/// Right-click and drag-out on one row/tile. `item` is that row's ListItem.
pub(crate) fn row_gestures(w: &gtk::Widget, item: &gtk::ListItem, b: &Weak<Browser>) {
    let g = gtk::GestureClick::builder().button(3).build();
    let (bw, iw) = (b.clone(), item.downgrade());
    g.connect_pressed(move |g, _, x, y| {
        g.set_state(gtk::EventSequenceState::Claimed);
        let (Some(b), Some(it)) = (bw.upgrade(), iw.upgrade()) else { return };
        let pos = it.position();
        if !b.sel().is_selected(pos) {
            b.sel().select_item(pos, true);
        }
        b.popup(&b.item_menu, &g.widget().unwrap(), x, y);
    });
    w.add_controller(g);

    let ds = gtk::DragSource::builder().actions(gdk::DragAction::COPY | gdk::DragAction::MOVE).build();
    let (bw, iw) = (b.clone(), item.downgrade());
    ds.connect_prepare(move |_, _, _| {
        let (b, it) = (bw.upgrade()?, iw.upgrade()?);
        let pos = it.position();
        if !b.sel().is_selected(pos) {
            b.sel().select_item(pos, true);
        }
        Some(gdk::ContentProvider::for_value(&gdk::FileList::from_array(&b.selected_files()).to_value()))
    });
    w.add_controller(ds);
}

/// Drop files onto a folder row/tile: into that folder. Other rows decline, so the drop falls
/// through to the view (the folder being shown).
pub(crate) fn folder_drop_target(w: &gtk::Widget, item: &gtk::ListItem, b: &Weak<Browser>) {
    let dt = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY | gdk::DragAction::MOVE);
    let iw = item.downgrade();
    dt.connect_accept(move |_, _| {
        iw.upgrade()
            .and_then(|it| it.item())
            .and_downcast::<gio::FileInfo>()
            .is_some_and(|i| is_dirlike(&i))
    });
    let (bw, iw) = (b.clone(), item.downgrade());
    dt.connect_drop(move |t, v, _, _| {
        let (Some(b), Some(it), Ok(list)) = (bw.upgrade(), iw.upgrade(), v.get::<gdk::FileList>()) else {
            return false;
        };
        let Some(dest) = it.item().and_downcast::<gio::FileInfo>().and_then(|i| nav_target(&i)) else {
            return false;
        };
        let files = list.files();
        let actions = t.current_drop().map(|d| d.actions()).unwrap_or(gdk::DragAction::empty());
        b.ops.drop_files(actions, files, dest);
        true
    });
    w.add_controller(dt);
}

/// `view`: Some(0) list names / Some(1) grid tiles get tracked (rename popup anchor) and accept
/// folder drops; other columns don't.
pub(crate) fn factory(
    b: &Weak<Browser>,
    view: Option<u8>,
    setup: impl Fn() -> gtk::Widget + 'static,
    bind: impl Fn(&gtk::Widget, &gio::FileInfo) + 'static,
) -> gtk::SignalListItemFactory {
    let f = gtk::SignalListItemFactory::new();
    let bw = b.clone();
    f.connect_setup(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let w = setup();
        row_gestures(&w, item, &bw);
        if view.is_some() {
            folder_drop_target(&w, item, &bw);
        }
        item.set_child(Some(&w));
    });
    let bw = b.clone();
    f.connect_bind(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        if let (Some(v), Some(b)) = (view, bw.upgrade()) {
            b.rows.borrow_mut().insert((item.position(), v), item.downgrade());
        }
        if let (Some(w), Some(info)) = (item.child(), item.item().and_downcast::<gio::FileInfo>()) {
            bind(&w, &info);
        }
    });
    f
}

pub(crate) fn label_factory(b: &Weak<Browser>, xalign: f32, text: impl Fn(&gio::FileInfo) -> String + 'static) -> gtk::SignalListItemFactory {
    factory(
        b,
        None,
        move || {
            gtk::Label::builder()
                .xalign(xalign)
                .ellipsize(gtk::pango::EllipsizeMode::Middle)
                .css_classes(["dim-label"])
                .build()
                .upcast()
        },
        move |w, info| w.downcast_ref::<gtk::Label>().unwrap().set_text(&text(info)),
    )
}

/// " · → /target" for a symlink, " · 3 hard links" for a file with other names.
pub(crate) fn link_note(info: &gio::FileInfo) -> String {
    if let Some(t) = link_target(info) {
        return format!(" · \u{2192} {}", t.display());
    }
    let n = info.attribute_uint32("unix::nlink");
    if n > 1 && info.file_type() == gio::FileType::Regular {
        format!(" · {n} hard links")
    } else {
        String::new()
    }
}

/// Thumbnail if one is cached (ours or Nautilus'), else the type icon.
pub fn set_icon(img: &gtk::Image, info: &gio::FileInfo) {
    if info.boolean("thumbnail::is-valid")
        && let Some(p) = info.attribute_byte_string("thumbnail::path")
    {
        img.set_from_file(Some(p.as_str()));
        return;
    }
    match info.icon() {
        Some(i) => img.set_from_gicon(&i),
        None => img.set_icon_name(Some("text-x-generic")),
    }
}
