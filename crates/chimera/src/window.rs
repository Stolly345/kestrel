//! Main window: sidebar, the central search box, two tabbed panes, and every window action.

use crate::browser::{Browser, Loc, Mode, Scope};
use crate::fileops::{Op, Ops};
use crate::query_ui::{self, Group};
use crate::sidebar::{self, Sidebar};
use adw::prelude::*;
use gtk::{gdk, gio, glib};
use serde::{Deserialize, Serialize};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

thread_local! {
    /// Every open tab, across windows; tabs can be dragged between panes and windows.
    static BROWSERS: RefCell<Vec<Rc<Browser>>> = RefCell::default();
    /// Every open window. Signal handlers only hold weak refs, so this is what keeps a `Win`
    /// alive; without it every button and shortcut silently did nothing once startup returned.
    static WINDOWS: RefCell<Vec<Rc<Win>>> = RefCell::default();
}

fn find(child: &gtk::Widget) -> Option<Rc<Browser>> {
    BROWSERS.with_borrow(|v| v.iter().find(|b| b.root.upcast_ref::<gtk::Widget>() == child).cloned())
}

fn unregister(child: &gtk::Widget) {
    BROWSERS.with_borrow_mut(|v| v.retain(|b| b.root.upcast_ref::<gtk::Widget>() != child));
}

#[derive(Serialize, Deserialize, Default)]
pub struct Session {
    pub left: Vec<String>,
    pub right: Vec<String>,
    pub dual: bool,
    pub hidden: bool,
    pub mode: String,
    pub size: (i32, i32),
}

fn session_file() -> std::path::PathBuf {
    glib::user_config_dir().join("chimera/session.json")
}

pub fn load_session() -> Option<Session> {
    serde_json::from_str(&std::fs::read_to_string(session_file()).ok()?).ok()
}

pub struct Pane {
    pub root: gtk::Box,
    pub view: adw::TabView,
}

impl Pane {
    fn new() -> Pane {
        let view = adw::TabView::builder().vexpand(true).build();
        let bar = adw::TabBar::builder().view(&view).autohide(false).build();
        bar.set_end_action_widget(Some(
            &gtk::Button::builder()
                .icon_name("tab-new-symbolic")
                .action_name("win.new-tab")
                .tooltip_text("New tab (Ctrl+T)")
                .css_classes(["flat"])
                .build(),
        ));
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("pane");
        root.append(&bar);
        root.append(&view);
        Pane { root, view }
    }

    pub fn current(&self) -> Option<Rc<Browser>> {
        find(&self.view.selected_page()?.child())
    }

    fn browsers(&self) -> Vec<Rc<Browser>> {
        (0..self.view.n_pages()).filter_map(|i| find(&self.view.nth_page(i).child())).collect()
    }
}

/// The search box in the middle of the header, with its scope selector and history.
struct SearchBar {
    entry: gtk::SearchEntry,
    scope: gtk::DropDown,
    /// Recent searches (empty box) or completions (while typing).
    history: gtk::Popover,
    /// Completions currently offered; Tab takes the first.
    suggestions: RefCell<Vec<String>>,
    /// Filter chips under the header, shown while searching.
    chips: gtk::Revealer,
    chip_btns: Vec<(Group, gtk::MenuButton)>,
    /// Set while the window itself writes into the entry/dropdown (tab switch), so that isn't a search.
    setting: Cell<bool>,
    debounce: RefCell<Option<glib::SourceId>>,
}

pub struct Win {
    pub window: adw::ApplicationWindow,
    pub panes: [Pane; 2],
    active: Cell<usize>,
    pub ops: Rc<Ops>,
    bar: SearchBar,
    banner: adw::Banner,
    back_btn: gtk::Button,
    fwd_btn: gtk::Button,
    up_btn: gtk::Button,
    item_menu: gio::MenuModel,
    bg_menu: gio::MenuModel,
    pub sidebar: Rc<Sidebar>,
    preview: crate::preview::Preview,
    show_hidden: Cell<bool>,
    me: Weak<Win>,
}

fn menus() -> (gio::Menu, gio::Menu) {
    let item = gio::Menu::new();
    let s = gio::Menu::new();
    s.append(Some("Open"), Some("win.open"));
    s.append(Some("Open With…"), Some("win.open-with"));
    s.append(Some("Open in New Tab"), Some("win.open-tab"));
    s.append(Some("Open in Other Pane"), Some("win.open-other"));
    item.append_section(None, &s);
    let s = gio::Menu::new();
    s.append(Some("Cut"), Some("win.cut"));
    s.append(Some("Copy"), Some("win.copy"));
    s.append(Some("Copy to Other Pane"), Some("win.copy-other"));
    s.append(Some("Move to Other Pane"), Some("win.move-other"));
    item.append_section(None, &s);
    let s = gio::Menu::new();
    s.append(Some("Restore"), Some("win.restore"));
    s.append(Some("Rename…"), Some("win.rename"));
    s.append(Some("Move to Trash"), Some("win.trash"));
    s.append(Some("Delete Permanently…"), Some("win.delete"));
    item.append_section(None, &s);
    let s = gio::Menu::new();
    let tags = gio::Menu::new();
    for t in chimera_index::TAGS {
        tags.append(Some(&format!("{}{}", t[..1].to_uppercase(), &t[1..])), Some(&format!("win.tag::{t}")));
    }
    tags.append(Some("Clear Tags"), Some("win.tag::none"));
    s.append_submenu(Some("Tags"), &tags);
    s.append(Some("Compress…"), Some("win.compress"));
    s.append(Some("Extract Here"), Some("win.extract"));
    s.append(Some("Browse Archive"), Some("win.browse-archive"));
    item.append_section(None, &s);
    let s = gio::Menu::new();
    s.append(Some("Copy Path"), Some("win.copy-path"));
    s.append(Some("Open in Terminal"), Some("win.terminal"));
    s.append(Some("Properties"), Some("win.properties"));
    item.append_section(None, &s);

    let bg = gio::Menu::new();
    let s = gio::Menu::new();
    s.append(Some("New Folder…"), Some("win.new-folder"));
    s.append(Some("Paste"), Some("win.paste"));
    s.append(Some("Empty Trash…"), Some("win.empty-trash"));
    bg.append_section(None, &s);
    let s = gio::Menu::new();
    s.append(Some("Show Hidden Files"), Some("win.show-hidden"));
    s.append(Some("Add to Bookmarks"), Some("win.add-bookmark"));
    s.append(Some("Open in Terminal"), Some("win.terminal"));
    s.append(Some("Properties"), Some("win.properties"));
    bg.append_section(None, &s);
    (item, bg)
}

impl Win {
    pub fn new(app: &adw::Application, left: Vec<Loc>, right: Vec<Loc>, dual: bool) -> Rc<Win> {
        let w = Rc::new_cyclic(|me| Win::build(app, me.clone()));
        WINDOWS.with_borrow_mut(|v| v.push(w.clone()));
        w.wire();
        for l in left {
            w.add_tab(0, l, true);
        }
        for l in right {
            w.add_tab(1, l, true);
        }
        w.set_dual(dual);
        w.update();
        w
    }

    fn build(app: &adw::Application, me: Weak<Win>) -> Win {
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .default_width(1200)
            .default_height(760)
            .title("Chimera")
            .build();
        let toasts = adw::ToastOverlay::new();
        let ops = Ops::new(toasts.clone());
        let (item_menu, bg_menu) = menus();

        let nav_me = me.clone();
        let drop_ops = ops.clone();
        let sidebar = Sidebar::new(
            Rc::new(move |loc| {
                if let Some(w) = nav_me.upgrade() {
                    w.go(loc);
                }
            }),
            Rc::new(move |actions, files, target| {
                if target.uri_scheme().as_deref() == Some("trash") {
                    drop_ops.trash(files);
                } else {
                    drop_ops.transfer(crate::fileops::drop_op(actions, &files, &target), files, target);
                }
            }),
        );

        let panes = [Pane::new(), Pane::new()];
        let paned = gtk::Paned::builder()
            .start_child(&panes[0].root)
            .end_child(&panes[1].root)
            .wide_handle(true)
            .shrink_start_child(false)
            .shrink_end_child(false)
            .build();
        let preview = crate::preview::Preview::new();
        let content = gtk::Paned::builder()
            .start_child(&paned)
            .end_child(&preview.root)
            .shrink_end_child(false)
            .resize_end_child(false)
            .build();

        // the search box: centred, always visible, focused on launch
        let entry = gtk::SearchEntry::builder()
            .placeholder_text("Search files…   e.g. invoice ext:pdf dm:thismonth")
            .hexpand(true)
            .width_request(220)
            .css_classes(["main-search"])
            .build();
        let scope = gtk::DropDown::from_strings(&Scope::ALL.map(|s| s.label()));
        scope.set_tooltip_text(Some("What to search"));
        let help = gtk::MenuButton::builder()
            .icon_name("help-about-symbolic")
            .tooltip_text("Search syntax")
            .css_classes(["flat"])
            .popover(
                &gtk::Popover::builder()
                    .child(
                        &gtk::Label::builder()
                            .label(SYNTAX)
                            .css_classes(["monospace"])
                            .margin_start(6)
                            .margin_end(6)
                            .build(),
                    )
                    .build(),
            )
            .build();
        let row = gtk::Box::builder().spacing(6).css_classes(["search-row"]).build();
        row.append(&entry);
        row.append(&scope);
        row.append(&help);
        let clamp = adw::Clamp::builder()
            .maximum_size(760)
            .tightening_threshold(560)
            .child(&row)
            .hexpand(true)
            .build();
        let history = gtk::Popover::builder().autohide(false).has_arrow(false).halign(gtk::Align::Start).build();
        history.set_parent(&entry);
        let (scope_w, help_w) = (scope.clone(), help.clone());

        // filter chips: each edits its own tokens in the query text, so chips and syntax never disagree
        let chip_box = gtk::Box::builder().spacing(6).css_classes(["chip-bar"]).build();
        chip_box.append(&gtk::Label::builder().label("Filter").css_classes(["dim-label"]).build());
        let mut chip_btns = Vec::new();
        for (g, action) in [(Group::Type, "chip-type"), (Group::Date, "chip-date"), (Group::Size, "chip-size")] {
            let m = gio::Menu::new();
            for (tok, label) in query_ui::options(g) {
                m.append(Some(label), Some(&format!("win.{action}::{tok}")));
            }
            let b = gtk::MenuButton::builder().menu_model(&m).css_classes(["chip"]).always_show_arrow(true).build();
            chip_box.append(&b);
            chip_btns.push((g, b));
        }
        chip_box.append(
            &gtk::Button::builder()
                .label("Clear filters")
                .action_name("win.chip-clear")
                .css_classes(["flat", "chip-clear"])
                .build(),
        );
        let chips = gtk::Revealer::builder().child(&chip_box).reveal_child(false).build();
        let sc = gtk::ShortcutController::new();
        sc.add_shortcut(gtk::Shortcut::new(
            gtk::ShortcutTrigger::parse_string("Escape"),
            Some(gtk::NamedAction::new("win.clear-search")),
        ));
        chip_box.add_controller(sc);

        entry.update_property(&[gtk::accessible::Property::Label("Search files")]);
        scope.update_property(&[gtk::accessible::Property::Label("Search scope")]);
        let bar = SearchBar {
            entry,
            scope,
            history,
            suggestions: RefCell::default(),
            chips,
            chip_btns,
            setting: Cell::new(false),
            debounce: RefCell::default(),
        };
        let banner = adw::Banner::builder()
            .title("The search service (chimera-indexd) isn't running, so search results are unavailable.")
            .button_label("Start Service")
            .revealed(false)
            .build();

        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&clamp));
        let btn = |icon: &str, action: &str, tip: &str| gtk::Button::builder().icon_name(icon).action_name(action).tooltip_text(tip).build();
        let back_btn = btn("go-previous-symbolic", "win.back", "Back (Alt+Left)");
        let fwd_btn = btn("go-next-symbolic", "win.forward", "Forward (Alt+Right)");
        let up_btn = btn("go-up-symbolic", "win.up", "Up (Alt+Up)");
        let split = adw::OverlaySplitView::new();
        let toggle = gtk::ToggleButton::builder()
            .icon_name("sidebar-show-symbolic")
            .tooltip_text("Sidebar (F9)")
            .active(true)
            .build();
        toggle.bind_property("active", &split, "show-sidebar").bidirectional().sync_create().build();
        header.pack_start(&toggle);
        header.pack_start(&back_btn);
        header.pack_start(&fwd_btn);
        header.pack_start(&up_btn);

        let app_menu = gio::Menu::new();
        let s = gio::Menu::new();
        s.append(Some("New Window"), Some("win.new-window"));
        s.append(Some("New Tab"), Some("win.new-tab"));
        app_menu.append_section(None, &s);
        let s = gio::Menu::new();
        s.append(Some("Preferences"), Some("win.preferences"));
        s.append(Some("Command Palette"), Some("win.palette"));
        s.append(Some("Search Syntax"), Some("win.syntax"));
        s.append(Some("About Chimera"), Some("win.about"));
        app_menu.append_section(None, &s);
        header.pack_end(
            &gtk::MenuButton::builder()
                .icon_name("open-menu-symbolic")
                .menu_model(&app_menu)
                .primary(true)
                .tooltip_text("Menu")
                .build(),
        );
        // one View menu instead of six buttons: a centred title needs the same width free on both sides
        let view_menu = gio::Menu::new();
        let v = gio::Menu::new();
        v.append(Some("Details"), Some("win.view::list"));
        v.append(Some("Grid"), Some("win.view::grid"));
        v.append(Some("Columns"), Some("win.view::columns"));
        view_menu.append_section(None, &v);
        let v = gio::Menu::new();
        v.append(Some("Dual Pane"), Some("win.dual"));
        v.append(Some("Preview Pane"), Some("win.preview"));
        v.append(Some("Show Hidden Files"), Some("win.show-hidden"));
        view_menu.append_section(None, &v);
        header.pack_end(
            &gtk::MenuButton::builder()
                .icon_name("view-grid-symbolic")
                .menu_model(&view_menu)
                .tooltip_text("View")
                .build(),
        );

        let main = adw::ToolbarView::new();
        main.add_top_bar(&header);
        main.add_top_bar(&banner);
        main.add_top_bar(&bar.chips);
        // file-operation progress: a thin bar under the header, never squeezing the header itself
        ops.progress.set_valign(gtk::Align::Start);
        ops.progress.set_hexpand(true);
        main.add_top_bar(&ops.progress);
        main.set_content(Some(&content));

        let side = adw::ToolbarView::new();
        side.add_top_bar(&adw::HeaderBar::builder().title_widget(&adw::WindowTitle::new("Chimera", "")).build());
        side.set_content(Some(&sidebar.root));
        split.set_sidebar(Some(&side));
        split.set_content(Some(&main));
        split.set_sidebar_width_fraction(0.18);
        toasts.set_child(Some(&split));
        window.set_content(Some(&toasts));

        // narrow windows: sidebar becomes an overlay, then the scope picker and help hide (Ctrl+F still scopes)
        let bp = adw::Breakpoint::new(adw::BreakpointCondition::parse("max-width: 1000sp").unwrap());
        bp.add_setter(&split, "collapsed", Some(&true.to_value()));
        window.add_breakpoint(bp);
        let bp = adw::Breakpoint::new(adw::BreakpointCondition::parse("max-width: 760sp").unwrap());
        bp.add_setter(&split, "collapsed", Some(&true.to_value()));
        bp.add_setter(&scope_w, "visible", Some(&false.to_value()));
        bp.add_setter(&help_w, "visible", Some(&false.to_value()));
        window.add_breakpoint(bp);

        Win {
            window,
            panes,
            active: Cell::new(0),
            ops,
            bar,
            banner,
            back_btn,
            fwd_btn,
            up_btn,
            item_menu: item_menu.upcast(),
            bg_menu: bg_menu.upcast(),
            sidebar,
            preview,
            show_hidden: Cell::new(false),
            me,
        }
    }

    pub fn current(&self) -> Option<Rc<Browser>> {
        let a = self.active.get();
        self.panes[a].current().or_else(|| self.panes[1 - a].current())
    }

    fn other(&self) -> Option<Rc<Browser>> {
        if !self.panes[1].root.is_visible() {
            return None;
        }
        self.panes[1 - self.active.get()].current()
    }

    pub fn navigate(&self, f: gio::File) {
        self.go(Loc::Dir(f));
    }

    pub fn go(&self, loc: Loc) {
        match self.current() {
            Some(b) => b.go(loc),
            None => {
                self.add_tab(self.active.get(), loc, true);
            }
        }
        if let Some(b) = self.current() {
            b.focus_view();
        }
    }

    pub fn add_tab(&self, pane: usize, loc: Loc, select: bool) -> Rc<Browser> {
        let b = Browser::new(self.ops.clone(), &self.item_menu, &self.bg_menu, loc);
        b.set_show_hidden(self.show_hidden.get());
        let s = crate::prefs::get();
        b.set_mode(s.view());
        b.set_scope(s.scope());
        BROWSERS.with_borrow_mut(|v| v.push(b.clone()));
        let page = self.panes[pane].view.append(&b.root);
        crate::label_icon_buttons(b.root.upcast_ref());
        self.attach(&b, &page);
        if select {
            self.panes[pane].view.set_selected_page(&page);
        }
        b
    }

    /// Point a tab (new, or dragged in from elsewhere) at this window.
    fn attach(&self, b: &Rc<Browser>, page: &adw::TabPage) {
        *b.page.borrow_mut() = Some(page.clone());
        page.set_title(&b.title());
        let me = self.me.clone();
        let bw = Rc::downgrade(b);
        *b.on_change.borrow_mut() = Some(Rc::new(move || {
            if let (Some(w), Some(b)) = (me.upgrade(), bw.upgrade())
                && w.current().is_some_and(|c| Rc::ptr_eq(&c, &b))
            {
                w.update();
            }
        }));
        let me = self.me.clone();
        *b.on_daemon.borrow_mut() = Some(Rc::new(move |ok| {
            if let Some(w) = me.upgrade() {
                w.banner.set_revealed(!ok);
            }
        }));
        let me = self.me.clone();
        let bw = Rc::downgrade(b);
        *b.on_select.borrow_mut() = Some(Rc::new(move || {
            if let (Some(w), Some(b)) = (me.upgrade(), bw.upgrade())
                && w.current().is_some_and(|c| Rc::ptr_eq(&c, &b))
            {
                w.refresh_preview();
                w.update_actions();
            }
        }));
    }

    pub fn set_dual(&self, on: bool) {
        self.panes[1].root.set_visible(on);
        if on && self.panes[1].view.n_pages() == 0 {
            let loc = self.current().map_or_else(|| gio::File::for_path(glib::home_dir()), |b| b.base_dir());
            let keep = self.active.get();
            self.add_tab(1, Loc::Dir(loc), true);
            // stay in the pane you were in; the new one just opens beside it
            self.active.set(keep);
        }
        if !on && self.active.get() == 1 {
            self.set_active(0);
        }
        if let Some(a) = self.window.lookup_action("dual").and_downcast::<gio::SimpleAction>() {
            a.set_state(&on.to_variant());
        }
        for (i, p) in self.panes.iter().enumerate() {
            p.root.set_css_classes(if on && i != self.active.get() {
                &["pane", "inactive-pane"]
            } else {
                &["pane"]
            });
        }
    }

    fn set_active(&self, i: usize) {
        if self.active.replace(i) != i {
            self.set_dual(self.panes[1].root.is_visible());
            self.update();
        }
    }

    fn pane_emptied(&self, i: usize) {
        if i == 1 {
            self.set_dual(false);
        } else if self.panes[1].view.n_pages() > 0 && self.panes[1].root.is_visible() {
            while self.panes[1].view.n_pages() > 0 {
                let p = self.panes[1].view.nth_page(0);
                self.panes[1].view.transfer_page(&p, &self.panes[0].view, self.panes[0].view.n_pages());
            }
            self.set_dual(false);
        } else {
            self.window.close();
        }
    }

    /// Grey out menu items that don't apply to the current selection/location.
    fn update_actions(&self) {
        let b = self.current();
        let infos = b.as_ref().map(|b| b.selected_infos()).unwrap_or_default();
        let any = !infos.is_empty();
        let folder = infos.iter().any(|i| i.file_type() == gio::FileType::Directory);
        let archive = infos.iter().any(|i| {
            let n = i.name().to_string_lossy().to_lowercase();
            [".zip", ".7z", ".rar", ".tar", ".tgz", ".txz", ".gz", ".xz", ".zst", ".bz2"]
                .iter()
                .any(|e| n.ends_with(e))
        });
        let trash = b.as_ref().is_some_and(|b| b.is_trash());
        let in_dir = b.as_ref().is_some_and(|b| b.dir().is_some()) && !trash;
        for (name, on) in [
            ("open", any),
            ("open-with", any),
            ("open-tab", folder),
            ("open-other", folder || in_dir),
            ("cut", any),
            ("copy", any),
            ("copy-other", any),
            ("move-other", any),
            ("rename", infos.len() == 1 && !trash),
            ("trash", any && !trash),
            ("restore", any && trash),
            ("empty-trash", trash),
            ("delete", any),
            ("tag", any),
            ("compress", any),
            ("extract", archive),
            ("browse-archive", archive),
            ("paste", in_dir),
            ("new-folder", in_dir),
        ] {
            if let Some(a) = self.window.lookup_action(name).and_downcast::<gio::SimpleAction>() {
                a.set_enabled(on);
            }
        }
    }

    fn refresh_preview(&self) {
        if !self.preview.root.is_visible() {
            return;
        }
        let Some(b) = self.current() else { return };
        let infos = b.selected_infos();
        let git = match infos.as_slice() {
            [i] => crate::browser::info_file(i).and_then(|f| b.git_status(&f)),
            _ => None,
        };
        self.preview.show(&infos, git);
    }

    /// Reflect the current tab in the header.
    pub fn update(&self) {
        self.refresh_preview();
        self.update_actions();
        let Some(b) = self.current() else { return };
        self.back_btn.set_sensitive(b.can_back());
        self.fwd_btn.set_sensitive(b.can_forward());
        self.window.set_title(Some(&format!("{} — Chimera", b.title())));
        if let Some(a) = self.window.lookup_action("view").and_downcast::<gio::SimpleAction>() {
            a.set_state(&b.mode().name().to_variant());
        }
        self.up_btn.set_sensitive(b.is_searching() || b.base_dir().parent().is_some());
        // the search box mirrors the active tab (tab switches, sidebar searches). The tab keeps the
        // query trimmed, so compare trimmed: rewriting "invoice " as "invoice" while you type ate the
        // space and threw the cursor back to the start.
        let q = b.query().unwrap_or_default();
        if self.bar.entry.text().trim() != q.trim() {
            self.set_entry(&q);
        }
        self.sync_chips();
        let si = Scope::ALL.iter().position(|s| *s == b.scope()).unwrap_or(0) as u32;
        if self.bar.scope.selected() != si {
            self.bar.setting.set(true);
            self.bar.scope.set_selected(si);
            self.bar.setting.set(false);
        }
    }

    fn set_entry(&self, text: &str) {
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
    fn show_suggestions(&self) {
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
    fn take_suggestion(&self, full: &str, run: bool) {
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
    fn sync_chips(&self) {
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

    /// Every keystroke (after the entry's 75 ms debounce).
    fn on_search_changed(&self) {
        if let Some(b) = self.current() {
            b.set_search(&self.bar.entry.text());
        }
    }

    /// Enter: remember the query and move into the results.
    fn commit_search(&self) {
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

    /// Start the daemon through polkit, then retry the current search.
    fn start_service(&self) {
        let me = self.me.clone();
        glib::spawn_future_local(async move {
            let ok = gio::spawn_blocking(|| {
                std::process::Command::new("pkexec")
                    .args(["systemctl", "start", "chimera-indexd"])
                    .status()
                    .is_ok_and(|s| s.success())
            })
            .await
            .unwrap_or(false);
            let Some(w) = me.upgrade() else { return };
            if !ok {
                w.ops.toast("Couldn't start the search service");
                return;
            }
            // the first scan takes a few seconds before the socket appears
            for _ in 0..60 {
                if std::os::unix::net::UnixStream::connect(chimera_index::proto::socket_path()).is_ok() {
                    w.banner.set_revealed(false);
                    if let Some(b) = w.current() {
                        b.reload();
                    }
                    return;
                }
                glib::timeout_future(std::time::Duration::from_millis(500)).await;
            }
            w.ops.toast("The search service started but isn't answering yet");
        });
    }

    pub fn save_session(&self) {
        let toks = |p: &Pane| p.browsers().iter().map(|b| b.loc().to_token()).collect();
        let s = Session {
            left: toks(&self.panes[0]),
            right: if self.panes[1].root.is_visible() { toks(&self.panes[1]) } else { vec![] },
            dual: self.panes[1].root.is_visible(),
            hidden: self.show_hidden.get(),
            mode: self.current().map_or("list", |b| b.mode().name()).into(),
            size: self.window.default_size(),
        };
        let p = session_file();
        let _ = std::fs::create_dir_all(p.parent().unwrap());
        let _ = std::fs::write(p, serde_json::to_string_pretty(&s).unwrap());
    }

    fn act(&self, name: &str, f: impl Fn(&Rc<Win>) + 'static) {
        let a = gio::SimpleAction::new(name, None);
        let me = self.me.clone();
        a.connect_activate(move |_, _| {
            if let Some(w) = me.upgrade() {
                f(&w);
            }
        });
        self.window.add_action(&a);
    }

    /// Selected files of the current tab, or nothing.
    fn sel(&self) -> Vec<gio::File> {
        self.current().map(|b| b.selected_files()).unwrap_or_default()
    }

    /// Folder actions apply to: a single selected folder, else the current folder.
    fn target_dir(&self) -> Option<gio::File> {
        let b = self.current()?;
        let infos = b.selected_infos();
        match infos.as_slice() {
            [i] if i.file_type() == gio::FileType::Directory => crate::browser::info_file(i),
            _ => b.dir(),
        }
    }

    fn wire(self: &Rc<Self>) {
        for (i, p) in self.panes.iter().enumerate() {
            let me = self.me.clone();
            p.view.connect_selected_page_notify(move |_| {
                if let Some(w) = me.upgrade() {
                    w.set_active(i);
                    w.update();
                }
            });
            p.view.connect_close_page(|view, page| {
                unregister(&page.child());
                view.close_page_finish(page, true);
                glib::Propagation::Stop
            });
            let me = self.me.clone();
            p.view.connect_page_attached(move |_, page, _| {
                if let (Some(w), Some(b)) = (me.upgrade(), find(&page.child())) {
                    w.attach(&b, page);
                }
            });
            let me = self.me.clone();
            p.view.connect_page_detached(move |view, _, _| {
                if view.n_pages() == 0
                    && let Some(w) = me.upgrade()
                {
                    // let AdwTabView finish the detach before we move or close things
                    glib::idle_add_local_once(move || w.pane_emptied(i));
                }
            });
            let me = self.me.clone();
            p.view.connect_create_window(move |_| {
                let w = me.upgrade()?;
                let nw = Win::new(&w.window.application()?.downcast().ok()?, vec![], vec![], false);
                nw.window.present();
                Some(nw.panes[0].view.clone())
            });
            let fc = gtk::EventControllerFocus::new();
            let me = self.me.clone();
            fc.connect_enter(move |_| me.upgrade().into_iter().for_each(|w| w.set_active(i)));
            p.root.add_controller(fc);
            let g = gtk::GestureClick::builder().propagation_phase(gtk::PropagationPhase::Capture).build();
            let me = self.me.clone();
            g.connect_pressed(move |_, _, _, _| me.upgrade().into_iter().for_each(|w| w.set_active(i)));
            p.root.add_controller(g);
        }

        // File keys. On the window in the bubble phase: text fields get Ctrl+C/V/Z, Delete, Backspace
        // first and stop them; everything else (including "focus nowhere" after navigating) lands here.
        let sc = gtk::ShortcutController::new();
        sc.set_propagation_phase(gtk::PropagationPhase::Bubble);
        for (key, action) in [
            ("Delete", "win.trash"),
            ("KP_Delete", "win.trash"),
            ("<Shift>Delete", "win.delete"),
            ("F2", "win.rename"),
            ("<Control>c", "win.copy"),
            ("<Control>x", "win.cut"),
            ("<Control>v", "win.paste"),
            ("<Control>z", "win.undo"),
            ("BackSpace", "win.up"),
            ("<Alt>Return", "win.properties"),
            ("<Control><Shift>c", "win.copy-path"),
            ("space", "win.preview"),
            ("Menu", "win.context-menu"),
            ("<Shift>F10", "win.context-menu"),
            ("Escape", "win.clear-search"),
        ] {
            sc.add_shortcut(gtk::Shortcut::new(gtk::ShortcutTrigger::parse_string(key), Some(gtk::NamedAction::new(action))));
        }
        self.window.add_controller(sc);

        // start typing anywhere (outside a text field) to search; also bubble phase, for the same reason
        let k = gtk::EventControllerKey::new();
        k.set_propagation_phase(gtk::PropagationPhase::Bubble);
        let me = self.me.clone();
        k.connect_key_pressed(move |_, key, _, mods| {
            let Some(w) = me.upgrade() else { return glib::Propagation::Proceed };
            if !crate::prefs::get().type_to_search {
                return glib::Propagation::Proceed;
            }
            let printable = key.to_unicode().filter(|c| !c.is_control() && *c != ' ');
            if mods.intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK) || printable.is_none() {
                return glib::Propagation::Proceed;
            }
            w.focus_search(Some(&printable.unwrap().to_string()));
            glib::Propagation::Stop
        });
        self.window.add_controller(k);

        // search box
        let me = self.me.clone();
        // our own debounce on `changed`: GtkSearchEntry's delayed search-changed fires after the
        // `setting` guard is already reset, and didn't fire reliably while typing
        self.bar.entry.connect_changed(move |_| {
            let Some(w) = me.upgrade() else { return };
            w.show_suggestions();
            w.sync_chips();
            if w.bar.setting.get() {
                return;
            }
            if let Some(id) = w.bar.debounce.take() {
                id.remove();
            }
            let me = w.me.clone();
            *w.bar.debounce.borrow_mut() = Some(glib::timeout_add_local_once(std::time::Duration::from_millis(75), move || {
                if let Some(w) = me.upgrade() {
                    w.bar.debounce.take();
                    w.on_search_changed();
                }
            }));
        });
        let me = self.me.clone();
        self.bar
            .entry
            .connect_activate(move |_| me.upgrade().into_iter().for_each(|w| w.commit_search()));
        // Esc: clear the search (back to the folder); Esc on an empty box: back to the file list
        let me = self.me.clone();
        self.bar.entry.connect_stop_search(move |e| {
            let Some(w) = me.upgrade() else { return };
            w.bar.history.popdown();
            if e.text().is_empty() {
                w.current().into_iter().for_each(|b| b.focus_view());
            } else {
                e.set_text("");
                w.current().into_iter().for_each(|b| b.clear_search());
            }
        });
        let k = gtk::EventControllerKey::new();
        let me = self.me.clone();
        k.connect_key_pressed(move |_, key, _, _| {
            let Some(w) = me.upgrade() else { return glib::Propagation::Proceed };
            if key == gdk::Key::Tab {
                let first = w.bar.suggestions.borrow().first().cloned();
                if let Some(full) = first {
                    w.take_suggestion(&full, false);
                    return glib::Propagation::Stop;
                }
            }
            if key == gdk::Key::Down
                && let Some(b) = w.current()
            {
                w.bar.history.popdown();
                b.focus_view();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        self.bar.entry.add_controller(k);
        let fc = gtk::EventControllerFocus::new();
        let me = self.me.clone();
        // recent searches on a click into the box (and on Ctrl+F / F6), not whenever it gets focus:
        // it's focused at launch, and the list popping up unasked covered the files
        let g = gtk::GestureClick::builder().propagation_phase(gtk::PropagationPhase::Capture).build();
        let me2 = self.me.clone();
        g.connect_released(move |_, _, _, _| {
            let me = me2.clone();
            glib::idle_add_local_once(move || me.upgrade().into_iter().for_each(|w| w.show_suggestions()));
        });
        self.bar.entry.add_controller(g);
        fc.connect_leave(move |_| {
            // let a click on a history row land before hiding the list
            if let Some(w) = me.upgrade() {
                let h = w.bar.history.clone();
                glib::timeout_add_local_once(std::time::Duration::from_millis(200), move || h.popdown());
            }
        });
        self.bar.entry.add_controller(fc);
        let me = self.me.clone();
        self.bar.scope.connect_selected_notify(move |d| {
            let Some(w) = me.upgrade() else { return };
            if w.bar.setting.get() {
                return;
            }
            if let Some(b) = w.current() {
                b.set_scope(Scope::ALL[d.selected().min(2) as usize]);
            }
        });
        let me = self.me.clone();
        self.banner
            .connect_button_clicked(move |_| me.upgrade().into_iter().for_each(|w| w.start_service()));
        if std::os::unix::net::UnixStream::connect(chimera_index::proto::socket_path()).is_err() {
            self.banner.set_revealed(true);
        }

        let me = self.me.clone();
        self.window.connect_close_request(move |_| {
            if let Some(w) = me.upgrade() {
                w.save_session();
                for p in &w.panes {
                    for b in p.browsers() {
                        unregister(b.root.upcast_ref());
                    }
                }
                WINDOWS.with_borrow_mut(|v| v.retain(|x| !Rc::ptr_eq(x, &w)));
            }
            glib::Propagation::Proceed
        });

        self.actions();
        crate::label_icon_buttons(self.window.upcast_ref());
    }

    fn actions(self: &Rc<Self>) {
        let home = || gio::File::for_path(glib::home_dir());
        self.act("new-tab", move |w| {
            let loc = w.current().and_then(|b| b.dir()).unwrap_or_else(home);
            w.add_tab(w.active.get(), Loc::Dir(loc), true).focus_view();
        });
        self.act("close-tab", |w| {
            let v = &w.panes[w.active.get()].view;
            if let Some(p) = v.selected_page() {
                v.close_page(&p);
            }
        });
        self.act("new-window", move |w| {
            let loc = w.current().map_or_else(|| Loc::Dir(home()), |b| b.loc());
            if let Some(app) = w.window.application().and_downcast::<adw::Application>() {
                Win::new(&app, vec![loc], vec![], false).window.present();
            }
        });
        self.act("back", |w| w.current().into_iter().for_each(|b| b.back()));
        self.act("forward", |w| w.current().into_iter().for_each(|b| b.forward()));
        self.act("up", |w| w.current().into_iter().for_each(|b| b.up()));
        self.act("reload", |w| w.current().into_iter().for_each(|b| b.reload()));
        self.act("edit-location", |w| w.current().into_iter().for_each(|b| b.edit_location()));
        self.act("search-here", |w| {
            if let Some(b) = w.current() {
                b.set_scope(Scope::Folder);
                w.update();
            }
            w.focus_search(None);
            w.show_suggestions();
        });
        self.act("search-everywhere", |w| {
            if let Some(b) = w.current() {
                b.set_scope(Scope::Everywhere);
                w.update();
            }
            w.focus_search(None);
            w.show_suggestions();
        });
        self.act("open", |w| {
            let Some(b) = w.current() else { return };
            for i in b.selected_infos() {
                b.open_info(&i);
            }
        });
        self.act("open-with", |w| {
            if let (Some(b), Some(f)) = (w.current(), w.sel().into_iter().next()) {
                b.launch(&f, true);
            }
        });
        self.act("open-tab", |w| {
            let Some(b) = w.current() else { return };
            for i in b.selected_infos().iter().filter(|i| i.file_type() == gio::FileType::Directory) {
                if let Some(f) = crate::browser::info_file(i) {
                    w.add_tab(w.active.get(), Loc::Dir(f), false);
                }
            }
        });
        self.act("open-other", |w| {
            let Some(d) = w.target_dir() else { return };
            if !w.panes[1].root.is_visible() {
                w.set_dual(true);
            }
            if let Some(o) = w.other() {
                o.go(Loc::Dir(d));
            }
        });
        self.act("copy", |w| w.ops.set_clipboard(&w.sel(), false));
        self.act("cut", |w| w.ops.set_clipboard(&w.sel(), true));
        self.act("paste", |w| match w.current().and_then(|b| b.dir()) {
            Some(d) => w.ops.paste(d),
            None => w.ops.toast("Open a folder to paste into"),
        });
        for (name, op) in [("copy-other", Op::Copy), ("move-other", Op::Move)] {
            self.act(name, move |w| match w.other().and_then(|o| o.dir()) {
                Some(d) => w.ops.transfer(op, w.sel(), d),
                None => w.ops.toast("Turn on dual pane (F3) and open a folder on the other side"),
            });
        }
        self.act("rename", |w| {
            if w.current().is_some_and(|b| b.rename_inline()) {
                return;
            }
            if let Some(f) = w.sel().into_iter().next() {
                w.ops.rename(f);
            }
        });
        self.act("trash", |w| w.ops.trash(w.sel()));
        self.act("restore", |w| w.ops.restore(w.sel()));
        self.act("empty-trash", |w| w.ops.empty_trash());
        self.act("delete", |w| w.ops.delete(w.sel()));
        self.act("new-folder", |w| match w.current().and_then(|b| b.dir()) {
            Some(d) => w.ops.new_folder(d),
            None => w.ops.toast("Open a folder first"),
        });
        self.act("undo", |w| w.ops.undo());
        self.act("copy-path", |w| {
            let files = w.sel();
            let files = if files.is_empty() {
                w.current().and_then(|b| b.dir()).into_iter().collect()
            } else {
                files
            };
            let text = files
                .iter()
                .map(|f| f.path().map_or_else(|| f.uri().to_string(), |p| p.display().to_string()))
                .collect::<Vec<_>>()
                .join("\n");
            w.window.clipboard().set_text(&text);
            w.ops.toast("Path copied");
        });
        self.act("add-bookmark", |w| {
            // in a search, Ctrl+D saves the search instead
            if let Some(q) = w.current().and_then(|b| b.query()) {
                sidebar::save_search(&q);
                w.sidebar.refresh_searches();
                w.ops.toast("Search saved");
                return;
            }
            if let Some(d) = w.target_dir() {
                match sidebar::add_bookmark(&d) {
                    Ok(()) => w.sidebar.refresh_bookmarks(),
                    Err(e) => w.ops.toast(&e.to_string()),
                }
            }
        });
        self.act("terminal", |w| {
            let Some(dir) = w.target_dir().and_then(|d| d.path()) else {
                w.ops.toast("Not a local folder");
                return;
            };
            let d = dir.display().to_string();
            let tries: [(&str, Vec<String>); 3] = [
                ("ptyxis", vec!["--new-window".into(), format!("--working-directory={d}")]),
                ("gnome-terminal", vec![format!("--working-directory={d}")]),
                ("xdg-terminal-exec", vec![]),
            ];
            if !tries
                .iter()
                .any(|(cmd, args)| std::process::Command::new(cmd).args(args).current_dir(&dir).spawn().is_ok())
            {
                w.ops.toast("No terminal found");
            }
        });
        self.act("context-menu", |w| w.current().into_iter().for_each(|b| b.context_menu()));
        self.act("other-pane", |w| {
            if w.panes[1].root.is_visible() {
                w.set_active(1 - w.active.get());
                w.current().into_iter().for_each(|b| b.focus_view());
            }
        });
        self.act("syntax", |w| {
            let d = adw::AlertDialog::new(Some("Search syntax"), None);
            let l = gtk::Label::builder()
                .label(SYNTAX)
                .css_classes(["monospace"])
                .xalign(0.0)
                .selectable(true)
                .build();
            d.set_extra_child(Some(&l));
            d.add_response("ok", "OK");
            d.present(Some(&w.window));
        });
        self.act("about", |w| {
            adw::AboutDialog::builder()
                .application_name("Chimera")
                .application_icon("dev.chimera.Chimera")
                .version(env!("CARGO_PKG_VERSION"))
                .comments("Everything's instant search, Files' interface, for Linux.")
                .license_type(gtk::License::MitX11)
                .build()
                .present(Some(&w.window));
        });

        self.act("properties", |w| {
            let Some(b) = w.current() else { return };
            let mut infos = b.selected_infos();
            if infos.is_empty()
                && let Some(d) = b.dir()
                && let Ok(i) = d.query_info(crate::browser::ATTRS, gio::FileQueryInfoFlags::NONE, gio::Cancellable::NONE)
            {
                i.set_attribute_object("standard::file", &d);
                infos.push(i);
            }
            let ops = w.ops.clone();
            crate::props::show(&w.window, infos, move |m| ops.toast(m));
        });
        self.act("palette", |w| crate::palette::show(&w.window));
        self.act("preferences", |w| {
            let (me, ops) = (w.me.clone(), w.ops.clone());
            crate::prefs::show(
                &w.window,
                move || {
                    if let Some(w) = me.upgrade() {
                        w.set_hidden(crate::prefs::get().show_hidden);
                    }
                },
                move |m| ops.toast(m),
            );
        });
        self.act("compress", |w| {
            let files = w.sel();
            let Some(dir) = files.first().and_then(|f| f.parent()) else { return };
            let default = match files.as_slice() {
                [one] => format!("{}.zip", one.basename().unwrap_or_default().display()),
                _ => "Archive.zip".into(),
            };
            let ops = w.ops.clone();
            w.ops
                .ask_name("Compress", "Create", &default, move |name| ops.compress(files.clone(), dir.clone(), name));
        });
        self.act("extract", |w| {
            for f in w.sel() {
                w.ops.extract(f);
            }
        });
        self.act("browse-archive", |w| {
            if let Some(f) = w.sel().into_iter().next() {
                // GVfs' archive backend mounts it; Browser::show handles the mount
                w.navigate(gio::File::for_uri(&format!("archive://{}", glib::Uri::escape_string(&f.uri(), None, false))));
            }
        });
        let me = self.me.clone();
        let a = gio::SimpleAction::new("tag", Some(glib::VariantTy::STRING));
        a.connect_activate(move |_, v| {
            let (Some(w), Some(t)) = (me.upgrade(), v.and_then(|v| v.get::<String>())) else {
                return;
            };
            let files = w.sel();
            let bit = chimera_index::tag_bits(&t);
            // toggle: if every selected file has it, remove it; otherwise add it to all
            let all = files.iter().all(|f| crate::tags::read(f) & bit != 0);
            for f in &files {
                let cur = crate::tags::read(f);
                let new = if t == "none" {
                    0
                } else if all {
                    cur & !bit
                } else {
                    cur | bit
                };
                if let Err(e) = crate::tags::write(f, new) {
                    w.ops.toast(&format!("Couldn't tag: {e}"));
                    return;
                }
            }
        });
        self.window.add_action(&a);

        for (action, g) in [("chip-type", Group::Type), ("chip-date", Group::Date), ("chip-size", Group::Size)] {
            let me = self.me.clone();
            let a = gio::SimpleAction::new_stateful(action, Some(glib::VariantTy::STRING), &"".to_variant());
            a.connect_activate(move |_, v| {
                let (Some(w), Some(tok)) = (me.upgrade(), v.and_then(|v| v.get::<String>())) else {
                    return;
                };
                let q = query_ui::set_group(&w.bar.entry.text(), g, &tok);
                w.bar.entry.set_text(&q);
                w.bar.entry.set_position(-1);
            });
            self.window.add_action(&a);
        }
        // F6 / Shift+F6: jump between the three regions (search box, file list, sidebar)
        for (name, step) in [("focus-next-region", 1usize), ("focus-prev-region", 2)] {
            self.act(name, move |w| {
                let focus = GtkWindowExt::focus(&w.window);
                let inside = |r: &gtk::Widget| focus.as_ref().is_some_and(|f| f == r || f.is_ancestor(r));
                let regions: [gtk::Widget; 3] = [
                    w.bar.entry.clone().upcast(),
                    w.current().map_or_else(|| w.bar.entry.clone().upcast(), |b| b.root.clone().upcast()),
                    w.sidebar.root.clone().upcast(),
                ];
                let cur = regions.iter().position(inside).unwrap_or(0);
                match (cur + step) % 3 {
                    0 => {
                        w.focus_search(None);
                        w.show_suggestions();
                    }
                    1 => w.current().into_iter().for_each(|b| b.focus_view()),
                    _ => {
                        w.sidebar.root.child_focus(gtk::DirectionType::TabForward);
                    }
                }
            });
        }
        // Esc from the file list or filter bar (the search box handles its own Esc)
        self.act("clear-search", |w| {
            if w.current().is_some_and(|b| b.is_searching()) {
                w.bar.entry.set_text("");
                if let Some(b) = w.current() {
                    b.clear_search();
                    b.focus_view();
                }
            }
        });
        self.act("chip-clear", |w| {
            let mut q = w.bar.entry.text().to_string();
            for g in [Group::Type, Group::Date, Group::Size] {
                q = query_ui::set_group(&q, g, "");
            }
            w.bar.entry.set_text(&q);
        });

        // a checkbox in the menu; flipping it rewires folders + "Show in Folder" for this user
        let me = self.me.clone();
        let a = gio::SimpleAction::new_stateful("default-fm", None, &crate::filemanager1::is_default().to_variant());
        a.connect_activate(move |a, _| {
            let on = !a.state().and_then(|s| s.get::<bool>()).unwrap_or(false);
            let Some(w) = me.upgrade() else { return };
            match crate::filemanager1::make_default(on) {
                Ok(()) => {
                    a.set_state(&on.to_variant());
                    w.ops.toast(if on {
                        "Chimera now opens folders and \u{201c}Show in Folder\u{201d}"
                    } else {
                        "Folders open in Files (Nautilus) again"
                    });
                }
                Err(e) => w.ops.toast(&format!("Couldn't change the default: {e}")),
            }
        });
        self.window.add_action(&a);

        // stateful
        let me = self.me.clone();
        let a = gio::SimpleAction::new_stateful("preview", None, &false.to_variant());
        a.connect_activate(move |a, _| {
            let on = !a.state().and_then(|s| s.get::<bool>()).unwrap_or(false);
            a.set_state(&on.to_variant());
            if let Some(w) = me.upgrade() {
                w.preview.root.set_visible(on);
                w.refresh_preview();
            }
        });
        self.window.add_action(&a);
        let me = self.me.clone();
        let a = gio::SimpleAction::new_stateful("show-hidden", None, &false.to_variant());
        a.connect_activate(move |a, _| {
            let on = !a.state().and_then(|s| s.get::<bool>()).unwrap_or(false);
            a.set_state(&on.to_variant());
            crate::prefs::update(|s| s.show_hidden = on);
            if let Some(w) = me.upgrade() {
                w.show_hidden.set(on);
                BROWSERS.with_borrow(|v| v.iter().for_each(|b| b.set_show_hidden(on)));
            }
        });
        self.window.add_action(&a);
        let me = self.me.clone();
        let a = gio::SimpleAction::new_stateful("dual", None, &false.to_variant());
        a.connect_activate(move |a, _| {
            let on = !a.state().and_then(|s| s.get::<bool>()).unwrap_or(false);
            me.upgrade().into_iter().for_each(|w| w.set_dual(on));
        });
        self.window.add_action(&a);
        let me = self.me.clone();
        let a = gio::SimpleAction::new_stateful("view", Some(glib::VariantTy::STRING), &"list".to_variant());
        a.connect_activate(move |a, v| {
            let Some(m) = v.and_then(|v| v.get::<String>()) else { return };
            a.set_state(&m.to_variant());
            if let Some(b) = me.upgrade().and_then(|w| w.current()) {
                b.set_mode(Mode::parse(&m));
                b.focus_view();
            }
        });
        self.window.add_action(&a);
    }

    pub fn set_hidden(&self, on: bool) {
        if on != self.show_hidden.get() {
            let _ = WidgetExt::activate_action(&self.window, "win.show-hidden", None);
        }
    }
}

pub const SYNTAX: &str = "a b        both          a|b       either        !a      not
\"a b\"      literal       *.rs      wildcard (whole name)
a/b        path match    path:x    path match    case:X  case-sensitive
regex:^x   file:  folder:   ext:pdf;docx
size:>1gb  size:1mb..10mb  size:empty|tiny|small|medium|large|huge|gigantic
dm:today|yesterday|thisweek|thismonth|thisyear|last7days|2026-10|>2026-10-01
audio:  video:  pic:  doc:  zip:  exe:

Ctrl+F searches inside the current folder.";
