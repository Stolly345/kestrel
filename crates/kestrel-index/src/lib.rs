//! In-memory filesystem index: flat entry table + name arenas, Everything-style.

pub mod proto;
pub mod query;
pub mod scan;
pub mod snapshot;

use rayon::prelude::*;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

pub use query::Query;

pub const NO_PARENT: u32 = u32::MAX;

/// Case-folded name: ASCII fast path, Unicode lowercasing for the rest (non-UTF-8 stays ASCII-folded).
pub fn lowercase(name: &[u8]) -> Vec<u8> {
    match std::str::from_utf8(name) {
        Ok(s) if !s.is_ascii() => s.to_lowercase().into_bytes(),
        _ => name.to_ascii_lowercase(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    File,
    Dir,
    Symlink,
    Other,
    /// Removed by a live update; slot is dead until the next full rescan compacts it.
    Gone,
}

#[derive(Clone, Copy, Debug)]
pub struct Entry {
    name_off: u32,
    name_len: u16,
    /// The lowercased name has its own place: Unicode lowercasing can change the byte length.
    lname_len: u16,
    lname_off: u32,
    pub kind: Kind,
    pub parent: u32,
    first_child: u32,
    next_sibling: u32,
    pub size: u64,
    pub mtime: i64,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    /// Color tag bits, see [`TAGS`].
    pub tags: u8,
    /// Access and birth (creation) times; 0 when the filesystem doesn't record them.
    pub atime: i64,
    pub btime: i64,
}

/// One POSIX ACL entry (`system.posix_acl_access`), as the kernel stores it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AclEntry {
    pub tag: u16,
    pub perm: u16,
    pub id: u32,
}

pub const ACL_USER_OBJ: u16 = 0x01;
pub const ACL_USER: u16 = 0x02;
pub const ACL_GROUP_OBJ: u16 = 0x04;
pub const ACL_GROUP: u16 = 0x08;
pub const ACL_MASK: u16 = 0x10;
pub const ACL_OTHER: u16 = 0x20;

/// Color tags, stored on files as the `user.chimera.tags` xattr ("red,blue"); bit i = TAGS[i].
pub const TAGS: [&str; 7] = ["red", "orange", "yellow", "green", "blue", "purple", "gray"];
pub const TAG_XATTR: &str = "user.chimera.tags";

pub fn tag_bits(list: &str) -> u8 {
    list.split([',', ';'])
        .filter_map(|t| TAGS.iter().position(|x| x.eq_ignore_ascii_case(t.trim())))
        .fold(0, |m, i| m | 1 << i)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Sort {
    #[default]
    Name,
    Path,
    Size,
    Mtime,
    Ext,
}

/// Who is asking. uid 0 sees everything.
#[derive(Clone)]
pub struct Cred {
    pub uid: u32,
    pub gids: Vec<u32>,
}

thread_local! {
    /// Whose permissions `content:` reads with on this thread (set per search; None = our own).
    static READ_AS: std::cell::RefCell<Option<Cred>> = const { std::cell::RefCell::new(None) };
}

/// Read a file for `content:` as the caller: the open happens with this thread's filesystem uid,
/// gid and groups switched to theirs, so the kernel enforces their permissions even if the index's
/// copy of the file's mode is a moment stale. On Linux these are per-thread, and root's
/// file-access capabilities drop while the fsuid isn't 0. Needs CAP_SETUID/CAP_SETGID; if the
/// switch doesn't take, nothing is read.
pub(crate) fn read_as_caller(path: &Path) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    // only root has more access than the caller; anyone else already reads with their own rights
    let as_root = unsafe { libc::geteuid() } == 0;
    let Some(c) = READ_AS.with_borrow(|c| c.clone()).filter(|c| c.uid != 0 && as_root) else {
        return std::fs::read(path);
    };
    let gid = c.gids.first().copied().unwrap_or(65534);
    let denied = || std::io::Error::from(std::io::ErrorKind::PermissionDenied);
    // SAFETY: plain syscalls on this thread's credentials; every path below restores them
    unsafe {
        let n = libc::getgroups(0, std::ptr::null_mut());
        let mut old = vec![0 as libc::gid_t; n.max(0) as usize];
        if libc::getgroups(n, old.as_mut_ptr()) < 0 {
            return Err(denied());
        }
        // raw syscall: glibc's setgroups() would change every thread in the process
        let set = |g: &[u32]| libc::syscall(libc::SYS_setgroups, g.len(), g.as_ptr()) == 0;
        let restore = || {
            libc::setfsuid(0);
            libc::setfsgid(0);
            set(&old);
        };
        if !set(&c.gids) {
            return Err(denied());
        }
        libc::setfsgid(gid);
        libc::setfsuid(c.uid);
        // setfsuid() reports the previous value: asking again tells whether the switch happened
        let switched = libc::setfsuid(c.uid) as u32 == c.uid && libc::setfsgid(gid) as u32 == gid;
        let file = if switched { std::fs::File::open(path) } else { Err(denied()) };
        restore();
        let mut buf = Vec::new();
        file?.read_to_end(&mut buf)?;
        Ok(buf)
    }
}

impl Cred {
    /// `bits` is an rwx triplet (4=r, 2=w, 1=x), checked the way the kernel does: mode bits, or the
    /// POSIX ACL when the entry has one (owner, named user, then groups, then other).
    fn can(&self, e: &Entry, acl: Option<&[AclEntry]>, bits: u32) -> bool {
        let ok = |perm: u32| perm & bits == bits;
        let Some(acl) = acl else {
            let shift = if e.uid == self.uid {
                6
            } else if self.gids.contains(&e.gid) {
                3
            } else {
                0
            };
            return ok(e.mode >> shift);
        };
        let find = |tag: u16, id: Option<u32>| acl.iter().find(|a| a.tag == tag && id.is_none_or(|i| a.id == i)).map(|a| a.perm as u32);
        if e.uid == self.uid {
            return ok(find(ACL_USER_OBJ, None).unwrap_or(0));
        }
        let mask = find(ACL_MASK, None).unwrap_or(7);
        if let Some(p) = find(ACL_USER, Some(self.uid)) {
            return ok(p & mask);
        }
        // any matching group entry that grants it wins; matching groups that don't means denied
        let mut matched = false;
        for a in acl {
            let hit = (a.tag == ACL_GROUP_OBJ && self.gids.contains(&e.gid)) || (a.tag == ACL_GROUP && self.gids.contains(&a.id));
            if hit {
                matched = true;
                if ok(a.perm as u32 & mask) {
                    return true;
                }
            }
        }
        !matched && ok(find(ACL_OTHER, None).unwrap_or(0))
    }
}

/// Names live in one arena, lowercased names in another (Unicode-aware, so "É" matches "é").
#[derive(Default)]
pub struct Index {
    pub entries: Vec<Entry>,
    names: Vec<u8>,
    lower: Vec<u8>,
    /// Directories that carry a POSIX ACL (rare): their entries, for the permission filter.
    pub acls: std::collections::HashMap<u32, Box<[AclEntry]>>,
    roots: Vec<u32>,
    pub gone: usize,
    /// Changes when the whole index is replaced; entry ids from an older epoch are meaningless.
    pub epoch: u64,
}

impl Index {
    #[allow(clippy::too_many_arguments)]
    pub fn push(&mut self, parent: u32, name: &[u8], kind: Kind, size: u64, mtime: i64, mode: u32, uid: u32, gid: u32) -> u32 {
        let id = self.entries.len() as u32;
        let name_off = self.names.len() as u32;
        self.names.extend_from_slice(name);
        let lname_off = self.lower.len() as u32;
        self.lower.extend_from_slice(&lowercase(name));
        let lname_len = (self.lower.len() as u32 - lname_off) as u16;
        let next_sibling = if parent == NO_PARENT {
            NO_PARENT
        } else {
            self.entries[parent as usize].first_child
        };
        self.entries.push(Entry {
            name_off,
            name_len: name.len() as u16,
            lname_len,
            lname_off,
            kind,
            parent,
            first_child: NO_PARENT,
            next_sibling,
            size,
            mtime,
            mode,
            uid,
            gid,
            tags: 0,
            atime: 0,
            btime: 0,
        });
        if parent == NO_PARENT {
            self.roots.push(id);
        } else {
            self.entries[parent as usize].first_child = id;
        }
        id
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn live(&self) -> usize {
        self.len() - self.gone
    }

    pub fn name(&self, i: u32) -> &[u8] {
        let e = &self.entries[i as usize];
        &self.names[e.name_off as usize..e.name_off as usize + e.name_len as usize]
    }

    pub fn lname(&self, i: u32) -> &[u8] {
        let e = &self.entries[i as usize];
        &self.lower[e.lname_off as usize..e.lname_off as usize + e.lname_len as usize]
    }

    pub fn children(&self, dir: u32) -> impl Iterator<Item = u32> + '_ {
        let real = |c: u32| (c != NO_PARENT).then_some(c);
        std::iter::successors(real(self.entries[dir as usize].first_child), move |&c| {
            real(self.entries[c as usize].next_sibling)
        })
    }

    pub fn child(&self, dir: u32, name: &[u8]) -> Option<u32> {
        self.children(dir).find(|&c| self.name(c) == name)
    }

    /// Entry for an absolute path, via the deepest matching root.
    pub fn lookup(&self, path: &Path) -> Option<u32> {
        let (mut cur, rest) = self
            .roots
            .iter()
            .filter_map(|&r| path.strip_prefix(OsStr::from_bytes(self.name(r))).ok().map(|rest| (r, rest)))
            .max_by_key(|&(r, _)| self.name(r).len())?;
        for c in rest.components() {
            cur = self.child(cur, c.as_os_str().as_bytes())?;
        }
        Some(cur)
    }

    /// Detach `i` from its parent and mark it and everything under it Gone.
    pub fn remove(&mut self, i: u32) {
        let (p, next) = (self.entries[i as usize].parent, self.entries[i as usize].next_sibling);
        if p != NO_PARENT {
            if self.entries[p as usize].first_child == i {
                self.entries[p as usize].first_child = next;
            } else {
                let prev = self.children(p).find(|&c| self.entries[c as usize].next_sibling == i);
                if let Some(prev) = prev {
                    self.entries[prev as usize].next_sibling = next;
                }
            }
        }
        let mut stack = vec![i];
        while let Some(x) = stack.pop() {
            stack.extend(self.children(x));
            self.entries[x as usize].kind = Kind::Gone;
            self.gone += 1;
        }
    }

    fn join<'a>(&'a self, i: u32, part: impl Fn(u32) -> &'a [u8]) -> Vec<u8> {
        let mut chain = vec![i];
        let mut p = self.entries[i as usize].parent;
        while p != NO_PARENT {
            chain.push(p);
            p = self.entries[p as usize].parent;
        }
        let mut out = Vec::with_capacity(64);
        for &c in chain.iter().rev() {
            if !out.is_empty() && out.last() != Some(&b'/') {
                out.push(b'/');
            }
            out.extend_from_slice(part(c));
        }
        out
    }

    pub fn path(&self, i: u32) -> Vec<u8> {
        self.join(i, |c| self.name(c))
    }

    pub fn lpath(&self, i: u32) -> Vec<u8> {
        self.join(i, |c| self.lname(c))
    }

    /// Extension of a file (lowercased), `None` for dirs and dotfiles like `.bashrc`.
    pub fn ext(&self, i: u32) -> Option<&[u8]> {
        if self.entries[i as usize].kind == Kind::Dir {
            return None;
        }
        let n = self.lname(i);
        match memchr::memrchr(b'.', n) {
            Some(p) if p > 0 => Some(&n[p + 1..]),
            _ => None,
        }
    }

    /// Could `cred` discover this entry with `ls`? Needs r+x on its directory and x on every ancestor.
    pub fn visible(&self, i: u32, cred: &Cred) -> bool {
        if cred.uid == 0 {
            return true;
        }
        let mut p = self.entries[i as usize].parent;
        let mut bits = 5;
        while p != NO_PARENT {
            let e = &self.entries[p as usize];
            if !cred.can(e, self.acls.get(&p).map(|a| &a[..]), bits) {
                return false;
            }
            bits = 1;
            p = e.parent;
        }
        true
    }

    /// A file's ACL, read from disk (only files about to be opened by `content:` need it).
    fn file_acl(&self, i: u32) -> Option<Box<[AclEntry]>> {
        match self.acls.get(&i) {
            Some(a) => Some(a.clone()),
            None if self.entries[i as usize].kind == Kind::File => scan::read_acl(std::path::Path::new(OsStr::from_bytes(&self.path(i)))),
            None => None,
        }
    }

    /// Detach a whole filesystem (unplugged drive).
    pub fn remove_root(&mut self, root: &Path) -> bool {
        let Some(pos) = self.roots.iter().position(|&r| self.name(r) == root.as_os_str().as_bytes()) else {
            return false;
        };
        let r = self.roots.remove(pos);
        self.remove(r);
        true
    }

    pub fn roots(&self) -> impl Iterator<Item = &[u8]> + '_ {
        self.roots.iter().map(|&r| self.name(r))
    }

    /// Drop spare capacity left by Vec doubling during a scan.
    pub fn shrink(&mut self) {
        self.entries.shrink_to_fit();
        self.names.shrink_to_fit();
        self.lower.shrink_to_fit();
    }

    pub fn approx_bytes(&self) -> usize {
        self.entries.capacity() * size_of::<Entry>() + self.names.capacity() + self.lower.capacity()
    }

    // ponytail: sorts the match set per query; pre-sorted column arrays (Everything's trick) if huge result sets feel slow.
    pub fn search(&self, q: &Query, sort: Sort, desc: bool, cred: Option<&Cred>) -> Vec<u32> {
        self.search_top(q, sort, desc, cred, None, &std::sync::atomic::AtomicBool::new(false)).0
    }

    /// `search`, but only the first `top` hits are guaranteed in order: the rest all rank after
    /// them, unordered until `sort_rest`. A one-character query matches nearly everything, and
    /// fully sorting millions of hits to show 500 is what made typing `.` hang. Returns the hits
    /// and how many are sorted. Setting `cancel` makes it give up and return nothing.
    pub fn search_top(&self, q: &Query, sort: Sort, desc: bool, cred: Option<&Cred>, top: Option<usize>, cancel: &std::sync::atomic::AtomicBool) -> (Vec<u32>, usize) {
        use std::sync::atomic::Ordering::Relaxed;
        let read_as = if q.reads { cred.cloned() } else { None };
        let mut hits: Vec<u32> = (0..self.len() as u32)
            .into_par_iter()
            // each worker thread reads `content:` files as the caller (see read_as_caller)
            .map_init(
                || READ_AS.set(read_as.clone()),
                |_, i| {
                    if cancel.load(Relaxed) {
                        return None;
                    }
                    let e = &self.entries[i as usize];
                    let keep = e.kind != Kind::Gone
                        && if q.reads {
                            // never open what the caller couldn't: the index's view first, then the kernel's
                            cred.is_none_or(|c| c.uid == 0 || (self.visible(i, c) && c.can(e, self.file_acl(i).as_deref(), 4))) && q.matches(self, i)
                        } else {
                            q.matches(self, i) && cred.is_none_or(|c| self.visible(i, c))
                        };
                    keep.then_some(i)
                },
            )
            .flatten()
            .collect();
        if cancel.load(Relaxed) {
            return (Vec::new(), 0);
        }
        let cmp = self.order(q, sort, desc);
        match top {
            // ponytail: Path keys allocate per comparison, and dupes regroup everything; both keep the full sort
            Some(k) if k < hits.len() && sort != Sort::Path && q.dupe.is_none() => {
                hits.select_nth_unstable_by(k, &cmp);
                hits[..k].par_sort_unstable_by(&cmp);
                return (hits, k);
            }
            _ => {}
        }
        if sort == Sort::Path {
            let boosted = |i: u32| self.boosted(q, i);
            let mut keyed: Vec<_> = hits.par_iter().map(|&i| (!boosted(i), self.lpath(i), i)).collect();
            keyed.par_sort_unstable_by(|a, b| a.0.cmp(&b.0).then(if desc { (&b.1, b.2).cmp(&(&a.1, a.2)) } else { (&a.1, a.2).cmp(&(&b.1, b.2)) }));
            hits = keyed.into_iter().map(|(_, _, i)| i).collect();
        } else {
            hits.par_sort_unstable_by(&cmp);
        }
        if let Some(d) = q.dupe {
            hits = self.dupes(hits, d);
        }
        let n = hits.len();
        (hits, n)
    }

    /// Finish the order `search_top` left partial: pass the unsorted tail.
    pub fn sort_rest(&self, tail: &mut [u32], q: &Query, sort: Sort, desc: bool) {
        tail.par_sort_unstable_by(self.order(q, sort, desc));
    }

    /// Typed a type (`.deb`, `excel`): those rank first, each part keeping the chosen order.
    fn boosted(&self, q: &Query, i: u32) -> bool {
        !q.boost.is_empty() && self.ext(i).is_some_and(|e| q.boost.iter().any(|b| b == e))
    }

    /// Result order as one total comparator, so a partial sort and a full one agree.
    fn order<'a>(&'a self, q: &'a Query, sort: Sort, desc: bool) -> impl Fn(&u32, &u32) -> std::cmp::Ordering + Sync + 'a {
        move |&a, &b| {
            let base = match sort {
                Sort::Name => self.lname(a).cmp(self.lname(b)),
                Sort::Size => self.entries[a as usize].size.cmp(&self.entries[b as usize].size),
                Sort::Mtime => self.entries[a as usize].mtime.cmp(&self.entries[b as usize].mtime),
                Sort::Ext => self.ext(a).cmp(&self.ext(b)).then(self.lname(a).cmp(self.lname(b))),
                Sort::Path => self.lpath(a).cmp(&self.lpath(b)),
            }
            .then(a.cmp(&b));
            self.boosted(q, b).cmp(&self.boosted(q, a)).then(if desc { base.reverse() } else { base })
        }
    }

    /// Keep hits whose key another hit shares; stable-sort so each group sits together.
    fn dupes(&self, hits: Vec<u32>, d: query::Dupe) -> Vec<u32> {
        use query::Dupe;
        let key = |i: u32| -> Option<(&[u8], u64)> {
            let e = &self.entries[i as usize];
            match d {
                Dupe::Name => Some((self.lname(i), 0)),
                // size dupes only make sense for non-empty files
                Dupe::Size => (e.kind == Kind::File && e.size > 0).then_some((&[][..], e.size)),
                Dupe::NameSize => (e.kind == Kind::File).then_some((self.lname(i), e.size)),
            }
        };
        let mut count: std::collections::HashMap<(&[u8], u64), u32> = std::collections::HashMap::new();
        for &i in &hits {
            if let Some(k) = key(i) {
                *count.entry(k).or_default() += 1;
            }
        }
        let mut out: Vec<u32> = hits.into_iter().filter(|&i| key(i).is_some_and(|k| count[&k] > 1)).collect();
        out.sort_by_key(|&i| key(i));
        out
    }

    /// Folders between this entry and `/` (a mount root counts its own path segments).
    pub fn depth(&self, i: u32) -> usize {
        let mut d = 0;
        let mut cur = i;
        loop {
            let p = self.entries[cur as usize].parent;
            if p == NO_PARENT {
                return d + self.name(cur).split(|&b| b == b'/').filter(|s| !s.is_empty()).count();
            }
            d += 1;
            cur = p;
        }
    }
}

#[cfg(test)]
mod read_as_tests {
    use super::*;

    #[test]
    fn content_reads_fail_closed() {
        let p = std::env::temp_dir().join(format!("kestrel-readas-{}", std::process::id()));
        std::fs::write(&p, "hello").unwrap();
        let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
        if uid == 0 {
            return; // the root path (switching to the caller) is covered by scripts/smoke-live.sh
        }
        // not root: nothing to drop, reads go through with our own rights and creds stay put
        READ_AS.set(Some(Cred { uid: uid + 1, gids: vec![gid] }));
        assert_eq!(read_as_caller(&p).unwrap(), b"hello");
        READ_AS.set(None);
        assert_eq!(unsafe { libc::setfsuid(u32::MAX) } as u32, uid);
        std::fs::remove_file(&p).unwrap();
    }
}
