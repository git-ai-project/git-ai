use crate::authorship::attribution_tracker::LineAttribution;
use crate::authorship::authorship_log::{HumanRecord, LineRange, PromptRecord, SessionRecord};
use crate::authorship::authorship_log_serialization::AuthorshipLog;
use crate::error::GitAiError;
use crate::git::notes_api::read_authorship_v3;
use crate::git::repo_storage::InitialAttributions;
use crate::git::repository::Repository;
use std::collections::HashMap;

/// Handles working log reconstruction after a backward reset (e.g. git reset --mixed HEAD~N).
///
/// After reset, HEAD is at new_tip but working tree still has content from old_tip.
/// We need to reconstruct working log entries from the authorship notes of the
/// "un-done" commits so that the next commit preserves AI attribution.
pub fn reconstruct_working_log_after_backward_reset(
    repo: &Repository,
    old_tip: &str,
    new_tip: &str,
) -> Result<(), GitAiError> {
    // List all commits being "un-done" (between new_tip exclusive and old_tip inclusive)
    let commits = list_commits_in_range(repo, new_tip, old_tip);
    if commits.is_empty() {
        return Ok(());
    }

    // Collect authorship from all un-done commits
    let mut file_attributions: HashMap<String, Vec<LineAttribution>> = HashMap::new();
    let mut prompts: HashMap<String, PromptRecord> = HashMap::new();
    let mut sessions: std::collections::BTreeMap<String, SessionRecord> =
        std::collections::BTreeMap::new();
    let mut humans: std::collections::BTreeMap<String, HumanRecord> =
        std::collections::BTreeMap::new();

    for commit_sha in &commits {
        let Ok(log) = read_authorship_v3(repo, commit_sha) else {
            continue;
        };
        extract_attributions_from_log(
            &log,
            &mut file_attributions,
            &mut prompts,
            &mut sessions,
            &mut humans,
        );
    }

    if file_attributions.is_empty() {
        return Ok(());
    }

    // Read current file contents from working directory for blob snapshots.
    // Only include files that differ from new_tip (the reset target).
    // If a file matches new_tip exactly (e.g., reset --hard), there's no
    // uncommitted AI content to track.
    let workdir = repo.workdir()?;
    let mut file_blobs: HashMap<String, String> = HashMap::new();
    for file_path in file_attributions.keys() {
        let abs_path = workdir.join(file_path);
        if abs_path.exists()
            && let Ok(content) = std::fs::read_to_string(&abs_path)
        {
            let target_content = file_content_at_commit(repo, new_tip, file_path);
            if content != target_content {
                file_blobs.insert(file_path.clone(), content);
            }
        }
    }

    // If no files differ from the target (reset --hard), nothing to reconstruct
    if file_blobs.is_empty() {
        let _ = repo.storage.delete_working_log_for_base_commit(old_tip);
        return Ok(());
    }

    // Only keep attributions for files that have uncommitted content
    file_attributions.retain(|path, _| file_blobs.contains_key(path));

    // Write as initial working log for new_tip
    let working_log = repo.storage.working_log_for_base_commit(new_tip)?;
    working_log.reset_working_log()?;

    let initial = InitialAttributions {
        files: file_attributions,
        prompts,
        file_blobs,
        humans,
        sessions,
    };
    working_log.write_initial(initial)?;

    // Delete old working log if it exists
    let _ = repo.storage.delete_working_log_for_base_commit(old_tip);

    Ok(())
}

fn extract_attributions_from_log(
    log: &AuthorshipLog,
    file_attributions: &mut HashMap<String, Vec<LineAttribution>>,
    prompts: &mut HashMap<String, PromptRecord>,
    sessions: &mut std::collections::BTreeMap<String, SessionRecord>,
    humans: &mut std::collections::BTreeMap<String, HumanRecord>,
) {
    for fa in &log.attestations {
        let attrs = file_attributions.entry(fa.file_path.clone()).or_default();
        for entry in &fa.entries {
            for range in &entry.line_ranges {
                let (start, end) = match range {
                    LineRange::Single(l) => (*l, *l),
                    LineRange::Range(s, e) => (*s, *e),
                };
                attrs.push(LineAttribution::new(start, end, entry.hash.clone(), None));
            }
        }
    }

    for (key, record) in &log.metadata.prompts {
        prompts.entry(key.clone()).or_insert_with(|| record.clone());
    }
    for (key, record) in &log.metadata.sessions {
        sessions
            .entry(key.clone())
            .or_insert_with(|| record.clone());
    }
    for (key, record) in &log.metadata.humans {
        humans.entry(key.clone()).or_insert_with(|| record.clone());
    }
}

fn list_commits_in_range(repo: &Repository, base: &str, tip: &str) -> Vec<String> {
    crate::authorship::rewrite::list_commits_in_range(repo, base, tip)
}

pub(crate) fn file_content_at_commit(repo: &Repository, commit: &str, file_path: &str) -> String {
    use crate::git::repository::exec_git_allow_nonzero;
    let mut args = repo.global_args_for_exec();
    args.extend(["show".to_string(), format!("{}:{}", commit, file_path)]);
    exec_git_allow_nonzero(&args)
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default()
}
