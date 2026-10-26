//! FileInfo helpers and how the views format values: types, dates, sizes, typed locations.

use super::*;

pub fn info_file(info: &gio::FileInfo) -> Option<gio::File> {
    info.attribute_object("standard::file").and_downcast()
}

/// The link's target, or None for a non-link. GIO only fills standard::symlink-target for links
/// (and search hits have no link attributes at all), and its getters log a CRITICAL when asked
/// for an attribute that was never set.
pub(crate) fn link_target(info: &gio::FileInfo) -> Option<std::path::PathBuf> {
    info.has_attribute("standard::symlink-target").then(|| info.symlink_target()).flatten()
}

pub(crate) fn is_dirlike(info: &gio::FileInfo) -> bool {
    matches!(info.file_type(), gio::FileType::Directory | gio::FileType::Mountable | gio::FileType::Shortcut)
}

/// Where activating this entry should go: shortcuts/mountables (network://, computer://) carry a target.
pub(crate) fn nav_target(info: &gio::FileInfo) -> Option<gio::File> {
    match info.attribute_string("standard::target-uri") {
        Some(t) => Some(gio::File::for_uri(&t)),
        None => info_file(info),
    }
}

pub(crate) fn info_of(o: &glib::Object) -> &gio::FileInfo {
    o.downcast_ref::<gio::FileInfo>().unwrap()
}

pub(crate) fn cmp<T: Ord>(a: T, b: T) -> gtk::Ordering {
    a.cmp(&b).into()
}

pub(crate) fn mtime(i: &gio::FileInfo) -> i64 {
    i.modification_date_time().map_or(0, |d| d.to_unix())
}

/// The Type column, in the style chosen in Preferences.
pub fn type_label(i: &gio::FileInfo) -> String {
    let desc = i.content_type().map(|c| gio::content_type_get_description(&c).to_string()).unwrap_or_default();
    let name = i.name();
    let ext = (i.file_type() != gio::FileType::Directory).then(|| name.to_str().and_then(extension)).flatten();
    type_text(&desc, ext.as_deref(), crate::prefs::type_style())
}

/// "report.TAR.gz" -> "TAR.GZ", "a.xlsx" -> "XLSX"; none for dotfiles and names without one.
pub(crate) fn extension(name: &str) -> Option<String> {
    let lower = name.to_lowercase();
    let start = match lower.rfind(".tar.") {
        Some(p) if p > 0 => p + 1,
        _ => lower.rfind('.').filter(|&p| p > 0 && p + 1 < lower.len())? + 1,
    };
    Some(name[start..].to_uppercase())
}

pub(crate) fn type_text(desc: &str, ext: Option<&str>, style: crate::prefs::TypeStyle) -> String {
    use crate::prefs::TypeStyle;
    // GIO's fallback for unknown types is "text/x-foo type": no better than the extension
    let unknown = desc.is_empty() || desc.ends_with(" type") && desc.contains('/');
    match style {
        TypeStyle::Extension => ext.map(str::to_string).unwrap_or_else(|| desc.to_string()),
        _ if unknown => ext.map(|e| format!("{e} file")).unwrap_or_else(|| desc.to_string()),
        TypeStyle::Full => desc.to_string(),
        TypeStyle::Short => {
            // "Microsoft Excel Worksheet" -> "Excel", "JPEG image" -> "JPEG", "Tar archive (gzip-compressed)" -> "Tar"
            let d = desc.split(" (").next().unwrap_or(desc);
            let d = d.strip_prefix("Microsoft ").unwrap_or(d);
            const GENERIC: [&str; 9] = [
                "document",
                "worksheet",
                "spreadsheet",
                "presentation",
                "image",
                "video",
                "audio",
                "archive",
                "file",
            ];
            match d.rsplit_once(' ') {
                Some((head, last)) if GENERIC.contains(&last.to_lowercase().as_str()) => head.to_string(),
                _ => d.to_string(),
            }
        }
    }
}

/// Modified time, time first: "14:32 Today", "09:05 Yesterday", "18:40 12 Oct", "18:40 12 Oct 2024".
pub fn fmt_date(i: &gio::FileInfo) -> String {
    let now = glib::DateTime::now_local().ok();
    i.modification_date_time()
        .filter(|d| d.to_unix() != 0)
        .and_then(|d| d.to_local().ok())
        .zip(now)
        .map(|(d, now)| fmt_when(&d, &now, crate::prefs::get().clock_12h()))
        .unwrap_or_default()
}

pub(crate) fn fmt_when(d: &glib::DateTime, now: &glib::DateTime, h12: bool) -> String {
    let f = |fmt: &str| d.format(fmt).map(|s| s.trim().to_string()).unwrap_or_default();
    let time = f(if h12 { "%l:%M %p" } else { "%H:%M" });
    let same_day = |a: &glib::DateTime, b: &glib::DateTime| (a.year(), a.day_of_year()) == (b.year(), b.day_of_year());
    let date = if same_day(d, now) {
        "Today".into()
    } else if now.add_days(-1).is_ok_and(|y| same_day(d, &y)) {
        "Yesterday".into()
    } else if d.year() == now.year() {
        f("%e %b")
    } else {
        f("%e %b %Y")
    };
    format!("{time} {date}")
}

/// 1234567 -> "1,234,567"
pub fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Text typed into a location bar: a path, `~/…`, or a URI. Nonexistent paths are None.
pub fn as_location(t: &str) -> Option<gio::File> {
    let t = t.trim();
    let f = if t == "~" || t.starts_with("~/") {
        gio::File::for_path(glib::home_dir().join(t[1..].trim_start_matches('/')))
    } else if t.starts_with('/') {
        gio::File::for_path(t)
    } else if t.contains(':') {
        return Some(gio::File::for_uri(t));
    } else {
        return None;
    };
    match f.query_file_type(gio::FileQueryInfoFlags::NONE, gio::Cancellable::NONE) {
        gio::FileType::Unknown => None,
        gio::FileType::Directory => Some(f),
        _ => f.parent(),
    }
}
