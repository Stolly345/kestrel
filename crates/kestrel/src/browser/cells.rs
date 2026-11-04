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

    w.add_css_class("drag-item"); // see drag_beats_rubberband
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
        let actions = crate::fileops::drop_intent(t);
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
        let child = match (view, bw.upgrade()) {
            (Some(v), Some(b)) => {
                folder_drop_target(&w, item, &bw);
                with_check(&b, &w, item, v == 1)
            }
            _ => w,
        };
        item.set_child(Some(&child));
    });
    let bw = b.clone();
    f.connect_bind(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        if let (Some(v), Some(b)) = (view, bw.upgrade()) {
            b.rows.borrow_mut().insert((item.position(), v), item.downgrade());
        }
        let w = item.child().map(|w| if view.is_some() { unwrap_check(&w) } else { w });
        if let (Some(w), Some(info)) = (w, item.item().and_downcast::<gio::FileInfo>()) {
            bind(&w, &info);
        }
    });
    f
}

/// Select mode's checkbox: in front of a row, or on a tile's corner. It mirrors the item's selection,
/// and ticking it adds the item to the selection (or takes it out) without touching the others.
fn with_check(b: &Browser, w: &gtk::Widget, item: &gtk::ListItem, tile: bool) -> gtk::Widget {
    let check = gtk::CheckButton::builder()
        .visible(b.select_mode.get())
        .halign(gtk::Align::Start)
        .valign(if tile { gtk::Align::Start } else { gtk::Align::Center })
        .css_classes(["select-check"])
        .build();
    check.update_property(&[gtk::accessible::Property::Label("Select")]);
    item.bind_property("selected", &check, "active").sync_create().build();
    let (iw, bw) = (item.downgrade(), b.me.clone());
    check.connect_toggled(move |c| {
        let (Some(it), Some(b)) = (iw.upgrade(), bw.upgrade()) else { return };
        if c.is_active() != it.is_selected() {
            if c.is_active() {
                b.sel().select_item(it.position(), false);
            } else {
                b.sel().unselect_item(it.position());
            }
        }
    });
    // the row selects on press (replacing the selection): claim the press first and toggle here
    let g = gtk::GestureClick::new();
    g.connect_pressed(|g, _, _, _| {
        g.set_state(gtk::EventSequenceState::Claimed);
        if let Some(c) = g.widget().and_downcast::<gtk::CheckButton>() {
            c.set_active(!c.is_active());
        }
    });
    check.add_controller(g);
    b.checks.borrow_mut().push(check.downgrade());
    if tile {
        let o = gtk::Overlay::builder().child(w).build();
        o.add_overlay(&check);
        o.upcast()
    } else {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        row.append(&check);
        row.append(w);
        row.upcast()
    }
}

/// The view's own widget inside with_check's wrapper.
fn unwrap_check(w: &gtk::Widget) -> gtk::Widget {
    match w.downcast_ref::<gtk::Overlay>() {
        Some(o) => o.child().unwrap(),
        None => w.last_child().unwrap(),
    }
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

/// A folder's total bytes from the index, set on its FileInfo once fetched; NO_SIZE while the
/// query runs or when the index doesn't cover the folder (so it's asked once per listing).
const DIR_BYTES: &str = "kestrel::dir-bytes";
const NO_SIZE: u64 = u64::MAX;

/// Size column value: file size, or a folder's total once known (for sorting).
pub(crate) fn size_of(info: &gio::FileInfo) -> i64 {
    match info.has_attribute(DIR_BYTES).then(|| info.attribute_uint64(DIR_BYTES)) {
        Some(b) if b != NO_SIZE => b as i64,
        _ if is_dirlike(info) => 0,
        _ => info.size(),
    }
}

/// The Size cell. Folders ask kestrel-indexd for their total (0.1-0.3 s, off the GTK thread) the
/// first time they're shown; the label is filled in if it still shows that folder when the answer comes.
pub(crate) fn bind_size(l: &gtk::Label, info: &gio::FileInfo) {
    unsafe { l.set_data("kestrel-size-info", info.clone()) };
    let text = |info: &gio::FileInfo| {
        let known = !is_dirlike(info) || info.has_attribute(DIR_BYTES) && info.attribute_uint64(DIR_BYTES) != NO_SIZE;
        if known { glib::format_size(size_of(info) as u64).to_string() } else { String::new() }
    };
    l.set_text(&text(info));
    if !is_dirlike(info) || info.has_attribute(DIR_BYTES) {
        return;
    }
    // non-native = GVfs (smb://, sftp://): not indexed, and the empty-check could block on a dead share
    let Some(p) = info_file(info).filter(|f| f.is_native()).and_then(|f| f.path()) else { return };
    info.set_attribute_uint64(DIR_BYTES, NO_SIZE);
    let (lw, info) = (l.downgrade(), info.clone());
    glib::spawn_future_local(async move {
        if let Ok(Ok((_, b))) = gio::spawn_blocking(move || crate::search::folder_stats(&p)).await {
            info.set_attribute_uint64(DIR_BYTES, b);
        }
        let Some(l) = lw.upgrade() else { return };
        let shown = unsafe { l.data::<gio::FileInfo>("kestrel-size-info").map(|p| p.as_ref().clone()) };
        if shown.as_ref() == Some(&info) {
            l.set_text(&text(&info));
        }
    });
}

/// ", links to /target" for a symlink, ", 3 hard links" for a file with other names.
pub(crate) fn link_note(info: &gio::FileInfo) -> String {
    if let Some(t) = link_target(info) {
        return format!(", links to {}", t.display());
    }
    let n = info.attribute_uint32("unix::nlink");
    if n > 1 && info.file_type() == gio::FileType::Regular {
        format!(", {n} hard links")
    } else {
        String::new()
    }
}

/// Thumbnail if one is cached (ours or Nautilus'), else the type icon.
pub fn set_icon(img: &gtk::Image, info: &gio::FileInfo) {
    if info.boolean("thumbnail::is-valid")
        && let Some(p) = info.attribute_byte_string("thumbnail::path")
    {
        crate::thumbs::show(img, std::path::Path::new(p.as_str()));
        return;
    }
    match info.icon() {
        Some(i) => img.set_from_gicon(&i),
        None => img.set_icon_name(Some("text-x-generic")),
    }
}
