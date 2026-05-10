//! Pre-flight validation and post-rebase verification for attribution preservation.
//!
//! This module provides checks to detect conditions that may cause attribution loss
//! during Git rebases, and verification to detect when loss has occurred.

use crate::error::GitAiError;
use crate::git::repository::Repository;

/// Validation warnings that indicate potential attribution loss risk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RebaseValidationWarning {
    /// Background daemon sync is pending - working logs may be incomplete
    DaemonSyncPending,
    /// Uncommitted checkpoints exist - should commit before rebasing
    UncommittedCheckpoints { count: usize },
    /// No working logs found for current HEAD - attribution may not survive
    MissingWorkingLogs { head_sha: String },
    /// Working log directory is empty or corrupted
    CorruptedWorkingLogs,
}

impl RebaseValidationWarning {
    pub fn message(&self) -> String {
        match self {
            RebaseValidationWarning::DaemonSyncPending => {
                "Background sync pending. AI attribution may be incomplete.\n\
                 Run `git-ai sync --wait` before rebasing for best results."
                    .to_string()
            }
            RebaseValidationWarning::UncommittedCheckpoints { count } => {
                format!(
                    "{} uncommitted AI checkpoint{}. Commit changes before rebasing.",
                    count,
                    if *count == 1 { "" } else { "s" }
                )
            }
            RebaseValidationWarning::MissingWorkingLogs { head_sha } => {
                format!(
                    "No AI working logs found for current HEAD ({}).\n\
                     AI attribution may not survive this rebase.",
                    &head_sha[..8]
                )
            }
            RebaseValidationWarning::CorruptedWorkingLogs => {
                "Working log directory is corrupted or empty.\n\
                 Attribution recovery may fail."
                    .to_string()
            }
        }
    }
}

/// Result of post-rebase attribution verification.
#[derive(Debug)]
pub struct RebaseVerificationResult {
    /// Original commit SHAs that had authorship notes before rebase
    pub original_commits_with_notes: Vec<String>,
    /// Rebased commit SHAs
    pub rebased_commits: Vec<String>,
    /// Commits where attribution was lost (original had note, rebased doesn't)
    pub missing_attribution: Vec<(String, String)>, // (original_sha, rebased_sha)
}

impl RebaseVerificationResult {
    pub fn has_attribution_loss(&self) -> bool {
        !self.missing_attribution.is_empty()
    }

    pub fn attribution_survival_rate(&self) -> f64 {
        if self.original_commits_with_notes.is_empty() {
            return 1.0;
        }
        let preserved = self.original_commits_with_notes.len() - self.missing_attribution.len();
        preserved as f64 / self.original_commits_with_notes.len() as f64
    }
}

/// Validate preconditions before a rebase to detect attribution loss risks.
///
/// Returns a list of warnings. An empty list means all preconditions are good.
/// Warnings are informational - the rebase can still proceed, but attribution
/// may be at risk.
pub fn validate_rebase_preconditions(
    repository: &Repository,
) -> Result<Vec<RebaseValidationWarning>, GitAiError> {
    let mut warnings = Vec::new();

    // Check 1: Daemon sync status
    if !is_daemon_synced(repository)? {
        warnings.push(RebaseValidationWarning::DaemonSyncPending);
    }

    // Check 2: Uncommitted checkpoints
    let pending_checkpoints = count_pending_checkpoints(repository)?;
    if pending_checkpoints > 0 {
        warnings.push(RebaseValidationWarning::UncommittedCheckpoints {
            count: pending_checkpoints,
        });
    }

    // Check 3: Working logs exist for HEAD
    if let Ok(head_ref) = repository.head()
        && let Ok(head_sha) = head_ref.target()
        && !working_logs_exist_for(repository, &head_sha)?
    {
        warnings.push(RebaseValidationWarning::MissingWorkingLogs {
            head_sha: head_sha.clone(),
        });
    }

    // Check 4: Working log directory integrity
    if has_corrupted_working_logs(repository)? {
        warnings.push(RebaseValidationWarning::CorruptedWorkingLogs);
    }

    Ok(warnings)
}

/// Verify that attribution was preserved after a rebase.
///
/// Compares original commit SHAs (before rebase) to rebased commit SHAs (after rebase)
/// and checks if authorship notes migrated successfully.
pub fn verify_rebase_attribution(
    repository: &Repository,
    original_commits: &[String],
    rebased_commits: &[String],
) -> Result<RebaseVerificationResult, GitAiError> {
    let mut original_commits_with_notes = Vec::new();
    let mut missing_attribution = Vec::new();

    for (orig_sha, new_sha) in original_commits.iter().zip(rebased_commits) {
        // Check if original commit had a note
        if let Ok(Some(_orig_note)) = read_note(repository, orig_sha) {
            original_commits_with_notes.push(orig_sha.clone());

            // Check if rebased commit has a note
            if read_note(repository, new_sha)?.is_none() {
                missing_attribution.push((orig_sha.clone(), new_sha.clone()));
            }
        }
    }

    Ok(RebaseVerificationResult {
        original_commits_with_notes,
        rebased_commits: rebased_commits.to_vec(),
        missing_attribution,
    })
}

/// Display verification results to user with actionable suggestions.
pub fn display_verification_results(result: &RebaseVerificationResult) {
    if !result.has_attribution_loss() {
        return;
    }

    eprintln!("\n[git-ai] ERROR: Authorship notes lost during rebase!");
    eprintln!(
        "  {} of {} commits missing AI attribution metadata",
        result.missing_attribution.len(),
        result.original_commits_with_notes.len()
    );
    eprintln!(
        "  Attribution survival rate: {:.1}%",
        result.attribution_survival_rate() * 100.0
    );
    eprintln!();
    eprintln!("Lost attribution for commits:");
    for (orig, new) in &result.missing_attribution {
        eprintln!("  {} → {}", &orig[..8], &new[..8]);
    }
    eprintln!();
    eprintln!("To recover:");
    if let (Some(first_orig), Some(last_new)) = (
        result.missing_attribution.first().map(|(o, _)| o),
        result.missing_attribution.last().map(|(_, n)| n),
    ) {
        eprintln!(
            "  git-ai rebase recover --from {} --to {}",
            first_orig, last_new
        );
    }
    eprintln!();
}

// ============================================================================
// Helper functions (implementation stubs for now)
// ============================================================================

/// Check if daemon has finished syncing all pending operations.
fn is_daemon_synced(repository: &Repository) -> Result<bool, GitAiError> {
    // Check if there's a pending flush by looking for the daemon's sync marker
    let sync_marker = repository.storage.ai_dir.join("daemon_sync_pending");

    // If the marker file doesn't exist, daemon is synced
    // If it exists, daemon has pending work
    Ok(!sync_marker.exists())
}

/// Count uncommitted checkpoints (working log entries not yet in commits).
fn count_pending_checkpoints(repository: &Repository) -> Result<usize, GitAiError> {
    let working_logs_base = &repository.storage.working_logs;

    if !working_logs_base.exists() {
        return Ok(0);
    }

    // Count directories in working_logs that have content
    let mut count = 0;
    if let Ok(entries) = std::fs::read_dir(working_logs_base) {
        for entry in entries.flatten() {
            if let Ok(metadata) = entry.metadata()
                && metadata.is_dir()
                && let Ok(log_entries) = std::fs::read_dir(entry.path())
                && log_entries.count() > 0
            {
                count += 1;
            }
        }
    }

    Ok(count)
}

/// Check if working logs exist for a specific commit.
fn working_logs_exist_for(repository: &Repository, commit_sha: &str) -> Result<bool, GitAiError> {
    let working_logs_dir = repository.storage.working_logs.join(commit_sha);

    Ok(working_logs_dir.exists() && working_logs_dir.is_dir())
}

/// Check if working log directory is corrupted.
fn has_corrupted_working_logs(repository: &Repository) -> Result<bool, GitAiError> {
    let working_logs_base = &repository.storage.working_logs;

    if !working_logs_base.exists() {
        return Ok(false); // Not corrupted, just doesn't exist yet
    }

    // Check if directory is readable
    match std::fs::read_dir(working_logs_base) {
        Ok(_) => Ok(false),
        Err(_) => Ok(true), // Can't read directory - consider corrupted
    }
}

/// Read authorship note for a commit.
fn read_note(repository: &Repository, commit_sha: &str) -> Result<Option<String>, GitAiError> {
    Ok(crate::git::refs::show_authorship_note(
        repository, commit_sha,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validation_warning_messages() {
        let warning = RebaseValidationWarning::DaemonSyncPending;
        assert!(warning.message().contains("sync pending"));

        let warning = RebaseValidationWarning::UncommittedCheckpoints { count: 3 };
        assert!(warning.message().contains("3"));
        assert!(warning.message().contains("checkpoints"));

        let warning = RebaseValidationWarning::MissingWorkingLogs {
            head_sha: "abc123def456".to_string(),
        };
        assert!(warning.message().contains("abc123de"));
    }

    #[test]
    fn test_verification_result_survival_rate() {
        let result = RebaseVerificationResult {
            original_commits_with_notes: vec!["a".into(), "b".into(), "c".into(), "d".into()],
            rebased_commits: vec!["a'".into(), "b'".into(), "c'".into(), "d'".into()],
            missing_attribution: vec![("b".into(), "b'".into())], // 1 of 4 lost
        };

        assert!(result.has_attribution_loss());
        assert_eq!(result.attribution_survival_rate(), 0.75); // 75%
    }

    #[test]
    fn test_verification_result_no_loss() {
        let result = RebaseVerificationResult {
            original_commits_with_notes: vec!["a".into(), "b".into()],
            rebased_commits: vec!["a'".into(), "b'".into()],
            missing_attribution: vec![],
        };

        assert!(!result.has_attribution_loss());
        assert_eq!(result.attribution_survival_rate(), 1.0); // 100%
    }
}
