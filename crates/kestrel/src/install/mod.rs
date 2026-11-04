//! Install an app from a file: a .deb, an .rpm or an AppImage, in one click-through wizard
//! (what it is → where it goes → installing → done) that ends with a launcher carrying the
//! app's own icon, in the app grid and, if wanted, on the Desktop.
//!
//! Everything about the file is read before anything is installed, without running it: package
//! metadata from dpkg-deb / rpm, and the launcher and icon from inside the package or the
//! AppImage's squashfs, unpacked in the archive sandbox. .deb and .rpm install through the
//! system's package manager under pkexec, so dependencies come from the distribution. An
//! AppImage needs no password: it goes to ~/Applications with a launcher of its own.

mod appimage;
mod pkg;
mod wizard;

pub use wizard::show;

use gtk::prelude::*;
use gtk::{gio, glib};
use std::path::{Path, PathBuf};

/// Whether double-clicking `name` opens the installer.
pub fn handles(name: &str) -> bool {
    kind_of(name).is_some()
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    Deb,
    Rpm,
    AppImage,
}

pub fn kind_of(name: &str) -> Option<Kind> {
    let n = name.to_lowercase();
    if n.ends_with(".deb") {
        Some(Kind::Deb)
    } else if n.ends_with(".rpm") && !n.ends_with(".src.rpm") {
        Some(Kind::Rpm)
    } else if n.ends_with(".appimage") {
        Some(Kind::AppImage)
    } else {
        None
    }
}

/// The package manager this system installs with.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Pm {
    Apt,
    Dnf,
    Zypper,
    Unknown,
}

pub fn pm() -> Pm {
    let has = |p: &str| glib::find_program_in_path(p).is_some();
    if has("apt-get") && has("dpkg") {
        Pm::Apt
    } else if has("dnf") {
        Pm::Dnf
    } else if has("zypper") {
        Pm::Zypper
    } else {
        Pm::Unknown
    }
}

/// "Ubuntu", "Fedora Linux"…: /etc/os-release NAME.
pub fn os_name() -> String {
    std::fs::read_to_string("/etc/os-release")
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("NAME=").map(|v| v.trim_matches('"').to_string()))
        })
        .unwrap_or_else(|| "This system".into())
}

/// Whether the file can be installed here, and if not, why.
#[derive(Clone, Debug, PartialEq)]
pub enum Fit {
    Ok,
    /// `made_for` names who it's for ("Fedora and openSUSE"); `why` is the whole explanation.
    No { made_for: String, why: String },
}

/// Everything the wizard shows and the installer needs, read before installing.
pub struct Pkg {
    pub kind: Kind,
    pub path: PathBuf,
    /// Package name, or the AppImage's launcher id ("joplin").
    pub id: String,
    /// What people call it ("Joplin", "Visual Studio Code").
    pub name: String,
    pub version: String,
    pub summary: String,
    /// Bytes: installed size for packages, the file's size for an AppImage.
    pub size: u64,
    /// The app's icon, unpacked into the scratch folder.
    pub icon: Option<PathBuf>,
    /// The version installed now, if any.
    pub installed: Option<String>,
    pub fit: Fit,
    /// The site it was downloaded from (browsers record it in an xattr).
    pub origin: Option<String>,
    /// .deb: how many more packages apt will fetch.
    pub needs: Option<usize>,
    pub appimage: Option<appimage::Facts>,
    /// Where the package was unpacked; removed with the Pkg.
    pub _scratch: Scratch,
}

/// A temporary folder under ~/.cache/kestrel/install, removed when dropped.
pub struct Scratch(pub PathBuf);

impl Scratch {
    pub fn new() -> std::io::Result<Scratch> {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let p = glib::user_cache_dir()
            .join("kestrel/install")
            .join(format!("{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p)?;
        Ok(Scratch(p))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Where an install writes; the user's own folders, or a test's.
#[derive(Clone, Debug)]
pub struct Places {
    /// ~/Applications: AppImages.
    pub bin: PathBuf,
    /// ~/.local/share/applications: launchers.
    pub apps: PathBuf,
    /// ~/.local/share/icons/hicolor: icons, by size.
    pub icons: PathBuf,
    /// ~/Desktop.
    pub desktop: PathBuf,
}

impl Places {
    pub fn user() -> Places {
        let data = glib::user_data_dir();
        Places {
            bin: glib::home_dir().join("Applications"),
            apps: data.join("applications"),
            icons: data.join("icons/hicolor"),
            desktop: glib::user_special_dir(glib::UserDirectory::Desktop).unwrap_or_else(|| glib::home_dir().join("Desktop")),
        }
    }
}

/// The host a browser recorded as this file's source ("github.com"), if any.
pub fn origin(path: &Path) -> Option<String> {
    let info = gio::File::for_path(path)
        .query_info("xattr::xdg.origin.url", gio::FileQueryInfoFlags::NONE, gio::Cancellable::NONE)
        .ok()?;
    let url = info.attribute_as_string("xattr::xdg.origin.url")?;
    let host = glib::Uri::parse(&url, glib::UriFlags::NONE).ok()?.host()?;
    Some(host.trim_start_matches("www.").to_string())
}

// ── launchers and icons inside an unpacked package ──

/// What a launcher (.desktop file) says about its app.
#[derive(Clone, Debug, Default)]
pub struct Launcher {
    /// The file name without .desktop ("appimagekit-joplin", "org.gnome.Calculator").
    pub id: String,
    pub name: String,
    pub comment: String,
    pub icon: String,
    pub categories: String,
    pub mime: String,
    pub wm_class: String,
    pub version: String,
}

pub fn read_launcher(path: &Path) -> Option<Launcher> {
    let k = glib::KeyFile::new();
    k.load_from_file(path, glib::KeyFileFlags::NONE).ok()?;
    let g = |key: &str| k.locale_string("Desktop Entry", key, None).map(|s| s.to_string()).unwrap_or_default();
    let flag = |key: &str| k.boolean("Desktop Entry", key).unwrap_or(false);
    if flag("NoDisplay") || flag("Hidden") || g("Type") != "Application" {
        return None;
    }
    Some(Launcher {
        id: path.file_stem()?.to_string_lossy().into_owned(),
        name: g("Name"),
        comment: g("Comment"),
        icon: g("Icon"),
        categories: g("Categories"),
        mime: g("MimeType"),
        wm_class: g("StartupWMClass"),
        version: g("X-AppImage-Version"),
    })
}

/// The app's launcher in an unpacked tree: one in usr/share/applications first, then any
/// other (/opt apps keep theirs beside the program).
pub fn find_launcher(root: &Path) -> Option<Launcher> {
    let mut found = Vec::new();
    walk(root, 6, &mut |p| {
        if p.extension().is_some_and(|e| e == "desktop") {
            found.push(p.to_path_buf());
        }
    });
    found.sort_by_key(|p| (!p.to_string_lossy().contains("/usr/share/applications/"), p.components().count()));
    found.iter().find_map(|p| read_launcher(p))
}

fn walk(dir: &Path, depth: u32, f: &mut dyn FnMut(&Path)) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let Ok(t) = e.file_type() else { continue };
        if t.is_dir() {
            if depth > 0 {
                walk(&p, depth - 1, f);
            }
        } else {
            f(&p);
        }
    }
}

/// `p` if it is a regular file whose real location is inside `root` (a symlink in a hostile
/// package must not point the installer at /etc/shadow).
pub fn inside(root: &Path, p: &Path) -> Option<PathBuf> {
    let real = p.canonicalize().ok()?;
    let root = root.canonicalize().ok()?;
    (real.starts_with(&root) && real.is_file()).then_some(real)
}

const ICON_EXTS: [&str; 3] = ["svg", "png", "xpm"];

/// Every hicolor size of icon `name` in the tree: ("256x256", file), ("scalable", file)…
pub fn hicolor_icons(root: &Path, name: &str) -> Vec<(String, PathBuf)> {
    let base = root.join("usr/share/icons/hicolor");
    let Ok(rd) = std::fs::read_dir(&base) else { return vec![] };
    let mut out = Vec::new();
    for e in rd.flatten() {
        let size = e.file_name().to_string_lossy().into_owned();
        for ext in ["svg", "png"] {
            if let Some(p) = inside(root, &e.path().join("apps").join(format!("{name}.{ext}"))) {
                out.push((size.clone(), p));
                break;
            }
        }
    }
    out.sort_by_key(|(s, _)| std::cmp::Reverse(size_rank(s)));
    out
}

/// Bigger is better; scalable beats every fixed size.
fn size_rank(dir: &str) -> u32 {
    if dir == "scalable" {
        return u32::MAX;
    }
    dir.split('x').next().and_then(|n| n.split('@').next()?.parse().ok()).unwrap_or(0)
}

/// The best file for icon `name` in an unpacked tree: hicolor (scalable, then the largest),
/// pixmaps, then a file of that name anywhere (/opt apps); an absolute Icon= is looked up
/// inside the tree.
pub fn resolve_icon(root: &Path, name: &str) -> Option<PathBuf> {
    if name.is_empty() {
        return None;
    }
    if name.starts_with('/') {
        return inside(root, &root.join(name.trim_start_matches('/')));
    }
    if let Some((_, p)) = hicolor_icons(root, name).into_iter().next() {
        return Some(p);
    }
    for ext in ICON_EXTS {
        if let Some(p) = inside(root, &root.join(format!("usr/share/pixmaps/{name}.{ext}"))) {
            return Some(p);
        }
    }
    let mut any = None;
    walk(root, 8, &mut |p| {
        let file = p.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
        if any.is_none() && ICON_EXTS.iter().any(|e| file == format!("{name}.{e}")) {
            any = inside(root, p);
        }
    });
    any
}

/// Width of a PNG, from its header.
pub fn png_width(p: &Path) -> Option<u32> {
    use std::io::Read;
    let mut h = [0u8; 24];
    std::fs::File::open(p).ok()?.read_exact(&mut h).ok()?;
    (&h[..8] == b"\x89PNG\r\n\x1a\n").then(|| u32::from_be_bytes([h[16], h[17], h[18], h[19]]))
}

/// The hicolor folder a lone icon file belongs in: "scalable" for SVG, else the largest
/// standard size not bigger than the picture.
pub fn size_dir_for(p: &Path) -> String {
    if p.extension().is_some_and(|e| e == "svg") {
        return "scalable".into();
    }
    let w = png_width(p).unwrap_or(256);
    let s = [16, 22, 24, 32, 48, 64, 96, 128, 256, 512].into_iter().rev().find(|&s| s <= w).unwrap_or(16);
    format!("{s}x{s}")
}

// ── after installing ──

/// Copy a launcher to the Desktop, executable and marked trusted so it runs on double-click
/// without an "Allow Launching" step.
pub fn to_desktop(launcher: &Path, desktop: &Path) -> std::io::Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(desktop)?;
    let dst = desktop.join(launcher.file_name().unwrap_or_default());
    std::fs::copy(launcher, &dst)?;
    std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(0o755))?;
    gio::File::for_path(&dst)
        .set_attribute_string("metadata::trusted", "true", gio::FileQueryInfoFlags::NONE, gio::Cancellable::NONE)
        .map_err(std::io::Error::other)?;
    Ok(dst)
}

/// Start an app from its launcher.
pub fn launch(launcher: &Path) -> Result<(), String> {
    // `gio launch` rather than the gio-unix crate just for DesktopAppInfo
    gio::Subprocess::newv(&["gio".as_ref(), "launch".as_ref(), launcher.as_os_str()], gio::SubprocessFlags::NONE)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// The visible launchers a package installed, from its file list: ones in
/// /usr/share/applications, or (Chrome-style /opt packages, whose postinst copies them) the
/// copy there of one it ships elsewhere.
pub fn installed_launchers(files: &str) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for l in files.lines().filter(|l| l.ends_with(".desktop")) {
        let p = Path::new(l);
        let p = if l.starts_with("/usr/share/applications/") {
            p.to_path_buf()
        } else {
            Path::new("/usr/share/applications").join(p.file_name().unwrap_or_default())
        };
        if !out.contains(&p) && p.is_file() && read_launcher(&p).is_some() {
            out.push(p);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_and_sizes() {
        assert_eq!(kind_of("Joplin-3.7.22.AppImage"), Some(Kind::AppImage));
        assert_eq!(kind_of("X.DEB"), Some(Kind::Deb));
        assert_eq!(kind_of("kestrel-0.9.40-1.x86_64.rpm"), Some(Kind::Rpm));
        assert_eq!(kind_of("foo-1.src.rpm"), None);
        assert_eq!(kind_of("notes.txt"), None);
        assert_eq!(size_rank("scalable"), u32::MAX);
        assert_eq!(size_rank("256x256@2"), 256);
        assert!(size_rank("512x512") > size_rank("48x48"));
    }

    #[test]
    fn finds_launcher_and_icon_in_a_tree() {
        let s = Scratch::new().unwrap();
        let r = &s.0;
        let w = |p: &str, body: &[u8]| {
            let p = r.join(p);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        };
        w("usr/share/applications/hidden.desktop", b"[Desktop Entry]\nType=Application\nName=Helper\nNoDisplay=true\n");
        w("usr/share/applications/org.ex.App.desktop", b"[Desktop Entry]\nType=Application\nName=Example\nIcon=ex\nComment=Does things\n");
        let png = |w: u32| {
            let mut v = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
            v.extend(w.to_be_bytes());
            v.extend(w.to_be_bytes());
            v
        };
        w("usr/share/icons/hicolor/48x48/apps/ex.png", &png(48));
        w("usr/share/icons/hicolor/256x256/apps/ex.png", &png(256));
        let l = find_launcher(r).unwrap();
        assert_eq!((l.name.as_str(), l.icon.as_str(), l.id.as_str()), ("Example", "ex", "org.ex.App"));
        let icons = hicolor_icons(r, "ex");
        assert_eq!(icons.iter().map(|(s, _)| s.as_str()).collect::<Vec<_>>(), ["256x256", "48x48"]);
        assert!(resolve_icon(r, "ex").unwrap().ends_with("256x256/apps/ex.png"));
        assert_eq!(size_dir_for(&icons[0].1), "256x256");
        // a symlink out of the tree is not an icon
        std::os::unix::fs::symlink("/etc/hostname", r.join("usr/share/pixmaps-evil")).unwrap();
        std::fs::create_dir_all(r.join("usr/share/pixmaps")).unwrap();
        std::os::unix::fs::symlink("/etc/hostname", r.join("usr/share/pixmaps/bad.png")).unwrap();
        assert_eq!(resolve_icon(r, "bad"), None);
        assert_eq!(resolve_icon(r, "/etc/hostname"), None);
    }
}
