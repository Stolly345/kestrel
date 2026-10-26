//! Freedesktop thumbnails: run the system's thumbnailers (/usr/share/thumbnailers) and store
//! results in the shared ~/.cache/thumbnails, so Nautilus and Chimera reuse each other's work.

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

pub fn can_thumbnail(info: &gio::FileInfo) -> bool {
    info.content_type().is_some_and(|ct| find(&ct).is_some())
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

/// Run the thumbnailer (on a worker thread). Killed after 10 s: broken files can hang some of them.
fn generate(file: &gio::File, ct: &str, mtime: i64, size: Size) -> Option<PathBuf> {
    let t = find(ct)?;
    let uri = file.uri().to_string();
    // inside ~/.cache/thumbnails, not /tmp: AppArmor lets sandboxed thumbnailers (papers for PDFs)
    // write under $HOME but denies creating files in /tmp
    let out = glib::user_cache_dir()
        .join("thumbnails")
        .join(format!(".chimera-{}-{}.png", std::process::id(), glib::random_int()));
    let _ = std::fs::create_dir_all(out.parent()?);
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
    let mut child = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let start = Instant::now();
    let ok = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s.success(),
            Ok(None) if start.elapsed() < Duration::from_secs(10) => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break false;
            }
        }
    };
    let pb = ok.then(|| gdk_pixbuf::Pixbuf::from_file(&out).ok()).flatten();
    let _ = std::fs::remove_file(&out);
    let pb = pb?;
    let dest = cache_path(&uri, size);
    std::fs::create_dir_all(dest.parent()?).ok()?;
    let tmp = dest.with_extension(format!("{}.tmp", std::process::id()));
    pb.savev(&tmp, "png", &[("tEXt::Thumb::URI", uri.as_str()), ("tEXt::Thumb::MTime", &mtime.to_string())])
        .ok()?;
    std::fs::rename(&tmp, &dest).ok()?;
    Some(dest)
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
    let ct = info.content_type()?.to_string();
    let uri = file.uri().to_string();
    let mtime = info.modification_date_time().map_or(0, |d| d.to_unix());
    if FAILED.with_borrow(|f| f.contains(&uri)) || find(&ct).is_none() {
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
