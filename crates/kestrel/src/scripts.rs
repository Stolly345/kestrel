//! Right-click → Scripts: executables in ~/.local/share/nautilus/scripts, run the way Nautilus
//! runs them (same folder, same environment variables), so existing Nautilus scripts just work.

use gtk::prelude::*;
use gtk::{gio, glib};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

pub fn dir() -> PathBuf {
    glib::user_data_dir().join("nautilus/scripts")
}

thread_local! {
    /// Keeps the folder monitors alive (one per window menu).
    static MONITORS: std::cell::RefCell<Vec<gio::FileMonitor>> = Default::default();
}

/// Fill `section` with a "Scripts" submenu (nothing if there are none) and keep it current.
pub fn watch(section: &gio::Menu) {
    fill(section);
    let Ok(m) = gio::File::for_path(dir()).monitor_directory(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE) else {
        return;
    };
    let s = section.clone();
    m.connect_changed(move |_, _, _, _| fill(&s));
    MONITORS.with_borrow_mut(|v| v.push(m));
}

fn fill(section: &gio::Menu) {
    section.remove_all();
    let tree = gio::Menu::new();
    if add(&tree, &dir()) {
        section.append_submenu(Some("Scripts"), &tree);
    }
}

/// Executables in `d` (folders become submenus), sorted by name; false if none.
fn add(menu: &gio::Menu, d: &Path) -> bool {
    let Ok(rd) = std::fs::read_dir(d) else { return false };
    let mut entries: Vec<_> = rd.flatten().filter(|e| !e.file_name().to_string_lossy().starts_with('.')).collect();
    entries.sort_by_key(|e| e.file_name().to_string_lossy().to_lowercase());
    let mut any = false;
    for e in entries {
        let (path, name) = (e.path(), e.file_name().to_string_lossy().into_owned());
        let Ok(md) = std::fs::metadata(&path) else { continue };
        if md.is_dir() {
            let sub = gio::Menu::new();
            if add(&sub, &path) {
                menu.append_submenu(Some(&name), &sub);
                any = true;
            }
        } else if md.permissions().mode() & 0o111 != 0 {
            let item = gio::MenuItem::new(Some(&name), None);
            item.set_action_and_target_value(Some("win.script"), Some(&path.to_string_lossy().to_variant()));
            menu.append_item(&item);
            any = true;
        }
    }
    any
}

/// Run `script` on `files` from folder `cwd`, like Nautilus: arguments are the names (relative
/// when the files are in `cwd`), plus the NAUTILUS_SCRIPT_* variables.
pub fn run(script: &Path, files: &[gio::File], cwd: &gio::File) -> std::io::Result<()> {
    let lines = |v: Vec<String>| v.into_iter().map(|s| s + "\n").collect::<String>();
    let mut c = std::process::Command::new(script);
    c.args(args(files, cwd))
        .env(
            "NAUTILUS_SCRIPT_SELECTED_FILE_PATHS",
            lines(files.iter().filter_map(|f| f.path()).map(|p| p.display().to_string()).collect()),
        )
        .env("NAUTILUS_SCRIPT_SELECTED_URIS", lines(files.iter().map(|f| f.uri().to_string()).collect()))
        .env("NAUTILUS_SCRIPT_CURRENT_URI", cwd.uri().as_str())
        .stdin(std::process::Stdio::null());
    if let Some(p) = cwd.path() {
        c.current_dir(p);
    }
    let mut child = c.spawn()?;
    // reap it whenever it finishes; scripts may run for minutes
    std::thread::spawn(move || child.wait());
    Ok(())
}

fn args(files: &[gio::File], cwd: &gio::File) -> Vec<String> {
    files
        .iter()
        .map(|f| match (f.parent().is_some_and(|p| p.equal(cwd)), f.path()) {
            (true, _) => f.basename().unwrap_or_default().to_string_lossy().into_owned(),
            (false, Some(p)) => p.display().to_string(),
            (false, None) => f.uri().to_string(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nautilus_style_args() {
        let cwd = gio::File::for_path("/home/a/Pictures");
        let files = [cwd.child("x y.jpg"), gio::File::for_path("/tmp/z.png"), gio::File::for_uri("sftp://h/f.txt")];
        assert_eq!(args(&files, &cwd), ["x y.jpg", "/tmp/z.png", "sftp://h/f.txt"]);
    }
}
