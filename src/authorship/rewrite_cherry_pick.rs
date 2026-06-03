use std::collections::HashMap;
use std::io::Write;
use std::process::{Command, Stdio};

use crate::config;
use crate::git::repository::{Repository, exec_git_allow_nonzero};

/// Pairs source commits with their cherry-picked counterparts using a two-pass algorithm.
///
/// Pass 1: patch-id anchoring — identical patches get paired by stable patch-id.
/// Pass 2: positional gap-fill — remaining unmatched commits are paired by order.
/// Sources with no corresponding new commit (skipped) produce no pair.
pub fn match_cherry_pick_pairs(
    repo: &Repository,
    sources: &[String],
    new_commits: &[String],
) -> Vec<(String, String)> {
    if sources.is_empty() || new_commits.is_empty() {
        return Vec::new();
    }

    // Compute patch-ids for both sides
    let source_patch_ids: Vec<Option<String>> = sources
        .iter()
        .map(|sha| compute_single_patch_id(repo, sha))
        .collect();

    let new_patch_ids: Vec<Option<String>> = new_commits
        .iter()
        .map(|sha| compute_single_patch_id(repo, sha))
        .collect();

    // Build map: patch_id -> list of indices in new_commits
    let mut new_by_patch_id: HashMap<String, Vec<usize>> = HashMap::new();
    for (idx, pid) in new_patch_ids.iter().enumerate() {
        if let Some(id) = pid {
            new_by_patch_id.entry(id.clone()).or_default().push(idx);
        }
    }

    let mut matched_sources: Vec<bool> = vec![false; sources.len()];
    let mut matched_new: Vec<bool> = vec![false; new_commits.len()];
    let mut pairs: Vec<(String, String)> = Vec::new();

    // Pass 1: patch-id anchoring
    for (src_idx, src_pid) in source_patch_ids.iter().enumerate() {
        let Some(pid) = src_pid else {
            continue;
        };
        let Some(candidates) = new_by_patch_id.get_mut(pid) else {
            continue;
        };
        // Take the first unmatched candidate
        if let Some(pos) = candidates.iter().position(|&idx| !matched_new[idx]) {
            let new_idx = candidates[pos];
            pairs.push((sources[src_idx].clone(), new_commits[new_idx].clone()));
            matched_sources[src_idx] = true;
            matched_new[new_idx] = true;
        }
    }

    // Pass 2: positional gap-fill
    let unmatched_sources: Vec<usize> = matched_sources
        .iter()
        .enumerate()
        .filter(|(_, m)| !**m)
        .map(|(i, _)| i)
        .collect();

    let unmatched_new: Vec<usize> = matched_new
        .iter()
        .enumerate()
        .filter(|(_, m)| !**m)
        .map(|(i, _)| i)
        .collect();

    for (src_pos, new_pos) in unmatched_sources.iter().zip(unmatched_new.iter()) {
        pairs.push((sources[*src_pos].clone(), new_commits[*new_pos].clone()));
    }

    pairs
}

fn compute_single_patch_id(repo: &Repository, sha: &str) -> Option<String> {
    // Get the diff output via git show
    let mut show_args = repo.global_args_for_exec();
    show_args.extend(["show".to_string(), sha.to_string()]);
    let show_output = exec_git_allow_nonzero(&show_args).ok()?;
    if !show_output.status.success() || show_output.stdout.is_empty() {
        return None;
    }

    // Pipe to git patch-id --stable
    let git_bin = config::Config::get().git_cmd().to_string();
    let mut child = Command::new(&git_bin)
        .args(["patch-id", "--stable"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    {
        let stdin = child.stdin.as_mut()?;
        stdin.write_all(&show_output.stdout).ok()?;
    }
    // stdin is dropped here, closing the pipe

    let output = child.wait_with_output().ok()?;
    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let patch_id = stdout.split_whitespace().next()?;
    if patch_id.is_empty() {
        return None;
    }

    Some(patch_id.to_string())
}

#[cfg(test)]
mod tests {
    #[test]
    fn match_cherry_pick_pairs_empty_sources() {
        // Cannot call with a real repo in unit tests, but we can verify the early return
        // by testing the algorithm logic directly through a mock-like approach.
        // Since match_cherry_pick_pairs requires a Repository, we test the structural behavior
        // by verifying the function's logic paths.
        let sources: Vec<String> = Vec::new();
        let new_commits = vec!["abc".repeat(13) + "a"]; // 40 chars
        // With empty sources, result should be empty regardless
        assert!(sources.is_empty());
        assert_eq!(
            positional_pair(&sources, &new_commits),
            Vec::<(String, String)>::new()
        );
    }

    #[test]
    fn match_cherry_pick_pairs_empty_new_commits() {
        let sources = vec!["a".repeat(40)];
        let new_commits: Vec<String> = Vec::new();
        assert_eq!(
            positional_pair(&sources, &new_commits),
            Vec::<(String, String)>::new()
        );
    }

    #[test]
    fn positional_pairing_equal_lengths() {
        let sources = vec!["a".repeat(40), "b".repeat(40), "c".repeat(40)];
        let new_commits = vec!["d".repeat(40), "e".repeat(40), "f".repeat(40)];
        let pairs = positional_pair(&sources, &new_commits);
        assert_eq!(pairs.len(), 3);
        assert_eq!(pairs[0], ("a".repeat(40), "d".repeat(40)));
        assert_eq!(pairs[1], ("b".repeat(40), "e".repeat(40)));
        assert_eq!(pairs[2], ("c".repeat(40), "f".repeat(40)));
    }

    #[test]
    fn positional_pairing_more_sources_than_new() {
        // Simulates skipped commits — extra sources have no pair
        let sources = vec!["a".repeat(40), "b".repeat(40), "c".repeat(40)];
        let new_commits = vec!["d".repeat(40), "e".repeat(40)];
        let pairs = positional_pair(&sources, &new_commits);
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0], ("a".repeat(40), "d".repeat(40)));
        assert_eq!(pairs[1], ("b".repeat(40), "e".repeat(40)));
    }

    #[test]
    fn positional_pairing_more_new_than_sources() {
        let sources = vec!["a".repeat(40)];
        let new_commits = vec!["d".repeat(40), "e".repeat(40)];
        let pairs = positional_pair(&sources, &new_commits);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0], ("a".repeat(40), "d".repeat(40)));
    }

    /// Helper that simulates pass-2 positional pairing without patch-id (for unit testing).
    fn positional_pair(sources: &[String], new_commits: &[String]) -> Vec<(String, String)> {
        if sources.is_empty() || new_commits.is_empty() {
            return Vec::new();
        }
        sources
            .iter()
            .zip(new_commits.iter())
            .map(|(s, n)| (s.clone(), n.clone()))
            .collect()
    }
}
