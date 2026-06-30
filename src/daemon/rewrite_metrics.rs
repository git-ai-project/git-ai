use std::collections::{HashMap, HashSet};

use crate::authorship::authorship_log_serialization::AuthorshipLog;
use crate::authorship::ignore::effective_ignore_patterns;
use crate::authorship::post_commit::metric_tool_model_breakdown;
use crate::authorship::rewrite::{DiffTreeResult, RewriteMetricCommit};
use crate::config::Config;
use crate::error::GitAiError;
use crate::git::repository::Repository;
use crate::metrics::{EventAttributes, MetricEvent, PosEncoded, RewriteCommittedValues};

pub(crate) fn spawn_rewrite_commit_metrics(
    repo: &Repository,
    metric_commits: Vec<RewriteMetricCommit>,
) {
    if !crate::authorship::rewrite::rewrite_metrics_enabled() {
        return;
    }
    if metric_commits.is_empty() {
        return;
    }

    let repo = repo.clone();
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
        runtime.spawn(async move {
            let result = tokio::task::spawn_blocking(move || {
                build_rewrite_metric_events(&repo, &metric_commits)
            })
            .await;
            match result {
                Ok(events) => submit_events(events),
                Err(err) => tracing::warn!(%err, "rewrite metrics worker panicked"),
            }
        });
    } else {
        std::thread::spawn(move || {
            submit_events(build_rewrite_metric_events(&repo, &metric_commits));
        });
    }
}

fn submit_events(events: Vec<MetricEvent>) {
    if !events.is_empty() {
        crate::observability::log_metrics(events);
    }
}

pub(crate) fn dedupe_metric_commits(
    metric_commits: Vec<RewriteMetricCommit>,
) -> Vec<RewriteMetricCommit> {
    #[derive(Hash, PartialEq, Eq)]
    struct MetricCommitKey {
        new_sha: String,
        original_shas: Vec<String>,
        operation: crate::authorship::rewrite::RewriteMetricOperation,
        branch: Option<String>,
    }

    let mut deduped = Vec::new();
    let mut indices_by_key: HashMap<MetricCommitKey, usize> = HashMap::new();
    for commit in metric_commits {
        if commit.new_sha.is_empty() {
            continue;
        }
        let key = MetricCommitKey {
            new_sha: commit.new_sha.clone(),
            original_shas: commit.original_shas.clone(),
            operation: commit.operation,
            branch: commit.branch.clone(),
        };
        if let Some(index) = indices_by_key.get(&key).copied() {
            merge_metric_commit_context(&mut deduped[index], commit);
        } else {
            indices_by_key.insert(key, deduped.len());
            deduped.push(commit);
        }
    }
    deduped
}

fn merge_metric_commit_context(target: &mut RewriteMetricCommit, source: RewriteMetricCommit) {
    if target.parent_sha.is_none() {
        target.parent_sha = source.parent_sha;
    }
    if target.authorship_note.is_none() {
        target.authorship_note = source.authorship_note;
    }
    if target.parent_diff.is_none() {
        target.parent_diff = source.parent_diff;
    }
}

fn build_rewrite_metric_events(
    repo: &Repository,
    metric_commits: &[RewriteMetricCommit],
) -> Vec<MetricEvent> {
    let mut metric_commits = dedupe_metric_commits(metric_commits.to_vec());
    hydrate_missing_parent_shas(repo, &mut metric_commits);
    hydrate_missing_parent_diffs(repo, &mut metric_commits);
    let batch_context = RewriteMetricBatchContext::new(repo);

    let mut events = Vec::new();
    for metric_commit in &metric_commits {
        match build_rewrite_committed_metric_event(metric_commit, &batch_context) {
            Ok(Some(event)) => events.push(event),
            Ok(None) => {}
            Err(err) => {
                tracing::debug!(
                    %err,
                    commit_sha = %metric_commit.new_sha,
                    operation_kind = metric_commit.operation.as_str(),
                    "skipping rewrite committed metric"
                );
            }
        }
    }
    events
}

fn hydrate_missing_parent_shas(repo: &Repository, metric_commits: &mut [RewriteMetricCommit]) {
    let mut new_shas = Vec::new();
    let mut seen = HashSet::new();
    for metric_commit in metric_commits.iter() {
        if metric_commit.parent_sha.is_some() {
            continue;
        }
        if seen.insert(metric_commit.new_sha.clone()) {
            new_shas.push(metric_commit.new_sha.clone());
        }
    }
    if new_shas.is_empty() {
        return;
    }

    let Some(parent_by_commit) = parent_shas_for_commits(repo, &new_shas) else {
        return;
    };
    for metric_commit in metric_commits {
        if metric_commit.parent_sha.is_none()
            && let Some(parent_sha) = parent_by_commit.get(&metric_commit.new_sha)
        {
            metric_commit.parent_sha = Some(parent_sha.clone());
        }
    }
}

fn parent_shas_for_commits(
    repo: &Repository,
    commit_shas: &[String],
) -> Option<HashMap<String, String>> {
    if commit_shas.is_empty() {
        return Some(HashMap::new());
    }

    let mut args = repo.global_args_for_exec();
    args.extend([
        "show".to_string(),
        "-s".to_string(),
        "--format=%H %P".to_string(),
        "--no-walk".to_string(),
    ]);
    args.extend(commit_shas.iter().cloned());

    let output = crate::git::repository::exec_git(&args).ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut parent_by_commit = HashMap::new();
    for line in stdout.lines() {
        let mut parts = line.split_whitespace();
        let Some(commit_sha) = parts.next() else {
            continue;
        };
        let parents = parts.collect::<Vec<_>>();
        match parents.as_slice() {
            [] => {
                parent_by_commit.insert(commit_sha.to_string(), "initial".to_string());
            }
            [parent_sha] => {
                parent_by_commit.insert(commit_sha.to_string(), (*parent_sha).to_string());
            }
            _ => {
                // Existing rewrite metrics skip merge commits.
            }
        }
    }
    Some(parent_by_commit)
}

struct RewriteMetricBatchContext {
    ignore_patterns: Vec<String>,
    repo_url: Option<String>,
    custom_attributes_json: Option<String>,
}

impl RewriteMetricBatchContext {
    fn new(repo: &Repository) -> Self {
        Self {
            ignore_patterns: effective_ignore_patterns(repo, &[], &[]),
            repo_url: rewrite_metric_repo_url(repo),
            custom_attributes_json: rewrite_metric_custom_attributes_json(),
        }
    }
}

fn rewrite_metric_repo_url(repo: &Repository) -> Option<String> {
    let remotes = repo.remotes_with_urls().ok()?;
    let (_, url) = remotes
        .iter()
        .find(|(name, _)| name == "origin")
        .or_else(|| remotes.first())?;
    crate::repo_url::normalize_repo_url(url).ok()
}

fn rewrite_metric_custom_attributes_json() -> Option<String> {
    let config = Config::fresh();
    let attrs = config.custom_attributes();
    if attrs.is_empty() {
        None
    } else {
        serde_json::to_string(attrs).ok()
    }
}

fn hydrate_missing_parent_diffs(repo: &Repository, metric_commits: &mut [RewriteMetricCommit]) {
    let mut indices = Vec::new();
    let mut pairs = Vec::new();
    for (index, metric_commit) in metric_commits.iter().enumerate() {
        if metric_commit.parent_diff.is_some() {
            continue;
        }
        let Some(parent_sha) = metric_commit.parent_sha.as_ref() else {
            continue;
        };
        indices.push(index);
        pairs.push((parent_sha.clone(), metric_commit.new_sha.clone()));
    }
    if pairs.is_empty() {
        return;
    }

    let Ok(results) = crate::authorship::rewrite::compute_diff_trees_batch(repo, &pairs) else {
        return;
    };
    for (index, result) in indices.into_iter().zip(results) {
        metric_commits[index].parent_diff = Some(result);
    }
}

fn build_rewrite_committed_metric_event(
    metric_commit: &RewriteMetricCommit,
    batch_context: &RewriteMetricBatchContext,
) -> Result<Option<MetricEvent>, GitAiError> {
    let Some(raw_note) = metric_commit.authorship_note.as_ref() else {
        return Ok(None);
    };
    let authorship_log = match AuthorshipLog::deserialize_from_string(raw_note) {
        Ok(log) => log,
        Err(_) => return Ok(None),
    };

    let Some(parent_diff) = metric_commit.parent_diff.as_ref() else {
        return Ok(None);
    };

    let diff_hunks = diff_hunks_from_diff_tree_result(parent_diff);
    if should_skip_rewrite_metric_stats(&diff_hunks, &batch_context.ignore_patterns) {
        return Ok(None);
    }
    let stats = crate::authorship::stats::stats_for_commit_stats_from_hunks_with_merge_flag(
        &batch_context.ignore_patterns,
        &diff_hunks,
        Some(&authorship_log),
        false,
    );
    let Some(breakdown) = metric_tool_model_breakdown(&stats) else {
        return Ok(None);
    };

    let mut values = RewriteCommittedValues::new()
        .human_additions(stats.human_additions)
        .git_diff_deleted_lines(stats.git_diff_deleted_lines)
        .git_diff_added_lines(stats.git_diff_added_lines)
        .tool_model_pairs(breakdown.tool_model_pairs)
        .ai_additions(breakdown.ai_additions)
        .ai_accepted(breakdown.ai_accepted)
        .authorship_note(raw_note.clone())
        .operation_kind(metric_commit.operation.as_str())
        .original_commit_shas(metric_commit.original_shas.clone());

    values = values.commit_subject_null().commit_body_null().hunks_null();

    let attrs = rewrite_metric_attrs(metric_commit, batch_context);

    Ok(Some(MetricEvent::from_values(values, attrs.to_sparse())))
}

fn diff_hunks_from_diff_tree_result(
    result: &DiffTreeResult,
) -> Vec<crate::commands::diff::DiffHunk> {
    let mut hunks = Vec::new();
    for (file_path, file_hunks) in &result.hunks_by_file {
        for hunk in file_hunks {
            hunks.push(crate::commands::diff::DiffHunk {
                file_path: file_path.clone(),
                old_file_path: None,
                old_start: hunk.old_start,
                old_count: hunk.old_count,
                new_start: hunk.new_start,
                new_count: hunk.new_count,
                deleted_lines: line_numbers(hunk.old_start, hunk.old_count),
                added_lines: line_numbers(hunk.new_start, hunk.new_count),
                deleted_contents: Vec::new(),
                added_contents: Vec::new(),
            });
        }
    }
    hunks
}

fn line_numbers(start: u32, count: u32) -> Vec<u32> {
    if count == 0 {
        return Vec::new();
    }
    (start..start.saturating_add(count))
        .filter(|line| *line > 0)
        .collect()
}

fn should_skip_rewrite_metric_stats(
    hunks: &[crate::commands::diff::DiffHunk],
    ignore_patterns: &[String],
) -> bool {
    let ignore_matcher = crate::authorship::ignore::build_ignore_matcher(ignore_patterns);
    let mut files_with_additions = std::collections::HashSet::new();
    let mut added_lines = 0usize;
    let mut deleted_lines = 0usize;
    let mut hunk_ranges = 0usize;

    for hunk in hunks {
        if crate::authorship::ignore::should_ignore_file_with_matcher(
            &hunk.file_path,
            &ignore_matcher,
        ) {
            continue;
        }
        if !hunk.added_lines.is_empty() {
            files_with_additions.insert(hunk.file_path.as_str());
            hunk_ranges += 1;
        }
        added_lines += hunk.added_lines.len();
        deleted_lines += hunk.deleted_lines.len();
    }

    hunk_ranges >= crate::authorship::post_commit::STATS_SKIP_MAX_HUNKS
        || added_lines >= crate::authorship::post_commit::STATS_SKIP_MAX_ADDED_LINES
        || files_with_additions.len()
            >= crate::authorship::post_commit::STATS_SKIP_MAX_FILES_WITH_ADDITIONS
        || deleted_lines >= crate::authorship::post_commit::STATS_SKIP_MAX_DELETED_LINES
}

fn rewrite_metric_attrs(
    metric_commit: &RewriteMetricCommit,
    batch_context: &RewriteMetricBatchContext,
) -> EventAttributes {
    let base_commit_sha = metric_commit.parent_sha.as_deref().unwrap_or("initial");
    let mut attrs = EventAttributes::with_version(env!("CARGO_PKG_VERSION"))
        .commit_sha(metric_commit.new_sha.clone())
        .base_commit_sha(base_commit_sha);

    attrs = apply_rewrite_metric_branch(attrs, metric_commit);

    if let Some(repo_url) = batch_context.repo_url.as_deref() {
        attrs = attrs.repo_url(repo_url);
    }

    attrs = apply_rewrite_metric_custom_attributes(
        attrs,
        batch_context.custom_attributes_json.as_deref(),
    );

    attrs
}

fn apply_rewrite_metric_custom_attributes(
    attrs: EventAttributes,
    custom_attributes_json: Option<&str>,
) -> EventAttributes {
    if let Some(custom_attributes_json) = custom_attributes_json {
        // `custom_attributes_map` serializes the map and stores this same string field.
        // Rewrite metrics pre-serialize once per batch to avoid repeated serde work.
        attrs.custom_attributes(custom_attributes_json)
    } else {
        attrs
    }
}

fn apply_rewrite_metric_branch(
    attrs: EventAttributes,
    metric_commit: &RewriteMetricCommit,
) -> EventAttributes {
    if let Some(branch) = metric_commit.branch.as_deref() {
        return attrs.branch(branch);
    }

    attrs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authorship::authorship_log::LineRange;
    use crate::authorship::authorship_log_serialization::AttestationEntry;
    use crate::authorship::rewrite::RewriteMetricOperation;
    use crate::authorship::working_log::AgentId;
    use crate::metrics::EventValues;
    use crate::metrics::events::rewrite_committed_pos;
    use std::collections::HashMap;

    fn metric_commit(
        new_sha: &str,
        originals: &[&str],
        operation: RewriteMetricOperation,
    ) -> RewriteMetricCommit {
        RewriteMetricCommit::new(
            new_sha.to_string(),
            originals.iter().map(|s| s.to_string()).collect(),
            operation,
        )
    }

    fn note_for_ai_line(file_path: &str, line: u32) -> String {
        let prompt_id = "prompt1".to_string();
        let mut log = AuthorshipLog::new();
        log.metadata.prompts.insert(
            prompt_id.clone(),
            crate::authorship::authorship_log::PromptRecord {
                agent_id: AgentId {
                    tool: "codex".to_string(),
                    id: "session".to_string(),
                    model: "gpt-5".to_string(),
                },
                human_author: None,
                messages_url: None,
                total_additions: 0,
                total_deletions: 0,
                accepted_lines: 0,
                overriden_lines: 0,
                custom_attributes: None,
            },
        );
        log.get_or_create_file(file_path)
            .add_entry(AttestationEntry::new(
                prompt_id,
                vec![LineRange::Single(line)],
            ));
        log.serialize_to_string().expect("serialize note")
    }

    #[test]
    fn dedupe_metric_commits_keeps_distinct_original_sets() {
        let first = metric_commit("new", &["old1"], RewriteMetricOperation::Rebase);
        let second = metric_commit("new", &["old1"], RewriteMetricOperation::Rebase);
        let squash = metric_commit(
            "new",
            &["old1", "old2"],
            RewriteMetricOperation::SquashMerge,
        );

        let result = dedupe_metric_commits(vec![first.clone(), second, squash.clone()]);

        assert_eq!(result, vec![first, squash]);
    }

    #[test]
    fn rewrite_event_schema_does_not_emit_position_10() {
        let values = RewriteCommittedValues::new()
            .human_additions(1)
            .git_diff_deleted_lines(2)
            .git_diff_added_lines(3)
            .tool_model_pairs(vec!["all".to_string()])
            .ai_additions(vec![1])
            .ai_accepted(vec![1])
            .commit_subject("subject")
            .commit_body_null()
            .authorship_note("note")
            .hunks("[]")
            .operation_kind("rebase")
            .original_commit_shas(vec!["old".to_string()]);

        let sparse = PosEncoded::to_sparse(&values);

        assert_eq!(
            RewriteCommittedValues::event_id(),
            crate::metrics::types::MetricEventId::RewriteCommitted
        );
        assert!(!sparse.contains_key("10"));
        assert_eq!(
            sparse.get(&rewrite_committed_pos::OPERATION_KIND.to_string()),
            Some(&serde_json::json!("rebase"))
        );
        assert_eq!(
            sparse.get(&rewrite_committed_pos::ORIGINAL_COMMIT_SHAS.to_string()),
            Some(&serde_json::json!(["old"]))
        );
    }

    #[test]
    fn rewrite_metric_branch_overrides_head_branch_attr() {
        let commit = metric_commit("new", &["old"], RewriteMetricOperation::NonFastForward)
            .with_branch("feature");
        let attrs = apply_rewrite_metric_branch(
            crate::metrics::EventAttributes::with_version("test").branch("main"),
            &commit,
        );
        let sparse = attrs.to_sparse();

        assert_eq!(
            sparse.get(&crate::metrics::attrs::attr_pos::BRANCH.to_string()),
            Some(&serde_json::json!("feature"))
        );
    }

    #[test]
    fn rewrite_metric_custom_attributes_match_map_builder_wire_format() {
        let mut custom_attributes = HashMap::new();
        custom_attributes.insert("team".to_string(), "metrics".to_string());
        let custom_attributes_json =
            serde_json::to_string(&custom_attributes).expect("serialize custom attributes");

        let sparse_from_batch_json = apply_rewrite_metric_custom_attributes(
            crate::metrics::EventAttributes::with_version("test"),
            Some(&custom_attributes_json),
        )
        .to_sparse();
        let sparse_from_map = crate::metrics::EventAttributes::with_version("test")
            .custom_attributes_map(&custom_attributes)
            .to_sparse();

        assert_eq!(
            sparse_from_batch_json
                .get(&crate::metrics::attrs::attr_pos::CUSTOM_ATTRIBUTES.to_string()),
            sparse_from_map.get(&crate::metrics::attrs::attr_pos::CUSTOM_ATTRIBUTES.to_string())
        );
    }

    #[test]
    fn rewrite_metric_event_uses_supplied_note_and_parent_diff() {
        let tmp = crate::git::test_utils::TmpRepo::new().expect("tmp repo");
        let note = note_for_ai_line("file.txt", 1);

        let mut hunks_by_file = HashMap::new();
        hunks_by_file.insert(
            "file.txt".to_string(),
            vec![crate::authorship::hunk_shift::DiffHunk {
                old_start: 0,
                old_count: 0,
                new_start: 1,
                new_count: 1,
            }],
        );
        let parent_diff = DiffTreeResult {
            hunks_by_file,
            added_lines_by_file: HashMap::new(),
            renames: Vec::new(),
        };
        let commit = metric_commit("new", &["old"], RewriteMetricOperation::Rebase)
            .with_branch("feature")
            .with_parent_sha("parent")
            .with_authorship_note(note.clone())
            .with_parent_diff(parent_diff);

        let batch_context = RewriteMetricBatchContext::new(tmp.gitai_repo());
        let event = build_rewrite_committed_metric_event(&commit, &batch_context)
            .expect("metric build")
            .expect("event");

        assert_eq!(
            event
                .values
                .get(&rewrite_committed_pos::AUTHORSHIP_NOTE.to_string()),
            Some(&serde_json::json!(note))
        );
        assert_eq!(
            event
                .values
                .get(&rewrite_committed_pos::GIT_DIFF_ADDED_LINES.to_string()),
            Some(&serde_json::json!(1))
        );
        assert_eq!(
            event
                .values
                .get(&rewrite_committed_pos::TOOL_MODEL_PAIRS.to_string()),
            Some(&serde_json::json!(["all", "codex::gpt-5"]))
        );
        assert_eq!(
            event
                .attrs
                .get(&crate::metrics::attrs::attr_pos::BRANCH.to_string()),
            Some(&serde_json::json!("feature"))
        );
        assert_eq!(
            event
                .attrs
                .get(&crate::metrics::attrs::attr_pos::BASE_COMMIT_SHA.to_string()),
            Some(&serde_json::json!("parent"))
        );
    }

    #[test]
    fn rewrite_metric_worker_hydrates_missing_parent_and_diff() {
        let tmp = crate::git::test_utils::TmpRepo::new().expect("tmp repo");
        tmp.write_file("file.txt", "base\n", false)
            .expect("write base");
        let parent_sha = tmp.commit_all("base").expect("base commit");
        tmp.write_file("file.txt", "base\nai\n", false)
            .expect("write update");
        let new_sha = tmp.commit_all("update").expect("update commit");
        let note = note_for_ai_line("file.txt", 2);

        let commit = metric_commit(
            &new_sha,
            &[&parent_sha],
            RewriteMetricOperation::NonFastForward,
        )
        .with_authorship_note(note);

        let events = build_rewrite_metric_events(tmp.gitai_repo(), &[commit]);

        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0]
                .attrs
                .get(&crate::metrics::attrs::attr_pos::BASE_COMMIT_SHA.to_string()),
            Some(&serde_json::json!(parent_sha))
        );
        assert_eq!(
            events[0]
                .values
                .get(&rewrite_committed_pos::GIT_DIFF_ADDED_LINES.to_string()),
            Some(&serde_json::json!(1))
        );
    }

    #[test]
    fn rewrite_metric_worker_hydrates_initial_parent_diff() {
        let tmp = crate::git::test_utils::TmpRepo::new().expect("tmp repo");
        tmp.write_file("file.txt", "ai\n", false)
            .expect("write root");
        let root_sha = tmp.commit_all("root").expect("root commit");
        let note = note_for_ai_line("file.txt", 1);

        let commit = metric_commit(&root_sha, &["old"], RewriteMetricOperation::Amend)
            .with_authorship_note(note);

        let events = build_rewrite_metric_events(tmp.gitai_repo(), &[commit]);

        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0]
                .attrs
                .get(&crate::metrics::attrs::attr_pos::BASE_COMMIT_SHA.to_string()),
            Some(&serde_json::json!("initial"))
        );
        assert_eq!(
            events[0]
                .values
                .get(&rewrite_committed_pos::GIT_DIFF_ADDED_LINES.to_string()),
            Some(&serde_json::json!(1))
        );
    }
}
