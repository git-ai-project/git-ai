//! Working-log reconstruction after `git restore --source <commit> -- <files>`.
//!
//! `git restore --source <C> -- <files>` rewrites the worktree/index for the
//! listed files using commit `C`'s versions, but fires no checkpoint and leaves
//! HEAD untouched. Without help, the next `git commit` finds an empty working
//! log for its base and attributes every restored line as untracked -- even
//! though the restored bytes are byte-identical to content that `C` already
//! attributes (AI or human) in its authorship note.
//!
//! This mirrors `rewrite_reset::reconstruct_working_log_after_backward_reset`,
//! but is simpler: the restored bytes equal `C`'s bytes 1:1 per file, so the
//! per-file line attributions map directly with NO line shifting. We read `C`'s
//! note, extract the attributions for the restored files, and write them as an
//! INITIAL working log keyed to the current HEAD (the base the next commit
//! uses). The existing post-commit `VirtualAttributions` machinery then
//! reconciles that INITIAL baseline against the committed content -- exactly as
//! it already does for reset and stash.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::authorship::attribution_tracker::LineAttribution;
use crate::authorship::authorship_log::{HumanRecord, PromptRecord, SessionRecord};
use crate::authorship::authorship_log_serialization::AuthorshipLog;
use crate::authorship::rewrite_reset::extract_attributions_from_log_shifted;
use crate::error::GitAiError;
use crate::git::notes_api;
use crate::git::repository::{Repository, batch_read_paths_at_treeishes};

/// Trailing-`*` prefix glob matcher mirroring `rewrite_stash::path_matches_any`,
/// so partial restores scoped to a directory/glob attribute only those paths.
///
/// `path` is repo-root-relative (authorship-log space); `pathspecs` come from the
/// command's raw argv. As with the stash handler, a pathspec given relative to a
/// subdirectory CWD (e.g. `feature.ts` from `src/`) is not CWD-normalized and so
/// may not match the root-relative log path. This shares the trace2 raw-argv
/// limitation of `rewrite_stash` and is acceptable for the common root-CWD case.
fn path_matches_any(path: &str, pathspecs: &[String]) -> bool {
    pathspecs.iter().any(|spec| {
        if let Some(prefix) = spec.strip_suffix('*') {
            return path.starts_with(prefix);
        }
        let normalized = spec.trim_end_matches('/');
        path == spec || path == normalized || {
            let prefix = format!("{}/", normalized);
            path.starts_with(&prefix)
        }
    })
}

/// Seed an INITIAL working log at `head` from `source_oid`'s authorship note for
/// the restored `pathspecs`, so the next commit preserves their attribution.
pub fn reconstruct_working_log_after_restore(
    repo: &Repository,
    source_oid: &str,
    head: &str,
    pathspecs: &[String],
) -> Result<(), GitAiError> {
    if pathspecs.is_empty() {
        return Ok(());
    }

    // Read the source commit's authorship note. No note => nothing to carry.
    let notes = notes_api::read_notes_batch(repo, &[source_oid.to_string()])?;
    let Some(raw_note) = notes.get(source_oid) else {
        return Ok(());
    };
    let Ok(log) = AuthorshipLog::deserialize_from_string(raw_note) else {
        return Ok(());
    };

    // Extract per-file attributions with NO shifting (hunks = None => 1:1).
    let mut file_attributions: HashMap<String, Vec<LineAttribution>> = HashMap::new();
    let mut prompts: HashMap<String, PromptRecord> = HashMap::new();
    let mut sessions: BTreeMap<String, SessionRecord> = BTreeMap::new();
    let mut humans: BTreeMap<String, HumanRecord> = BTreeMap::new();
    extract_attributions_from_log_shifted(
        &log,
        None,
        &mut file_attributions,
        &mut prompts,
        &mut sessions,
        &mut humans,
    );

    // Restrict to the files actually restored.
    file_attributions.retain(|path, _| path_matches_any(path, pathspecs));
    if file_attributions.is_empty() {
        return Ok(());
    }

    // Files that already carry a live checkpoint in the current working log are
    // owned by that checkpoint -- the customer's no-checkpoint scenario does not
    // apply to them, and the post-commit reconciliation already handles them.
    // Drop them so the restore baseline never clobbers richer checkpoint data.
    let working_log = repo.storage.working_log_for_base_commit(head)?;
    let checkpointed_files: HashSet<String> = working_log
        .read_all_checkpoints()
        .unwrap_or_default()
        .iter()
        .flat_map(|cp| cp.entries.iter().map(|e| e.file.clone()))
        .collect();
    file_attributions.retain(|path, _| !checkpointed_files.contains(path));
    if file_attributions.is_empty() {
        return Ok(());
    }

    // Snapshot the restored content from the source commit (the restored bytes
    // equal source_oid's version for each file). Skip files absent/empty there.
    let blob_requests: Vec<(String, String)> = file_attributions
        .keys()
        .map(|file| (source_oid.to_string(), file.clone()))
        .collect();
    let tree_contents = batch_read_paths_at_treeishes(repo, &blob_requests)?;

    let mut contents: HashMap<String, String> = HashMap::new();
    for file in file_attributions.keys() {
        if let Some(content) = tree_contents.get(&(source_oid.to_string(), file.clone()))
            && !content.is_empty()
        {
            contents.insert(file.clone(), content.clone());
        }
    }
    file_attributions.retain(|path, _| contents.contains_key(path));
    if file_attributions.is_empty() {
        return Ok(());
    }

    // Merge directly into the existing INITIAL: set entries (and persist blobs)
    // for the restored files, while leaving every other file's entry and stored
    // blob snapshot untouched. This avoids round-tripping unrelated files through
    // content reconstruction, so a restore can never drop another file's INITIAL
    // attribution. We only touch the INITIAL file -- never checkpoints.jsonl (see
    // rewrite_reset.rs note).
    let mut initial = working_log.read_initial_attributions();
    for (path, attrs) in file_attributions {
        let content = contents.remove(&path).unwrap_or_default();
        let blob_sha = working_log.persist_file_version(&content)?;
        initial.files.insert(path.clone(), attrs);
        initial.file_blobs.insert(path, blob_sha);
    }
    for (k, v) in prompts {
        initial.prompts.entry(k).or_insert(v);
    }
    for (k, v) in humans {
        initial.humans.entry(k).or_insert(v);
    }
    for (k, v) in sessions {
        initial.sessions.entry(k).or_insert(v);
    }

    working_log.write_initial(initial)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::path_matches_any;

    #[test]
    fn matches_exact_path() {
        let specs = vec!["src/a.ts".to_string()];
        assert!(path_matches_any("src/a.ts", &specs));
        assert!(!path_matches_any("src/b.ts", &specs));
    }

    #[test]
    fn matches_directory_pathspec() {
        let specs = vec!["src/".to_string()];
        assert!(path_matches_any("src/a.ts", &specs));
        assert!(path_matches_any("src/nested/b.ts", &specs));
        assert!(!path_matches_any("other/c.ts", &specs));
    }

    #[test]
    fn matches_trailing_glob() {
        let specs = vec!["src/foo*".to_string()];
        assert!(path_matches_any("src/foobar.ts", &specs));
        assert!(!path_matches_any("src/bar.ts", &specs));
    }
}
