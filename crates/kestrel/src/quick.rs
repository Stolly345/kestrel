//! Quick search (`kestrel --quick`, bound to a shortcut in Preferences): a small window over
//! whatever you're doing, like a launcher. Apps, files from the whole-disk index, GNOME Settings
//! pages, sums, web searches and system actions in one Best Match list; pinned and recent items
//! before you type. Enter opens, Esc clears then hides. It runs as its own single-instance app
//! that stays in the background after the first use, so the shortcut opens it instantly.

use crate::launcher::{self, Act, Row};
use adw::prelude::*;
use gtk::{gdk, gio, glib};
use kestrel_index::Sort;
use kestrel_index::proto::Request;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

pub const APP_ID: &str = "dev.kestrel.Kestrel.Quick";
const FILES: usize = 40;
const SHOWN: i32 = 8;


struct Quick {
    win: adw::ApplicationWindow,
    entry: gtk::Entry,
    list: gtk::ListBox,
    scroll: gtk::ScrolledWindow,
    hint: gtk::Label,
    rows: RefCell<Vec<Row>>,
    apps: RefCell<Vec<launcher::App>>,
    pages: Vec<launcher::App>,
    generation: Cell<u64>,
    /// The system action waiting for a second Enter.
    armed: RefCell<Option<&'static str>>,
    debounce: RefCell<Option<glib::SourceId>>,
    /// Don't hide when focus goes elsewhere (headless screenshots).
    sticky: Cell<bool>,
}

pub fn main() -> glib::ExitCode {
    let headless = std::env::args().any(|a| a == "--screenshot");
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(if headless {
            gio::ApplicationFlags::NON_UNIQUE
        } else {
            gio::ApplicationFlags::empty()
        })
        .build();
    let quick: Rc<RefCell<Option<Rc<Quick>>>> = Rc::default();
    app.connect_startup(|app| {
        gtk::Window::set_default_icon_name("dev.kestrel.Kestrel");
        crate::prefs::apply_color_scheme(&crate::prefs::get());
        crate::install_styles();
        // stay running between uses: the next press of the shortcut shows the window at once
        std::mem::forget(app.hold());
    });
    // headless checks: kestrel --quick --type TEXT --screenshot OUT.png (see scripts/ui-check.sh)
    let args: Vec<String> = std::env::args().skip(2).collect();
    let flag = |f: &str| args.iter().position(|a| a == f).and_then(|i| args.get(i + 1)).cloned();
    let (typed, shot) = (flag("--type"), flag("--screenshot"));
    app.connect_activate(move |app| {
        let q = quick.borrow_mut().get_or_insert_with(|| build(app)).clone();
        if q.win.is_visible() && q.win.is_active() {
            hide(&q);
        } else {
            show(&q);
        }
        if let Some(t) = &typed {
            q.entry.set_text(t);
        }
        if let Some(path) = shot.clone() {
            q.sticky.set(true);
            if let Some(s) = gtk::Settings::default() {
                s.set_gtk_enable_animations(false);
            }
            let q = q.clone();
            let _ = std::fs::remove_file(&path);
            let tries = Cell::new(0);
            // Broadway sometimes has no renderer for a frame or two: try again until it draws
            glib::timeout_add_local(std::time::Duration::from_millis(400), move || {
                tries.set(tries.get() + 1);
                if tries.get() < 3 {
                    return glib::ControlFlow::Continue; // let files arrive first
                }
                crate::screenshot(&q.win, &path);
                if std::path::Path::new(&path).exists() || tries.get() > 12 {
                    std::process::exit(0);
                }
                glib::ControlFlow::Continue
            });
        }
    });
    app.run_with_args(&["kestrel"])
}

fn build(app: &adw::Application) -> Rc<Quick> {
    let entry = gtk::Entry::builder()
        .placeholder_text("Search apps, files, settings… (? for tips)")
        .hexpand(true)
        .css_classes(["quick-entry"])
        .primary_icon_name("system-search-symbolic")
        .build();
    let list = gtk::ListBox::builder()
        .css_classes(["quick-list"])
        .selection_mode(gtk::SelectionMode::Single)
        .build();
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .max_content_height(SHOWN * 52)
        .child(&list)
        .build();
    // the keys, each beside what it does, spaced apart rather than joined
    let hint = gtk::Box::builder().spacing(18).css_classes(["quick-hint"]).build();
    for (key, what) in [("Enter", "open"), ("Ctrl+Enter", "show in folder"), ("Ctrl+C", "copy path"), ("Esc", "close")] {
        let pair = gtk::Box::builder().spacing(6).build();
        pair.append(&gtk::Label::builder().label(key).css_classes(["quick-key"]).build());
        pair.append(&gtk::Label::new(Some(what)));
        hint.append(&pair);
    }
    let note = gtk::Label::builder().xalign(0.0).hexpand(true).css_classes(["quick-note"]).build();
    hint.append(&note);
    let card = gtk::Box::builder().orientation(gtk::Orientation::Vertical).css_classes(["quick-card"]).build();
    card.append(&entry);
    card.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    card.append(&scroll);
    card.append(&hint);
    let win = adw::ApplicationWindow::builder()
        .application(app)
        .title("Kestrel Search")
        .default_width(680)
        .resizable(false)
        .decorated(false)
        .css_classes(["quick"])
        .content(&card)
        .build();
    let q = Rc::new(Quick {
        win,
        entry,
        list,
        scroll,
        hint: note,
        rows: RefCell::default(),
        apps: RefCell::new(launcher::all_apps()),
        pages: launcher::settings_pages(),
        generation: Cell::new(0),
        armed: RefCell::default(),
        debounce: RefCell::default(),
        sticky: Cell::new(false),
    });
    // apps installed or removed while we wait in the background
    let w = Rc::downgrade(&q);
    let monitor = gio::AppInfoMonitor::get();
    monitor.connect_changed(move |_| {
        if let Some(q) = w.upgrade() {
            *q.apps.borrow_mut() = launcher::all_apps();
        }
    });
    std::mem::forget(monitor);

    let w = Rc::downgrade(&q);
    q.entry.connect_changed(move |_| {
        let Some(q) = w.upgrade() else { return };
        q.armed.take();
        refresh_local(&q);
        if let Some(id) = q.debounce.take() {
            id.remove();
        }
        let w2 = Rc::downgrade(&q);
        *q.debounce.borrow_mut() = Some(glib::timeout_add_local_once(std::time::Duration::from_millis(60), move || {
            if let Some(q) = w2.upgrade() {
                q.debounce.take();
                fetch_files(&q);
            }
        }));
    });
    let w = Rc::downgrade(&q);
    q.entry.connect_activate(move |_| {
        if let Some(q) = w.upgrade() {
            let i = q.list.selected_row().map_or(0, |r| r.index().max(0) as usize);
            activate(&q, i, false);
        }
    });
    let w = Rc::downgrade(&q);
    q.list.connect_row_activated(move |_, r| {
        if let Some(q) = w.upgrade() {
            activate(&q, r.index().max(0) as usize, false);
        }
    });

    // keys work while typing: the entry keeps focus, arrows move the selection
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let w = Rc::downgrade(&q);
    keys.connect_key_pressed(move |_, key, _, mods| {
        let Some(q) = w.upgrade() else { return glib::Propagation::Proceed };
        let ctrl = mods.contains(gdk::ModifierType::CONTROL_MASK);
        let sel = q.list.selected_row().map_or(-1, |r| r.index());
        let n = q.rows.borrow().len() as i32;
        match key {
            gdk::Key::Escape => {
                if q.entry.text().is_empty() {
                    hide(&q);
                } else {
                    q.entry.set_text("");
                }
            }
            gdk::Key::Down | gdk::Key::Up | gdk::Key::Page_Down | gdk::Key::Page_Up if n > 0 => {
                let step = match key {
                    gdk::Key::Down => 1,
                    gdk::Key::Up => -1,
                    gdk::Key::Page_Down => SHOWN,
                    _ => -SHOWN,
                };
                select(&q, (sel + step).clamp(0, n - 1));
            }
            gdk::Key::Return | gdk::Key::KP_Enter if ctrl => activate(&q, sel.max(0) as usize, true),
            gdk::Key::c if ctrl && q.entry.selection_bounds().is_none() => {
                if let Some(Act::Path(p)) = q.rows.borrow().get(sel.max(0) as usize).map(|r| r.act.clone()) {
                    q.win.clipboard().set_text(&p);
                    hide(&q);
                }
            }
            gdk::Key::Tab => {
                // a folder's path into the box, to keep typing inside it
                if let Some(Act::Path(p)) = q.rows.borrow().get(sel.max(0) as usize).map(|r| r.act.clone())
                    && std::path::Path::new(&p).is_dir()
                {
                    q.entry.set_text(&format!("under:\"{p}\" "));
                    q.entry.set_position(-1);
                }
            }
            gdk::Key::F10 if mods.contains(gdk::ModifierType::SHIFT_MASK) => context_menu(&q, sel.max(0), None),
            gdk::Key::Menu => context_menu(&q, sel.max(0), None),
            _ => return glib::Propagation::Proceed,
        }
        glib::Propagation::Stop
    });
    q.win.add_controller(keys);

    // right-click a row
    let click = gtk::GestureClick::builder().button(3).build();
    let w = Rc::downgrade(&q);
    click.connect_pressed(move |_, _, x, y| {
        if let Some(q) = w.upgrade()
            && let Some(r) = q.list.row_at_y(y as i32)
        {
            select(&q, r.index());
            context_menu(&q, r.index(), Some((x, y)));
        }
    });
    q.list.add_controller(click);

    // clicking elsewhere hides it, like a launcher
    let w = Rc::downgrade(&q);
    q.win.connect_is_active_notify(move |win| {
        if !win.is_active()
            && let Some(q) = w.upgrade()
        {
            // a menu of our own takes focus too: only hide if no window of ours is active
            let w2 = Rc::downgrade(&q);
            glib::timeout_add_local_once(std::time::Duration::from_millis(150), move || {
                if let Some(q) = w2.upgrade()
                    && !q.sticky.get()
                    && !q.win.is_active()
                    && !has_popover(&q)
                {
                    hide(&q);
                }
            });
        }
    });
    let w = Rc::downgrade(&q);
    q.win.connect_close_request(move |_| {
        if let Some(q) = w.upgrade() {
            hide(&q);
        }
        glib::Propagation::Stop
    });
    q
}

fn has_popover(q: &Quick) -> bool {
    let mut c = q.list.first_child();
    while let Some(w) = c {
        let mut d = w.first_child();
        while let Some(x) = d {
            if x.downcast_ref::<gtk::Popover>().is_some_and(|p| p.is_visible()) {
                return true;
            }
            d = x.next_sibling();
        }
        c = w.next_sibling();
    }
    false
}

fn show(q: &Rc<Quick>) {
    crate::apply_accent(); // the preference may have changed while we waited in the background
    if crate::prefs::get().quick_clear_on_open {
        q.entry.set_text("");
    }
    refresh_local(q);
    fetch_files(q);
    q.win.present();
    q.entry.grab_focus();
    q.entry.select_region(0, -1);
}

fn hide(q: &Quick) {
    q.armed.take();
    q.win.set_visible(false);
}

fn select(q: &Quick, i: i32) {
    if let Some(r) = q.list.row_at_index(i) {
        q.list.select_row(Some(&r));
        // keep it in view without moving keyboard focus out of the entry
        if let Some(p) = r.compute_point(&q.list, &gtk::graphene::Point::new(0.0, 0.0)) {
            let adj = q.scroll.vadjustment();
            let (top, h) = (p.y() as f64, r.height() as f64);
            if top < adj.value() {
                adj.set_value(top);
            } else if top + h > adj.value() + adj.page_size() {
                adj.set_value(top + h - adj.page_size());
            }
        }
    }
}

fn text(q: &Quick) -> String {
    q.entry.text().trim().to_string()
}

/// Everything that doesn't need the search service, shown at once on every keystroke; files
/// follow when the service answers (`fetch_files`).
fn refresh_local(q: &Rc<Quick>) {
    let t = text(q);
    let rows = local_rows(q, &t);
    // keep the files from the last answer while the next one is on its way, if they still fit
    let files: Vec<Row> = q
        .rows
        .borrow()
        .iter()
        .filter(|r| matches!(r.act, Act::Path(_)) && !t.is_empty() && !t.starts_with('?'))
        .cloned()
        .collect();
    let mut all = rows;
    if !t.is_empty() && launcher::web(&t, &crate::prefs::get().web()).is_none() {
        all.extend(files);
    }
    picked_first(&mut all, &t);
    set_rows(q, all);
}

fn local_rows(q: &Quick, t: &str) -> Vec<Row> {
    let picks = crate::picks::load();
    if t.is_empty() {
        let mut out: Vec<Row> = picks.pinned.iter().filter_map(|p| path_row(q, p, "Pinned")).collect();
        let max = crate::prefs::get().quick_recent as usize;
        out.extend(
            picks
                .recent
                .iter()
                .filter(|p| !picks.is_pinned(p))
                .filter_map(|p| path_row(q, p, "Recent"))
                .take(max),
        );
        return out;
    }
    if t == "?" {
        return launcher::tips();
    }
    let lq = t.to_lowercase();
    let mut out = Vec::new();
    if let Some(r) = launcher::calc(t) {
        out.push(r);
    }
    if let Some(r) = launcher::web(t, &crate::prefs::get().web()) {
        out.push(r);
        return out;
    }
    let bonus_map: std::collections::HashMap<String, i32> = picks.prefer(t, glib::real_time() / 1_000_000).into_iter().collect();
    let bonus = |k: &str| bonus_map.get(k).copied().unwrap_or(0) / 10;
    out.extend(launcher::system(t));
    out.extend(launcher::apps(&lq, &q.apps.borrow(), &bonus, 5));
    out.extend(launcher::settings(&lq, &q.pages, 3));
    out
}

/// A row for a remembered path (pinned or recent), or an app remembered as `app:ID`.
fn path_row(q: &Quick, p: &str, why: &'static str) -> Option<Row> {
    if let Some(id) = p.strip_prefix("app:") {
        let a = q.apps.borrow().iter().find(|a| a.info.id().is_some_and(|x| x == id)).cloned()?;
        return Some(Row {
            title: a.name,
            subtitle: a.desc,
            icon: a.icon,
            act: Act::App(a.info),
            kind: "App",
            section: why,
        });
    }
    let path = std::path::Path::new(p);
    let meta = std::fs::metadata(path).ok()?;
    Some(file_row(p, meta.is_dir(), Some(why)))
}

fn file_row(p: &str, dir: bool, why: Option<&'static str>) -> Row {
    let path = std::path::Path::new(p);
    let name = path.file_name().map_or_else(|| p.to_string(), |n| n.to_string_lossy().into_owned());
    let folder = path
        .parent()
        .map(|d| d.to_string_lossy().replacen(&glib::home_dir().to_string_lossy().into_owned(), "~", 1))
        .unwrap_or_default();
    let icon = if dir {
        gio::ThemedIcon::new("folder").upcast()
    } else {
        gio::content_type_get_icon(&gio::content_type_guess(Some(&name), None).0)
    };
    Row {
        title: name,
        subtitle: folder,
        icon,
        act: Act::Path(p.to_string()),
        kind: "",
        section: why.unwrap_or_default(),
    }
}

fn fetch_files(q: &Rc<Quick>) {
    let t = text(q);
    let generation = q.generation.get() + 1;
    q.generation.set(generation);
    if t.is_empty() || t.starts_with('?') || launcher::web(&t, &crate::prefs::get().web()).is_some() {
        return;
    }
    let prefer = crate::picks::prefer(&t).into_iter().filter(|(p, _)| !p.starts_with("app:")).collect();
    let req = Request {
        q: t.clone(),
        sort: Sort::Relevance,
        limit: Some(FILES),
        prefer,
        ..Default::default()
    };
    let w = Rc::downgrade(q);
    glib::spawn_future_local(async move {
        let r = gio::spawn_blocking(move || crate::search::ask_once(&req)).await.ok().flatten();
        let Some(q) = w.upgrade() else { return };
        if q.generation.get() != generation {
            return;
        }
        let mut rows = local_rows(&q, &t);
        match r {
            Some(resp) if resp.error.is_none() => rows.extend(resp.hits.iter().map(|h| file_row(&h.path, h.dir, None))),
            Some(resp) if resp.indexing => rows.push(note("Indexing your files…", "File results appear once the first scan finishes")),
            Some(_) => {} // a half-typed query: apps and the rest still show
            None => rows.push(note("File search is off", "Kestrel's search service isn't running")),
        }
        picked_first(&mut rows, &t);
        set_rows(&q, rows);
    });
}

/// What you opened for this query before goes first, apps and files alike, most picked first.
fn picked_first(rows: &mut [Row], t: &str) {
    let bonus: std::collections::HashMap<String, i32> = crate::picks::prefer(t).into_iter().collect();
    rows.sort_by_key(|r| -match &r.act {
        Act::Path(p) => bonus.get(p).copied().unwrap_or(0),
        Act::App(a) => bonus.get(&launcher::app_key(a)).copied().unwrap_or(0),
        _ => 0,
    });
}

fn note(title: &str, sub: &str) -> Row {
    Row {
        title: title.into(),
        subtitle: sub.into(),
        icon: gio::ThemedIcon::new("dialog-information-symbolic").upcast(),
        act: Act::Insert(String::new()),
        kind: "",
        section: "",
    }
}

fn set_rows(q: &Rc<Quick>, rows: Vec<Row>) {
    let keep = q.list.selected_row().map(|r| r.index()).filter(|&i| i > 0 && (i as usize) < rows.len());
    q.list.remove_all();
    let t = text(q);
    let picks = crate::picks::load();
    for (i, r) in rows.iter().enumerate() {
        let row = row_widget(r);
        // before you type: a heading over the first row of each section (Pinned, Recent)
        if !r.section.is_empty() && (i == 0 || rows[i - 1].section != r.section) {
            row.set_header(Some(&gtk::Label::builder().label(r.section).xalign(0.0).css_classes(["quick-heading"]).build()));
        }
        if t.is_empty()
            && picks.is_pinned(match &r.act {
                Act::Path(p) => p,
                _ => "",
            })
            || matches!(&r.act, Act::App(a) if t.is_empty() && picks.is_pinned(&launcher::app_key(a)))
        {
            drag_reorder(q, &row, i);
        }
        q.list.append(&row);
    }
    *q.rows.borrow_mut() = rows;
    select(q, keep.unwrap_or(0));
    q.scroll.set_visible(!q.rows.borrow().is_empty());
}

fn row_widget(r: &Row) -> gtk::ListBoxRow {
    let icon = gtk::Image::builder().gicon(&r.icon).pixel_size(32).build();
    let title = gtk::Label::builder()
        .label(&r.title)
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::Middle)
        .build();
    let sub = gtk::Label::builder()
        .label(&r.subtitle)
        .xalign(0.0)
        // paths keep their end (the folder the file is in); descriptions keep their start
        .ellipsize(if matches!(r.act, Act::Path(_)) {
            gtk::pango::EllipsizeMode::Start
        } else {
            gtk::pango::EllipsizeMode::End
        })
        .css_classes(["dim-label", "caption"])
        .build();
    let text = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .valign(gtk::Align::Center)
        .hexpand(true)
        .build();
    text.append(&title);
    sub.set_visible(!r.subtitle.is_empty());
    text.append(&sub);
    let b = gtk::Box::builder().spacing(12).build();
    b.append(&icon);
    b.append(&text);
    if !r.kind.is_empty() {
        b.append(&gtk::Label::builder().label(r.kind).valign(gtk::Align::Center).css_classes(["quick-kind"]).build());
    }
    let row = gtk::ListBoxRow::builder().child(&b).build();
    // drag a file straight into another app
    if let Act::Path(p) = &r.act {
        let f = gio::File::for_path(p);
        let src = gtk::DragSource::builder().actions(gdk::DragAction::COPY).build();
        src.connect_prepare(move |_, _, _| {
            let list = gdk::FileList::from_array(std::slice::from_ref(&f));
            Some(gdk::ContentProvider::for_value(&list.to_value()))
        });
        row.add_controller(src);
    }
    row
}

/// Pinned rows: drag one onto another to move it there.
fn drag_reorder(q: &Rc<Quick>, row: &gtk::ListBoxRow, i: usize) {
    let src = gtk::DragSource::builder().actions(gdk::DragAction::MOVE).build();
    src.connect_prepare(move |_, _, _| Some(gdk::ContentProvider::for_value(&format!("kestrel-pin:{i}").to_value())));
    row.add_controller(src);
    let drop = gtk::DropTarget::new(glib::Type::STRING, gdk::DragAction::MOVE);
    let w = Rc::downgrade(q);
    drop.connect_drop(move |_, v, _, _| {
        let (Some(q), Ok(s)) = (w.upgrade(), v.get::<String>()) else { return false };
        let Some(from) = s.strip_prefix("kestrel-pin:").and_then(|n| n.parse::<usize>().ok()) else {
            return false;
        };
        let key = |r: &Row| match &r.act {
            Act::Path(p) => Some(p.clone()),
            Act::App(a) => Some(launcher::app_key(a)),
            _ => None,
        };
        let (a, b) = {
            let rows = q.rows.borrow();
            (rows.get(from).and_then(key), rows.get(i).and_then(key))
        };
        let (Some(a), Some(b)) = (a, b) else { return false };
        crate::picks::update(|p| {
            let (Some(fa), Some(fb)) = (p.pinned.iter().position(|x| *x == a), p.pinned.iter().position(|x| *x == b)) else {
                return;
            };
            let item = p.pinned.remove(fa);
            p.pinned.insert(fb, item);
        });
        refresh_local(&q);
        true
    });
    row.add_controller(drop);
}

fn record(q: &Quick, key: &str) {
    let t = text(q);
    if t.is_empty() {
        crate::picks::update(|p| {
            p.recent.retain(|x| x != key);
            p.recent.insert(0, key.to_string());
        });
    } else {
        crate::picks::record(&t, key);
    }
}

fn launch_ctx(q: &Quick) -> gdk::AppLaunchContext {
    gtk::prelude::WidgetExt::display(&q.win).app_launch_context()
}

fn activate(q: &Rc<Quick>, i: usize, in_folder: bool) {
    let Some(r) = q.rows.borrow().get(i).cloned() else { return };
    match r.act {
        Act::App(a) => {
            record(q, &launcher::app_key(&a));
            if let Err(e) = a.launch(&[], Some(&launch_ctx(q))) {
                q.hint.set_label(&format!("Couldn't start {}: {e}", a.name()));
                return;
            }
        }
        Act::Path(p) => {
            record(q, &p);
            let path = std::path::Path::new(&p);
            let me = std::env::current_exe().unwrap_or_else(|_| "kestrel".into());
            if in_folder {
                let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else { return };
                let _ = std::process::Command::new(&me).arg(dir).arg("--select").arg(name).spawn();
            } else if path.is_dir() {
                let _ = std::process::Command::new(&me).arg(path).spawn();
            } else {
                let uri = gio::File::for_path(path).uri();
                if let Err(e) = gio::AppInfo::launch_default_for_uri(&uri, Some(&launch_ctx(q))) {
                    q.hint.set_label(&format!("Couldn't open it: {e}"));
                    return;
                }
            }
        }
        Act::Copy(s) => q.win.clipboard().set_text(&s),
        Act::Url(u) => {
            let _ = gio::AppInfo::launch_default_for_uri(&u, Some(&launch_ctx(q)));
        }
        Act::System(a) => {
            if a.confirm && q.armed.borrow().is_none_or(|n| n != a.name) {
                *q.armed.borrow_mut() = Some(a.name);
                if let Some(row) = q.list.row_at_index(i as i32)
                    && let Some(sub) = row
                        .child()
                        .and_then(|b| b.first_child())
                        .and_then(|icon| icon.next_sibling())
                        .and_then(|t| t.last_child())
                        .and_downcast::<gtk::Label>()
                {
                    sub.set_visible(true);
                    sub.set_label(&format!("Press Enter again to {}", a.name.to_lowercase()));
                    sub.add_css_class("quick-confirm");
                }
                return;
            }
            q.armed.take();
            if !launcher::run_system(a) {
                q.hint.set_label(&format!("Couldn't {}", a.name.to_lowercase()));
                return;
            }
        }
        Act::Insert(s) => {
            if !s.is_empty() {
                q.entry.set_text(&s);
                q.entry.set_position(-1);
            }
            return;
        }
    }
    hide(q);
}

fn context_menu(q: &Rc<Quick>, i: i32, at: Option<(f64, f64)>) {
    let Some(r) = q.rows.borrow().get(i as usize).cloned() else { return };
    let key = match &r.act {
        Act::Path(p) => p.clone(),
        Act::App(a) => launcher::app_key(a),
        _ => return,
    };
    let pinned = crate::picks::load().is_pinned(&key);
    let menu = gio::Menu::new();
    menu.append(Some("Open"), Some("quick.open"));
    if matches!(r.act, Act::Path(_)) {
        menu.append(Some("Open With…"), Some("quick.open-with"));
        menu.append(Some("Open File Location"), Some("quick.locate"));
        menu.append(Some("Copy Path"), Some("quick.copy-path"));
    }
    menu.append(Some(if pinned { "Unpin" } else { "Pin to Top" }), Some("quick.pin"));
    menu.append(Some("Forget"), Some("quick.forget"));
    let group = gio::SimpleActionGroup::new();
    type Handler = Box<dyn Fn(&Rc<Quick>)>;
    let add = |name: &str, f: Handler| {
        let a = gio::SimpleAction::new(name, None);
        let w = Rc::downgrade(q);
        a.connect_activate(move |_, _| {
            if let Some(q) = w.upgrade() {
                f(&q);
            }
        });
        group.add_action(&a);
    };
    let ii = i as usize;
    add("open", Box::new(move |q| activate(q, ii, false)));
    add("locate", Box::new(move |q| activate(q, ii, true)));
    let k = key.clone();
    add(
        "copy-path",
        Box::new(move |q| {
            q.win.clipboard().set_text(&k);
            hide(q);
        }),
    );
    let k = key.clone();
    add(
        "open-with",
        Box::new(move |q| {
            let l = gtk::FileLauncher::new(Some(&gio::File::for_path(&k)));
            l.set_always_ask(true);
            l.launch(Some(&q.win), gio::Cancellable::NONE, |_| {});
        }),
    );
    let k = key.clone();
    add(
        "pin",
        Box::new(move |q| {
            crate::picks::update(|p| {
                if p.is_pinned(&k) {
                    p.pinned.retain(|x| *x != k);
                } else {
                    p.pinned.push(k.clone());
                }
            });
            refresh_local(q);
        }),
    );
    add(
        "forget",
        Box::new(move |q| {
            crate::picks::update(|p| p.forget(&key));
            refresh_local(q);
        }),
    );
    let Some(row) = q.list.row_at_index(i) else { return };
    row.insert_action_group("quick", Some(&group));
    let pop = gtk::PopoverMenu::from_model(Some(&menu));
    pop.set_parent(&row);
    pop.set_has_arrow(false);
    if let Some((x, y)) = at
        && let Some(p) = q.list.compute_point(&row, &gtk::graphene::Point::new(x as f32, y as f32))
    {
        pop.set_pointing_to(Some(&gdk::Rectangle::new(p.x() as i32, p.y() as i32, 1, 1)));
    }
    pop.connect_closed(|p| {
        let p = p.clone();
        glib::idle_add_local_once(move || p.unparent());
    });
    pop.popup();
}

// ── the global shortcut ──

const KEYS_SCHEMA: &str = "org.gnome.settings-daemon.plugins.media-keys";
const BINDING_SCHEMA: &str = "org.gnome.settings-daemon.plugins.media-keys.custom-keybinding";
const BINDING_PATH: &str = "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/kestrel-quick/";

/// Add, change or remove Kestrel's entry in GNOME's custom keyboard shortcuts (the same list
/// Settings › Keyboard › Custom Shortcuts shows), leaving everyone else's alone.
pub fn set_shortcut(accel: Option<&str>) -> Result<(), String> {
    let src = gio::SettingsSchemaSource::default().ok_or("GNOME's settings schemas aren't installed")?;
    if src.lookup(KEYS_SCHEMA, true).is_none() || src.lookup(BINDING_SCHEMA, true).is_none() {
        return Err("Global shortcuts need GNOME (gnome-settings-daemon)".into());
    }
    let keys = gio::Settings::new(KEYS_SCHEMA);
    let mut list: Vec<String> = keys.strv("custom-keybindings").iter().map(|s| s.to_string()).collect();
    list.retain(|p| p != BINDING_PATH);
    let b = gio::Settings::with_path(BINDING_SCHEMA, BINDING_PATH);
    match accel {
        Some(a) => {
            let me = std::env::current_exe()
                .ok()
                .filter(|p| p.starts_with("/usr"))
                .map_or_else(|| "kestrel".to_string(), |p| p.to_string_lossy().into_owned());
            b.set_string("name", "Kestrel Quick Search").map_err(|e| e.to_string())?;
            b.set_string("command", &format!("{me} --quick")).map_err(|e| e.to_string())?;
            b.set_string("binding", a).map_err(|e| e.to_string())?;
            list.push(BINDING_PATH.into());
        }
        None => {
            for k in ["name", "command", "binding"] {
                b.reset(k);
            }
        }
    }
    let refs: Vec<&str> = list.iter().map(String::as_str).collect();
    keys.set_strv("custom-keybindings", refs.as_slice()).map_err(|e| e.to_string())?;
    gio::Settings::sync();
    Ok(())
}

/// Shortcuts GNOME itself already uses for `accel` (by name), so Preferences can warn.
pub fn shortcut_clash(accel: &str) -> Option<String> {
    let src = gio::SettingsSchemaSource::default()?;
    for schema in [
        "org.gnome.desktop.wm.keybindings",
        "org.gnome.shell.keybindings",
        "org.gnome.mutter.keybindings",
        "org.gnome.settings-daemon.plugins.media-keys",
    ] {
        let Some(sc) = src.lookup(schema, true) else { continue };
        let s = gio::Settings::new(schema);
        for k in sc.list_keys() {
            if sc.key(&k).value_type().as_str() != "as" {
                continue;
            }
            if s.strv(&k).iter().any(|v| v.eq_ignore_ascii_case(accel)) {
                return Some(k.to_string());
            }
        }
    }
    None
}
