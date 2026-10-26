//! Everything-style HTTP search page + JSON API. Off unless `--http ADDR --http-user NAME`.
//! HTTP has no peer credentials, so every request searches as that one named user.
// ponytail: search only, no file downloads; serving file bodies from a root daemon needs more care.

use kestrel_index::proto::{Hit, Response};
use kestrel_index::{Cred, Index, Kind, Query, Sort};
use std::ffi::CString;
use std::io::{BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, RwLock};

/// uid + all groups of a login name, the way the kernel would see that user.
pub fn user_cred(name: &str) -> Option<Cred> {
    let c = CString::new(name).ok()?;
    let pw = unsafe { libc::getpwnam(c.as_ptr()) };
    if pw.is_null() {
        return None;
    }
    let (uid, gid) = unsafe { ((*pw).pw_uid, (*pw).pw_gid) };
    let mut n: libc::c_int = 64;
    let mut gids = vec![0 as libc::gid_t; n as usize];
    if unsafe { libc::getgrouplist(c.as_ptr(), gid, gids.as_mut_ptr(), &mut n) } < 0 {
        gids.resize(n as usize, 0);
        unsafe { libc::getgrouplist(c.as_ptr(), gid, gids.as_mut_ptr(), &mut n) };
    }
    gids.truncate(n.max(0) as usize);
    Some(Cred { uid, gids })
}

fn url_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => match ((b[i + 1] as char).to_digit(16), (b[i + 2] as char).to_digit(16)) {
                (Some(hi), Some(lo)) => {
                    out.push((hi * 16 + lo) as u8);
                    i += 2;
                }
                _ => out.push(b'%'),
            },
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn param(query: &str, key: &str) -> Option<String> {
    query
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| url_decode(v))
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn run(ix: &RwLock<Index>, cred: &Cred, q: &str, sort: Sort, desc: bool, limit: usize) -> Response {
    let query = match Query::parse(q) {
        Ok(q) => q,
        Err(e) => {
            return Response {
                error: Some(e),
                ..Default::default()
            };
        }
    };
    let ix = ix.read().unwrap();
    if ix.epoch == 0 {
        return Response {
            indexing: true,
            error: Some("Still indexing — try again in a moment".into()),
            ..Default::default()
        };
    }
    let hits = ix.search(&query, sort, desc, Some(cred));
    let bytes = hits
        .iter()
        .map(|&i| &ix.entries[i as usize])
        .filter(|e| e.kind == Kind::File)
        .map(|e| e.size)
        .sum();
    let out = hits
        .iter()
        .take(limit)
        .map(|&i| {
            let e = &ix.entries[i as usize];
            Hit {
                path: String::from_utf8_lossy(&ix.path(i)).into_owned(),
                size: e.size,
                mtime: e.mtime,
                dir: e.kind == Kind::Dir,
            }
        })
        .collect();
    Response {
        total: hits.len(),
        bytes,
        hits: out,
        error: None,
        indexing: false,
    }
}

fn page(q: &str, r: Option<&Response>) -> String {
    let mut h = format!(
        "<!doctype html><meta charset=utf-8><meta name=viewport content='width=device-width'><title>Kestrel search</title>\
         <style>body{{font:15px system-ui;margin:16px;max-width:1100px}}input{{font:inherit;width:70%;padding:6px}}\
         table{{border-collapse:collapse;width:100%;margin-top:12px}}td{{padding:3px 8px;border-bottom:1px solid #8883}}\
         td.n{{text-align:right;white-space:nowrap}}.err{{color:#c01c28}}</style>\
         <form><input name=q autofocus value=\"{}\" placeholder='ext:pdf invoice'> <button>Search</button></form>",
        esc(q)
    );
    match r {
        Some(Response { error: Some(e), .. }) => h += &format!("<p class=err>{}</p>", esc(e)),
        Some(r) => {
            h += &format!("<p>{} results</p><table>", r.total);
            for hit in &r.hits {
                h += &format!(
                    "<tr><td>{}{}</td><td class=n>{}</td></tr>",
                    esc(&hit.path),
                    if hit.dir { "/" } else { "" },
                    if hit.dir { String::new() } else { hit.size.to_string() }
                );
            }
            h += "</table>";
        }
        None => {}
    }
    h
}

fn handle(mut s: TcpStream, ix: &RwLock<Index>, cred: &Cred) -> std::io::Result<()> {
    let mut r = BufReader::new(s.try_clone()?);
    let mut raw = Vec::new();
    crate::read_line_limited(&mut r, &mut raw)?;
    let line = String::from_utf8_lossy(&raw).into_owned();
    // drain headers (each bounded; a client can't make us buffer without end)
    let mut h = Vec::new();
    while crate::read_line_limited(&mut r, &mut h)? > 2 {}
    let target = line.split_whitespace().nth(1).unwrap_or("/");
    let (path, qs) = target.split_once('?').unwrap_or((target, ""));
    let q = param(qs, "q").unwrap_or_default();
    let sort = match param(qs, "sort").as_deref() {
        Some("path") => Sort::Path,
        Some("size") => Sort::Size,
        Some("mtime") => Sort::Mtime,
        Some("ext") => Sort::Ext,
        _ => Sort::Name,
    };
    let desc = param(qs, "desc").is_some_and(|d| d == "1");
    let limit = param(qs, "limit").and_then(|l| l.parse().ok()).unwrap_or(200).min(100_000);
    let (status, ctype, body) = match path {
        "/" => {
            let resp = (!q.is_empty()).then(|| run(ix, cred, &q, sort, desc, limit));
            ("200 OK", "text/html; charset=utf-8", page(&q, resp.as_ref()))
        }
        "/api" => (
            "200 OK",
            "application/json",
            serde_json::to_string(&run(ix, cred, &q, sort, desc, limit)).unwrap(),
        ),
        _ => ("404 Not Found", "text/plain", "not found".into()),
    };
    write!(
        s,
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\nX-Content-Type-Options: nosniff\r\n\r\n{body}",
        body.len()
    )
}

pub fn serve(addr: &str, user: &str, ix: Arc<RwLock<Index>>) -> Result<(), String> {
    let cred = user_cred(user).ok_or_else(|| format!("no such user: {user}"))?;
    if cred.uid == 0 {
        return Err("--http-user root would expose every filename; pick a normal user".into());
    }
    let l = TcpListener::bind(addr).map_err(|e| format!("http bind {addr}: {e}"))?;
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let (ix, cred) = (
                ix.clone(),
                Cred {
                    uid: cred.uid,
                    gids: cred.gids.clone(),
                },
            );
            std::thread::spawn(move || {
                let _ = handle(s, &ix, &cred);
            });
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn decode() {
        assert_eq!(super::url_decode("ext%3Apdf+invoice%7Creceipt"), "ext:pdf invoice|receipt");
        assert_eq!(super::url_decode("100%"), "100%");
        assert_eq!(super::url_decode("%é"), "%é", "no panic on non-ASCII after %");
        assert_eq!(super::param("a=1&q=x%20y", "q").as_deref(), Some("x y"));
        assert_eq!(super::esc("<a href=\"x\">&"), "&lt;a href=&quot;x&quot;&gt;&amp;");
    }
}
