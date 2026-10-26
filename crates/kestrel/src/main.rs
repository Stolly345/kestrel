//! Kestrel: Everything-style instant search inside a Files-style file manager.

mod archive;
mod browser;
mod filemanager1;
mod fileops;
mod git;
mod icon;
mod network;
mod palette;
mod prefs;
mod preview;
mod props;
mod query_ui;
mod rename;
mod scripts;
mod search;
mod sidebar;
mod tags;
mod term;
mod thumbs;
mod whiteboard;
mod window;
mod zim;

use adw::prelude::*;
use browser::{Loc, Mode};
use gtk::{gdk, gio, glib};
use window::Win;

const CSS: &str = "
image.thumb { outline: 1px solid alpha(currentColor, 0.25); outline-offset: -1px; border-radius: 2px; }
.md-page, .md-page > viewport { background: #ffffff; color: #1e1e1e; }
levelbar.usage block.used { background: @accent_bg_color; }
levelbar.usage block.nearly-full { background: @error_bg_color; }
levelbar.usage trough { min-height: 4px; }
levelbar.usage block { min-height: 4px; }
.inactive-pane tabbar, .inactive-pane .statusbar { opacity: 0.55; }
.miller-col { border-right: 1px solid alpha(currentColor, 0.12); }
.statusbar { min-height: 22px; }
.statusbar.error { color: @error_color; opacity: 1; }
.location-bar { padding: 2px 6px; border-bottom: 1px solid alpha(currentColor, 0.08); min-height: 34px; }
.location-bar .crumb { padding: 2px 6px; min-height: 26px; }
.main-search { min-height: 34px; font-size: 1.05em; }
.chip-bar { padding: 4px 10px; border-bottom: 1px solid alpha(currentColor, 0.08); }
menubutton.chip > button { padding: 2px 10px; min-height: 26px; border-radius: 99px; }
menubutton.chip.chip-active > button { background: alpha(@accent_bg_color, 0.22); color: @accent_color; }
.preview-pane { background: var(--sidebar-bg-color); border-left: 1px solid var(--border-color); }
.preview-header { padding: 0 6px 0 14px; min-height: 46px; border-bottom: 1px solid var(--border-color); }
.tag-dot.tag-big { min-width: 16px; min-height: 16px; color: white; box-shadow: inset 0 0 0 1px alpha(black, 0.15); }
button.tag-pick { min-width: 22px; min-height: 22px; padding: 1px; }
.tag-dot.tag-big.tag-small { min-width: 14px; min-height: 14px; }
.tag-dot { min-width: 10px; min-height: 10px; border-radius: 99px; }
.tag-red { background: @red_3; }
.tag-orange { background: @orange_3; }
.tag-yellow { background: @yellow_3; }
.tag-green { background: @green_3; }
.tag-blue { background: @blue_3; }
.tag-purple { background: @purple_3; }
.tag-gray { background: @light_5; }
";

const ACCELS: &[(&str, &[&str])] = &[
    ("win.new-tab", &["<Control>t"]),
    ("win.close-tab", &["<Control>w"]),
    ("win.new-window", &["<Control>n"]),
    ("win.back", &["<Alt>Left", "<Alt>KP_Left", "Back"]),
    ("win.forward", &["<Alt>Right", "<Alt>KP_Right", "Forward"]),
    ("win.up", &["<Alt>Up"]),
    ("win.reload", &["<Control>r", "F5"]),
    ("win.edit-location", &["<Control>l"]),
    ("win.search-here", &["<Control>f"]),
    ("win.search-everywhere", &["<Control><Shift>f"]),
    ("win.show-hidden", &["<Control>h"]),
    ("win.dual", &["F3"]),
    ("win.other-pane", &["F4"]),
    ("win.copy-other", &["F7"]),
    ("win.move-other", &["F8"]),
    ("win.new-folder", &["<Control><Shift>n"]),
    ("win.add-bookmark", &["<Control>d"]),
    ("win.palette", &["<Control><Shift>p"]),
    ("win.preferences", &["<Control>comma"]),
    ("win.preview", &["<Alt>p"]),
    ("win.terminal-panel", &["<Control>grave"]),
    ("win.sidebar", &["F9"]),
    ("win.focus-next-region", &["F6"]),
    ("win.focus-prev-region", &["<Shift>F6"]),
    ("win.view::list", &["<Control>1"]),
    ("win.view::grid", &["<Control>2"]),
    ("win.view::columns", &["<Control>3"]),
];

#[derive(Default)]
struct Args {
    locs: Vec<Loc>,
    screenshot: Option<String>,
    delay: u64,
    dual: bool,
    mode: Option<Mode>,
    hidden: bool,
    actions: Vec<String>,
    select: Vec<String>,
    /// D-Bus activated as org.freedesktop.FileManager1: no window of our own, windows on request.
    filemanager1: bool,
}

fn parse_args() -> Args {
    let mut a = Args {
        delay: 1500,
        ..Default::default()
    };
    let mut it = std::env::args().skip(1);
    while let Some(x) = it.next() {
        match x.as_str() {
            "--screenshot" => a.screenshot = it.next(),
            "--delay" => a.delay = it.next().and_then(|d| d.parse().ok()).unwrap_or(1500),
            "--search" => a.locs.push(Loc::Search(it.next().unwrap_or_default())),
            "--dual" => a.dual = true,
            "--hidden" => a.hidden = true,
            "--mode" => a.mode = it.next().map(|m| Mode::parse(&m)),
            "--action" => a.actions.extend(it.next()),
            "--select" => a.select.extend(it.next()),
            "--filemanager1" => a.filemanager1 = true,
            "-h" | "--help" => {
                println!("usage: kestrel [PATH|URI]...");
                std::process::exit(0);
            }
            _ => a.locs.push(Loc::Dir(gio::File::for_commandline_arg(&x))),
        }
    }
    a
}

/// Screen readers: an icon-only button has no name of its own, so give it its tooltip.
/// Run on any subtree after building buttons (window, sidebar refresh, breadcrumbs).
pub fn label_icon_buttons(root: &gtk::Widget) {
    let mut stack = vec![root.clone()];
    while let Some(w) = stack.pop() {
        let is_button = w.is::<gtk::Button>() || w.is::<gtk::MenuButton>();
        let has_text = w.downcast_ref::<gtk::Button>().is_some_and(|b| b.label().is_some_and(|l| !l.is_empty()))
            || w.downcast_ref::<gtk::MenuButton>().is_some_and(|b| b.label().is_some_and(|l| !l.is_empty()));
        if is_button
            && !has_text
            && let Some(t) = w.tooltip_text()
        {
            w.update_property(&[gtk::accessible::Property::Label(&t)]);
        }
        let mut c = w.first_child();
        while let Some(ch) = c {
            c = ch.next_sibling();
            stack.push(ch);
        }
    }
}

/// Render a widget to a PNG (used for headless checks under GDK_BACKEND=broadway).
fn screenshot(w: &impl IsA<gtk::Widget>, path: &str) {
    let p = gtk::WidgetPaintable::new(Some(w));
    let snap = gtk::Snapshot::new();
    p.snapshot(&snap, w.width() as f64, w.height() as f64);
    let (Some(node), Some(r)) = (snap.to_node(), w.native().and_then(|n| n.renderer())) else {
        eprintln!("screenshot: nothing rendered");
        return;
    };
    if let Err(e) = r.render_texture(&node, None).save_to_png(path) {
        eprintln!("screenshot: {e}");
    }
}

fn main() -> glib::ExitCode {
    let args = std::rc::Rc::new(parse_args());
    filemanager1::migrate_from_chimera();
    let fm1 = args.filemanager1;
    let app = if fm1 {
        // a service: stays up without windows, exits a minute after the last one closes
        let app = adw::Application::builder()
            .application_id("dev.kestrel.Kestrel.FileManager1")
            .flags(gio::ApplicationFlags::IS_SERVICE)
            .build();
        app.set_inactivity_timeout(60_000);
        app
    } else {
        adw::Application::builder()
            .application_id("dev.kestrel.Kestrel")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build()
    };
    app.connect_startup(move |app| {
        gtk::Window::set_default_icon_name("dev.kestrel.Kestrel");
        icon::follow_theme();
        prefs::apply_color_scheme(&prefs::get());
        let css = gtk::CssProvider::new();
        css.load_from_string(CSS);
        gtk::style_context_add_provider_for_display(&gdk::Display::default().unwrap(), &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        for (action, keys) in ACCELS {
            app.set_accels_for_action(action, keys);
        }
        if fm1 {
            filemanager1::own(app);
        }
    });
    app.connect_activate(move |app| {
        if fm1 {
            return;
        }
        let session = window::load_session().filter(|_| args.locs.is_empty() && prefs::get().restore_session);
        let home = || Loc::Dir(gio::File::for_path(glib::home_dir()));
        let w = match &session {
            Some(s) if !s.left.is_empty() => {
                let toks = |v: &Vec<String>| v.iter().map(|t| Loc::from_token(t)).collect::<Vec<_>>();
                let w = Win::new(app, toks(&s.left), toks(&s.right), s.dual);
                if s.size.0 > 200 && s.size.1 > 200 {
                    w.window.set_default_size(s.size.0, s.size.1);
                }
                w
            }
            _ => Win::new(app, if args.locs.is_empty() { vec![home()] } else { args.locs.clone() }, vec![], args.dual),
        };
        if args.dual {
            w.set_dual(true);
        }
        let preview = match prefs::get().preview_at_start.as_str() {
            "show" => true,
            "hide" => false,
            _ => window::load_session().is_some_and(|s| s.preview),
        };
        if preview {
            gio::prelude::ActionGroupExt::activate_action(&w.window, "preview", None);
        }
        if args.hidden || prefs::get().show_hidden {
            w.set_hidden(true);
        }
        let mode = args.mode.or_else(|| session.as_ref().map(|s| Mode::parse(&s.mode)));
        if let (Some(m), Some(b)) = (mode, w.current()) {
            b.set_mode(m);
            w.update();
        }
        w.window.present();
        w.focus_search(None);

        // headless testing: select, then run actions, once the folder has loaded
        let (wc, args2) = (w.clone(), args.clone());
        glib::timeout_add_local_once(std::time::Duration::from_millis(600), move || {
            let run = {
                let wc = wc.clone();
                let actions = args2.actions.clone();
                move || {
                    for a in &actions {
                        let (name, target) = a.split_once("::").map_or((a.as_str(), None), |(n, t)| (n, Some(t.to_variant())));
                        if let Err(e) = WidgetExt::activate_action(&wc.window, &format!("win.{name}"), target.as_ref()) {
                            eprintln!("action {name}: {e}");
                        }
                    }
                }
            };
            match wc.current().filter(|_| !args2.select.is_empty()) {
                // a slow folder (USB disk, network) may still be listing: select once it's loaded
                Some(b) => b.select_when_loaded(args2.select.clone(), run),
                None => run(),
            }
        });
        // debug: KESTREL_WIDTHS=1 prints each widget's minimum width (find what makes the window too wide)
        if std::env::var_os("KESTREL_WIDTHS").is_some() {
            let win = w.window.clone();
            glib::timeout_add_local_once(std::time::Duration::from_millis(1500), move || {
                fn walk(wd: &gtk::Widget, depth: usize) {
                    let (min, _, _, _) = wd.measure(gtk::Orientation::Horizontal, -1);
                    if min >= 150 && depth < 14 {
                        eprintln!("{}{} {}", "  ".repeat(depth), wd.type_().name(), min);
                    }
                    let mut c = wd.first_child();
                    while let Some(ch) = c {
                        walk(&ch, depth + 1);
                        c = ch.next_sibling();
                    }
                }
                walk(win.upcast_ref(), 0);
            });
        }
        if let Some(path) = args.screenshot.clone() {
            // headless Broadway has no frame clock driving animations; dialogs would never appear
            if let Some(s) = gtk::Settings::default() {
                s.set_gtk_enable_animations(false);
            }
            let (win, app) = (w.window.clone(), app.clone());
            glib::timeout_add_local_once(std::time::Duration::from_millis(args.delay), move || {
                screenshot(&win, &path);
                if let Some(d) = win.visible_dialog() {
                    screenshot(&d, &format!("{path}.dialog.png"));
                }
                // dialogs may be separate toplevels: save those too, as PATH.1.png, PATH.2.png…
                let others: Vec<gtk::Window> = gtk::Window::list_toplevels()
                    .into_iter()
                    .filter_map(|w| w.downcast().ok())
                    .filter(|w: &gtk::Window| *w != win.clone().upcast::<gtk::Window>() && w.is_visible())
                    .collect();
                for (i, w) in others.iter().enumerate() {
                    screenshot(w, &format!("{path}.{}.png", i + 1));
                }
                app.quit();
            });
        }
    });
    app.run_with_args(&["kestrel"])
}
