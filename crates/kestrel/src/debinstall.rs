//! Install a .deb: a two-step wizard (what it is → installing), then a launcher on the Desktop.
//! apt does the work (`pkexec apt-get install ./x.deb`), so dependencies come from the archive.

use crate::fileops::Ops;
use adw::prelude::*;
use gtk::{gio, glib};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;

pub fn is_deb(name: &str) -> bool {
    name.to_lowercase().ends_with(".deb")
}

struct Deb {
    package: String,
    version: String,
    arch: String,
    size_kib: u64,
    summary: String,
    /// The version already installed, if any.
    installed: Option<String>,
}

fn read(path: &Path) -> Option<Deb> {
    let out = Command::new("dpkg-deb")
        .args([
            "--show",
            "--showformat=${Package}\\n${Version}\\n${Architecture}\\n${Installed-Size}\\n${binary:Summary}\\n",
        ])
        .arg(path)
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let mut l = text.lines().map(str::to_string);
    let (package, version, arch) = (l.next()?, l.next()?, l.next()?);
    let size_kib = l.next()?.parse().unwrap_or(0);
    let summary = l.next().unwrap_or_default();
    let q = Command::new("dpkg-query")
        .args(["-W", "-f=${db:Status-Status}\\t${Version}", &package])
        .output()
        .ok();
    let installed = q
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .and_then(|s| s.strip_prefix("installed\t").map(str::to_string));
    Some(Deb {
        package,
        version,
        arch,
        size_kib,
        summary,
        installed,
    })
}

/// The package's visible launchers (`/usr/share/applications/*.desktop`, minus NoDisplay ones).
fn launchers(package: &str) -> Vec<PathBuf> {
    let Ok(o) = Command::new("dpkg-query").args(["-L", package]).output() else {
        return vec![];
    };
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .filter(|l| l.starts_with("/usr/share/applications/") && l.ends_with(".desktop"))
        .map(PathBuf::from)
        .filter(|p| {
            let k = glib::KeyFile::new();
            let hidden = |key| k.boolean("Desktop Entry", key).unwrap_or(false);
            k.load_from_file(p, glib::KeyFileFlags::NONE).is_ok() && !hidden("NoDisplay") && !hidden("Hidden")
        })
        .collect()
}

/// Copy a launcher to ~/Desktop, executable and marked trusted so Desktop Icons NG runs it.
fn to_desktop(launcher: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let dir = glib::user_special_dir(glib::UserDirectory::Desktop).unwrap_or_else(|| glib::home_dir().join("Desktop"));
    std::fs::create_dir_all(&dir)?;
    let dst = dir.join(launcher.file_name().unwrap_or_default());
    std::fs::copy(launcher, &dst)?;
    std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(0o755))?;
    gio::File::for_path(&dst)
        .set_attribute_string("metadata::trusted", "true", gio::FileQueryInfoFlags::NONE, gio::Cancellable::NONE)
        .map_err(std::io::Error::other)
}

fn row(title: &str, value: &str) -> adw::ActionRow {
    let r = adw::ActionRow::builder()
        .title(title)
        .subtitle(glib::markup_escape_text(value))
        .css_classes(["property"])
        .build();
    r.set_subtitle_selectable(true);
    r
}

/// Step 1: what the package is, whether it's installed, and the Desktop icon choice.
pub fn show(ops: &Rc<Ops>, f: &gio::File) {
    let Some(path) = f.path() else { return };
    let ops = ops.clone();
    glib::spawn_future_local(async move {
        let p = path.clone();
        let Ok(Some(deb)) = gio::spawn_blocking(move || read(&p)).await else {
            ops.toast("Couldn't read this package (is it a .deb?)");
            return;
        };
        let list = gtk::ListBox::builder()
            .css_classes(["boxed-list"])
            .selection_mode(gtk::SelectionMode::None)
            .build();
        list.append(&row("Version", &deb.version));
        list.append(&row("Architecture", &deb.arch));
        list.append(&row("Installed size", &glib::format_size(deb.size_kib * 1024)));
        let (status, ok) = match &deb.installed {
            None => ("Not installed".to_string(), "Install"),
            Some(v) if *v == deb.version => ("This version is installed".to_string(), "Reinstall"),
            Some(v) => (format!("Version {v} is installed"), "Replace"),
        };
        list.append(&row("Status", &status));
        let icon = adw::SwitchRow::builder().title("Add an icon to the Desktop").active(true).build();
        list.append(&icon);

        let d = adw::AlertDialog::builder()
            .heading(format!("Install {}?", deb.package))
            .body(&deb.summary)
            .extra_child(&list)
            .build();
        d.add_responses(&[("cancel", "Cancel"), ("browse", "Browse Contents"), ("ok", ok)]);
        d.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
        d.set_default_response(Some("ok"));
        d.set_close_response("cancel");
        // the .deb is the selection (it was just opened), so the menu's Browse Archive does it
        let ops2 = ops.clone();
        d.connect_response(Some("browse"), move |_, _| {
            if let Some(w) = ops2.window() {
                _ = WidgetExt::activate_action(&w, "win.browse-archive", None);
            }
        });
        let ops2 = ops.clone();
        d.connect_response(Some("ok"), move |_, _| install(&ops2, &path, &deb, icon.is_active()));
        d.present(ops.window().as_ref());
    });
}

/// Step 2: run apt under pkexec, showing its output, then add the Desktop icon and offer Open.
fn install(ops: &Rc<Ops>, path: &Path, deb: &Deb, desktop_icon: bool) {
    let mut argv: Vec<&std::ffi::OsStr> = [
        "pkexec",
        "env",
        "DEBIAN_FRONTEND=noninteractive",
        "apt-get",
        "install",
        "-y",
        "--allow-downgrades",
    ]
    .iter()
    .map(|s| s.as_ref())
    .collect();
    if deb.installed.as_deref() == Some(&deb.version) {
        argv.push("--reinstall".as_ref());
    }
    argv.push(path.as_os_str());

    let spinner = adw::Spinner::builder().height_request(32).build();
    let log = gtk::TextView::builder()
        .editable(false)
        .monospace(true)
        .cursor_visible(false)
        .wrap_mode(gtk::WrapMode::WordChar)
        .build();
    let scroll = gtk::ScrolledWindow::builder().child(&log).height_request(200).build();
    let details = gtk::Expander::builder().label("Details").child(&scroll).build();
    let body = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(12).build();
    body.append(&spinner);
    body.append(&details);
    let d = adw::AlertDialog::builder()
        .heading(format!("Installing {}…", deb.package))
        .extra_child(&body)
        .build();
    d.add_responses(&[("close", "Close"), ("open", "Open")]);
    d.set_response_enabled("close", false);
    d.set_response_enabled("open", false);
    d.set_response_appearance("open", adw::ResponseAppearance::Suggested);
    d.set_close_response("close");
    d.set_can_close(false);
    d.present(ops.window().as_ref());

    let package = deb.package.clone();
    let proc = gio::Subprocess::newv(&argv, gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_MERGE);
    let ops = ops.clone();
    glib::spawn_future_local(async move {
        let buf = log.buffer();
        let ok = match proc {
            Err(e) => {
                buf.set_text(&e.to_string());
                Err("Couldn't start the installer".to_string())
            }
            Ok(p) => {
                let out = gio::DataInputStream::new(&p.stdout_pipe().expect("piped"));
                while let Ok(Some(line)) = out.read_line_utf8_future(glib::Priority::DEFAULT).await {
                    buf.insert(&mut buf.end_iter(), &format!("{line}\n"));
                    log.scroll_to_mark(&buf.create_mark(None, &buf.end_iter(), false), 0.0, false, 0.0, 0.0);
                }
                _ = p.wait_future().await;
                match p.exit_status() {
                    0 => Ok(()),
                    // pkexec: the password prompt was dismissed / not allowed
                    126 | 127 => Err("Not installed: permission wasn't given".to_string()),
                    _ => Err("Install failed: see Details".to_string()),
                }
            }
        };
        spinner.set_visible(false);
        d.set_can_close(true);
        d.set_response_enabled("close", true);
        if let Err(e) = ok {
            d.set_heading(Some(&e));
            details.set_expanded(true);
            return;
        }
        let pkg = package.clone();
        let found = gio::spawn_blocking(move || launchers(&pkg)).await.unwrap_or_default();
        d.set_heading(Some(&format!("{package} is installed")));
        let msg = if found.is_empty() {
            "It has no app launcher (a command-line tool or a library).".to_string()
        } else if !desktop_icon {
            String::new()
        } else {
            match found.iter().find_map(|l| to_desktop(l).err()) {
                None => "Its icon is on the Desktop.".into(),
                Some(e) => format!("Couldn't add the Desktop icon: {e}"),
            }
        };
        d.set_body(&msg);
        if let Some(app) = found.first().cloned() {
            d.set_response_enabled("open", true);
            d.set_default_response(Some("open"));
            d.connect_response(Some("open"), move |_, _| {
                // ponytail: `gio launch` rather than the gio-unix crate just for DesktopAppInfo
                if let Err(e) = gio::Subprocess::newv(&["gio".as_ref(), "launch".as_ref(), app.as_os_str()], gio::SubprocessFlags::NONE) {
                    ops.toast(&format!("Couldn't open it: {e}"));
                }
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_built_deb() {
        let dir = std::env::temp_dir().join(format!("kestrel-deb-{}", std::process::id()));
        let root = dir.join("pkg");
        std::fs::create_dir_all(root.join("DEBIAN")).unwrap();
        std::fs::write(
            root.join("DEBIAN/control"),
            "Package: kestrel-test-pkg\nVersion: 1.2-3\nArchitecture: all\nMaintainer: t <t@t>\nInstalled-Size: 4\nDescription: a test package\n longer text\n",
        )
        .unwrap();
        let deb = dir.join("t.deb");
        assert!(
            Command::new("dpkg-deb")
                .args(["--build", "--root-owner-group"])
                .arg(&root)
                .arg(&deb)
                .output()
                .unwrap()
                .status
                .success()
        );
        let d = read(&deb).unwrap();
        assert_eq!(
            (d.package.as_str(), d.version.as_str(), d.arch.as_str(), d.size_kib),
            ("kestrel-test-pkg", "1.2-3", "all", 4)
        );
        assert_eq!(d.summary, "a test package");
        assert_eq!(d.installed, None);
        assert!(read(&root.join("DEBIAN/control")).is_none());
        assert!(is_deb("X.DEB") && !is_deb("x.udeb.txt"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
