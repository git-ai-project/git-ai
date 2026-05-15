use std::path::Path;
use std::process::Command;

use crate::metrics::cache::{CommitStats, FileStats, StatsCache};

/// Result of an incremental cache update.
#[derive(Debug, Clone, PartialEq)]
pub struct UpdateResult {
    pub new_entries: usize,
    pub total_cached: usize,
}

/// Run an incremental cache update. Finds all commits with authorship notes
/// and caches stats for any that are not yet cached.
///
/// This is designed to be called after each commit (by the daemon) or
/// on-demand (by `git-ai stats`).
pub fn update_cache(git_dir: &Path, repo_path: &Path) -> Result<UpdateResult, String> {
    let commits_with_notes = list_commits_with_notes(repo_path)?;
    let mut new_entries = 0usize;
    let mut total_cached = 0usize;

    for sha in &commits_with_notes {
        if StatsCache::has(git_dir, sha) {
            total_cached += 1;
            continue;
        }

        // Parse the note and compute stats for this commit
        if let Some(stats) = parse_note_for_commit(repo_path, sha) {
            StatsCache::put(git_dir, &stats)?;
            new_entries += 1;
            total_cached += 1;
        }
    }

    Ok(UpdateResult {
        new_entries,
        total_cached,
    })
}

/// List all commit SHAs that have a note in the `refs/notes/ai` namespace.
fn list_commits_with_notes(repo_path: &Path) -> Result<Vec<String>, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .args(["notes", "--ref=ai", "list"])
        .output()
        .map_err(|e| format!("failed to run git notes list: {}", e))?;

    if !output.status.success() {
        // No notes ref yet — not an error, just empty
        return Ok(Vec::new());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let commits: Vec<String> = stdout
        .lines()
        .filter_map(|line| {
            // `git notes list` output format: "<note_blob_sha> <commit_sha>"
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 {
                Some(parts[1].to_string())
            } else {
                None
            }
        })
        .collect();

    Ok(commits)
}

/// Read the authorship note for a commit and extract stats from it.
fn parse_note_for_commit(repo_path: &Path, sha: &str) -> Option<CommitStats> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_path)
        .args(["notes", "--ref=ai", "show", sha])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let note_content = String::from_utf8_lossy(&output.stdout);
    parse_authorship_note(&note_content, sha)
}

/// Parse an authorship note (JSON) and produce `CommitStats`.
///
/// The authorship log schema (version `authorship/3.0.0`) contains attestation
/// entries with line ranges for each file. We count AI vs human vs untracked lines
/// from the attestation data.
fn parse_authorship_note(content: &str, sha: &str) -> Option<CommitStats> {
    let value: serde_json::Value = serde_json::from_str(content).ok()?;

    let attestations = value.get("attestations").and_then(|a| a.as_object())?;

    let mut files: Vec<FileStats> = Vec::new();
    let mut total_ai: u64 = 0;
    let mut total_human: u64 = 0;
    let mut total_untracked: u64 = 0;

    for (path, file_data) in attestations {
        let entries = file_data.get("entries").and_then(|e| e.as_array())?;
        let mut file_ai: u64 = 0;
        let mut file_human: u64 = 0;
        let mut file_untracked: u64 = 0;

        for entry in entries {
            let kind = entry.get("kind").and_then(|k| k.as_str()).unwrap_or("");
            let ranges = entry.get("ranges").and_then(|r| r.as_array());

            let line_count = if let Some(ranges) = ranges {
                count_lines_in_ranges(ranges)
            } else {
                0
            };

            match kind {
                "ai" => file_ai += line_count,
                "human" | "known_human" => file_human += line_count,
                _ => file_untracked += line_count,
            }
        }

        total_ai += file_ai;
        total_human += file_human;
        total_untracked += file_untracked;

        files.push(FileStats {
            path: path.clone(),
            ai_lines: file_ai,
            human_lines: file_human,
            untracked_lines: file_untracked,
        });
    }

    let now = chrono_like_now();

    Some(CommitStats {
        commit_sha: sha.to_string(),
        ai_lines: total_ai,
        human_lines: total_human,
        untracked_lines: total_untracked,
        files,
        cached_at: now,
    })
}

/// Count total lines spanned by an array of range objects `{"start": N, "end": M}`.
/// Each range is inclusive on both ends: lines start..=end.
fn count_lines_in_ranges(ranges: &[serde_json::Value]) -> u64 {
    let mut count: u64 = 0;
    for range in ranges {
        let start = range.get("start").and_then(|v| v.as_u64()).unwrap_or(0);
        let end = range.get("end").and_then(|v| v.as_u64()).unwrap_or(0);
        if end >= start {
            count += end - start + 1;
        }
    }
    count
}

/// Simple ISO-8601-ish timestamp without pulling in chrono.
fn chrono_like_now() -> String {
    use std::time::SystemTime;
    let duration = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = duration.as_secs();
    // Format as Unix timestamp string — simple, monotonic, always valid
    format!("{}Z", secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_count_lines_in_ranges() {
        let ranges: Vec<serde_json::Value> = vec![
            serde_json::json!({"start": 1, "end": 5}),
            serde_json::json!({"start": 10, "end": 12}),
        ];
        // 1..=5 is 5 lines, 10..=12 is 3 lines
        assert_eq!(count_lines_in_ranges(&ranges), 8);
    }

    #[test]
    fn test_count_lines_single_line_range() {
        let ranges: Vec<serde_json::Value> = vec![serde_json::json!({"start": 3, "end": 3})];
        assert_eq!(count_lines_in_ranges(&ranges), 1);
    }

    #[test]
    fn test_parse_authorship_note() {
        let note = serde_json::json!({
            "version": "authorship/3.0.0",
            "attestations": {
                "src/main.rs": {
                    "entries": [
                        {
                            "kind": "ai",
                            "ranges": [{"start": 1, "end": 10}]
                        },
                        {
                            "kind": "known_human",
                            "ranges": [{"start": 11, "end": 15}]
                        }
                    ]
                },
                "src/lib.rs": {
                    "entries": [
                        {
                            "kind": "untracked",
                            "ranges": [{"start": 1, "end": 3}]
                        }
                    ]
                }
            }
        });

        let sha = "abc123def456";
        let stats = parse_authorship_note(&note.to_string(), sha).unwrap();

        assert_eq!(stats.commit_sha, sha);
        assert_eq!(stats.ai_lines, 10);
        assert_eq!(stats.human_lines, 5);
        assert_eq!(stats.untracked_lines, 3);
        assert_eq!(stats.files.len(), 2);
    }

    #[test]
    fn test_parse_authorship_note_invalid_json() {
        assert!(parse_authorship_note("not json at all", "abc123").is_none());
    }

    #[test]
    fn test_parse_authorship_note_missing_attestations() {
        let note = serde_json::json!({"version": "authorship/3.0.0"});
        assert!(parse_authorship_note(&note.to_string(), "abc123").is_none());
    }

    #[test]
    fn test_incremental_update_with_no_repo() {
        // Using a path that won't have any git repo - should return empty
        let git_dir = std::env::temp_dir().join(format!("git-ai-incr-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&git_dir);
        let fake_repo = std::env::temp_dir().join("nonexistent-repo-for-test");

        let result = update_cache(&git_dir, &fake_repo);
        // Either an error (git not finding repo) or Ok with 0 entries
        if let Ok(r) = result {
            assert_eq!(r.new_entries, 0);
            assert_eq!(r.total_cached, 0);
        }
        // Err is acceptable - no repo there

        let _ = std::fs::remove_dir_all(&git_dir);
    }
}
