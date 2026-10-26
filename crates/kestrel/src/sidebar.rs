//! Places, bookmarks (shared with Nautilus via ~/.config/gtk-3.0/bookmarks) and devices.

use crate::browser::Loc;
use adw::prelude::*;
use gtk::gdk;
use gtk::{gio, glib};
use std::path::PathBuf;
use std::rc::Rc;

pub fn bookmarks_file() -> PathBuf {
    glib::user_config_dir().join("gtk-3.0/bookmarks")
}

/// (uri, label) pairs; label defaults to the folder name.
pub fn bookmarks() -> Vec<(String, String)> {
    std::fs::read_to_string(bookmarks_file())
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let (uri, name) = l.split_once(' ').unwrap_or((l, ""));
            let name = if name.is_empty() {
                gio::File::for_uri(uri)
                    .basename()
                    .map(|b| b.to_string_lossy().into_owned())
                    .unwrap_or_else(|| uri.into())
            } else {
                name.into()
            };
            (uri.to_string(), name)
        })
        .collect()
}

pub fn add_bookmark(f: &gio::File) -> std::io::Result<()> {
    let uri = f.uri().to_string();
    if bookmarks().iter().any(|(u, _)| *u == uri) {
        return Ok(());
    }
    let p = bookmarks_file();
    std::fs::create_dir_all(p.parent().unwrap())?;
    use std::io::Write;
    writeln!(std::fs::OpenOptions::new().create(true).append(true).open(p)?, "{uri}")
}

/// Saved searches and search history: one query per line under ~/.config/kestrel/.
pub fn read_lines(name: &str) -> Vec<String> {
    std::fs::read_to_string(glib::user_config_dir().join("kestrel").join(name))
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(String::from)
        .collect()
}

fn write_lines(name: &str, lines: &[String]) {
    let p = glib::user_config_dir().join("kestrel").join(name);
    let _ = std::fs::create_dir_all(p.parent().unwrap());
    let _ = std::fs::write(p, lines.join("\n") + "\n");
}

/// Most recent first, de-duplicated, capped.
pub fn remember_search(q: &str) {
    let q = q.trim();
    if q.is_empty() {
        return;
    }
    let mut h = read_lines("history");
    h.retain(|x| x != q);
    h.insert(0, q.to_string());
    h.truncate(50);
    write_lines("history", &h);
}

pub fn save_search(q: &str) {
    let mut s = read_lines("searches");
    if !s.iter().any(|x| x == q) {
        s.push(q.to_string());
        write_lines("searches", &s);
    }
}

pub fn forget_search(q: &str) {
    let mut s = read_lines("searches");
    s.retain(|x| x != q);
    write_lines("searches", &s);
}

/// Re-run icon-button labelling when a refresh finishes (on drop, after the rows are built).
struct Relabel(gtk::Widget);

impl Drop for Relabel {
    fn drop(&mut self) {
        crate::label_icon_buttons(&self.0);
    }
}

pub type DropFn = Rc<dyn Fn(gdk::DragAction, Vec<gio::File>, gio::File)>;

/// Let files be dropped onto a sidebar row (a folder, a drive, or the trash).
fn droppable(r: &gtk::ListBoxRow, target: gio::File, drop: &DropFn) {
    let dt = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY | gdk::DragAction::MOVE);
    let drop = drop.clone();
    dt.connect_drop(move |t, v, _, _| {
        let Ok(list) = v.get::<gdk::FileList>() else { return false };
        let actions = t.current_drop().map(|d| d.actions()).unwrap_or(gdk::DragAction::empty());
        drop(actions, list.files(), target.clone());
        true
    });
    r.add_controller(dt);
}

pub struct Sidebar {
    searches: gtk::ListBox,
    pub root: gtk::ScrolledWindow,
    /// URIs already listed under places; bookmarks repeating them are skipped.
    place_uris: Vec<String>,
    bookmarks: gtk::ListBox,
    devices: gtk::ListBox,
    navigate: Rc<dyn Fn(Loc)>,
    /// Files dropped on a row: (drag actions, files, target location).
    drop: DropFn,
    _monitor: gio::VolumeMonitor,
}

fn row(icon: &str, label: &str) -> (gtk::ListBoxRow, gtk::Box) {
    let b = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    b.append(&gtk::Image::from_icon_name(icon));
    b.append(
        &gtk::Label::builder()
            .label(label)
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build(),
    );
    let r = gtk::ListBoxRow::builder().child(&b).build();
    r.set_tooltip_text(Some(label));
    (r, b)
}

fn section(title: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(title)
        .xalign(0.0)
        .css_classes(["heading", "dim-label"])
        .margin_start(12)
        .margin_top(12)
        .margin_bottom(2)
        .build()
}

fn listbox() -> gtk::ListBox {
    gtk::ListBox::builder()
        .css_classes(["navigation-sidebar"])
        .selection_mode(gtk::SelectionMode::None)
        .build()
}

/// Rows carry their target (a URI) as the widget name.
fn wire(lb: &gtk::ListBox, nav: &Rc<dyn Fn(Loc)>) {
    let nav = nav.clone();
    lb.connect_row_activated(move |_, r| {
        let name = r.widget_name();
        if name.contains(':') {
            nav(Loc::Dir(gio::File::for_uri(&name)));
        }
    });
}

impl Sidebar {
    pub fn new(navigate: Rc<dyn Fn(Loc)>, drop: DropFn) -> Rc<Sidebar> {
        let col = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let places = listbox();
        let home = glib::home_dir();
        let mut items: Vec<(&str, String, PathBuf)> = vec![("user-home-symbolic", "Home".into(), home.clone())];
        for (icon, d) in [
            ("user-desktop-symbolic", glib::UserDirectory::Desktop),
            ("folder-documents-symbolic", glib::UserDirectory::Documents),
            ("folder-download-symbolic", glib::UserDirectory::Downloads),
            ("folder-music-symbolic", glib::UserDirectory::Music),
            ("folder-pictures-symbolic", glib::UserDirectory::Pictures),
            ("folder-videos-symbolic", glib::UserDirectory::Videos),
        ] {
            if let Some(p) = glib::user_special_dir(d).filter(|p| *p != home && p.is_dir()) {
                items.push((icon, p.file_name().unwrap().to_string_lossy().into_owned(), p));
            }
        }
        let mut place_uris = Vec::new();
        for (icon, label, p) in items {
            let (r, _) = row(icon, &label);
            let f = gio::File::for_path(p);
            let uri = f.uri().to_string();
            r.set_widget_name(&uri);
            droppable(&r, f, &drop);
            place_uris.push(uri);
            places.append(&r);
        }
        for (icon, label, uri) in [
            ("document-open-recent-symbolic", "Recent", "recent:///"),
            ("user-trash-symbolic", "Trash", "trash:///"),
            ("drive-harddisk-symbolic", "File System", "file:///"),
            ("network-workgroup-symbolic", "Network", "network:///"),
        ] {
            let (r, _) = row(icon, label);
            r.set_widget_name(uri);
            if uri.starts_with("trash:") || uri.starts_with("file:") {
                droppable(&r, gio::File::for_uri(uri), &drop);
            }
            places.append(&r);
        }
        wire(&places, &navigate);
        let bookmarks = listbox();
        wire(&bookmarks, &navigate);
        let searches = listbox();
        let nav = navigate.clone();
        searches.connect_row_activated(move |_, r| nav(Loc::Search(r.widget_name().trim_start_matches("search:").to_string())));
        let devices = listbox();
        // one row of colour dots: click one to list every file with that tag (an index search)
        let tags = gtk::Box::builder().spacing(0).margin_start(8).margin_end(8).margin_bottom(4).build();
        for t in kestrel_index::TAGS {
            let b = crate::tags::dot_button(t);
            b.set_tooltip_text(Some(&format!("Files tagged {t}")));
            let nav = navigate.clone();
            let q = format!("tag:{t}");
            b.connect_clicked(move |_| nav(Loc::Search(q.clone())));
            tags.append(&b);
        }

        // tags first, above Home
        col.append(&section("Tags"));
        col.append(&tags);
        col.append(&places);
        col.append(&section("Bookmarks"));
        col.append(&bookmarks);
        col.append(&section("Saved Searches"));
        col.append(&searches);
        col.append(&section("Devices"));
        col.append(&devices);
        let root = gtk::ScrolledWindow::builder()
            .child(&col)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .build();

        let monitor = gio::VolumeMonitor::get();
        let s = Rc::new(Sidebar {
            searches,
            root,
            place_uris,
            bookmarks,
            devices,
            navigate,
            drop,
            _monitor: monitor.clone(),
        });
        s.refresh_bookmarks();
        s.refresh_searches();
        s.refresh_devices();
        let w = Rc::downgrade(&s);
        let refresh = move || {
            if let Some(s) = w.upgrade() {
                s.refresh_devices();
            }
        };
        let r = refresh.clone();
        monitor.connect_mount_added(move |_, _| r());
        let r = refresh.clone();
        monitor.connect_mount_removed(move |_, _| r());
        let r = refresh.clone();
        monitor.connect_volume_added(move |_, _| r());
        monitor.connect_volume_removed(move |_, _| refresh());
        s
    }

    pub fn refresh_bookmarks(&self) {
        while let Some(c) = self.bookmarks.first_child() {
            self.bookmarks.remove(&c);
        }
        for (uri, name) in bookmarks().into_iter().filter(|(u, _)| !self.place_uris.contains(u)) {
            let (r, _) = row("folder-symbolic", &name);
            r.set_widget_name(&uri);
            droppable(&r, gio::File::for_uri(&uri), &self.drop);
            self.bookmarks.append(&r);
        }
    }

    pub fn refresh_searches(self: &Rc<Self>) {
        let _relabel = Relabel(self.searches.clone().upcast());
        while let Some(c) = self.searches.first_child() {
            self.searches.remove(&c);
        }
        for q in read_lines("searches") {
            let (r, b) = row("system-search-symbolic", &q);
            r.set_widget_name(&format!("search:{q}"));
            let btn = gtk::Button::builder()
                .icon_name("window-close-symbolic")
                .css_classes(["flat", "circular"])
                .tooltip_text("Forget this search")
                .build();
            let me = Rc::downgrade(self);
            btn.connect_clicked(move |_| {
                forget_search(&q);
                if let Some(s) = me.upgrade() {
                    s.refresh_searches();
                }
            });
            b.append(&btn);
            self.searches.append(&r);
        }
    }

    fn refresh_devices(&self) {
        let _relabel = Relabel(self.devices.clone().upcast());
        while let Some(c) = self.devices.first_child() {
            self.devices.remove(&c);
        }
        let monitor = gio::VolumeMonitor::get();
        for m in monitor.mounts() {
            if m.is_shadowed() {
                continue;
            }
            let (r, b) = row("drive-removable-media-symbolic", &m.name());
            if let Some(img) = b.first_child().and_downcast::<gtk::Image>() {
                img.set_from_gicon(&m.symbolic_icon());
            }
            if m.can_unmount() || m.can_eject() {
                let btn = gtk::Button::builder()
                    .icon_name("media-eject-symbolic")
                    .css_classes(["flat", "circular"])
                    .tooltip_text("Eject")
                    .build();
                let m2 = m.clone();
                btn.connect_clicked(move |btn| {
                    let op = gtk::MountOperation::new(btn.root().and_downcast::<gtk::Window>().as_ref());
                    let m = m2.clone();
                    glib::spawn_future_local(async move {
                        let _ = if m.can_eject() {
                            m.eject_with_operation_future(gio::MountUnmountFlags::NONE, Some(&op)).await
                        } else {
                            m.unmount_with_operation_future(gio::MountUnmountFlags::NONE, Some(&op)).await
                        };
                    });
                });
                b.append(&btn);
            }
            let nav = self.navigate.clone();
            let root = m.root();
            let g = gtk::GestureClick::new();
            droppable(&r, root.clone(), &self.drop);
            g.connect_released(move |_, _, _, _| nav(Loc::Dir(root.clone())));
            r.add_controller(g);
            self.devices.append(&r);
        }
        // plugged in but not mounted yet: click mounts, then opens
        for v in monitor.volumes() {
            if v.get_mount().is_some() || !v.can_mount() {
                continue;
            }
            let (r, b) = row("drive-harddisk-symbolic", &v.name());
            if let Some(img) = b.first_child().and_downcast::<gtk::Image>() {
                img.set_from_gicon(&v.symbolic_icon());
            }
            b.set_opacity(0.7);
            let nav = self.navigate.clone();
            let g = gtk::GestureClick::new();
            g.connect_released(move |g, _, _, _| {
                let op = gtk::MountOperation::new(g.widget().and_then(|w| w.root()).and_downcast::<gtk::Window>().as_ref());
                let (v, nav) = (v.clone(), nav.clone());
                glib::spawn_future_local(async move {
                    if v.mount_future(gio::MountMountFlags::NONE, Some(&op)).await.is_ok()
                        && let Some(m) = v.get_mount()
                    {
                        nav(Loc::Dir(m.root()));
                    }
                });
            });
            r.add_controller(g);
            self.devices.append(&r);
        }
    }
}
