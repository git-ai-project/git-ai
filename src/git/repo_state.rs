use std::fs;
use std::path::{Path, PathBuf};

pub fn is_valid_git_oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.chars().all(|c| c.is_ascii_hexdigit())
}

pub fn git_dir_for_worktree(worktree: &Path) -> Option<PathBuf> {
    let dot_git = worktree.join(".git");
    if dot_git.is_dir() {
        return Some(dot_git);
    }
    if !dot_git.is_file() {
        return None;
    }
    let contents = fs::read_to_string(&dot_git).ok()?;
    let pointer = contents.strip_prefix("gitdir:")?.trim();
    let candidate = PathBuf::from(pointer);
    if candidate.is_absolute() {
        return Some(candidate);
    }
    Some(worktree.join(candidate))
}

pub fn resolve_squash_source_head_from_git_dir(git_dir: &Path) -> Option<String> {
    let merge_head_path = git_dir.join("MERGE_HEAD");
    if let Ok(contents) = fs::read_to_string(merge_head_path)
        && let Some(candidate) = contents
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
        && is_valid_git_oid(candidate)
    {
        return Some(candidate.to_string());
    }

    let squash_msg_path = git_dir.join("SQUASH_MSG");
    if let Ok(contents) = fs::read_to_string(squash_msg_path) {
        for line in contents.lines() {
            if let Some(rest) = line.trim_start().strip_prefix("commit ")
                && let Some(candidate) = rest.split_whitespace().next()
                && is_valid_git_oid(candidate)
            {
                return Some(candidate.to_string());
            }
        }
    }

    None
}

pub fn resolve_squash_source_head_for_worktree(worktree: &Path) -> Option<String> {
    let git_dir = git_dir_for_worktree(worktree)?;
    resolve_squash_source_head_from_git_dir(&git_dir)
}
