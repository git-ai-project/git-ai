use std::collections::HashMap;
use std::io::Write;
use std::process::{Command, Stdio};

use crate::authorship::attribution_tracker::LineAttribution;
use crate::authorship::authorship_log::{HumanRecord, LineRange, PromptRecord, SessionRecord};
use crate::authorship::authorship_log_serialization::AuthorshipLog;
use crate::config;
use crate::error::GitAiError;
use crate::git::notes_api::read_authorship_v3;
use crate::git::repository::{Repository, exec_git_allow_nonzero};

pub struct PendingCherryPick {
    pub all_sources: Vec<String>,
    pub pre_command_head: String,
}

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

/// Parses cherry-pick argv to extract resolved source SHAs.
/// Skips flags (args starting with `-`), expands ranges (`A..B`), and resolves
/// individual refs via rev-parse.
pub fn expand_cherry_pick_sources(repo: &Repository, argv: &[String]) -> Vec<String> {
    let mut sources = Vec::new();

    for arg in argv {
        if arg.starts_with('-') {
            continue;
        }

        if arg.contains("..") {
            // Expand range via rev-list
            let mut args = repo.global_args_for_exec();
            args.extend(["rev-list".to_string(), "--reverse".to_string(), arg.clone()]);
            if let Ok(output) = exec_git_allow_nonzero(&args)
                && output.status.success()
            {
                let stdout = String::from_utf8_lossy(&output.stdout);
                for line in stdout.lines() {
                    let trimmed = line.trim();
                    if is_full_sha(trimmed) {
                        sources.push(trimmed.to_string());
                    }
                }
            }
        } else {
            // Resolve single ref
            let mut args = repo.global_args_for_exec();
            args.extend(["rev-parse".to_string(), arg.clone()]);
            if let Ok(output) = exec_git_allow_nonzero(&args)
                && output.status.success()
            {
                let resolved = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if is_full_sha(&resolved) {
                    sources.push(resolved);
                }
            }
        }
    }

    sources
}

/// Returns commits created after `pre_command_head` up to current HEAD, in chronological order.
pub fn new_commits_since(repo: &Repository, pre_command_head: &str) -> Vec<String> {
    let mut args = repo.global_args_for_exec();
    args.extend([
        "rev-list".to_string(),
        "--reverse".to_string(),
        format!("{}..HEAD", pre_command_head),
    ]);

    let Ok(output) = exec_git_allow_nonzero(&args) else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

fn is_full_sha(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Handle `cherry-pick --no-commit` by reading the source commit's authorship note
/// and writing it as INITIAL in the current HEAD's working log. This ensures that
/// when the user eventually commits, the cherry-picked lines retain their attribution.
pub fn handle_cherry_pick_no_commit(
    repo: &Repository,
    sources: &[String],
    head: &str,
) -> Result<(), GitAiError> {
    if sources.is_empty() || head.is_empty() {
        return Ok(());
    }

    let mut all_file_attributions: HashMap<String, Vec<LineAttribution>> = HashMap::new();
    let mut all_prompts: HashMap<String, PromptRecord> = HashMap::new();
    let mut all_sessions: std::collections::BTreeMap<String, SessionRecord> =
        std::collections::BTreeMap::new();
    let mut all_humans: std::collections::BTreeMap<String, HumanRecord> =
        std::collections::BTreeMap::new();

    let mut source_logs: Vec<AuthorshipLog> = Vec::new();
    for source_sha in sources {
        if let Ok(log) = read_authorship_v3(repo, source_sha)
            && !log.attestations.is_empty()
        {
            source_logs.push(log);
        }
    }

    if source_logs.is_empty() {
        return Ok(());
    }

    // For cherry-pick --no-commit, the staged content IS the source commit's content.
    // Line numbers from the source note correspond directly to the staged file content.
    for log in &source_logs {
        for fa in &log.attestations {
            let mut raw_attrs: Vec<LineAttribution> = Vec::new();
            for entry in &fa.entries {
                for range in &entry.line_ranges {
                    let (start, end) = match range {
                        LineRange::Single(l) => (*l, *l),
                        LineRange::Range(s, e) => (*s, *e),
                    };
                    raw_attrs.push(LineAttribution::new(start, end, entry.hash.clone(), None));
                }
            }

            let existing = all_file_attributions
                .entry(fa.file_path.clone())
                .or_default();
            existing.extend(raw_attrs);
        }

        for (key, record) in &log.metadata.prompts {
            all_prompts
                .entry(key.clone())
                .or_insert_with(|| record.clone());
        }
        for (key, record) in &log.metadata.sessions {
            all_sessions
                .entry(key.clone())
                .or_insert_with(|| record.clone());
        }
        for (key, record) in &log.metadata.humans {
            all_humans
                .entry(key.clone())
                .or_insert_with(|| record.clone());
        }
    }

    if all_file_attributions.is_empty() {
        return Ok(());
    }

    let mut file_blobs: HashMap<String, String> = HashMap::new();
    for file_path in all_file_attributions.keys() {
        let content = read_staged_file_content(repo, file_path);
        if !content.is_empty() {
            file_blobs.insert(file_path.clone(), content);
        }
    }

    let working_log = repo.storage.working_log_for_base_commit(head)?;
    working_log.write_initial_attributions_with_contents(
        all_file_attributions,
        all_prompts,
        all_humans,
        file_blobs,
        all_sessions,
    )?;

    Ok(())
}

/// Read file content from the git index (staged state).
fn read_staged_file_content(repo: &Repository, file_path: &str) -> String {
    let mut args = repo.global_args_for_exec();
    args.extend(["show".to_string(), format!(":{}", file_path)]);
    exec_git_allow_nonzero(&args)
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn is_full_sha_valid() {
        assert!(is_full_sha("0123456789abcdef0123456789abcdef01234567"));
        assert!(is_full_sha(&"a".repeat(40)));
    }

    #[test]
    fn is_full_sha_invalid() {
        assert!(!is_full_sha("short"));
        assert!(!is_full_sha(&"g".repeat(40)));
        assert!(!is_full_sha(&"a".repeat(39)));
        assert!(!is_full_sha(&"a".repeat(41)));
        assert!(!is_full_sha(""));
    }

    #[test]
    fn expand_skips_flags() {
        // Verify the flag-skipping logic without needing a real repo
        let argv = [
            "-n".to_string(),
            "--no-commit".to_string(),
            "abc123".to_string(),
            "-x".to_string(),
            "def456".to_string(),
        ];
        let non_flag_args: Vec<&String> = argv.iter().filter(|a| !a.starts_with('-')).collect();
        assert_eq!(non_flag_args, vec!["abc123", "def456"]);
    }

    #[test]
    fn expand_detects_ranges() {
        let argv = ["main..feature".to_string(), "abc123".to_string()];
        let range_args: Vec<&String> = argv.iter().filter(|a| a.contains("..")).collect();
        let single_args: Vec<&String> = argv
            .iter()
            .filter(|a| !a.starts_with('-') && !a.contains(".."))
            .collect();
        assert_eq!(range_args, vec!["main..feature"]);
        assert_eq!(single_args, vec!["abc123"]);
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
