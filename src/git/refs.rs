use crate::error::GitAiError;
use crate::git::repository::Repository;
use crate::log_fmt::authorship_log_serialization::{AUTHORSHIP_LOG_VERSION, AuthorshipLog};
use crate::log_fmt::working_log::Checkpoint;
use serde_json;

pub const AI_AUTHORSHIP_REFSPEC: &str = "+refs/notes/ai/authorship:refs/notes/ai/authorship";

/// Store content as a git note
///
/// This function stores content as a git note attached to a commit.
/// The ref_name should be in the format "ai/authorship/{commit_sha}".
pub fn put_reference(
    repo: &Repository,
    ref_name: &str,
    content: &str,
    _message: &str,
) -> Result<(), GitAiError> {
    // Parse ref_name to extract commit SHA
    // Expected format: "ai/authorship/{commit_sha}"
    let parts: Vec<&str> = ref_name.split('/').collect();
    if parts.len() != 3 || parts[0] != "ai" || parts[1] != "authorship" {
        return Err(GitAiError::Generic(format!(
            "Invalid ref_name format: {}. Expected ai/authorship/{{commit_sha}}",
            ref_name
        )));
    }
    let commit_sha = parts[2];

    // Use git notes to store the content
    // The notes ref is "ai/authorship" which will become "refs/notes/ai/authorship"
    repo.notes_add("ai/authorship", commit_sha, content, true)?;

    Ok(())
}

/// Retrieve content from a git note
///
/// This function retrieves content from a git note attached to a commit.
/// The ref_name should be in the format "ai/authorship/{commit_sha}".
pub fn get_reference(repo: &Repository, ref_name: &str) -> Result<String, GitAiError> {
    // Parse ref_name to extract commit SHA
    // Expected format: "ai/authorship/{commit_sha}"
    let parts: Vec<&str> = ref_name.split('/').collect();
    if parts.len() != 3 || parts[0] != "ai" || parts[1] != "authorship" {
        return Err(GitAiError::Generic(format!(
            "Invalid ref_name format: {}. Expected ai/authorship/{{commit_sha}}",
            ref_name
        )));
    }
    let commit_sha = parts[2];

    // Use git notes to retrieve the content
    // The notes ref is "ai/authorship" which will become "refs/notes/ai/authorship"
    let content = repo.notes_show("ai/authorship", commit_sha)?;

    Ok(content)
}

#[allow(dead_code)]
pub fn get_reference_as_working_log(
    repo: &Repository,
    ref_name: &str,
) -> Result<Vec<Checkpoint>, GitAiError> {
    let content = get_reference(repo, ref_name)?;
    let working_log = serde_json::from_str(&content)?;
    Ok(working_log)
}

pub fn get_reference_as_authorship_log_v3(
    repo: &Repository,
    ref_name: &str,
) -> Result<AuthorshipLog, GitAiError> {
    let content = get_reference(repo, ref_name)?;

    // Try to deserialize as AuthorshipLog
    let authorship_log = match AuthorshipLog::deserialize_from_string(&content) {
        Ok(log) => log,
        Err(_) => {
            return Err(GitAiError::Generic(
                "Failed to parse authorship log".to_string(),
            ));
        }
    };

    // Check version compatibility
    if authorship_log.metadata.schema_version != AUTHORSHIP_LOG_VERSION {
        return Err(GitAiError::Generic(format!(
            "Unsupported authorship log version: {} (expected: {})",
            authorship_log.metadata.schema_version, AUTHORSHIP_LOG_VERSION
        )));
    }

    Ok(authorship_log)
}

// TODO Implement later if needed (requires a new reference.delete() method in repository.rs)
// #[allow(dead_code)]
// pub fn delete_reference(repo: &Repository, ref_name: &str) -> Result<(), GitAiError> {
//     let full_ref_name = format!("refs/{}", ref_name);

//     // Try to find and delete the reference
//     match repo.find_reference(&full_ref_name) {
//         Ok(mut reference) => {
//             reference.delete()?;
//             Ok(())
//         }
//         Err(_) => {
//             // Reference doesn't exist, which is fine for deletion
//             Ok(())
//         }
//     }
// }
