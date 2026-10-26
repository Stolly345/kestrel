//! The Network page (network:///), laid out like GNOME Files': shares you're connected to, servers
//! you've used before (GTK's ~/.config/gtk-4.0/servers, shared with Files) and online-account
//! drives, what's found on the local network, and a box to type a server address.
//! network:/// on its own lists only the last group.

use adw::prelude::*;
use gtk::{gio, glib};
use std::rc::Rc;

fn servers_file() -> std::path::PathBuf {
    glib::user_config_dir().join("gtk-4.0/servers")
}

/// Servers used before, newest first: (uri, title).
// ponytail: reads the XBEL by hand; glib-rs 0.22's BookmarkFile panics on a NULL from C ("assertion
// failed: !ptr.is_null()"), which took the whole app down when the Network page opened
fn recent_servers() -> Vec<(String, String)> {
    let Ok(xml) = std::fs::read_to_string(servers_file()) else {
        return Vec::new();
    };
    let mut v: Vec<(String, String, String)> = xml
        .split("<bookmark ")
        .skip(1)
        .filter_map(|b| {
            let attr = |k: &str| b.split(&format!("{k}=\"")).nth(1).and_then(|r| r.split('"').next()).map(unxml);
            let title = b
                .split("<title>")
                .nth(1)
                .and_then(|t| t.split("</title>").next())
                .map(unxml)
                .unwrap_or_default();
            Some((attr("visited").or_else(|| attr("added")).unwrap_or_default(), attr("href")?, title))
        })
        .collect();
    v.sort_by(|a, b| b.0.cmp(&a.0)); // ISO-8601 dates sort as text
    v.into_iter().map(|(_, u, t)| (u, t)).collect()
}

fn unxml(s: &str) -> String {
    s.replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;").replace('"', "&quot;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Remember a server the way GTK's file chooser and GNOME Files do (same file, same format).
pub fn remember_server(uri: &str) {
    if recent_servers().iter().any(|(u, _)| u == uri) {
        return;
    }
    let now = glib::DateTime::now_utc()
        .ok()
        .and_then(|d| d.format_iso8601().ok())
        .map(|s| s.to_string())
        .unwrap_or_default();
    let entry = format!(
        "  <bookmark href=\"{}\" added=\"{now}\" modified=\"{now}\" visited=\"{now}\">\n  </bookmark>\n",
        xml(uri)
    );
    let old = std::fs::read_to_string(servers_file()).unwrap_or_default();
    let new = match old.rfind("</xbel>") {
        Some(end) => format!("{}{entry}{}", &old[..end], &old[end..]),
        None => format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<xbel version=\"1.0\">\n{entry}</xbel>\n"),
    };
    let _ = std::fs::create_dir_all(glib::user_config_dir().join("gtk-4.0"));
    let _ = std::fs::write(servers_file(), new);
}

fn forget_server(uri: &str) {
    let Ok(old) = std::fs::read_to_string(servers_file()) else { return };
    let href = format!("<bookmark href=\"{}\"", xml(uri));
    let Some(start) = old.find(&href) else { return };
    let Some(len) = old[start..].find("</bookmark>") else { return };
    let mut end = start + len + "</bookmark>".len();
    if old[end..].starts_with('\n') {
        end += 1;
    }
    let line_start = old[..start].rfind('\n').map_or(start, |n| n + 1);
    let _ = std::fs::write(servers_file(), format!("{}{}", &old[..line_start], &old[end..]));
}

/// "sftp://homeserver@192.168.50.159/mnt/sabrent" -> ("homeserver@192.168.50.159", "/mnt/sabrent")
fn short_name(uri: &str) -> String {
    let rest = uri.split_once("://").map_or(uri, |(_, r)| r);
    let rest = glib::Uri::unescape_string(rest, None::<&str>).map_or(rest.to_string(), |s| s.to_string());
    rest.trim().trim_end_matches('/').to_string()
}

fn is_remote(f: &gio::File) -> bool {
    !f.is_native() || f.path().is_some_and(|p| p.starts_with(glib::user_runtime_dir().join("gvfs")))
}

fn row(title: &str, subtitle: &str, icon: &str) -> adw::ActionRow {
    let r = adw::ActionRow::builder()
        .title(glib::markup_escape_text(title))
        .subtitle(glib::markup_escape_text(subtitle))
        .activatable(true)
        .build();
    r.add_prefix(&gtk::Image::from_icon_name(icon));
    r
}

/// The page. `open` takes a location to browse (Browser::load_dir mounts it if needed).
pub fn page(open: Rc<dyn Fn(gio::File)>) -> gtk::Widget {
    let page = adw::PreferencesPage::new();
    let monitor = gio::VolumeMonitor::get();

    // Connected: mounted network shares and online-account drives
    let connected = adw::PreferencesGroup::builder().title("Connected").build();
    let mut mounted_uris = Vec::new();
    for m in monitor.mounts() {
        let root = m.root();
        if !is_remote(&root) || m.is_shadowed() {
            continue;
        }
        mounted_uris.push(root.uri().to_string());
        let r = row(&m.name(), &root.uri(), "folder-remote-symbolic");
        if m.can_unmount() {
            let eject = gtk::Button::builder()
                .icon_name("media-eject-symbolic")
                .tooltip_text("Disconnect")
                .valign(gtk::Align::Center)
                .css_classes(["flat"])
                .build();
            let (m2, r2) = (m.clone(), r.downgrade());
            eject.connect_clicked(move |_| {
                let r2 = r2.clone();
                m2.unmount_with_operation(gio::MountUnmountFlags::NONE, gio::MountOperation::NONE, gio::Cancellable::NONE, move |res| {
                    if res.is_ok()
                        && let Some(r) = r2.upgrade()
                    {
                        r.set_visible(false);
                    }
                });
            });
            r.add_suffix(&eject);
        }
        let o = open.clone();
        r.connect_activated(move |_| o(root.clone()));
        connected.add(&r);
    }
    if !mounted_uris.is_empty() {
        page.add(&connected);
    }

    // Previous: servers used before, and online-account drives not mounted right now
    let previous = adw::PreferencesGroup::builder().title("Previous").build();
    let mut any = false;
    for (uri, title) in recent_servers() {
        if mounted_uris.iter().any(|m| m.trim_end_matches('/') == uri.trim().trim_end_matches('/')) {
            continue;
        }
        any = true;
        let name = if title.is_empty() { short_name(&uri) } else { title };
        let r = row(&name, uri.trim(), "network-server-symbolic");
        let forget = gtk::Button::builder()
            .icon_name("edit-clear-symbolic")
            .tooltip_text("Forget this server")
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .build();
        let (u, r2) = (uri.clone(), r.downgrade());
        forget.connect_clicked(move |_| {
            forget_server(&u);
            if let Some(r) = r2.upgrade() {
                r.set_visible(false);
            }
        });
        r.add_suffix(&forget);
        let o = open.clone();
        r.connect_activated(move |_| o(gio::File::for_uri(uri.trim())));
        previous.add(&r);
    }
    for v in monitor.volumes() {
        // online accounts (Google Drive, OneDrive…) appear as volumes that mount on demand
        if v.get_mount().is_some() || v.activation_root().is_none_or(|a| !is_remote(&a)) {
            continue;
        }
        any = true;
        let r = row(&v.name(), "Online Account", "network-server-symbolic");
        let (v2, o) = (v.clone(), open.clone());
        r.connect_activated(move |_| {
            let o = o.clone();
            v2.mount(gio::MountMountFlags::NONE, gio::MountOperation::NONE, gio::Cancellable::NONE, {
                let v3 = v2.clone();
                move |res| {
                    if res.is_ok()
                        && let Some(m) = v3.get_mount()
                    {
                        o(m.root());
                    }
                }
            });
        });
        previous.add(&r);
    }
    if any {
        page.add(&previous);
    }

    // Available on the current network: what GVfs finds (DNS-SD / WS-Discovery), listed async
    let found = adw::PreferencesGroup::builder().title("Available on Current Network").build();
    let looking = gtk::Label::builder()
        .label("Looking…")
        .css_classes(["dim-label"])
        .halign(gtk::Align::Start)
        .build();
    found.add(&looking);
    page.add(&found);
    let (o, found2) = (open.clone(), found.clone());
    glib::spawn_future_local(async move {
        let net = gio::File::for_uri("network:///");
        let attrs = "standard::display-name,standard::target-uri,standard::icon";
        let items = match net
            .enumerate_children_future(attrs, gio::FileQueryInfoFlags::NONE, glib::Priority::DEFAULT)
            .await
        {
            Ok(e) => e.next_files_future(200, glib::Priority::DEFAULT).await.unwrap_or_default(),
            Err(_) => Vec::new(),
        };
        looking.set_label(if items.is_empty() { "Nothing found" } else { "" });
        looking.set_visible(items.is_empty());
        for i in items {
            let target = i
                .attribute_string("standard::target-uri")
                .map(|t| t.to_string())
                .unwrap_or_else(|| net.child(i.name()).uri().to_string());
            let r = row(&i.display_name(), &target, "network-server-symbolic");
            let o = o.clone();
            r.connect_activated(move |_| o(gio::File::for_uri(&target)));
            found2.add(&r);
        }
    });

    // a server by address: smb://nas.local/share, sftp://user@host/path, ftp://…
    let connect = adw::PreferencesGroup::builder().title("Connect to Server").build();
    let entry = adw::EntryRow::builder()
        .title("Server address, e.g. smb://nas.local/share or sftp://user@host")
        .show_apply_button(true)
        .build();
    let o = open.clone();
    entry.connect_apply(move |e| {
        let text = e.text().trim().to_string();
        if text.contains("://") {
            remember_server(&text);
            o(gio::File::for_uri(&text));
        }
    });
    connect.add(&entry);
    page.add(&connect);
    page.upcast()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_server_history() {
        assert_eq!(
            short_name("sftp://homeserver@192.168.50.159/mnt/sabrent%0A"),
            "homeserver@192.168.50.159/mnt/sabrent"
        );
        assert_eq!(short_name("smb://nas.local/share/"), "nas.local/share");
        // remember / forget round-trip on a copy of the format GTK writes
        let dir = std::env::temp_dir().join(format!("kestrel-net-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("gtk-4.0")).unwrap();
        // SAFETY: single-threaded test; glib reads XDG_CONFIG_HOME once per process
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &dir) };
        if glib::user_config_dir() != dir {
            return; // another test already pinned the config dir in this process
        }
        std::fs::write(
            servers_file(),
            "<?xml version=\"1.0\"?>\n<xbel version=\"1.0\">\n  <bookmark href=\"sftp://old@host/x%0A\" added=\"2026-07-28T02:25:56Z\" visited=\"2026-07-28T02:25:56Z\">\n    <info/>\n  </bookmark>\n</xbel>\n",
        )
        .unwrap();
        remember_server("smb://nas.local/a&b");
        let got = recent_servers();
        assert_eq!(
            got.iter().map(|(u, _)| u.as_str()).collect::<Vec<_>>(),
            ["smb://nas.local/a&b", "sftp://old@host/x%0A"]
        );
        forget_server("sftp://old@host/x%0A");
        assert_eq!(recent_servers().len(), 1);
        assert!(std::fs::read_to_string(servers_file()).unwrap().contains("a&amp;b"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[ignore]
    fn read_real_servers() {
        // XDG_CONFIG_HOME=… cargo test -p kestrel read_real_servers -- --ignored --nocapture
        println!("{:?}", recent_servers());
    }
}
