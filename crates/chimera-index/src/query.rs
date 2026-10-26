//! Everything-style query language.
//!
//! `a b` AND, `a|b` OR (binds tighter than AND), `!a` NOT, `"a b"` literal with spaces.
//! Wildcards `*` `?` match the whole name. A term containing `/` matches the full path.
//! Modifiers: `case:` `nocase:` `regex:` `path:` `file:` `folder:` `ext:a;b` `size:` `dm:`,
//! macros `audio:` `video:` `pic:` `doc:` `zip:` `exe:`, `tag:red;blue`, `parent:/dir` (direct
//! children), `under:/dir` (everything below), `depth:2` `depth:<3`, `empty:` (empty folders),
//! `content:text` (reads files; slow), `dupe:` (same name), `sizedupe:` (same size),
//! `namesizedupe:` (both).
//! Ranges: `size:>1gb` `size:1mb..10mb` `size:empty|tiny|small|medium|large|huge|gigantic`,
//! `dm:today|yesterday|thisweek|thismonth|thisyear|last<N><day|week|month|year>s|2026|2026-10|2026-10-25`.

use crate::{Index, Kind};
use chrono::{Datelike, Duration, Local, NaiveDate, TimeZone};
use memchr::memmem::Finder;
use regex::bytes::Regex;
use std::os::unix::ffi::OsStrExt;

pub struct Query {
    root: Node,
    /// Keep only hits sharing this key with another hit, grouped together.
    pub dupe: Option<Dupe>,
    /// Some leaf opens files: permissions must be checked before matching, not after.
    pub reads: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dupe {
    Name,
    Size,
    NameSize,
}

/// Files bigger than this are skipped by `content:`.
// ponytail: whole-file read per candidate; stream + mmap if people grep multi-GB logs.
pub const CONTENT_MAX: u64 = 64 << 20;

enum Node {
    All(Vec<Node>),
    Any(Vec<Node>),
    Not(Box<Node>),
    Leaf(Leaf),
}

enum Leaf {
    Name(Pat),
    Path(Pat),
    Ext(Vec<Vec<u8>>),
    Size(i64, i64),
    Mtime(i64, i64),
    Folder(bool),
    Tag(u8),
    /// Direct children of this (lowercased) folder path.
    Parent(Vec<u8>),
    /// Anything below this (lowercased) folder path, with trailing `/`.
    Under(Vec<u8>),
    Depth(i64, i64),
    EmptyDir,
    Content(Finder<'static>, bool),
    Created(i64, i64),
    Accessed(i64, i64),
    /// Name length in characters.
    Len(i64, i64),
}

enum Pat {
    Sub(Finder<'static>, bool),
    Glob(Vec<u8>, bool),
    Re(Regex),
}

impl Pat {
    fn case(&self) -> bool {
        !matches!(self, Pat::Sub(_, false) | Pat::Glob(_, false))
    }

    fn is_match(&self, s: &[u8]) -> bool {
        match self {
            Pat::Sub(f, _) => f.find(s).is_some(),
            Pat::Glob(p, _) => glob(p, s),
            Pat::Re(r) => r.is_match(s),
        }
    }
}

impl Query {
    pub fn parse(s: &str) -> Result<Query, String> {
        let mut dupe = None;
        let mut toks = tokens(s).into_iter();
        let root = Node::All(seq(&mut toks, &mut dupe, false)?);
        Ok(Query {
            reads: cost(&root) >= 3,
            root,
            dupe,
        })
    }

    pub fn matches(&self, ix: &Index, i: u32) -> bool {
        eval(&self.root, ix, i)
    }
}

/// Terms up to the end (or the `>` closing this group): AND of terms, `|` binding tighter.
fn seq(toks: &mut std::vec::IntoIter<String>, dupe: &mut Option<Dupe>, in_group: bool) -> Result<Vec<Node>, String> {
    let mut items: Vec<Node> = Vec::new();
    let mut or_next = false;
    while let Some(t) = toks.next() {
        let n = match t.as_str() {
            "|" => {
                or_next = !items.is_empty();
                continue;
            }
            ">" if in_group => break,
            ">" => continue, // stray close: ignore
            "<" | "!<" => {
                let inner = Node::All(seq(toks, dupe, true)?);
                if t == "!<" { Node::Not(Box::new(inner)) } else { inner }
            }
            _ => match term(&t, dupe)? {
                Some(n) => n,
                None => continue,
            },
        };
        if or_next {
            let prev = items.pop().unwrap();
            items.push(match prev {
                Node::Any(mut v) => {
                    v.push(n);
                    Node::Any(v)
                }
                p => Node::Any(vec![p, n]),
            });
            or_next = false;
        } else {
            items.push(n);
        }
    }
    // cheap name checks first, path reconstruction later, file reads last
    items.sort_by_key(cost);
    Ok(items)
}

fn cost(n: &Node) -> u8 {
    match n {
        Node::Leaf(Leaf::Content(..)) => 3,
        Node::Leaf(Leaf::Path(_) | Leaf::Parent(_) | Leaf::Under(_)) => 2,
        Node::Leaf(Leaf::Name(Pat::Re(_))) => 1,
        Node::Leaf(_) => 0,
        Node::Not(n) => cost(n),
        Node::All(v) | Node::Any(v) => v.iter().map(cost).max().unwrap_or(0),
    }
}

fn eval(n: &Node, ix: &Index, i: u32) -> bool {
    match n {
        Node::All(v) => v.iter().all(|n| eval(n, ix, i)),
        Node::Any(v) => v.iter().any(|n| eval(n, ix, i)),
        Node::Not(n) => !eval(n, ix, i),
        Node::Leaf(l) => {
            let e = &ix.entries[i as usize];
            match l {
                Leaf::Name(p) => p.is_match(if p.case() { ix.name(i) } else { ix.lname(i) }),
                Leaf::Path(p) => p.is_match(&if p.case() { ix.path(i) } else { ix.lpath(i) }),
                Leaf::Ext(list) => ix.ext(i).is_some_and(|x| list.iter().any(|l| l == x)),
                Leaf::Size(lo, hi) => e.kind != Kind::Dir && (*lo..*hi).contains(&(e.size as i64)),
                Leaf::Mtime(lo, hi) => (*lo..*hi).contains(&e.mtime),
                Leaf::Created(lo, hi) => e.btime != 0 && (*lo..*hi).contains(&e.btime),
                Leaf::Accessed(lo, hi) => (*lo..*hi).contains(&e.atime),
                Leaf::Len(lo, hi) => {
                    let n = ix.name(i);
                    let chars = std::str::from_utf8(n).map_or(n.len(), |s| s.chars().count());
                    (*lo..*hi).contains(&(chars as i64))
                }
                Leaf::Folder(want) => (e.kind == Kind::Dir) == *want,
                Leaf::Tag(bits) => e.tags & bits != 0,
                Leaf::Parent(p) => e.parent != crate::NO_PARENT && ix.lpath(e.parent) == *p,
                Leaf::Under(p) => ix.lpath(i).starts_with(p),
                Leaf::Depth(lo, hi) => (*lo..*hi).contains(&(ix.depth(i) as i64)),
                Leaf::EmptyDir => e.kind == Kind::Dir && ix.children(i).next().is_none(),
                Leaf::Content(f, case) => {
                    e.kind == Kind::File
                        && e.size <= CONTENT_MAX
                        && std::fs::read(std::ffi::OsStr::from_bytes(&ix.path(i))).is_ok_and(|mut b| {
                            if !case {
                                b.make_ascii_lowercase();
                            }
                            f.find(&b).is_some()
                        })
                }
            }
        }
    }
}

/// Split on whitespace and `|`, keeping quotes together. `<` opening a word and `>` closing one
/// are group brackets (`<a|b> c`); inside a word (`size:>1mb`, `dm:<2026`) they're comparisons.
fn tokens(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let (mut quoted, mut any) = (false, false);
    let flush = |cur: &mut String, any: &mut bool, out: &mut Vec<String>| {
        if *any {
            out.push(std::mem::take(cur));
        }
        *any = false;
    };
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        let boundary = |n: Option<&char>| n.is_none_or(|n| n.is_whitespace() || *n == '|' || *n == '>');
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            '|' if !quoted => {
                flush(&mut cur, &mut any, &mut out);
                out.push("|".into());
            }
            '<' if !quoted && (cur.is_empty() || cur == "!") => {
                let neg = cur == "!";
                cur.clear();
                any = false;
                out.push(if neg { "!<".into() } else { "<".into() });
            }
            '>' if !quoted && !cur.ends_with(':') && boundary(chars.peek()) => {
                flush(&mut cur, &mut any, &mut out);
                out.push(">".into());
            }
            c if c.is_whitespace() && !quoted => flush(&mut cur, &mut any, &mut out),
            c => {
                cur.push(c);
                any = true;
            }
        }
    }
    flush(&mut cur, &mut any, &mut out);
    out
}

fn macro_exts(m: &str) -> Option<&'static str> {
    Some(match m {
        "audio" => "mp3;flac;wav;ogg;opus;m4a;aac;wma;aiff",
        "video" => "mp4;mkv;avi;mov;webm;wmv;m4v;mpg;mpeg;ts",
        "pic" => "jpg;jpeg;png;gif;bmp;webp;heic;tif;tiff;svg;avif",
        "doc" => "pdf;doc;docx;odt;txt;md;rtf;xls;xlsx;ods;ppt;pptx;odp;csv",
        "zip" => "zip;7z;rar;tar;gz;xz;bz2;zst;tgz",
        "exe" => "sh;appimage;deb;rpm;run;bin;exe;msi",
        _ => return None,
    })
}

fn ext_list(v: &str) -> Vec<Vec<u8>> {
    v.split(';')
        .filter(|s| !s.is_empty())
        .map(|s| s.trim_start_matches('.').to_lowercase().into_bytes())
        .collect()
}

fn term(t: &str, dupe: &mut Option<Dupe>) -> Result<Option<Node>, String> {
    let (neg, mut t) = match t.strip_prefix('!') {
        Some(r) => (true, r),
        None => (false, t),
    };
    let (mut case, mut re, mut path) = (false, false, false);
    let mut leaves = Vec::new();
    while let Some((k, v)) = t.split_once(':') {
        // `ext:` / `dm:` / `size:`… with nothing after it is a half-typed term: ignore it while typing
        const VALUED: &[&str] = &[
            "ext",
            "size",
            "dm",
            "datemodified",
            "dc",
            "datecreated",
            "da",
            "dateaccessed",
            "len",
            "depth",
            "content",
            "tag",
            "tags",
            "parent",
            "under",
        ];
        if v.is_empty() && VALUED.contains(&k.to_ascii_lowercase().as_str()) {
            t = "";
            break;
        }
        match k.to_ascii_lowercase().as_str() {
            "case" => case = true,
            "nocase" => case = false,
            "regex" => re = true,
            "path" => path = true,
            "file" | "files" => leaves.push(Leaf::Folder(false)),
            "folder" | "folders" => leaves.push(Leaf::Folder(true)),
            "ext" => {
                leaves.push(Leaf::Ext(ext_list(v)));
                t = "";
                break;
            }
            "size" => {
                let (lo, hi) = size(v).ok_or_else(|| format!("bad size: {v}"))?;
                leaves.push(Leaf::Size(lo, hi));
                t = "";
                break;
            }
            "dupe" | "sizedupe" | "namesizedupe" if v.is_empty() || k.eq_ignore_ascii_case("dupe") => {
                *dupe = Some(match (k.to_ascii_lowercase().as_str(), v.to_ascii_lowercase().as_str()) {
                    ("sizedupe", _) | ("dupe", "size") => Dupe::Size,
                    ("namesizedupe", _) | ("dupe", "namesize" | "name;size") => Dupe::NameSize,
                    _ => Dupe::Name,
                });
                t = "";
                break;
            }
            "empty" => {
                leaves.push(Leaf::EmptyDir);
                t = v;
                continue;
            }
            "depth" => {
                let (lo, hi) = range(v, |s| s.parse::<i64>().ok().map(|n| (n, n + 1))).ok_or_else(|| format!("bad depth: {v}"))?;
                leaves.push(Leaf::Depth(lo, hi));
                t = "";
                break;
            }
            "content" => {
                let p = if case { v.to_string() } else { v.to_ascii_lowercase() };
                if p.is_empty() {
                    return Err("content: needs text".into());
                }
                leaves.push(Leaf::Content(Finder::new(p.as_bytes()).into_owned(), case));
                t = "";
                break;
            }
            "tag" | "tags" => {
                let bits = crate::tag_bits(v);
                if bits == 0 {
                    return Err(format!("unknown tag: {v} (tags: {})", crate::TAGS.join(", ")));
                }
                leaves.push(Leaf::Tag(bits));
                t = "";
                break;
            }
            "parent" | "under" => {
                let p = v.trim_end_matches('/').to_lowercase();
                let p = if p.is_empty() { "/".to_string() } else { p };
                leaves.push(if k.eq_ignore_ascii_case("parent") {
                    Leaf::Parent(p.into_bytes())
                } else {
                    Leaf::Under(format!("{}/", p.trim_end_matches('/')).into_bytes())
                });
                t = "";
                break;
            }
            kk @ ("dm" | "datemodified" | "dc" | "datecreated" | "da" | "dateaccessed") => {
                let (lo, hi) = range(v, date).ok_or_else(|| format!("bad date: {v}"))?;
                leaves.push(match kk {
                    "dm" | "datemodified" => Leaf::Mtime(lo, hi),
                    "dc" | "datecreated" => Leaf::Created(lo, hi),
                    _ => Leaf::Accessed(lo, hi),
                });
                t = "";
                break;
            }
            "len" => {
                let (lo, hi) = range(v, |s| s.parse::<i64>().ok().map(|n| (n, n + 1))).ok_or_else(|| format!("bad length: {v}"))?;
                leaves.push(Leaf::Len(lo, hi));
                t = "";
                break;
            }
            m => match macro_exts(m) {
                Some(exts) => {
                    leaves.push(Leaf::Folder(false));
                    leaves.push(Leaf::Ext(ext_list(exts)));
                }
                None => break, // not a modifier: `foo:bar` is literal text
            },
        }
        t = v;
    }
    if !t.is_empty() {
        let pat = if re {
            let src = if case { t.to_string() } else { format!("(?i){t}") };
            Pat::Re(Regex::new(&src).map_err(|e| e.to_string())?)
        } else {
            let p = if case { t.to_string() } else { t.to_lowercase() };
            if p.contains(['*', '?']) {
                Pat::Glob(p.into_bytes(), case)
            } else {
                Pat::Sub(Finder::new(p.as_bytes()).into_owned(), case)
            }
        };
        leaves.push(if path || t.contains('/') { Leaf::Path(pat) } else { Leaf::Name(pat) });
    }
    if leaves.is_empty() {
        return Ok(None); // a pure flag like `dupe:`
    }
    let mut node = if leaves.len() == 1 {
        Node::Leaf(leaves.pop().unwrap())
    } else {
        Node::All(leaves.into_iter().map(Node::Leaf).collect())
    };
    if neg {
        node = Node::Not(Box::new(node));
    }
    Ok(Some(node))
}

/// `a..b`, `>a`, `>=a`, `<a`, `<=a`, `=a`, `a`, where `f` maps a value to its half-open span.
fn range(v: &str, f: impl Fn(&str) -> Option<(i64, i64)>) -> Option<(i64, i64)> {
    if let Some((a, b)) = v.split_once("..") {
        return Some((f(a)?.0, f(b)?.1));
    }
    for (op, pick) in [(">=", 0), ("<=", 1), (">", 2), ("<", 3), ("=", 4)] {
        if let Some(r) = v.strip_prefix(op) {
            let (a, b) = f(r)?;
            return Some(match pick {
                0 => (a, i64::MAX),
                1 => (i64::MIN, b),
                2 => (b, i64::MAX),
                3 => (i64::MIN, a),
                _ => (a, b),
            });
        }
    }
    f(v)
}

const K: i64 = 1024;

fn size(v: &str) -> Option<(i64, i64)> {
    Some(match v.to_ascii_lowercase().as_str() {
        "empty" => (0, 1),
        "tiny" => (0, 10 * K),
        "small" => (10 * K, 100 * K),
        "medium" => (100 * K, K * K),
        "large" => (K * K, 16 * K * K),
        "huge" => (16 * K * K, 128 * K * K),
        "gigantic" => (128 * K * K, i64::MAX),
        v => return range(v, |s| size_value(s).map(|n| (n, n + 1))),
    })
}

fn size_value(s: &str) -> Option<i64> {
    let s = s.to_ascii_lowercase();
    let split = s.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(s.len());
    let n: f64 = s[..split].parse().ok()?;
    let unit = match &s[split..] {
        "" | "b" => 1,
        "k" | "kb" => K,
        "m" | "mb" => K * K,
        "g" | "gb" => K * K * K,
        "t" | "tb" => K * K * K * K,
        _ => return None,
    };
    Some((n * unit as f64) as i64)
}

fn midnight(d: NaiveDate) -> i64 {
    Local
        .from_local_datetime(&d.and_hms_opt(0, 0, 0).unwrap())
        .earliest()
        .map_or(0, |t| t.timestamp())
}

fn date(v: &str) -> Option<(i64, i64)> {
    let now = Local::now();
    let today = now.date_naive();
    let day = Duration::days(1);
    let month_start = |y: i32, m: u32| NaiveDate::from_ymd_opt(y, m, 1);
    let next_month = |y: i32, m: u32| if m == 12 { month_start(y + 1, 1) } else { month_start(y, m + 1) };
    let v = v.to_ascii_lowercase();
    Some(match v.as_str() {
        "today" => (midnight(today), midnight(today + day)),
        "yesterday" => (midnight(today - day), midnight(today)),
        "thisweek" => {
            let mon = today - Duration::days(today.weekday().num_days_from_monday() as i64);
            (midnight(mon), midnight(mon + Duration::days(7)))
        }
        "thismonth" => (
            midnight(month_start(today.year(), today.month())?),
            midnight(next_month(today.year(), today.month())?),
        ),
        "thisyear" => (
            midnight(NaiveDate::from_ymd_opt(today.year(), 1, 1)?),
            midnight(NaiveDate::from_ymd_opt(today.year() + 1, 1, 1)?),
        ),
        v if v.starts_with("last") => {
            let r = &v[4..];
            let split = r.find(|c: char| !c.is_ascii_digit()).unwrap_or(r.len());
            let n: i64 = if split == 0 { 1 } else { r[..split].parse().ok()? };
            let days = match r[split..].trim_end_matches('s') {
                "day" => 1,
                "week" => 7,
                "month" => 30,
                "year" => 365,
                _ => return None,
            };
            (now.timestamp() - n * days * 86_400, i64::MAX)
        }
        v => {
            let p: Vec<&str> = v.split('-').collect();
            let y: i32 = p.first()?.parse().ok()?;
            match p.len() {
                1 => (midnight(NaiveDate::from_ymd_opt(y, 1, 1)?), midnight(NaiveDate::from_ymd_opt(y + 1, 1, 1)?)),
                2 => {
                    let m: u32 = p[1].parse().ok()?;
                    (midnight(month_start(y, m)?), midnight(next_month(y, m)?))
                }
                3 => {
                    let d = NaiveDate::from_ymd_opt(y, p[1].parse().ok()?, p[2].parse().ok()?)?;
                    (midnight(d), midnight(d + day))
                }
                _ => return None,
            }
        }
    })
}

fn glob(p: &[u8], s: &[u8]) -> bool {
    let (mut pi, mut si, mut star, mut mark) = (0, 0, None, 0);
    while si < s.len() {
        if pi < p.len() && (p[pi] == b'?' || p[pi] == s[si]) {
            pi += 1;
            si += 1;
        } else if pi < p.len() && p[pi] == b'*' {
            star = Some(pi);
            mark = si;
            pi += 1;
        } else if let Some(sp) = star {
            pi = sp + 1;
            mark += 1;
            si = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NO_PARENT, Sort};

    fn fixture() -> Index {
        let now = Local::now().timestamp();
        let mut ix = Index::default();
        let root = ix.push(NO_PARENT, b"/", Kind::Dir, 0, 0, 0o755, 0, 0);
        let home = ix.push(root, b"home", Kind::Dir, 0, 0, 0o755, 0, 0);
        let alex = ix.push(home, b"alex", Kind::Dir, 0, 0, 0o700, 1000, 1000);
        ix.push(alex, b"notes.txt", Kind::File, 100, now, 0o644, 1000, 1000);
        ix.push(alex, b"Photo.JPG", Kind::File, 2 << 20, 0, 0o644, 1000, 1000);
        ix.push(alex, b"my file.txt", Kind::File, 0, 0, 0o644, 1000, 1000);
        let proj = ix.push(alex, b"proj", Kind::Dir, 0, 0, 0o755, 1000, 1000);
        ix.push(proj, b"main.rs", Kind::File, 10, 0, 0o644, 1000, 1000);
        let etc = ix.push(root, b"etc", Kind::Dir, 0, 0, 0o755, 0, 0);
        ix.push(etc, b"hosts", Kind::File, 200, 0, 0o644, 0, 0);
        ix.entries[4].tags = crate::tag_bits("red,blue");
        ix
    }

    fn names(ix: &Index, q: &str) -> Vec<String> {
        let q = Query::parse(q).unwrap();
        ix.search(&q, Sort::Name, false, None)
            .into_iter()
            .map(|i| String::from_utf8_lossy(ix.name(i)).into_owned())
            .collect()
    }

    #[test]
    fn queries() {
        let ix = fixture();
        assert_eq!(ix.path(7), b"/home/alex/proj/main.rs");
        assert_eq!(names(&ix, "notes"), ["notes.txt"]);
        assert_eq!(names(&ix, "ext:jpg"), ["Photo.JPG"]);
        assert_eq!(names(&ix, "pic:"), ["Photo.JPG"]);
        assert!(names(&ix, "case:photo").is_empty());
        assert_eq!(names(&ix, "case:Photo"), ["Photo.JPG"]);
        assert_eq!(names(&ix, "*.rs"), ["main.rs"]);
        assert!(names(&ix, "*.r").is_empty(), "wildcards match the whole name");
        assert_eq!(names(&ix, "proj/main"), ["main.rs"]);
        assert_eq!(names(&ix, "path:alex ext:rs"), ["main.rs"]);
        assert_eq!(names(&ix, "notes | hosts"), ["hosts", "notes.txt"]);
        assert_eq!(names(&ix, "notes|hosts"), ["hosts", "notes.txt"]);
        assert_eq!(names(&ix, "file: !ext:txt"), ["hosts", "main.rs", "Photo.JPG"]);
        assert_eq!(names(&ix, "size:>1mb"), ["Photo.JPG"]);
        assert_eq!(names(&ix, "size:10..100"), ["main.rs", "notes.txt"]);
        assert_eq!(names(&ix, "size:empty"), ["my file.txt"]);
        assert_eq!(names(&ix, "folder: al"), ["alex"]);
        assert_eq!(names(&ix, r"regex:^m.*\.rs$"), ["main.rs"]);
        assert_eq!(names(&ix, "dm:today"), ["notes.txt"]);
        assert_eq!(names(&ix, "dm:last2days"), ["notes.txt"]);
        assert_eq!(
            names(&ix, "dm:<2000"),
            ["/", "alex", "etc", "home", "hosts", "main.rs", "my file.txt", "Photo.JPG", "proj"]
        );
        assert_eq!(names(&ix, "\"my file\""), ["my file.txt"]);
        assert_eq!(names(&ix, "foo:bar").len(), 0);
        assert!(Query::parse("size:>lots").is_err());
        assert!(Query::parse("regex:(").is_err());
        assert_eq!(names(&ix, "").len(), ix.len());
        assert_eq!(names(&ix, "parent:/home/alex file:"), ["my file.txt", "notes.txt", "Photo.JPG"]);
        assert_eq!(names(&ix, "under:/home/alex/ ext:rs"), ["main.rs"]);
        assert_eq!(names(&ix, "under:/home/al").len(), 0, "under: is a folder, not a prefix");
        assert_eq!(names(&ix, "parent:/").len(), 2);
        assert!(Query::parse("tag:pink").is_err());
        assert_eq!(names(&ix, "notes ext:"), ["notes.txt"], "empty value is ignored, not match-nothing");
        assert_eq!(names(&ix, "notes dm: size:"), ["notes.txt"]);
        // grouping
        assert_eq!(names(&ix, "<notes|hosts> file:"), ["hosts", "notes.txt"]);
        assert_eq!(names(&ix, "<ext:txt> !<my>"), ["notes.txt"]);
        assert_eq!(names(&ix, "size:>1mb"), ["Photo.JPG"], "> inside a word is still a comparison");
        assert_eq!(names(&ix, "<<notes>>"), ["notes.txt"], "nested groups");
        assert_eq!(names(&ix, "len:5"), ["hosts"]);
        assert_eq!(names(&ix, "len:4 folder:"), ["alex", "home", "proj"]);
        assert_eq!(names(&ix, "len:<4 folder:"), ["/", "etc"]);
        assert_eq!(names(&ix, "tag:blue"), ["Photo.JPG"]);
        assert_eq!(names(&ix, "tag:green;red"), ["Photo.JPG"]);
        assert!(names(&ix, "tag:green").is_empty());
    }

    #[test]
    fn dupes_depth_empty() {
        let mut ix = Index::default();
        let root = ix.push(NO_PARENT, b"/", Kind::Dir, 0, 0, 0o755, 0, 0);
        let a = ix.push(root, b"a", Kind::Dir, 0, 0, 0o755, 0, 0);
        let b = ix.push(root, b"b", Kind::Dir, 0, 0, 0o755, 0, 0);
        ix.push(root, b"void", Kind::Dir, 0, 0, 0o755, 0, 0);
        ix.push(a, b"Report.pdf", Kind::File, 500, 0, 0o644, 0, 0);
        ix.push(b, b"report.pdf", Kind::File, 500, 0, 0o644, 0, 0);
        ix.push(b, b"other.bin", Kind::File, 500, 0, 0o644, 0, 0);
        ix.push(a, b"unique.txt", Kind::File, 7, 0, 0o644, 0, 0);
        let mounted = ix.push(NO_PARENT, b"/boot/efi", Kind::Dir, 0, 0, 0o755, 0, 0);
        let efi = ix.push(mounted, b"BOOTX64.EFI", Kind::File, 9, 0, 0o644, 0, 0);
        let n = |q: &str| names(&ix, q);
        assert_eq!(n("dupe:"), ["Report.pdf", "report.pdf"], "case-insensitive name dupes");
        assert_eq!(n("sizedupe:").len(), 3);
        assert_eq!(
            n("dupe:size ext:bin|txt"),
            Vec::<String>::new(),
            "a single 500-byte .bin has no dupe among these hits"
        );
        assert_eq!(n("namesizedupe: report"), ["Report.pdf", "report.pdf"]);
        assert_eq!(n("empty:"), ["void"]);
        assert_eq!(n("depth:1 folder:"), ["a", "b", "void"]);
        assert_eq!(
            n("depth:>1"),
            ["/boot/efi", "BOOTX64.EFI", "other.bin", "Report.pdf", "report.pdf", "unique.txt"]
        );
        assert_eq!((ix.depth(mounted), ix.depth(efi)), (2, 3));
    }

    #[test]
    fn unicode_and_times() {
        let mut ix = Index::default();
        let root = ix.push(NO_PARENT, b"/", Kind::Dir, 0, 0, 0o755, 0, 0);
        let c = ix.push(root, "Écoles CAFÉ.txt".as_bytes(), Kind::File, 1, 0, 0o644, 0, 0);
        ix.push(root, "straße.md".as_bytes(), Kind::File, 1, 0, 0o644, 0, 0);
        let now = Local::now().timestamp();
        ix.entries[c as usize].btime = now;
        ix.entries[c as usize].atime = now;
        let n = |q: &str| names(&ix, q);
        assert_eq!(n("école"), ["Écoles CAFÉ.txt"], "Unicode case-insensitive");
        assert_eq!(n("café"), ["Écoles CAFÉ.txt"]);
        assert_eq!(n("STRASSE"), Vec::<String>::new(), "lowercasing, not full folding: ß stays ß");
        assert_eq!(n("STRAßE"), ["straße.md"]);
        assert_eq!(n("case:café"), Vec::<String>::new());
        assert_eq!(n("dc:today"), ["Écoles CAFÉ.txt"]);
        assert_eq!(n("da:today file:"), ["Écoles CAFÉ.txt"]);
        assert_eq!(n("dc:<2000"), Vec::<String>::new(), "unknown birth time never matches");
        assert_eq!(n("len:9"), ["straße.md"], "characters, not bytes");
    }

    #[test]
    fn sorting() {
        let ix = fixture();
        let q = Query::parse("file:").unwrap();
        let by_size: Vec<_> = ix.search(&q, Sort::Size, true, None).into_iter().map(|i| ix.entries[i as usize].size).collect();
        assert_eq!(by_size, [2 << 20, 200, 100, 10, 0]);
        let by_path: Vec<_> = ix
            .search(&q, Sort::Path, false, None)
            .into_iter()
            .map(|i| String::from_utf8_lossy(&ix.path(i)).into_owned())
            .collect();
        assert_eq!(
            by_path,
            [
                "/etc/hosts",
                "/home/alex/my file.txt",
                "/home/alex/notes.txt",
                "/home/alex/Photo.JPG",
                "/home/alex/proj/main.rs"
            ]
        );
    }

    #[test]
    fn visibility() {
        use crate::Cred;
        let ix = fixture();
        let q = Query::parse("file:").unwrap();
        let count = |c: Cred| ix.search(&q, Sort::Name, false, Some(&c)).len();
        assert_eq!(count(Cred { uid: 0, gids: vec![0] }), 5);
        assert_eq!(count(Cred { uid: 1000, gids: vec![1000] }), 5);
        // alex/ is 0700: another user sees /etc/hosts only, nothing under alex/ (incl. proj/main.rs)
        assert_eq!(count(Cred { uid: 1001, gids: vec![1001] }), 1);
        let dirs = Query::parse("folder: alex").unwrap();
        assert_eq!(
            ix.search(&dirs, Sort::Name, false, Some(&Cred { uid: 1001, gids: vec![] })).len(),
            1,
            "the 0700 dir itself is listable from home/"
        );
    }
}
