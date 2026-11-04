//! AppImages: read without running them (the squashfs after the runtime, unpacked in the
//! archive sandbox), installed for you alone with no password: the file goes to
//! ~/Applications, its icons into ~/.local/share/icons/hicolor, and a launcher into
//! ~/.local/share/applications that the dock matches to the running window.

use super::{Fit, Kind, Launcher, Pkg, Places, Scratch};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

/// What the installer needs to know beyond what the wizard shows.
#[derive(Clone, Debug, Default)]
pub struct Facts {
    /// The app's own launcher, from inside the AppImage.
    pub launcher: Launcher,
    /// Launcher id we install under ("joplin"): the app's own, minus electron-builder's
    /// "appimagekit-", so the window's app id (taken from it) matches.
    pub slug: String,
    /// Icon files by hicolor size folder ("256x256", "scalable"), largest first.
    pub icons: Vec<(String, PathBuf)>,
    /// An Electron app: its sandbox needs user namespaces.
    pub electron: bool,
    /// Ubuntu blocks the namespaces and the AppImage doesn't work around it itself: we start
    /// it with --no-sandbox.
    pub no_sandbox: bool,
    /// FUSE can't mount it here: start it with APPIMAGE_EXTRACT_AND_RUN=1.
    pub extract_and_run: bool,
    /// Where an earlier install put it, if any.
    pub installed_at: Option<PathBuf>,
}

/// Where the squashfs starts, and the processor the runtime is for: (offset, e_machine).
fn header(path: &Path) -> Option<(u64, u16)> {
    let mut h = [0u8; 64];
    std::fs::File::open(path).ok()?.read_exact(&mut h).ok()?;
    if &h[..4] != b"\x7fELF" || h[4] != 2 || &h[8..11] != b"AI\x02" {
        return None; // not a 64-bit type-2 AppImage
    }
    let u16_at = |o: usize| u16::from_le_bytes([h[o], h[o + 1]]);
    let shoff = u64::from_le_bytes(h[0x28..0x30].try_into().ok()?);
    let offset = shoff + u64::from(u16_at(0x3A)) * u64::from(u16_at(0x3C));
    let mut f = std::fs::File::open(path).ok()?;
    std::io::Seek::seek(&mut f, std::io::SeekFrom::Start(offset)).ok()?;
    let mut magic = [0u8; 4];
    f.read_exact(&mut magic).ok()?;
    (&magic == b"hsqs").then_some((offset, u16_at(0x12)))
}

fn machine_name(m: u16) -> &'static str {
    match m {
        0x3E => "x86_64",
        0xB7 => "aarch64",
        0x03 => "x86",
        0x28 => "arm",
        _ => "?",
    }
}

/// Unpack the parts we read into `dir/r`: launchers, icons, AppRun, chrome-sandbox. With
/// unsquashfs nothing in the AppImage runs; without it, its own runtime extracts, inside the
/// sandbox.
fn unpack(path: &Path, offset: u64, dir: &Path) -> Option<(PathBuf, Vec<String>)> {
    let root = dir.join("r");
    if gtk::glib::find_program_in_path("unsquashfs").is_some() {
        let off = offset.to_string();
        let list = crate::archive::sandboxed("unsquashfs", path, dir)
            .args(["-o", &off, "-lc", "-d", "r"])
            .arg(path)
            .output()
            .ok()?;
        let files: Vec<String> = String::from_utf8_lossy(&list.stdout)
            .lines()
            .filter_map(|l| l.strip_prefix("r/").map(str::to_string))
            .collect();
        let wanted: Vec<&String> = files
            .iter()
            .filter(|f| {
                let top = !f.contains('/');
                (top && (f.ends_with(".desktop") || f.ends_with(".png") || f.ends_with(".svg") || *f == ".DirIcon" || *f == "AppRun"))
                    || f.starts_with("usr/share/icons/hicolor/") && f.contains("/apps/")
            })
            .collect();
        if wanted.is_empty() {
            std::fs::create_dir_all(&root).ok()?;
            return Some((root, files));
        }
        let mut c = crate::archive::sandboxed("unsquashfs", path, dir);
        c.args(["-o", &off, "-f", "-q", "-n", "-no-xattrs", "-follow-symlinks", "-no-wildcards", "-d"]).arg(&root).arg(path);
        c.args(wanted);
        let _ = c.output();
        return root.is_dir().then_some((root, files));
    }
    // the runtime's own extractor: it must be executable, and runs sandboxed in `dir`
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755));
    for pat in ["*.desktop", ".DirIcon", "AppRun", "chrome-sandbox", "*.png", "*.svg", "usr/share/icons/hicolor/*"] {
        let _ = crate::archive::sandboxed(&path.to_string_lossy(), path, dir)
            .current_dir(dir)
            .args(["--appimage-extract", pat])
            .output();
    }
    std::fs::rename(dir.join("squashfs-root"), &root).ok()?;
    let mut files = Vec::new();
    if root.join("chrome-sandbox").exists() {
        files.push("chrome-sandbox".to_string());
    }
    Some((root, files))
}

/// Whether unprivileged user namespaces work here (Ubuntu 24.04+ blocks them for apps without
/// an AppArmor profile, which Electron's sandbox needs).
fn userns_ok() -> bool {
    match Command::new("unshare").args(["-Ur", "true"]).output() {
        Ok(o) => o.status.success(),
        Err(_) => std::fs::read_to_string("/proc/sys/kernel/apparmor_restrict_unprivileged_userns").map_or(true, |s| s.trim() != "1"),
    }
}

/// Whether the AppImage's runtime can mount it: older runtimes load libfuse.so.2, newer
/// static ones need the fusermount3 helper.
fn fuse_ok(path: &Path, offset: u64) -> bool {
    if !Path::new("/dev/fuse").exists() {
        return false;
    }
    let mut rt = Vec::new();
    let _ = std::fs::File::open(path).map(|f| f.take(offset.min(4 << 20)).read_to_end(&mut rt));
    if rt.windows(12).any(|w| w == b"libfuse.so.2") {
        ["/usr/lib/x86_64-linux-gnu/libfuse.so.2", "/usr/lib/aarch64-linux-gnu/libfuse.so.2", "/usr/lib64/libfuse.so.2", "/usr/lib/libfuse.so.2", "/lib64/libfuse.so.2"]
            .iter()
            .any(|p| Path::new(p).exists())
    } else {
        ["fusermount3", "fusermount"].iter().any(|p| gtk::glib::find_program_in_path(p).is_some())
    }
}

/// "Joplin-3.7.22.AppImage" → "3.7.22"; "Krita_5.2.6-x86_64.AppImage" → "5.2.6".
fn version_from_name(file: &str) -> String {
    let stem = file.rsplit_once('.').map_or(file, |(s, _)| s);
    stem.split(['-', '_', ' '])
        .map(|t| t.strip_prefix(['v', 'V']).unwrap_or(t))
        .find(|t| t.starts_with(|c: char| c.is_ascii_digit()) && t.contains('.'))
        .map(str::to_string)
        .unwrap_or_default()
}

/// A launcher id from a name: lowercase letters, digits and dashes.
fn slugify(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || c == '.' {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

/// The launcher Kestrel installed for `slug`, if any: (file, version, AppImage path).
pub fn installed(places: &Places, slug: &str) -> Option<(PathBuf, String, PathBuf)> {
    let f = places.apps.join(format!("{slug}.desktop"));
    let k = gtk::glib::KeyFile::new();
    k.load_from_file(&f, gtk::glib::KeyFileFlags::NONE).ok()?;
    let at = k.string("Desktop Entry", "X-Kestrel-AppImage").ok()?;
    let v = k.string("Desktop Entry", "X-AppImage-Version").map(|s| s.to_string()).unwrap_or_default();
    Some((f, v, PathBuf::from(at.as_str())))
}

pub(super) fn read(path: &Path, places: &Places) -> Option<Pkg> {
    let (offset, mach) = header(path)?;
    let scratch = Scratch::new().ok()?;
    let (root, files) = unpack(path, offset, &scratch.0)?;
    let mut launcher = std::fs::read_dir(&root)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "desktop"))
        .find_map(|p| super::read_launcher(&p))
        .unwrap_or_default();
    let file = path.file_name()?.to_string_lossy().into_owned();
    if launcher.name.is_empty() {
        launcher.name = file.split(['-', '_', '.']).next().unwrap_or(&file).to_string();
    }
    let base = launcher.id.strip_prefix("appimagekit-").unwrap_or(&launcher.id);
    let mut slug = slugify(if base.is_empty() { &launcher.name } else { base });
    if slug.is_empty() {
        slug = "appimage".into();
    }
    let mut icons = super::hicolor_icons(&root, &launcher.icon);
    if icons.is_empty() {
        // no hicolor icons: one beside the launcher, or .DirIcon
        let lone = ["svg", "png"]
            .iter()
            .find_map(|e| super::inside(&root, &root.join(format!("{}.{e}", launcher.icon))))
            .or_else(|| super::inside(&root, &root.join(".DirIcon")));
        if let Some(p) = lone {
            icons.push((super::size_dir_for(&p), p));
        }
    }
    let electron = files.iter().any(|f| f == "chrome-sandbox" || f == "resources/app.asar");
    let mut apprun = Vec::new();
    let _ = std::fs::File::open(root.join("AppRun")).map(|f| f.take(1 << 20).read_to_end(&mut apprun));
    let handles_sandbox = apprun.windows(10).any(|w| w == b"no-sandbox");
    let prior = installed(places, &slug);
    let here = std::env::consts::ARCH;
    let fit = if machine_name(mach) == here {
        Fit::Ok
    } else {
        Fit::No {
            made_for: format!("{} computers", machine_name(mach)),
            why: format!(
                "This AppImage is made for {} computers, and this one is {here}. Look for the {here} AppImage of {} on the same download page.",
                machine_name(mach),
                launcher.name
            ),
        }
    };
    let version = if launcher.version.is_empty() {
        version_from_name(&file)
    } else {
        launcher.version.clone()
    };
    Some(Pkg {
        kind: Kind::AppImage,
        path: path.to_path_buf(),
        id: slug.clone(),
        name: launcher.name.clone(),
        version,
        summary: launcher.comment.clone(),
        size: std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
        icon: icons.first().map(|(_, p)| p.clone()),
        installed: prior.as_ref().map(|(_, v, _)| v.clone()),
        fit,
        origin: super::origin(path),
        needs: None,
        appimage: Some(Facts {
            slug,
            icons,
            electron,
            no_sandbox: electron && !handles_sandbox && !userns_ok(),
            extract_and_run: !fuse_ok(path, offset),
            installed_at: prior.map(|(_, _, p)| p),
            launcher,
        }),
        _scratch: scratch,
    })
}

/// The file name it gets in ~/Applications: "Joplin.AppImage", no version, so an update
/// replaces it in place.
pub fn file_name(name: &str) -> String {
    let clean: String = name
        .chars()
        .map(|c| if c.is_alphanumeric() || "._-+".contains(c) { c } else { '-' })
        .collect();
    let clean = clean.trim_matches(['-', '.']);
    format!("{}.AppImage", if clean.is_empty() { "App" } else { clean })
}

/// Step 1: put the file in ~/Applications (moved, or copied to keep the download), executable.
pub fn place(p: &Pkg, keep_download: bool, places: &Places) -> std::io::Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(&places.bin)?;
    let dest = places.bin.join(file_name(&p.name));
    let same = p.path.canonicalize().ok() == dest.canonicalize().ok();
    if !same {
        let tmp = places.bin.join(format!(".{}.part", file_name(&p.name)));
        let moved = !keep_download && std::fs::rename(&p.path, &dest).is_ok();
        if !moved {
            std::fs::copy(&p.path, &tmp)?;
            std::fs::rename(&tmp, &dest)?;
            if !keep_download {
                let _ = std::fs::remove_file(&p.path);
            }
        }
    }
    std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755))?;
    // an older version somewhere else goes
    if let Some(old) = p.appimage.as_ref().and_then(|f| f.installed_at.clone())
        && old != dest
        && old.canonicalize().ok() != p.path.canonicalize().ok()
    {
        let _ = std::fs::remove_file(old);
    }
    Ok(dest)
}

/// Quote one Exec= argument the way the desktop-entry spec wants.
fn exec_arg(s: &str) -> String {
    if s.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+=:".contains(c)) {
        return s.into();
    }
    let mut out = String::from("\"");
    for c in s.chars() {
        if "\"`$\\".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// The command line the launcher runs.
pub fn exec_line(f: &Facts, dest: &Path) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut env = Vec::new();
    if f.electron {
        // Electron names its window after CHROME_DESKTOP: the dock then matches it to this launcher
        env.push(format!("CHROME_DESKTOP={}.desktop", f.slug));
    }
    if f.extract_and_run {
        env.push("APPIMAGE_EXTRACT_AND_RUN=1".into());
    }
    if !env.is_empty() {
        parts.push("env".into());
        parts.extend(env);
    }
    parts.push(exec_arg(&dest.to_string_lossy()));
    if f.no_sandbox {
        parts.push("--no-sandbox".into());
    }
    parts.push("%U".into());
    parts.join(" ")
}

/// Step 2: the icons (every size) and the launcher. Returns the launcher.
pub fn add_launcher(p: &Pkg, dest: &Path, places: &Places) -> std::io::Result<PathBuf> {
    let f = p.appimage.as_ref().ok_or_else(|| std::io::Error::other("not an AppImage"))?;
    let icon_name = format!("kestrel-{}", f.slug);
    remove_icons(places, &icon_name);
    for (size, file) in &f.icons {
        let ext = file.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_else(|| "png".into());
        let size = if size_known(size) { size.clone() } else { super::size_dir_for(file) };
        let dir = places.icons.join(&size).join("apps");
        std::fs::create_dir_all(&dir)?;
        std::fs::copy(file, dir.join(format!("{icon_name}.{ext}")))?;
    }
    refresh_icons(places);

    let k = gtk::glib::KeyFile::new();
    let s = |key: &str, v: &str| {
        if !v.is_empty() {
            k.set_string("Desktop Entry", key, v);
        }
    };
    s("Type", "Application");
    s("Name", &p.name);
    s("Comment", &p.summary);
    s("Exec", &exec_line(f, dest));
    s("TryExec", &dest.to_string_lossy());
    s("Icon", if f.icons.is_empty() { "application-x-executable" } else { &icon_name });
    s("Terminal", "false");
    s("Categories", &f.launcher.categories);
    s("MimeType", &f.launcher.mime);
    let wm = if f.electron || f.launcher.wm_class.is_empty() || f.launcher.wm_class.starts_with("appimagekit") {
        f.slug.clone()
    } else {
        f.launcher.wm_class.clone()
    };
    s("StartupWMClass", &wm);
    s("X-AppImage-Version", &p.version);
    s("X-Kestrel-AppImage", &dest.to_string_lossy());
    std::fs::create_dir_all(&places.apps)?;
    let file = places.apps.join(format!("{}.desktop", f.slug));
    k.save_to_file(&file).map_err(std::io::Error::other)?;
    if !f.launcher.mime.is_empty() {
        let _ = Command::new("update-desktop-database").arg(&places.apps).output();
    }
    Ok(file)
}

/// Sizes hicolor's index.theme lists (an icon elsewhere is never found).
fn size_known(dir: &str) -> bool {
    dir == "scalable" || ["16", "22", "24", "32", "48", "64", "96", "128", "256", "512"].iter().any(|s| dir == format!("{s}x{s}"))
}

fn remove_icons(places: &Places, icon_name: &str) {
    let Ok(rd) = std::fs::read_dir(&places.icons) else { return };
    for e in rd.flatten() {
        for ext in ["png", "svg", "xpm"] {
            let _ = std::fs::remove_file(e.path().join("apps").join(format!("{icon_name}.{ext}")));
        }
    }
}

/// A stale icon-theme.cache hides new icons (GTK trusts it while it's newer than the folder):
/// rebuild it if there is one, and touch the folder so watchers look again.
fn refresh_icons(places: &Places) {
    if places.icons.join("icon-theme.cache").exists() {
        let _ = Command::new("gtk-update-icon-cache").args(["-q", "-t", "-f"]).arg(&places.icons).output();
    }
    let touch = places.icons.join(".kestrel-touch");
    let _ = std::fs::write(&touch, b"");
    let _ = std::fs::remove_file(&touch);
}

/// Take an installed AppImage away: the file, its launcher (and the Desktop copy), its icons.
pub fn uninstall(slug: &str, places: &Places) -> std::io::Result<()> {
    let (launcher, _, at) = installed(places, slug).ok_or_else(|| std::io::Error::other("it isn't installed by Kestrel"))?;
    let _ = std::fs::remove_file(&at);
    std::fs::remove_file(&launcher)?;
    let on_desktop = places.desktop.join(launcher.file_name().unwrap_or_default());
    if installed_at_desktop(&on_desktop) {
        let _ = std::fs::remove_file(on_desktop);
    }
    remove_icons(places, &format!("kestrel-{slug}"));
    refresh_icons(places);
    Ok(())
}

fn installed_at_desktop(p: &Path) -> bool {
    std::fs::read_to_string(p).is_ok_and(|s| s.contains("X-Kestrel-AppImage="))
}

/// Run it once without installing (it becomes executable).
pub fn run_once(p: &Pkg) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&p.path, std::fs::Permissions::from_mode(0o755))?;
    let mut c = Command::new(&p.path);
    if let Some(f) = &p.appimage {
        if f.extract_and_run {
            c.env("APPIMAGE_EXTRACT_AND_RUN", "1");
        }
        if f.no_sandbox {
            c.arg("--no-sandbox");
        }
    }
    c.spawn().map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_exec() {
        assert_eq!(version_from_name("Joplin-3.7.22.AppImage"), "3.7.22");
        assert_eq!(version_from_name("Krita_5.2.6-x86_64.AppImage"), "5.2.6");
        assert_eq!(version_from_name("Obsidian-v1.9.10.AppImage"), "1.9.10");
        assert_eq!(version_from_name("App.AppImage"), "");
        assert_eq!(slugify("Visual Studio Code"), "visual-studio-code");
        assert_eq!(slugify("org.kde.krita"), "org.kde.krita");
        assert_eq!(file_name("Visual Studio Code"), "Visual-Studio-Code.AppImage");
        assert_eq!(file_name("Joplin"), "Joplin.AppImage");
        assert_eq!(exec_arg("/home/a/Applications/My App.AppImage"), "\"/home/a/Applications/My App.AppImage\"");
        assert_eq!(exec_arg("/a/$x\"y"), "\"/a/\\$x\\\"y\"");
        let f = Facts {
            slug: "joplin".into(),
            electron: true,
            no_sandbox: true,
            ..Default::default()
        };
        assert_eq!(
            exec_line(&f, Path::new("/h/Applications/Joplin.AppImage")),
            "env CHROME_DESKTOP=joplin.desktop /h/Applications/Joplin.AppImage --no-sandbox %U"
        );
    }

    /// Build an AppImage-shaped file: a minimal ELF header saying "type-2 AppImage" whose
    /// sections end at 64, then a squashfs there. It can't run; it doesn't need to.
    pub(crate) fn fake_appimage(dir: &Path, desktop: &str, icon_png: Option<&[u8]>, electron: bool) -> Option<PathBuf> {
        if gtk::glib::find_program_in_path("mksquashfs").is_none()
            || gtk::glib::find_program_in_path("unsquashfs").is_none()
            || gtk::glib::find_program_in_path("bwrap").is_none()
        {
            return None;
        }
        let tree = dir.join("appdir");
        std::fs::create_dir_all(tree.join("usr/share/icons/hicolor/48x48/apps")).unwrap();
        std::fs::write(tree.join("ft.desktop"), desktop).unwrap();
        std::fs::write(tree.join("AppRun"), "#!/bin/sh\necho hi\n").unwrap();
        if let Some(png) = icon_png {
            std::fs::write(tree.join("usr/share/icons/hicolor/48x48/apps/ftapp.png"), png).unwrap();
        }
        if electron {
            std::fs::write(tree.join("chrome-sandbox"), "x").unwrap();
        }
        let sq = dir.join("fs.sqfs");
        let ok = Command::new("mksquashfs").arg(&tree).arg(&sq).args(["-noappend", "-quiet", "-no-progress"]).output().ok()?.status.success();
        assert!(ok);
        let mut img = vec![0u8; 64];
        img[..4].copy_from_slice(b"\x7fELF");
        img[4] = 2;
        img[5] = 1;
        img[8..11].copy_from_slice(b"AI\x02");
        let m: u16 = match std::env::consts::ARCH {
            "aarch64" => 0xB7,
            _ => 0x3E,
        };
        img[0x12..0x14].copy_from_slice(&m.to_le_bytes());
        img[0x28..0x30].copy_from_slice(&64u64.to_le_bytes()); // e_shoff
        img[0x3A..0x3C].copy_from_slice(&64u16.to_le_bytes()); // e_shentsize, e_shnum 0
        img.extend(std::fs::read(&sq).unwrap());
        let out = dir.join("FT_App-2.1.0-x86_64.AppImage");
        std::fs::write(&out, img).unwrap();
        Some(out)
    }

    #[test]
    fn reads_installs_updates_and_uninstalls() {
        let s = Scratch::new().unwrap();
        let png = {
            let mut v = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
            v.extend(48u32.to_be_bytes());
            v.extend(48u32.to_be_bytes());
            v
        };
        let desktop = "[Desktop Entry]\nType=Application\nName=FT App\nComment=A test app\nIcon=ftapp\nExec=AppRun %U\nCategories=Utility;\nStartupWMClass=appimagekit-ft\n";
        let Some(img) = fake_appimage(&s.0, desktop, Some(&png), true) else { return };
        let places = Places {
            bin: s.0.join("home/Applications"),
            apps: s.0.join("home/apps"),
            icons: s.0.join("home/icons/hicolor"),
            desktop: s.0.join("home/Desktop"),
        };
        let p = read(&img, &places).expect("read");
        assert_eq!((p.name.as_str(), p.version.as_str(), p.id.as_str(), p.summary.as_str()), ("FT App", "2.1.0", "ft", "A test app"));
        assert_eq!(p.fit, Fit::Ok);
        assert_eq!(p.installed, None);
        let f = p.appimage.as_ref().unwrap();
        assert!(f.electron);
        assert_eq!(f.icons.len(), 1);
        assert!(p.icon.as_ref().unwrap().ends_with("48x48/apps/ftapp.png"));

        let dest = place(&p, false, &places).unwrap();
        assert_eq!(dest, places.bin.join("FT-App.AppImage"));
        assert!(!img.exists(), "the download was moved");
        let launcher = add_launcher(&p, &dest, &places).unwrap();
        assert_eq!(launcher, places.apps.join("ft.desktop"));
        let text = std::fs::read_to_string(&launcher).unwrap();
        assert!(text.contains("Icon=kestrel-ft\n") && text.contains("StartupWMClass=ft\n") && text.contains("X-AppImage-Version=2.1.0"), "{text}");
        assert!(text.contains(&format!("Exec=env CHROME_DESKTOP=ft.desktop {}", dest.display())), "{text}");
        assert!(places.icons.join("48x48/apps/kestrel-ft.png").is_file());
        let icon = to_desktop_ok(&launcher, &places);
        assert!(icon.is_file());

        // the same app again, newer: an update of what's there
        let s2 = Scratch::new().unwrap();
        let img2 = fake_appimage(&s2.0, &desktop.replace("Name=FT App", "Name=FT App\nX-AppImage-Version=2.2.0"), Some(&png), true).unwrap();
        let p2 = read(&img2, &places).unwrap();
        assert_eq!((p2.version.as_str(), p2.installed.as_deref()), ("2.2.0", Some("2.1.0")));
        let d2 = place(&p2, true, &places).unwrap();
        assert_eq!(d2, dest);
        assert!(img2.exists(), "kept the download");

        uninstall("ft", &places).unwrap();
        assert!(!dest.exists() && !launcher.exists() && !icon.exists());
        assert!(!places.icons.join("48x48/apps/kestrel-ft.png").exists());
    }

    fn to_desktop_ok(launcher: &Path, places: &Places) -> PathBuf {
        // metadata::trusted needs gvfsd-metadata; tests may lack it, so fall back to a plain copy
        super::super::to_desktop(launcher, &places.desktop).unwrap_or_else(|_| places.desktop.join(launcher.file_name().unwrap()))
    }

    #[test]
    fn not_an_appimage() {
        let s = Scratch::new().unwrap();
        let f = s.0.join("x.AppImage");
        std::fs::write(&f, b"#!/bin/sh\n").unwrap();
        assert!(header(&f).is_none());
    }
}
