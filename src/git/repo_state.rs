use crate::error::GitAiError;
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReflogEntry {
    old: String,
    new: String,
}

fn read_reflog_entries(common_dir: &Path, refname: &str) -> Option<Vec<ReflogEntry>> {
    let path = common_dir.join("logs").join(refname);
    let contents = fs::read_to_string(path).ok()?;
    let mut entries = Vec::new();
    for line in contents.lines() {
        let head = line.split('\t').next().unwrap_or_default();
        let mut parts = head.split_whitespace();
        let old = parts.next()?;
        let new = parts.next()?;
        if is_valid_git_oid(old) && is_valid_git_oid(new) {
            entries.push(ReflogEntry {
                old: old.to_string(),
                new: new.to_string(),
            });
        }
    }
    Some(entries)
}

fn read_reflog_new_oids(common_dir: &Path, refname: &str) -> Option<Vec<String>> {
    Some(
        read_reflog_entries(common_dir, refname)?
            .into_iter()
            .map(|entry| entry.new)
            .collect(),
    )
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

pub fn latest_reflog_old_oid_for_worktree(worktree: &Path, refname: &str) -> Option<String> {
    let common_dir = common_dir_for_worktree(worktree)?;
    read_reflog_entries(&common_dir, refname)?
        .into_iter()
        .rev()
        .map(|entry| entry.old)
        .find(|oid| is_valid_git_oid(oid) && !oid.chars().all(|c| c == '0'))
}

pub fn resolve_reflog_old_oid_for_ref_new_oid_in_worktree(
    worktree: &Path,
    refname: &str,
    new_oid: &str,
) -> Option<String> {
    if !is_valid_git_oid(new_oid) {
        return None;
    }

    let common_dir = common_dir_for_worktree(worktree)?;
    read_reflog_entries(&common_dir, refname)?
        .into_iter()
        .rev()
        .find(|entry| entry.new == new_oid && is_valid_git_oid(&entry.old))
        .map(|entry| entry.old)
}

pub fn resolve_worktree_head_reflog_old_oid_for_new_head(
    worktree: &Path,
    new_oid: &str,
) -> Result<Option<String>, GitAiError> {
    if !is_valid_git_oid(new_oid) {
        return Ok(None);
    }

    Ok(read_head_reflog_transitions_for_worktree(worktree)?
        .into_iter()
        .rev()
        .find(|transition| transition.new == new_oid && is_valid_git_oid(&transition.old))
        .map(|transition| transition.old))
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct HeadReflogTransition {
    old: String,
    new: String,
    message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RebaseReflogSegment {
    pub original_head: String,
    pub onto_head: String,
    pub new_head: String,
    pub action_prefix: String,
    pub start_target: String,
    pub finish_target: Option<String>,
}

fn read_head_reflog_transitions_for_worktree_internal(
    worktree: &Path,
    include_noop: bool,
) -> Result<Vec<HeadReflogTransition>, GitAiError> {
    let git_dir = git_dir_for_worktree(worktree).ok_or_else(|| {
        GitAiError::Generic(format!(
            "missing gitdir for worktree while reading HEAD reflog: {}",
            worktree.display()
        ))
    })?;
    let path = git_dir.join("logs").join("HEAD");
    let contents = fs::read_to_string(&path).map_err(|err| {
        GitAiError::Generic(format!(
            "failed to read HEAD reflog for worktree {} at {}: {}",
            worktree.display(),
            path.display(),
            err
        ))
    })?;

    let mut out = Vec::new();
    for line in contents.lines() {
        let (head, message) = line
            .split_once('\t')
            .map(|(head, message)| (head, message.trim()))
            .unwrap_or((line, ""));
        let mut parts = head.split_whitespace();
        let Some(old) = parts.next().map(str::trim) else {
            continue;
        };
        let Some(new) = parts.next().map(str::trim) else {
            continue;
        };
        if !is_valid_git_oid(old) || !is_valid_git_oid(new) || (!include_noop && old == new) {
            continue;
        }
        out.push(HeadReflogTransition {
            old: old.to_string(),
            new: new.to_string(),
            message: message.to_string(),
        });
    }

    Ok(out)
}

fn read_head_reflog_transitions_for_worktree(
    worktree: &Path,
) -> Result<Vec<HeadReflogTransition>, GitAiError> {
    read_head_reflog_transitions_for_worktree_internal(worktree, false)
}

fn try_resolve_linear_head_chain(
    transitions: &[HeadReflogTransition],
    end_index: usize,
    expected_count: usize,
    message_fragment: Option<&str>,
) -> Option<(String, Vec<String>)> {
    let mut out = Vec::with_capacity(expected_count);
    let mut cursor = end_index;

    loop {
        let current = transitions.get(cursor)?;
        if let Some(fragment) = message_fragment
            && !current.message.contains(fragment)
        {
            return None;
        }
        out.push(current.new.clone());
        if out.len() == expected_count {
            out.reverse();
            return Some((current.old.clone(), out));
        }

        let target = current.old.as_str();
        cursor = (0..cursor)
            .rev()
            .find(|idx| transitions[*idx].new == target)?;
    }
}

fn rebase_like_start(message: &str) -> Option<(String, String)> {
    let (prefix, target) = message.split_once(" (start): checkout ")?;
    let prefix = prefix.trim();
    if prefix != "rebase" && !prefix.starts_with("pull") {
        return None;
    }
    let target = target.trim();
    if target.is_empty() {
        return None;
    }
    Some((prefix.to_string(), target.to_string()))
}

fn rebase_like_finish_target(message: &str, action_prefix: &str) -> Option<String> {
    let prefix = format!("{} (finish): returning to ", action_prefix);
    message
        .strip_prefix(&prefix)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn rebase_start_targets_match(segment_target: &str, hint: &str) -> bool {
    segment_target == hint
        || segment_target
            .strip_prefix("refs/heads/")
            .is_some_and(|target| target == hint)
        || hint
            .strip_prefix("refs/heads/")
            .is_some_and(|target| target == segment_target)
}

fn read_complete_rebase_segments_for_worktree(
    worktree: &Path,
) -> Result<Vec<RebaseReflogSegment>, GitAiError> {
    let transitions = read_head_reflog_transitions_for_worktree_internal(worktree, true)?;
    let mut segments = Vec::new();
    let mut index = 0usize;

    while index < transitions.len() {
        let Some((action_prefix, start_target)) = rebase_like_start(&transitions[index].message)
        else {
            index += 1;
            continue;
        };

        let original_head = transitions[index].old.clone();
        let onto_head = transitions[index].new.clone();
        let mut new_head = onto_head.clone();
        let mut finish_target = None;
        let mut cursor = index + 1;
        let mut completed = false;

        while cursor < transitions.len() {
            let transition = &transitions[cursor];
            if rebase_like_start(&transition.message).is_some() {
                break;
            }
            if let Some(target) = rebase_like_finish_target(&transition.message, &action_prefix) {
                finish_target = Some(target);
                if transition.old != transition.new {
                    new_head = transition.new.clone();
                }
                completed = true;
                cursor += 1;
                break;
            }
            if transition.old != transition.new
                && transition
                    .message
                    .starts_with(&format!("{} (", action_prefix))
            {
                new_head = transition.new.clone();
            }
            cursor += 1;
        }

        if completed {
            segments.push(RebaseReflogSegment {
                original_head,
                onto_head,
                new_head,
                action_prefix,
                start_target,
                finish_target,
            });
        }

        index = cursor.max(index + 1);
    }

    Ok(segments)
}

pub fn resolve_rebase_segment_for_worktree(
    worktree: &Path,
    start_target_hint: Option<&str>,
    already_processed_new_heads: &std::collections::HashSet<String>,
) -> Result<Option<RebaseReflogSegment>, GitAiError> {
    let candidates = read_complete_rebase_segments_for_worktree(worktree)?
        .into_iter()
        .filter(|segment| !already_processed_new_heads.contains(&segment.new_head))
        .collect::<Vec<_>>();

    if let Some(start_target_hint) = start_target_hint
        && let Some(segment) = candidates
            .iter()
            .find(|segment| rebase_start_targets_match(&segment.start_target, start_target_hint))
    {
        return Ok(Some(segment.clone()));
    }

    Ok(candidates.into_iter().next())
}

pub fn resolve_linear_head_commit_chain_for_worktree(
    worktree: &Path,
    new_head: &str,
    expected_count: usize,
    message_fragment: Option<&str>,
) -> Result<(String, Vec<String>), GitAiError> {
    if expected_count == 0 {
        return Err(GitAiError::Generic(
            "cannot resolve HEAD reflog chain with zero expected commits".to_string(),
        ));
    }
    if !is_valid_git_oid(new_head) {
        return Err(GitAiError::Generic(format!(
            "invalid HEAD reflog chain bound new={}",
            new_head
        )));
    }

    let transitions = read_head_reflog_transitions_for_worktree(worktree)?;
    if transitions.is_empty() {
        return Err(GitAiError::Generic(format!(
            "HEAD reflog is empty or missing valid transitions for worktree {}",
            worktree.display()
        )));
    }

    let mut matches = Vec::new();
    for (index, transition) in transitions.iter().enumerate() {
        if transition.new != new_head {
            continue;
        }
        if let Some((original_head, chain)) =
            try_resolve_linear_head_chain(&transitions, index, expected_count, message_fragment)
        {
            matches.push((original_head, chain));
        }
    }

    match matches.len() {
        1 => Ok(matches.remove(0)),
        0 => Err(GitAiError::Generic(format!(
            "failed to reconstruct HEAD reflog chain for worktree {} new={} expected_count={}",
            worktree.display(),
            new_head,
            expected_count
        ))),
        count => Err(GitAiError::Generic(format!(
            "ambiguous HEAD reflog chain for worktree {} new={} expected_count={} candidates={}",
            worktree.display(),
            new_head,
            expected_count,
            count
        ))),
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
    fn latest_reflog_old_oid_reads_previous_top_entry() {
        let temp = tempfile::tempdir().unwrap();
        let worktree = temp.path();
        let git_dir = worktree.join(".git");
        write_file(&git_dir.join("HEAD"), "ref: refs/heads/main\n");
        write_file(
            &git_dir.join("logs/refs/stash"),
            concat!(
                "0000000000000000000000000000000000000000 aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa Test <t@example.com> 0 -0000\tstash: first\n",
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb Test <t@example.com> 0 -0000\tstash: second\n",
            ),
        );

        let resolved = latest_reflog_old_oid_for_worktree(worktree, "refs/stash").unwrap();
        assert_eq!(resolved, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    }

    #[test]
    fn resolve_reflog_old_oid_for_ref_new_oid_reads_matching_branch_entry() {
        let temp = tempfile::tempdir().unwrap();
        let worktree = temp.path();
        let git_dir = worktree.join(".git");
        let old = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let new = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        write_file(&git_dir.join("HEAD"), "ref: refs/heads/main\n");
        write_file(
            &git_dir.join("logs/refs/heads/feature"),
            &format!(
                concat!(
                    "0000000000000000000000000000000000000000 {old} Test <t@example.com> 0 -0000\tbranch: Created from main\n",
                    "{old} {new} Test <t@example.com> 0 -0000\trebase (finish): refs/heads/feature onto main\n",
                ),
                old = old,
                new = new
            ),
        );

        let resolved =
            resolve_reflog_old_oid_for_ref_new_oid_in_worktree(worktree, "refs/heads/feature", new)
                .unwrap();
        assert_eq!(resolved, old);
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
    fn resolve_linear_head_commit_chain_for_worktree_recovers_multi_step_chain() {
        let temp = tempfile::tempdir().unwrap();
        let worktree = temp.path();
        let git_dir = worktree.join(".git");
        let original = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let first = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let second = "cccccccccccccccccccccccccccccccccccccccc";
        let third = "dddddddddddddddddddddddddddddddddddddddd";
        write_file(&git_dir.join("HEAD"), "ref: refs/heads/main\n");
        write_file(
            &git_dir.join("logs/HEAD"),
            &format!(
                concat!(
                    "{original} {first} Test <t@example.com> 0 -0000\tcherry-pick: first\n",
                    "{first} {second} Test <t@example.com> 0 -0000\tcherry-pick: second\n",
                    "{second} {third} Test <t@example.com> 0 -0000\tcherry-pick: third\n",
                ),
                original = original,
                first = first,
                second = second,
                third = third
            ),
        );

        let (resolved_original, commits) =
            resolve_linear_head_commit_chain_for_worktree(worktree, third, 3, None).unwrap();
        assert_eq!(resolved_original, original);
        assert_eq!(
            commits,
            vec![first.to_string(), second.to_string(), third.to_string()]
        );
    }

    #[test]
    fn resolve_linear_head_commit_chain_for_worktree_errors_when_chain_is_incomplete() {
        let temp = tempfile::tempdir().unwrap();
        let worktree = temp.path();
        let git_dir = worktree.join(".git");
        let original = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let second = "cccccccccccccccccccccccccccccccccccccccc";
        let third = "dddddddddddddddddddddddddddddddddddddddd";
        write_file(&git_dir.join("HEAD"), "ref: refs/heads/main\n");
        write_file(
            &git_dir.join("logs/HEAD"),
            &format!(
                concat!(
                    "{original} {second} Test <t@example.com> 0 -0000\tnoise\n",
                    "{second} {third} Test <t@example.com> 0 -0000\tcherry-pick: third\n",
                ),
                original = original,
                second = second,
                third = third
            ),
        );

        let err =
            resolve_linear_head_commit_chain_for_worktree(worktree, third, 3, None).unwrap_err();
        assert!(
            err.to_string()
                .contains("failed to reconstruct HEAD reflog chain")
        );
    }

    #[test]
    fn resolve_linear_head_commit_chain_for_worktree_errors_when_chain_is_ambiguous() {
        let temp = tempfile::tempdir().unwrap();
        let worktree = temp.path();
        let git_dir = worktree.join(".git");
        let original = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let first = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let second = "cccccccccccccccccccccccccccccccccccccccc";
        write_file(&git_dir.join("HEAD"), "ref: refs/heads/main\n");
        write_file(
            &git_dir.join("logs/HEAD"),
            &format!(
                concat!(
                    "{original} {first} Test <t@example.com> 0 -0000\tfirst chain 1\n",
                    "{first} {second} Test <t@example.com> 0 -0000\tfirst chain 2\n",
                    "{original} {first} Test <t@example.com> 0 -0000\tsecond chain 1\n",
                    "{first} {second} Test <t@example.com> 0 -0000\tsecond chain 2\n",
                ),
                original = original,
                first = first,
                second = second
            ),
        );

        let err =
            resolve_linear_head_commit_chain_for_worktree(worktree, second, 2, None).unwrap_err();
        assert!(err.to_string().contains("ambiguous HEAD reflog chain"));
    }

    #[test]
    fn resolve_linear_head_commit_chain_for_worktree_filters_by_reflog_action() {
        let temp = tempfile::tempdir().unwrap();
        let worktree = temp.path();
        let git_dir = worktree.join(".git");
        let original = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let commit = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        write_file(&git_dir.join("HEAD"), "ref: refs/heads/main\n");
        write_file(
            &git_dir.join("logs/HEAD"),
            &format!(
                concat!(
                    "{original} {commit} Test <t@example.com> 0 -0000\tcommit: feature\n",
                    "{original} {commit} Test <t@example.com> 0 -0000\tcherry-pick: feature\n",
                ),
                original = original,
                commit = commit
            ),
        );

        let (resolved_original, commits) =
            resolve_linear_head_commit_chain_for_worktree(worktree, commit, 1, Some("cherry-pick"))
                .unwrap();
        assert_eq!(resolved_original, original);
        assert_eq!(commits, vec![commit.to_string()]);
    }
}
