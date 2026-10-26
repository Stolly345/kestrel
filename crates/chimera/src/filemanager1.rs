//! org.freedesktop.FileManager1 on the session bus: how other apps do "Show in Folder"
//! (browsers' downloads, IDEs, `gio open` of a folder via portals). Run as `chimera --filemanager1`,
//! started on demand by D-Bus once the user makes Chimera the default file manager.

use crate::browser::Loc;
use crate::window::Win;
use adw::prelude::*;
use gtk::{gio, glib};
use std::path::PathBuf;

const NAME: &str = "org.freedesktop.FileManager1";
const PATH: &str = "/org/freedesktop/FileManager1";
const XML: &str = r#"<node>
  <interface name="org.freedesktop.FileManager1">
    <method name="ShowFolders"><arg type="as" name="URIs" direction="in"/><arg type="s" name="StartupId" direction="in"/></method>
    <method name="ShowItems"><arg type="as" name="URIs" direction="in"/><arg type="s" name="StartupId" direction="in"/></method>
    <method name="ShowItemProperties"><arg type="as" name="URIs" direction="in"/><arg type="s" name="StartupId" direction="in"/></method>
  </interface>
</node>"#;

fn handle(app: &adw::Application, method: &str, uris: Vec<String>) {
    let files: Vec<gio::File> = uris.iter().map(|u| gio::File::for_uri(u)).collect();
    match method {
        "ShowFolders" => {
            let locs = files.into_iter().map(Loc::Dir).collect();
            Win::new(app, locs, vec![], false).window.present();
        }
        // one window per parent folder, with the items selected
        "ShowItems" | "ShowItemProperties" => {
            let mut by_dir: Vec<(gio::File, Vec<String>)> = Vec::new();
            for f in &files {
                let (Some(dir), Some(name)) = (f.parent(), f.basename()) else { continue };
                let name = name.to_string_lossy().into_owned();
                match by_dir.iter_mut().find(|(d, _)| d.equal(&dir)) {
                    Some((_, names)) => names.push(name),
                    None => by_dir.push((dir, vec![name])),
                }
            }
            for (dir, names) in by_dir {
                let w = Win::new(app, vec![Loc::Dir(dir)], vec![], false);
                w.window.present();
                if let Some(b) = w.current() {
                    let then_props = method == "ShowItemProperties";
                    let win = w.window.clone();
                    b.select_when_loaded(names, move || {
                        if then_props {
                            let _ = WidgetExt::activate_action(&win, "win.properties", None);
                        }
                    });
                }
            }
        }
        _ => {}
    }
}

/// Take the name and export the object. `REPLACE` so we win over a stale owner.
pub fn own(app: &adw::Application) {
    let app = app.clone();
    gio::bus_own_name(
        gio::BusType::Session,
        NAME,
        gio::BusNameOwnerFlags::REPLACE | gio::BusNameOwnerFlags::ALLOW_REPLACEMENT,
        move |conn, _| {
            let info = gio::DBusNodeInfo::for_xml(XML).expect("FileManager1 introspection");
            let iface = info.lookup_interface(NAME).expect("interface");
            let app = app.clone();
            let r = conn
                .register_object(PATH, &iface)
                .method_call(move |_, _, _, _, method, params, invocation| {
                    if let Some((uris, _startup)) = params.get::<(Vec<String>, String)>() {
                        handle(&app, method, uris);
                    }
                    invocation.return_value(None);
                })
                .build();
            if let Err(e) = r {
                eprintln!("chimera: FileManager1 register failed: {e}");
            }
        },
        |_, _| {},
        |_, _| eprintln!("chimera: lost {NAME} to another file manager"),
    );
}

fn service_file() -> PathBuf {
    glib::user_data_dir().join("dbus-1/services").join(format!("{NAME}.service"))
}

pub fn is_default() -> bool {
    std::fs::read_to_string(service_file()).is_ok_and(|s| s.contains("chimera"))
}

/// Make Chimera what folders open in and what "Show in Folder" talks to, for this user only.
/// A user-level D-Bus service file takes precedence over Nautilus' system one.
pub fn make_default(on: bool) -> Result<(), String> {
    let f = service_file();
    if on {
        std::fs::create_dir_all(f.parent().unwrap()).map_err(|e| e.to_string())?;
        std::fs::write(&f, format!("[D-BUS Service]\nName={NAME}\nExec=/usr/bin/chimera --filemanager1\n")).map_err(|e| e.to_string())?;
    } else if f.exists() {
        std::fs::remove_file(&f).map_err(|e| e.to_string())?;
    }
    let desktop = if on { "dev.chimera.Chimera.desktop" } else { "org.gnome.Nautilus.desktop" };
    let ok = std::process::Command::new("xdg-mime")
        .args(["default", desktop, "inode/directory"])
        .status()
        .is_ok_and(|s| s.success());
    if ok { Ok(()) } else { Err("xdg-mime failed".into()) }
}
