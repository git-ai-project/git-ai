use crate::authorship::authorship_log::{LineRange, SessionRecord};
use crate::authorship::authorship_log_serialization::{
    AuthorshipLog, generate_session_id, generate_trace_id,
};
use crate::authorship::working_log::CheckpointKind;
use crate::commands::checkpoint_agent::bash_tool::StatEntry;
use crate::daemon::bash_history_db::{BashCheckpointCall, distance_to_call_window};
use crate::error::GitAiError;
use crate::git::repo_state::worktree_root_for_path;
use crate::git::repository::Repository;
use crate::metrics::{CheckpointValues, EventAttributes, MetricEvent, PosEncoded};
use serde_json::json;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const BASH_RECOVERY_WINDOW_NS: u128 = 3_000_000_000;
const EDGE_EXTENSION_MAX_LINES: usize = 3;

pub(crate) fn recover_attribution(
    repo: &Repository,
    parent_sha: &str,
    commit_sha: &str,
    human_author: &str,
    authorship_log: &mut AuthorshipLog,
    committed_hunks: &HashMap<String, Vec<LineRange>>,
) -> Result<(), GitAiError> {
    if committed_hunks.is_empty() {
        return Ok(());
    }

    if unknown_lines_by_file(authorship_log, committed_hunks).is_empty() {
        return Ok(());
    }

    recover_bash_mtime(
        repo,
        parent_sha,
        commit_sha,
        human_author,
        authorship_log,
        committed_hunks,
    )?;
    recover_adjacent_edges(
        repo,
        parent_sha,
        commit_sha,
        authorship_log,
        committed_hunks,
    );
    Ok(())
}

fn recover_bash_mtime(
    repo: &Repository,
    parent_sha: &str,
    commit_sha: &str,
    human_author: &str,
    authorship_log: &mut AuthorshipLog,
    committed_hunks: &HashMap<String, Vec<LineRange>>,
) -> Result<(), GitAiError> {
    let repo_work_dir = repo_worktree_key(repo)?;
    let workdir = repo.workdir()?;
    let unknown_by_file = unknown_lines_by_file(authorship_log, committed_hunks);
    if unknown_by_file.is_empty() {
        return Ok(());
    }

    let mut timestamps_by_file = HashMap::new();
    let mut all_timestamps = Vec::new();
    for file_path in unknown_by_file.keys() {
        let timestamps = file_timestamps_ns(&workdir, file_path);
        if !timestamps.is_empty() {
            all_timestamps.extend(timestamps.iter().copied());
            timestamps_by_file.insert(file_path.clone(), timestamps);
        }
    }
    if all_timestamps.is_empty() {
        return Ok(());
    }
    all_timestamps.sort_unstable();
    all_timestamps.dedup();

    let candidates = match crate::daemon::bash_history_db::BashHistoryDatabase::global() {
        Ok(db) => match db.lock() {
            Ok(db) => db.candidates_near_timestamps(&all_timestamps, BASH_RECOVERY_WINDOW_NS)?,
            Err(_) => Vec::new(),
        },
        Err(_) => Vec::new(),
    };
    if candidates.is_empty() {
        return Ok(());
    }
    let commit_session_ids = session_ids_in_commit(authorship_log);

    for (file_path, unknown_lines) in unknown_by_file {
        let Some(timestamps) = timestamps_by_file.get(&file_path) else {
            continue;
        };
        let target_path = workdir.join(&file_path);
        let Some(selection) =
            select_best_bash_candidate(&candidates, timestamps, &commit_session_ids, &target_path)
        else {
            continue;
        };
        if selection.distance_ns > BASH_RECOVERY_WINDOW_NS {
            continue;
        }
        let candidate = selection.candidate;

        let trace_id = generate_trace_id();
        let session_id = generate_session_id(&candidate.agent_id.id, &candidate.agent_id.tool);
        let author_id = format!("{}::{}", session_id, trace_id);
        insert_session_record(authorship_log, &session_id, candidate, human_author);
        add_attestation(authorship_log, &file_path, &author_id, &unknown_lines);

        let metadata = json!({
            "solver": "bash_mtime",
            "file_path": file_path,
            "unknown_lines": unknown_lines,
            "target_repo_work_dir": repo_work_dir.as_str(),
            "file_timestamps_ns": timestamps,
            "selected_bash_call_id": candidate.id,
            "selected_bash_repo_work_dir": candidate.repo_work_dir.as_str(),
            "selected_tool_use_id": candidate.tool_use_id,
            "selected_command": candidate.command,
            "distance_ns": selection.distance_ns,
            "ranking_session_already_in_commit": selection.session_already_in_commit,
            "ranking_repo_workdir_is_parent": selection.repo_workdir_is_parent,
            "window_ns": BASH_RECOVERY_WINDOW_NS,
            "start_time_ns": candidate.start_time_ns,
            "end_time_ns": candidate.end_time_ns,
            "start_trace_id": candidate.start_trace_id,
            "end_trace_id": candidate.end_trace_id,
        });
        record_recovery_metric(RecoveryMetricInput {
            repo,
            parent_sha,
            commit_sha,
            file_path: &file_path,
            author_id: &author_id,
            session_id: &session_id,
            trace_id: &trace_id,
            tool: &candidate.agent_id.tool,
            model: &candidate.agent_id.model,
            external_session_id: &candidate.agent_id.id,
            external_tool_use_id: Some(&candidate.tool_use_id),
            edit_kind: "bash",
            checkpoint_type: "recovered_bash",
            recovered_line_count: unknown_lines.len() as u32,
            metadata,
            event_ts: Some((candidate.start_time_ns / 1_000_000_000) as u32),
        });
    }

    Ok(())
}

fn recover_adjacent_edges(
    repo: &Repository,
    parent_sha: &str,
    commit_sha: &str,
    authorship_log: &mut AuthorshipLog,
    committed_hunks: &HashMap<String, Vec<LineRange>>,
) {
    let unknown = unknown_lines_by_file(authorship_log, committed_hunks);
    for (file_path, unknown_lines) in unknown {
        let line_to_author = line_author_map(authorship_log, &file_path);
        let runs = contiguous_runs(&unknown_lines);
        for run in runs {
            let Some(recovery) = edge_recovery_for_run(&line_to_author, &run) else {
                continue;
            };
            let trace_id = generate_trace_id();
            let source_session = recovery
                .source_author
                .split("::")
                .next()
                .unwrap_or(&recovery.source_author)
                .to_string();
            let recovered_author = if source_session.starts_with("s_") {
                format!("{}::{}", source_session, trace_id)
            } else {
                recovery.source_author.clone()
            };
            let recovered_line_count = recovery.lines.len() as u32;
            add_attestation(
                authorship_log,
                &file_path,
                &recovered_author,
                &recovery.lines,
            );

            let metadata = json!({
                "solver": "edge_extension",
                "file_path": file_path,
                "source_author": &recovery.source_author,
                "recovered_lines": &recovery.lines,
            });
            record_recovery_metric(RecoveryMetricInput {
                repo,
                parent_sha,
                commit_sha,
                file_path: &file_path,
                author_id: &recovered_author,
                session_id: &source_session,
                trace_id: &trace_id,
                tool: "",
                model: "",
                external_session_id: "",
                external_tool_use_id: None,
                edit_kind: "attribution_recovery_edge",
                checkpoint_type: "recovered_edge_extension",
                recovered_line_count,
                metadata,
                event_ts: None,
            });
        }
    }
}

fn repo_worktree_key(repo: &Repository) -> Result<String, GitAiError> {
    let workdir = repo.workdir()?;
    let normalized = worktree_root_for_path(&workdir).unwrap_or(workdir);
    Ok(normalized
        .canonicalize()
        .unwrap_or(normalized)
        .to_string_lossy()
        .to_string())
}

fn file_timestamps_ns(workdir: &std::path::Path, file_path: &str) -> Vec<u128> {
    let Ok(meta) = fs::symlink_metadata(workdir.join(file_path)) else {
        return Vec::new();
    };
    let stat = StatEntry::from_metadata(&meta);
    let mut timestamps = Vec::new();
    if let Some(mtime) = stat.mtime {
        timestamps.push(system_time_to_ns(mtime));
    }
    if let Some(ctime) = stat.ctime {
        timestamps.push(system_time_to_ns(ctime));
    }
    timestamps.sort_unstable();
    timestamps.dedup();
    timestamps
}

fn system_time_to_ns(time: SystemTime) -> u128 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

#[derive(Debug, Clone, Copy)]
struct BashCandidateSelection<'a> {
    candidate: &'a BashCheckpointCall,
    distance_ns: u128,
    session_already_in_commit: bool,
    repo_workdir_is_parent: bool,
}

fn select_best_bash_candidate<'a>(
    candidates: &'a [BashCheckpointCall],
    timestamps: &[u128],
    commit_session_ids: &HashSet<String>,
    target_path: &Path,
) -> Option<BashCandidateSelection<'a>> {
    candidates
        .iter()
        .filter_map(|candidate| {
            let distance = timestamps
                .iter()
                .map(|ts| distance_to_call_window(*ts, candidate))
                .min()
                .unwrap_or(u128::MAX);
            (distance <= BASH_RECOVERY_WINDOW_NS).then(|| {
                let session_id =
                    generate_session_id(&candidate.agent_id.id, &candidate.agent_id.tool);
                BashCandidateSelection {
                    candidate,
                    distance_ns: distance,
                    session_already_in_commit: commit_session_ids.contains(&session_id),
                    repo_workdir_is_parent: candidate_workdir_contains_path(candidate, target_path),
                }
            })
        })
        .min_by(compare_bash_candidate_selection)
}

fn compare_bash_candidate_selection(
    left: &BashCandidateSelection<'_>,
    right: &BashCandidateSelection<'_>,
) -> std::cmp::Ordering {
    left.session_already_in_commit
        .cmp(&right.session_already_in_commit)
        .reverse()
        .then_with(|| {
            if left.session_already_in_commit && right.session_already_in_commit {
                std::cmp::Ordering::Equal
            } else {
                left.repo_workdir_is_parent
                    .cmp(&right.repo_workdir_is_parent)
                    .reverse()
            }
        })
        .then_with(|| left.distance_ns.cmp(&right.distance_ns))
        .then_with(|| {
            right
                .candidate
                .end_time_ns
                .is_some()
                .cmp(&left.candidate.end_time_ns.is_some())
        })
        .then_with(|| {
            right
                .candidate
                .command
                .is_some()
                .cmp(&left.candidate.command.is_some())
        })
        .then_with(|| right.candidate.id.cmp(&left.candidate.id))
}

fn session_ids_in_commit(authorship_log: &AuthorshipLog) -> HashSet<String> {
    authorship_log
        .attestations
        .iter()
        .flat_map(|file| file.entries.iter())
        .filter(|entry| entry.hash.starts_with("s_"))
        .map(|entry| {
            entry
                .hash
                .split("::")
                .next()
                .unwrap_or(&entry.hash)
                .to_string()
        })
        .collect()
}

fn candidate_workdir_contains_path(candidate: &BashCheckpointCall, target_path: &Path) -> bool {
    let candidate_workdir = PathBuf::from(&candidate.repo_work_dir);
    let candidate_workdir = candidate_workdir
        .canonicalize()
        .unwrap_or(candidate_workdir);
    let target_path = target_path
        .canonicalize()
        .unwrap_or_else(|_| target_path.to_path_buf());
    target_path.starts_with(candidate_workdir)
}

fn insert_session_record(
    authorship_log: &mut AuthorshipLog,
    session_id: &str,
    candidate: &BashCheckpointCall,
    human_author: &str,
) {
    authorship_log
        .metadata
        .sessions
        .entry(session_id.to_string())
        .or_insert_with(|| SessionRecord {
            agent_id: candidate.agent_id.clone(),
            human_author: Some(human_author.to_string()),
            custom_attributes: None,
        });
}

fn unknown_lines_by_file(
    authorship_log: &AuthorshipLog,
    committed_hunks: &HashMap<String, Vec<LineRange>>,
) -> BTreeMap<String, Vec<u32>> {
    let covered = covered_lines_by_file(authorship_log);
    let mut result = BTreeMap::new();
    for (file_path, ranges) in committed_hunks {
        let covered_lines = covered.get(file_path);
        let mut unknown = Vec::new();
        for line in ranges.iter().flat_map(LineRange::expand) {
            if !covered_lines.is_some_and(|lines| lines.contains(&line)) {
                unknown.push(line);
            }
        }
        unknown.sort_unstable();
        unknown.dedup();
        if !unknown.is_empty() {
            result.insert(file_path.clone(), unknown);
        }
    }
    result
}

fn covered_lines_by_file(authorship_log: &AuthorshipLog) -> HashMap<String, HashSet<u32>> {
    let mut covered = HashMap::new();
    for file_attestation in &authorship_log.attestations {
        let lines = covered
            .entry(file_attestation.file_path.clone())
            .or_insert_with(HashSet::new);
        for entry in &file_attestation.entries {
            for line in entry.line_ranges.iter().flat_map(LineRange::expand) {
                lines.insert(line);
            }
        }
    }
    covered
}

fn line_author_map(authorship_log: &AuthorshipLog, file_path: &str) -> BTreeMap<u32, String> {
    let mut map = BTreeMap::new();
    let Some(file_attestation) = authorship_log
        .attestations
        .iter()
        .find(|att| att.file_path == file_path)
    else {
        return map;
    };
    for entry in &file_attestation.entries {
        for line in entry.line_ranges.iter().flat_map(LineRange::expand) {
            map.insert(line, entry.hash.clone());
        }
    }
    map
}

fn contiguous_runs(lines: &[u32]) -> Vec<Vec<u32>> {
    if lines.is_empty() {
        return Vec::new();
    }
    let mut sorted = lines.to_vec();
    sorted.sort_unstable();
    sorted.dedup();

    let mut runs: Vec<Vec<u32>> = Vec::new();
    let mut current = vec![sorted[0]];
    for line in sorted.into_iter().skip(1) {
        if line == current.last().copied().unwrap_or(line) + 1 {
            current.push(line);
        } else {
            runs.push(current);
            current = vec![line];
        }
    }
    runs.push(current);
    runs
}

struct EdgeRecovery {
    source_author: String,
    lines: Vec<u32>,
}

fn edge_recovery_for_run(
    line_to_author: &BTreeMap<u32, String>,
    run: &[u32],
) -> Option<EdgeRecovery> {
    let first = *run.first()?;
    let last = *run.last()?;
    let prev = first
        .checked_sub(1)
        .and_then(|line| line_to_author.get(&line));
    let next = line_to_author.get(&(last + 1));

    match (prev, next) {
        (Some(left), Some(right))
            if is_ai_attestation(left)
                && is_ai_attestation(right)
                && ai_session_key(left) == ai_session_key(right) =>
        {
            let mut lines = run
                .iter()
                .take(EDGE_EXTENSION_MAX_LINES)
                .copied()
                .collect::<Vec<_>>();
            lines.extend(run.iter().rev().take(EDGE_EXTENSION_MAX_LINES).copied());
            lines.sort_unstable();
            lines.dedup();
            Some(EdgeRecovery {
                source_author: left.clone(),
                lines,
            })
        }
        (Some(left), None) if is_ai_attestation(left) => Some(EdgeRecovery {
            source_author: left.clone(),
            lines: run.iter().take(EDGE_EXTENSION_MAX_LINES).copied().collect(),
        }),
        (None, Some(right)) if is_ai_attestation(right) => Some(EdgeRecovery {
            source_author: right.clone(),
            lines: run
                .iter()
                .rev()
                .take(EDGE_EXTENSION_MAX_LINES)
                .copied()
                .collect(),
        }),
        _ => None,
    }
}

#[cfg(test)]
fn edge_recovered_lines(line_to_author: &BTreeMap<u32, String>, run: &[u32]) -> Option<Vec<u32>> {
    edge_recovery_for_run(line_to_author, run).map(|mut recovery| {
        recovery.lines.sort_unstable();
        recovery.lines
    })
}

fn is_ai_attestation(author: &str) -> bool {
    author != CheckpointKind::Human.to_str() && !author.starts_with("h_")
}

fn ai_session_key(author: &str) -> &str {
    author.split("::").next().unwrap_or(author)
}

fn add_attestation(
    authorship_log: &mut AuthorshipLog,
    file_path: &str,
    author_id: &str,
    lines: &[u32],
) {
    let mut sorted = lines.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    if sorted.is_empty() {
        return;
    }
    let ranges = LineRange::compress_lines(&sorted);
    let entry = crate::authorship::authorship_log_serialization::AttestationEntry::new(
        author_id.to_string(),
        ranges,
    );
    authorship_log
        .get_or_create_file(file_path)
        .add_entry(entry);
}

struct RecoveryMetricInput<'a> {
    repo: &'a Repository,
    parent_sha: &'a str,
    commit_sha: &'a str,
    file_path: &'a str,
    author_id: &'a str,
    session_id: &'a str,
    trace_id: &'a str,
    tool: &'a str,
    model: &'a str,
    external_session_id: &'a str,
    external_tool_use_id: Option<&'a str>,
    edit_kind: &'a str,
    checkpoint_type: &'a str,
    recovered_line_count: u32,
    metadata: serde_json::Value,
    event_ts: Option<u32>,
}

fn record_recovery_metric(input: RecoveryMetricInput<'_>) {
    if input.tool == "mock_ai" {
        return;
    }

    let checkpoint_ts = input.event_ts.map(u64::from).unwrap_or_else(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    });
    let mut values = CheckpointValues::new()
        .checkpoint_ts(checkpoint_ts)
        .kind(CheckpointKind::AiAgent.to_str())
        .file_path(input.file_path)
        .lines_added(input.recovered_line_count)
        .lines_deleted(0)
        .lines_added_sloc(input.recovered_line_count)
        .lines_deleted_sloc(0)
        .edit_kind(input.edit_kind)
        .checkpoint_type(input.checkpoint_type)
        .attribution_recovery_metadata(input.metadata.to_string());
    if let Some(tool_use_id) = input.external_tool_use_id {
        values = values.external_tool_use_id(tool_use_id);
    }

    let mut attrs = EventAttributes::with_version(env!("CARGO_PKG_VERSION"))
        .base_commit_sha(input.parent_sha)
        .commit_sha(input.commit_sha)
        .session_id(input.session_id)
        .trace_id(input.trace_id);

    if !input.tool.is_empty() {
        attrs = attrs.tool(input.tool);
    }
    if !input.model.is_empty() {
        attrs = attrs.model(input.model);
    }
    if !input.external_session_id.is_empty() {
        attrs = attrs.external_session_id(input.external_session_id);
    }
    if let Some(url) = crate::repo_url::resolve_repo_url_from_repo(input.repo) {
        attrs = attrs.repo_url(url);
    }
    if let Ok(head_ref) = input.repo.head()
        && let Ok(short_branch) = head_ref.shorthand()
    {
        attrs = attrs.branch(short_branch);
    }
    attrs = attrs.author(input.author_id);

    let event = MetricEvent::from_values_with_timestamp(values, attrs.to_sparse(), input.event_ts);
    crate::observability::log_metrics(vec![event]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authorship::authorship_log_serialization::{
        AttestationEntry, AuthorshipLog, FileAttestation,
    };
    use crate::authorship::working_log::AgentId;

    fn test_candidate(
        id: i64,
        external_session_id: &str,
        repo_work_dir: &str,
        start_time_ns: u128,
        end_time_ns: u128,
    ) -> BashCheckpointCall {
        BashCheckpointCall {
            id,
            invocation_key: format!("{external_session_id}/tool"),
            repo_work_dir: repo_work_dir.to_string(),
            session_id: external_session_id.to_string(),
            tool_use_id: "tool".to_string(),
            agent_id: AgentId {
                tool: "codex".to_string(),
                id: external_session_id.to_string(),
                model: "gpt-5".to_string(),
            },
            start_trace_id: Some(format!("t_start_{id}")),
            end_trace_id: Some(format!("t_end_{id}")),
            start_time_ns,
            end_time_ns: Some(end_time_ns),
            command: Some("cat > target.txt".to_string()),
            metadata: HashMap::new(),
        }
    }

    #[test]
    fn unknown_lines_exclude_existing_attestations() {
        let mut log = AuthorshipLog::new();
        log.attestations.push(FileAttestation {
            file_path: "a.txt".to_string(),
            entries: vec![AttestationEntry::new(
                "s_abc::t_def".to_string(),
                vec![LineRange::Single(2)],
            )],
        });
        let committed = HashMap::from([(
            "a.txt".to_string(),
            vec![LineRange::Range(1, 3), LineRange::Single(5)],
        )]);

        let unknown = unknown_lines_by_file(&log, &committed);
        assert_eq!(unknown.get("a.txt").unwrap(), &vec![1, 3, 5]);
    }

    #[test]
    fn edge_recovery_extends_one_sided_runs_and_bridges_matching_ai_neighbors() {
        let map = BTreeMap::from([
            (1, "s_a::t_1".to_string()),
            (3, "s_a::t_2".to_string()),
            (10, "s_a::t_3".to_string()),
            (20, "s_b::t_1".to_string()),
        ]);

        assert_eq!(
            edge_recovered_lines(&map, &[2]).as_deref(),
            Some(&[2][..]),
            "different trace ids for the same session should extend by session"
        );
        assert_eq!(
            edge_recovered_lines(&map, &[4, 5, 6, 7, 8, 9]).as_deref(),
            Some(&[4, 5, 6, 7, 8, 9][..]),
            "matching AI neighbors should recover up to three lines from each side"
        );
        assert_eq!(
            edge_recovered_lines(&map, &[11, 12, 13, 14]).as_deref(),
            Some(&[11, 12, 13][..]),
            "trailing edge extension should recover at most three lines"
        );
        assert_eq!(
            edge_recovered_lines(&map, &[16, 17, 18, 19]).as_deref(),
            Some(&[17, 18, 19][..]),
            "leading edge extension should recover the three lines nearest the AI block"
        );
    }

    #[test]
    fn edge_recovery_keeps_human_and_different_session_guardrails() {
        let map = BTreeMap::from([
            (1, "s_a::t_1".to_string()),
            (3, "s_b::t_1".to_string()),
            (5, "h_human::t_1".to_string()),
        ]);

        assert_eq!(
            edge_recovered_lines(&map, &[2]).as_deref(),
            None,
            "different sessions must not be bridged"
        );
        assert_eq!(
            edge_recovered_lines(&map, &[4]).as_deref(),
            None,
            "known-human neighbors must not be used for edge extension"
        );
    }

    #[test]
    fn bash_candidate_ranking_prefers_session_already_in_commit_then_time() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target.txt");
        let in_commit_far =
            test_candidate(1, "in-commit-far", "/outside", 1_000_000_000, 1_100_000_000);
        let in_commit_near = test_candidate(
            2,
            "in-commit-near",
            "/outside",
            2_000_000_000,
            2_100_000_000,
        );
        let unrelated_closest =
            test_candidate(3, "unrelated", "/outside", 2_400_000_000, 2_500_000_000);
        let candidates = vec![in_commit_far, in_commit_near, unrelated_closest];
        let commit_sessions = HashSet::from([
            generate_session_id("in-commit-far", "codex"),
            generate_session_id("in-commit-near", "codex"),
        ]);

        let selected =
            select_best_bash_candidate(&candidates, &[2_550_000_000], &commit_sessions, &target)
                .unwrap();

        assert_eq!(selected.candidate.agent_id.id, "in-commit-near");
        assert!(selected.session_already_in_commit);
    }

    #[test]
    fn bash_candidate_ranking_prefers_parent_workdir_when_no_session_matches_commit() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("repo").join("target.txt");
        let parent = test_candidate(
            1,
            "parent",
            temp.path().to_str().unwrap(),
            1_000_000_000,
            1_100_000_000,
        );
        let closer_non_parent =
            test_candidate(2, "closer", "/outside", 2_000_000_000, 2_100_000_000);
        let candidates = vec![parent, closer_non_parent];

        let selected =
            select_best_bash_candidate(&candidates, &[2_150_000_000], &HashSet::new(), &target)
                .unwrap();

        assert_eq!(selected.candidate.agent_id.id, "parent");
        assert!(selected.repo_workdir_is_parent);
    }

    #[test]
    fn bash_candidate_ranking_falls_back_to_closest_time() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target.txt");
        let older = test_candidate(1, "older", "/outside-a", 1_000_000_000, 1_100_000_000);
        let closer = test_candidate(2, "closer", "/outside-b", 2_000_000_000, 2_100_000_000);
        let candidates = vec![older, closer];

        let selected =
            select_best_bash_candidate(&candidates, &[2_200_000_000], &HashSet::new(), &target)
                .unwrap();

        assert_eq!(selected.candidate.agent_id.id, "closer");
        assert!(!selected.session_already_in_commit);
        assert!(!selected.repo_workdir_is_parent);
    }
}
