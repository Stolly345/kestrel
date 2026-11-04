//! Daemon wire format: one JSON object per line each way over the Unix socket.

use crate::Sort;
use serde::{Deserialize, Serialize};

pub const DEFAULT_SOCKET: &str = "/run/kestrel/sock";

pub fn socket_path() -> String {
    std::env::var("KESTREL_SOCK").unwrap_or_else(|_| DEFAULT_SOCKET.into())
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
    /// Ask for the network shares' listing status instead of searching (`q` is ignored).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub shares: bool,
    /// Paths this user opened for this query before, with a bonus each: they rank higher under
    /// `Sort::Relevance` if they match. At most `MAX_PREFER` are used.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prefer: Vec<(String, i32)>,
}

pub const MAX_PREFER: usize = 50;

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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shares: Vec<ShareStatus>,
}

/// One network share from /etc/fstab (`x-kestrel`), as the index service last saw it.
#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq)]
pub struct ShareStatus {
    /// Mount point.
    pub path: String,
    pub state: ShareState,
    /// Entries listed so far (Listing) or in the listing searched now.
    pub entries: usize,
    /// When the listing searched now was made (Unix seconds); 0 = not since the service started.
    pub at: i64,
    /// Unreadable folders in the attempt that failed.
    pub errors: usize,
}

#[derive(Serialize, Deserialize, Default, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ShareState {
    /// Not listed since the service started (it lists every share right away).
    #[default]
    Waiting,
    Listing,
    Listed,
    /// Not reachable: its last listing is still searched.
    Offline,
    /// Reached, but too many folders couldn't be read: its last listing is still searched.
    Failed,
}

// ponytail: paths go over the wire lossy-UTF-8; non-UTF-8 filenames show U+FFFD. Send bytes if that bites.
#[derive(Serialize, Deserialize)]
pub struct Hit {
    pub path: String,
    pub size: u64,
    pub mtime: i64,
    pub dir: bool,
}
