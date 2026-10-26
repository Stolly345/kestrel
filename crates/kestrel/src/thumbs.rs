//! Freedesktop thumbnails: run the system's thumbnailers (/usr/share/thumbnailers) and store
//! results in the shared ~/.cache/thumbnails, so Nautilus and Kestrel reuse each other's work.

use gtk::prelude::*;
use gtk::{gdk_pixbuf, gio, glib};
use std::cell::RefCell;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

struct Thumbnailer {
    exec: String,
    mimes: Vec<String>,
}

fn thumbnailers() -> &'static [Thumbnailer] {
    static T: OnceLock<Vec<Thumbnailer>> = OnceLock::new();
    T.get_or_init(|| {
        let mut dirs: Vec<PathBuf> = glib::system_data_dirs().into_iter().map(|d| d.join("thumbnailers")).collect();
        dirs.insert(0, glib::user_data_dir().join("thumbnailers"));
        let mut out = Vec::new();
        for d in dirs {
            let Ok(rd) = std::fs::read_dir(d) else { continue };
            for e in rd.flatten() {
                let kf = glib::KeyFile::new();
                if kf.load_from_file(e.path(), glib::KeyFileFlags::NONE).is_err() {
                    continue;
                }
                let (Ok(exec), Ok(mimes)) = (kf.string("Thumbnailer Entry", "Exec"), kf.string_list("Thumbnailer Entry", "MimeType")) else {
                    continue;
                };
                out.push(Thumbnailer {
                    exec: exec.into(),
                    mimes: mimes.iter().map(|m| m.to_string()).collect(),
                });
            }
        }
        out
    })
}

fn find(ct: &str) -> Option<&'static Thumbnailer> {
    let all = thumbnailers();
    all.iter()
        .find(|t| t.mimes.iter().any(|m| m == ct))
        .or_else(|| all.iter().find(|t| t.mimes.iter().any(|m| gio::content_type_is_a(ct, m))))
}

/// Zip-based project files that carry their own preview picture, and where it lives (tried in
/// order). Read by 7-Zip in the archive sandbox, so no system thumbnailer is needed.
const EMBEDDED: &[(&str, &[&str])] = &[
    (".kra", &["mergedimage.png", "preview.png"]),
    (".ora", &["mergedimage.png", "Thumbnails/thumbnail.png"]),
    (".xmind", &["Thumbnails/thumbnail.png"]),
    (".sketch", &["previews/preview.png"]),
    (".3mf", &["Metadata/thumbnail.png", "Metadata/plate_1.png"]),
    (".mscz", &["Thumbnails/thumbnail.png"]),
    (".procreate", &["QuickLook/Thumbnail.png"]),
    (".pages", &["preview.jpg", "QuickLook/Thumbnail.jpg"]),
    (".numbers", &["preview.jpg", "QuickLook/Thumbnail.jpg"]),
    (".key", &["preview.jpg", "QuickLook/Thumbnail.jpg"]),
    (".fig", &["thumbnail.png"]),
    (".odt", &["Thumbnails/thumbnail.png"]),
    (".ods", &["Thumbnails/thumbnail.png"]),
    (".odp", &["Thumbnails/thumbnail.png"]),
    (".odg", &["Thumbnails/thumbnail.png"]),
    (".docx", &["docProps/thumbnail.jpeg", "docProps/thumbnail.png"]),
    (".xlsx", &["docProps/thumbnail.jpeg", "docProps/thumbnail.png"]),
    (".pptx", &["docProps/thumbnail.jpeg", "docProps/thumbnail.png"]),
];

fn embedded(name: &str) -> Option<&'static [&'static str]> {
    let lower = name.to_lowercase();
    EMBEDDED.iter().find(|(e, _)| lower.ends_with(e)).map(|(_, m)| *m)
}

pub fn can_thumbnail(info: &gio::FileInfo) -> bool {
    embedded(&info.name().to_string_lossy()).is_some() || crate::browser::content_type(info).is_some_and(|ct| find(&ct).is_some())
}

/// Wait for `child`, killing it after 10 s: broken files can hang thumbnailers (and 7-Zip).
fn finish(mut child: std::process::Child) -> bool {
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(s)) => return s.success(),
            Ok(None) if start.elapsed() < Duration::from_secs(10) => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

/// The preview picture stored inside a project file, scaled to `px`.
fn from_embedded(path: &std::path::Path, members: &[&str], px: u32) -> Option<gdk_pixbuf::Pixbuf> {
    let scratch = glib::user_cache_dir()
        .join("kestrel/thumbs")
        .join(format!("{}-{}", std::process::id(), glib::random_int()));
    std::fs::create_dir_all(&scratch).ok()?;
    let path = path.canonicalize().ok()?;
    let pb = members.iter().find_map(|m| {
        let mut c = crate::archive::sandboxed(&crate::archive::seven_zip(), &path, &scratch);
        c.args(["e", "-y", "-bso0", "-bsp0", "-p"])
            .arg(format!("-o{}", scratch.display()))
            .arg("--")
            .arg(&path)
            .arg(m);
        let child = c.stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn().ok()?;
        finish(child).then_some(())?;
        let f = scratch.join(m.rsplit('/').next()?);
        gdk_pixbuf::Pixbuf::from_file_at_scale(&f, px as i32, px as i32, true).ok()
    });
    let _ = std::fs::remove_dir_all(&scratch);
    pb
}

#[derive(Clone, Copy)]
pub enum Size {
    Large,
    XLarge,
}

impl Size {
    fn px(self) -> u32 {
        match self {
            Size::Large => 256,
            Size::XLarge => 512,
        }
    }
    fn dir(self) -> &'static str {
        match self {
            Size::Large => "large",
            Size::XLarge => "x-large",
        }
    }
}

fn cache_path(uri: &str, size: Size) -> PathBuf {
    let md5 = glib::compute_checksum_for_string(glib::ChecksumType::Md5, uri).unwrap();
    glib::user_cache_dir().join("thumbnails").join(size.dir()).join(format!("{md5}.png"))
}

/// Valid per the spec: exists and records the file's current mtime.
fn cached(uri: &str, mtime: i64, size: Size) -> Option<PathBuf> {
    let p = cache_path(uri, size);
    let pb = gdk_pixbuf::Pixbuf::from_file(&p).ok()?;
    (pb.option("tEXt::Thumb::MTime").and_then(|m| m.parse::<i64>().ok()) == Some(mtime)).then_some(p)
}

/// Run the thumbnailer (on a worker thread).
fn generate(file: &gio::File, ct: &str, mtime: i64, size: Size) -> Option<PathBuf> {
    let uri = file.uri().to_string();
    // the file's own preview picture first; most Office files have none, so a thumbnailer may still
    let pb = file
        .path()
        .zip(file.basename().and_then(|n| embedded(&n.to_string_lossy())))
        .and_then(|(p, members)| from_embedded(&p, members, size.px()))
        .or_else(|| run_thumbnailer(file, find(ct)?, size))?;
    let dest = cache_path(&uri, size);
    std::fs::create_dir_all(dest.parent()?).ok()?;
    let tmp = dest.with_extension(format!("{}.tmp", std::process::id()));
    pb.savev(&tmp, "png", &[("tEXt::Thumb::URI", uri.as_str()), ("tEXt::Thumb::MTime", &mtime.to_string())])
        .ok()?;
    std::fs::rename(&tmp, &dest).ok()?;
    Some(dest)
}

/// A freedesktop thumbnailer's picture of `file`.
fn run_thumbnailer(file: &gio::File, t: &Thumbnailer, size: Size) -> Option<gdk_pixbuf::Pixbuf> {
    let uri = file.uri().to_string();
    // named like GNOME's own scratch files: Ubuntu's AppArmor profile for the PDF thumbnailer
    // (papers) only lets it write /tmp/gnome-desktop-*, so anywhere else every PDF came out
    // blank. file_open_tmp creates it exclusively (mode 600), so the name can't be pre-planted.
    let (fd, out) = glib::file_open_tmp(Some("gnome-desktop-kestrel-XXXXXX.png")).ok()?;
    drop(fd);
    let path = file.path().map(|p| p.display().to_string()).unwrap_or_default();
    let argv: Vec<String> = glib::shell_parse_argv(&t.exec)
        .ok()?
        .iter()
        .map(|a| {
            a.to_string_lossy()
                .replace("%s", &size.px().to_string())
                .replace("%u", &uri)
                .replace("%i", &path)
                .replace("%o", &out.display().to_string())
                .replace("%%", "%")
        })
        .collect();
    let child = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let ok = finish(child);
    let pb = ok.then(|| gdk_pixbuf::Pixbuf::from_file(&out).ok()).flatten();
    let _ = std::fs::remove_file(&out);
    pb
}

thread_local! {
    static FAILED: RefCell<HashSet<String>> = RefCell::default();
    static RUNNING: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Thumbnail path for `info`, generating it if needed. `None` = use the icon.
/// At most 4 thumbnailers run at once; queued requests give up once `wanted()` turns false
/// (the tile scrolled away and was rebound).
pub async fn get(info: &gio::FileInfo, size: Size, wanted: impl Fn() -> bool) -> Option<PathBuf> {
    let file = crate::browser::info_file(info)?;
    let ct = crate::browser::content_type(info)?.to_string();
    let uri = file.uri().to_string();
    let mtime = info.modification_date_time().map_or(0, |d| d.to_unix());
    if FAILED.with_borrow(|f| f.contains(&uri)) || !can_thumbnail(info) {
        return None;
    }
    let (u, m) = (uri.clone(), mtime);
    if let Ok(Some(p)) = gio::spawn_blocking(move || cached(&u, m, size)).await {
        return Some(p);
    }
    while RUNNING.get() >= 4 {
        if !wanted() {
            return None;
        }
        glib::timeout_future(Duration::from_millis(50)).await;
    }
    RUNNING.set(RUNNING.get() + 1);
    let r = gio::spawn_blocking(move || generate(&file, &ct, mtime, size)).await.ok().flatten();
    RUNNING.set(RUNNING.get() - 1);
    if r.is_none() {
        FAILED.with_borrow_mut(|f| f.insert(uri));
    }
    r
}

/// Fill `img` with a thumbnail once ready, unless it has been rebound to another file meanwhile.
pub fn load_into(img: &gtk::Image, info: &gio::FileInfo) {
    if info.boolean("thumbnail::is-valid") || !can_thumbnail(info) {
        return;
    }
    let Some(uri) = crate::browser::info_file(info).map(|f| f.uri()) else {
        return;
    };
    img.set_widget_name(&uri);
    let (img, info) = (img.clone(), info.clone());
    glib::spawn_future_local(async move {
        let wanted = || img.widget_name() == uri;
        if let Some(p) = get(&info, Size::Large, wanted).await
            && wanted()
        {
            img.set_from_file(Some(&p));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_preview() {
        let Ok(z) = std::env::var("KESTREL_7ZZ") else { return };
        let d = std::env::temp_dir().join(format!("kestrel-emb-{}", std::process::id()));
        std::fs::create_dir_all(d.join("Thumbnails")).unwrap();
        let pb = gdk_pixbuf::Pixbuf::new(gdk_pixbuf::Colorspace::Rgb, false, 8, 600, 300).unwrap();
        pb.savev(d.join("Thumbnails/thumbnail.png"), "png", &[]).unwrap();
        let zip = |name: &str| {
            assert!(
                std::process::Command::new(&z)
                    .current_dir(&d)
                    .args(["a", "-tzip", name, "Thumbnails"])
                    .output()
                    .unwrap()
                    .status
                    .success()
            )
        };
        zip("map.xmind");
        let t = from_embedded(&d.join("map.xmind"), embedded("Map.XMIND").unwrap(), 256).expect("thumbnail");
        assert_eq!((t.width(), t.height()), (256, 128));
        // a member that isn't there is a miss, not an error
        assert!(from_embedded(&d.join("map.xmind"), embedded("a.sketch").unwrap(), 256).is_none());
        assert!(embedded("notes.txt").is_none());
        let _ = std::fs::remove_dir_all(&d);
    }
}
