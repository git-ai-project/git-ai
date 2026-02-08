use crate::authorship::working_log::CheckpointKind;
use crate::error::GitAiError;
use crate::git::repository::Repository;

pub fn pre_commit(repo: &Repository, default_author: String) -> Result<(), GitAiError> {
    // Run checkpoint as human editor.
    let result: Result<(usize, usize, usize), GitAiError> = crate::commands::checkpoint::run(
        repo,
        &default_author,
        CheckpointKind::Human,
        false,
        false,
        true,
        None,
        true, // should skip if NO AI CHECKPOINTS
    );
    result.map(|_| ())
}
