//! ks — command-line search against kestrel-indexd (like Everything's es.exe).

use kestrel_index::Sort;
use kestrel_index::proto::{Request, Response, socket_path};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::process::exit;

const USAGE: &str = "usage: ks [-n N] [-s name|path|size|mtime|ext] [-r] [-l] [-0] [--json] [--scope DIR] [--count] QUERY...
  -n N     at most N results     -s KEY  sort (default name)   -r  reverse
  -l       show size and date    -0      NUL-separated paths   --json  raw response
  --scope DIR   only below DIR (. = here)                     --count  print the number of matches
query syntax: see `ks --syntax`";

const SYNTAX: &str = "a b      both            a|b     either            !a       not
\"a b\"    literal space   *.rs    wildcard (whole name)
a/b      match full path    path:x  match full path   case:X   case-sensitive
regex:^x file: folder:      ext:pdf;docx
size:>1gb size:1mb..10mb size:empty|tiny|small|medium|large|huge|gigantic
dm:today|yesterday|thisweek|thismonth|thisyear|last7days|2026-10|>2026-10-01
audio: video: pic: doc: zip: exe:";

fn main() {
    let mut req = Request::default();
    let (mut long, mut nul, mut json, mut count) = (false, false, false, false);
    let mut scope: Option<std::path::PathBuf> = None;
    let mut words = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                return;
            }
            "--syntax" => {
                println!("{SYNTAX}");
                return;
            }
            "-n" => req.limit = args.next().and_then(|n| n.parse().ok()),
            "-s" => {
                req.sort = match args.next().as_deref() {
                    Some("name") => Sort::Name,
                    Some("path") => Sort::Path,
                    Some("size") => Sort::Size,
                    Some("mtime" | "date") => Sort::Mtime,
                    Some("ext") => Sort::Ext,
                    _ => fail("-s takes name|path|size|mtime|ext"),
                }
            }
            "-r" => req.desc = true,
            "-l" => long = true,
            "-0" => nul = true,
            "--json" => json = true,
            "--count" => count = true,
            "--scope" => {
                let d = args.next().unwrap_or_else(|| fail("--scope takes a folder"));
                scope = Some(std::fs::canonicalize(&d).unwrap_or_else(|e| fail(&format!("--scope {d}: {e}"))));
            }
            _ => words.push(a),
        }
    }
    req.q = words.join(" ");
    if let Some(d) = &scope {
        req.q = format!("{} \"under:{}\"", req.q, d.display());
    }
    if count {
        req.limit = Some(0);
    }

    let sock = socket_path();
    let mut s = UnixStream::connect(&sock).unwrap_or_else(|e| fail(&format!("can't reach kestrel-indexd at {sock}: {e}")));
    serde_json::to_writer(&mut s, &req).unwrap();
    s.write_all(b"\n").unwrap();
    let mut line = String::new();
    BufReader::new(&s).read_line(&mut line).unwrap_or_else(|e| fail(&e.to_string()));
    if json {
        print!("{line}");
        return;
    }
    let resp: Response = serde_json::from_str(&line).unwrap_or_else(|e| fail(&format!("bad response: {e}")));
    if let Some(e) = resp.error {
        fail(&e);
    }
    if count {
        println!("{}", resp.total);
        exit(if resp.total == 0 { 1 } else { 0 });
    }
    let mut out = std::io::BufWriter::new(std::io::stdout().lock());
    for h in &resp.hits {
        let r = if nul {
            write!(out, "{}\0", h.path)
        } else if long {
            let size = if h.dir { "-".into() } else { human(h.size) };
            let date = chrono::DateTime::from_timestamp(h.mtime, 0)
                .map(|d| d.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_default();
            writeln!(out, "{size:>8}  {date}  {}", h.path)
        } else {
            writeln!(out, "{}", h.path)
        };
        if r.is_err() {
            exit(0); // stdout closed (| head)
        }
    }
    drop(out);
    exit(if resp.total == 0 { 1 } else { 0 });
}

fn human(n: u64) -> String {
    let mut v = n as f64;
    for u in ["B", "K", "M", "G", "T"] {
        if v < 1024.0 || u == "T" {
            return if u == "B" { format!("{n}B") } else { format!("{v:.1}{u}") };
        }
        v /= 1024.0;
    }
    unreachable!()
}

fn fail(msg: &str) -> ! {
    eprintln!("ks: {msg}");
    exit(2)
}
