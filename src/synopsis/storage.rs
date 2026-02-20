use crate::error::GitAiError;
use crate::git::repository::{Repository, exec_git, exec_git_stdin};
use crate::synopsis::types::Synopsis;

/// Store a synopsis as a git note on the commit referenced by
/// `synopsis.metadata.commit_sha`, under the given `notes_ref`.
///
/// The note content is a JSON object with `metadata` and `content` fields.
pub fn store_synopsis(
    repo: &Repository,
    synopsis: &Synopsis,
    notes_ref: &str,
) -> Result<(), GitAiError> {
    let json = serde_json::to_string_pretty(synopsis).map_err(GitAiError::JsonError)?;

    let mut args = repo.global_args_for_exec();
    args.push("notes".to_string());
    args.push(format!("--ref={}", notes_ref));
    args.push("add".to_string());
    args.push("-f".to_string()); // Force overwrite if a note already exists
    args.push("-F".to_string());
    args.push("-".to_string()); // Read content from stdin
    args.push(synopsis.metadata.commit_sha.clone());

    exec_git_stdin(&args, json.as_bytes())?;
    Ok(())
}

/// Retrieve the synopsis for a specific commit, or `None` if no note exists.
pub fn retrieve_synopsis(
    repo: &Repository,
    commit_sha: &str,
    notes_ref: &str,
) -> Result<Option<Synopsis>, GitAiError> {
    let mut args = repo.global_args_for_exec();
    args.push("notes".to_string());
    args.push(format!("--ref={}", notes_ref));
    args.push("show".to_string());
    args.push(commit_sha.to_string());

    match exec_git(&args) {
        Ok(output) => {
            let raw = String::from_utf8(output.stdout).map_err(GitAiError::FromUtf8Error)?;
            let raw = raw.trim();
            if raw.is_empty() {
                return Ok(None);
            }
            let synopsis: Synopsis = serde_json::from_str(raw).map_err(GitAiError::JsonError)?;
            Ok(Some(synopsis))
        }
        Err(GitAiError::GitCliError { code: Some(1), .. }) => Ok(None),
        Err(GitAiError::GitCliError {
            code: Some(128), ..
        }) => Ok(None),
        Err(e) => Err(e),
    }
}

/// List all commit SHAs that have a synopsis note under `notes_ref`.
///
/// The output of `git notes --ref=<ref> list` is lines of the form:
/// `<note-blob-sha> <commit-sha>`
pub fn list_synopses(repo: &Repository, notes_ref: &str) -> Result<Vec<String>, GitAiError> {
    let mut args = repo.global_args_for_exec();
    args.push("notes".to_string());
    args.push(format!("--ref={}", notes_ref));
    args.push("list".to_string());

    match exec_git(&args) {
        Ok(output) => {
            let stdout = String::from_utf8(output.stdout).map_err(GitAiError::FromUtf8Error)?;
            let shas = stdout
                .lines()
                .filter_map(|line| {
                    let parts: Vec<&str> = line.split_whitespace().collect();
                    parts.get(1).map(|s| s.to_string())
                })
                .collect();
            Ok(shas)
        }
        // refs/notes/<ref> doesn't exist yet — not an error
        Err(GitAiError::GitCliError { code: Some(1), .. }) => Ok(Vec::new()),
        Err(GitAiError::GitCliError {
            code: Some(128), ..
        }) => Ok(Vec::new()),
        Err(e) => Err(e),
    }
}
