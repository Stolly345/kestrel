//! Save the index to disk and load it back, so search works within a second of boot instead of
//! after a full scan. The daemon still rescans in the background to catch changes made while it
//! was down; the snapshot only bridges that gap.
//!
//! Format: little-endian, field by field (no layout tricks), versioned. Loading validates every
//! link and offset, so a damaged file is rejected instead of crashing searches later.

use crate::{AclEntry, Entry, Index, Kind, NO_PARENT};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

const MAGIC: &[u8; 8] = b"CHIMIDX\0";
const VERSION: u32 = 1;

struct W<T: Write>(T);

impl<T: Write> W<T> {
    fn u8(&mut self, v: u8) -> io::Result<()> {
        self.0.write_all(&[v])
    }
    fn u16(&mut self, v: u16) -> io::Result<()> {
        self.0.write_all(&v.to_le_bytes())
    }
    fn u32(&mut self, v: u32) -> io::Result<()> {
        self.0.write_all(&v.to_le_bytes())
    }
    fn u64(&mut self, v: u64) -> io::Result<()> {
        self.0.write_all(&v.to_le_bytes())
    }
    fn i64(&mut self, v: i64) -> io::Result<()> {
        self.0.write_all(&v.to_le_bytes())
    }
    fn bytes(&mut self, b: &[u8]) -> io::Result<()> {
        self.u64(b.len() as u64)?;
        self.0.write_all(b)
    }
}

struct R<T: Read>(T);

impl<T: Read> R<T> {
    fn arr<const N: usize>(&mut self) -> io::Result<[u8; N]> {
        let mut b = [0u8; N];
        self.0.read_exact(&mut b)?;
        Ok(b)
    }
    fn u8(&mut self) -> io::Result<u8> {
        Ok(self.arr::<1>()?[0])
    }
    fn u16(&mut self) -> io::Result<u16> {
        Ok(u16::from_le_bytes(self.arr()?))
    }
    fn u32(&mut self) -> io::Result<u32> {
        Ok(u32::from_le_bytes(self.arr()?))
    }
    fn u64(&mut self) -> io::Result<u64> {
        Ok(u64::from_le_bytes(self.arr()?))
    }
    fn i64(&mut self) -> io::Result<i64> {
        Ok(i64::from_le_bytes(self.arr()?))
    }
    /// Length-prefixed bytes, refusing absurd lengths from a corrupt file.
    fn bytes(&mut self, max: u64) -> io::Result<Vec<u8>> {
        let n = self.u64()?;
        if n > max {
            return Err(bad("length out of range"));
        }
        let mut v = vec![0u8; n as usize];
        self.0.read_exact(&mut v)?;
        Ok(v)
    }
}

fn bad(why: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("bad index snapshot: {why}"))
}

/// Write atomically (temp file + rename), readable by root only: it lists every filename.
pub fn save(ix: &Index, path: &Path) -> io::Result<()> {
    let tmp = path.with_extension("tmp");
    let f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
    let mut w = W(BufWriter::with_capacity(1 << 20, f));
    w.0.write_all(MAGIC)?;
    w.u32(VERSION)?;
    w.u64(ix.entries.len() as u64)?;
    for e in &ix.entries {
        w.u32(e.name_off)?;
        w.u16(e.name_len)?;
        w.u16(e.lname_len)?;
        w.u32(e.lname_off)?;
        w.u8(e.kind as u8)?;
        w.u32(e.parent)?;
        w.u32(e.first_child)?;
        w.u32(e.next_sibling)?;
        w.u64(e.size)?;
        w.i64(e.mtime)?;
        w.u32(e.mode)?;
        w.u32(e.uid)?;
        w.u32(e.gid)?;
        w.u8(e.tags)?;
        w.i64(e.atime)?;
        w.i64(e.btime)?;
    }
    w.bytes(&ix.names)?;
    w.bytes(&ix.lower)?;
    w.u64(ix.roots.len() as u64)?;
    for &r in &ix.roots {
        w.u32(r)?;
    }
    w.u64(ix.acls.len() as u64)?;
    for (&id, acl) in &ix.acls {
        w.u32(id)?;
        w.u16(acl.len() as u16)?;
        for a in acl.iter() {
            w.u16(a.tag)?;
            w.u16(a.perm)?;
            w.u32(a.id)?;
        }
    }
    w.u64(ix.gone as u64)?;
    w.0.into_inner().map_err(|e| e.into_error())?.sync_all()?;
    std::fs::rename(&tmp, path)
}

fn kind(k: u8) -> io::Result<Kind> {
    Ok(match k {
        0 => Kind::File,
        1 => Kind::Dir,
        2 => Kind::Symlink,
        3 => Kind::Other,
        4 => Kind::Gone,
        _ => return Err(bad("unknown entry kind")),
    })
}

/// Load and validate. The caller gives it a fresh epoch (it's a different index from any
/// result a client may have cached).
pub fn load(path: &Path) -> io::Result<Index> {
    let mut r = R(BufReader::with_capacity(1 << 20, std::fs::File::open(path)?));
    if &r.arr::<8>()? != MAGIC {
        return Err(bad("not a snapshot"));
    }
    if r.u32()? != VERSION {
        return Err(bad("other version"));
    }
    let n = r.u64()?;
    if n >= NO_PARENT as u64 {
        return Err(bad("too many entries"));
    }
    let mut entries = Vec::with_capacity(n as usize);
    for _ in 0..n {
        entries.push(Entry {
            name_off: r.u32()?,
            name_len: r.u16()?,
            lname_len: r.u16()?,
            lname_off: r.u32()?,
            kind: kind(r.u8()?)?,
            parent: r.u32()?,
            first_child: r.u32()?,
            next_sibling: r.u32()?,
            size: r.u64()?,
            mtime: r.i64()?,
            mode: r.u32()?,
            uid: r.u32()?,
            gid: r.u32()?,
            tags: r.u8()?,
            atime: r.i64()?,
            btime: r.i64()?,
        });
    }
    let names = r.bytes(u32::MAX as u64)?;
    let lower = r.bytes(u32::MAX as u64)?;
    let nroots = r.u64()?;
    let mut roots = Vec::new();
    for _ in 0..nroots.min(n) {
        roots.push(r.u32()?);
    }
    let nacl = r.u64()?;
    let mut acls = std::collections::HashMap::new();
    for _ in 0..nacl.min(n) {
        let id = r.u32()?;
        let len = r.u16()?;
        let mut v = Vec::with_capacity(len as usize);
        for _ in 0..len {
            v.push(AclEntry {
                tag: r.u16()?,
                perm: r.u16()?,
                id: r.u32()?,
            });
        }
        acls.insert(id, v.into_boxed_slice());
    }
    let gone = r.u64()? as usize;

    // every link and offset must point inside the index, or searches would panic later
    let link_ok = |l: u32| l == NO_PARENT || (l as u64) < n;
    for e in &entries {
        let names_ok = e.name_off as usize + e.name_len as usize <= names.len();
        let lower_ok = e.lname_off as usize + e.lname_len as usize <= lower.len();
        if !(names_ok && lower_ok && link_ok(e.parent) && link_ok(e.first_child) && link_ok(e.next_sibling)) {
            return Err(bad("entry points outside the index"));
        }
    }
    if roots.len() as u64 != nroots || roots.iter().any(|&x| x as u64 >= n) || acls.keys().any(|&x| x as u64 >= n) {
        return Err(bad("root or ACL out of range"));
    }
    Ok(Index {
        entries,
        names,
        lower,
        acls,
        roots,
        gone,
        epoch: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Query, Sort};

    #[test]
    fn roundtrip_and_corruption() {
        let d = std::env::temp_dir().join(format!("chimera-snap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("tree/Écoles")).unwrap();
        std::fs::write(d.join("tree/Écoles/report.pdf"), "x").unwrap();
        std::fs::write(d.join("tree/notes.txt"), "hello").unwrap();
        let mut ix = Index::default();
        crate::scan::scan(&mut ix, &d.join("tree"), &[]).unwrap();
        let snap = d.join("index.bin");
        save(&ix, &snap).unwrap();
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&snap).unwrap().permissions().mode() & 0o777, 0o600, "root-only");

        let back = load(&snap).unwrap();
        assert_eq!(back.len(), ix.len());
        let q = Query::parse("écoles/ ext:pdf").unwrap(); // path match, Unicode-insensitive
        let hits = back.search(&q, Sort::Name, false, None);
        assert_eq!(hits.len(), 1);
        assert!(String::from_utf8_lossy(&back.path(hits[0])).ends_with("tree/Écoles/report.pdf"));
        assert!(back.lookup(&d.join("tree/notes.txt")).is_some(), "roots and child links survive");

        // damaged files are rejected, never trusted
        let mut bytes = std::fs::read(&snap).unwrap();
        bytes.truncate(bytes.len() / 2);
        std::fs::write(&snap, &bytes).unwrap();
        assert!(load(&snap).is_err(), "truncated");
        let mut bytes = std::fs::read(&snap).unwrap();
        bytes[8] = 99;
        std::fs::write(&snap, &bytes).unwrap();
        assert!(load(&snap).is_err(), "wrong version");
        save(&ix, &snap).unwrap();
        let mut bytes = std::fs::read(&snap).unwrap();
        // first entry's parent field (after magic 8 + version 4 + count 8 + 4+2+2+4+1 bytes)
        bytes[8 + 4 + 8 + 13..8 + 4 + 8 + 17].copy_from_slice(&123_456u32.to_le_bytes());
        std::fs::write(&snap, &bytes).unwrap();
        assert!(load(&snap).is_err(), "dangling parent link");
        std::fs::remove_dir_all(&d).unwrap();
    }
}
