//! What you opened from search results (~/.config/kestrel/picks.json). Results you picked for a
//! query rank higher the next time you type it (Best Match); the quick-search window also shows
//! your pinned items and the ones you opened recently before you type anything.

use gtk::glib;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Queries remembered; the least recently used go first.
const MAX_QUERIES: usize = 500;
/// Picks remembered per query.
const MAX_PER_QUERY: usize = 10;
const MAX_RECENT: usize = 50;

#[derive(Serialize, Deserialize, Default, Clone)]
#[serde(default)]
pub struct Picks {
    /// Normalised query → what was opened for it.
    pub by_query: HashMap<String, Vec<Pick>>,
    /// Opened from search, newest first.
    pub recent: Vec<String>,
    /// Pinned in the quick-search window, in the order shown.
    pub pinned: Vec<String>,
}

#[derive(Serialize, Deserialize, Default, Clone, PartialEq, Debug)]
pub struct Pick {
    pub path: String,
    pub count: u32,
    /// Unix seconds.
    pub last: i64,
}

fn file() -> PathBuf {
    glib::user_config_dir().join("kestrel/picks.json")
}

pub fn load() -> Picks {
    std::fs::read_to_string(file())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save(p: &Picks) {
    let f = file();
    if let Some(d) = f.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let tmp = f.with_extension("json.tmp");
    if std::fs::write(&tmp, serde_json::to_string(p).unwrap_or_default()).is_ok() {
        let _ = std::fs::rename(&tmp, &f);
    }
}

fn now() -> i64 {
    glib::real_time() / 1_000_000
}

/// Case and spacing don't make a different query.
pub fn normalise(q: &str) -> String {
    q.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

impl Picks {
    /// `path` was opened while searching for `query`.
    pub fn record(&mut self, query: &str, path: &str, at: i64) {
        let q = normalise(query);
        if !q.is_empty() {
            let v = self.by_query.entry(q).or_default();
            match v.iter_mut().find(|p| p.path == path) {
                Some(p) => {
                    p.count += 1;
                    p.last = at;
                }
                None => v.push(Pick {
                    path: path.into(),
                    count: 1,
                    last: at,
                }),
            }
            v.sort_by(|a, b| b.count.cmp(&a.count).then(b.last.cmp(&a.last)));
            v.truncate(MAX_PER_QUERY);
            if self.by_query.len() > MAX_QUERIES {
                let oldest = self
                    .by_query
                    .iter()
                    .min_by_key(|(_, v)| v.iter().map(|p| p.last).max().unwrap_or(0))
                    .map(|(k, _)| k.clone());
                if let Some(k) = oldest {
                    self.by_query.remove(&k);
                }
            }
        }
        self.recent.retain(|p| p != path);
        self.recent.insert(0, path.into());
        self.recent.truncate(MAX_RECENT);
    }

    /// Bonus points for paths picked before, for the search service (Best Match). The same query
    /// counts fully; one you were typing towards (`rep` → `report`) or a shorter one you typed
    /// before (`report` ← `rep`) counts a bit less. More picks and fresher picks count more.
    pub fn prefer(&self, query: &str, at: i64) -> Vec<(String, i32)> {
        let q = normalise(query);
        if q.is_empty() {
            return Vec::new();
        }
        let mut best: HashMap<&str, i32> = HashMap::new();
        for (k, v) in &self.by_query {
            let weight = if *k == q {
                10
            } else if k.starts_with(&q) || q.starts_with(k.as_str()) {
                6
            } else {
                continue;
            };
            for p in v {
                let days = ((at - p.last).max(0) / 86400) as i32;
                let fresh = (30 - days).clamp(0, 30); // fades over a month
                let bonus = weight * (15 + 6 * p.count.min(8) as i32 + fresh);
                let b = best.entry(p.path.as_str()).or_default();
                *b = (*b).max(bonus);
            }
        }
        let mut out: Vec<(String, i32)> = best.into_iter().map(|(p, b)| (p.to_string(), b)).collect();
        out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        out.truncate(kestrel_index::proto::MAX_PREFER);
        out
    }

    pub fn is_pinned(&self, path: &str) -> bool {
        self.pinned.iter().any(|p| p == path)
    }

    /// Never mind this path: gone from recent items and every query's picks (pins stay).
    pub fn forget(&mut self, path: &str) {
        self.recent.retain(|p| p != path);
        for v in self.by_query.values_mut() {
            v.retain(|p| p.path != path);
        }
        self.by_query.retain(|_, v| !v.is_empty());
    }
}

/// Load, change, save.
pub fn update(f: impl FnOnce(&mut Picks)) {
    let mut p = load();
    f(&mut p);
    save(&p);
}

pub fn record(query: &str, path: &str) {
    update(|p| p.record(query, path, now()));
}

pub fn prefer(query: &str) -> Vec<(String, i32)> {
    load().prefer(query, now())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_rank_and_fade() {
        let mut p = Picks::default();
        let day = 86400;
        p.record("Report", "/a/report.pdf", 0);
        p.record("report ", "/a/report.pdf", 0);
        p.record("report", "/b/old.pdf", 0);
        p.record("rep", "/c/rep.txt", 0);
        p.record("budget", "/d/budget.xlsx", 0);
        let got = p.prefer("REPORT", 0);
        let paths: Vec<_> = got.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(
            paths,
            ["/a/report.pdf", "/b/old.pdf", "/c/rep.txt"],
            "same query first, more picks first, related queries less"
        );
        assert!(got[0].1 > got[1].1 && got[1].1 > got[2].1);
        let later = p.prefer("report", 40 * day);
        assert!(later[0].1 < got[0].1, "old picks count less");
        assert!(p.prefer("", 0).is_empty());
        assert_eq!(p.recent[0], "/d/budget.xlsx");
        p.forget("/a/report.pdf");
        assert!(p.prefer("report", 0).iter().all(|(x, _)| x != "/a/report.pdf"));
        assert!(!p.recent.contains(&"/a/report.pdf".to_string()));
    }
}
