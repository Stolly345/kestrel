//! What the quick-search window finds besides files: apps, GNOME Settings pages, sums, web
//! searches, system actions, and the `?` tips. Each provider turns the typed text into rows.

use gtk::gio;
use gtk::prelude::*;
use serde::{Deserialize, Serialize};

/// What Enter does on a row.
#[derive(Clone)]
pub enum Act {
    App(gio::AppInfo),
    /// A file or folder (folders open in Kestrel).
    Path(String),
    Copy(String),
    Url(String),
    System(&'static SysAction),
    /// Put this text in the search box.
    Insert(String),
}

#[derive(Clone)]
pub struct Row {
    pub title: String,
    pub subtitle: String,
    pub icon: gio::Icon,
    pub act: Act,
    /// What kind of thing it is, shown quietly at the row's end ("App", "Settings"); empty for files.
    pub kind: &'static str,
    /// The section it sits under before you type ("Pinned", "Recent"); empty while searching.
    pub section: &'static str,
}

fn themed(name: &str) -> gio::Icon {
    gio::ThemedIcon::new(name).upcast()
}

// ── apps and Settings pages ──

/// Lowercased words of a name, split at spaces, punctuation and camelCase: "VSCodium Insiders" →
/// ["vscodium", "insiders"], "LibreOffice Calc" → ["libre", "office", "calc"].
fn words(name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut prev_lower = false;
    for c in name.chars() {
        if !c.is_alphanumeric() {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur).to_lowercase());
            }
            prev_lower = false;
            continue;
        }
        if c.is_uppercase() && prev_lower && !cur.is_empty() {
            out.push(std::mem::take(&mut cur).to_lowercase());
        }
        prev_lower = c.is_lowercase();
        cur.push(c);
    }
    if !cur.is_empty() {
        out.push(cur.to_lowercase());
    }
    out
}

/// How well `q` (lowercased) fits a name: the whole name, its start, a word's start, its
/// initials (`vsc` → Visual Studio Code), anywhere; then extra terms (keywords, the command).
pub fn name_score(q: &str, name: &str, extra: &[String]) -> i32 {
    let n = name.to_lowercase();
    let w = words(name);
    let initials: String = w.iter().filter_map(|x| x.chars().next()).collect();
    if n == q {
        100
    } else if n.starts_with(q) {
        80
    } else if w.iter().any(|x| x.starts_with(q)) {
        60
    } else if q.len() >= 2 && initials.starts_with(q) {
        55
    } else if q.len() >= 3 && n.contains(q) {
        30
    } else if q.len() >= 3
        && extra
            .iter()
            .any(|k| k.to_lowercase().starts_with(q) || words(k).iter().any(|w| w.starts_with(q)))
    {
        25
    } else {
        0
    }
}

/// An app or a Settings page, with the words it can be found by.
#[derive(Clone)]
pub struct App {
    pub info: gio::AppInfo,
    pub name: String,
    pub desc: String,
    pub icon: gio::Icon,
    /// Keywords, generic name ("Web Browser") and the command's name.
    pub terms: Vec<String>,
}

fn desktop_dirs() -> Vec<std::path::PathBuf> {
    let mut dirs = vec![gtk::glib::user_data_dir()];
    dirs.extend(gtk::glib::system_data_dirs());
    dirs.into_iter().map(|d| d.join("applications")).collect()
}

/// Keywords and GenericName from an app's .desktop file (GIO's AppInfo doesn't expose them).
fn desktop_terms(id: &str) -> Vec<String> {
    let kf = gtk::glib::KeyFile::new();
    let found = desktop_dirs()
        .into_iter()
        .any(|d| kf.load_from_file(d.join(id), gtk::glib::KeyFileFlags::NONE).is_ok());
    if !found {
        return Vec::new();
    }
    let mut v: Vec<String> = kf
        .locale_string_list("Desktop Entry", "Keywords", None)
        .map(|l| l.iter().map(|k| k.to_string()).collect())
        .unwrap_or_default();
    v.extend(kf.locale_string("Desktop Entry", "GenericName", None).ok().map(|g| g.to_string()));
    v
}

impl App {
    fn from_info(a: &gio::AppInfo) -> App {
        let mut terms = a.id().map(|id| desktop_terms(&id)).unwrap_or_default();
        if let Some(exe) = a.executable().file_name() {
            terms.push(exe.to_string_lossy().into_owned());
        }
        App {
            info: a.clone(),
            name: a.name().to_string(),
            desc: a.description().map(|d| d.to_string()).unwrap_or_default(),
            icon: a.icon().unwrap_or_else(|| themed("application-x-executable")),
            terms,
        }
    }

    /// The id used for an app in pick history.
    pub fn key(&self) -> String {
        app_key(&self.info)
    }
}

/// The id used for an app in pick history.
pub fn app_key(a: &gio::AppInfo) -> String {
    format!("app:{}", a.id().map(|s| s.to_string()).unwrap_or_else(|| a.name().to_string()))
}

pub fn apps(q: &str, all: &[App], bonus: &dyn Fn(&str) -> i32, max: usize) -> Vec<Row> {
    let mut hits: Vec<(i32, &App)> = all
        .iter()
        .filter_map(|a| {
            let s = name_score(q, &a.name, &a.terms);
            (s > 0).then(|| (s + bonus(&a.key()), a))
        })
        .collect();
    hits.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.name.cmp(&b.1.name)));
    hits.into_iter()
        .take(max)
        .map(|(_, a)| Row {
            title: a.name.clone(),
            subtitle: if a.desc.is_empty() { "Application".into() } else { a.desc.clone() },
            icon: a.icon.clone(),
            act: Act::App(a.info.clone()),
            kind: "App",
            section: "",
        })
        .collect()
}

/// Installed apps that show in the app grid.
pub fn all_apps() -> Vec<App> {
    gio::AppInfo::all().iter().filter(|a| a.should_show()).map(App::from_info).collect()
}

/// GNOME Settings pages: each ships as a hidden gnome-*-panel.desktop with its own keywords
/// ("dark", "wallpaper", "resolution") and runs `gnome-control-center PAGE`.
pub fn settings_pages() -> Vec<App> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for d in desktop_dirs() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let file = e.file_name().to_string_lossy().into_owned();
            if !(file.starts_with("gnome-") && file.ends_with("-panel.desktop")) || !seen.insert(file) {
                continue;
            }
            let kf = gtk::glib::KeyFile::new();
            if kf.load_from_file(e.path(), gtk::glib::KeyFileFlags::NONE).is_err() {
                continue;
            }
            let get = |k: &str| kf.locale_string("Desktop Entry", k, None).ok().map(|s| s.to_string());
            let (Some(name), Some(exec)) = (get("Name"), kf.string("Desktop Entry", "Exec").ok()) else {
                continue;
            };
            let exec: String = exec.split_whitespace().filter(|w| !w.starts_with('%')).collect::<Vec<_>>().join(" ");
            let Ok(info) = gio::AppInfo::create_from_commandline(&exec, Some(&name), gio::AppInfoCreateFlags::NONE) else {
                continue;
            };
            let terms = kf
                .locale_string_list("Desktop Entry", "Keywords", None)
                .map(|l| l.iter().map(|k| k.to_string()).collect())
                .unwrap_or_default();
            out.push(App {
                info,
                desc: get("Comment").unwrap_or_default(),
                icon: get("Icon").map_or_else(|| themed("org.gnome.Settings"), |i| themed(&i)),
                name,
                terms,
            });
        }
    }
    out
}

pub fn settings(q: &str, pages: &[App], max: usize) -> Vec<Row> {
    if q.len() < 2 {
        return Vec::new();
    }
    let mut hits: Vec<(i32, &App)> = pages
        .iter()
        .filter_map(|a| {
            // keywords are how people find these ("dark mode", "wallpaper"): they count fully
            let kw = q.len() >= 3
                && a.terms
                    .iter()
                    .any(|k| k.to_lowercase().starts_with(q) || words(k).iter().any(|w| w.starts_with(q)));
            let s = name_score(q, &a.name, &[]).max(if kw { 50 } else { 0 });
            (s > 0).then_some((s, a))
        })
        .collect();
    hits.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.name.cmp(&b.1.name)));
    hits.into_iter()
        .take(max)
        .map(|(_, a)| Row {
            title: a.name.clone(),
            subtitle: a.desc.clone(),
            icon: a.icon.clone(),
            act: Act::App(a.info.clone()),
            kind: "Settings",
            section: "",
        })
        .collect()
}

// ── calculator ──

pub fn calc(q: &str) -> Option<Row> {
    let v = crate::calc::eval(q)?;
    let s = crate::calc::format(v);
    Some(Row {
        title: format!("= {s}"),
        subtitle: "Enter copies the answer".into(),
        icon: themed("accessories-calculator-symbolic"),
        act: Act::Copy(s),
        kind: "Sum",
        section: "",
    })
}

// ── web searches ──

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct WebSearch {
    /// What you type first: `g rust lifetimes`.
    pub key: String,
    pub name: String,
    /// `{q}` becomes the search, URL-encoded.
    pub url: String,
}

pub fn default_web() -> Vec<WebSearch> {
    [
        ("g", "Google", "https://www.google.com/search?q={q}"),
        ("ddg", "DuckDuckGo", "https://duckduckgo.com/?q={q}"),
        ("yt", "YouTube", "https://www.youtube.com/results?search_query={q}"),
        ("gh", "GitHub", "https://github.com/search?q={q}"),
        ("wiki", "Wikipedia", "https://en.wikipedia.org/w/index.php?search={q}"),
    ]
    .into_iter()
    .map(|(k, n, u)| WebSearch {
        key: k.into(),
        name: n.into(),
        url: u.into(),
    })
    .collect()
}

pub fn web(q: &str, engines: &[WebSearch]) -> Option<Row> {
    let (key, rest) = q.trim_start().split_once(char::is_whitespace)?;
    let rest = rest.trim();
    let e = engines.iter().find(|e| e.key.eq_ignore_ascii_case(key))?;
    if rest.is_empty() {
        return None;
    }
    let enc = gtk::glib::Uri::escape_string(rest, None, false);
    Some(Row {
        title: format!("Search {} for “{rest}”", e.name),
        subtitle: e.url.split('/').nth(2).unwrap_or_default().to_string(),
        icon: themed("web-browser-symbolic"),
        act: Act::Url(e.url.replace("{q}", &enc)),
        kind: "Web",
        section: "",
    })
}

// ── system actions ──

pub struct SysAction {
    pub name: &'static str,
    pub words: &'static [&'static str],
    pub icon: &'static str,
    /// Tried in order until one starts.
    pub commands: &'static [&'static [&'static str]],
    /// Asks for a second Enter first.
    pub confirm: bool,
}

pub const SYSTEM: &[SysAction] = &[
    SysAction {
        name: "Lock Screen",
        words: &["lock"],
        icon: "system-lock-screen-symbolic",
        commands: &[&["loginctl", "lock-session"], &["xdg-screensaver", "lock"]],
        confirm: false,
    },
    SysAction {
        name: "Suspend",
        words: &["sleep", "suspend"],
        icon: "weather-clear-night-symbolic",
        commands: &[&["systemctl", "suspend"]],
        confirm: false,
    },
    SysAction {
        name: "Log Out",
        words: &["log out", "logout", "sign out"],
        icon: "system-log-out-symbolic",
        commands: &[&["gnome-session-quit", "--logout", "--no-prompt"], &["loginctl", "terminate-session", ""]],
        confirm: true,
    },
    // gnome-session-quit first: GNOME's session holds a shutdown inhibitor that makes a bare
    // `systemctl reboot` fail with "Operation denied due to active block inhibitor"
    SysAction {
        name: "Restart",
        words: &["restart", "reboot"],
        icon: "system-reboot-symbolic",
        commands: &[&["gnome-session-quit", "--reboot", "--no-prompt"], &["systemctl", "reboot"]],
        confirm: true,
    },
    SysAction {
        name: "Shut Down",
        words: &["shut down", "shutdown", "power off", "poweroff"],
        icon: "system-shutdown-symbolic",
        commands: &[&["gnome-session-quit", "--power-off", "--no-prompt"], &["systemctl", "poweroff"]],
        confirm: true,
    },
    SysAction {
        name: "Restart into Firmware Setup",
        words: &["bios", "uefi", "firmware"],
        icon: "system-reboot-symbolic",
        commands: &[&[
            "sh",
            "-c",
            "busctl call org.freedesktop.login1 /org/freedesktop/login1 org.freedesktop.login1.Manager SetRebootToFirmwareSetup b true && (gnome-session-quit --reboot --no-prompt || systemctl reboot)",
        ]],
        confirm: true,
    },
];

pub fn system(q: &str) -> Vec<Row> {
    let q = q.trim().to_lowercase();
    if q.len() < 3 {
        return Vec::new();
    }
    SYSTEM
        .iter()
        .filter(|a| a.words.iter().any(|w| w.starts_with(&q)) || a.name.to_lowercase().starts_with(&q))
        .map(|a| Row {
            title: a.name.into(),
            subtitle: if a.confirm { "Asks before it acts".into() } else { String::new() },
            icon: themed(a.icon),
            act: Act::System(a),
            kind: "System",
            section: "",
        })
        .collect()
}

pub fn run_system(a: &SysAction) -> bool {
    a.commands.iter().any(|c| std::process::Command::new(c[0]).args(&c[1..]).spawn().is_ok())
}

// ── tips ──

/// (what to type, what it does, inserted on Enter)
pub const TIPS: &[(&str, &str, &str)] = &[
    ("report ext:pdf", "Files by extension (ext:jpg;png for several)", "ext:"),
    ("pic:  video:  audio:  doc:  code:  zip:", "Files by kind", "pic: "),
    ("size:>100mb", "By size (also size:huge, size:1mb..10mb)", "size:>"),
    ("dm:thisweek", "By date modified (today, last2weeks, 2026-10…)", "dm:"),
    ("under:~/Projects", "Only below a folder", "under:"),
    ("kestrel/src main", "A word with / matches the full path", ""),
    ("!tmp", "Leave out names containing a word", "!"),
    ("report | summary", "Either word", ""),
    ("2^10 * 3", "Calculator; Enter copies the answer", ""),
    ("g rust lifetimes", "Web search: g, ddg, yt, gh, wiki (add your own in settings.json)", "g "),
    ("bluetooth, display", "GNOME Settings pages", ""),
    ("lock, sleep, restart", "System actions; restart and shut down ask first", ""),
    ("Ctrl+Enter", "Show the selected file in its folder", ""),
    ("Ctrl+C", "Copy the selected file's path", ""),
    ("Right-click", "Pin to top, Open file location, Copy path, Forget", ""),
];

pub fn tips() -> Vec<Row> {
    TIPS.iter()
        .map(|(t, d, ins)| Row {
            title: t.to_string(),
            subtitle: d.to_string(),
            icon: themed("dialog-information-symbolic"),
            act: Act::Insert(ins.to_string()),
            kind: "Tip",
            section: "",
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_names_match_like_a_launcher() {
        assert_eq!(words("LibreOffice Calc"), ["libre", "office", "calc"]);
        assert_eq!(words("Visual Studio Code"), ["visual", "studio", "code"]);
        assert_eq!(name_score("firefox", "Firefox", &[]), 100);
        assert_eq!(name_score("fire", "Firefox", &[]), 80);
        assert_eq!(name_score("calc", "LibreOffice Calc", &[]), 60);
        assert_eq!(name_score("vsc", "Visual Studio Code", &[]), 55);
        assert_eq!(name_score("off", "LibreOffice Calc", &[]), 60, "camelCase word");
        assert_eq!(name_score("ice", "LibreOffice Calc", &[]), 30);
        assert_eq!(name_score("brow", "Firefox", &["Web Browser".into()]), 25);
        assert_eq!(name_score("zz", "Firefox", &[]), 0);
        assert_eq!(name_score("v", "Visual Studio Code", &[]), 80);
    }

    #[test]
    fn prefixes() {
        let w = default_web();
        let r = web("g rust lifetimes", &w).unwrap();
        assert!(matches!(&r.act, Act::Url(u) if u == "https://www.google.com/search?q=rust%20lifetimes"));
        assert!(web("g", &w).is_none() && web("g   ", &w).is_none() && web("gx rust", &w).is_none());
        assert_eq!(system("rest")[0].title, "Restart");
        assert_eq!(system("bios")[0].title, "Restart into Firmware Setup");
        assert_eq!(system("re").len(), 0, "too short: that's a search");
        assert_eq!(system("shut")[0].title, "Shut Down");
        assert!(calc("2+2").is_some() && calc("report").is_none());
    }
}
