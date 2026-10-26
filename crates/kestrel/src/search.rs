//! A `gio::ListModel` over kestrel-indexd results, fetched lazily a page at a time so a
//! million-hit query scrolls like a ten-hit one.

use gtk::gio::subclass::prelude::*;
use gtk::prelude::*;
use gtk::{gio, glib};
use kestrel_index::Sort;
use kestrel_index::proto::{Hit, Request, Response, socket_path};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::time::Instant;

const PAGE: u32 = 500;

pub struct Conn {
    r: BufReader<UnixStream>,
    w: UnixStream,
}

impl Conn {
    fn open() -> std::io::Result<Conn> {
        let w = UnixStream::connect(socket_path())?;
        Ok(Conn {
            r: BufReader::new(w.try_clone()?),
            w,
        })
    }

    fn ask(&mut self, req: &Request) -> std::io::Result<Response> {
        serde_json::to_writer(&mut self.w, req)?;
        self.w.write_all(b"\n")?;
        let mut line = String::new();
        self.r.read_line(&mut line)?;
        serde_json::from_str(&line).map_err(std::io::Error::other)
    }
}

/// FileInfo shaped like what `gtk::DirectoryList` produces, so every view renders both modes.
pub fn hit_info(h: &Hit) -> gio::FileInfo {
    let file = gio::File::for_path(&h.path);
    let info = gio::FileInfo::new();
    let name = file.basename().unwrap_or_default();
    info.set_name(&name);
    info.set_display_name(&name.to_string_lossy());
    info.set_size(h.size as i64);
    // 0 = unknown (some archives and app bundles); leave it blank rather than show 1969
    if let Some(t) = (h.mtime != 0).then(|| glib::DateTime::from_unix_local(h.mtime).ok()).flatten() {
        info.set_modification_date_time(&t);
    }
    let (ty, ct) = if h.dir {
        (gio::FileType::Directory, glib::GString::from("inode/directory"))
    } else {
        (gio::FileType::Regular, gio::content_type_guess(Some(&name), None).0)
    };
    info.set_file_type(ty);
    info.set_content_type(&ct);
    info.set_icon(&gio::content_type_get_icon(&ct));
    info.set_attribute_object("standard::file", &file);
    if let Some(dir) = std::path::Path::new(&h.path).parent() {
        info.set_attribute_string("kestrel::dir", &dir.to_string_lossy());
    }
    info
}

/// One request on the shared connection, reconnecting once if the daemon restarted.
fn ask(conn: &Mutex<Option<Conn>>, req: &Request) -> Result<Response, SearchError> {
    let mut c = conn.lock().unwrap();
    for _ in 0..2 {
        if c.is_none() {
            *c = Conn::open().ok();
        }
        let Some(cc) = c.as_mut() else { break };
        match cc.ask(req) {
            Ok(r) => return Ok(r),
            Err(_) => *c = None,
        }
    }
    Err(SearchError::Down(format!("Can't reach the search service at {}.", socket_path())))
}

#[derive(Debug, Clone)]
pub enum SearchError {
    /// kestrel-indexd isn't reachable.
    Down(String),
    /// The query didn't parse.
    Query(String),
    /// The daemon is up but still on its first scan.
    Indexing,
}

#[derive(Debug, Clone, Copy)]
pub struct Outcome {
    pub total: u32,
    pub bytes: u64,
    pub ms: u128,
}

#[derive(Default)]
pub struct State {
    req: Request,
    total: u32,
    pages: HashMap<u32, Vec<gio::FileInfo>>,
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct SearchModel {
        pub state: RefCell<State>,
        /// Shared with worker threads; page fetches on the main thread wait for an in-flight query.
        pub conn: Arc<Mutex<Option<Conn>>>,
        /// Bumped per query; answers to older queries are dropped.
        pub generation: Cell<u64>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SearchModel {
        const NAME: &'static str = "KestrelSearchModel";
        type Type = super::SearchModel;
        type Interfaces = (gio::ListModel,);
    }

    impl ObjectImpl for SearchModel {}

    impl ListModelImpl for SearchModel {
        fn item_type(&self) -> glib::Type {
            gio::FileInfo::static_type()
        }
        fn n_items(&self) -> u32 {
            self.state.borrow().total
        }
        fn item(&self, pos: u32) -> Option<glib::Object> {
            let mut s = self.state.borrow_mut();
            if pos >= s.total {
                return None;
            }
            let page = pos / PAGE;
            if !s.pages.contains_key(&page) {
                // ponytail: crude cache bound; LRU if people scroll through 100k+ hits repeatedly
                if s.pages.len() > 200 {
                    s.pages.clear();
                }
                let req = Request {
                    offset: (page * PAGE) as usize,
                    limit: Some(PAGE as usize),
                    more: true,
                    ..s.req.clone()
                };
                let hits = ask(&self.conn, &req).map(|r| r.hits).unwrap_or_default();
                s.pages.insert(page, hits.iter().map(hit_info).collect());
            }
            // a page can come back short if entries vanished since the search; pad with the last item
            let p = &s.pages[&page];
            p.get((pos % PAGE) as usize).or(p.last()).map(|i| i.clone().upcast())
        }
    }
}

glib::wrapper! {
    pub struct SearchModel(ObjectSubclass<imp::SearchModel>) @implements gio::ListModel;
}

impl Default for SearchModel {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl SearchModel {
    /// Run a query on a worker thread; `done` gets the outcome unless a newer query superseded it.
    /// Results replace the model only on success, so a typo mid-typing keeps the last good results.
    pub fn run(&self, q: &str, sort: Sort, desc: bool, done: impl FnOnce(Result<Outcome, SearchError>) + 'static) {
        let generation = self.imp().generation.get() + 1;
        self.imp().generation.set(generation);
        let req = Request {
            q: q.into(),
            sort,
            desc,
            offset: 0,
            limit: Some(PAGE as usize),
            more: false,
        };
        let conn = self.imp().conn.clone();
        let me = self.downgrade();
        glib::spawn_future_local(async move {
            let r2 = req.clone();
            let r = gio::spawn_blocking(move || {
                let t = Instant::now();
                ask(&conn, &r2).map(|resp| (resp, t.elapsed().as_millis()))
            })
            .await
            .unwrap_or_else(|_| Err(SearchError::Down("search worker crashed".into())));
            let Some(me) = me.upgrade() else { return };
            if me.imp().generation.get() != generation {
                return;
            }
            let out = match r {
                Err(e) => {
                    me.clear();
                    Err(e)
                }
                Ok((Response { indexing: true, .. }, _)) => Err(SearchError::Indexing),
                Ok((Response { error: Some(e), .. }, _)) => Err(SearchError::Query(e)),
                Ok((resp, ms)) => {
                    let old = me.n_items();
                    {
                        let mut s = me.imp().state.borrow_mut();
                        s.req = req;
                        s.total = resp.total as u32;
                        s.pages.clear();
                        s.pages.insert(0, resp.hits.iter().map(hit_info).collect());
                    }
                    me.items_changed(0, old, me.n_items());
                    Ok(Outcome {
                        total: resp.total as u32,
                        bytes: resp.bytes,
                        ms,
                    })
                }
            };
            done(out);
        });
    }

    /// Forget results, and drop any answer still in flight.
    pub fn cancel(&self) {
        self.imp().generation.set(self.imp().generation.get() + 1);
        self.clear();
    }

    pub fn query(&self) -> String {
        self.imp().state.borrow().req.q.clone()
    }

    pub fn clear(&self) {
        let old = self.n_items();
        {
            let mut s = self.imp().state.borrow_mut();
            s.total = 0;
            s.pages.clear();
        }
        self.items_changed(0, old, 0);
    }
}

/// (entries, bytes) for a query, without fetching hits. Blocking: call from a worker thread.
pub fn stats(q: &str) -> Option<(usize, u64)> {
    let mut c = Conn::open().ok()?;
    let r = c
        .ask(&Request {
            q: q.into(),
            limit: Some(0),
            ..Default::default()
        })
        .ok()?;
    if r.error.is_some() {
        return None;
    }
    Some((r.total, r.bytes))
}

/// Everything below a local folder, quoted so spaces survive the query parser.
pub fn under_query(p: &std::path::Path) -> String {
    format!("\"under:{}\"", p.display())
}
