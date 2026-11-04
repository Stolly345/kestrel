//! The click-through: what it is → where it goes → installing → done. One dialog, its pages in
//! a stack; Back in the header, three dots for where you are, the app's own icon throughout.

use super::{Fit, Kind, Pkg, Places, Pm, appimage, pkg};
use crate::fileops::Ops;
use adw::prelude::*;
use gtk::{gio, glib};
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

struct Wizard {
    dialog: adw::Dialog,
    stack: gtk::Stack,
    back: gtk::Button,
    title: gtk::Label,
    dots: gtk::Box,
    ops: Rc<Ops>,
    pkg: RefCell<Option<Arc<Pkg>>>,
    desktop_icon: Cell<bool>,
    keep_download: Cell<bool>,
    pm: Pm,
}

/// Open the installer for a .deb, .rpm or AppImage. The dialog appears at once and reads the
/// file in the background (a big package takes a moment to unpack).
pub fn show(ops: &Rc<Ops>, f: &gio::File) {
    let Some(path) = f.path() else { return };
    let Some(kind) = super::kind_of(&path.file_name().unwrap_or_default().to_string_lossy()) else { return };
    let w = Wizard::new(ops);
    let file = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
    w.page("loading", &loading(&format!("Reading {file}…")), None);
    w.dialog.present(ops.window().as_ref());
    let pm = w.pm;
    glib::spawn_future_local(async move {
        let p = path.clone();
        let read = gio::spawn_blocking(move || match kind {
            Kind::AppImage => appimage::read(&p, &Places::user()),
            k => pkg::read(k, &p, pm),
        })
        .await
        .ok()
        .flatten();
        match read {
            Some(p) => {
                *w.pkg.borrow_mut() = Some(Arc::new(p));
                w.intro();
            }
            None => {
                let what = match kind {
                    Kind::AppImage => "an AppImage this computer can read",
                    Kind::Deb => "a .deb package",
                    Kind::Rpm => "an .rpm package",
                };
                w.page(
                    "error",
                    &status_page("dialog-warning-symbolic", "Can't read this file", &format!("{file} doesn't look like {what}. It may be damaged or only partly downloaded.")),
                    Some(close_button(&w.dialog).upcast_ref()),
                );
            }
        }
    });
}

impl Wizard {
    fn new(ops: &Rc<Ops>) -> Rc<Wizard> {
        let back = gtk::Button::builder().icon_name("go-previous-symbolic").tooltip_text("Back").visible(false).build();
        let title = gtk::Label::builder().label("Install App").css_classes(["heading"]).build();
        let header = adw::HeaderBar::builder().title_widget(&title).build();
        header.pack_start(&back);
        let dots = gtk::Box::builder().spacing(6).halign(gtk::Align::Center).css_classes(["installer-dots"]).visible(false).build();
        for _ in 0..3 {
            dots.append(&gtk::Box::builder().css_classes(["installer-dot"]).valign(gtk::Align::Center).build());
        }
        let stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::SlideLeftRight)
            .vexpand(true)
            .vhomogeneous(false)
            .build();
        let body = gtk::Box::builder().orientation(gtk::Orientation::Vertical).build();
        body.append(&dots);
        body.append(&stack);
        let tv = adw::ToolbarView::builder().content(&body).build();
        tv.add_top_bar(&header);
        let dialog = adw::Dialog::builder()
            .title("Install App")
            .content_width(400)
            .content_height(620)
            .child(&tv)
            .css_classes(["installer"])
            .build();
        let w = Rc::new(Wizard {
            dialog,
            stack,
            back,
            title,
            dots,
            ops: ops.clone(),
            pkg: RefCell::default(),
            desktop_icon: Cell::new(true),
            keep_download: Cell::new(false),
            pm: super::pm(),
        });
        // the dialog keeps the wizard alive until it closes (its buttons hold only weak refs)
        let keep = RefCell::new(Some(w.clone()));
        w.dialog.connect_closed(move |_| {
            keep.take();
        });
        let me = Rc::downgrade(&w);
        w.back.connect_clicked(move |_| {
            if let Some(w) = me.upgrade() {
                w.intro();
            }
        });
        crate::label_icon_buttons(w.back.upcast_ref());
        w
    }

    fn pkg(&self) -> Arc<Pkg> {
        self.pkg.borrow().clone().expect("read before the pages")
    }

    /// Show a page: its body over its footer.
    fn page(&self, name: &str, body: &gtk::Widget, footer: Option<&gtk::Widget>) {
        if let Some(old) = self.stack.child_by_name(name) {
            self.stack.remove(&old);
        }
        let b = gtk::Box::builder().orientation(gtk::Orientation::Vertical).build();
        body.set_vexpand(true);
        b.append(body);
        if let Some(f) = footer {
            b.append(f);
        }
        self.stack.add_named(&b, Some(name));
        self.stack.set_visible_child_name(name);
    }

    /// Which of the three steps is current (None hides the dots).
    fn step(&self, n: Option<usize>) {
        self.dots.set_visible(n.is_some());
        let mut c = self.dots.first_child();
        let mut i = 0;
        while let Some(d) = c {
            if Some(i) == n {
                d.add_css_class("on");
            } else {
                d.remove_css_class("on");
            }
            c = d.next_sibling();
            i += 1;
        }
    }

    // ── 1: what it is ──

    fn intro(self: &Rc<Self>) {
        let p = self.pkg();
        self.back.set_visible(false);
        self.title.set_label("Install App");
        let body = page_body();
        if let Fit::No { made_for, why } = &p.fit {
            self.step(None);
            body.set_valign(gtk::Align::Center);
            let h = hero(&p, 96, &[p.version_line().as_str()]);
            h.first_child().inspect(|i| i.set_opacity(0.55));
            body.append(&h);
            let pills = gtk::Box::builder().spacing(8).halign(gtk::Align::Center).build();
            pills.append(&pill(made_for, ""));
            pills.append(&pill(&format!("not {}", super::os_name()), "no"));
            body.append(&pills);
            body.append(&note("dialog-warning-symbolic", why, true));
            let foot = footer();
            if p.kind != Kind::AppImage {
                foot.append(&self.browse_button(false));
            }
            foot.append(&button("Close", "flat", {
                let d = self.dialog.clone();
                move || {
                    d.close();
                }
            }));
            self.page("intro", body.upcast_ref(), Some(foot.upcast_ref()));
            return;
        }
        self.step(Some(0));
        let size = if p.size > 0 { glib::format_size(p.size).to_string() } else { String::new() };
        let sub = [p.version_line(), size].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ");
        body.append(&hero(&p, 96, &[sub.as_str(), p.summary.as_str()]));

        let rows = props();
        let kind = match p.kind {
            Kind::AppImage => ("AppImage", Some(("runs on its own", ""))),
            Kind::Deb => ("Debian / Ubuntu package", None),
            Kind::Rpm => ("Fedora / openSUSE package", None),
        };
        rows.append(&prop("Kind", kind.0, kind.1));
        if let Some(o) = &p.origin {
            rows.append(&prop("Downloaded from", o, None));
        }
        let status = match &p.installed {
            None => ("Not installed".to_string(), None),
            Some(v) if *v == p.version => ("This version is installed".to_string(), None),
            Some(v) => (format!("{v} is installed"), Some(("update", "go"))),
        };
        rows.append(&prop("Status", &status.0, status.1));
        match p.needs {
            Some(0) => rows.append(&prop("Also needs", "nothing else", None)),
            Some(n) => rows.append(&prop("Also needs", &format!("{n} more package{}", if n == 1 { "" } else { "s" }), None)),
            None => {}
        }
        rows.append(&prop(
            "Installs for",
            if p.kind == Kind::AppImage { "only you" } else { "everyone on this computer" },
            None,
        ));
        body.append(&rows);
        if p.kind == Kind::AppImage {
            body.append(&note(
                "dialog-warning-symbolic",
                "An AppImage runs as you and can open your files. Install it only if you trust where it came from.",
                true,
            ));
        } else {
            body.append(&note("system-lock-screen-symbolic", "Asks for your password once, at Install.", false));
        }

        let foot = footer();
        let me = Rc::downgrade(self);
        foot.append(&button("Next", "suggested-action", move || {
            if let Some(w) = me.upgrade() {
                w.options();
            }
        }));
        match p.kind {
            Kind::AppImage => {
                if let Some(v) = p.installed.clone() {
                    let me = Rc::downgrade(self);
                    foot.append(&button(&format!("Uninstall {v}"), "flat", move || {
                        if let Some(w) = me.upgrade() {
                            w.uninstall();
                        }
                    }));
                } else {
                    let (me, pk) = (Rc::downgrade(self), p.clone());
                    foot.append(&button("Run Without Installing", "flat", move || {
                        let Some(w) = me.upgrade() else { return };
                        match appimage::run_once(&pk) {
                            Ok(()) => {
                                w.dialog.close();
                            }
                            Err(e) => w.ops.toast(&format!("Couldn't start it: {}", crate::browser::human_io(&e))),
                        }
                    }));
                }
            }
            _ => foot.append(&self.browse_button(true)),
        }
        self.page("intro", body.upcast_ref(), Some(foot.upcast_ref()));
    }

    /// Browse Contents: the package is the selection (it was just opened), so the menu's Browse
    /// Archive does it.
    fn browse_button(&self, flat: bool) -> gtk::Button {
        let (ops, d) = (self.ops.clone(), self.dialog.clone());
        button("Browse Contents", if flat { "flat" } else { "" }, move || {
            d.close();
            if let Some(w) = ops.window() {
                _ = WidgetExt::activate_action(&w, "win.browse-archive", None);
            }
        })
    }

    // ── 2: where it goes ──

    fn options(self: &Rc<Self>) {
        let p = self.pkg();
        self.step(Some(1));
        self.back.set_visible(true);
        self.title.set_label(&format!("Install {}", p.name));
        let body = page_body();

        let preview = gtk::Box::builder().spacing(28).halign(gtk::Align::Center).build();
        preview.append(&tile(&p, 56, "installer-tile"));
        let desk = tile(&p, 48, "installer-tile");
        desk.set_visible(self.desktop_icon.get());
        preview.append(&desk);
        let frame = gtk::Box::builder().css_classes(["installer-preview"]).build();
        preview.set_hexpand(true);
        frame.append(&preview);
        body.append(&frame);
        body.append(&caption("How it will look in your apps and on the Desktop"));

        let list = gtk::ListBox::builder().css_classes(["boxed-list"]).selection_mode(gtk::SelectionMode::None).build();
        if p.kind == Kind::AppImage {
            list.append(
                &adw::SwitchRow::builder()
                    .title("Add to your apps")
                    .subtitle("Search and the app grid")
                    .active(true)
                    .sensitive(false)
                    .build(),
            );
        }
        let icon = adw::SwitchRow::builder().title("Add an icon to the Desktop").active(self.desktop_icon.get()).build();
        let me = Rc::downgrade(self);
        icon.connect_active_notify(move |s| {
            desk.set_visible(s.is_active());
            if let Some(w) = me.upgrade() {
                w.desktop_icon.set(s.is_active());
            }
        });
        list.append(&icon);
        let into_place = p.kind == Kind::AppImage
            && p.path.parent().is_some_and(|d| d.canonicalize().ok() != Places::user().bin.canonicalize().ok());
        if into_place {
            let keep = adw::SwitchRow::builder()
                .title("Remove the download")
                .subtitle(format!("Moves it instead of copying, saves {}", glib::format_size(p.size)))
                .active(!self.keep_download.get())
                .build();
            let me = Rc::downgrade(self);
            keep.connect_active_notify(move |s| {
                if let Some(w) = me.upgrade() {
                    w.keep_download.set(!s.is_active());
                }
            });
            list.append(&keep);
        }
        body.append(&list);
        let where_ = match p.kind {
            Kind::AppImage => format!(
                "Goes to ~/Applications/{}. No password needed; only you get it.",
                appimage::file_name(&p.name)
            ),
            _ => format!(
                "{} installs it for everyone on this computer, with anything else it needs, and asks for your password.",
                match self.pm {
                    Pm::Apt => "apt",
                    Pm::Dnf => "dnf",
                    _ => "zypper",
                }
            ),
        };
        body.append(&note("dialog-information-symbolic", &where_, false));

        let verb = match &p.installed {
            None => "Install".to_string(),
            Some(v) if *v == p.version => "Reinstall".to_string(),
            Some(_) => "Update".to_string(),
        };
        let foot = footer();
        let me = Rc::downgrade(self);
        foot.append(&button(&verb, "suggested-action", move || {
            if let Some(w) = me.upgrade() {
                w.install();
            }
        }));
        self.page("options", body.upcast_ref(), Some(foot.upcast_ref()));
    }

    // ── 3: installing ──

    fn install(self: &Rc<Self>) {
        let p = self.pkg();
        self.step(Some(2));
        self.back.set_visible(false);
        self.title.set_label(&format!("Installing {}", p.name));
        self.dialog.set_can_close(false);
        let body = page_body();
        let img = app_icon(&p, 72);
        img.set_halign(gtk::Align::Center);
        body.append(&img);
        let bar = gtk::ProgressBar::builder().css_classes(["installer-progress"]).build();
        body.append(&bar);
        let desktop = self.desktop_icon.get();
        let mut names: Vec<String> = match p.kind {
            Kind::AppImage => vec![
                if self.keep_download.get() { "Copying to ~/Applications" } else { "Moving to ~/Applications" }.to_string(),
                format!("Adding {} to your apps, with its icon", p.name),
            ],
            _ => vec![
                "Waiting for your password".to_string(),
                match p.needs {
                    Some(n) if n > 0 => format!("Installing {} and {n} more package{}", p.name, if n == 1 { "" } else { "s" }),
                    _ => format!("Installing {}", p.name),
                },
            ],
        };
        if desktop {
            names.push("Adding the Desktop icon".into());
        }
        let steps = Steps::new(&names);
        body.append(&steps.root);
        let log = gtk::TextView::builder()
            .editable(false)
            .monospace(true)
            .cursor_visible(false)
            .wrap_mode(gtk::WrapMode::WordChar)
            .css_classes(["installer-log"])
            .build();
        let details = gtk::Expander::builder()
            .label("Details")
            .child(&gtk::ScrolledWindow::builder().child(&log).height_request(130).build())
            .build();
        body.append(&details);
        let foot = footer();
        let close = close_button(&self.dialog);
        close.set_visible(false);
        foot.append(&close);
        self.page("installing", body.upcast_ref(), Some(foot.upcast_ref()));

        let ui = Progress {
            bar,
            steps,
            log,
            details,
            close,
            body,
        };
        let me = self.clone();
        glib::spawn_future_local(async move {
            let r = match p.kind {
                Kind::AppImage => me.run_appimage(&p, &ui, desktop).await,
                _ => me.run_package(&p, &ui, desktop).await,
            };
            me.dialog.set_can_close(true);
            match r {
                Ok(done) => me.done(done),
                Err(e) => {
                    ui.bar.set_fraction(0.0);
                    ui.details.set_expanded(true);
                    ui.body.append(&note("dialog-error-symbolic", &e, true));
                    ui.close.set_visible(true);
                    me.title.set_label(&format!("Couldn't install {}", p.name));
                }
            }
        });
    }

    async fn run_appimage(&self, p: &Arc<Pkg>, ui: &Progress, desktop: bool) -> Result<Done, String> {
        let places = Places::user();
        let keep = self.keep_download.get();
        let n = ui.steps.len() as f64;
        ui.steps.set(0, Mark::Run);
        let (pk, pl) = (p.clone(), places.clone());
        let dest = gio::spawn_blocking(move || appimage::place(&pk, keep, &pl))
            .await
            .map_err(|_| "The installer stopped".to_string())?
            .map_err(|e| {
                ui.steps.set(0, Mark::Fail);
                format!("Couldn't put it in ~/Applications: {}", crate::browser::human_io(&e))
            })?;
        ui.log(&format!("placed: {}", dest.display()));
        ui.steps.set(0, Mark::Ok);
        ui.bar.set_fraction(1.0 / n);
        ui.steps.set(1, Mark::Run);
        let (pk, pl, d) = (p.clone(), places.clone(), dest.clone());
        let launcher = gio::spawn_blocking(move || appimage::add_launcher(&pk, &d, &pl))
            .await
            .map_err(|_| "The installer stopped".to_string())?
            .map_err(|e| {
                ui.steps.set(1, Mark::Fail);
                format!("Couldn't add it to your apps: {}", crate::browser::human_io(&e))
            })?;
        let f = p.appimage.as_ref().expect("AppImage facts");
        ui.log(&format!("icons: {} size{} as kestrel-{}", f.icons.len(), if f.icons.len() == 1 { "" } else { "s" }, f.slug));
        ui.log(&format!("launcher: {}", launcher.display()));
        ui.steps.set(1, Mark::Ok);
        ui.bar.set_fraction(2.0 / n);
        let mut notes = Vec::new();
        if desktop {
            ui.steps.set(2, Mark::Run);
            match super::to_desktop(&launcher, &places.desktop) {
                Ok(_) => ui.steps.set(2, Mark::Ok),
                Err(e) => {
                    ui.steps.set(2, Mark::Fail);
                    notes.push(format!("Couldn't add the Desktop icon: {}", crate::browser::human_io(&e)));
                }
            }
        }
        ui.bar.set_fraction(1.0);
        if f.icons.is_empty() {
            notes.push("It came without an icon, so it shows a plain one.".into());
        }
        if f.no_sandbox {
            notes.push(format!("Starts without Electron's sandbox: {} blocks it for apps installed this way.", super::os_name()));
        }
        if f.extract_and_run {
            notes.push("FUSE isn't set up here, so it unpacks itself each time it starts (a little slower).".into());
        }
        Ok(Done {
            launcher: Some(launcher),
            file: Some(dest),
            on_desktop: desktop,
            notes,
        })
    }

    async fn run_package(&self, p: &Arc<Pkg>, ui: &Progress, desktop: bool) -> Result<Done, String> {
        ui.steps.set(0, Mark::Run);
        let argv = pkg::install_argv(p, self.pm);
        let argv: Vec<&std::ffi::OsStr> = argv.iter().map(|s| s.as_os_str()).collect();
        let proc = gio::Subprocess::newv(&argv, gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_MERGE)
            .map_err(|e| format!("Couldn't start the installer: {e}"))?;
        let pulsing = Rc::new(Cell::new(true));
        let (bar, flag) = (ui.bar.clone(), pulsing.clone());
        glib::timeout_add_local(std::time::Duration::from_millis(120), move || {
            if flag.get() {
                bar.pulse();
                glib::ControlFlow::Continue
            } else {
                glib::ControlFlow::Break
            }
        });
        let out = gio::DataInputStream::new(&proc.stdout_pipe().expect("piped"));
        let mut first = true;
        while let Ok(Some(line)) = out.read_line_utf8_future(glib::Priority::DEFAULT).await {
            if first {
                // output means pkexec let it through
                ui.steps.set(0, Mark::Ok);
                ui.steps.set(1, Mark::Run);
                first = false;
            }
            ui.log(&line);
        }
        let _ = proc.wait_future().await;
        pulsing.set(false);
        match proc.exit_status() {
            0 => {}
            126 | 127 => {
                ui.steps.set(0, Mark::Fail);
                return Err("Not installed: the password wasn't given.".into());
            }
            _ => {
                ui.steps.set(if first { 0 } else { 1 }, Mark::Fail);
                return Err("The package manager stopped with an error; Details says why.".into());
            }
        }
        ui.steps.set(0, Mark::Ok);
        ui.steps.set(1, Mark::Ok);
        ui.bar.set_fraction(0.85);
        let pk = p.clone();
        let found = gio::spawn_blocking(move || pkg::launchers(&pk)).await.unwrap_or_default();
        let mut notes = Vec::new();
        if found.is_empty() {
            notes.push("It has no app launcher: it's a command-line tool or a library.".into());
        }
        if desktop {
            ui.steps.set(2, Mark::Run);
            let res = found.iter().map(|l| super::to_desktop(l, &Places::user().desktop)).find_map(Result::err);
            match res {
                None => ui.steps.set(2, Mark::Ok),
                Some(e) => {
                    ui.steps.set(2, Mark::Fail);
                    notes.push(format!("Couldn't add the Desktop icon: {}", crate::browser::human_io(&e)));
                }
            }
        }
        ui.bar.set_fraction(1.0);
        Ok(Done {
            on_desktop: desktop && !found.is_empty(),
            launcher: found.into_iter().next(),
            file: None,
            notes,
        })
    }

    // ── done ──

    fn done(self: &Rc<Self>, d: Done) {
        let p = self.pkg();
        self.step(None);
        self.title.set_label("");
        let body = page_body();
        body.set_valign(gtk::Align::Center);
        let badge = gtk::Overlay::builder().halign(gtk::Align::Center).child(&app_icon(&p, 96)).build();
        badge.add_overlay(
            &gtk::Label::builder()
                .label("✓")
                .halign(gtk::Align::End)
                .valign(gtk::Align::End)
                .css_classes(["installer-badge"])
                .build(),
        );
        body.append(&badge);
        body.append(&gtk::Label::builder().label(format!("{} is installed", p.name)).css_classes(["title-2"]).wrap(true).justify(gtk::Justification::Center).build());
        if d.launcher.is_some() {
            body.append(&centered(
                if d.on_desktop { "Find it in your apps or on the Desktop." } else { "Find it in your apps." },
                "installer-desc",
            ));
        }
        for n in &d.notes {
            body.append(&note("dialog-information-symbolic", n, false));
        }
        let foot = footer();
        if let Some(l) = d.launcher.clone() {
            let (ops, dl) = (self.ops.clone(), self.dialog.clone());
            foot.append(&button(&format!("Open {}", p.name), "suggested-action", move || {
                match super::launch(&l) {
                    Ok(()) => {
                        dl.close();
                    }
                    Err(e) => ops.toast(&format!("Couldn't open it: {e}")),
                }
            }));
        }
        let two = gtk::Box::builder().spacing(8).homogeneous(true).build();
        if let Some(f) = d.file.clone() {
            two.append(&button("Show in Folder", "", move || show_in_folder(&f)));
        }
        two.append(&close_button(&self.dialog));
        foot.append(&two);
        self.page("done", body.upcast_ref(), Some(foot.upcast_ref()));
    }

    fn uninstall(self: &Rc<Self>) {
        let p = self.pkg();
        let a = adw::AlertDialog::builder()
            .heading(format!("Uninstall {}?", p.name))
            .body(format!(
                "Removes ~/Applications/{}, its launcher and its icons. Settings and documents it saved elsewhere stay.",
                appimage::file_name(&p.name)
            ))
            .build();
        a.add_responses(&[("cancel", "Cancel"), ("remove", "Uninstall")]);
        a.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
        a.set_close_response("cancel");
        let me = Rc::downgrade(self);
        a.connect_response(Some("remove"), move |_, _| {
            let Some(w) = me.upgrade() else { return };
            let p = w.pkg();
            match appimage::uninstall(&p.id, &Places::user()) {
                Ok(()) => {
                    w.ops.toast(&format!("{} is uninstalled", p.name));
                    w.dialog.close();
                }
                Err(e) => w.ops.toast(&format!("Couldn't uninstall it: {}", crate::browser::human_io(&e))),
            }
        });
        a.present(Some(&self.dialog));
    }
}

impl Pkg {
    fn version_line(&self) -> String {
        if self.version.is_empty() {
            String::new()
        } else {
            format!("Version {}", self.version)
        }
    }
}

/// How an install went, for the last page.
struct Done {
    launcher: Option<PathBuf>,
    /// An AppImage's file, for Show in Folder.
    file: Option<PathBuf>,
    on_desktop: bool,
    notes: Vec<String>,
}

struct Progress {
    bar: gtk::ProgressBar,
    steps: Steps,
    log: gtk::TextView,
    details: gtk::Expander,
    close: gtk::Button,
    body: gtk::Box,
}

impl Progress {
    fn log(&self, line: &str) {
        let buf = self.log.buffer();
        buf.insert(&mut buf.end_iter(), &format!("{line}\n"));
        let mark = buf.create_mark(None, &buf.end_iter(), false);
        self.log.scroll_to_mark(&mark, 0.0, false, 0.0, 0.0);
    }
}

#[derive(Clone, Copy)]
enum Mark {
    Todo,
    Run,
    Ok,
    Fail,
}

/// The checklist on the installing page: ○ to do, a spinner, ✓ done, ✕ failed.
struct Steps {
    root: gtk::Box,
    rows: Vec<(gtk::Stack, gtk::Label)>,
}

impl Steps {
    fn new(names: &[String]) -> Steps {
        let root = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(9).css_classes(["installer-steps"]).build();
        let mut rows = Vec::new();
        for n in names {
            let mark = gtk::Stack::builder().width_request(16).build();
            mark.add_named(&gtk::Label::builder().label("○").css_classes(["installer-check-todo"]).build(), Some("todo"));
            mark.add_named(&adw::Spinner::builder().width_request(16).height_request(16).build(), Some("run"));
            mark.add_named(&gtk::Label::builder().label("✓").css_classes(["installer-check-ok"]).build(), Some("ok"));
            mark.add_named(&gtk::Label::builder().label("✕").css_classes(["installer-check-fail"]).build(), Some("fail"));
            let label = gtk::Label::builder().label(n).xalign(0.0).wrap(true).css_classes(["dim-label"]).build();
            let row = gtk::Box::builder().spacing(10).build();
            row.append(&mark);
            row.append(&label);
            root.append(&row);
            rows.push((mark, label));
        }
        let s = Steps { root, rows };
        for i in 0..s.len() {
            s.set(i, Mark::Todo);
        }
        s
    }

    fn len(&self) -> usize {
        self.rows.len()
    }

    fn set(&self, i: usize, m: Mark) {
        let Some((mark, label)) = self.rows.get(i) else { return };
        mark.set_visible_child_name(match m {
            Mark::Todo => "todo",
            Mark::Run => "run",
            Mark::Ok => "ok",
            Mark::Fail => "fail",
        });
        if matches!(m, Mark::Todo) {
            label.add_css_class("dim-label");
        } else {
            label.remove_css_class("dim-label");
        }
    }
}

// ── pieces ──

fn page_body() -> gtk::Box {
    gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(14)
        .margin_start(22)
        .margin_end(22)
        .margin_top(10)
        .build()
}

fn footer() -> gtk::Box {
    gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(8)
        .margin_start(22)
        .margin_end(22)
        .margin_top(14)
        .margin_bottom(18)
        .build()
}

fn button(label: &str, class: &str, f: impl Fn() + 'static) -> gtk::Button {
    let b = gtk::Button::builder().label(label).css_classes(["installer-button"]).build();
    if !class.is_empty() {
        b.add_css_class(class);
    }
    b.connect_clicked(move |_| f());
    b
}

fn close_button(d: &adw::Dialog) -> gtk::Button {
    let d = d.clone();
    button("Close", "", move || {
        d.close();
    })
}

fn app_icon(p: &Pkg, px: i32) -> gtk::Image {
    let img = match &p.icon {
        Some(f) => gtk::Image::from_file(f),
        None => gtk::Image::from_icon_name("application-x-executable"),
    };
    img.set_pixel_size(px);
    img.add_css_class("installer-icon");
    img.update_property(&[gtk::accessible::Property::Label(&format!("{} icon", p.name))]);
    img
}

/// The icon, the name, then lines under it (empty ones skipped).
fn hero(p: &Pkg, px: i32, lines: &[&str]) -> gtk::Box {
    let b = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(4).halign(gtk::Align::Center).build();
    b.append(&app_icon(p, px));
    let name = gtk::Label::builder().label(&p.name).css_classes(["title-2", "installer-name"]).wrap(true).justify(gtk::Justification::Center).build();
    b.append(&name);
    for (i, l) in lines.iter().enumerate().filter(|(_, l)| !l.is_empty()) {
        b.append(&centered(l, if i == 0 { "installer-sub" } else { "installer-desc" }));
    }
    b
}

fn centered(text: &str, class: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .wrap(true)
        .max_width_chars(34)
        .justify(gtk::Justification::Center)
        .css_classes([class])
        .build()
}

fn caption(text: &str) -> gtk::Label {
    gtk::Label::builder().label(text).css_classes(["caption", "dim-label"]).build()
}

fn props() -> gtk::ListBox {
    gtk::ListBox::builder().css_classes(["boxed-list", "installer-props"]).selection_mode(gtk::SelectionMode::None).build()
}

/// A row: the label on the left, its value (and a pill) on the right.
fn prop(key: &str, value: &str, pill_of: Option<(&str, &str)>) -> gtk::ListBoxRow {
    let b = gtk::Box::builder().spacing(12).margin_start(14).margin_end(14).margin_top(9).margin_bottom(9).build();
    b.append(&gtk::Label::builder().label(key).xalign(0.0).hexpand(true).css_classes(["dim-label", "caption"]).build());
    let v = gtk::Box::builder().spacing(6).build();
    v.append(&gtk::Label::builder().label(value).xalign(1.0).wrap(true).selectable(true).build());
    if let Some((t, c)) = pill_of {
        v.append(&pill(t, c));
    }
    b.append(&v);
    gtk::ListBoxRow::builder().child(&b).activatable(false).build()
}

fn pill(text: &str, class: &str) -> gtk::Label {
    let l = gtk::Label::builder().label(text).css_classes(["installer-pill"]).valign(gtk::Align::Center).build();
    if !class.is_empty() {
        l.add_css_class(class);
    }
    l
}

fn note(icon: &str, text: &str, warn: bool) -> gtk::Box {
    let b = gtk::Box::builder().spacing(10).css_classes(["installer-note"]).build();
    if warn {
        b.add_css_class("warn");
    }
    b.append(&gtk::Image::builder().icon_name(icon).valign(gtk::Align::Start).css_classes(["installer-note-icon"]).build());
    b.append(&gtk::Label::builder().label(text).wrap(true).xalign(0.0).hexpand(true).build());
    b
}

/// The launcher as it will look: icon over name.
fn tile(p: &Pkg, px: i32, class: &str) -> gtk::Box {
    let b = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(6).css_classes([class]).build();
    b.append(&app_icon(p, px));
    b.append(&gtk::Label::builder().label(&p.name).ellipsize(gtk::pango::EllipsizeMode::End).max_width_chars(12).css_classes(["caption"]).build());
    b
}

fn loading(text: &str) -> gtk::Widget {
    let b = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .valign(gtk::Align::Center)
        .halign(gtk::Align::Center)
        .build();
    b.append(&adw::Spinner::builder().width_request(32).height_request(32).build());
    b.append(&gtk::Label::builder().label(text).wrap(true).css_classes(["dim-label"]).build());
    b.upcast()
}

fn status_page(icon: &str, title: &str, desc: &str) -> gtk::Widget {
    adw::StatusPage::builder().icon_name(icon).title(title).description(glib::markup_escape_text(desc)).build().upcast()
}

/// A new Kestrel window on the AppImage's folder, with it selected.
fn show_in_folder(file: &Path) {
    let me = std::env::current_exe().unwrap_or_else(|_| "kestrel".into());
    if let (Some(dir), Some(name)) = (file.parent(), file.file_name()) {
        let _ = std::process::Command::new(me).arg(dir).arg("--select").arg(name).spawn();
    }
}
