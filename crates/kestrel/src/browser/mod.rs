//! One tab: a folder (any GIO location) with an optional search layered on top.
//!
//! The folder listing stays loaded while searching, with its own selection and scroll position,
//! so clearing the search returns exactly where you were.

mod cells;
mod format;
mod miller;
mod searching;

pub use cells::set_icon;
use cells::*;
pub use format::*;

use crate::fileops::Ops;
use crate::search::{SearchError, SearchModel};
use adw::prelude::*;
use gtk::{gdk, gio, glib, graphene};
use kestrel_index::Sort;
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

pub const ATTRS: &str = "standard::*,time::modified,unix::mode,unix::nlink,owner::user,xattr::*,thumbnail::path,thumbnail::is-valid,id::filesystem";

/// What a tab is told to show (CLI, session, sidebar).
#[derive(Clone, Debug)]
pub enum Loc {
    Dir(gio::File),
    Search(String),
}

impl Loc {
    /// Session/CLI form: a URI, or `search:<query>`.
    pub fn to_token(&self) -> String {
        match self {
            Loc::Dir(f) => f.uri().to_string(),
            Loc::Search(q) => format!("search:{q}"),
        }
    }

    pub fn from_token(t: &str) -> Loc {
        match t.strip_prefix("search:") {
            Some(q) => Loc::Search(q.into()),
            None => Loc::Dir(gio::File::for_commandline_arg(t)),
        }
    }
}

/// A folder's title, worked out from its path/URI alone: never touches the disk or GVfs. A query
/// here ran on the main thread and froze the window for 20+ s on a dead network mount (and on
/// trash:///, whose backend checks every mount for a .Trash folder).
/// Pressing on a file and dragging moves the file; only a press on empty space starts a rubber-band
/// selection, as in GNOME Files. With rubber-banding always on, the list's own drag gesture beat
/// the file's drag source and files couldn't be dragged anywhere.
fn drag_beats_rubberband(view: &gtk::Widget, set: impl Fn(&gtk::Widget, bool) + 'static) {
    let press = gtk::GestureClick::builder().button(1).propagation_phase(gtk::PropagationPhase::Capture).build();
    press.connect_pressed(move |g, _, x, y| {
        let Some(v) = g.widget() else { return };
        let mut hit = v.pick(x, y, gtk::PickFlags::DEFAULT);
        let mut on_item = false;
        while let Some(w) = hit {
            // the cell's padding counts too: no rubber band starts from a file's row or tile
            let cell = matches!(w.type_().name(), "GtkColumnViewCellWidget" | "GtkListItemWidget" | "GtkGridViewChild");
            if w.has_css_class("drag-item") || cell {
                on_item = true;
                break;
            }
            hit = w.parent();
        }
        set(&v, !on_item);
    });
    view.add_controller(press);
}

/// Space opens the preview, as in GNOME Files. The list widgets bind Space to "toggle this row's
/// selection" and see the key before the window's shortcut does, so take it first.
fn space_previews(view: &gtk::Widget) {
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    keys.connect_key_pressed(|k, key, _, mods| {
        if key != gtk::gdk::Key::space
            || mods.intersects(gtk::gdk::ModifierType::CONTROL_MASK | gtk::gdk::ModifierType::SHIFT_MASK | gtk::gdk::ModifierType::ALT_MASK)
        {
            return glib::Propagation::Proceed;
        }
        let _ = k.widget().and_then(|w| w.activate_action("win.preview", None).ok());
        glib::Propagation::Stop
    });
    view.add_controller(keys);
}

pub fn dir_title(f: &gio::File) -> String {
    match f.path() {
        Some(p) if p == glib::home_dir() => "Home".into(),
        Some(p) if p.as_os_str() == "/" => "File System".into(),
        Some(p) => glib::filename_display_basename(&p).to_string(),
        None => match (f.uri_scheme().as_deref(), f.basename()) {
            (Some("trash"), _) if f.parent().is_none() => "Trash".into(),
            (Some("recent"), _) => "Recent".into(),
            (Some("network"), _) => "Network".into(),
            (Some("computer"), _) => "Computer".into(),
            (Some("archive"), _) if f.parent().is_none() => crate::archive::archive_source(f)
                .and_then(|a| a.basename())
                .map_or_else(|| "Archive".into(), |b| b.to_string_lossy().into_owned()),
            (_, Some(b)) if b.as_os_str() != "/" => b.to_string_lossy().into_owned(),
            _ => f.uri().to_string(),
        },
    }
}

/// What a search covers. Folder scopes become an `under:`/`parent:` term on the folder the tab is in.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Scope {
    #[default]
    Everywhere,
    Folder,
    FolderOnly,
}

impl Scope {
    pub const ALL: [Scope; 3] = [Scope::Everywhere, Scope::Folder, Scope::FolderOnly];

    pub fn label(self) -> &'static str {
        match self {
            Scope::Everywhere => "Everywhere",
            Scope::Folder => "This folder",
            Scope::FolderOnly => "This folder only",
        }
    }

    /// The extra query term, or None (Everywhere, or a folder the index can't see).
    fn term(self, dir: &gio::File) -> Option<String> {
        let p = dir.path()?;
        let p = p.display();
        match self {
            Scope::Everywhere => None,
            Scope::Folder => Some(format!("\"under:{p}\"")),
            Scope::FolderOnly => Some(format!("\"parent:{p}\"")),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    List,
    Grid,
    Columns,
}

impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Mode::List => "list",
            Mode::Grid => "grid",
            Mode::Columns => "columns",
        }
    }
    pub fn parse(s: &str) -> Mode {
        match s {
            "grid" => Mode::Grid,
            "columns" => Mode::Columns,
            _ => Mode::List,
        }
    }
}

/// A callback the window installs on a tab after creating it.
pub type Hook<F> = RefCell<Option<Rc<F>>>;

type PendingSelect = (Vec<String>, Box<dyn FnOnce()>);

/// Result of the last search, for the status bar.
#[derive(Clone)]
enum SearchState {
    Running,
    Done(crate::search::Outcome),
    BadQuery(String),
    Down(String),
    Indexing,
}

pub struct Browser {
    pub root: gtk::Box,
    stack: gtk::Stack,
    list: gtk::ColumnView,
    grid: gtk::GridView,
    list_scroll: gtk::ScrolledWindow,
    grid_scroll: gtk::ScrolledWindow,
    miller: gtk::Box,
    miller_scroll: gtk::ScrolledWindow,
    /// The column view's selection (its FileInfo, so nothing has to be queried again).
    miller_sel: RefCell<Option<gio::FileInfo>>,
    status: adw::StatusPage,
    statusbar: gtk::Label,
    // location bar
    loc_stack: gtk::Stack,
    crumbs: gtk::Box,
    loc_label: gtk::Label,
    loc_entry: gtk::Entry,
    dirlist: gtk::DirectoryList,
    filter: gtk::CustomFilter,
    search: SearchModel,
    dir_sel: gtk::MultiSelection,
    search_sel: gtk::MultiSelection,
    item_menu: gtk::PopoverMenu,
    bg_menu: gtk::PopoverMenu,
    /// The colour dots inside item_menu, checked for the selection on every popup.
    tag_picker: gtk::Box,
    dir_col: gtk::ColumnViewColumn,
    git_col: gtk::ColumnViewColumn,
    /// Size/Modified/Type by settings id: order, width and visibility come from Preferences.
    opt_cols: Vec<(&'static str, gtk::ColumnViewColumn)>,
    /// set while applying settings, so the column-change handlers don't save them right back
    applying_cols: Cell<bool>,
    col_save_pending: Cell<bool>,
    git: RefCell<Option<Rc<crate::git::GitInfo>>>,
    git_pending: Cell<bool>,
    /// (position, view) -> the ListItem showing it, to anchor the rename popup.
    rows: RefCell<std::collections::HashMap<(u32, u8), glib::WeakRef<gtk::ListItem>>>,
    pending_select: RefCell<Option<PendingSelect>>,
    cols: Vec<(gtk::ColumnViewColumn, Sort)>,
    dir: RefCell<gio::File>,
    back: RefCell<Vec<gio::File>>,
    fwd: RefCell<Vec<gio::File>>,
    /// The user's search text while a search is showing.
    query: RefCell<Option<String>>,
    /// Words of the query to bold in result names.
    hl: Rc<RefCell<Vec<String>>>,
    scope: Cell<Scope>,
    search_state: RefCell<SearchState>,
    /// Folder view scroll positions (list, grid), restored when a search is cleared.
    saved_scroll: Cell<(f64, f64)>,
    show_hidden: Rc<Cell<bool>>,
    /// Select mode: a checkbox on every row/tile (see cells::with_check).
    select_mode: Cell<bool>,
    checks: RefCell<Vec<glib::WeakRef<gtk::CheckButton>>>,
    mode: Cell<Mode>,
    pub page: RefCell<Option<adw::TabPage>>,
    /// Window hooks: header refresh, selection changes (preview pane), daemon reachable or not.
    pub on_change: Hook<dyn Fn()>,
    pub on_select: Hook<dyn Fn()>,
    pub on_daemon: Hook<dyn Fn(bool)>,
    ops: Rc<Ops>,
    me: Weak<Browser>,
}

impl Browser {
    pub fn new(ops: Rc<Ops>, item_model: &gio::MenuModel, bg_model: &gio::MenuModel, loc: Loc) -> Rc<Browser> {
        let (dir, query) = match loc {
            Loc::Dir(f) => (f, None),
            Loc::Search(q) => (gio::File::for_path(glib::home_dir()), Some(q)),
        };
        let b = Rc::new_cyclic(|me: &Weak<Browser>| Browser::build(ops, item_model, bg_model, dir, me.clone()));
        b.apply_columns();
        b.wire();
        b.load_dir();
        if let Some(q) = query {
            b.set_search(&q);
        }
        b
    }

    fn build(ops: Rc<Ops>, item_model: &gio::MenuModel, bg_model: &gio::MenuModel, dir: gio::File, me: Weak<Browser>) -> Browser {
        let show_hidden = Rc::new(Cell::new(false));
        let dirlist = gtk::DirectoryList::new(Some(ATTRS), None::<&gio::File>);
        let sh = show_hidden.clone();
        // boolean(), not is_hidden(): archive mounts leave is-hidden unset and the getter logs a CRITICAL
        let filter = gtk::CustomFilter::new(move |o| sh.get() || !info_of(o).boolean("standard::is-hidden"));
        let filtered = gtk::FilterListModel::new(Some(dirlist.clone()), Some(filter.clone()));
        let sorted = gtk::SortListModel::new(Some(filtered), None::<gtk::Sorter>);
        let dir_sel = gtk::MultiSelection::new(Some(sorted.clone()));
        let search = SearchModel::default();
        let search_sel = gtk::MultiSelection::new(Some(search.clone()));

        let hl: Rc<RefCell<Vec<String>>> = Rc::default();
        let hl2 = hl.clone();
        // details view
        let list = gtk::ColumnView::builder().model(&dir_sel).enable_rubberband(true).reorderable(false).build();
        list.add_css_class("data-table");
        let name_f = factory(
            &me,
            Some(0),
            || {
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                row.append(&gtk::Image::builder().pixel_size(16).build());
                row.append(
                    &gtk::Label::builder()
                        .xalign(0.0)
                        .hexpand(true)
                        .ellipsize(gtk::pango::EllipsizeMode::Middle)
                        .build(),
                );
                // symlink badge; the status bar shows where it points
                row.append(&gtk::Image::builder().icon_name("emblem-symbolic-link").pixel_size(16).visible(false).build());
                row.append(&gtk::Box::builder().spacing(3).build());
                row.upcast()
            },
            move |w, info| {
                let img = w.first_child().and_downcast::<gtk::Image>().unwrap();
                // every view makes missing thumbnails, so a PDF looks the same wherever it's listed
                img.set_widget_name("");
                set_icon(&img, info);
                img.set_pixel_size(16);
                crate::thumbs::load_into(&img, info);
                let label = img.next_sibling().and_downcast::<gtk::Label>().unwrap();
                // hl is only non-empty for search results (folder views never set it)
                let terms = hl2.borrow();
                if terms.is_empty() || info.attribute_string("kestrel::dir").is_none() {
                    label.set_text(&info.display_name());
                } else {
                    label.set_markup(&crate::query_ui::highlight_markup(&info.display_name(), &terms));
                }
                if let Some(link) = label.next_sibling().and_downcast::<gtk::Image>() {
                    let t = link_target(info);
                    link.set_visible(t.is_some());
                    link.set_tooltip_text(t.map(|t| format!("Link to {}", t.display())).as_deref());
                }
                crate::tags::fill_dots(&w.last_child().and_downcast::<gtk::Box>().unwrap(), crate::tags::of_info(info));
            },
        );
        let col = |title: &str, f: gtk::SignalListItemFactory, sorter: gtk::CustomSorter, width: i32| {
            let c = gtk::ColumnViewColumn::builder()
                .title(title)
                .factory(&f)
                .sorter(&sorter)
                .resizable(true)
                .build();
            // Name/Folder: a floor width, then take the rest; narrow panes scroll sideways instead of squashing names
            c.set_fixed_width(if width > 0 { width } else { 220 });
            c.set_expand(width <= 0);
            c
        };
        let name_col = col(
            "Name",
            name_f,
            gtk::CustomSorter::new(|a, b| cmp(info_of(a).display_name().to_lowercase(), info_of(b).display_name().to_lowercase())),
            0,
        );
        let dir_col = col(
            "Folder",
            label_factory(&me, 0.0, |i| i.attribute_string("kestrel::dir").map(|s| tilde(&s)).unwrap_or_default()),
            gtk::CustomSorter::new(|a, b| cmp(info_of(a).attribute_string("kestrel::dir"), info_of(b).attribute_string("kestrel::dir"))),
            0,
        );
        dir_col.set_visible(false);
        let size_col = col(
            "Size",
            label_factory(&me, 1.0, |i| {
                if is_dirlike(i) {
                    String::new()
                } else {
                    glib::format_size(i.size() as u64).to_string()
                }
            }),
            gtk::CustomSorter::new(|a, b| cmp(info_of(a).size(), info_of(b).size())),
            90,
        );
        let date_col = col(
            "Modified",
            label_factory(&me, 0.0, fmt_date),
            gtk::CustomSorter::new(|a, b| cmp(mtime(info_of(a)), mtime(info_of(b)))),
            // fits the longest form, "12:30 PM 14 Mar 2025"
            170,
        );
        let type_col = col(
            "Type",
            label_factory(&me, 0.0, type_label),
            gtk::CustomSorter::new(|a, b| cmp(type_label(info_of(a)), type_label(info_of(b)))),
            // fits "Microsoft PowerPoint Presentation" (the Full style)
            220,
        );
        let gw = me.clone();
        let git_col = gtk::ColumnViewColumn::builder()
            .title("Git")
            .fixed_width(48)
            .visible(false)
            .factory(&factory(
                &me,
                None,
                || gtk::Label::builder().css_classes(["monospace", "heading"]).build().upcast(),
                move |w, info| {
                    let l = w.downcast_ref::<gtk::Label>().unwrap();
                    let st = gw.upgrade().and_then(|b| info_file(info).and_then(|f| b.git_status(&f)));
                    l.set_text(&st.map(String::from).unwrap_or_default());
                    l.set_css_classes(&[
                        "monospace",
                        "heading",
                        match st {
                            Some('M' | 'R') => "warning",
                            Some('A' | '?') => "success",
                            Some('D' | 'U') => "error",
                            _ => "dim-label",
                        },
                    ]);
                },
            ))
            .build();
        for c in [&name_col, &git_col, &dir_col, &size_col, &date_col, &type_col] {
            list.append_column(c);
        }
        let opt_cols = vec![("size", size_col.clone()), ("modified", date_col.clone()), ("type", type_col.clone())];
        list.sort_by_column(Some(&name_col), gtk::SortType::Ascending);
        let dirs_first = gtk::CustomSorter::new(|a, b| cmp(!is_dirlike(info_of(a)), !is_dirlike(info_of(b))));
        let multi = gtk::MultiSorter::new();
        multi.append(dirs_first);
        if let Some(s) = list.sorter() {
            multi.append(s);
        }
        sorted.set_sorter(Some(&multi));

        // grid view
        let tile_f = factory(
            &me,
            Some(1),
            || {
                let t = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(4).width_request(96).build();
                t.append(&gtk::Image::builder().pixel_size(64).build());
                t.append(
                    &gtk::Label::builder()
                        .wrap(true)
                        .wrap_mode(gtk::pango::WrapMode::WordChar)
                        .lines(2)
                        .ellipsize(gtk::pango::EllipsizeMode::End)
                        .justify(gtk::Justification::Center)
                        .max_width_chars(14)
                        .build(),
                );
                t.upcast()
            },
            |w, info| {
                let img = w.first_child().and_downcast::<gtk::Image>().unwrap();
                img.set_widget_name("");
                set_icon(&img, info);
                img.set_pixel_size(64);
                crate::thumbs::load_into(&img, info);
                w.last_child().and_downcast::<gtk::Label>().unwrap().set_text(&info.display_name());
            },
        );
        let grid = gtk::GridView::builder()
            .model(&dir_sel)
            .factory(&tile_f)
            .enable_rubberband(true)
            .max_columns(64)
            .build();

        let miller = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        for v in [list.upcast_ref::<gtk::Widget>(), grid.upcast_ref(), miller.upcast_ref()] {
            space_previews(v);
        }
        drag_beats_rubberband(list.upcast_ref(), |w, on| {
            w.downcast_ref::<gtk::ColumnView>().unwrap().set_enable_rubberband(on)
        });
        drag_beats_rubberband(grid.upcast_ref(), |w, on| w.downcast_ref::<gtk::GridView>().unwrap().set_enable_rubberband(on));
        let miller_scroll = gtk::ScrolledWindow::builder().child(&miller).vscrollbar_policy(gtk::PolicyType::Never).build();
        let status = adw::StatusPage::new();

        let list_scroll = gtk::ScrolledWindow::builder().child(&list).vexpand(true).build();
        let grid_scroll = gtk::ScrolledWindow::builder().child(&grid).vexpand(true).build();
        let stack = gtk::Stack::new();
        stack.add_named(&list_scroll, Some("list"));
        stack.add_named(&grid_scroll, Some("grid"));
        stack.add_named(&miller_scroll, Some("columns"));
        stack.add_named(&status, Some("status"));

        // location bar: breadcrumbs, or an editable path (Ctrl+L)
        let loc_label = gtk::Label::builder()
            .css_classes(["dim-label", "search-scope"])
            .visible(false)
            .margin_start(6)
            .build();
        let crumbs = gtk::Box::builder().spacing(0).build();
        let crumb_scroll = gtk::ScrolledWindow::builder()
            .child(&crumbs)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .hexpand(true)
            .propagate_natural_height(true)
            .build();
        let crumb_row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        // Select: checkboxes on every item; All / None / Invert while it's on
        let select = gtk::ToggleButton::builder()
            .icon_name("selection-mode-symbolic")
            .action_name("win.select-mode")
            .tooltip_text("Select (checkboxes)")
            .css_classes(["flat", "crumb"])
            .build();
        crumb_row.append(&select);
        for (icon, action, tip) in [
            ("edit-select-all-symbolic", "win.select-all", "Select all (Ctrl+A)"),
            ("edit-clear-all-symbolic", "win.select-none", "Select none (Ctrl+Shift+A)"),
            ("object-flip-horizontal-symbolic", "win.invert-selection", "Invert selection (Ctrl+Shift+I)"),
        ] {
            let b = gtk::Button::builder().icon_name(icon).action_name(action).tooltip_text(tip).css_classes(["flat", "crumb"]).build();
            select.bind_property("active", &b, "visible").sync_create().build();
            crumb_row.append(&b);
        }
        // Properties of the selection, or of this folder when nothing is selected
        crumb_row.append(
            &gtk::Button::builder()
                .icon_name("help-about-symbolic")
                .action_name("win.properties")
                .tooltip_text("Properties (Alt+Enter)")
                .css_classes(["flat", "crumb"])
                .build(),
        );
        crumb_row.append(&loc_label);
        crumb_row.append(&crumb_scroll);
        let loc_entry = gtk::Entry::builder()
            .placeholder_text("Type a path or address, e.g. ~/Projects or sftp://host/")
            .hexpand(true)
            .build();
        loc_entry.update_property(&[gtk::accessible::Property::Label("Location")]);
        let loc_stack = gtk::Stack::builder().css_classes(["location-bar"]).build();
        loc_stack.add_named(&crumb_row, Some("crumbs"));
        loc_stack.add_named(&loc_entry, Some("edit"));

        let statusbar = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["dim-label", "caption", "statusbar"])
            .margin_start(8)
            .margin_end(8)
            .margin_top(2)
            .margin_bottom(2)
            .build();
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(&loc_stack);
        root.append(&stack);
        root.append(&statusbar);

        let menu = |m: &gio::MenuModel| {
            let p = gtk::PopoverMenu::from_model(Some(m));
            p.set_parent(&root);
            p.set_has_arrow(false);
            p.set_halign(gtk::Align::Start);
            p
        };
        let (item_menu, bg_menu) = (menu(item_model), menu(bg_model));
        let im = item_menu.downgrade();
        let tag_picker = crate::tags::picker(move |t| {
            if let Some(m) = im.upgrade() {
                let _ = m.activate_action("win.tag", Some(&t.to_variant()));
                m.popdown();
            }
        });
        item_menu.add_child(&tag_picker, "tags");
        let cols = vec![
            (name_col, Sort::Name),
            (dir_col.clone(), Sort::Path),
            (size_col, Sort::Size),
            (date_col, Sort::Mtime),
            (type_col, Sort::Ext),
        ];

        Browser {
            root,
            stack,
            list,
            grid,
            list_scroll,
            grid_scroll,
            miller,
            miller_scroll,
            miller_sel: RefCell::default(),
            status,
            statusbar,
            loc_stack,
            crumbs,
            loc_label,
            loc_entry,
            dirlist,
            filter,
            search,
            dir_sel,
            search_sel,
            item_menu,
            bg_menu,
            tag_picker,
            dir_col,
            git_col,
            opt_cols,
            applying_cols: Cell::new(false),
            col_save_pending: Cell::new(false),
            git: RefCell::default(),
            git_pending: Cell::new(false),
            rows: RefCell::default(),
            pending_select: RefCell::default(),
            cols,
            dir: RefCell::new(dir),
            back: RefCell::default(),
            fwd: RefCell::default(),
            query: RefCell::default(),
            hl,
            scope: Cell::new(Scope::Everywhere),
            search_state: RefCell::new(SearchState::Running),
            saved_scroll: Cell::new((0.0, 0.0)),
            show_hidden,
            select_mode: Cell::new(false),
            checks: RefCell::default(),
            mode: Cell::new(Mode::List),
            page: RefCell::default(),
            on_change: RefCell::default(),
            on_select: RefCell::default(),
            on_daemon: RefCell::default(),
            ops,
            me,
        }
    }

    /// Signal handlers that need `self`.
    fn wire(self: &Rc<Self>) {
        // remember columns resized or moved by dragging in the list
        for (_, c) in &self.opt_cols {
            let w = self.me.clone();
            c.connect_fixed_width_notify(move |_| w.upgrade().into_iter().for_each(|b| b.save_columns_soon()));
        }
        let w = self.me.clone();
        self.list
            .columns()
            .connect_items_changed(move |_, _, _, _| w.upgrade().into_iter().for_each(|b| b.save_columns_soon()));
        self.column_drag();
        let me = self.me.clone();
        let (im, bm) = (self.item_menu.clone(), self.bg_menu.clone());
        self.root.connect_destroy(move |_| {
            im.unparent();
            bm.unparent();
        });

        let w = me.clone();
        self.list.connect_activate(move |_, pos| w.upgrade().into_iter().for_each(|b| b.activate(pos)));
        let w = me.clone();
        self.grid.connect_activate(move |_, pos| w.upgrade().into_iter().for_each(|b| b.activate(pos)));

        // search results are sorted by the daemon: re-query when a column header is clicked
        if let Some(s) = self.list.sorter() {
            let w = me.clone();
            s.connect_changed(move |_, _| {
                if let Some(b) = w.upgrade()
                    && b.is_searching()
                {
                    b.run_search();
                }
            });
        }

        for sel in [&self.dir_sel, &self.search_sel] {
            // A file whose attributes change (tagged, saved) is replaced in place in the model,
            // which drops it from the selection: remember what's selected and put it back.
            let selected = Rc::new(RefCell::new(std::collections::HashSet::<std::path::PathBuf>::new()));
            let w = me.clone();
            let sn = selected.clone();
            sel.connect_selection_changed(move |sel, _, _| {
                let bs = sel.selection();
                *sn.borrow_mut() = (0..bs.size() as u32)
                    .filter_map(|i| sel.item(bs.nth(i)).and_downcast::<gio::FileInfo>())
                    .filter_map(|i| info_file(&i).and_then(|f| f.path()))
                    .collect();
                if let Some(b) = w.upgrade() {
                    b.update_status();
                    b.notify_select();
                }
            });
            let w = me.clone();
            sel.connect_items_changed(move |sel, pos, removed, added| {
                if removed == added && added > 0 && !selected.borrow().is_empty() {
                    for i in pos..pos + added {
                        let was = sel
                            .item(i)
                            .and_downcast::<gio::FileInfo>()
                            .and_then(|i| info_file(&i).and_then(|f| f.path()))
                            .is_some_and(|p| selected.borrow().contains(&p));
                        if was && !sel.is_selected(i) {
                            sel.select_item(i, false);
                        }
                    }
                }
                w.upgrade().into_iter().for_each(|b| b.update_status());
            });
        }

        // files appearing/changing in a repo: re-run git status, at most twice a second
        let w = me.clone();
        self.dirlist.connect_items_changed(move |dl, _, _, _| {
            let Some(b) = w.upgrade() else { return };
            if dl.is_loading() || b.git.borrow().is_none() || b.git_pending.replace(true) {
                return;
            }
            let w = w.clone();
            glib::timeout_add_local_once(std::time::Duration::from_millis(500), move || {
                if let Some(b) = w.upgrade() {
                    b.git_pending.set(false);
                    let d = b.dir.borrow().clone();
                    b.refresh_git(&d);
                }
            });
        });
        let w = me.clone();
        self.dirlist.connect_loading_notify(move |dl| {
            let Some(b) = w.upgrade() else { return };
            if dl.is_loading() || b.is_searching() {
                return;
            }
            b.show_folder_state();
        });

        // empty-space right click
        let g = gtk::GestureClick::builder().button(3).build();
        let w = me.clone();
        g.connect_pressed(move |g, _, x, y| {
            let Some(b) = w.upgrade() else { return };
            b.sel().unselect_all();
            b.popup(&b.bg_menu, &g.widget().unwrap(), x, y);
        });
        self.stack.add_controller(g);

        // drop files in: same filesystem moves, otherwise copies (Shift forces move, Ctrl copy)
        let dt = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY | gdk::DragAction::MOVE);
        let w = me.clone();
        dt.connect_drop(move |t, v, _, _| {
            let (Some(b), Ok(list)) = (w.upgrade(), v.get::<gdk::FileList>()) else {
                return false;
            };
            let Some(dest) = b.dir() else { return false };
            let files = list.files();
            let actions = crate::fileops::drop_intent(t);
            b.ops.drop_files(actions, files, dest);
            true
        });
        self.stack.add_controller(dt);

        // location bar: click the breadcrumb strip's empty space (or Ctrl+L) to type a path
        let g = gtk::GestureClick::new();
        let w = me.clone();
        g.connect_released(move |_, _, _, _| w.upgrade().into_iter().for_each(|b| b.edit_location()));
        self.crumbs.parent().unwrap().parent().unwrap().add_controller(g);
        let w = me.clone();
        self.loc_entry.connect_activate(move |e| {
            let (w, text) = (w.clone(), e.text().to_string());
            // checking the path can stall on a dead network mount: do it off the GTK thread
            glib::spawn_future_local(async move {
                let f = gio::spawn_blocking(move || as_location(&text)).await.ok().flatten();
                let Some(b) = w.upgrade() else { return };
                match f {
                    Some(f) => {
                        b.go(Loc::Dir(f));
                        b.focus_view();
                    }
                    None => b.ops.toast("No such folder"),
                }
            });
        });
        let k = gtk::EventControllerKey::new();
        let w = me.clone();
        k.connect_key_pressed(move |_, key, _, _| {
            if key != gdk::Key::Escape {
                return glib::Propagation::Proceed;
            }
            if let Some(b) = w.upgrade() {
                b.loc_stack.set_visible_child_name("crumbs");
                b.focus_view();
            }
            glib::Propagation::Stop
        });
        self.loc_entry.add_controller(k);
        let fc = gtk::EventControllerFocus::new();
        let w = me.clone();
        fc.connect_leave(move |_| w.upgrade().into_iter().for_each(|b| b.loc_stack.set_visible_child_name("crumbs")));
        self.loc_entry.add_controller(fc);
    }

    /// Column order, widths and visibility from Preferences. Name, Git and Folder stay first.
    pub fn apply_columns(&self) {
        self.applying_cols.set(true);
        for (i, p) in crate::prefs::get().columns().iter().enumerate() {
            if let Some((_, c)) = self.opt_cols.iter().find(|(id, _)| *id == p.id) {
                c.set_visible(p.visible);
                c.set_fixed_width(p.width);
                self.list.insert_column(3 + i as u32, c); // moves it if already there
            }
        }
        self.applying_cols.set(false);
    }

    /// Drag a Size/Modified/Type header onto another to move it there. GTK 4.22's own reordering
    /// never starts (the title's click gesture claims the press and denies the view's drag gesture),
    /// so watch raw events instead; they reach capture-phase controllers whatever gestures claim.
    fn column_drag(self: &Rc<Self>) {
        let at = Rc::new(Cell::new((0.0, 0.0)));
        let m = gtk::EventControllerMotion::new();
        m.set_propagation_phase(gtk::PropagationPhase::Capture);
        let a = at.clone();
        m.connect_motion(move |_, x, y| a.set((x, y)));
        self.list.add_controller(m);

        // (column pressed, press x, its width then: a changed width means it was a resize)
        let drag: RefCell<Option<(gtk::ColumnViewColumn, f64, i32)>> = RefCell::default();
        let l = gtk::EventControllerLegacy::new();
        l.set_propagation_phase(gtk::PropagationPhase::Capture);
        let w = self.me.clone();
        l.connect_event(move |_, ev| {
            let Some(b) = w.upgrade() else { return glib::Propagation::Proceed };
            let (x, y) = at.get();
            match ev.event_type() {
                gdk::EventType::ButtonPress if ev.downcast_ref::<gdk::ButtonEvent>().is_some_and(|e| e.button() == 1) => {
                    *drag.borrow_mut() = b.opt_col_at(x, y).map(|c| {
                        let width = c.fixed_width();
                        (c, x, width)
                    });
                }
                gdk::EventType::ButtonRelease => {
                    if let Some((c, x0, width)) = drag.take()
                        && (x - x0).abs() > 16.0
                        && c.fixed_width() == width
                        && let Some(target) = b.opt_col_at(x, b.list.first_child().map_or(0, |h| h.height()) as f64 / 2.0)
                        && target != c
                    {
                        let cols = b.list.columns();
                        if let Some(i) = (0..cols.n_items()).find(|&i| cols.item(i).as_ref() == Some(target.upcast_ref())) {
                            b.list.insert_column(i, &c); // the column list change saves it
                        }
                    }
                }
                _ => {}
            }
            glib::Propagation::Proceed
        });
        self.list.add_controller(l);
    }

    /// The Size/Modified/Type column whose header is at (x, y) in the list's coordinates.
    fn opt_col_at(&self, x: f64, y: f64) -> Option<gtk::ColumnViewColumn> {
        // the header row is the view's first child, its titles in column order
        let header = self.list.first_child()?;
        let cols = self.list.columns();
        let mut title = header.first_child();
        let mut i = 0;
        while let Some(t) = title {
            if t.is_visible()
                && t.compute_bounds(&self.list)
                    .is_some_and(|r| r.contains_point(&graphene::Point::new(x as f32, y as f32)))
            {
                let c = cols.item(i).and_downcast::<gtk::ColumnViewColumn>()?;
                return self.opt_cols.iter().any(|(_, o)| *o == c).then_some(c);
            }
            title = t.next_sibling();
            i += 1;
        }
        None
    }

    /// A column was dragged wider/narrower or moved in the list: remember it (once things settle).
    fn save_columns_soon(&self) {
        if self.applying_cols.get() || self.col_save_pending.replace(true) {
            return;
        }
        let me = self.me.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(500), move || {
            let Some(b) = me.upgrade() else { return };
            b.col_save_pending.set(false);
            let cols = b.list.columns();
            let order: Vec<crate::prefs::ColumnPref> = (0..cols.n_items())
                .filter_map(|i| cols.item(i).and_downcast::<gtk::ColumnViewColumn>())
                .filter_map(|c| {
                    let (id, _) = b.opt_cols.iter().find(|(_, x)| *x == c)?;
                    Some(crate::prefs::ColumnPref {
                        id: id.to_string(),
                        visible: c.is_visible(),
                        width: c.fixed_width(),
                    })
                })
                .collect();
            crate::prefs::update(|s| s.columns = order);
        });
    }

    /// Tags every selected file has (shown checked in the menu's colour dots).
    fn sync_tags(&self) {
        let bits = self.selected_files().iter().fold(u8::MAX, |acc, f| acc & crate::tags::read(f));
        crate::tags::set_picker(&self.tag_picker, if bits == u8::MAX { 0 } else { bits });
    }

    fn popup(&self, menu: &gtk::PopoverMenu, w: &gtk::Widget, x: f64, y: f64) {
        self.sync_tags();
        let p = w
            .compute_point(&self.root, &graphene::Point::new(x as f32, y as f32))
            .unwrap_or(graphene::Point::new(x as f32, y as f32));
        menu.set_pointing_to(Some(&gdk::Rectangle::new(p.x() as i32, p.y() as i32, 1, 1)));
        menu.popup();
    }

    /// Keyboard context menu: at the view's top-left area.
    pub fn context_menu(&self) {
        let menu = if self.selected_files().is_empty() { &self.bg_menu } else { &self.item_menu };
        self.sync_tags();
        menu.set_pointing_to(Some(&gdk::Rectangle::new(48, 96, 1, 1)));
        menu.popup();
    }

    /// The selection model on screen: search results or the folder.
    pub fn sel(&self) -> &gtk::MultiSelection {
        if self.is_searching() { &self.search_sel } else { &self.dir_sel }
    }

    pub fn is_searching(&self) -> bool {
        self.query.borrow().is_some()
    }

    pub fn query(&self) -> Option<String> {
        self.query.borrow().clone()
    }

    pub fn scope(&self) -> Scope {
        self.scope.get()
    }

    /// What to restore this tab to (session).
    pub fn loc(&self) -> Loc {
        match self.query() {
            Some(q) => Loc::Search(q),
            None => Loc::Dir(self.dir.borrow().clone()),
        }
    }

    pub fn mode(&self) -> Mode {
        self.mode.get()
    }

    /// The folder being shown; None while search results are showing.
    pub fn dir(&self) -> Option<gio::File> {
        (!self.is_searching()).then(|| self.dir.borrow().clone())
    }

    /// The folder this tab is in, searching or not (search scope, location bar).
    pub fn base_dir(&self) -> gio::File {
        self.dir.borrow().clone()
    }

    pub fn title(&self) -> String {
        match self.query() {
            Some(q) => format!("\u{201c}{q}\u{201d}"),
            None => dir_title(&self.dir.borrow()),
        }
    }

    pub fn can_back(&self) -> bool {
        self.is_searching() || !self.back.borrow().is_empty()
    }

    pub fn can_forward(&self) -> bool {
        !self.is_searching() && !self.fwd.borrow().is_empty()
    }

    pub fn selected_infos(&self) -> Vec<gio::FileInfo> {
        if self.mode.get() == Mode::Columns && !self.is_searching() {
            return self.miller_sel.borrow().iter().cloned().collect();
        }
        let sel = self.sel();
        let bs = sel.selection();
        (0..bs.size() as u32)
            .filter_map(|i| sel.item(bs.nth(i)).and_downcast::<gio::FileInfo>())
            .collect()
    }

    /// Select entries by display name (headless testing, `--select`).
    pub fn select_names(&self, names: &[String]) {
        if self.mode.get() == Mode::Columns && !self.is_searching() {
            // the first column lists this folder; it selects one entry, which opens the next column.
            // ponytail: its DirectoryList loads separately, so a match not loaded yet is missed
            let first = self.miller.first_child().and_downcast::<gtk::ScrolledWindow>().and_then(|c| c.child());
            if let Some(sel) = first
                .and_downcast::<gtk::ListView>()
                .and_then(|lv| lv.model())
                .and_downcast::<gtk::SingleSelection>()
            {
                let hit = (0..sel.n_items()).find(|&i| {
                    sel.item(i)
                        .and_downcast::<gio::FileInfo>()
                        .is_some_and(|info| names.iter().any(|n| *n == info.display_name()))
                });
                sel.set_selected(hit.unwrap_or(gtk::INVALID_LIST_POSITION));
            }
            return;
        }
        let sel = self.sel();
        sel.unselect_all();
        for i in 0..sel.n_items() {
            if let Some(info) = sel.item(i).and_downcast::<gio::FileInfo>()
                && names.iter().any(|n| *n == info.display_name())
            {
                sel.select_item(i, false);
            }
        }
    }

    /// F2: rename in a popup on the file's own row/tile. False if that isn't on screen (columns view).
    pub fn rename_inline(&self) -> bool {
        let sel = self.sel();
        let bs = sel.selection();
        if bs.size() != 1 || (self.mode.get() == Mode::Columns && !self.is_searching()) {
            return false;
        }
        let pos = bs.nth(0);
        let view = if self.stack.visible_child_name().as_deref() == Some("grid") { 1 } else { 0 };
        if view == 0 {
            self.list.scroll_to(pos, None, gtk::ListScrollFlags::NONE, None);
        } else {
            self.grid.scroll_to(pos, gtk::ListScrollFlags::NONE, None);
        }
        let item = self.rows.borrow().get(&(pos, view)).and_then(|w| w.upgrade()).filter(|it| it.position() == pos);
        let (Some(anchor), Some(file)) = (
            item.and_then(|it| it.child()).filter(|c| c.is_mapped()),
            self.selected_files().into_iter().next(),
        ) else {
            return false;
        };
        let old = file.basename().unwrap_or_default().to_string_lossy().into_owned();
        let stem = old.rfind('.').filter(|&p| p > 0).unwrap_or(old.len());
        let n = old[..stem].chars().count() as i32;
        let entry = gtk::Entry::builder().text(&old).width_chars(28).build();
        let pop = gtk::Popover::builder().child(&entry).position(gtk::PositionType::Bottom).build();
        pop.set_parent(&anchor);
        pop.connect_closed(|p| {
            let p = p.clone();
            glib::idle_add_local_once(move || p.unparent());
        });
        let (ops, p) = (self.ops.clone(), pop.clone());
        entry.connect_activate(move |e| {
            let name = e.text().trim().to_string();
            p.popdown();
            if !name.is_empty() && name != old && !name.contains('/') {
                ops.rename_to(&file, &name);
            }
        });
        pop.popup();
        entry.grab_focus();
        glib::idle_add_local_once(move || entry.select_region(0, n));
        true
    }

    pub fn selected_files(&self) -> Vec<gio::File> {
        self.selected_infos().iter().filter_map(info_file).collect()
    }

    // ── navigation ──

    pub fn go(&self, loc: Loc) {
        match loc {
            Loc::Search(q) => self.set_search(&q),
            Loc::Dir(f) => {
                self.end_search(false);
                let prev = self.dir.replace(f);
                self.back.borrow_mut().push(prev);
                self.fwd.borrow_mut().clear();
                self.saved_scroll.set((0.0, 0.0));
                self.load_dir();
            }
        }
    }

    /// Back leaves a search first (like Esc), then walks folder history.
    pub fn back(&self) {
        if self.is_searching() {
            self.clear_search();
            return;
        }
        let Some(f) = self.back.borrow_mut().pop() else { return };
        let cur = self.dir.replace(f);
        self.fwd.borrow_mut().push(cur);
        self.load_dir();
    }

    pub fn forward(&self) {
        let Some(f) = self.fwd.borrow_mut().pop() else { return };
        let cur = self.dir.replace(f);
        self.back.borrow_mut().push(cur);
        self.load_dir();
    }

    pub fn up(&self) {
        if self.is_searching() {
            self.clear_search();
            return;
        }
        let parent = self.dir.borrow().parent(); // borrow must end before go() replaces dir
        if let Some(p) = parent {
            self.go(Loc::Dir(p));
        }
    }

    pub fn reload(&self) {
        if self.is_searching() {
            self.run_search();
            return;
        }
        let f = self.dir.borrow().clone();
        self.dirlist.set_file(None::<&gio::File>);
        self.dirlist.set_file(Some(&f));
        if self.mode.get() == Mode::Columns {
            self.miller_reset(&f);
        }
    }

    pub fn set_show_hidden(&self, on: bool) {
        self.show_hidden.set(on);
        self.filter.changed(gtk::FilterChange::Different);
    }

    pub fn set_select_mode(&self, on: bool) {
        self.select_mode.set(on);
        // rows are recycled, not freed, so this stays as long as the views' visible rows
        self.checks.borrow_mut().retain(|c| match c.upgrade() {
            Some(c) => {
                c.set_visible(on);
                true
            }
            None => false,
        });
    }

    pub fn set_mode(&self, m: Mode) {
        if self.mode.replace(m) == m {
            return;
        }
        if m == Mode::Columns && !self.is_searching() {
            let d = self.dir.borrow().clone();
            self.miller_reset(&d);
        }
        self.show_view();
        self.changed();
    }

    pub fn focus_view(&self) {
        let _ = match self.stack.visible_child_name().as_deref() {
            Some("grid") => self.grid.grab_focus(),
            Some("columns") => self.miller.first_child().is_some_and(|c| c.child_focus(gtk::DirectionType::TabForward)),
            _ => self.list.grab_focus(),
        };
    }

    /// Ctrl+L: the location bar becomes a path field.
    pub fn edit_location(&self) {
        let d = self.dir.borrow().clone();
        self.loc_entry
            .set_text(&d.path().map_or_else(|| d.uri().to_string(), |p| p.display().to_string()));
        self.loc_stack.set_visible_child_name("edit");
        self.loc_entry.grab_focus();
        self.loc_entry.select_region(0, -1);
    }

    /// Showing the trash? It's read straight from ~/.local/share/Trash/files: going through GVfs's
    /// trash:/// checks every mount for a .Trash folder, and one dead network mount made that take
    /// 25 s per request (the window froze, which looked like a crash).
    pub fn is_trash(&self) -> bool {
        self.dir.borrow().uri() == "trash:///"
    }

    fn load_dir(&self) {
        let f = self.dir.borrow().clone();
        self.set_crumbs();
        if self.is_network() {
            self.show_network();
            return;
        }
        if self.is_trash() {
            let files = crate::fileops::home_trash().join("files");
            let _ = std::fs::create_dir_all(&files);
            self.set_dir(&gio::File::for_path(files));
        } else if f.is_native() {
            self.set_dir(&f);
        } else {
            // GVfs locations (sftp://, smb://, archives) may need mounting first
            let me = self.me.clone();
            glib::spawn_future_local(async move {
                let Some(b) = me.upgrade() else { return };
                b.show_status("folder-remote-symbolic", "Connecting…", &f.uri());
                if let Err(e) = f
                    .query_info_future("standard::type", gio::FileQueryInfoFlags::NONE, glib::Priority::DEFAULT)
                    .await
                    && e.matches(gio::IOErrorEnum::NotMounted)
                {
                    let op = gtk::MountOperation::new(b.root.root().and_downcast::<gtk::Window>().as_ref());
                    if let Err(e) = f.mount_enclosing_volume_future(gio::MountMountFlags::NONE, Some(&op)).await {
                        b.show_status("dialog-error-symbolic", "Couldn't connect", &human_gio(&e));
                        return;
                    }
                }
                b.set_dir(&f);
            });
        }
        self.changed();
    }

    fn set_dir(&self, f: &gio::File) {
        self.refresh_git(f);
        self.dirlist.set_file(Some(f));
        if self.mode.get() == Mode::Columns {
            self.miller_reset(f);
        }
        if !self.dirlist.is_loading() {
            self.show_folder_state();
            return;
        }
        // slow folders (network mounts, the trash): say so after a moment instead of a blank view
        let (me, want) = (self.me.clone(), f.clone());
        glib::timeout_add_local_once(std::time::Duration::from_millis(400), move || {
            if let Some(b) = me.upgrade()
                && b.dirlist.is_loading()
                && b.dirlist.file().is_some_and(|d| d.equal(&want))
                && !b.is_searching()
                && b.dir_sel.n_items() == 0
            {
                b.show_status(
                    "content-loading-symbolic",
                    "Loading…",
                    "This location is slow to answer (a network drive, or the trash checking every drive).",
                );
            }
        });
    }

    /// Select these names once the folder has loaded (FileManager1.ShowItems), then call `then`.
    pub fn select_when_loaded(&self, names: Vec<String>, then: impl FnOnce() + 'static) {
        *self.pending_select.borrow_mut() = Some((names, Box::new(then)));
        // a search tab selects once its results arrive (run_search calls apply_pending_select), or
        // now if they already have: a fast search can answer before this is asked
        let ready = if self.is_searching() {
            matches!(*self.search_state.borrow(), SearchState::Done(_))
        } else {
            !self.dirlist.is_loading() && self.dirlist.file().is_some()
        };
        if ready {
            self.apply_pending_select();
        }
    }

    pub(super) fn apply_pending_select(&self) {
        let Some((names, then)) = self.pending_select.take() else { return };
        self.select_names(&names);
        let first = self.sel().selection().minimum();
        if first != u32::MAX {
            self.list.scroll_to(first, None, gtk::ListScrollFlags::FOCUS, None);
        }
        then();
    }

    /// Folder view, or an empty/error page once loading has finished.
    fn show_folder_state(&self) {
        if !self.dirlist.is_loading() && !self.is_searching() {
            self.apply_pending_select();
        }
        if let Some(e) = self.dirlist.error() {
            self.show_status("dialog-error-symbolic", "Can't open this folder", &human_gio(&e));
        } else if self.dir_sel.n_items() == 0 && !self.dirlist.is_loading() && self.mode.get() != Mode::Columns {
            self.show_status("folder-symbolic", "Folder is empty", "");
        } else {
            self.show_view();
        }
    }

    fn set_crumbs(&self) {
        let c = &self.crumbs;
        while let Some(w) = c.first_child() {
            c.remove(&w);
        }
        // inside home, start the trail at Home instead of / › home › alex
        let home = gio::File::for_path(glib::home_dir());
        let mut chain = vec![self.dir.borrow().clone()];
        while !chain.last().unwrap().equal(&home) {
            match chain.last().unwrap().parent() {
                Some(p) => chain.push(p),
                None => break,
            }
        }
        for (i, d) in chain.iter().rev().enumerate() {
            if i > 0 {
                c.append(&gtk::Label::builder().label("\u{203a}").css_classes(["dim-label"]).build());
            }
            let b = gtk::Button::builder().css_classes(["flat", "crumb"]).build();
            match d.path() {
                Some(p) if p.as_os_str() == "/" => b.set_icon_name("drive-harddisk-symbolic"),
                _ if d.equal(&home) => b.set_icon_name("user-home-symbolic"),
                _ => b.set_label(&dir_title(d)),
            }
            let path = d.path().map_or_else(|| d.uri().to_string(), |p| p.display().to_string());
            let (me, d) = (self.me.clone(), d.clone());
            if i + 1 == chain.len() {
                // the folder you're in: first click refreshes, the next one edits the path
                // (crumbs are rebuilt on navigation, which re-arms the refresh)
                b.set_tooltip_text(Some(&format!("{path}\nClick to refresh, click again to edit")));
                let armed = Cell::new(false);
                b.connect_clicked(move |_| {
                    if let Some(b) = me.upgrade() {
                        if armed.replace(true) {
                            b.edit_location();
                        } else {
                            b.reload();
                        }
                    }
                });
            } else {
                b.set_tooltip_text(Some(&path));
                b.connect_clicked(move |_| {
                    if let Some(b) = me.upgrade() {
                        b.go(Loc::Dir(d.clone()));
                    }
                });
            }
            c.append(&b);
        }
        crate::label_icon_buttons(c.upcast_ref());
        let sc = c.parent().and_then(|v| v.parent()).and_downcast::<gtk::ScrolledWindow>();
        glib::idle_add_local_once(move || {
            if let Some(sc) = sc {
                let a = sc.hadjustment();
                a.set_value(a.upper());
            }
        });
    }

    fn update_locbar(&self) {
        if !self.is_searching() {
            self.loc_label.set_visible(false);
            return;
        }
        self.loc_label.set_visible(true);
        let local = self.dir.borrow().path().is_some();
        self.loc_label.set_label(match self.scope.get() {
            Scope::Everywhere => "Searching everywhere — started from",
            _ if !local => "Searching everywhere (this location isn't indexed) — from",
            Scope::Folder => "Searching in",
            Scope::FolderOnly => "Searching only in",
        });
    }

    pub fn is_network(&self) -> bool {
        self.dir.borrow().uri() == "network:///"
    }

    /// network:/// gets its own page (connected, previous, found, connect-to-server), built afresh
    /// each visit so it reflects what's mounted now.
    fn show_network(&self) {
        if let Some(old) = self.stack.child_by_name("network") {
            self.stack.remove(&old);
        }
        let me = self.me.clone();
        let open: std::rc::Rc<dyn Fn(gio::File)> = std::rc::Rc::new(move |f| {
            if let Some(b) = me.upgrade() {
                b.go(Loc::Dir(f));
            }
        });
        self.stack.add_named(&crate::network::page(open), Some("network"));
        self.stack.set_visible_child_name("network");
    }

    fn show_view(&self) {
        if self.is_network() && !self.is_searching() {
            return self.stack.set_visible_child_name("network");
        }
        let m = if self.is_searching() && self.mode.get() == Mode::Columns {
            Mode::List
        } else {
            self.mode.get()
        };
        self.stack.set_visible_child_name(m.name());
    }

    fn show_status(&self, icon: &str, title: &str, desc: &str) {
        if icon == "dialog-error-symbolic" {
            eprintln!("folder error: {title}: {desc}"); // scripts/ui-check.sh fails on these
        }
        self.status.set_icon_name(Some(icon));
        self.status.set_title(title);
        self.status.set_description(Some(desc).filter(|d| !d.is_empty()));
        self.stack.set_visible_child_name("status");
    }

    fn changed(&self) {
        if let Some(p) = self.page.borrow().as_ref() {
            p.set_title(&self.title());
            let d = self.dir.borrow();
            p.set_tooltip(&match self.query() {
                Some(q) => q,
                None => d.path().map_or_else(|| d.uri().to_string(), |p| p.display().to_string()),
            });
            p.set_icon(Some(&gio::ThemedIcon::new(if self.is_searching() {
                "system-search-symbolic"
            } else {
                "folder-symbolic"
            })));
        }
        let cb = self.on_change.borrow().clone();
        if let Some(cb) = cb {
            cb();
        }
    }

    fn notify_select(&self) {
        let cb = self.on_select.borrow().clone();
        if let Some(cb) = cb {
            cb();
        }
    }

    pub fn git_status(&self, f: &gio::File) -> Option<char> {
        self.git.borrow().as_ref().and_then(|g| g.get(&f.path()?))
    }

    /// Git status runs on a worker; the column appears once it's known this is a repo.
    fn refresh_git(&self, f: &gio::File) {
        let Some(p) = f.path() else {
            *self.git.borrow_mut() = None;
            self.git_col.set_visible(false);
            return;
        };
        let me = self.me.clone();
        let want = f.clone();
        glib::spawn_future_local(async move {
            let g = gio::spawn_blocking(move || crate::git::status(&p)).await.ok().flatten();
            let Some(b) = me.upgrade() else { return };
            if !b.dir.borrow().equal(&want) {
                return;
            }
            *b.git.borrow_mut() = g.map(Rc::new);
            b.git_col.set_visible(b.git.borrow().is_some() && !b.is_searching());
            // rebind the column's cells (GTK keeps rows whose items didn't change)
            let f = b.git_col.factory();
            b.git_col.set_factory(None::<&gtk::ListItemFactory>);
            b.git_col.set_factory(f.as_ref());
            b.update_status();
        });
    }

    fn update_status(&self) {
        self.statusbar.remove_css_class("error");
        let mut s = if self.is_searching() {
            match self.search_state.borrow().clone() {
                SearchState::Running => "Searching…".to_string(),
                SearchState::Indexing => "Indexing…".to_string(),
                SearchState::Done(o) => format!(
                    "{} result{} · {} · {} ms",
                    thousands(o.total as u64),
                    if o.total == 1 { "" } else { "s" },
                    glib::format_size(o.bytes),
                    o.ms
                ),
                SearchState::BadQuery(e) => {
                    self.statusbar.add_css_class("error");
                    format!("Query error: {e}")
                }
                SearchState::Down(e) => {
                    self.statusbar.add_css_class("error");
                    e
                }
            }
        } else {
            let n = self.dir_sel.n_items();
            let mut s = format!("{} item{}", thousands(n as u64), if n == 1 { "" } else { "s" });
            if let Some(g) = self.git.borrow().as_ref() {
                s = format!("\u{2387} {} · {s}", g.branch);
            }
            s
        };
        let sel = self.sel().selection().size();
        if sel > 0 {
            // ponytail: sums only the first 10k selected; Ctrl+A on a million search hits stays instant
            let infos = self.selected_infos();
            let bytes: u64 = infos.iter().take(10_000).filter(|i| !is_dirlike(i)).map(|i| i.size() as u64).sum();
            s += &format!(" · {} selected ({})", thousands(sel), glib::format_size(bytes));
            if let [one] = infos.as_slice() {
                s += &link_note(one);
            }
        }
        self.statusbar.set_text(&s);
    }

    fn activate(&self, pos: u32) {
        let Some(info) = self.sel().item(pos).and_downcast::<gio::FileInfo>() else {
            return;
        };
        self.open_info(&info);
    }

    /// Folders open in this tab (leaving any search); files open in their app.
    pub fn open_info(&self, info: &gio::FileInfo) {
        if let Some(q) = self.query() {
            crate::sidebar::remember_search(&q);
        }
        if is_dirlike(info) {
            if let Some(f) = nav_target(info) {
                self.go(Loc::Dir(f));
                // the row that had focus is gone; keep the keyboard in the file list
                self.focus_view();
            }
        } else if let Some(f) = info_file(info) {
            // GVfs' archive backend is in gvfs-backends (Recommends); without it, open in an app
            if f.is_native() && crate::archive::browse_on_open(&info.name().to_string_lossy()) && std::path::Path::new("/usr/libexec/gvfsd-archive").exists() {
                self.go(Loc::Dir(crate::archive::archive_root(&f)));
                self.focus_view();
            } else {
                self.launch(&f, false);
            }
        }
    }

    pub fn launch(&self, f: &gio::File, ask: bool) {
        let l = gtk::FileLauncher::new(Some(f));
        l.set_always_ask(ask);
        let ops = self.ops.clone();
        l.launch(self.root.root().and_downcast::<gtk::Window>().as_ref(), gio::Cancellable::NONE, move |r| {
            if let Err(e) = r
                && !e.matches(gtk::DialogError::Dismissed)
            {
                ops.toast(&human_gio(&e));
            }
        });
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn type_names() {
        use crate::prefs::TypeStyle::*;
        let t = |d, e: Option<&str>, s| super::type_text(d, e, s);
        assert_eq!(t("Microsoft Excel Worksheet", Some("XLSX"), Full), "Microsoft Excel Worksheet");
        assert_eq!(t("Microsoft Excel Worksheet", Some("XLSX"), Short), "Excel");
        assert_eq!(t("Microsoft Excel Worksheet", Some("XLSX"), Extension), "XLSX");
        assert_eq!(t("Microsoft PowerPoint Presentation", None, Short), "PowerPoint");
        assert_eq!(t("Plain text document", None, Short), "Plain text");
        assert_eq!(t("Tar archive (gzip-compressed)", None, Short), "Tar");
        assert_eq!(t("Folder", None, Extension), "Folder");
        assert_eq!(t("Shell script", None, Short), "Shell script");
        assert_eq!(t("text/x-rust type", Some("RS"), Full), "RS file");
        assert_eq!(super::extension("Q3 report.xlsx").as_deref(), Some("XLSX"));
        assert_eq!(super::extension("backup.tar.gz").as_deref(), Some("TAR.GZ"));
        assert_eq!(super::extension(".bashrc"), None);
        assert_eq!(super::extension("Makefile"), None);
    }

    #[test]
    fn modified_column_dates() {
        use gtk::glib;
        let tz = glib::TimeZone::utc();
        let at = |y, mo, d, h, mi| glib::DateTime::new(&tz, y, mo, d, h, mi, 0.0).unwrap();
        let now = at(2026, 10, 26, 15, 0);
        let w = |d: glib::DateTime, now: &glib::DateTime, h12| super::fmt_when(&d, now, h12);
        assert_eq!(w(at(2026, 10, 26, 9, 5), &now, false), "09:05 Today");
        assert_eq!(w(at(2026, 10, 25, 23, 20), &now, false), "23:20 Yesterday");
        assert_eq!(w(at(2026, 10, 2, 18, 40), &now, false), "18:40 2 Oct");
        assert_eq!(w(at(2024, 12, 31, 18, 40), &now, false), "18:40 31 Dec 2024");
        assert_eq!(w(at(2026, 10, 26, 14, 32), &now, true), "2:32 PM Today");
        // yesterday across a year boundary
        assert_eq!(w(at(2025, 12, 31, 8, 0), &at(2026, 1, 1, 10, 0), false), "08:00 Yesterday");
    }

    #[test]
    fn helpers() {
        assert_eq!(super::thousands(0), "0");
        assert_eq!(super::thousands(1234), "1,234");
        assert_eq!(super::thousands(1234567), "1,234,567");
        assert!(super::as_location("no-such-thing").is_none());
        assert!(super::as_location("/").is_some());
        assert!(super::as_location("/definitely/not/here/x").is_none());
        assert!(super::as_location("sftp://host/").is_some());
    }
}
