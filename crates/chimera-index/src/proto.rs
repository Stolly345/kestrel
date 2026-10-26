//! Daemon wire format: one JSON object per line each way over the Unix socket.

use crate::Sort;
use serde::{Deserialize, Serialize};

pub const DEFAULT_SOCKET: &str = "/run/chimera/sock";

pub fn socket_path() -> String {
    std::env::var("CHIMERA_SOCK").unwrap_or_else(|_| DEFAULT_SOCKET.into())
}

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct Request {
    pub q: String,
    #[serde(default)]
    pub sort: Sort,
    #[serde(default)]
    pub desc: bool,
    #[serde(default)]
    pub offset: usize,
    /// `None` = all hits.
    #[serde(default)]
    pub limit: Option<usize>,
    /// Page through the previous result of the same query on this connection instead of re-running it.
    #[serde(default)]
    pub more: bool,
}

#[derive(Serialize, Deserialize, Default)]
pub struct Response {
    pub total: usize,
    /// Sum of file sizes over all hits (folder size for `under:` queries).
    #[serde(default)]
    pub bytes: u64,
    pub hits: Vec<Hit>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The daemon is up but its first scan hasn't finished; retry shortly.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub indexing: bool,
}

// ponytail: paths go over the wire lossy-UTF-8; non-UTF-8 filenames show U+FFFD. Send bytes if that bites.
#[derive(Serialize, Deserialize)]
pub struct Hit {
    pub path: String,
    pub size: u64,
    pub mtime: i64,
    pub dir: bool,
}
