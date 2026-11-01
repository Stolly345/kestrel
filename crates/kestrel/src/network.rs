//! The Network page (network:///), laid out like GNOME Files': shares you're connected to, servers
//! you've used before (GTK's ~/.config/gtk-4.0/servers, shared with Files) and online-account
//! drives, what's found on the local network, and a box to type a server address.
//! network:/// on its own lists only the last group. Connected SMB shares can be indexed for search
//! (Index…): `kestrel-indexd --add-share` under pkexec puts them in /etc/fstab with `x-kestrel`.

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

/// A share indexed from /etc/fstab: "//nas.local/alex", "/mnt/nas", added from this page.
struct Indexed {
    source: String,
    mount: std::path::PathBuf,
    ours: bool,
}

fn indexed_in(fstab: &str) -> Vec<Indexed> {
    fstab
        .lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>())
        .filter(|f| f.len() >= 4 && !f[0].starts_with('#') && kestrel_index::scan::NETWORK_FS.contains(&f[2]))
        .filter(|f| f[3].split(',').any(|o| o == "x-kestrel"))
        .map(|f| Indexed {
            source: kestrel_index::scan::unescape(f[0]),
            mount: kestrel_index::scan::unescape(f[1]).into(),
            ours: f[3].contains("credentials=/etc/kestrel/shares/"),
        })
        .collect()
}

fn indexed() -> Vec<Indexed> {
    indexed_in(&std::fs::read_to_string("/etc/fstab").unwrap_or_default())
}

/// smb://[domain;user@]server/share/… -> (server, share, user)
fn smb_parts(uri: &str) -> Option<(String, String, String)> {
    let u = glib::Uri::parse(uri, glib::UriFlags::NONE).ok()?;
    if u.scheme() != "smb" {
        return None;
    }
    let share = u.path().trim_start_matches('/').split('/').next().filter(|s| !s.is_empty())?.to_string();
    let user = u.userinfo().map(|i| i.rsplit(';').next().unwrap_or("").to_string()).unwrap_or_default();
    Some((u.host()?.to_string(), share, user))
}

const INDEXD: &str = "/usr/bin/kestrel-indexd";

/// Run the share helper as root; Ok(stdout) or Err(message). None = the password prompt was dismissed.
async fn helper(args: &[&str], input: &str) -> Option<Result<String, String>> {
    let mut argv: Vec<&std::ffi::OsStr> = vec!["pkexec".as_ref(), INDEXD.as_ref()];
    argv.extend(args.iter().map(|a| std::ffi::OsStr::new(*a)));
    let flags = gio::SubprocessFlags::STDIN_PIPE | gio::SubprocessFlags::STDOUT_PIPE | gio::SubprocessFlags::STDERR_PIPE;
    let p = match gio::Subprocess::newv(&argv, flags) {
        Ok(p) => p,
        Err(e) => return Some(Err(e.to_string())),
    };
    let (out, err) = match p.communicate_utf8_future(Some(input.to_string())).await {
        Ok(o) => o,
        Err(e) => return Some(Err(e.to_string())),
    };
    match p.exit_status() {
        0 => Some(Ok(out.map(|s| s.trim().to_string()).unwrap_or_default())),
        126 | 127 => None,
        _ => Some(Err(err.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).unwrap_or_else(|| "It didn't work".into()))),
    }
}

/// "today at 10:52", "Oct 30 at 09:15"
fn when(at: i64) -> String {
    let Ok(t) = glib::DateTime::from_unix_local(at) else { return String::new() };
    let today = glib::DateTime::now_local().is_ok_and(|n| n.ymd() == t.ymd());
    t.format(if today { "today at %H:%M" } else { "%b %-d at %H:%M" }).map(|s| s.to_string()).unwrap_or_default()
}

/// An Indexed Shares row's subtitle: where it's mounted, and what the index service says about it.
fn share_line(mount: &str, st: Option<&kestrel_index::proto::ShareStatus>) -> String {
    use kestrel_index::proto::ShareState::*;
    let Some(st) = st else { return mount.to_string() };
    let n = crate::preview::thousands(st.entries as u64);
    let last = if st.at > 0 { format!("the listing from {}", when(st.at)) } else { "its last listing".into() };
    let state = match st.state {
        Waiting => "Waiting to be listed".to_string(),
        Listing => format!("Listing… {n} items so far"),
        Listed => format!("{n} items · listed {}", when(st.at)),
        Offline => format!("Can't be reached · searching {last}"),
        Failed => format!("Couldn't list it ({} folders unreadable) · searching {last}", st.errors),
    };
    format!("{mount} · {state}")
}

fn say(parent: &impl IsA<gtk::Widget>, heading: &str, body: &str) {
    let d = adw::AlertDialog::new(Some(heading), Some(body));
    d.add_response("ok", "OK");
    d.present(Some(parent));
}

/// Index…: ask for the share's user name and password, then hand it to the helper.
fn index_button(server: String, share: String, user: String) -> gtk::Button {
    let b = gtk::Button::builder()
        .label("Index…")
        .tooltip_text("Make this share's files show up in search")
        .valign(gtk::Align::Center)
        .build();
    b.connect_clicked(move |b| {
        let d = adw::AlertDialog::new(
            Some("Index this share?"),
            Some(&format!(
                "Kestrel will connect //{server}/{share} by itself and list it every 3 hours, so its files show up in your searches (no one else's). It needs the share's user name and password, and then your own password once."
            )),
        );
        let g = adw::PreferencesGroup::new();
        let name = adw::EntryRow::builder().title("User name on the share").text(&user).build();
        let pass = adw::PasswordEntryRow::builder().title("Password").build();
        g.add(&name);
        g.add(&pass);
        d.set_extra_child(Some(&g));
        d.add_response("cancel", "Cancel");
        d.add_response("index", "Index");
        d.set_response_appearance("index", adw::ResponseAppearance::Suggested);
        d.set_default_response(Some("index"));
        let (server, share, b2) = (server.clone(), share.clone(), b.downgrade());
        d.connect_response(Some("index"), move |_, _| {
            let input = format!("{server}\n{share}\n{}\n{}\n", name.text(), pass.text());
            let Some(b) = b2.upgrade() else { return };
            b.set_sensitive(false);
            b.set_label("Connecting…");
            glib::spawn_future_local(async move {
                match helper(&["--add-share"], &input).await {
                    None => {
                        b.set_label("Index…");
                        b.set_sensitive(true);
                    }
                    Some(Ok(mp)) => {
                        b.set_label("Indexed");
                        say(&b, "Indexed", &format!("Its files will show up in search within a few minutes. It's also at {mp}."));
                    }
                    Some(Err(e)) => {
                        b.set_label("Index…");
                        b.set_sensitive(true);
                        say(&b, "Couldn't index this share", &e);
                    }
                }
            });
        });
        d.present(Some(b));
    });
    b
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
        if let Some((server, share, user)) = smb_parts(&root.uri()) {
            // ponytail: compared as written: mounted by IP but browsed by name looks unindexed
            let src = format!("//{server}/{share}");
            if indexed().iter().any(|i| i.source.eq_ignore_ascii_case(&src)) {
                r.add_suffix(&gtk::Label::builder().label("Indexed").css_classes(["dim-label"]).build());
            } else {
                r.add_suffix(&index_button(server, share, user));
            }
        }
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

    // Indexed: network shares in fstab with x-kestrel; Stop Indexing for the ones added here
    let shares = adw::PreferencesGroup::builder()
        .title("Indexed Shares")
        .description("Listed every 3 hours, so their files show up in search")
        .build();
    let list = indexed();
    let mut rows = Vec::new();
    for s in &list {
        let r = row(&s.source, &s.mount.to_string_lossy(), "folder-remote-symbolic");
        rows.push((s.mount.to_string_lossy().into_owned(), r.downgrade()));
        if s.ours {
            let stop = gtk::Button::builder().label("Stop Indexing").valign(gtk::Align::Center).build();
            let (mp, r2) = (s.mount.to_string_lossy().into_owned(), r.downgrade());
            stop.connect_clicked(move |b| {
                let (mp, r2, b) = (mp.clone(), r2.clone(), b.clone());
                b.set_sensitive(false);
                glib::spawn_future_local(async move {
                    match helper(&["--remove-share", &mp], "").await {
                        Some(Ok(_)) => {
                            if let Some(r) = r2.upgrade() {
                                r.set_visible(false);
                            }
                        }
                        None => b.set_sensitive(true),
                        Some(Err(e)) => {
                            b.set_sensitive(true);
                            say(&b, "Couldn't stop indexing", &e);
                        }
                    }
                });
            });
            r.add_suffix(&stop);
        }
        let (o, mp) = (open.clone(), s.mount.clone());
        r.connect_activated(move |_| o(gio::File::for_path(&mp)));
        shares.add(&r);
    }
    if !list.is_empty() {
        page.add(&shares);
        // live status while the page is on screen: every 2 s, asked off the main thread
        let poll = move || {
            let rows = rows.clone();
            glib::spawn_future_local(async move {
                let Ok(Some(all)) = gio::spawn_blocking(crate::search::share_status).await else { return };
                for (mount, r) in &rows {
                    if let Some(r) = r.upgrade() {
                        r.set_subtitle(&glib::markup_escape_text(&share_line(mount, all.iter().find(|s| &s.path == mount))));
                    }
                }
            });
        };
        poll();
        let g = shares.downgrade();
        glib::timeout_add_local(std::time::Duration::from_secs(2), move || {
            let Some(g) = g.upgrade() else { return glib::ControlFlow::Break };
            if g.is_mapped() {
                poll();
            }
            glib::ControlFlow::Continue
        });
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
    fn share_status_lines() {
        use kestrel_index::proto::{ShareState, ShareStatus};
        let st = |state, entries, at, errors| ShareStatus { path: "/mnt/nas".into(), state, entries, at, errors };
        assert_eq!(share_line("/mnt/nas", None), "/mnt/nas");
        assert_eq!(share_line("/mnt/nas", Some(&st(ShareState::Listing, 84000, 0, 0))), "/mnt/nas · Listing… 84,000 items so far");
        let now = glib::DateTime::now_local().unwrap();
        let line = share_line("/mnt/nas", Some(&st(ShareState::Listed, 182200, now.to_unix(), 0)));
        assert!(line.starts_with("/mnt/nas · 182,200 items · listed today at "), "{line}");
        assert_eq!(share_line("/mnt/nas", Some(&st(ShareState::Offline, 0, 0, 0))), "/mnt/nas · Can't be reached · searching its last listing");
        assert!(share_line("/mnt/nas", Some(&st(ShareState::Failed, 0, 0, 14))).contains("(14 folders unreadable)"));
    }

    #[test]
    fn share_parts_and_fstab() {
        assert_eq!(smb_parts("smb://mycloudex2ultra.local/alex/"), Some(("mycloudex2ultra.local".into(), "alex".into(), String::new())));
        assert_eq!(smb_parts("smb://WORKGROUP;bob@nas/My%20Share/sub"), Some(("nas".into(), "My Share".into(), "bob".into())));
        assert_eq!(smb_parts("smb://nas/"), None, "a server, not a share");
        assert_eq!(smb_parts("sftp://host/x"), None);
        let got = indexed_in(
            "//192.168.50.238/alex /mnt/nas cifs credentials=/etc/nas-alex.cred,x-kestrel 0 0\n\
             //nas.local/My\\040Share /mnt/nas-my-share cifs credentials=/etc/kestrel/shares/n.cred,x-kestrel 0 0\n\
             //tv/games /mnt/tvbox cifs credentials=/etc/t,noauto 0 0\n",
        );
        assert_eq!(got.iter().map(|i| (i.source.as_str(), i.ours)).collect::<Vec<_>>(), [("//192.168.50.238/alex", false), ("//nas.local/My Share", true)]);
    }

    #[test]
    #[ignore]
    fn read_real_servers() {
        // XDG_CONFIG_HOME=… cargo test -p kestrel read_real_servers -- --ignored --nocapture
        println!("{:?}", recent_servers());
    }
}
