//! FileInfo helpers and how the views format values: types, dates, sizes, typed locations.

use super::*;

/// A GIO error in plain words for the screen; uncommon ones keep GIO's own text.
pub fn human_gio(e: &glib::Error) -> String {
    use gio::IOErrorEnum as E;
    let plain = match e.kind::<E>() {
        Some(E::PermissionDenied) => "You don't have permission",
        Some(E::NoSpace) => "The disk is full",
        Some(E::NotFound) => "It isn't there any more",
        Some(E::Exists) => "Something with that name already exists",
        Some(E::NotMounted) => "That drive or share isn't connected",
        Some(E::ReadOnly) => "This drive is read-only",
        Some(E::Busy) => "Another program is using it",
        Some(E::NotEmpty) => "The folder isn't empty",
        Some(E::FilenameTooLong) => "The name is too long",
        Some(E::InvalidFilename) => "Names can't contain \u{201c}/\u{201d}",
        Some(E::WouldRecurse) => "A folder can't go inside itself",
        Some(E::TimedOut) | Some(E::HostNotFound) | Some(E::HostUnreachable) | Some(E::ConnectionRefused) | Some(E::NetworkUnreachable) => {
            "The server didn't answer"
        }
        Some(E::NotSupported) => "That isn't possible on this drive",
        Some(E::Cancelled) => "Cancelled",
        _ => return e.message().to_string(),
    };
    plain.into()
}

/// The same for errors from std::fs: "Permission denied (os error 13)" reads like a crash.
pub fn human_io(e: &std::io::Error) -> String {
    use std::io::ErrorKind as K;
    match e.kind() {
        K::PermissionDenied => "You don't have permission".into(),
        K::NotFound => "It isn't there any more".into(),
        K::AlreadyExists => "Something with that name already exists".into(),
        K::StorageFull => "The disk is full".into(),
        K::ReadOnlyFilesystem => "This drive is read-only".into(),
        K::DirectoryNotEmpty => "The folder isn't empty".into(),
        K::ResourceBusy => "Another program is using it".into(),
        _ => {
            let t = e.to_string();
            t.split(" (os error").next().unwrap_or(&t).to_string()
        }
    }
}

pub fn info_file(info: &gio::FileInfo) -> Option<gio::File> {
    info.attribute_object("standard::file").and_downcast()
}

/// The link's target, or None for a non-link. GIO only fills standard::symlink-target for links
/// (and search hits have no link attributes at all), and its getters log a CRITICAL when asked
/// for an attribute that was never set.
pub(crate) fn link_target(info: &gio::FileInfo) -> Option<std::path::PathBuf> {
    info.has_attribute("standard::symlink-target").then(|| info.symlink_target()).flatten()
}

/// The file's type, if the listing included it. Some entries (network places, items still loading)
/// come without one, and GIO's getter logs a CRITICAL for every such call.
pub(crate) fn content_type(info: &gio::FileInfo) -> Option<glib::GString> {
    info.has_attribute("standard::content-type").then(|| info.content_type()).flatten()
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
    let desc = content_type(i).map(|c| gio::content_type_get_description(&c).to_string()).unwrap_or_default();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_read_as_sentences() {
        assert_eq!(human_io(&std::io::Error::from_raw_os_error(13)), "You don't have permission");
        // uncommon errors keep their text, minus the errno noise
        assert_eq!(human_io(&std::io::Error::from_raw_os_error(71)), "Protocol error");
        let e = glib::Error::new(gio::IOErrorEnum::NotMounted, "The specified location is not mounted");
        assert_eq!(human_gio(&e), "That drive or share isn't connected");
        let e = glib::Error::new(gio::IOErrorEnum::Failed, "Something odd");
        assert_eq!(human_gio(&e), "Something odd");
    }
}
