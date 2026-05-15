use std::collections::HashMap;
use std::path::Path;

use crate::metrics::cache::StatsCache;

/// Aggregated statistics across multiple commits.
#[derive(Debug, Clone, PartialEq)]
pub struct AggregateStats {
    pub total_ai_lines: u64,
    pub total_human_lines: u64,
    pub total_untracked_lines: u64,
    pub ai_percent: f64,
    pub commits_cached: usize,
    pub commits_missing: usize,
}

/// Aggregate stats for a range of commits. Commits not in cache are counted
/// in `commits_missing` so the caller knows what still needs to be computed.
pub fn aggregate_range(git_dir: &Path, commits: &[String]) -> AggregateStats {
    let mut total_ai: u64 = 0;
    let mut total_human: u64 = 0;
    let mut total_untracked: u64 = 0;
    let mut cached = 0usize;
    let mut missing = 0usize;

    for sha in commits {
        match StatsCache::get(git_dir, sha) {
            Some(stats) => {
                total_ai += stats.ai_lines;
                total_human += stats.human_lines;
                total_untracked += stats.untracked_lines;
                cached += 1;
            }
            None => {
                missing += 1;
            }
        }
    }

    let total = total_ai + total_human + total_untracked;
    let ai_percent = if total > 0 {
        (total_ai as f64 / total as f64) * 100.0
    } else {
        0.0
    };

    AggregateStats {
        total_ai_lines: total_ai,
        total_human_lines: total_human,
        total_untracked_lines: total_untracked,
        ai_percent,
        commits_cached: cached,
        commits_missing: missing,
    }
}

/// Return the top N files by AI percentage across the given commits.
///
/// Files are deduplicated by path — totals are summed across all commits
/// where that file appears. Returns `(path, ai_percent)` pairs sorted descending.
pub fn top_files(git_dir: &Path, commits: &[String], n: usize) -> Vec<(String, f64)> {
    let mut file_totals: HashMap<String, (u64, u64, u64)> = HashMap::new();

    for sha in commits {
        if let Some(stats) = StatsCache::get(git_dir, sha) {
            for file in &stats.files {
                let entry = file_totals.entry(file.path.clone()).or_insert((0, 0, 0));
                entry.0 += file.ai_lines;
                entry.1 += file.human_lines;
                entry.2 += file.untracked_lines;
            }
        }
    }

    let mut results: Vec<(String, f64)> = file_totals
        .into_iter()
        .map(|(path, (ai, human, untracked))| {
            let total = ai + human + untracked;
            let pct = if total > 0 {
                (ai as f64 / total as f64) * 100.0
            } else {
                0.0
            };
            (path, pct)
        })
        .collect();

    results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    results.truncate(n);
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::cache::{CommitStats, FileStats};
    use std::fs;
    use std::path::PathBuf;

    fn make_temp_git_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "git-ai-agg-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn test_aggregate_range_all_cached() {
        let git_dir = make_temp_git_dir();

        let stats1 = CommitStats {
            commit_sha: "aaaa000000000000000000000000000000000001".to_string(),
            ai_lines: 10,
            human_lines: 5,
            untracked_lines: 2,
            files: vec![],
            cached_at: "2026-05-15T00:00:00Z".to_string(),
        };
        let stats2 = CommitStats {
            commit_sha: "bbbb000000000000000000000000000000000002".to_string(),
            ai_lines: 20,
            human_lines: 10,
            untracked_lines: 3,
            files: vec![],
            cached_at: "2026-05-15T00:00:00Z".to_string(),
        };

        StatsCache::put(&git_dir, &stats1).unwrap();
        StatsCache::put(&git_dir, &stats2).unwrap();

        let commits = vec![stats1.commit_sha.clone(), stats2.commit_sha.clone()];
        let agg = aggregate_range(&git_dir, &commits);

        assert_eq!(agg.total_ai_lines, 30);
        assert_eq!(agg.total_human_lines, 15);
        assert_eq!(agg.total_untracked_lines, 5);
        assert_eq!(agg.commits_cached, 2);
        assert_eq!(agg.commits_missing, 0);
        // ai_percent = 30 / 50 * 100 = 60.0
        assert!((agg.ai_percent - 60.0).abs() < 0.001);

        let _ = fs::remove_dir_all(&git_dir);
    }

    #[test]
    fn test_aggregate_range_with_missing() {
        let git_dir = make_temp_git_dir();

        let stats1 = CommitStats {
            commit_sha: "cccc000000000000000000000000000000000003".to_string(),
            ai_lines: 10,
            human_lines: 5,
            untracked_lines: 0,
            files: vec![],
            cached_at: "2026-05-15T00:00:00Z".to_string(),
        };
        StatsCache::put(&git_dir, &stats1).unwrap();

        let commits = vec![
            stats1.commit_sha.clone(),
            "dddd000000000000000000000000000000000004".to_string(), // not cached
        ];
        let agg = aggregate_range(&git_dir, &commits);

        assert_eq!(agg.commits_cached, 1);
        assert_eq!(agg.commits_missing, 1);
        assert_eq!(agg.total_ai_lines, 10);

        let _ = fs::remove_dir_all(&git_dir);
    }

    #[test]
    fn test_aggregate_range_empty() {
        let git_dir = make_temp_git_dir();
        let agg = aggregate_range(&git_dir, &[]);
        assert_eq!(agg.total_ai_lines, 0);
        assert_eq!(agg.ai_percent, 0.0);
        assert_eq!(agg.commits_cached, 0);
        assert_eq!(agg.commits_missing, 0);
        let _ = fs::remove_dir_all(&git_dir);
    }

    #[test]
    fn test_top_files() {
        let git_dir = make_temp_git_dir();

        let stats = CommitStats {
            commit_sha: "eeee000000000000000000000000000000000005".to_string(),
            ai_lines: 50,
            human_lines: 10,
            untracked_lines: 0,
            files: vec![
                FileStats {
                    path: "src/main.rs".to_string(),
                    ai_lines: 40,
                    human_lines: 10,
                    untracked_lines: 0,
                },
                FileStats {
                    path: "src/lib.rs".to_string(),
                    ai_lines: 10,
                    human_lines: 0,
                    untracked_lines: 0,
                },
                FileStats {
                    path: "README.md".to_string(),
                    ai_lines: 0,
                    human_lines: 20,
                    untracked_lines: 0,
                },
            ],
            cached_at: "2026-05-15T00:00:00Z".to_string(),
        };
        StatsCache::put(&git_dir, &stats).unwrap();

        let commits = vec![stats.commit_sha.clone()];
        let top = top_files(&git_dir, &commits, 2);

        assert_eq!(top.len(), 2);
        // src/lib.rs is 100% AI, src/main.rs is 80% AI
        assert_eq!(top[0].0, "src/lib.rs");
        assert!((top[0].1 - 100.0).abs() < 0.001);
        assert_eq!(top[1].0, "src/main.rs");
        assert!((top[1].1 - 80.0).abs() < 0.001);

        let _ = fs::remove_dir_all(&git_dir);
    }
}
