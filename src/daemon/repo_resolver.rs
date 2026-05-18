use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Resolves and caches repository working directory paths.
///
/// Trace2 events provide a `worktree` path which may be:
/// - A symlink (should resolve to canonical path)
/// - A git worktree (should resolve to its own working directory, not the main one)
/// - An already-canonical path
///
/// This resolver normalizes paths so the same physical repo always maps to
/// the same key, and caches results to avoid repeated filesystem/git calls.
pub struct RepoPathResolver {
    cache: HashMap<PathBuf, ResolvedRepo>,
    max_age: Duration,
}

#[derive(Clone)]
struct ResolvedRepo {
    working_dir: PathBuf,
    resolved_at: Instant,
}

impl Default for RepoPathResolver {
    fn default() -> Self {
        Self {
            cache: HashMap::new(),
            max_age: Duration::from_secs(300),
        }
    }
}

impl RepoPathResolver {
    pub fn new() -> Self {
        Self::default()
    }

    /// Resolve a worktree path from a trace2 event to a canonical working directory.
    ///
    /// Returns the canonical path to the repo's working directory. For worktrees,
    /// this is the worktree's own directory (not the main worktree), since each
    /// worktree has its own HEAD and working files.
    pub fn resolve(&mut self, raw_path: &Path) -> PathBuf {
        if let Some(cached) = self.cache.get(raw_path)
            && cached.resolved_at.elapsed() < self.max_age
        {
            return cached.working_dir.clone();
        }

        let resolved = self.do_resolve(raw_path);
        self.cache.insert(
            raw_path.to_path_buf(),
            ResolvedRepo {
                working_dir: resolved.clone(),
                resolved_at: Instant::now(),
            },
        );
        resolved
    }

    /// Evict stale cache entries.
    pub fn prune(&mut self) {
        self.cache
            .retain(|_, entry| entry.resolved_at.elapsed() < self.max_age);
    }

    fn do_resolve(&self, raw_path: &Path) -> PathBuf {
        // First try to canonicalize the filesystem path (resolves symlinks)
        let canonical = std::fs::canonicalize(raw_path).unwrap_or_else(|_| raw_path.to_path_buf());

        // Ask git for the actual toplevel working directory. This handles cases where
        // the trace2 event reports a subdirectory or a worktree path.
        let toplevel =
            crate::git_cmd::git_in_repo(&canonical, &["rev-parse", "--show-toplevel"]).ok();

        match toplevel {
            Some(tl) => {
                // Canonicalize the toplevel too (it may itself contain symlinks)
                let tl_path = PathBuf::from(&tl);
                std::fs::canonicalize(&tl_path).unwrap_or(tl_path)
            }
            None => canonical,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn resolve_real_repo() {
        let mut resolver = RepoPathResolver::new();
        // Use this repo's own path as a test
        let this_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let resolved = resolver.resolve(&this_dir);
        // Should return a canonical path
        assert!(resolved.is_absolute());
        assert!(resolved.join(".git").exists() || resolved.join(".git").is_file());
    }

    #[test]
    fn resolve_caches_result() {
        let mut resolver = RepoPathResolver::new();
        let this_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let first = resolver.resolve(&this_dir);
        let second = resolver.resolve(&this_dir);
        assert_eq!(first, second);
        assert_eq!(resolver.cache.len(), 1);
    }

    #[test]
    fn resolve_nonexistent_returns_input() {
        let mut resolver = RepoPathResolver::new();
        let fake = PathBuf::from("/nonexistent/repo/path");
        let resolved = resolver.resolve(&fake);
        // Should return the input path since canonicalize and git both fail
        assert_eq!(resolved, fake);
    }

    #[test]
    fn prune_removes_old_entries() {
        let mut resolver = RepoPathResolver::new();
        resolver.max_age = Duration::from_millis(0);
        let this_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        resolver.resolve(&this_dir);
        assert_eq!(resolver.cache.len(), 1);
        // Everything is immediately stale with max_age=0
        resolver.prune();
        assert_eq!(resolver.cache.len(), 0);
    }

    #[test]
    fn resolve_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let real_repo = dir.path().join("real");
        std::fs::create_dir(&real_repo).unwrap();

        // Init a git repo
        Command::new("git")
            .args(["init", real_repo.to_str().unwrap()])
            .env("GIT_TRACE2_EVENT", "0")
            .output()
            .unwrap();

        // Create a symlink to it
        let link = dir.path().join("link");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real_repo, &link).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&real_repo, &link).unwrap();

        let mut resolver = RepoPathResolver::new();
        let from_real = resolver.resolve(&real_repo);
        let from_link = resolver.resolve(&link);

        assert_eq!(
            from_real, from_link,
            "symlink and real path should resolve to same repo"
        );
    }

    #[test]
    fn resolve_valid_path_is_absolute() {
        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path().join("myrepo");
        std::fs::create_dir(&repo_path).unwrap();

        Command::new("git")
            .args(["init", repo_path.to_str().unwrap()])
            .env("GIT_TRACE2_EVENT", "0")
            .output()
            .unwrap();

        let mut resolver = RepoPathResolver::new();
        let resolved = resolver.resolve(&repo_path);

        assert!(resolved.is_absolute(), "resolved path must be absolute");
        // The resolved path should point to the same repo (canonicalized)
        let canonical = std::fs::canonicalize(&repo_path).unwrap();
        assert_eq!(resolved, canonical);
    }

    #[test]
    #[cfg(unix)]
    fn resolve_symlinked_path_returns_canonical() {
        let dir = tempfile::tempdir().unwrap();
        let real_repo = dir.path().join("actual");
        std::fs::create_dir(&real_repo).unwrap();

        Command::new("git")
            .args(["init", real_repo.to_str().unwrap()])
            .env("GIT_TRACE2_EVENT", "0")
            .output()
            .unwrap();

        // Create a chain of symlinks: link2 -> link1 -> real_repo
        let link1 = dir.path().join("link1");
        let link2 = dir.path().join("link2");
        std::os::unix::fs::symlink(&real_repo, &link1).unwrap();
        std::os::unix::fs::symlink(&link1, &link2).unwrap();

        let mut resolver = RepoPathResolver::new();
        let resolved = resolver.resolve(&link2);

        let canonical = std::fs::canonicalize(&real_repo).unwrap();
        assert_eq!(
            resolved, canonical,
            "symlink chain should resolve to canonical path of the real repo"
        );
    }

    #[test]
    fn cache_returns_same_result_on_second_call() {
        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path().join("cached_repo");
        std::fs::create_dir(&repo_path).unwrap();

        Command::new("git")
            .args(["init", repo_path.to_str().unwrap()])
            .env("GIT_TRACE2_EVENT", "0")
            .output()
            .unwrap();

        let mut resolver = RepoPathResolver::new();

        let first = resolver.resolve(&repo_path);
        assert_eq!(
            resolver.cache.len(),
            1,
            "first resolve should populate cache"
        );

        let second = resolver.resolve(&repo_path);
        assert_eq!(first, second, "second resolve should return cached result");
        // Cache should still have exactly one entry (no duplicates)
        assert_eq!(resolver.cache.len(), 1);
    }

    #[test]
    fn prune_clears_stale_entries_but_keeps_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let repo_a = dir.path().join("repo_a");
        let repo_b = dir.path().join("repo_b");
        std::fs::create_dir(&repo_a).unwrap();
        std::fs::create_dir(&repo_b).unwrap();

        Command::new("git")
            .args(["init", repo_a.to_str().unwrap()])
            .env("GIT_TRACE2_EVENT", "0")
            .output()
            .unwrap();
        Command::new("git")
            .args(["init", repo_b.to_str().unwrap()])
            .env("GIT_TRACE2_EVENT", "0")
            .output()
            .unwrap();

        let mut resolver = RepoPathResolver::new();
        // Use a very short max_age so entries become stale instantly
        resolver.max_age = Duration::from_millis(0);

        resolver.resolve(&repo_a);
        assert_eq!(resolver.cache.len(), 1);

        // After prune, the stale entry should be gone
        resolver.prune();
        assert_eq!(resolver.cache.len(), 0, "stale entries should be pruned");

        // Now use a large max_age and verify prune keeps fresh entries
        resolver.max_age = Duration::from_secs(3600);
        resolver.resolve(&repo_b);
        assert_eq!(resolver.cache.len(), 1);

        resolver.prune();
        assert_eq!(
            resolver.cache.len(),
            1,
            "fresh entries should survive prune"
        );
    }
}
