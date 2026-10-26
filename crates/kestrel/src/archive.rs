//! The archive engine: which files Kestrel extracts, the bubblewrap sandbox every extractor and
//! document renderer runs in, split-archive volumes, listing an archive's contents, and running
//! 7-Zip / tar with progress, passwords and Cancel. The Extract/Compress UI is in fileops.rs.

use crate::fileops::Progress;
use gtk::prelude::*;
use gtk::{gio, glib};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

/// Run `wait` (which waits for process `pid`) with the Cancel button able to kill it. SIGKILL
/// to the outer bwrap takes the sandbox down with it (--die-with-parent).
pub(crate) fn killable<T>(p: &Progress, pid: u32, wait: impl FnOnce() -> T) -> T {
    p.can_cancel.store(true, Ordering::Relaxed);
    let id = p.cancel.connect_cancelled(move |_| unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGKILL);
    });
    let r = wait();
    if let Some(id) = id {
        p.cancel.disconnect_cancelled(id);
    }
    p.can_cancel.store(false, Ordering::Relaxed);
    r
}

/// Every extension Kestrel offers Extract on: what the bundled 7-Zip reads (`7zz i`) that is
/// really an archive, package or disk image. Documents and programs 7-Zip can also open (.docx,
/// .exe, .dll, ELF) are left out so Extract isn't offered on every Word file. Compound
/// extensions (.tar.gz) must come before their tail (.gz): the first match names the output folder.
/// Keep in sync with docs/extract-formats.md.
pub const ARCHIVE_EXTS: &[&str] = &[
    // tarballs (system tar, sandboxed)
    ".tar.gz",
    ".tar.xz",
    ".tar.zst",
    ".tar.bz2",
    ".tar.z",
    ".tgz",
    ".tpz",
    ".txz",
    ".tzst",
    ".tbz2",
    ".tbz",
    ".taz",
    ".tar",
    ".ova",
    // archives
    ".7z",
    ".zip",
    ".zipx",
    ".rar",
    ".cab",
    ".arj",
    ".lzh",
    ".lha",
    ".cpio",
    ".xar",
    ".wim",
    ".swm",
    ".esd",
    ".chm",
    ".z01",
    ".001",
    ".r00",
    // single-file compression
    ".gz",
    ".gzip",
    ".xz",
    ".bz2",
    ".bzip2",
    ".zst",
    ".lzma",
    ".z",
    // books and comics (zip, or RAR/7z/tar for .cbr/.cb7/.cbt)
    ".epub",
    ".cbz",
    ".cbr",
    ".cb7",
    ".cbt",
    // packages and installers
    ".deb",
    ".udeb",
    ".rpm",
    ".msi",
    ".msp",
    ".apk",
    ".jar",
    ".war",
    ".whl",
    ".xpi",
    ".crx",
    ".ipa",
    ".appx",
    ".msix",
    ".pkg",
    ".ear",
    ".nupkg",
    ".gem",
    ".ipk",
    ".snap",
    ".appimage",
    ".a",
    // disk and file-system images
    ".iso",
    ".img",
    ".dmg",
    ".udf",
    ".vhd",
    ".vhdx",
    ".vmdk",
    ".vdi",
    ".qcow",
    ".qcow2",
    ".squashfs",
    ".cramfs",
    ".hfs",
    ".hfsx",
    ".simg",
];

/// The inside of an archive as a folder (GVfs' archive backend mounts it; Browser::show
/// handles the mount).
/// GVfs names an archive mount by its file URI escaped twice (`file%253A…`, as Nautilus builds it);
/// escaped once, the mount succeeds but the folder is then "not mounted".
pub fn archive_root(f: &gio::File) -> gio::File {
    let once = glib::Uri::escape_string(&f.uri(), None, false);
    gio::File::for_uri(&format!("archive://{}/", glib::Uri::escape_string(&once, None, false)))
}

/// The archive file an `archive://` folder lives in (any folder inside it), for naming the tab.
pub fn archive_source(f: &gio::File) -> Option<gio::File> {
    let host = f.uri().strip_prefix("archive://")?.split('/').next()?.to_string();
    let once = glib::Uri::unescape_string(&host, None::<&str>)?;
    Some(gio::File::for_uri(&glib::Uri::unescape_string(&once, None::<&str>)?))
}

/// Double-click opens these as folders. Packages, books and disk images still open in their
/// apps (software installer, e-book reader, Disks); Browse Archive works on them too.
/// What GNOME's archive backend (gvfsd-archive, on libarchive) can't open, from mounting every
/// sample: 7z needs seeking it doesn't do, single streams have no listing, the rest aren't
/// libarchive formats. These still extract and preview; they just can't be browsed as folders.
const NO_GVFS: &[&str] = &[
    ".7z",
    ".cb7",
    ".snap",
    ".appimage",
    ".arj",
    ".chm",
    ".cramfs",
    ".crx",
    ".esd",
    ".msi",
    ".msp",
    ".qcow",
    ".qcow2",
    ".rpm",
    ".simg",
    ".squashfs",
    ".swm",
    ".vdi",
    ".vhd",
    ".vhdx",
    ".vmdk",
    ".wim",
    ".gz",
    ".gzip",
    ".xz",
    ".bz2",
    ".bzip2",
    ".zst",
    ".lzma",
    ".z",
];

/// Whether Browse Archive can open it as a folder.
pub fn browsable(name: &str) -> bool {
    archive_ext(name).is_some_and(|e| !NO_GVFS.contains(&e))
}

pub fn browse_on_open(name: &str) -> bool {
    const IN_APP: &[&str] = &[
        ".deb",
        ".udeb",
        ".rpm",
        ".msi",
        ".msp",
        ".apk",
        ".jar",
        ".war",
        ".whl",
        ".xpi",
        ".crx",
        ".ipa",
        ".appx",
        ".msix",
        ".pkg",
        ".ear",
        ".nupkg",
        ".gem",
        ".ipk",
        ".snap",
        ".appimage",
        ".a",
        ".epub",
        ".cbz",
        ".cbr",
        ".cb7",
        ".cbt",
        ".chm",
        ".ova",
    ];
    browsable(name)
        && archive_ext(name).is_some_and(|e| {
            let lower = name.to_lowercase();
            let image = ARCHIVE_EXTS.iter().position(|x| *x == ".iso").is_some_and(|i| ARCHIVE_EXTS[i..].contains(&e));
            !image && !IN_APP.iter().any(|x| lower.ends_with(x))
        })
}

/// The `ARCHIVE_EXTS` entry `name` ends with, if any.
pub fn archive_ext(name: &str) -> Option<&'static str> {
    let lower = name.to_lowercase();
    ARCHIVE_EXTS.iter().copied().find(|e| lower.ends_with(e))
}

pub(crate) fn is_tarball(lower: &str) -> bool {
    [
        ".tar", ".tar.gz", ".tgz", ".tpz", ".tar.xz", ".txz", ".tar.zst", ".tzst", ".tar.bz2", ".tbz2", ".tbz", ".tar.z", ".taz", ".ova",
    ]
    .iter()
    .any(|e| lower.ends_with(e))
}

/// Run a command; on failure return its last stderr line.
pub(crate) fn run(mut cmd: std::process::Command) -> Result<(), String> {
    let out = cmd.stdin(std::process::Stdio::null()).output().map_err(|e| e.to_string())?;
    check(out)
}

fn check(out: std::process::Output) -> Result<(), String> {
    if out.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("failed").trim().to_string())
    }
}

/// The 7-Zip bundled in the .deb. Ubuntu's 7zip package lags months behind on decoder CVEs and
/// is built without RAR. `KESTREL_7ZZ` points a dev build at packaging/7zip/7zz.
pub(crate) fn seven_zip() -> String {
    std::env::var("KESTREL_7ZZ").unwrap_or_else(|_| "/usr/lib/kestrel/7zz".into())
}

/// `prog` in a bubblewrap sandbox: no network, a read-only system, home/tmp/run/removable
/// media hidden, only `archive` (read-only) and `out` visible. 7-Zip fixes several
/// code-execution bugs in its decoders a year; a hostile archive can at worst write junk into
/// `out`, and no entry can land outside it (the kernel enforces what a zip-slip check would).
pub(crate) fn sandboxed(prog: &str, archive: &Path, out: &Path) -> std::process::Command {
    let mut c = std::process::Command::new("bwrap");
    c.args(["--ro-bind", "/", "/", "--dev", "/dev", "--proc", "/proc"]);
    for hide in ["/home", "/root", "/tmp", "/var/tmp", "/run", "/media", "/mnt", "/srv"] {
        if Path::new(hide).is_dir() {
            c.args(["--tmpfs", hide]);
        }
    }
    let p = Path::new(prog);
    if p.is_absolute() {
        c.arg("--ro-bind").arg(p).arg(p);
    }
    // every volume of a split archive, or 7-Zip sees only the first part
    for v in volumes(archive) {
        c.arg("--ro-bind").arg(&v).arg(&v);
    }
    c.arg("--bind").arg(out).arg(out);
    c.args(["--unshare-all", "--die-with-parent", "--new-session", "--", prog]);
    c
}

/// A split archive's name without its volume suffix ("v.7z" for v.7z.003, "x" for x.part2.rar,
/// x.r00, x.z01, x.zip), or None if `name` doesn't look like a volume.
fn volume_stem(name: &str) -> Option<&str> {
    let (stem, ext) = name.rsplit_once('.')?;
    let ext = ext.to_ascii_lowercase();
    let numbered = |p: &str| ext.strip_prefix(p).is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
    if ext == "rar" {
        // x.part2.rar → x
        return Some(match stem.rsplit_once('.') {
            Some((s, part)) if part.len() > 4 && part[..4].eq_ignore_ascii_case("part") && part[4..].bytes().all(|b| b.is_ascii_digit()) => s,
            _ => stem,
        });
    }
    (numbered("") || numbered("z") || numbered("r") || ext == "zip" || ext == "zipx").then_some(stem)
}

/// `archive` plus the other volumes next to it (same stem, any volume suffix). Just `archive`
/// for an ordinary file.
pub fn volumes(archive: &Path) -> Vec<PathBuf> {
    let mut all = vec![archive.to_path_buf()];
    let (Some(name), Some(dir)) = (archive.file_name().and_then(|n| n.to_str()), archive.parent()) else {
        return all;
    };
    let Some(stem) = volume_stem(name) else { return all };
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let n = e.file_name();
            let Some(n) = n.to_str() else { continue };
            if n != name && n.len() > stem.len() && n.starts_with(stem) && n.as_bytes()[stem.len()] == b'.' && volume_stem(n) == Some(stem) {
                all.push(dir.join(n));
            }
        }
    }
    all
}

/// The volume 7-Zip must be pointed at: old-style RAR sets (x.rar, x.r00, x.r01…) start at x.rar.
pub fn first_volume(archive: &Path) -> PathBuf {
    let name = archive.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    if let Some((stem, ext)) = name.rsplit_once('.')
        && ext.len() > 1
        && ext.as_bytes()[0].eq_ignore_ascii_case(&b'r')
        && ext[1..].bytes().all(|b| b.is_ascii_digit())
    {
        let first = archive.with_file_name(format!("{stem}.rar"));
        if first.exists() {
            return first;
        }
    }
    archive.to_path_buf()
}

/// One entry of an archive listing.
pub struct Entry {
    pub path: String,
    pub size: u64,
    pub dir: bool,
}

/// What's inside `archive` (canonical path), listed in the sandbox; `scratch` is the empty
/// writable dir the sandbox needs. Tarballs go through tar (7-Zip would show the one .tar inside).
pub fn list_archive(archive: &Path, scratch: &Path) -> Result<Vec<Entry>, String> {
    let archive = &first_volume(archive);
    let name = archive.file_name().unwrap_or_default().to_string_lossy().to_lowercase();
    let tar = is_tarball(&name);
    let mut c = if tar {
        let mut c = sandboxed("tar", archive, scratch);
        c.arg("-tvf").arg(archive);
        c
    } else {
        let mut c = sandboxed(&seven_zip(), archive, scratch);
        c.args(["l", "-slt", "-p", "--"]).arg(archive);
        c
    };
    let out = c.stdin(std::process::Stdio::null()).output().map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() {
        let msg = format!("{text}{}", String::from_utf8_lossy(&out.stderr)).to_lowercase();
        return Err(if msg.contains("encrypted") || msg.contains("password") {
            "Encrypted archive".into()
        } else {
            "Couldn't read this archive".into()
        });
    }
    Ok(if tar { parse_tar_tv(&text) } else { parse_7z_slt(&text) })
}

/// `tar -tv` lines: "drwxr-xr-x user/group 0 2026-10-26 08:25 d/sub/".
fn parse_tar_tv(text: &str) -> Vec<Entry> {
    text.lines()
        .filter_map(|l| {
            let mut rest = l;
            let mut tok = Vec::new();
            for _ in 0..5 {
                rest = rest.trim_start();
                let end = rest.find(char::is_whitespace)?;
                tok.push(&rest[..end]);
                rest = &rest[end..];
            }
            let path = rest.trim_start().split(" -> ").next()?.trim_end_matches('/');
            Some(Entry {
                path: path.into(),
                size: tok[2].parse().unwrap_or(0),
                dir: tok[0].starts_with('d'),
            })
        })
        .filter(|e| !e.path.is_empty())
        .collect()
}

/// `7zz l -slt` blocks ("Path = …", "Folder = +", "Size = …") after the "----------" line.
/// A single compressed stream (.xz, .lzma, a .deb's data.tar.xz…) has no Path: 7-Zip names it
/// after the innermost layer, minus its extension ("data.tar.xz" → "data.tar"), as Extract does.
/// Some formats (XAR) don't mark folders: an entry with entries under it is one.
fn parse_7z_slt(text: &str) -> Vec<Entry> {
    let Some((head, body)) = text.split_once("\n----------\n") else {
        return Vec::new();
    };
    let layer = head.lines().filter_map(|l| l.strip_prefix("Path = ")).next_back().unwrap_or_default();
    let layer = Path::new(layer).file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned());
    let unnamed = layer.rsplit_once('.').map_or(layer.as_str(), |(stem, _)| stem).to_string();
    let blocks: Vec<&str> = body.split("\n\n").filter(|b| b.contains(" = ")).collect();
    // only a lone entry is an unnamed stream; disk images add pathless info blocks after theirs
    let lone = blocks.len() == 1;
    let mut entries: Vec<Entry> = blocks
        .into_iter()
        .filter_map(|block| {
            let field = |k: &str| block.lines().find_map(|l| l.strip_prefix(k)?.strip_prefix(" = "));
            let path = match field("Path") {
                Some(p) => p.to_string(),
                None if lone => unnamed.clone(),
                None => return None,
            };
            Some(Entry {
                path,
                size: field("Size").and_then(|s| s.parse().ok()).unwrap_or(0),
                dir: field("Folder") == Some("+") || field("Attributes").is_some_and(|a| a.starts_with('D')),
            })
        })
        .collect();
    let parents: std::collections::HashSet<String> = entries.iter().filter_map(|e| e.path.rsplit_once('/').map(|(d, _)| d.to_string())).collect();
    for e in &mut entries {
        e.dir |= parents.contains(&e.path);
    }
    entries
}

/// Why extracting failed: a missing/wrong password (ask for one), the Cancel button, or anything else.
pub(crate) enum ExtractErr {
    Password,
    Cancelled,
    Other(String),
}

/// Extract `archive` (canonical, first volume) into the existing folder `out`, in the sandbox:
/// tarballs with tar, everything else with the bundled 7-Zip. Returns how many unsafe links
/// were skipped. This is all of Extract Here except naming and placing the result.
pub(crate) fn unpack(archive: &Path, out: &Path, pw: Option<&str>, p: &Progress) -> Result<usize, ExtractErr> {
    let name = archive.file_name().unwrap_or_default().to_string_lossy().to_lowercase();
    if !is_tarball(&name) {
        return run_7z(archive, out, pw, p);
    }
    let other = |e: std::io::Error| ExtractErr::Other(e.to_string());
    let mut c = sandboxed("tar", archive, out);
    c.arg("-xf").arg(archive).arg("-C").arg(out);
    c.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    let child = c.spawn().map_err(other)?;
    match killable(p, child.id(), || child.wait_with_output()) {
        _ if p.cancel.is_cancelled() => Err(ExtractErr::Cancelled),
        Ok(out) => check(out).map(|_| 0).map_err(ExtractErr::Other),
        Err(e) => Err(other(e)),
    }
}

/// Extract `archive` into `out` with the sandboxed 7-Zip, feeding its percentage to the
/// progress bar. The password goes over stdin, not argv, so other users can't read it from /proc.
/// Returns how many unsafe links 7-Zip refused to create (see `outcome`).
fn run_7z(archive: &Path, out: &Path, pw: Option<&str>, p: &Progress) -> Result<usize, ExtractErr> {
    use std::io::{Read, Write};
    use std::process::Stdio;
    let mut cmd = sandboxed(&seven_zip(), archive, out);
    cmd.args(["x", "-y", "-bso0", "-bse1", "-bsp1"]).arg(format!("-o{}", out.display()));
    // -p with no value: a password-protected archive fails at once instead of prompting
    if pw.is_none() {
        cmd.arg("-p");
    }
    // everything after -- is a file name or filter, so switches must come first
    cmd.arg("--").arg(archive).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    let mut child = cmd.spawn().map_err(|e| ExtractErr::Other(e.to_string()))?;
    if let (Some(mut si), Some(pw)) = (child.stdin.take(), pw) {
        let _ = writeln!(si, "{pw}");
    }
    p.total.store(100, Ordering::Relaxed);
    let mut so = child.stdout.take().unwrap();
    let (mut buf, mut msgs) = ([0u8; 4096], String::new());
    let ok = killable(p, child.id(), || {
        while let Ok(n @ 1..) = so.read(&mut buf) {
            // progress redraws in place with backspaces: "  42% 3 - name"
            for seg in String::from_utf8_lossy(&buf[..n]).split(['\x08', '\r']) {
                match seg.trim_start().split_once('%').and_then(|(n, _)| n.parse::<u64>().ok()) {
                    Some(pct) => p.done.store(pct, Ordering::Relaxed),
                    None => msgs.push_str(seg),
                }
            }
        }
        child.wait().is_ok_and(|s| s.success())
    });
    if p.cancel.is_cancelled() {
        return Err(ExtractErr::Cancelled);
    }
    let r = outcome(ok, &msgs);
    if r.is_ok() {
        p.done.store(100, Ordering::Relaxed); // small archives finish before any percentage prints
    }
    r
}

/// 7-Zip's verdict from its exit status and messages. Links pointing outside the output or at
/// themselves (Ubuntu ISOs have `ubuntu -> .`) are refused as "Dangerous link path" and the run
/// "fails", though every file came out: that's a success with N links skipped.
fn outcome(ok: bool, msgs: &str) -> Result<usize, ExtractErr> {
    if ok {
        return Ok(0);
    }
    if msgs.contains("Wrong password") {
        return Err(ExtractErr::Password);
    }
    let errors: Vec<&str> = msgs.lines().filter_map(|l| l.split_once("ERROR:").map(|(_, e)| e.trim())).collect();
    if !errors.is_empty() && errors.iter().all(|e| e.starts_with("Dangerous link path")) {
        return Ok(errors.len());
    }
    let last = errors
        .last()
        .copied()
        .or_else(|| msgs.lines().map(str::trim).rfind(|l| !l.is_empty() && *l != "ERRORS:"));
    Err(ExtractErr::Other(last.unwrap_or("failed").to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_exts_compound_first() {
        // a later entry ending in an earlier one could never match (".tar.gz" after ".gz")
        for (i, a) in ARCHIVE_EXTS.iter().enumerate() {
            for b in &ARCHIVE_EXTS[i + 1..] {
                assert!(!b.ends_with(a), "{b} is shadowed by {a}");
            }
        }
        assert_eq!(archive_ext("Photos.TAR.GZ"), Some(".tar.gz"));
        assert_eq!(archive_ext("disk.iso"), Some(".iso"));
        assert_eq!(archive_ext("report.docx"), None);
        let root = archive_root(&gio::File::for_path("/tmp/a b.zip")).uri();
        assert_eq!(root, "archive://file%253A%252F%252F%252Ftmp%252Fa%252520b.zip/");
        let back = archive_source(&gio::File::for_uri(&root).child("sub")).and_then(|f| f.path());
        assert_eq!(back.as_deref(), Some(std::path::Path::new("/tmp/a b.zip")));
        for n in ["a.zip", "b.tar.bz2", "c.tar.gz", "d.part1.rar", "e.cab"] {
            assert!(browse_on_open(n), "{n}");
        }
        for n in [
            "a.deb",
            "b.iso",
            "c.epub",
            "d.qcow2",
            "e.docx",
            "f.whl",
            "g.7z",
            "h.gz",
            "i.cbr",
            "j.cbt",
            "k.AppImage",
            "l.snap",
            "m.a",
        ] {
            assert!(!browse_on_open(n), "{n}");
        }
    }

    /// Every extension Extract is offered on, extracted from a real sample through the same
    /// sandboxed path as Extract Here. Samples: tests/fixtures/archives, built by
    /// scripts/make-archive-fixtures.sh. A new ARCHIVE_EXTS entry fails here until it has one.
    #[test]
    fn every_format_extracts() {
        if std::env::var("KESTREL_7ZZ").is_err() {
            return;
        }
        fn walk(d: &Path, out: &mut Vec<PathBuf>) {
            for e in std::fs::read_dir(d).into_iter().flatten().flatten() {
                match e.file_type() {
                    Ok(t) if t.is_dir() => walk(&e.path(), out),
                    Ok(t) if t.is_file() => out.push(e.path()),
                    _ => {}
                }
            }
        }
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/archives");
        let sample = |ext: &str| match ext {
            ".z01" => "split.z01".to_string(),
            ".001" => "split.7z.001".to_string(),
            ".r00" => "oldrar.r00".to_string(),
            e => format!("sample{e}"),
        };
        // packages keep their own layout (7-Zip gives a .deb's data.tar, an .rpm's .cpio);
        // LZH/CHM/old-style RAR are downloaded real-world files
        const NO_HELLO: &[&str] = &[".deb", ".udeb", ".ipk", ".rpm", ".lzh", ".lha", ".chm", ".r00"];
        let work = std::env::temp_dir().join(format!("kestrel-formats-{}", std::process::id()));
        let (mut missing, mut failed) = (Vec::new(), Vec::new());
        // one line per format; shown with `cargo test every_format -- --nocapture`
        let mut report = Vec::new();
        for (i, ext) in ARCHIVE_EXTS.iter().enumerate() {
            let before = failed.len();
            let f = dir.join(sample(ext));
            if !f.exists() {
                missing.push(*ext);
                continue;
            }
            let out = work.join(i.to_string());
            std::fs::create_dir_all(&out).unwrap();
            let out = out.canonicalize().unwrap();
            let arc = first_volume(&f.canonicalize().unwrap());
            if let Err(e) = unpack(&arc, &out, None, &Progress::default()) {
                failed.push(format!(
                    "{ext}: {}",
                    match e {
                        ExtractErr::Password => "asked for a password".into(),
                        ExtractErr::Cancelled => "cancelled".into(),
                        ExtractErr::Other(m) => m,
                    }
                ));
                continue;
            }
            let mut files = Vec::new();
            walk(&out, &mut files);
            let hello = files.iter().any(|p| std::fs::read(p).is_ok_and(|b| b == b"Hello from Kestrel\n"));
            if files.is_empty() {
                failed.push(format!("{ext}: nothing extracted"));
            }
            if !NO_HELLO.contains(ext) && !hello {
                failed.push(format!("{ext}: hello.txt missing or wrong"));
            }
            // what the preview pane lists must be what Extract produces
            let scratch = work.join(format!("list{i}"));
            std::fs::create_dir_all(&scratch).unwrap();
            let listed = match list_archive(&f.canonicalize().unwrap(), &scratch.canonicalize().unwrap()) {
                Ok(entries) => {
                    for e in entries.iter().filter(|e| !e.dir) {
                        if !out.join(&e.path).exists() {
                            failed.push(format!("{ext}: listed but not extracted: {}", e.path));
                        }
                    }
                    let n = entries.iter().filter(|e| !e.dir).count();
                    if n != files.len() {
                        failed.push(format!("{ext}: preview lists {n} files, Extract gives {}", files.len()));
                    }
                    n
                }
                Err(e) => {
                    failed.push(format!("{ext}: listing failed: {e}"));
                    0
                }
            };
            report.push(format!(
                "{:4}  {:9}  {:16}  {:>2} extracted, {:>2} listed  {}",
                if failed.len() == before { "ok" } else { "FAIL" },
                ext,
                f.file_name().unwrap().to_string_lossy(),
                files.len(),
                listed,
                match (hello, NO_HELLO.contains(ext)) {
                    (true, _) => "hello.txt ok",
                    (false, true) => "(own contents)",
                    (false, false) => "hello.txt MISSING",
                }
            ));
        }
        println!("{}\n{} formats, {} failed", report.join("\n"), report.len() + missing.len(), failed.len());
        let _ = std::fs::remove_dir_all(&work);
        if !missing.is_empty() {
            failed.push(format!("no sample for {missing:?}: run scripts/make-archive-fixtures.sh"));
        }
        assert!(failed.is_empty(), "\n{}", failed.join("\n"));
    }

    #[test]
    fn seven_zip_outcomes() {
        let iso = "  0M Scan /home/alex/Projects/tvbox/build/\nERROR: Dangerous link path was ignored : ubuntu : ./\n\nERRORS:\n";
        assert!(matches!(outcome(false, iso), Ok(1)));
        let bad = "  0M Scan /x/\nERROR: Data Error : src/r.bin\nERROR: Dangerous link path was ignored : a : ../\n";
        assert!(matches!(outcome(false, bad), Err(ExtractErr::Other(e)) if e == "Dangerous link path was ignored : a : ../"));
        assert!(matches!(outcome(false, "ERROR: Data Error : src/r.bin\n"), Err(ExtractErr::Other(e)) if e == "Data Error : src/r.bin"));
        assert!(matches!(outcome(false, "ERROR: Wrong password : a.txt\n"), Err(ExtractErr::Password)));
        assert!(matches!(outcome(true, ""), Ok(0)));
    }

    #[test]
    fn volume_sets() {
        let d = std::env::temp_dir().join(format!("kestrel-vol-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        for n in [
            "a.7z.001",
            "a.7z.002",
            "a.7z",
            "b.part1.rar",
            "b.part2.rar",
            "b.txt",
            "c.rar",
            "c.r00",
            "c.r01",
            "s.zip",
            "s.z01",
            "s.zip.txt",
            "solo.zip",
        ] {
            std::fs::write(d.join(n), "").unwrap();
        }
        let set = |n: &str| {
            let mut v: Vec<String> = volumes(&d.join(n))
                .iter()
                .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
                .collect();
            v.sort();
            v
        };
        assert_eq!(set("a.7z.002"), ["a.7z.001", "a.7z.002"]);
        assert_eq!(set("b.part1.rar"), ["b.part1.rar", "b.part2.rar"]);
        assert_eq!(set("c.r00"), ["c.r00", "c.r01", "c.rar"]);
        assert_eq!(set("s.zip"), ["s.z01", "s.zip"]);
        assert_eq!(set("solo.zip"), ["solo.zip"]);
        assert_eq!(set("b.txt"), ["b.txt"]);
        assert_eq!(first_volume(&d.join("c.r01")), d.join("c.rar"));
        assert_eq!(first_volume(&d.join("a.7z.002")), d.join("a.7z.002"));
        std::fs::remove_dir_all(&d).unwrap();
    }

    /// Sandboxed bundled 7-Zip: extracts, asks for a password, and can't write outside `out`.
    /// Needs KESTREL_7ZZ=packaging/7zip/7zz (scripts/update-7zip.sh fetches it); skipped otherwise.
    #[test]
    fn sandboxed_7z() {
        let Ok(z) = std::env::var("KESTREL_7ZZ") else { return };
        let d = std::env::temp_dir().join(format!("kestrel-7z-{}", std::process::id()));
        let out = d.join("out");
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(d.join("a.txt"), "hi").unwrap();
        let mk = |args: &[&str]| assert!(std::process::Command::new(&z).current_dir(&d).args(args).output().unwrap().status.success());
        mk(&["a", "plain.zip", "a.txt"]);
        mk(&["a", "-psecret", "-mhe", "enc.7z", "a.txt"]);
        let p = Progress::default();
        let x = |arc: &str, pw: Option<&str>| run_7z(&d.join(arc), &out, pw, &p);
        assert!(x("plain.zip", None).is_ok());
        assert_eq!(std::fs::read_to_string(out.join("a.txt")).unwrap(), "hi");
        assert!(matches!(x("enc.7z", None), Err(ExtractErr::Password)));
        assert!(matches!(x("enc.7z", Some("nope")), Err(ExtractErr::Password)));
        assert!(x("enc.7z", Some("secret")).is_ok());
        assert_eq!(p.done.load(Ordering::Relaxed), 100);
        // Cancel kills the sandbox (a cancel before it starts kills it at once)
        let c = Progress::default();
        c.cancel.cancel();
        assert!(matches!(run_7z(&d.join("plain.zip"), &out, None, &c), Err(ExtractErr::Cancelled)));
        assert!(!c.can_cancel.load(Ordering::Relaxed));
        // listings, in the sandbox
        std::fs::create_dir(d.join("sub")).unwrap();
        std::fs::write(d.join("sub/b.txt"), "yo!").unwrap();
        mk(&["a", "tree.zip", "sub"]);
        assert!(
            std::process::Command::new("tar")
                .current_dir(&d)
                .args(["-czf", "tree.tar.gz", "sub"])
                .status()
                .unwrap()
                .success()
        );
        for arc in ["tree.zip", "tree.tar.gz"] {
            let l = list_archive(&d.join(arc), &out).unwrap();
            assert_eq!(l.len(), 2, "{arc}");
            assert!(l.iter().any(|e| e.path == "sub" && e.dir), "{arc}");
            assert!(l.iter().any(|e| e.path == "sub/b.txt" && !e.dir && e.size == 3), "{arc}");
        }
        assert_eq!(list_archive(&d.join("enc.7z"), &out).err().as_deref(), Some("Encrypted archive"));
        // split archives: the sandbox must see every part (incompressible data, so it really splits)
        let mut big = vec![0u8; 3 << 20];
        let mut x: u64 = 1;
        for b in big.iter_mut() {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            *b = x as u8;
        }
        std::fs::write(d.join("sub/big.bin"), &big).unwrap();
        mk(&["a", "-v1m", "vol.7z", "sub"]);
        assert!(d.join("vol.7z.004").exists());
        let mut zips = vec!["vol.7z.001"];
        if std::process::Command::new("zip")
            .current_dir(&d)
            .args(["-q", "-r", "-s", "1m", "sz.zip", "sub"])
            .status()
            .is_ok_and(|s| s.success())
        {
            zips.extend(["sz.zip", "sz.z01"]);
        }
        for arc in zips {
            let o = d.join(format!("out-{arc}"));
            std::fs::create_dir(&o).unwrap();
            assert!(run_7z(&d.join(arc), &o, None, &p).is_ok(), "{arc}");
            assert_eq!(std::fs::read(o.join("sub/big.bin")).unwrap(), big, "{arc}");
        }
        // the sandbox itself: writes outside `out` are refused, home is hidden
        let mut c = sandboxed("/bin/sh", &d.join("plain.zip"), &out);
        c.args(["-c", &format!("echo x > {}/escaped; ls $HOME", d.display())]);
        assert!(!c.output().unwrap().status.success());
        assert!(!d.join("escaped").exists());
        std::fs::remove_dir_all(&d).unwrap();
    }
}
