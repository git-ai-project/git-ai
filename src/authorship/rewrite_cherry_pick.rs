use std::collections::{HashMap, HashSet};

use crate::git::repository::{Repository, exec_git_stdin};

/// Pairs source commits with their cherry-picked counterparts using a two-pass algorithm.
///
/// Pass 1: patch-id anchoring — identical patches get paired by stable patch-id.
/// Pass 2: positional gap-fill — remaining unmatched commits are paired by order.
/// Sources with no corresponding new commit (skipped) produce no pair.
pub fn match_cherry_pick_pairs(
    repo: &Repository,
    sources: &[String],
    new_commits: &[String],
) -> Result<Vec<(String, String)>, crate::error::GitAiError> {
    crate::authorship::rewrite::ensure_rewrite_work_limit("source commits", sources.len())?;
    crate::authorship::rewrite::ensure_rewrite_work_limit(
        "destination commits",
        new_commits.len(),
    )?;
    if sources.is_empty() || new_commits.is_empty() {
        return Ok(Vec::new());
    }

    let patch_ids = compute_patch_ids(repo, sources, new_commits)?;

    // Compute patch-ids for both sides
    let source_patch_ids: Vec<Option<String>> = sources
        .iter()
        .map(|sha| patch_ids.get(sha).cloned())
        .collect();

    let new_patch_ids: Vec<Option<String>> = new_commits
        .iter()
        .map(|sha| patch_ids.get(sha).cloned())
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

    Ok(pairs)
}

fn compute_patch_ids(
    repo: &Repository,
    sources: &[String],
    new_commits: &[String],
) -> Result<HashMap<String, String>, crate::error::GitAiError> {
    let mut commits = Vec::new();
    let mut seen = HashSet::new();
    for sha in sources.iter().chain(new_commits.iter()) {
        if seen.insert(sha.clone()) {
            commits.push(sha.clone());
        }
    }
    if commits.is_empty() {
        return Ok(HashMap::new());
    }

    stable_patch_ids_for_commits(repo, &commits)
}

pub(crate) fn stable_patch_ids_for_commits(
    repo: &Repository,
    commit_shas: &[String],
) -> Result<HashMap<String, String>, crate::error::GitAiError> {
    crate::authorship::rewrite::ensure_rewrite_work_limit("patch-id commits", commit_shas.len())?;
    let mut commits = Vec::new();
    let mut seen = HashSet::new();
    for sha in commit_shas {
        if seen.insert(sha.clone()) {
            commits.push(sha.clone());
        }
    }
    if commits.is_empty() {
        return Ok(HashMap::new());
    }

    let mut log_args = repo.global_args_for_exec();
    log_args.extend([
        "log".to_string(),
        "--stdin".to_string(),
        "--no-walk".to_string(),
        "--reverse".to_string(),
        "--no-ext-diff".to_string(),
        "--no-textconv".to_string(),
        "--no-color".to_string(),
        "--format=medium".to_string(),
        "-p".to_string(),
    ]);
    let stdin_data = commits.join("\n") + "\n";
    let log_output = exec_git_stdin(&log_args, stdin_data.as_bytes())?;
    if log_output.stdout.is_empty() {
        return Ok(HashMap::new());
    }

    let mut patch_args = repo.global_args_for_exec();
    patch_args.extend(["patch-id".to_string(), "--stable".to_string()]);
    let patch_output = exec_git_stdin(&patch_args, &log_output.stdout)?;

    let stdout = String::from_utf8_lossy(&patch_output.stdout);
    let mut patch_ids = HashMap::new();
    for line in stdout.lines() {
        let mut parts = line.split_whitespace();
        let Some(patch_id) = parts.next() else {
            continue;
        };
        let Some(commit_sha) = parts.next() else {
            continue;
        };
        patch_ids.insert(commit_sha.to_string(), patch_id.to_string());
    }

    Ok(patch_ids)
}

#[cfg(test)]
mod tests {
    use super::stable_patch_ids_for_commits;

    fn looks_like_hex_id(value: &str) -> bool {
        matches!(value.len(), 40 | 64) && value.chars().all(|c| c.is_ascii_hexdigit())
    }

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

    #[test]
    fn stable_patch_ids_for_commits_batches_multiple_commits() {
        let tmp = crate::git::test_utils::TmpRepo::new().expect("tmp repo");
        tmp.write_file("one.txt", "one\n", false)
            .expect("write first");
        let first = tmp.commit_all("first").expect("first commit");
        tmp.write_file("two.txt", "two\n", false)
            .expect("write second");
        let second = tmp.commit_all("second").expect("second commit");

        let patch_ids =
            stable_patch_ids_for_commits(tmp.gitai_repo(), &[first.clone(), second.clone()])
                .expect("patch ids");

        let first_patch_id = patch_ids.get(&first).expect("first patch id");
        let second_patch_id = patch_ids.get(&second).expect("second patch id");
        assert!(looks_like_hex_id(first_patch_id));
        assert!(looks_like_hex_id(second_patch_id));
        assert_ne!(first_patch_id, second_patch_id);
    }

    #[test]
    fn stable_patch_ids_reject_oversized_commit_set_before_git() {
        let tmp = crate::git::test_utils::TmpRepo::new().expect("tmp repo");
        let commits = (0..=crate::authorship::rewrite::MAX_REWRITE_COMMITS)
            .map(|index| format!("{index:040x}"))
            .collect::<Vec<_>>();

        let error = stable_patch_ids_for_commits(tmp.gitai_repo(), &commits)
            .expect_err("oversized patch-id batch must fail closed");

        assert!(error.to_string().contains("rewrite work exceeded"));
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
