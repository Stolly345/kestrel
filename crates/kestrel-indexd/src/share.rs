//! `kestrel-indexd --add-share` / `--remove-share MOUNTPOINT`, run by the app through pkexec (the
//! Network page's Index… and Stop Indexing). Adding mounts an SMB share from /etc/fstab at
//! /mnt/SERVER-SHARE on first use, opted in to indexing with `x-kestrel`; the password goes in a
//! root-only file under /etc/kestrel/shares, never in fstab. A test mount runs first, so a wrong
//! password changes nothing. Only lines this adds are ever removed.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

const FSTAB: &str = "/etc/fstab";
const CRED_DIR: &str = "/etc/kestrel/shares";
const MARK: &str = "# Added by Kestrel (Network page); remove it there with Stop Indexing";

/// What the app sends on stdin, one per line: server, share, user, password. The caller's uid comes
/// from pkexec (PKEXEC_UID), never from the request.
#[derive(Debug, PartialEq)]
struct Req {
    server: String,
    share: String,
    user: String,
    password: String,
}

fn parse(input: &str) -> Result<Req, String> {
    let mut l = input.split('\n');
    let mut next = || l.next().unwrap_or("").to_string();
    let r = Req { server: next(), share: next(), user: next(), password: next() };
    // a host name, IPv4 address or bracketed IPv6 address
    if r.server.is_empty() || !r.server.chars().all(|c| c.is_ascii_alphanumeric() || ".-:[]".contains(c)) {
        return Err(format!("not a server name: {:?}", r.server));
    }
    if r.share.is_empty() || r.share.chars().any(|c| c.is_control() || "/\\".contains(c)) {
        return Err(format!("not a share name: {:?}", r.share));
    }
    // the credentials file is one key=value per line
    if r.user.chars().any(char::is_control) || r.password.contains(['\n', '\r', '\0']) {
        return Err("user name or password contains a line break".into());
    }
    Ok(r)
}

/// "mycloudex2ultra.local", "alex" -> "mycloudex2ultra-alex"
fn slug(r: &Req) -> String {
    let host = r.server.trim_end_matches(".local");
    let s: String = format!("{host}-{}", r.share)
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '.' { c } else { '-' })
        .collect();
    s.trim_matches('-').to_string()
}

/// fstab escapes whitespace and backslashes as `\ooo`.
fn fstab_escape(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            ' ' | '\t' | '\\' => format!("\\{:03o}", c as u32),
            c => c.to_string(),
        })
        .collect()
}

fn fstab_line(r: &Req, mp: &Path, cred: &Path, uid: u32, gid: u32) -> String {
    format!(
        "{} {} cifs credentials={},uid={uid},gid={gid},dir_mode=0700,file_mode=0600,soft,noauto,x-systemd.automount,x-systemd.idle-timeout=30min,x-systemd.mount-timeout=10,_netdev,nofail,x-kestrel 0 0",
        fstab_escape(&format!("//{}/{}", r.server, r.share)),
        fstab_escape(&mp.to_string_lossy()),
        cred.display()
    )
}

/// The fstab without the line this added for `mp` (and its comment); None if it has no such line.
fn without(fstab: &str, mp: &Path) -> Option<String> {
    let want = fstab_escape(&mp.to_string_lossy());
    let ours = |l: &str| {
        let f: Vec<&str> = l.split_whitespace().collect();
        f.len() >= 4 && f[1] == want && f[3].contains(&format!("credentials={CRED_DIR}/")) && f[3].split(',').any(|o| o == "x-kestrel")
    };
    let lines: Vec<&str> = fstab.lines().collect();
    let i = lines.iter().position(|l| ours(l))?;
    let mut out: Vec<&str> = Vec::new();
    for (j, l) in lines.iter().enumerate() {
        if j == i || (j + 1 == i && *l == MARK) {
            continue;
        }
        out.push(l);
    }
    Some(out.join("\n") + "\n")
}

/// Replace /etc/fstab in one rename, keeping the previous one as /etc/fstab.kestrel-bak.
fn write_fstab(text: &str) -> Result<(), String> {
    let _ = std::fs::copy(FSTAB, "/etc/fstab.kestrel-bak");
    let tmp = format!("{FSTAB}.kestrel-new");
    std::fs::write(&tmp, text).and_then(|()| std::fs::rename(&tmp, FSTAB)).map_err(|e| format!("couldn't write {FSTAB}: {e}"))
}

/// mount.cifs's "mount error(13): Permission denied" in words; its last line is a generic pointer
/// to the manual.
fn mount_error(stderr: &str) -> String {
    let code = stderr.split("mount error(").nth(1).and_then(|r| r.split(')').next()).and_then(|c| c.parse::<i32>().ok());
    match code {
        Some(13) => "the user name or password is wrong".into(),
        Some(2) => "there's no share by that name".into(),
        Some(110 | 112 | 113 | 115) | Some(101) => "the server can't be reached".into(),
        _ => stderr.lines().find(|l| l.contains("mount error")).unwrap_or(stderr.trim()).to_string(),
    }
}

fn run(args: &[&str]) -> Result<String, String> {
    let o = Command::new(args[0]).args(&args[1..]).output().map_err(|e| format!("{}: {e}", args[0]))?;
    if o.status.success() {
        Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
    } else {
        let err = String::from_utf8_lossy(&o.stderr);
        Err(err.trim().lines().last().unwrap_or("failed").to_string())
    }
}

fn automount_unit(mp: &Path) -> Result<String, String> {
    run(&["systemd-escape", "--path", "--suffix=automount", &mp.to_string_lossy()])
}

/// The asking user, from pkexec.
fn caller() -> Result<(u32, u32), String> {
    let uid: u32 = std::env::var("PKEXEC_UID").ok().and_then(|u| u.parse().ok()).ok_or("run this through pkexec")?;
    let pw = unsafe { libc::getpwuid(uid) };
    if pw.is_null() {
        return Err(format!("no user {uid}"));
    }
    Ok((uid, unsafe { (*pw).pw_gid }))
}

fn add() -> Result<PathBuf, String> {
    let (uid, gid) = caller()?;
    let mut input = String::new();
    std::io::stdin().take(64 * 1024).read_to_string(&mut input).map_err(|e| e.to_string())?;
    let r = parse(&input)?;
    if !["/sbin/mount.cifs", "/usr/sbin/mount.cifs"].iter().any(|p| Path::new(p).exists()) {
        return Err("SMB support isn't installed: install the cifs-utils package".into());
    }
    let name = slug(&r);
    let mp = PathBuf::from("/mnt").join(&name);
    let fstab = std::fs::read_to_string(FSTAB).map_err(|e| format!("{FSTAB}: {e}"))?;
    let esc = fstab_escape(&mp.to_string_lossy());
    if fstab.lines().any(|l| l.split_whitespace().nth(1) == Some(esc.as_str())) {
        return Err(format!("{} is already in {FSTAB}", mp.display()));
    }
    std::fs::create_dir_all(CRED_DIR).map_err(|e| e.to_string())?;
    let _ = std::fs::set_permissions(CRED_DIR, std::os::unix::fs::PermissionsExt::from_mode(0o700));
    let cred = Path::new(CRED_DIR).join(format!("{name}.cred"));
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&cred).map_err(|e| e.to_string())?;
        writeln!(f, "username={}\npassword={}", r.user, r.password).map_err(|e| e.to_string())?;
    }
    // try it before touching fstab: a wrong password or share name ends here, with mount's message
    let probe = PathBuf::from(format!("/run/kestrel-probe-{}", std::process::id()));
    std::fs::create_dir_all(&probe).map_err(|e| e.to_string())?;
    let opts = format!("credentials={},uid={uid},gid={gid},soft", cred.display());
    let tried = Command::new("mount")
        .args(["-t", "cifs", &format!("//{}/{}", r.server, r.share), &probe.to_string_lossy(), "-o", &opts])
        .output()
        .map_err(|e| e.to_string())
        .and_then(|o| if o.status.success() { Ok(()) } else { Err(mount_error(&String::from_utf8_lossy(&o.stderr))) });
    if tried.is_ok() {
        let _ = run(&["umount", &probe.to_string_lossy()]);
    }
    let _ = std::fs::remove_dir(&probe);
    if let Err(e) = tried {
        let _ = std::fs::remove_file(&cred);
        return Err(format!("Couldn't connect to //{}/{}: {e}.", r.server, r.share));
    }
    std::fs::create_dir_all(&mp).map_err(|e| e.to_string())?;
    let mut text = fstab;
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text += &format!("{MARK}\n{}\n", fstab_line(&r, &mp, &cred, uid, gid));
    write_fstab(&text)?;
    run(&["systemctl", "daemon-reload"])?;
    run(&["systemctl", "start", &automount_unit(&mp)?])?;
    // list it now rather than within 3 hours (fine if the service isn't running)
    let _ = run(&["systemctl", "reload", "kestrel-indexd"]);
    Ok(mp)
}

fn remove(mp: &Path) -> Result<(), String> {
    caller()?;
    let fstab = std::fs::read_to_string(FSTAB).map_err(|e| format!("{FSTAB}: {e}"))?;
    let Some(text) = without(&fstab, mp) else {
        return Err(format!("{} wasn't added by Kestrel", mp.display()));
    };
    let cred = fstab
        .lines()
        .find(|l| l.split_whitespace().nth(1) == Some(fstab_escape(&mp.to_string_lossy()).as_str()))
        .and_then(|l| l.split_whitespace().nth(3))
        .and_then(|o| o.split(',').find_map(|o| o.strip_prefix("credentials=")))
        .map(PathBuf::from);
    // fstab first: the index service drops the share when it sees it unmounted and no longer listed
    write_fstab(&text)?;
    let unit = automount_unit(mp)?;
    let _ = run(&["systemctl", "stop", &unit, &unit.replace(".automount", ".mount")]);
    run(&["systemctl", "daemon-reload"])?;
    if let Some(c) = cred.filter(|c| c.starts_with(CRED_DIR)) {
        let _ = std::fs::remove_file(c);
    }
    let _ = std::fs::remove_dir(mp);
    let _ = run(&["systemctl", "reload", "kestrel-indexd"]);
    Ok(())
}

/// Handle `--add-share` / `--remove-share MP` and exit; anything else returns (a normal start).
pub fn command(args: &[String]) {
    let res = match args.first().map(String::as_str) {
        Some("--add-share") => add().map(|mp| println!("{}", mp.display())),
        Some("--remove-share") => match args.get(1) {
            Some(mp) => remove(Path::new(mp)),
            None => Err("--remove-share needs the mount point".into()),
        },
        _ => return,
    };
    if let Err(e) = res {
        eprintln!("{e}");
        std::process::exit(1);
    }
    std::process::exit(0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_are_checked() {
        let r = parse("mycloudex2ultra.local\nMy Share\nalex\np@ss word!?\n").unwrap();
        assert_eq!(slug(&r), "mycloudex2ultra-my-share");
        assert!(parse("nas;rm -rf /\nx\nu\np").is_err(), "server");
        assert!(parse("nas\n../etc\nu\np").is_err(), "share with a slash");
        assert!(parse("nas\nx\nu\x07\np").is_err(), "control char in user");
        assert!(parse("\nx\nu\np").is_err(), "empty server");
    }

    #[test]
    fn mount_errors_in_words() {
        let real = "mount error(13): Permission denied\nRefer to the mount.cifs(8) manual page (e.g. man mount.cifs) and kernel log messages (dmesg)\n";
        assert_eq!(mount_error(real), "the user name or password is wrong");
        assert_eq!(mount_error("mount error(2): No such file or directory\nRefer…"), "there's no share by that name");
        assert_eq!(mount_error("mount error(95): Operation not supported\nRefer…"), "mount error(95): Operation not supported");
    }

    #[test]
    fn fstab_round_trip() {
        let r = parse("nas.local\nMy Share\nalex\npw").unwrap();
        let mp = PathBuf::from("/mnt").join(slug(&r));
        let line = fstab_line(&r, &mp, Path::new("/etc/kestrel/shares/nas-my-share.cred"), 1000, 1000);
        assert!(line.starts_with("//nas.local/My\\040Share /mnt/nas-my-share cifs credentials=/etc/kestrel/shares/nas-my-share.cred,uid=1000,"));
        assert!(line.contains(",x-kestrel "));
        let before = "UUID=1 / ext4 defaults 0 1\n//other/x /mnt/nas-my-share-2 cifs x-kestrel 0 0\n";
        let after = format!("{before}{MARK}\n{line}\n");
        assert_eq!(without(&after, &mp).as_deref(), Some(before), "removes exactly its line and comment");
        assert_eq!(without(before, Path::new("/mnt/nas-my-share-2")), None, "never a line it didn't add");
    }
}
