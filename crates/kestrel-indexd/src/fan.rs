//! fanotify filesystem-wide watch, reporting (directory handle, name) per event.
//! Needs CAP_SYS_ADMIN (filesystem marks) and CAP_DAC_READ_SEARCH (open_by_handle_at).

use std::ffi::{CString, OsStr};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

const FAN_CLOEXEC: u32 = 0x1;
const FAN_UNLIMITED_QUEUE: u32 = 0x10;
const FAN_REPORT_DFID_NAME: u32 = 0x400 | 0x800;
const FAN_MARK_ADD: u32 = 0x1;
const FAN_MARK_FILESYSTEM: u32 = 0x100;

const FAN_ATTRIB: u64 = 0x4;
const FAN_CLOSE_WRITE: u64 = 0x8;
const FAN_MOVED_FROM: u64 = 0x40;
const FAN_MOVED_TO: u64 = 0x80;
const FAN_CREATE: u64 = 0x100;
const FAN_DELETE: u64 = 0x200;
const FAN_Q_OVERFLOW: u64 = 0x4000;
const FAN_ONDIR: u64 = 0x4000_0000;

const WATCH: u64 = FAN_CREATE | FAN_DELETE | FAN_MOVED_FROM | FAN_MOVED_TO | FAN_ATTRIB | FAN_CLOSE_WRITE | FAN_ONDIR;
const FAN_EVENT_INFO_TYPE_DFID_NAME: u8 = 2;

/// Log each kind of event-resolution failure once, instead of silently dropping every event.
fn warn_once(what: &str, e: &io::Error) {
    use std::sync::Mutex;
    static SEEN: Mutex<Vec<(String, i32)>> = Mutex::new(Vec::new());
    let key = (what.to_string(), e.raw_os_error().unwrap_or(0));
    let mut seen = SEEN.lock().unwrap();
    if !seen.contains(&key) {
        eprintln!("kestrel-indexd: {what} failed: {e} (a rescan will pick up what was missed)");
        seen.push(key);
    }
}

pub enum Event {
    /// Kernel queue overflowed: the index can no longer be trusted, rescan.
    Overflow,
    /// `fresh` = created or moved in.
    Path(PathBuf, bool),
    /// An event we couldn't turn into a path (not the harmless "already deleted" kind): the index
    /// may now miss a change, so the daemon schedules a rescan.
    Lost,
}

fn test_lose() -> Option<&'static str> {
    static T: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    T.get_or_init(|| std::env::var("KESTREL_TEST_LOSE").ok().filter(|s| !s.is_empty())).as_deref()
}

/// What became of one event's handle.
enum Resolved {
    Path(PathBuf),
    /// Nothing to do: the folder is already gone (its parent's event covers it), or it's on a
    /// filesystem we no longer watch.
    Skip,
    Lost,
}

pub struct Fan {
    fd: OwnedFd,
    /// fsid -> mount point. Opened only while resolving a handle: holding an fd on a mount
    /// would make unmounting a USB drive fail with "target is busy".
    mounts: Vec<([i32; 2], PathBuf)>,
}

fn check(r: i32) -> io::Result<i32> {
    if r < 0 { Err(io::Error::last_os_error()) } else { Ok(r) }
}

fn cpath(p: &Path) -> CString {
    CString::new(p.as_os_str().as_bytes()).unwrap()
}

fn fsid(p: &Path) -> io::Result<[i32; 2]> {
    let mut st: libc::statfs = unsafe { std::mem::zeroed() };
    check(unsafe { libc::statfs(cpath(p).as_ptr(), &mut st) })?;
    // fsid_t's field is private in libc; it is two c_ints.
    Ok(unsafe { std::mem::transmute::<libc::fsid_t, [i32; 2]>(st.f_fsid) })
}

impl Fan {
    pub fn new(mounts: &[PathBuf]) -> io::Result<Fan> {
        let fd = check(unsafe {
            libc::fanotify_init(
                FAN_CLOEXEC | FAN_UNLIMITED_QUEUE | FAN_REPORT_DFID_NAME,
                (libc::O_RDONLY | libc::O_LARGEFILE) as u32,
            )
        })?;
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        let mut fan = Fan { fd, mounts: Vec::new() };
        for m in mounts {
            fan.add(m)?;
        }
        Ok(fan)
    }

    /// Watch another filesystem (a drive that was just mounted).
    pub fn add(&mut self, m: &Path) -> io::Result<()> {
        check(unsafe {
            libc::fanotify_mark(
                self.fd.as_raw_fd(),
                FAN_MARK_ADD | FAN_MARK_FILESYSTEM,
                WATCH,
                libc::AT_FDCWD,
                cpath(m).as_ptr(),
            )
        })?;
        // marking again is a no-op in the kernel; keep one entry here too (a rescan re-syncs mounts)
        self.mounts.retain(|(_, p)| p != m);
        self.mounts.push((fsid(m)?, m.to_path_buf()));
        Ok(())
    }

    /// The kernel drops the mark itself on unmount; just forget the path.
    pub fn forget(&mut self, m: &Path) {
        self.mounts.retain(|(_, p)| p != m);
    }

    pub fn raw_fd(&self) -> i32 {
        self.fd.as_raw_fd()
    }

    /// Block until events arrive; returns one buffer's worth.
    pub fn read(&self) -> io::Result<Vec<Event>> {
        let mut buf = vec![0u8; 256 * 1024];
        let n = loop {
            let n = unsafe { libc::read(self.fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
            if n >= 0 {
                break n as usize;
            }
            let e = io::Error::last_os_error();
            if e.kind() != io::ErrorKind::Interrupted {
                return Err(e);
            }
        };
        let mut events = Vec::new();
        let mut off = 0;
        // struct fanotify_event_metadata { u32 event_len; u8 vers, reserved; u16 metadata_len; u64 mask; i32 fd, pid; }
        while off + 24 <= n {
            let ev = &buf[off..];
            let event_len = u32::from_ne_bytes(ev[0..4].try_into().unwrap()) as usize;
            let meta_len = u16::from_ne_bytes(ev[6..8].try_into().unwrap()) as usize;
            let mask = u64::from_ne_bytes(ev[8..16].try_into().unwrap());
            if event_len < 24 || off + event_len > n {
                break;
            }
            if mask & FAN_Q_OVERFLOW != 0 {
                events.push(Event::Overflow);
            } else {
                match self.resolve(&ev[meta_len..event_len]) {
                    // test hook (smoke-live.sh): pretend events for matching paths were lost
                    Resolved::Path(p) if test_lose().is_some_and(|t| p.as_os_str().as_bytes().windows(t.len()).any(|w| w == t.as_bytes())) => {
                        events.push(Event::Lost)
                    }
                    Resolved::Path(p) => events.push(Event::Path(p, mask & (FAN_CREATE | FAN_MOVED_TO) != 0)),
                    Resolved::Lost => events.push(Event::Lost),
                    Resolved::Skip => {}
                }
            }
            off += event_len;
        }
        Ok(events)
    }

    /// Info records: { u8 type; u8 pad; u16 len; fsid[2]; struct file_handle { u32 bytes; i32 type; u8 h[] }; name\0 }
    fn resolve(&self, mut info: &[u8]) -> Resolved {
        while info.len() >= 4 {
            let (ty, len) = (info[0], u16::from_ne_bytes([info[2], info[3]]) as usize);
            if len < 4 || len > info.len() {
                return Resolved::Lost;
            }
            let rec = &info[..len];
            info = &info[len..];
            if ty != FAN_EVENT_INFO_TYPE_DFID_NAME || rec.len() < 20 {
                continue;
            }
            let fsid = [
                i32::from_ne_bytes(rec[4..8].try_into().unwrap()),
                i32::from_ne_bytes(rec[8..12].try_into().unwrap()),
            ];
            let hbytes = u32::from_ne_bytes(rec[12..16].try_into().unwrap()) as usize;
            let hend = 20 + hbytes;
            if hend > rec.len() {
                return Resolved::Lost;
            }
            let name = &rec[hend..];
            let name = &name[..name.iter().position(|&b| b == 0).unwrap_or(name.len())];
            let Some((_, mount)) = self.mounts.iter().find(|(f, _)| *f == fsid) else {
                return Resolved::Skip; // a drive that was just unmounted
            };
            // a real (non-O_PATH) fd: open_by_handle_at rejects O_PATH mount fds with EBADF.
            // Opened per event and closed right after, so a USB drive can still be unmounted.
            let mfd = match check(unsafe { libc::open(cpath(mount).as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC) }) {
                Ok(fd) => fd,
                Err(e) => {
                    warn_once("open mount", &e);
                    return Resolved::Lost;
                }
            };
            let mfd = unsafe { OwnedFd::from_raw_fd(mfd) };
            // copy the handle into 4-byte-aligned storage for the syscall
            let mut handle = vec![0u32; hend.div_ceil(4)];
            unsafe { std::ptr::copy_nonoverlapping(rec[12..hend].as_ptr(), handle.as_mut_ptr().cast::<u8>(), hend - 12) };
            let dfd = unsafe { libc::syscall(libc::SYS_open_by_handle_at, mfd.as_raw_fd(), handle.as_ptr(), libc::O_PATH | libc::O_CLOEXEC) };
            if dfd < 0 {
                // ESTALE: directory already gone; its own parent's event covers it. Anything else
                // (e.g. ENOMEM seen during a build churning temp folders) loses the event.
                let e = io::Error::last_os_error();
                if e.raw_os_error() == Some(libc::ESTALE) {
                    return Resolved::Skip;
                }
                warn_once("open_by_handle_at", &e);
                return Resolved::Lost;
            }
            let dfd = unsafe { OwnedFd::from_raw_fd(dfd as i32) };
            let Ok(dir) = std::fs::read_link(format!("/proc/self/fd/{}", dfd.as_raw_fd())) else {
                return Resolved::Lost;
            };
            if dir.as_os_str().as_bytes().ends_with(b" (deleted)") {
                return Resolved::Skip;
            }
            return Resolved::Path(if name.is_empty() || name == b"." {
                dir
            } else {
                dir.join(OsStr::from_bytes(name))
            });
        }
        Resolved::Skip
    }
}
