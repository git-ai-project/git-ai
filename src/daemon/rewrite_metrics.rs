use crate::authorship::authorship_log_serialization::AuthorshipLog;
use crate::authorship::ignore::effective_ignore_patterns;
use crate::authorship::post_commit::{
    commit_metric_attrs, commit_subject_and_body, metric_tool_model_breakdown,
};
use crate::authorship::rewrite::RewriteMetricCommit;
use crate::config::Config;
use crate::error::GitAiError;
use crate::git::notes_api;
use crate::git::repository::Repository;
use crate::metrics::{MetricEvent, PosEncoded, RewriteCommittedValues};

const EMPTY_TREE_SHA: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

pub(crate) fn spawn_rewrite_commit_metrics(
    repo: &Repository,
    metric_commits: Vec<RewriteMetricCommit>,
) {
    if !Config::get().get_feature_flags().rewrite_metrics_events {
        return;
    }

    let fallback_branch = current_branch_name(repo);
    let metric_commits = metric_commits
        .into_iter()
        .map(|commit| {
            if commit.branch.is_some() {
                commit
            } else if let Some(branch) = fallback_branch.clone() {
                commit.with_branch(branch)
            } else {
                commit
            }
        })
        .collect();
    let metric_commits = dedupe_metric_commits(metric_commits);
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

fn current_branch_name(repo: &Repository) -> Option<String> {
    repo.head()
        .ok()
        .and_then(|head_ref| head_ref.shorthand().ok())
        .filter(|branch| !branch.is_empty())
}

fn submit_events(events: Vec<MetricEvent>) {
    if !events.is_empty() {
        crate::observability::log_metrics(events);
    }
}

pub(crate) fn dedupe_metric_commits(
    metric_commits: Vec<RewriteMetricCommit>,
) -> Vec<RewriteMetricCommit> {
    let mut deduped = Vec::new();
    for commit in metric_commits {
        if commit.new_sha.is_empty() {
            continue;
        }
        if !deduped.contains(&commit) {
            deduped.push(commit);
        }
    }
    deduped
}

fn build_rewrite_metric_events(
    repo: &Repository,
    metric_commits: &[RewriteMetricCommit],
) -> Vec<MetricEvent> {
    let mut events = Vec::new();
    for metric_commit in metric_commits {
        match build_rewrite_committed_metric_event(repo, metric_commit) {
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

pub(crate) fn build_rewrite_committed_metric_event(
    repo: &Repository,
    metric_commit: &RewriteMetricCommit,
) -> Result<Option<MetricEvent>, GitAiError> {
    let raw_note = match notes_api::read_note(repo, &metric_commit.new_sha) {
        Some(note) => note,
        None => return Ok(None),
    };
    let authorship_log = match AuthorshipLog::deserialize_from_string(&raw_note) {
        Ok(log) => log,
        Err(_) => return Ok(None),
    };

    let commit = repo.find_commit(metric_commit.new_sha.clone())?;
    let parent_count = commit.parent_count()?;
    if parent_count > 1 {
        return Ok(None);
    }

    let (base_commit_attr, diff_base) = if parent_count == 0 {
        ("initial".to_string(), EMPTY_TREE_SHA.to_string())
    } else {
        let parent_sha = commit.parent(0)?.id();
        (parent_sha.clone(), parent_sha)
    };

    let ignore_patterns = effective_ignore_patterns(repo, &[], &[]);
    let estimate = crate::authorship::post_commit::estimate_stats_cost_for_commit_range(
        repo,
        &diff_base,
        &metric_commit.new_sha,
        &ignore_patterns,
    )?;
    if estimate.should_skip() {
        return Ok(None);
    }

    let diff_hunks = crate::commands::diff::get_diff_with_line_numbers(
        repo,
        &diff_base,
        &metric_commit.new_sha,
    )?;
    let stats = crate::authorship::stats::stats_for_commit_stats_from_hunks(
        repo,
        &metric_commit.new_sha,
        &ignore_patterns,
        &diff_hunks,
        Some(&authorship_log),
    )?;
    let Some(breakdown) = metric_tool_model_breakdown(&stats) else {
        return Ok(None);
    };

    let hunks_json = crate::commands::diff::build_diff_artifacts_from_hunks(
        repo,
        diff_hunks,
        &metric_commit.new_sha,
        Some(&authorship_log),
    )
    .ok()
    .and_then(|artifacts| serde_json::to_string(&artifacts.json_hunks).ok());

    let (subject, body) = commit_subject_and_body(repo, &metric_commit.new_sha);
    let mut values = RewriteCommittedValues::new()
        .human_additions(stats.human_additions)
        .git_diff_deleted_lines(stats.git_diff_deleted_lines)
        .git_diff_added_lines(stats.git_diff_added_lines)
        .tool_model_pairs(breakdown.tool_model_pairs)
        .ai_additions(breakdown.ai_additions)
        .ai_accepted(breakdown.ai_accepted)
        .authorship_note(raw_note)
        .operation_kind(metric_commit.operation.as_str())
        .original_commit_shas(metric_commit.original_shas.clone());

    values = match subject {
        Some(subject) => values.commit_subject(subject),
        None => values.commit_subject_null(),
    };
    values = match body {
        Some(body) => values.commit_body(body),
        None => values.commit_body_null(),
    };
    values = if let Some(hunks) = hunks_json {
        values.hunks(hunks)
    } else {
        values.hunks_null()
    };

    let human_author = repo.effective_author_identity().formatted_or_unknown();
    let attrs = apply_rewrite_metric_branch(
        commit_metric_attrs(
            repo,
            &metric_commit.new_sha,
            &base_commit_attr,
            &human_author,
        ),
        metric_commit,
    );

    Ok(Some(MetricEvent::from_values(values, attrs.to_sparse())))
}

fn apply_rewrite_metric_branch(
    attrs: crate::metrics::EventAttributes,
    metric_commit: &RewriteMetricCommit,
) -> crate::metrics::EventAttributes {
    if let Some(branch) = metric_commit.branch.as_deref() {
        return attrs.branch(branch);
    }

    attrs
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authorship::rewrite::RewriteMetricOperation;
    use crate::metrics::EventValues;
    use crate::metrics::events::rewrite_committed_pos;

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
}
