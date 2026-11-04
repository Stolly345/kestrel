//! .deb and .rpm: metadata from dpkg-deb / rpm (or the file name when the tool is missing),
//! the app's launcher and icon unpacked in the archive sandbox, and the package manager's
//! install command.

use super::{Fit, Kind, Pkg, Pm, Scratch};
use std::path::Path;
use std::process::Command;

fn run(cmd: &mut Command) -> Option<String> {
    let o = cmd.env("LC_ALL", "C").output().ok().filter(|o| o.status.success())?;
    Some(String::from_utf8_lossy(&o.stdout).into_owned())
}

fn has(p: &str) -> bool {
    gtk::glib::find_program_in_path(p).is_some()
}

/// "name_1.2-3_amd64.deb" → (name, 1.2-3, amd64).
fn deb_from_name(file: &str) -> Option<(String, String, String)> {
    let stem = file.strip_suffix(".deb").or_else(|| file.strip_suffix(".DEB"))?;
    let mut it = stem.splitn(3, '_');
    Some((it.next()?.into(), it.next().unwrap_or_default().into(), it.next().unwrap_or_default().into()))
}

/// "name-1.2-3.fc43.x86_64.rpm" → (name, 1.2-3.fc43, x86_64).
pub(super) fn rpm_from_name(file: &str) -> Option<(String, String, String)> {
    let stem = file.strip_suffix(".rpm")?;
    let (rest, arch) = stem.rsplit_once('.')?;
    let (rest, release) = rest.rsplit_once('-')?;
    let (name, version) = rest.rsplit_once('-')?;
    Some((name.into(), format!("{version}-{release}"), arch.into()))
}

/// The machine's architecture as dpkg and rpm name it.
fn machine(kind: Kind) -> String {
    match (kind, std::env::consts::ARCH) {
        (Kind::Deb, "x86_64") => "amd64".into(),
        (Kind::Deb, "aarch64") => "arm64".into(),
        (Kind::Deb, "x86") => "i386".into(),
        (_, a) => a.into(),
    }
}

fn arch_ok(kind: Kind, arch: &str) -> bool {
    arch.is_empty() || ["all", "noarch", "any"].contains(&arch) || arch == machine(kind)
}

fn arch_words(arch: &str) -> &'static str {
    match arch {
        "arm64" | "aarch64" => "ARM computers (Raspberry Pi, Snapdragon laptops)",
        "i386" | "i686" | "x86" => "old 32-bit PCs",
        "armhf" | "armv7hl" => "32-bit ARM boards",
        _ => "a different kind of processor",
    }
}

/// Whether this package installs here: the right family (dpkg vs rpm) and processor.
pub(super) fn fit(kind: Kind, pm: Pm, arch: &str, name: &str) -> Fit {
    let os = super::os_name();
    let (made_for, ours) = match kind {
        Kind::Deb => ("Debian and Ubuntu", pm == Pm::Apt),
        _ => ("Fedora and openSUSE", matches!(pm, Pm::Dnf | Pm::Zypper)),
    };
    if !ours {
        let native = match pm {
            Pm::Apt => Some(".deb"),
            Pm::Dnf | Pm::Zypper => Some(".rpm"),
            Pm::Unknown => None,
        };
        let why = match native {
            Some(n) => format!(
                "This package is made for {made_for}. {os} installs {n} packages. Look for a {n} or an AppImage of {name} on the same download page."
            ),
            None => format!("This package is made for {made_for}, and {os} doesn't install it. Look for an AppImage of {name}."),
        };
        return Fit::No {
            made_for: made_for.into(),
            why,
        };
    }
    if !arch_ok(kind, arch) {
        return Fit::No {
            made_for: arch_words(arch).into(),
            why: format!(
                "This package is made for {}, not this computer ({}). Look for an {} version of {name}.",
                arch_words(arch),
                machine(kind),
                machine(kind)
            ),
        };
    }
    Fit::Ok
}

/// Unpack the package's launchers and icons into `out` (sandboxed: a hostile package can't
/// write anywhere else). Patterns that match nothing are fine.
fn unpack_launchers(kind: Kind, path: &Path, out: &Path) {
    let pats = "'*.desktop' './usr/share/icons/*/apps/*' './usr/share/pixmaps/*' './opt/*.png' './opt/*.svg'";
    let script = match kind {
        Kind::Deb if has("dpkg-deb") => format!(
            "dpkg-deb --fsys-tarfile \"$1\" | tar -x -C \"$2\" --no-same-owner --no-same-permissions --wildcards {pats} 2>/dev/null; exit 0"
        ),
        Kind::Rpm if has("rpm2cpio") && has("cpio") => {
            format!("cd \"$2\" && rpm2cpio \"$1\" | cpio -id --quiet --no-absolute-filenames {pats} 2>/dev/null; exit 0")
        }
        Kind::Rpm if has("bsdtar") => format!("bsdtar -x -f \"$1\" -C \"$2\" {pats} 2>/dev/null; exit 0"),
        _ => return,
    };
    let _ = crate::archive::sandboxed("sh", path, out)
        .args(["-c", &script, "sh"])
        .arg(path)
        .arg(out)
        .output();
}

pub(super) fn read(kind: Kind, path: &Path, pm: Pm) -> Option<Pkg> {
    let file = path.file_name()?.to_string_lossy().into_owned();
    let (id, version, arch, size_bytes, summary) = match kind {
        Kind::Deb => match run(Command::new("dpkg-deb")
            .args(["--show", "--showformat=${Package}\\n${Version}\\n${Architecture}\\n${Installed-Size}\\n${binary:Summary}\\n"])
            .arg(path))
        {
            Some(t) => {
                let mut l = t.lines().map(str::to_string);
                let (p, v, a) = (l.next()?, l.next()?, l.next()?);
                let kib: u64 = l.next().and_then(|s| s.parse().ok()).unwrap_or(0);
                (p, v, a, kib * 1024, l.next().unwrap_or_default())
            }
            None if has("dpkg-deb") => return None, // not a .deb
            None => {
                let (p, v, a) = deb_from_name(&file)?;
                (p, v, a, 0, String::new())
            }
        },
        _ => match run(Command::new("rpm")
            .args(["-qp", "--nosignature", "--nodigest", "--qf", "%{NAME}\\n%{VERSION}-%{RELEASE}\\n%{ARCH}\\n%{SIZE}\\n%{SUMMARY}\\n"])
            .arg(path))
        {
            Some(t) => {
                let mut l = t.lines().map(str::to_string);
                let (p, v, a) = (l.next()?, l.next()?, l.next()?);
                let size = l.next().and_then(|s| s.parse().ok()).unwrap_or(0);
                (p, v, a, size, l.next().unwrap_or_default())
            }
            None if has("rpm") => return None,
            None => {
                let (p, v, a) = rpm_from_name(&file)?;
                (p, v, a, 0, String::new())
            }
        },
    };
    let scratch = Scratch::new().ok()?;
    let tree = scratch.0.join("t");
    let _ = std::fs::create_dir_all(&tree);
    unpack_launchers(kind, path, &tree);
    let launcher = super::find_launcher(&tree);
    let name = launcher.as_ref().map(|l| l.name.clone()).filter(|n| !n.is_empty()).unwrap_or_else(|| id.clone());
    let icon = launcher.as_ref().and_then(|l| super::resolve_icon(&tree, &l.icon));
    let fit = fit(kind, pm, &arch, &name);
    let installed = if fit != Fit::Ok {
        None
    } else if kind == Kind::Deb {
        run(Command::new("dpkg-query").args(["-W", "-f=${db:Status-Status}\\t${Version}", &id]))
            .and_then(|s| s.strip_prefix("installed\t").map(str::to_string))
    } else {
        run(Command::new("rpm").args(["-q", "--qf", "%{VERSION}-%{RELEASE}", &id]))
    };
    let needs = (kind == Kind::Deb && fit == Fit::Ok)
        .then(|| {
            // a dry run, as yourself: what else apt would fetch
            let abs = path.canonicalize().ok()?;
            let t = run(Command::new("apt-get").args(["-s", "install"]).arg(&abs))?;
            Some(t.lines().filter(|l| l.starts_with("Inst ") && l.split_whitespace().nth(1) != Some(id.as_str())).count())
        })
        .flatten();
    Some(Pkg {
        kind,
        path: path.to_path_buf(),
        summary: if summary.is_empty() {
            launcher.as_ref().map(|l| l.comment.clone()).unwrap_or_default()
        } else {
            summary
        },
        id,
        name,
        version,
        size: size_bytes,
        icon,
        installed,
        fit,
        origin: super::origin(path),
        needs,
        appimage: None,
        _scratch: scratch,
    })
}

/// The command that installs it, run under pkexec.
pub(super) fn install_argv(p: &Pkg, pm: Pm) -> Vec<std::ffi::OsString> {
    let same = p.installed.as_deref() == Some(p.version.as_str());
    let path = p.path.canonicalize().unwrap_or_else(|_| p.path.clone()); // pkexec runs elsewhere
    let mut v: Vec<std::ffi::OsString> = match pm {
        Pm::Apt => {
            let mut v: Vec<_> = ["pkexec", "env", "DEBIAN_FRONTEND=noninteractive", "apt-get", "install", "-y", "--allow-downgrades"]
                .iter()
                .map(Into::into)
                .collect();
            if same {
                v.push("--reinstall".into());
            }
            v
        }
        Pm::Dnf => ["pkexec", "dnf", if same { "reinstall" } else { "install" }, "-y"].iter().map(Into::into).collect(),
        _ => {
            let mut v: Vec<_> = ["pkexec", "zypper", "--non-interactive", "install", "--allow-unsigned-rpm"]
                .iter()
                .map(Into::into)
                .collect();
            if same {
                v.push("--force".into());
            }
            v
        }
    };
    v.push(path.into_os_string());
    v
}

/// The launchers the package installed (it must be installed by now).
pub(super) fn launchers(p: &Pkg) -> Vec<std::path::PathBuf> {
    let files = match p.kind {
        Kind::Deb => run(Command::new("dpkg-query").args(["-L", &p.id])),
        _ => run(Command::new("rpm").args(["-ql", &p.id])),
    };
    super::installed_launchers(&files.unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_fit() {
        assert_eq!(
            rpm_from_name("kestrel-0.9.40-1.x86_64.rpm"),
            Some(("kestrel".into(), "0.9.40-1".into(), "x86_64".into()))
        );
        assert_eq!(
            rpm_from_name("google-chrome-stable-131.0.1-1.x86_64.rpm").unwrap().0,
            "google-chrome-stable"
        );
        assert_eq!(deb_from_name("kestrel_0.9.40-1_amd64.deb"), Some(("kestrel".into(), "0.9.40-1".into(), "amd64".into())));
        assert!(matches!(fit(Kind::Rpm, Pm::Apt, "x86_64", "X"), Fit::No { made_for, why } if made_for == "Fedora and openSUSE" && why.contains(".deb")));
        assert!(matches!(fit(Kind::Deb, Pm::Dnf, "amd64", "X"), Fit::No { why, .. } if why.contains(".rpm")));
        if std::env::consts::ARCH == "x86_64" {
            assert_eq!(fit(Kind::Deb, Pm::Apt, "amd64", "X"), Fit::Ok);
            assert_eq!(fit(Kind::Deb, Pm::Apt, "all", "X"), Fit::Ok);
            assert_eq!(fit(Kind::Rpm, Pm::Dnf, "noarch", "X"), Fit::Ok);
            assert!(matches!(fit(Kind::Deb, Pm::Apt, "arm64", "X"), Fit::No { why, .. } if why.contains("ARM")));
        }
    }

    #[test]
    fn reads_a_built_deb_with_its_launcher_and_icon() {
        if !has("dpkg-deb") || !has("bwrap") {
            return;
        }
        let s = Scratch::new().unwrap();
        let root = s.0.join("pkg");
        let w = |p: &str, body: &[u8]| {
            let p = root.join(p);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        };
        w(
            "DEBIAN/control",
            b"Package: kestrel-test-pkg\nVersion: 1.2-3\nArchitecture: all\nMaintainer: t <t@t>\nInstalled-Size: 4\nDescription: a test package\n longer text\n",
        );
        w("usr/share/applications/kt.desktop", b"[Desktop Entry]\nType=Application\nName=Kestrel Test\nIcon=kt\n");
        w("usr/share/icons/hicolor/scalable/apps/kt.svg", b"<svg xmlns='http://www.w3.org/2000/svg'/>");
        let deb = s.0.join("t.deb");
        assert!(Command::new("dpkg-deb").args(["--build", "--root-owner-group"]).arg(&root).arg(&deb).output().unwrap().status.success());
        let d = read(Kind::Deb, &deb, Pm::Apt).unwrap();
        assert_eq!((d.id.as_str(), d.version.as_str(), d.size, d.name.as_str()), ("kestrel-test-pkg", "1.2-3", 4096, "Kestrel Test"));
        assert_eq!(d.summary, "a test package");
        assert_eq!(d.installed, None);
        assert!(d.icon.as_ref().unwrap().ends_with("scalable/apps/kt.svg"), "{:?}", d.icon);
        assert!(read(Kind::Deb, &root.join("DEBIAN/control"), Pm::Apt).is_none());
        let argv = install_argv(&d, Pm::Apt);
        assert_eq!(argv[..5], ["pkexec", "env", "DEBIAN_FRONTEND=noninteractive", "apt-get", "install"].map(std::ffi::OsString::from));
        assert!(argv.last().unwrap().to_string_lossy().ends_with("/t.deb"));
    }
}
