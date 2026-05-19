use crate::error::GitAiError;
use crate::git::repository::Repository;

/// Handles working log after a backward reset (git reset --soft HEAD~N).
/// After reset, HEAD is at new_tip but working tree is unchanged.
/// The working log at old_tip needs to be re-keyed to new_tip.
/// Since reset --soft doesn't change the working tree, the attribution
/// line numbers are still correct for the current file state.
pub fn reconstruct_working_log_after_backward_reset(
    repo: &Repository,
    old_tip: &str,
    new_tip: &str,
) -> Result<(), GitAiError> {
    let working_logs_dir = &repo.storage.working_logs;
    let old_dir = working_logs_dir.join(old_tip);

    if !old_dir.exists() {
        return Ok(());
    }

    let new_dir = working_logs_dir.join(new_tip);
    if new_dir.exists() {
        // new_tip already has a working log — don't overwrite
        return Ok(());
    }

    // After reset --soft, working tree hasn't changed, so attributions
    // are still valid in their current coordinate space. Just re-key.
    std::fs::rename(&old_dir, &new_dir)?;
    Ok(())
}
