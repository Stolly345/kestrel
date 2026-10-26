//! chimera-indexd: indexes every local filesystem, keeps it live via fanotify, answers
//! queries on a Unix socket filtered to what the caller could see themselves.

mod fan;
mod http;

use chimera_index::proto::{Hit, Request, Response, socket_path};
use chimera_index::scan::{load_excludes, local_mounts, reconcile, scan_all};
use chimera_index::{Cred, Index, Kind, Query, Sort};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Instant;

const EXCLUDES_FILE: &str = "/etc/chimera/excludes";
/// systemd's StateDirectory=chimera (0700, root). CHIMERA_STATE overrides it for testing.
const STATE_FILE: &str = "/var/lib/chimera/index.bin";
/// Save at most this often while files keep changing (plus always on shutdown).
const SAVE_EVERY: std::time::Duration = std::time::Duration::from_secs(600);

fn state_file() -> PathBuf {
    std::env::var_os("CHIMERA_STATE").map_or_else(|| PathBuf::from(STATE_FILE), PathBuf::from)
}

fn now_nanos() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(1, |d| d.as_nanos() as u64)
}

/// Write the index to disk (skipped until the first scan or a snapshot load has given us one).
fn save_snapshot(ix: &RwLock<Index>) {
    let t = Instant::now();
    let r = ix.read().unwrap();
    if r.epoch == 0 {
        return;
    }
    let path = state_file();
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    match chimera_index::snapshot::save(&r, &path) {
        Ok(()) => log(format_args!("saved index snapshot ({} entries, {:.1?})", r.len(), t.elapsed())),
        Err(e) => log(format_args!("couldn't save snapshot to {}: {e}", path.display())),
    }
}

/// SIGTERM (systemd stop, shutdown) and SIGINT: save, then exit. Must be called before any other
/// thread starts, so they all inherit the blocked mask and only this thread receives the signal.
fn handle_shutdown(ix: Arc<RwLock<Index>>) {
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGTERM);
        libc::sigaddset(&mut set, libc::SIGINT);
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
        std::thread::spawn(move || {
            let mut sig = 0;
            libc::sigwait(&set, &mut sig);
            log("stopping: saving index");
            save_snapshot(&ix);
            std::process::exit(0);
        });
    }
}

fn log(msg: impl std::fmt::Display) {
    eprintln!("chimera-indexd: {msg}");
}

fn rescan(excludes: &[PathBuf]) -> Index {
    let t = Instant::now();
    let (mut ix, s) = scan_all(excludes);
    ix.shrink();
    // the scan's temporary maps are freed by now; hand that memory back to the OS (was ~550 MB kept)
    unsafe { libc::malloc_trim(0) };
    log(format_args!(
        "indexed {} entries in {:.1?} ({} errors, ~{} MB)",
        ix.len(),
        t.elapsed(),
        s.errors,
        ix.approx_bytes() >> 20
    ));
    ix
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let watch = !args.iter().any(|a| a == "--no-watch");
    let excludes = load_excludes(EXCLUDES_FILE.as_ref());
    log(format_args!("excludes: {excludes:?}"));

    // watch before the first scan so nothing changing during it is missed; events queue in the kernel meanwhile
    let fan = watch.then(|| match fan::Fan::new(&local_mounts(&excludes)) {
        Ok(f) => f,
        Err(e) => {
            log(format_args!("fanotify failed ({e}); needs root. Run with --no-watch for a static index."));
            std::process::exit(1);
        }
    });
    // answer on the socket right away. Last run's snapshot makes search work immediately; without
    // one, the index has epoch 0 = "indexing" until the first scan lands.
    let ix = Arc::new(RwLock::new(Index::default()));
    handle_shutdown(ix.clone());
    let t = Instant::now();
    match chimera_index::snapshot::load(&state_file()) {
        Ok(mut snap) => {
            snap.epoch = now_nanos();
            let age = std::fs::metadata(state_file())
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|m| m.elapsed().ok());
            log(format_args!(
                "loaded snapshot: {} entries in {:.1?} (saved {} min ago); rescanning in the background",
                snap.len(),
                t.elapsed(),
                age.map_or(0, |a| a.as_secs() / 60)
            ));
            *ix.write().unwrap() = snap;
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => log(format_args!("ignoring snapshot: {e}")),
    }
    let changes = Arc::new(std::sync::atomic::AtomicU64::new(0));

    let flag = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    if let Some(addr) = flag("--http") {
        let Some(user) = flag("--http-user") else {
            log("--http needs --http-user NAME: results are filtered to what that user may see");
            std::process::exit(1);
        };
        if let Err(e) = http::serve(&addr, &user, ix.clone()) {
            log(e);
            std::process::exit(1);
        }
        log(format_args!("http search on http://{addr}/ as {user}"));
    }

    let (ix2, excludes2, changes2) = (ix.clone(), excludes.clone(), changes.clone());
    std::thread::spawn(move || {
        let fresh = rescan(&excludes2);
        *ix2.write().unwrap() = fresh;
        changes2.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if let Some(fan) = fan {
            watch_loop(fan, ix2, excludes2, changes2);
        }
    });
    // periodic save, so a crash or power cut loses at most SAVE_EVERY of changes
    let (ix3, changes3) = (ix.clone(), changes.clone());
    std::thread::spawn(move || {
        let mut saved = 0;
        loop {
            std::thread::sleep(SAVE_EVERY);
            let now = changes3.load(std::sync::atomic::Ordering::Relaxed);
            if now != saved {
                save_snapshot(&ix3);
                saved = now;
            }
        }
    });
    serve(ix);
}

/// Index drives that appeared, drop ones that went away (USB sticks, SD cards, external disks).
// ponytail: scans a new drive under the write lock; queries wait for it (~1 s per 100k files).
fn sync_mounts(fan: &mut fan::Fan, ix: &RwLock<Index>, excludes: &[PathBuf]) {
    let now = local_mounts(excludes);
    let have: Vec<PathBuf> = ix.read().unwrap().roots().map(|r| PathBuf::from(std::ffi::OsStr::from_bytes(r))).collect();
    for gone in have.iter().filter(|h| !now.contains(h)) {
        fan.forget(gone);
        if ix.write().unwrap().remove_root(gone) {
            log(format_args!("unmounted: {}", gone.display()));
        }
    }
    for new in now.iter().filter(|n| !have.contains(n)) {
        if let Err(e) = fan.add(new) {
            log(format_args!("can't watch {}: {e}", new.display()));
            continue;
        }
        let t = Instant::now();
        let mut w = ix.write().unwrap();
        match chimera_index::scan::scan(&mut w, new, excludes) {
            Ok(s) => log(format_args!("mounted: {} ({} entries in {:.1?})", new.display(), s.entries, t.elapsed())),
            Err(e) => log(format_args!("can't scan {}: {e}", new.display())),
        }
    }
}

/// After a lost event, rescan, but not more often than this (CHIMERA_LOST_GAP_SECS overrides, for
/// tests): a build that churns thousands of temp folders can lose many events in a row, and a rescan
/// costs ~20 s of CPU.
fn lost_rescan_gap() -> std::time::Duration {
    let secs = std::env::var("CHIMERA_LOST_GAP_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(600);
    std::time::Duration::from_secs(secs)
}

fn watch_loop(mut fan: fan::Fan, ix: Arc<RwLock<Index>>, excludes: Vec<PathBuf>, changes: Arc<std::sync::atomic::AtomicU64>) {
    // /proc/self/mounts signals POLLPRI whenever something is mounted or unmounted
    let mounts = std::fs::File::open("/proc/self/mounts").ok();
    let mut last_rescan = Instant::now();
    let mut repair_pending = false;
    let gap = lost_rescan_gap();
    loop {
        // a pending repair rescan runs once the gap since the last rescan has passed
        if repair_pending && last_rescan.elapsed() >= gap {
            log("rescanning to pick up changes whose events were lost");
            let fresh = rescan(&excludes);
            *ix.write().unwrap() = fresh;
            changes.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            (last_rescan, repair_pending) = (Instant::now(), false);
        }
        let timeout = if repair_pending {
            gap.saturating_sub(last_rescan.elapsed()).as_millis().max(1) as i32
        } else {
            -1
        };
        let mut fds = vec![libc::pollfd {
            fd: fan.raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        }];
        if let Some(m) = &mounts {
            fds.push(libc::pollfd {
                fd: m.as_raw_fd(),
                events: libc::POLLPRI,
                revents: 0,
            });
        }
        if unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout) } < 0 {
            continue; // EINTR
        }
        if fds.get(1).is_some_and(|f| f.revents & (libc::POLLPRI | libc::POLLERR) != 0) {
            sync_mounts(&mut fan, &ix, &excludes);
            changes.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        if fds[0].revents & libc::POLLIN == 0 {
            continue;
        }
        let events = match fan.read() {
            Ok(e) => e,
            Err(e) => {
                log(format_args!("fanotify read: {e}"));
                std::process::exit(1);
            }
        };
        let mut overflow = false;
        // coalesce: last state on disk wins, `fresh` sticks if any event said so
        let mut paths: HashMap<PathBuf, bool> = HashMap::new();
        for e in events {
            match e {
                fan::Event::Overflow => overflow = true,
                fan::Event::Lost => repair_pending = true,
                fan::Event::Path(p, fresh) => *paths.entry(p).or_default() |= fresh,
            }
        }
        changes.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let compact = {
            let mut w = ix.write().unwrap();
            for (p, fresh) in &paths {
                reconcile(&mut w, p, *fresh, &excludes);
            }
            w.gone > w.len() / 4
        };
        if overflow || compact {
            log(if overflow { "event queue overflow, rescanning" } else { "compacting" });
            // queries keep using the old index while this runs; events queue in the kernel and replay after
            let fresh = rescan(&excludes);
            *ix.write().unwrap() = fresh;
            // a full rescan also covers anything lost so far
            (last_rescan, repair_pending) = (Instant::now(), false);
        }
    }
}

fn serve(ix: Arc<RwLock<Index>>) {
    let sock = PathBuf::from(socket_path());
    if let Some(dir) = sock.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::remove_file(&sock);
    let listener = UnixListener::bind(&sock).unwrap_or_else(|e| {
        log(format_args!("bind {}: {e}", sock.display()));
        std::process::exit(1);
    });
    // anyone may connect; results are filtered by the caller's credentials
    let _ = std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o666));
    log(format_args!("listening on {}", sock.display()));
    for conn in listener.incoming().flatten() {
        let ix = ix.clone();
        std::thread::spawn(move || {
            if let Some(cred) = peer_cred(&conn) {
                let _ = handle(conn, &ix, &cred);
            }
        });
    }
}

/// uid from SO_PEERCRED, supplementary groups from /proc/<pid>/status.
fn peer_cred(s: &UnixStream) -> Option<Cred> {
    let mut uc: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = size_of::<libc::ucred>() as libc::socklen_t;
    let r = unsafe {
        libc::getsockopt(
            s.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut uc as *mut libc::ucred).cast(),
            &mut len,
        )
    };
    if r != 0 {
        return None;
    }
    let status = std::fs::read_to_string(format!("/proc/{}/status", uc.pid)).ok()?;
    let mut gids: Vec<u32> = status
        .lines()
        .find_map(|l| l.strip_prefix("Groups:"))
        .map(|g| g.split_whitespace().filter_map(|n| n.parse().ok()).collect())
        .unwrap_or_default();
    gids.push(uc.gid);
    Some(Cred { uid: uc.uid, gids })
}

fn handle(conn: UnixStream, ix: &RwLock<Index>, cred: &Cred) -> std::io::Result<()> {
    let mut out = conn.try_clone()?;
    // last result on this connection, for paging: (query, sort, desc, epoch) -> hits
    let mut cache: Option<((String, Sort, bool, u64), Vec<u32>)> = None;
    for line in BufReader::new(conn).lines() {
        let resp = match serde_json::from_str::<Request>(&line?) {
            Err(e) => Response {
                error: Some(format!("bad request: {e}")),
                ..Default::default()
            },
            Ok(req) => match Query::parse(&req.q) {
                Err(e) => Response {
                    error: Some(e),
                    ..Default::default()
                },
                Ok(_) if ix.read().unwrap().epoch == 0 => Response {
                    indexing: true,
                    error: Some("Still indexing — search will be ready in a moment".into()),
                    ..Default::default()
                },
                Ok(q) => {
                    let ix = ix.read().unwrap();
                    let key = (req.q.clone(), req.sort, req.desc, ix.epoch);
                    if !(req.more && cache.as_ref().is_some_and(|(k, _)| *k == key)) {
                        cache = Some((key, ix.search(&q, req.sort, req.desc, Some(cred))));
                    }
                    let hits = &cache.as_ref().unwrap().1;
                    let page = hits.iter().skip(req.offset).take(req.limit.unwrap_or(usize::MAX));
                    let hits_out = page
                        .filter(|&&i| ix.entries[i as usize].kind != Kind::Gone)
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
                    let bytes = hits
                        .iter()
                        .map(|&i| &ix.entries[i as usize])
                        .filter(|e| e.kind == Kind::File)
                        .map(|e| e.size)
                        .sum();
                    Response {
                        total: hits.len(),
                        bytes,
                        hits: hits_out,
                        error: None,
                        indexing: false,
                    }
                }
            },
        };
        serde_json::to_writer(&mut out, &resp)?;
        out.write_all(b"\n")?;
    }
    Ok(())
}
