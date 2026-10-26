//! Git status for the folder being browsed: branch name and a per-entry status letter.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub struct GitInfo {
    pub branch: String,
    /// Absolute path -> status letter. Folders get `M` when anything below them changed.
    status: HashMap<PathBuf, char>,
}

impl GitInfo {
    pub fn get(&self, p: &Path) -> Option<char> {
        self.status.get(p).copied()
    }
}

fn letter(s: git2::Status) -> Option<char> {
    use git2::Status as S;
    Some(if s.is_conflicted() {
        'U'
    } else if s.intersects(S::INDEX_RENAMED | S::WT_RENAMED) {
        'R'
    } else if s.intersects(S::INDEX_DELETED | S::WT_DELETED) {
        'D'
    } else if s.contains(S::INDEX_NEW) {
        'A'
    } else if s.contains(S::WT_NEW) {
        '?'
    } else if s.intersects(S::INDEX_MODIFIED | S::WT_MODIFIED | S::INDEX_TYPECHANGE | S::WT_TYPECHANGE) {
        'M'
    } else {
        return None;
    })
}

/// Blocking; call from a worker thread. `None` outside a repository.
pub fn status(dir: &Path) -> Option<GitInfo> {
    let repo = git2::Repository::discover(dir).ok()?;
    let root = repo.workdir()?.to_path_buf();
    let branch = match repo.head() {
        Ok(h) => h.shorthand().unwrap_or("HEAD").to_string(),
        Err(_) => "(no commits)".into(),
    };
    let mut opts = git2::StatusOptions::new();
    opts.include_untracked(true).recurse_untracked_dirs(false).exclude_submodules(true);
    let mut status = HashMap::new();
    for e in repo.statuses(Some(&mut opts)).ok()?.iter() {
        let (Some(rel), Some(l)) = (e.path().ok(), letter(e.status())) else {
            continue;
        };
        let abs = root.join(rel.trim_end_matches('/'));
        let mut up = abs.parent();
        while let Some(p) = up.filter(|p| p.starts_with(&root) && *p != root) {
            status.entry(p.to_path_buf()).or_insert('M');
            up = p.parent();
        }
        status.insert(abs, l);
    }
    Some(GitInfo { branch, status })
}

#[cfg(test)]
mod tests {
    #[test]
    fn statuses() {
        let d = std::env::temp_dir().join(format!("kestrel-git-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("src")).unwrap();
        let repo = git2::Repository::init(&d).unwrap();
        std::fs::write(d.join("src/a.rs"), "1").unwrap();
        let mut idx = repo.index().unwrap();
        idx.add_path("src/a.rs".as_ref()).unwrap();
        idx.write().unwrap();
        let tree = repo.find_tree(idx.write_tree().unwrap()).unwrap();
        let sig = git2::Signature::now("t", "t@t").unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[]).unwrap();
        std::fs::write(d.join("src/a.rs"), "2").unwrap();
        std::fs::write(d.join("new.txt"), "").unwrap();

        let g = super::status(&d.join("src")).unwrap();
        assert!(!g.branch.is_empty());
        assert_eq!(g.get(&d.join("src/a.rs")), Some('M'));
        assert_eq!(g.get(&d.join("src")), Some('M'), "parent folder flagged");
        assert_eq!(g.get(&d.join("new.txt")), Some('?'));
        assert!(super::status(&std::env::temp_dir()).is_none() || std::env::temp_dir().join(".git").exists());
        std::fs::remove_dir_all(&d).unwrap();
    }
}
