//! Kiwix .zim archives (offline Wikipedia and friends): just enough of the format
//! (https://wiki.openzim.org/wiki/ZIM_file_format) to show what's inside one: its metadata and
//! icon. Clusters come compressed with zstd (current) or xz (old files); the system `zstd` and `xz`
//! (both Depends) unpack them, so no decoder runs in this process.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

const MAGIC: u32 = 0x044D_495A;

/// What the preview shows.
#[derive(Default, Debug)]
pub struct Info {
    pub title: Option<String>,
    pub description: Option<String>,
    pub language: Option<String>,
    pub creator: Option<String>,
    pub publisher: Option<String>,
    pub date: Option<String>,
    /// Articles (text/html entries), from M/Counter.
    pub articles: Option<u64>,
    pub entries: u32,
    /// The 48×48 PNG from M/Illustration_48x48@1.
    pub icon: Option<Vec<u8>>,
}

/// A directory entry: namespace, url, and (cluster, blob) unless it's a redirect.
type Entry = (u8, String, Option<(u32, u32)>);

struct Zim {
    f: File,
    entries: u32,
    url_ptr: u64,
    cluster_ptr: u64,
    clusters: u32,
    checksum_pos: u64,
}

fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes(b[o..o + 2].try_into().unwrap())
}
fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn u64_at(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

fn bad(what: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, what.to_string())
}

impl Zim {
    fn open(path: &Path) -> std::io::Result<Zim> {
        let mut f = File::open(path)?;
        let mut h = [0u8; 80];
        f.read_exact(&mut h)?;
        if u32_at(&h, 0) != MAGIC {
            return Err(bad("not a ZIM file"));
        }
        Ok(Zim {
            entries: u32_at(&h, 24),
            clusters: u32_at(&h, 28),
            url_ptr: u64_at(&h, 32),
            cluster_ptr: u64_at(&h, 48),
            checksum_pos: u64_at(&h, 72),
            f,
        })
    }

    fn read_at(&mut self, pos: u64, buf: &mut [u8]) -> std::io::Result<usize> {
        self.f.seek(SeekFrom::Start(pos))?;
        let mut n = 0;
        while n < buf.len() {
            match self.f.read(&mut buf[n..])? {
                0 => break,
                k => n += k,
            }
        }
        Ok(n)
    }

    fn u64_at(&mut self, pos: u64) -> std::io::Result<u64> {
        let mut b = [0u8; 8];
        self.read_at(pos, &mut b)?;
        Ok(u64::from_le_bytes(b))
    }

    /// Entry `i` in url order: (namespace, url, cluster, blob); redirects have no blob.
    fn entry(&mut self, i: u32) -> std::io::Result<Entry> {
        let off = self.u64_at(self.url_ptr + 8 * i as u64)?;
        let mut d = [0u8; 512];
        let n = self.read_at(off, &mut d)?;
        let d = &d[..n];
        if d.len() < 16 {
            return Err(bad("short directory entry"));
        }
        let redirect = u16_at(d, 0) == 0xFFFF;
        let ns = d[3];
        let (blob, name_at) = if redirect { (None, 12) } else { (Some((u32_at(d, 8), u32_at(d, 12))), 16) };
        let url = d[name_at..].split(|b| *b == 0).next().unwrap_or_default();
        Ok((ns, String::from_utf8_lossy(url).into_owned(), blob))
    }

    /// One blob, unpacking its cluster if it's compressed.
    fn blob(&mut self, cluster: u32, blob: u32) -> std::io::Result<Vec<u8>> {
        if cluster >= self.clusters {
            return Err(bad("cluster out of range"));
        }
        let start = self.u64_at(self.cluster_ptr + 8 * cluster as u64)?;
        let end = if cluster + 1 < self.clusters {
            self.u64_at(self.cluster_ptr + 8 * (cluster as u64 + 1))?
        } else {
            self.checksum_pos
        };
        // ponytail: reads the whole cluster (default ~2 MB packed); fine for metadata, not for articles
        let len = end.checked_sub(start).filter(|l| *l <= 256 << 20).ok_or_else(|| bad("bad cluster size"))?;
        let mut raw = vec![0u8; len as usize];
        self.read_at(start, &mut raw)?;
        let (info, packed) = raw.split_first().ok_or_else(|| bad("empty cluster"))?;
        let data = match info & 0x0F {
            0 | 1 => packed.to_vec(),
            4 => unpack("xz", packed)?,
            5 => unpack("zstd", packed)?,
            _ => return Err(bad("unknown cluster compression")),
        };
        let wide = info & 0x10 != 0;
        let w = if wide { 8 } else { 4 };
        let at = |i: usize| -> Option<usize> {
            let b = data.get(i * w..i * w + w)?;
            Some(if wide {
                u64::from_le_bytes(b.try_into().ok()?) as usize
            } else {
                u32::from_le_bytes(b.try_into().ok()?) as usize
            })
        };
        let (a, b) = (at(blob as usize), at(blob as usize + 1));
        match (a, b) {
            (Some(a), Some(b)) if a <= b && b <= data.len() => Ok(data[a..b].to_vec()),
            _ => Err(bad("blob out of range")),
        }
    }
}

/// `zstd -dc` / `xz -dc` over a pipe; the writer gets its own thread so a full stdout pipe can't
/// deadlock against a full stdin pipe.
fn unpack(tool: &str, packed: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut child = std::process::Command::new(tool)
        .args(["-dc", "-q"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    let mut stdin = child.stdin.take().unwrap();
    let input = packed.to_vec();
    let writer = std::thread::spawn(move || stdin.write_all(&input));
    let mut out = Vec::new();
    child.stdout.take().unwrap().read_to_end(&mut out)?;
    let _ = writer.join();
    child.wait()?;
    Ok(out)
}

/// Reads a ZIM's metadata (namespace M) and icon. Blocking: a binary search over the directory,
/// then a handful of reads; well under a second even for an 180 GB file on a spinning disk.
pub fn info(path: &Path) -> std::io::Result<Info> {
    let mut z = Zim::open(path)?;
    // the first M entry: entries are sorted by namespace, then url
    let (mut lo, mut hi) = (0, z.entries);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if z.entry(mid)?.0 < b'M' {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    let mut out = Info {
        entries: z.entries,
        ..Default::default()
    };
    let text = |b: Vec<u8>| Some(String::from_utf8_lossy(&b).trim().to_string()).filter(|s| !s.is_empty());
    for i in lo..z.entries.min(lo + 64) {
        let (ns, url, blob) = z.entry(i)?;
        if ns != b'M' {
            break;
        }
        let Some((c, b)) = blob else { continue };
        // one unreadable item shouldn't hide the rest
        let Ok(v) = z.blob(c, b) else { continue };
        match url.as_str() {
            "Title" => out.title = text(v),
            "Description" => out.description = text(v),
            "Language" => out.language = text(v),
            "Creator" => out.creator = text(v),
            "Publisher" => out.publisher = text(v),
            "Date" => out.date = text(v),
            "Counter" => out.articles = text(v).and_then(|c| articles(&c)),
            "Illustration_48x48@1" => out.icon = Some(v),
            _ => {}
        }
    }
    Ok(out)
}

/// "text/html=12345;image/webp=678" -> 12345
fn articles(counter: &str) -> Option<u64> {
    counter
        .split(';')
        .find_map(|kv| kv.strip_prefix("text/html="))
        .and_then(|n| n.trim().parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal ZIM: one uncompressed cluster with the metadata, one zstd cluster with the title,
    /// so both cluster kinds and the directory search are exercised.
    fn tiny_zim(path: &Path) {
        let items: [(&str, &[u8], u32); 4] = [
            ("Counter", b"text/html=42;image/png=7", 0),
            ("Description", b"A tiny test archive", 0),
            ("Language", b"eng", 0),
            ("Title", b"Tiny Wiki", 1),
        ];
        let cluster = |blobs: &[&[u8]], zstd: bool| -> Vec<u8> {
            let mut offs = Vec::new();
            let mut at = 4 * (blobs.len() as u32 + 1);
            for b in blobs {
                offs.extend_from_slice(&at.to_le_bytes());
                at += b.len() as u32;
            }
            offs.extend_from_slice(&at.to_le_bytes());
            let mut body = offs;
            for b in blobs {
                body.extend_from_slice(b);
            }
            if zstd {
                let mut c = std::process::Command::new("zstd")
                    .args(["-c", "-q"])
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::piped())
                    .spawn()
                    .unwrap();
                c.stdin.take().unwrap().write_all(&body).unwrap();
                body = c.wait_with_output().unwrap().stdout;
            }
            let mut out = vec![if zstd { 5 } else { 1 }];
            out.extend(body);
            out
        };
        let c0 = cluster(&[items[0].1, items[1].1, items[2].1], false);
        let c1 = cluster(&[items[3].1], true);
        // dirents, with an article in namespace C before the metadata
        let mut dirents: Vec<Vec<u8>> = Vec::new();
        let mut blob_in = [0u32; 2];
        let mut article = vec![0, 0, 0, b'C', 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        article.extend_from_slice(b"index\0\0");
        dirents.push(article);
        for (url, _, c) in items {
            let b = blob_in[c as usize];
            blob_in[c as usize] += 1;
            let mut d = vec![0, 0, 0, b'M', 0, 0, 0, 0];
            d.extend_from_slice(&c.to_le_bytes());
            d.extend_from_slice(&b.to_le_bytes());
            d.extend_from_slice(url.as_bytes());
            d.extend_from_slice(b"\0\0");
            dirents.push(d);
        }
        let mut f = vec![0u8; 80];
        let mut dir_offs = Vec::new();
        for d in &dirents {
            dir_offs.push(f.len() as u64);
            f.extend_from_slice(d);
        }
        let url_ptr = f.len() as u64;
        for o in &dir_offs {
            f.extend_from_slice(&o.to_le_bytes());
        }
        let cluster_ptr = f.len() as u64;
        let c0_at = cluster_ptr + 16;
        f.extend_from_slice(&c0_at.to_le_bytes());
        f.extend_from_slice(&(c0_at + c0.len() as u64).to_le_bytes());
        f.extend(&c0);
        f.extend(&c1);
        let checksum = f.len() as u64;
        f.extend_from_slice(&[0u8; 16]);
        f[0..4].copy_from_slice(&MAGIC.to_le_bytes());
        f[4..6].copy_from_slice(&6u16.to_le_bytes());
        f[24..28].copy_from_slice(&(dirents.len() as u32).to_le_bytes());
        f[28..32].copy_from_slice(&2u32.to_le_bytes());
        f[32..40].copy_from_slice(&url_ptr.to_le_bytes());
        f[48..56].copy_from_slice(&cluster_ptr.to_le_bytes());
        f[72..80].copy_from_slice(&checksum.to_le_bytes());
        std::fs::write(path, f).unwrap();
    }

    #[test]
    fn reads_metadata_from_both_cluster_kinds() {
        let dir = std::env::temp_dir().join(format!("kestrel-zim-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("tiny.zim");
        tiny_zim(&p);
        let i = info(&p).unwrap();
        assert_eq!(i.title.as_deref(), Some("Tiny Wiki")); // zstd cluster
        assert_eq!(i.description.as_deref(), Some("A tiny test archive"));
        assert_eq!(i.language.as_deref(), Some("eng"));
        assert_eq!(i.articles, Some(42));
        assert_eq!(i.entries, 5);
        assert!(info(&dir.join("missing.zim")).is_err());
        std::fs::write(dir.join("fake.zim"), [0u8; 100]).unwrap();
        assert!(info(&dir.join("fake.zim")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// KESTREL_ZIM=/path/a.zim:/path/b.zim cargo test -p kestrel real_zims -- --ignored --nocapture
    #[test]
    #[ignore]
    fn real_zims() {
        for p in std::env::var("KESTREL_ZIM").unwrap_or_default().split(':').filter(|p| !p.is_empty()) {
            let t = std::time::Instant::now();
            let i = info(Path::new(p)).unwrap();
            println!(
                "{p}: {:?} {:?} articles={:?} lang={:?} date={:?} icon={} ({:.0?})",
                i.title,
                i.description,
                i.articles,
                i.language,
                i.date,
                i.icon.map_or(0, |v| v.len()),
                t.elapsed()
            );
            assert!(i.title.is_some());
        }
    }
}
