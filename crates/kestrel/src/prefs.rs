//! User settings (~/.config/kestrel/settings.json) and the Preferences dialog.

use crate::browser::{Mode, Scope};
use adw::prelude::*;
use gtk::{gio, glib};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::path::PathBuf;

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
        }
    }
}

impl Settings {
    pub fn view(&self) -> Mode {
        Mode::parse(&self.default_view)
    }
    pub fn scope(&self) -> Scope {
        Scope::ALL[(self.default_scope as usize).min(2)]
    }
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

fn switch(title: &str, subtitle: &str, on: bool) -> adw::SwitchRow {
    adw::SwitchRow::builder().title(title).subtitle(subtitle).active(on).build()
}

/// `on_change` lets the window react (hidden files, etc.); `toast` reports results.
pub fn show(parent: &adw::ApplicationWindow, on_change: impl Fn() + 'static, toast: impl Fn(&str) + Clone + 'static) {
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
    page.add(&browse);

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
