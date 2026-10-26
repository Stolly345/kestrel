//! Full scans of local filesystems, plus applying single-path changes from the live watcher.

use crate::{AclEntry, Index, Kind, NO_PARENT};
use std::collections::HashMap;
use std::fs::{FileType, Metadata};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

const LOCAL_FS: &[&str] = &["ext4", "ext3", "ext2", "btrfs", "xfs", "f2fs", "vfat", "exfat", "ntfs3", "ntfs", "fuseblk"];

/// Used when `/etc/kestrel/excludes` is missing. Timeshift snapshots repeat the whole system per
/// snapshot; container storage holds millions of image-layer files (4.4M on Alex's laptop).
pub const DEFAULT_EXCLUDES: &[&str] = &["/timeshift", "/var/lib/docker", "/var/lib/containers"];

/// One absolute path per line; blank lines and `#` comments ignored.
pub fn load_excludes(file: &Path) -> Vec<PathBuf> {
    match std::fs::read_to_string(file) {
        Ok(t) => t.lines().map(str::trim).filter(|l| l.starts_with('/')).map(PathBuf::from).collect(),
        Err(_) => DEFAULT_EXCLUDES.iter().map(PathBuf::from).collect(),
    }
}

fn excluded(p: &Path, excludes: &[PathBuf]) -> bool {
    excludes.iter().any(|x| p.starts_with(x))
}

/// Mount points of local disk filesystems, one per device (bind mounts dropped), sorted.
pub fn local_mounts(excludes: &[PathBuf]) -> Vec<PathBuf> {
    let text = std::fs::read_to_string("/proc/self/mounts").unwrap_or_default();
    let mut by_dev: HashMap<u64, PathBuf> = HashMap::new();
    for line in text.lines() {
        let f: Vec<&str> = line.split(' ').collect();
        if f.len() < 3 || !LOCAL_FS.contains(&f[2]) {
            continue;
        }
        let mp = PathBuf::from(unescape(f[1]));
        let Ok(m) = std::fs::metadata(&mp) else { continue };
        if excluded(&mp, excludes) {
            continue;
        }
        by_dev
            .entry(m.dev())
            .and_modify(|p| {
                if mp.as_os_str().len() < p.as_os_str().len() {
                    *p = mp.clone();
                }
            })
            .or_insert(mp);
    }
    let mut v: Vec<_> = by_dev.into_values().collect();
    v.sort();
    v
}

/// `/proc/mounts` escapes space, tab, newline, backslash as `\ooo`.
fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c)) {
            out.push((b[i + 1] - b'0') * 64 + (b[i + 2] - b'0') * 8 + (b[i + 3] - b'0'));
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[derive(Clone, Debug, Default)]
pub struct Meta {
    tags: u8,
    size: u64,
    mtime: i64,
    atime: i64,
    btime: i64,
    mode: u32,
    uid: u32,
    gid: u32,
    /// Directories only (files' ACLs are read when `content:` needs them).
    acl: Option<Box<[AclEntry]>>,
}

impl From<&Metadata> for Meta {
    fn from(m: &Metadata) -> Self {
        Meta {
            tags: 0,
            size: m.size(),
            mtime: m.mtime(),
            atime: m.atime(),
            btime: m
                .created()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs() as i64),
            acl: None,
            mode: m.mode(),
            uid: m.uid(),
            gid: m.gid(),
        }
    }
}

/// Color tags from the xattr; 0 when absent (the common case: one failed syscall).
pub fn read_tags(p: &Path) -> u8 {
    let Ok(c) = std::ffi::CString::new(p.as_os_str().as_bytes()) else { return 0 };
    let mut buf = [0u8; 128];
    let n = unsafe { libc::lgetxattr(c.as_ptr(), c"user.chimera.tags".as_ptr(), buf.as_mut_ptr().cast(), buf.len()) };
    if n <= 0 {
        return 0;
    }
    crate::tag_bits(&String::from_utf8_lossy(&buf[..n as usize]))
}

fn kind(ft: FileType) -> Kind {
    if ft.is_symlink() {
        Kind::Symlink
    } else if ft.is_dir() {
        Kind::Dir
    } else if ft.is_file() {
        Kind::File
    } else {
        Kind::Other
    }
}

fn push(ix: &mut Index, parent: u32, name: &[u8], k: Kind, m: &Meta) -> u32 {
    let id = ix.push(parent, name, k, m.size, m.mtime, m.mode, m.uid, m.gid);
    let e = &mut ix.entries[id as usize];
    (e.tags, e.atime, e.btime) = (m.tags, m.atime, m.btime);
    if let Some(a) = &m.acl {
        ix.acls.insert(id, a.clone());
    }
    id
}

/// The `system.posix_acl_access` xattr, if this path has an extended ACL (most don't).
pub fn read_acl(p: &Path) -> Option<Box<[AclEntry]>> {
    let c = std::ffi::CString::new(p.as_os_str().as_bytes()).ok()?;
    let mut buf = [0u8; 4 + 8 * 32];
    let n = unsafe { libc::lgetxattr(c.as_ptr(), c"system.posix_acl_access".as_ptr(), buf.as_mut_ptr().cast(), buf.len()) };
    if n < 4 || u32::from_le_bytes(buf[0..4].try_into().unwrap()) != 2 {
        return None;
    }
    let v: Vec<AclEntry> = buf[4..n as usize]
        .as_chunks::<8>()
        .0
        .iter()
        .map(|c| AclEntry {
            tag: u16::from_le_bytes([c[0], c[1]]),
            perm: u16::from_le_bytes([c[2], c[3]]),
            id: u32::from_le_bytes(c[4..8].try_into().unwrap()),
        })
        .collect();
    Some(v.into())
}

fn meta_of(m: &Metadata, path: &Path) -> Meta {
    let mut meta: Meta = m.into();
    meta.tags = read_tags(path);
    if m.is_dir() {
        meta.acl = read_acl(path);
    }
    meta
}

#[derive(Debug, Default)]
pub struct ScanStats {
    pub entries: usize,
    pub errors: usize,
}

/// Index a whole filesystem rooted at `root` as a new root entry.
pub fn scan(ix: &mut Index, root: &Path, excludes: &[PathBuf]) -> std::io::Result<ScanStats> {
    let m = std::fs::symlink_metadata(root)?;
    let id = push(ix, NO_PARENT, root.as_os_str().as_bytes(), Kind::Dir, &meta_of(&m, root));
    let mut s = scan_under(ix, id, root, m.dev(), excludes);
    s.entries += 1;
    Ok(s)
}

/// Fresh index of every local mount.
pub fn scan_all(excludes: &[PathBuf]) -> (Index, ScanStats) {
    let mut ix = Index {
        epoch: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as u64),
        ..Default::default()
    };
    let mut total = ScanStats::default();
    for r in local_mounts(excludes) {
        if let Ok(s) = scan(&mut ix, &r, excludes) {
            total.entries += s.entries;
            total.errors += s.errors;
        }
    }
    (ix, total)
}

/// One folder's listing, read on a worker thread.
struct Listing {
    id: u32,
    path: PathBuf,
    items: Vec<(std::ffi::OsString, Kind, Meta)>,
    failed: bool,
}

/// Read and stat one folder. Subfolders on another filesystem or excluded are left out entirely
/// (they're indexed as their own root, or deliberately skipped).
fn list_dir(path: PathBuf, id: u32, dev: u64, excludes: &[PathBuf]) -> Listing {
    let mut items = Vec::new();
    let Ok(rd) = std::fs::read_dir(&path) else {
        return Listing { id, path, items, failed: true };
    };
    for e in rd.flatten() {
        // DirEntry::metadata is an lstat relative to the open folder: no symlink following
        let Ok(m) = e.metadata() else { continue };
        let child = path.join(e.file_name());
        if m.is_dir() && (m.dev() != dev || excluded(&child, excludes)) {
            continue;
        }
        items.push((e.file_name(), kind(m.file_type()), meta_of(&m, &child)));
    }
    Listing {
        id,
        path,
        items,
        failed: false,
    }
}

/// Walk `dir` (already indexed as `dir_id`) without crossing filesystems.
///
/// Worker threads list folders; this thread files each listing into the index and hands the new
/// subfolders back out as jobs. Nothing waits for tree order (jwalk did, buffering everything its
/// threads had read ahead: ~600 MB at peak on 2M files), so only listings in flight are held.
fn scan_under(ix: &mut Index, dir_id: u32, dir: &Path, dev: u64, excludes: &[PathBuf]) -> ScanStats {
    use std::sync::{Arc, Mutex, mpsc};
    let mut stats = ScanStats::default();
    let (job_tx, job_rx) = mpsc::channel::<(PathBuf, u32)>();
    let job_rx = Arc::new(Mutex::new(job_rx));
    let (res_tx, res_rx) = mpsc::channel::<Listing>();
    let workers = std::thread::available_parallelism().map_or(4, |n| n.get()).min(16);
    std::thread::scope(|sc| {
        for _ in 0..workers {
            let (rx, tx) = (job_rx.clone(), res_tx.clone());
            sc.spawn(move || {
                loop {
                    let job = rx.lock().unwrap().recv();
                    let Ok((path, id)) = job else { break };
                    if tx.send(list_dir(path, id, dev, excludes)).is_err() {
                        break;
                    }
                }
            });
        }
        drop(res_tx);
        let _ = job_tx.send((dir.to_path_buf(), dir_id));
        let mut pending = 1usize;
        while pending > 0 {
            let Ok(l) = res_rx.recv() else { break };
            pending -= 1;
            if l.failed {
                stats.errors += 1;
            }
            for (name, k, meta) in l.items {
                let id = push(ix, l.id, name.as_bytes(), k, &meta);
                stats.entries += 1;
                if k == Kind::Dir {
                    let _ = job_tx.send((l.path.join(&name), id));
                    pending += 1;
                }
            }
        }
        // workers see the closed channel and exit; scope joins them
        drop(job_tx);
    });
    stats
}

/// Bring one path in line with the disk after a watcher event. `fresh` = it was created or moved
/// in, so any existing entry (and subtree) is replaced rather than updated in place.
// ponytail: a moved directory is removed + rescanned, not relinked; FAN_RENAME pairing if big moves get slow.
pub fn reconcile(ix: &mut Index, path: &Path, fresh: bool, excludes: &[PathBuf]) {
    let (Some(dir_path), Some(name)) = (path.parent(), path.file_name()) else {
        return;
    };
    if excluded(path, excludes) {
        return;
    }
    // unknown parent: it is new too, and its own event rescans it
    let Some(dir) = ix.lookup(dir_path) else { return };
    let name = name.as_bytes();
    let existing = ix.child(dir, name);
    let Ok(m) = std::fs::symlink_metadata(path) else {
        if let Some(i) = existing {
            ix.remove(i);
        }
        return;
    };
    let k = kind(m.file_type());
    let meta = meta_of(&m, path);
    if let Some(i) = existing {
        if !fresh && ix.entries[i as usize].kind == k {
            let e = &mut ix.entries[i as usize];
            (e.size, e.mtime, e.atime, e.btime, e.mode, e.uid, e.gid, e.tags) =
                (meta.size, meta.mtime, meta.atime, meta.btime, meta.mode, meta.uid, meta.gid, meta.tags);
            match meta.acl {
                Some(a) => ix.acls.insert(i, a),
                None => ix.acls.remove(&i),
            };
            return;
        }
        ix.remove(i);
    }
    if k == Kind::Dir && std::fs::symlink_metadata(dir_path).is_ok_and(|pm| pm.dev() != m.dev()) {
        return; // a new mount point
    }
    let id = push(ix, dir, name, k, &meta);
    if k == Kind::Dir {
        scan_under(ix, id, path, m.dev(), excludes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Query, Sort};
    use std::fs;

    #[test]
    fn mount_unescape() {
        assert_eq!(unescape(r"/media/my\040disk"), "/media/my disk");
        assert_eq!(unescape(r"/plain"), "/plain");
    }

    #[test]
    fn excludes_file() {
        assert_eq!(load_excludes(Path::new("/nonexistent"))[0], PathBuf::from("/timeshift"));
        assert!(excluded(Path::new("/timeshift/snapshots/x"), &[PathBuf::from("/timeshift")]));
        assert!(!excluded(Path::new("/timeshifted"), &[PathBuf::from("/timeshift")]));
    }

    #[test]
    fn scan_parents_are_right() {
        // every entry's parent link must be right: each path rebuilds to a real file
        let d = std::env::temp_dir().join(format!("kestrel-tree-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        for i in 0..20 {
            fs::create_dir_all(d.join(format!("d{i}/sub{}/deep", i % 3))).unwrap();
            fs::write(d.join(format!("d{i}/f{i}.txt")), "x").unwrap();
            fs::write(d.join(format!("d{i}/sub{}/deep/g{i}", i % 3)), "x").unwrap();
        }
        let mut ix = Index::default();
        let s = scan(&mut ix, &d, &[]).unwrap();
        assert_eq!(s.errors, 0);
        assert_eq!(ix.len(), 1 + 20 * 5);
        for i in 0..ix.len() as u32 {
            let p = std::path::PathBuf::from(std::ffi::OsStr::from_bytes(&ix.path(i)));
            assert!(p.symlink_metadata().is_ok(), "{} doesn't exist", p.display());
        }
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn live_reconcile() {
        let d = std::env::temp_dir().join(format!("kestrel-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("a/b")).unwrap();
        fs::create_dir_all(d.join("skip/deep")).unwrap();
        fs::write(d.join("a/b/one.txt"), "x").unwrap();
        fs::write(d.join("skip/deep/hidden.txt"), "x").unwrap();
        let ex = [d.join("skip")];
        let mut ix = Index::default();
        scan(&mut ix, &d, &ex).unwrap();
        let n = |ix: &Index, q: &str| ix.search(&Query::parse(q).unwrap(), Sort::Name, false, None).len();

        assert_eq!(n(&ix, "one.txt"), 1);
        assert_eq!(n(&ix, "hidden"), 0, "excluded at scan");
        assert_eq!(n(&ix, "skip"), 0);

        fs::write(d.join("a/two.txt"), "yy").unwrap();
        reconcile(&mut ix, &d.join("a/two.txt"), true, &ex);
        assert_eq!(n(&ix, "two.txt size:2"), 1, "create");

        fs::write(d.join("a/two.txt"), "yyyy").unwrap();
        reconcile(&mut ix, &d.join("a/two.txt"), false, &ex);
        assert_eq!(n(&ix, "two.txt size:4"), 1, "modify");
        assert_eq!(n(&ix, "two.txt"), 1, "no duplicate");

        fs::rename(d.join("a/b"), d.join("a/c")).unwrap();
        reconcile(&mut ix, &d.join("a/b"), false, &ex);
        reconcile(&mut ix, &d.join("a/c"), true, &ex);
        assert_eq!(n(&ix, "a/c/one.txt"), 1, "dir rename keeps contents");
        assert_eq!(n(&ix, "a/b"), 0);

        fs::remove_file(d.join("a/two.txt")).unwrap();
        reconcile(&mut ix, &d.join("a/two.txt"), false, &ex);
        assert_eq!(n(&ix, "two.txt"), 0, "delete");

        fs::write(d.join("skip/new.txt"), "x").unwrap();
        reconcile(&mut ix, &d.join("skip/new.txt"), true, &ex);
        assert_eq!(n(&ix, "new.txt"), 0, "excluded live");

        // a whole new tree arriving in one go (mv from elsewhere, tar -x)
        fs::create_dir_all(d.join("t/u/v")).unwrap();
        fs::write(d.join("t/u/v/leaf.rs"), "").unwrap();
        reconcile(&mut ix, &d.join("t/u/v/leaf.rs"), true, &ex); // parent unknown yet: ignored
        reconcile(&mut ix, &d.join("t"), true, &ex);
        assert_eq!(n(&ix, "leaf.rs"), 1);
        assert!(ix.gone > 0);

        // tags: set on disk, picked up by a non-fresh event (fanotify reports xattr changes as ATTRIB)
        let leaf = d.join("t/u/v/leaf.rs");
        let c = std::ffi::CString::new(leaf.as_os_str().as_bytes()).unwrap();
        let ok = unsafe { libc::lsetxattr(c.as_ptr(), c"user.chimera.tags".as_ptr(), b"green".as_ptr().cast(), 5, 0) } == 0;
        if ok {
            // tmpfs without user xattrs would skip this
            reconcile(&mut ix, &leaf, false, &ex);
            assert_eq!(n(&ix, "tag:green"), 1);
        }
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn acl_deny_hides_entries() {
        use crate::Cred;
        let d = std::env::temp_dir().join(format!("kestrel-acl-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("shared")).unwrap();
        fs::write(d.join("shared/report.txt"), "x").unwrap();
        // mode 0755 lets everyone in, but the ACL gives user 4242 nothing
        let mut acl = 2u32.to_le_bytes().to_vec();
        for (tag, perm, id) in [
            (0x01u16, 7u16, u32::MAX),
            (0x02, 0, 4242),
            (0x04, 5, u32::MAX),
            (0x10, 5, u32::MAX),
            (0x20, 5, u32::MAX),
        ] {
            acl.extend(tag.to_le_bytes());
            acl.extend(perm.to_le_bytes());
            acl.extend(id.to_le_bytes());
        }
        let c = std::ffi::CString::new(d.join("shared").as_os_str().as_bytes()).unwrap();
        let r = unsafe { libc::lsetxattr(c.as_ptr(), c"system.posix_acl_access".as_ptr(), acl.as_ptr().cast(), acl.len(), 0) };
        if r != 0 {
            eprintln!("skipping: no POSIX ACL support here ({})", std::io::Error::last_os_error());
            fs::remove_dir_all(&d).unwrap();
            return;
        }
        let mut ix = Index::default();
        scan(&mut ix, &d, &[]).unwrap();
        let shared = ix.lookup(&d.join("shared")).unwrap();
        assert!(ix.acls.contains_key(&shared), "ACL read at scan time");
        let q = Query::parse("report").unwrap();
        let me = std::os::unix::fs::MetadataExt::uid(&fs::metadata(&d).unwrap());
        let see = |uid: u32| ix.search(&q, Sort::Name, false, Some(&Cred { uid, gids: vec![uid] })).len();
        assert_eq!(see(4242), 0, "named-user ACL entry with no perms hides the file");
        assert_eq!(see(4243), 1, "everyone else falls through to other:r-x");
        assert_eq!(see(me), 1, "owner");
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn content_respects_permissions() {
        use crate::Cred;
        let d = std::env::temp_dir().join(format!("kestrel-content-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("open.txt"), "the Secret word").unwrap();
        fs::write(d.join("closed.txt"), "the secret word").unwrap();
        let mut ix = Index::default();
        scan(&mut ix, &d, &[]).unwrap();
        let q = Query::parse("content:secret").unwrap();
        assert!(q.reads);
        assert_eq!(ix.search(&q, Sort::Name, false, None).len(), 2, "case-insensitive");
        assert_eq!(ix.search(&Query::parse("case:content:Secret").unwrap(), Sort::Name, false, None).len(), 1);
        // pretend closed.txt is root-only 0600: another user must not learn it matches
        let closed = ix.lookup(&d.join("closed.txt")).unwrap();
        (ix.entries[closed as usize].uid, ix.entries[closed as usize].mode) = (0, 0o100600);
        let me = Cred { uid: 4242, gids: vec![4242] };
        let hits = ix.search(&q, Sort::Name, false, Some(&me));
        assert_eq!(
            hits.iter().map(|&i| String::from_utf8_lossy(ix.name(i)).into_owned()).collect::<Vec<_>>(),
            ["open.txt"]
        );
        fs::remove_dir_all(&d).unwrap();
    }
}
