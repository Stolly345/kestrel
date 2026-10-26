//! User settings (~/.config/kestrel/settings.json) and the Preferences dialog.

use crate::browser::{Mode, Scope};
use adw::prelude::*;
use gtk::{gio, glib};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::path::PathBuf;

/// One optional Details-view column: shown or not, its width, and (by its place in the list) its order.
#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct ColumnPref {
    pub id: String,
    pub visible: bool,
    pub width: i32,
}

/// (id, title, default width) of the columns after Name, in default order.
pub const COLUMNS: [(&str, &str, i32); 3] = [("size", "Size", 90), ("modified", "Modified", 170), ("type", "Type", 220)];

fn default_columns() -> Vec<ColumnPref> {
    COLUMNS.iter().map(|&(id, _, width)| ColumnPref { id: id.into(), visible: true, width }).collect()
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
#[serde(default)]
pub struct Settings {
    /// "system" | "light" | "dark"
    pub color_scheme: String,
    /// View for new tabs: "list" | "grid" | "columns"
    pub default_view: String,
    pub show_hidden: bool,
    pub restore_session: bool,
    /// Scope for new tabs: 0 Everywhere, 1 This folder, 2 This folder only
    pub default_scope: u32,
    pub type_to_search: bool,
    /// Times in file lists: "system" (GNOME's clock setting) | "24h" | "12h"
    pub clock: String,
    /// Type column: "full" (Microsoft Excel Worksheet) | "short" (Excel) | "extension" (XLSX)
    pub type_style: String,
    /// Details-view columns after Name, in display order.
    pub columns: Vec<ColumnPref>,
    /// Preview pane when Kestrel starts: "remember" (as it was at quit) | "show" | "hide"
    pub preview_at_start: String,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            color_scheme: "system".into(),
            default_view: "list".into(),
            show_hidden: false,
            restore_session: true,
            default_scope: 0,
            type_to_search: true,
            clock: "system".into(),
            type_style: "full".into(),
            columns: default_columns(),
            preview_at_start: "remember".into(),
        }
    }
}

impl Settings {
    pub fn view(&self) -> Mode {
        Mode::parse(&self.default_view)
    }
    /// 12-hour times? "system" asks GNOME's clock-format setting.
    pub fn clock_12h(&self) -> bool {
        match self.clock.as_str() {
            "12h" => true,
            "24h" => false,
            _ => system_12h(),
        }
    }
    /// The saved columns, cleaned up: known ids only, each once, missing ones appended.
    pub fn columns(&self) -> Vec<ColumnPref> {
        let mut out: Vec<ColumnPref> = Vec::new();
        for c in &self.columns {
            if COLUMNS.iter().any(|(id, ..)| *id == c.id) && !out.iter().any(|o| o.id == c.id) {
                out.push(ColumnPref { width: c.width.clamp(40, 1000), ..c.clone() });
            }
        }
        for d in default_columns() {
            if !out.iter().any(|o| o.id == d.id) {
                out.push(d);
            }
        }
        out
    }
    pub fn scope(&self) -> Scope {
        Scope::ALL[(self.default_scope as usize).min(2)]
    }
}

/// GNOME's clock-format (read once; 24-hour if the schema isn't installed).
fn system_12h() -> bool {
    static H12: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *H12.get_or_init(|| {
        let id = "org.gnome.desktop.interface";
        gio::SettingsSchemaSource::default().and_then(|s| s.lookup(id, true)).is_some()
            && gio::Settings::new(id).string("clock-format") == "12h"
    })
}

fn file() -> PathBuf {
    glib::user_config_dir().join("kestrel/settings.json")
}

thread_local! {
    static SETTINGS: RefCell<Settings> = RefCell::new(
        std::fs::read_to_string(file()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    );
}

pub fn get() -> Settings {
    SETTINGS.with_borrow(|s| s.clone())
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TypeStyle {
    Full,
    Short,
    Extension,
}

/// The Type column style, without cloning all settings (called per row and per sort comparison).
pub fn type_style() -> TypeStyle {
    SETTINGS.with_borrow(|s| match s.type_style.as_str() {
        "short" => TypeStyle::Short,
        "extension" => TypeStyle::Extension,
        _ => TypeStyle::Full,
    })
}

/// Change, persist, and apply what applies globally (colour scheme).
pub fn update(f: impl FnOnce(&mut Settings)) {
    let s = SETTINGS.with_borrow_mut(|s| {
        f(s);
        s.clone()
    });
    let p = file();
    let _ = std::fs::create_dir_all(p.parent().unwrap());
    let _ = std::fs::write(p, serde_json::to_string_pretty(&s).unwrap());
    apply_color_scheme(&s);
}

pub fn apply_color_scheme(s: &Settings) {
    adw::StyleManager::default().set_color_scheme(match s.color_scheme.as_str() {
        "light" => adw::ColorScheme::ForceLight,
        "dark" => adw::ColorScheme::ForceDark,
        _ => adw::ColorScheme::Default,
    });
}

const EXCLUDES: &str = "/etc/kestrel/excludes";

fn read_excludes() -> Vec<String> {
    std::fs::read_to_string(EXCLUDES)
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with('/'))
        .map(String::from)
        .collect()
}

/// Write /etc/kestrel/excludes and restart the service, as root through one polkit prompt.
fn write_excludes(list: &[String]) -> Result<(), String> {
    use std::io::Write;
    let mut body = String::from("# kestrel-indexd: one absolute path per line; everything under it is not indexed.\n# Edited from Kestrel's Preferences.\n");
    for l in list {
        body += l;
        body += "\n";
    }
    let mut child = std::process::Command::new("pkexec")
        .args([
            "sh",
            "-c",
            "cat > /etc/kestrel/excludes.new && mv /etc/kestrel/excludes.new /etc/kestrel/excludes && systemctl restart kestrel-indexd",
        ])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    child.stdin.take().unwrap().write_all(body.as_bytes()).map_err(|e| e.to_string())?;
    let st = child.wait().map_err(|e| e.to_string())?;
    if st.success() { Ok(()) } else { Err("cancelled or not authorised".into()) }
}

fn combo(title: &str, subtitle: &str, items: &[&str], selected: u32) -> adw::ComboRow {
    adw::ComboRow::builder()
        .title(title)
        .subtitle(subtitle)
        .model(&gtk::StringList::new(items))
        .selected(selected)
        .build()
}

/// Columns: one row per column with width, move up/down, and show/hide; Reset restores defaults.
fn columns_group(apply: std::rc::Rc<dyn Fn()>) -> adw::PreferencesGroup {
    use std::rc::Rc;
    let group = adw::PreferencesGroup::builder()
        .title("Columns")
        .description("Details view. Name always comes first and takes the remaining space. Dragging a column edge in the list is remembered too.")
        .build();
    let reset = gtk::Button::builder().label("Reset").css_classes(["flat"]).valign(gtk::Align::Center).build();
    group.set_header_suffix(Some(&reset));
    let rows: Rc<RefCell<Vec<adw::ActionRow>>> = Rc::default();
    let rebuild: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::default();
    let change = {
        let (apply, rebuild) = (apply.clone(), rebuild.clone());
        move |f: &dyn Fn(&mut Vec<ColumnPref>), relayout: bool| {
            update(|s| {
                let mut c = s.columns();
                f(&mut c);
                s.columns = c;
            });
            apply();
            if relayout && let Some(r) = rebuild.borrow().clone() {
                // after the click handler returns: it's running inside a row that's about to go
                glib::idle_add_local_once(move || r());
            }
        }
    };
    let change: Rc<dyn Fn(&dyn Fn(&mut Vec<ColumnPref>), bool)> = Rc::new(change);
    let (g, rws, ch) = (group.clone(), rows.clone(), change.clone());
    let build: Rc<dyn Fn()> = Rc::new(move || {
        for r in rws.borrow_mut().drain(..) {
            g.remove(&r);
        }
        let cols = get().columns();
        let n = cols.len();
        for (i, c) in cols.into_iter().enumerate() {
            let title = COLUMNS.iter().find(|(id, ..)| *id == c.id).map_or("", |(_, t, _)| *t);
            let row = adw::ActionRow::builder().title(title).build();
            let width = gtk::SpinButton::with_range(40.0, 1000.0, 10.0);
            width.set_value(c.width as f64);
            width.set_valign(gtk::Align::Center);
            width.set_tooltip_text(Some("Width in pixels"));
            let (id, ch2) = (c.id.clone(), ch.clone());
            width.connect_value_changed(move |w| {
                let v = w.value() as i32;
                ch2(&|cols: &mut Vec<ColumnPref>| cols.iter_mut().filter(|x| x.id == id).for_each(|x| x.width = v), false);
            });
            let arrow = |icon: &str, tip: &str, ok: bool, delta: isize| {
                let b = gtk::Button::builder()
                    .icon_name(icon)
                    .tooltip_text(tip)
                    .css_classes(["flat", "circular"])
                    .valign(gtk::Align::Center)
                    .sensitive(ok)
                    .build();
                let ch3 = ch.clone();
                b.connect_clicked(move |_| {
                    let j = (i as isize + delta) as usize;
                    ch3(&|cols: &mut Vec<ColumnPref>| cols.swap(i, j), true);
                });
                b
            };
            let up = arrow("go-up-symbolic", "Move left", i > 0, -1);
            let down = arrow("go-down-symbolic", "Move right", i + 1 < n, 1);
            let show = gtk::Switch::builder().active(c.visible).valign(gtk::Align::Center).tooltip_text("Show this column").build();
            let (id, ch4) = (c.id.clone(), ch.clone());
            show.connect_active_notify(move |s| {
                let on = s.is_active();
                ch4(&|cols: &mut Vec<ColumnPref>| cols.iter_mut().filter(|x| x.id == id).for_each(|x| x.visible = on), false);
            });
            for w in [width.upcast_ref::<gtk::Widget>(), up.upcast_ref(), down.upcast_ref(), show.upcast_ref()] {
                row.add_suffix(w);
            }
            crate::label_icon_buttons(row.upcast_ref());
            g.add(&row);
            rws.borrow_mut().push(row);
        }
    });
    *rebuild.borrow_mut() = Some(build.clone());
    build();
    let ch = change.clone();
    reset.connect_clicked(move |_| ch(&|cols: &mut Vec<ColumnPref>| *cols = default_columns(), true));
    group
}

fn switch(title: &str, subtitle: &str, on: bool) -> adw::SwitchRow {
    adw::SwitchRow::builder().title(title).subtitle(subtitle).active(on).build()
}

/// `on_change` lets the window react (hidden files, etc.); `toast` reports results.
/// `on_change`: display settings changed (re-list); `on_columns`: column layout changed (cheap, live).
pub fn show(
    parent: &adw::ApplicationWindow,
    on_change: impl Fn() + 'static,
    on_columns: impl Fn() + 'static,
    toast: impl Fn(&str) + Clone + 'static,
) {
    let s = get();
    let on_change = std::rc::Rc::new(on_change);
    let dialog = adw::PreferencesDialog::builder().search_enabled(false).build();

    // ── General ──
    let page = adw::PreferencesPage::builder()
        .title("General")
        .icon_name("preferences-system-symbolic")
        .build();
    let look = adw::PreferencesGroup::builder().title("Appearance").build();
    let schemes = ["system", "light", "dark"];
    let scheme = combo(
        "Style",
        "Light or dark, or follow the system",
        &["Follow System", "Light", "Dark"],
        schemes.iter().position(|x| *x == s.color_scheme).unwrap_or(0) as u32,
    );
    scheme.connect_selected_notify(move |r| update(|s| s.color_scheme = schemes[r.selected().min(2) as usize].into()));
    look.add(&scheme);
    let clocks = ["system", "24h", "12h"];
    let clock = combo(
        "Time format",
        "For Modified times in file lists",
        &[if system_12h() { "Follow System (12-hour)" } else { "Follow System (24-hour)" }, "24-hour", "12-hour"],
        clocks.iter().position(|x| *x == s.clock).unwrap_or(0) as u32,
    );
    let oc = on_change.clone();
    clock.connect_selected_notify(move |r| {
        update(|s| s.clock = clocks[r.selected().min(2) as usize].into());
        oc();
    });
    look.add(&clock);
    page.add(&look);

    let browse = adw::PreferencesGroup::builder().title("Browsing").build();
    let views = ["list", "grid", "columns"];
    let view = combo(
        "Default view",
        "For new tabs and windows",
        &["Details", "Grid", "Columns"],
        views.iter().position(|x| *x == s.default_view).unwrap_or(0) as u32,
    );
    view.connect_selected_notify(move |r| update(|s| s.default_view = views[r.selected().min(2) as usize].into()));
    browse.add(&view);
    let hidden = switch("Show hidden files", "Files and folders starting with a dot (Ctrl+H)", s.show_hidden);
    let oc = on_change.clone();
    hidden.connect_active_notify(move |r| {
        update(|s| s.show_hidden = r.is_active());
        oc();
    });
    browse.add(&hidden);
    let restore = switch("Reopen tabs at start", "Pick up where you left off", s.restore_session);
    restore.connect_active_notify(|r| update(|s| s.restore_session = r.is_active()));
    browse.add(&restore);
    let starts = ["remember", "show", "hide"];
    let preview = combo(
        "Preview pane at start",
        "Whether the preview pane is open when Kestrel starts",
        &["Remember Last State", "Always Show", "Always Hide"],
        starts.iter().position(|x| *x == s.preview_at_start).unwrap_or(0) as u32,
    );
    preview.connect_selected_notify(move |r| update(|s| s.preview_at_start = starts[r.selected().min(2) as usize].into()));
    browse.add(&preview);
    let styles = ["full", "short", "extension"];
    let types = combo(
        "File type names",
        "Full: Microsoft Excel Worksheet · Short: Excel · Extension: XLSX",
        &["Full", "Short", "Extension"],
        styles.iter().position(|x| *x == s.type_style).unwrap_or(0) as u32,
    );
    let oc = on_change.clone();
    types.connect_selected_notify(move |r| {
        update(|s| s.type_style = styles[r.selected().min(2) as usize].into());
        oc();
    });
    browse.add(&types);
    page.add(&browse);
    page.add(&columns_group(std::rc::Rc::new(on_columns)));

    let search = adw::PreferencesGroup::builder().title("Search").build();
    let scope = combo(
        "Default scope",
        "What a new tab searches",
        &Scope::ALL.map(|s| s.label()),
        s.default_scope.min(2),
    );
    scope.connect_selected_notify(|r| update(|s| s.default_scope = r.selected().min(2)));
    search.add(&scope);
    let typing = switch(
        "Type anywhere to search",
        "Letters typed in the file list go to the search box",
        s.type_to_search,
    );
    typing.connect_active_notify(|r| update(|s| s.type_to_search = r.is_active()));
    search.add(&typing);
    page.add(&search);

    let system = adw::PreferencesGroup::builder().title("System").build();
    let fm = switch(
        "Default file manager",
        "Open folders and \u{201c}Show in Folder\u{201d} from other apps in Kestrel",
        crate::filemanager1::is_default(),
    );
    let t = toast.clone();
    fm.connect_active_notify(move |r| {
        if let Err(e) = crate::filemanager1::make_default(r.is_active()) {
            t(&format!("Couldn't change the default: {e}"));
        }
    });
    system.add(&fm);
    page.add(&system);
    dialog.add(&page);

    // ── Index ──
    let page = adw::PreferencesPage::builder()
        .title("Search Index")
        .icon_name("system-search-symbolic")
        .build();
    let status = adw::PreferencesGroup::builder().title("Search service").build();
    let state = adw::ActionRow::builder().title("Status").subtitle("Checking…").build();
    status.add(&state);
    let rebuild = adw::ButtonRow::builder().title("Rebuild Index").build();
    let t = toast.clone();
    rebuild.connect_activated(move |_| {
        let t = t.clone();
        glib::spawn_future_local(async move {
            let ok = gio::spawn_blocking(|| {
                std::process::Command::new("pkexec")
                    .args(["systemctl", "restart", "kestrel-indexd"])
                    .status()
                    .is_ok_and(|s| s.success())
            })
            .await
            .unwrap_or(false);
            t(if ok {
                "Rebuilding the index; search keeps working meanwhile"
            } else {
                "Not rebuilt (cancelled or not authorised)"
            });
        });
    });
    status.add(&rebuild);
    page.add(&status);
    let st = state.clone();
    glib::spawn_future_local(async move {
        let r = gio::spawn_blocking(|| crate::search::stats("")).await.ok().flatten();
        st.set_subtitle(&match r {
            Some((n, b)) => format!(
                "Running · {} files and folders you can see · {}",
                crate::browser::thousands(n as u64),
                glib::format_size(b)
            ),
            None => "Not running, or still indexing".into(),
        });
    });

    let ex = adw::PreferencesGroup::builder()
        .title("Excluded folders")
        .description("Not indexed, so never in search results. Changes apply to everyone on this computer and need an administrator password.")
        .build();
    let list: std::rc::Rc<RefCell<Vec<String>>> = std::rc::Rc::new(RefCell::new(read_excludes()));
    let rows = gtk::ListBox::builder()
        .css_classes(["boxed-list"])
        .selection_mode(gtk::SelectionMode::None)
        .build();
    ex.add(&rows);
    let apply = gtk::Button::builder().label("Apply").css_classes(["suggested-action"]).sensitive(false).build();
    let add = gtk::Button::builder().label("Add Folder…").build();
    let buttons = gtk::Box::builder().spacing(8).halign(gtk::Align::End).margin_top(8).build();
    buttons.append(&add);
    buttons.append(&apply);
    ex.add(&buttons);

    // rebuild the rows from `list`; removing one marks the list dirty
    let refresh: std::rc::Rc<RefCell<Option<Box<dyn Fn()>>>> = Default::default();
    let (rows2, list2, apply2, refresh2) = (rows.clone(), list.clone(), apply.clone(), refresh.clone());
    *refresh.borrow_mut() = Some(Box::new(move || {
        while let Some(c) = rows2.first_child() {
            rows2.remove(&c);
        }
        for (i, p) in list2.borrow().iter().enumerate() {
            let r = adw::ActionRow::builder().title(p).build();
            let del = gtk::Button::builder()
                .icon_name("user-trash-symbolic")
                .valign(gtk::Align::Center)
                .css_classes(["flat"])
                .tooltip_text("Stop excluding")
                .build();
            del.update_property(&[gtk::accessible::Property::Label(&format!("Stop excluding {p}"))]);
            let (l, a, rf) = (list2.clone(), apply2.clone(), refresh2.clone());
            del.connect_clicked(move |_| {
                l.borrow_mut().remove(i);
                a.set_sensitive(true);
                // defer: we're inside a row that the refresh removes
                let rf = rf.clone();
                glib::idle_add_local_once(move || {
                    if let Some(f) = rf.borrow().as_ref() {
                        f();
                    }
                });
            });
            r.add_suffix(&del);
            rows2.append(&r);
        }
        if list2.borrow().is_empty() {
            rows2.append(&adw::ActionRow::builder().title("Nothing excluded").css_classes(["dim-label"]).build());
        }
    }));
    if let Some(f) = refresh.borrow().as_ref() {
        f();
    }
    let (l, a, rf, d) = (list.clone(), apply.clone(), refresh.clone(), dialog.clone());
    add.connect_clicked(move |_| {
        let (l, a, rf) = (l.clone(), a.clone(), rf.clone());
        let fd = gtk::FileDialog::builder().title("Exclude a folder from search").build();
        fd.select_folder(d.root().and_downcast::<gtk::Window>().as_ref(), gio::Cancellable::NONE, move |r| {
            let Some(p) = r.ok().and_then(|f| f.path()) else { return };
            let p = p.display().to_string();
            if !l.borrow().contains(&p) {
                l.borrow_mut().push(p);
                a.set_sensitive(true);
                if let Some(f) = rf.borrow().as_ref() {
                    f();
                }
            }
        });
    });
    let (l, t) = (list.clone(), toast.clone());
    apply.connect_clicked(move |b| {
        let (items, t, b) = (l.borrow().clone(), t.clone(), b.clone());
        b.set_sensitive(false);
        glib::spawn_future_local(async move {
            let r = gio::spawn_blocking(move || write_excludes(&items)).await.unwrap_or(Err("failed".into()));
            match r {
                Ok(()) => t("Excluded folders saved; the index is rebuilding"),
                Err(e) => {
                    b.set_sensitive(true);
                    t(&format!("Not saved: {e}"));
                }
            }
        });
    });
    page.add(&ex);
    dialog.add(&page);

    dialog.present(Some(parent));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_columns_are_cleaned_up() {
        let c = |id: &str, visible, width| ColumnPref { id: id.into(), visible, width };
        // old/hand-edited settings: an unknown id, a duplicate, a silly width, Modified missing
        let s = Settings {
            columns: vec![c("type", true, 5000), c("bogus", true, 50), c("size", false, 90), c("type", false, 10)],
            ..Settings::default()
        };
        let got: Vec<_> = s.columns().into_iter().map(|c| (c.id, c.visible, c.width)).collect();
        assert_eq!(
            got,
            vec![("type".into(), true, 1000), ("size".into(), false, 90), ("modified".into(), true, 170)]
        );
        // settings files from before columns existed get the defaults
        let old: Settings = serde_json::from_str(r#"{"clock":"12h"}"#).unwrap();
        assert_eq!(old.columns().len(), 3);
    }
}
