use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

pub fn is_valid_git_oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.chars().all(|c| c.is_ascii_hexdigit())
}

/// Upper bound on memoized worktree→family-key entries. A daemon session
/// touches a handful of repos, so this is never hit in practice; it only
/// guards against unbounded growth in a very long-lived daemon spanning
/// thousands of distinct worktrees. On overflow we clear and rebuild rather
/// than evict per-entry — the set is tiny, so a coarse reset is cheapest.
const FAMILY_KEY_CACHE_LIMIT: usize = 4096;

fn family_key_cache() -> &'static Mutex<HashMap<PathBuf, String>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, String>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Resolve a worktree path to its canonical family key (the canonicalized
/// common dir, as a string), memoizing the result.
///
/// `common_dir_for_worktree` walks parent dirs stat-ing for `.git` (and reads
/// the `.git` file for linked worktrees), and `canonicalize` issues a
/// stat/readlink per path component. This runs per mutating command on the
/// trace-ingestion path, where the same worktree recurs constantly, so the
/// uncached cost is pure repeated syscalls. The mapping is a stable function
/// of the on-disk repo layout for a live worktree, so caching it is safe.
///
/// Only successful resolutions are cached: a path that is not yet a repository
/// (before `clone`/`init` completes) returns `None` and is re-resolved on the
/// next call, so a repo that appears later is still picked up.
pub fn canonical_family_key_for_worktree(worktree: &Path) -> Option<String> {
    if let Ok(cache) = family_key_cache().lock()
        && let Some(key) = cache.get(worktree)
    {
        return Some(key.clone());
    }

    let common_dir = common_dir_for_worktree(worktree)?;
    let key = common_dir
        .canonicalize()
        .unwrap_or(common_dir)
        .to_string_lossy()
        .to_string();

    if let Ok(mut cache) = family_key_cache().lock() {
        if cache.len() >= FAMILY_KEY_CACHE_LIMIT {
            cache.clear();
        }
        cache.insert(worktree.to_path_buf(), key.clone());
    }
    Some(key)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadState {
    pub head: Option<String>,
    pub branch: Option<String>,
    pub detached: bool,
}

pub fn worktree_root_for_path(path: &Path) -> Option<PathBuf> {
    let mut current = Some(path);
    while let Some(candidate) = current {
        let dot_git = candidate.join(".git");
        if dot_git.is_dir() || dot_git.is_file() {
            return Some(candidate.to_path_buf());
        }
        current = candidate.parent();
    }
    None
}

pub fn git_dir_for_worktree(worktree: &Path) -> Option<PathBuf> {
    let worktree_root = worktree_root_for_path(worktree)?;
    let dot_git = worktree_root.join(".git");
    if dot_git.is_dir() {
        return Some(dot_git);
    }
    let contents = fs::read_to_string(&dot_git).ok()?;
    let pointer = contents.strip_prefix("gitdir:")?.trim();
    let candidate = PathBuf::from(pointer);
    if candidate.is_absolute() {
        return Some(candidate);
    }
    Some(worktree_root.join(candidate))
}

pub fn common_dir_for_git_dir(git_dir: &Path) -> Option<PathBuf> {
    let parent = git_dir.parent()?;
    if parent.file_name().and_then(|name| name.to_str()) == Some("worktrees") {
        return parent.parent().map(PathBuf::from);
    }
    Some(git_dir.to_path_buf())
}

pub fn common_dir_for_worktree(worktree: &Path) -> Option<PathBuf> {
    let git_dir = git_dir_for_worktree(worktree)?;
    common_dir_for_git_dir(&git_dir)
}

pub fn common_dir_for_repo_path(path: &Path) -> Option<PathBuf> {
    if let Some(common_dir) = common_dir_for_worktree(path) {
        return Some(common_dir);
    }

    if path.is_dir() && path.join("HEAD").is_file() {
        return common_dir_for_git_dir(path);
    }

    if path.file_name().and_then(|name| name.to_str()) == Some(".git") && path.is_file() {
        let contents = fs::read_to_string(path).ok()?;
        let pointer = contents.strip_prefix("gitdir:")?.trim();
        let candidate = PathBuf::from(pointer);
        let git_dir = if candidate.is_absolute() {
            candidate
        } else {
            path.parent()?.join(candidate)
        };
        return common_dir_for_git_dir(&git_dir);
    }

    None
}

pub fn read_head_state_for_worktree(worktree: &Path) -> Option<HeadState> {
    use crate::git::fast_reader::{FastRefReader, HeadKind};
    let git_dir = git_dir_for_worktree(worktree)?;
    let common_dir = common_dir_for_git_dir(&git_dir)?;
    let reader = FastRefReader::new(&git_dir, &common_dir);
    match reader.try_read_head()? {
        HeadKind::Symbolic(refname) => {
            let branch = refname.strip_prefix("refs/heads/").map(|s| s.to_string());
            let detached = branch.is_none();
            let head = reader.try_resolve_ref(&refname);
            Some(HeadState {
                head,
                branch,
                detached,
            })
        }
        HeadKind::Detached(oid) => Some(HeadState {
            head: Some(oid),
            branch: None,
            detached: true,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_file(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, contents).unwrap();
    }

    #[test]
    fn worktree_root_for_path_walks_parent_directories() {
        let temp = tempfile::tempdir().unwrap();
        let worktree = temp.path();
        let nested = worktree.join("src").join("lib");
        fs::create_dir_all(&nested).unwrap();
        write_file(&worktree.join(".git/HEAD"), "ref: refs/heads/main\n");

        let resolved = worktree_root_for_path(&nested).unwrap();
        assert_eq!(resolved, worktree);
    }

    #[test]
    fn read_head_state_for_nested_path_uses_worktree_root() {
        let temp = tempfile::tempdir().unwrap();
        let worktree = temp.path();
        let nested = worktree.join("src").join("lib");
        fs::create_dir_all(&nested).unwrap();
        write_file(&worktree.join(".git/HEAD"), "ref: refs/heads/main\n");
        write_file(
            &worktree.join(".git/refs/heads/main"),
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n",
        );

        let state = read_head_state_for_worktree(&nested).unwrap();
        assert_eq!(
            state.head.as_deref(),
            Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        );
        assert_eq!(state.branch.as_deref(), Some("main"));
        assert!(!state.detached);
    }

    #[test]
    fn canonical_family_key_matches_uncached_computation_and_is_stable() {
        let temp = tempfile::tempdir().unwrap();
        let worktree = temp.path();
        write_file(&worktree.join(".git/HEAD"), "ref: refs/heads/main\n");

        // The memoized helper must equal the original uncached derivation:
        // common_dir_for_worktree → canonicalize → string.
        let common_dir = common_dir_for_worktree(worktree).unwrap();
        let expected = common_dir
            .canonicalize()
            .unwrap_or(common_dir)
            .to_string_lossy()
            .to_string();

        let first = canonical_family_key_for_worktree(worktree).unwrap();
        assert_eq!(first, expected);
        // A second call (served from cache) must return the identical value.
        let second = canonical_family_key_for_worktree(worktree).unwrap();
        assert_eq!(second, expected);
    }

    #[test]
    fn canonical_family_key_does_not_cache_misses() {
        // A path that is not yet a repository must return None AND must not be
        // cached, so that once it becomes a repo (e.g. after clone/init) the
        // next call resolves correctly rather than returning a stale miss.
        let temp = tempfile::tempdir().unwrap();
        let worktree = temp.path().join("not-a-repo-yet");
        fs::create_dir_all(&worktree).unwrap();

        assert!(canonical_family_key_for_worktree(&worktree).is_none());

        // The repo now appears at this path.
        write_file(&worktree.join(".git/HEAD"), "ref: refs/heads/main\n");

        let resolved = canonical_family_key_for_worktree(&worktree);
        assert!(
            resolved.is_some(),
            "a worktree that becomes a repo after a miss must resolve on the next call"
        );
    }
}
