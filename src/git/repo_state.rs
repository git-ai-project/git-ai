use std::fs;
use std::path::{Path, PathBuf};

pub fn is_valid_git_oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.chars().all(|c| c.is_ascii_hexdigit())
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

fn read_ref_oid_from_paths(refname: &str, git_dir: &Path, common_dir: &Path) -> Option<String> {
    for base in [common_dir, git_dir] {
        let path = base.join(refname);
        if let Ok(contents) = fs::read_to_string(&path) {
            let candidate = contents.trim();
            if is_valid_git_oid(candidate) {
                return Some(candidate.to_string());
            }
        }
    }

    let packed_refs_path = common_dir.join("packed-refs");
    let contents = fs::read_to_string(packed_refs_path).ok()?;
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('^') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let oid = parts.next()?;
        let name = parts.next()?;
        if name == refname && is_valid_git_oid(oid) {
            return Some(oid.to_string());
        }
    }
    None
}

fn read_reflog_new_oids(common_dir: &Path, refname: &str) -> Option<Vec<String>> {
    let path = common_dir.join("logs").join(refname);
    let contents = fs::read_to_string(path).ok()?;
    let mut oids = Vec::new();
    for line in contents.lines() {
        let head = line.split('\t').next().unwrap_or_default();
        let mut parts = head.split_whitespace();
        let _old = parts.next()?;
        let new = parts.next()?;
        if is_valid_git_oid(new) {
            oids.push(new.to_string());
        }
    }
    Some(oids)
}

pub fn read_ref_oid_for_worktree(worktree: &Path, refname: &str) -> Option<String> {
    let git_dir = git_dir_for_worktree(worktree)?;
    let common_dir = common_dir_for_git_dir(&git_dir)?;
    read_ref_oid_from_paths(refname, &git_dir, &common_dir)
}

pub fn read_ref_oid_for_common_dir(common_dir: &Path, refname: &str) -> Option<String> {
    read_ref_oid_from_paths(refname, common_dir, common_dir)
}

pub fn resolve_stash_target_oid_for_worktree(
    worktree: &Path,
    target_spec: Option<&str>,
) -> Option<String> {
    let target_spec = target_spec.unwrap_or("stash@{0}");
    if is_valid_git_oid(target_spec) {
        return Some(target_spec.to_string());
    }

    if matches!(target_spec, "stash@{0}" | "refs/stash" | "stash") {
        return read_ref_oid_for_worktree(worktree, "refs/stash");
    }

    if target_spec.starts_with("refs/") {
        return read_ref_oid_for_worktree(worktree, target_spec);
    }

    let index = target_spec
        .strip_prefix("stash@{")
        .and_then(|value| value.strip_suffix('}'))
        .and_then(|value| value.parse::<usize>().ok())?;
    let common_dir = common_dir_for_worktree(worktree)?;
    let oids = read_reflog_new_oids(&common_dir, "refs/stash")?;
    oids.into_iter().rev().nth(index)
}

pub fn read_head_state_for_worktree(worktree: &Path) -> Option<HeadState> {
    let git_dir = git_dir_for_worktree(worktree)?;
    let common_dir = common_dir_for_git_dir(&git_dir)?;
    let head_contents = fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head_contents = head_contents.trim();
    if let Some(reference) = head_contents.strip_prefix("ref:") {
        let reference = reference.trim();
        let branch = reference
            .strip_prefix("refs/heads/")
            .map(|value| value.to_string());
        let detached = branch.is_none();
        let head = read_ref_oid_from_paths(reference, &git_dir, &common_dir);
        return Some(HeadState {
            head,
            branch,
            detached,
        });
    }
    if is_valid_git_oid(head_contents) {
        return Some(HeadState {
            head: Some(head_contents.to_string()),
            branch: None,
            detached: true,
        });
    }
    None
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
    fn resolve_stash_target_oid_defaults_to_top_entry() {
        let temp = tempfile::tempdir().unwrap();
        let worktree = temp.path();
        let git_dir = worktree.join(".git");
        write_file(&git_dir.join("HEAD"), "ref: refs/heads/main\n");
        write_file(
            &git_dir.join("refs/stash"),
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\n",
        );
        write_file(
            &git_dir.join("logs/refs/stash"),
            concat!(
                "0000000000000000000000000000000000000000 aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa Test <t@example.com> 0 -0000\tstash: first\n",
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb Test <t@example.com> 0 -0000\tstash: second\n",
            ),
        );

        let resolved = resolve_stash_target_oid_for_worktree(worktree, None).unwrap();
        assert_eq!(resolved, "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
    }

    #[test]
    fn resolve_stash_target_oid_defaults_to_refs_stash_without_reflog() {
        let temp = tempfile::tempdir().unwrap();
        let worktree = temp.path();
        let git_dir = worktree.join(".git");
        write_file(&git_dir.join("HEAD"), "ref: refs/heads/main\n");
        write_file(
            &git_dir.join("refs/stash"),
            "cccccccccccccccccccccccccccccccccccccccc\n",
        );

        let resolved = resolve_stash_target_oid_for_worktree(worktree, None).unwrap();
        assert_eq!(resolved, "cccccccccccccccccccccccccccccccccccccccc");
    }

    #[test]
    fn resolve_stash_target_oid_reads_older_stack_entries() {
        let temp = tempfile::tempdir().unwrap();
        let worktree = temp.path();
        let git_dir = worktree.join(".git");
        write_file(&git_dir.join("HEAD"), "ref: refs/heads/main\n");
        write_file(
            &git_dir.join("refs/stash"),
            "cccccccccccccccccccccccccccccccccccccccc\n",
        );
        write_file(
            &git_dir.join("logs/refs/stash"),
            concat!(
                "0000000000000000000000000000000000000000 aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa Test <t@example.com> 0 -0000\tstash: first\n",
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb Test <t@example.com> 0 -0000\tstash: second\n",
                "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb cccccccccccccccccccccccccccccccccccccccc Test <t@example.com> 0 -0000\tstash: third\n",
            ),
        );

        let resolved = resolve_stash_target_oid_for_worktree(worktree, Some("stash@{1}")).unwrap();
        assert_eq!(resolved, "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
    }

    #[test]
    fn resolve_stash_target_oid_accepts_literal_oid() {
        let temp = tempfile::tempdir().unwrap();
        let resolved = resolve_stash_target_oid_for_worktree(
            temp.path(),
            Some("dddddddddddddddddddddddddddddddddddddddd"),
        )
        .unwrap();
        assert_eq!(resolved, "dddddddddddddddddddddddddddddddddddddddd");
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
}
