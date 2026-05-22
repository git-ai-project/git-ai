//! In-memory aggregation of local_events for `git-ai activity`.

use crate::error::GitAiError;
use crate::metrics::attrs::attr_pos;
use crate::metrics::db::MetricsDatabase;
use crate::metrics::events::{checkpoint_pos, committed_pos};
use crate::metrics::pos_encoded::{
    sparse_get_string, sparse_get_u32, sparse_get_vec_string, sparse_get_vec_u32,
};
use crate::metrics::types::MetricEvent;
use serde::Serialize;
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Serialize)]
pub struct LocalActivityStats {
    pub period_label: String,
    pub commits: CommitSummary,
    pub checkpoints: CheckpointSummary,
    pub sessions: SessionSummary,
}

#[derive(Debug, Serialize)]
pub struct CommitSummary {
    pub total: u32,
    pub ai_lines: u32,
    pub human_lines: u32,
    /// Per-tool AI line counts, sorted descending. Tool name only (strips "::model" suffix).
    pub by_tool: Vec<(String, u32)>,
}

#[derive(Debug, Serialize)]
pub struct CheckpointSummary {
    pub total: u32,
    pub ai_lines_added: u32,
    pub human_lines_added: u32,
    pub files_edited: u32,
}

#[derive(Debug, Serialize)]
pub struct SessionSummary {
    pub total: u32,
    pub by_tool: Vec<(String, u32)>,
}

/// Aggregate local_events since `since_ts` (Unix seconds) into activity stats.
pub fn compute_activity(
    since_ts: u32,
    period_label: String,
) -> Result<LocalActivityStats, GitAiError> {
    let records = {
        let db = MetricsDatabase::global()?;
        let db_lock = db
            .lock()
            .map_err(|_| GitAiError::Generic("metrics DB lock poisoned".to_string()))?;
        db_lock.get_local_events(since_ts)?
    };

    let mut total_commits = 0u32;
    let mut total_ai_lines = 0u32;
    let mut total_human_lines = 0u32;
    let mut commit_tool_counts: HashMap<String, u32> = HashMap::new();

    let mut total_checkpoints = 0u32;
    let mut ai_lines_added = 0u32;
    let mut human_lines_added = 0u32;
    let mut files_edited: HashSet<String> = HashSet::new();

    let mut session_ids: HashSet<String> = HashSet::new();
    let mut session_tool_counts: HashMap<String, u32> = HashMap::new();

    for record in &records {
        let event: MetricEvent = match serde_json::from_str(&record.event_json) {
            Ok(e) => e,
            Err(_) => continue,
        };

        match record.event_id {
            1 => aggregate_committed(
                &event,
                &mut total_commits,
                &mut total_ai_lines,
                &mut total_human_lines,
                &mut commit_tool_counts,
            ),
            4 => aggregate_checkpoint(
                &event,
                &mut total_checkpoints,
                &mut ai_lines_added,
                &mut human_lines_added,
                &mut files_edited,
            ),
            5 => aggregate_session(&event, &mut session_ids, &mut session_tool_counts),
            _ => {}
        }
    }

    let mut commit_by_tool: Vec<(String, u32)> = commit_tool_counts.into_iter().collect();
    commit_by_tool.sort_by_key(|&(_, count)| Reverse(count));

    let mut session_by_tool: Vec<(String, u32)> = session_tool_counts.into_iter().collect();
    session_by_tool.sort_by_key(|&(_, count)| Reverse(count));

    Ok(LocalActivityStats {
        period_label,
        commits: CommitSummary {
            total: total_commits,
            ai_lines: total_ai_lines,
            human_lines: total_human_lines,
            by_tool: commit_by_tool,
        },
        checkpoints: CheckpointSummary {
            total: total_checkpoints,
            ai_lines_added,
            human_lines_added,
            files_edited: files_edited.len() as u32,
        },
        sessions: SessionSummary {
            total: session_ids.len() as u32,
            by_tool: session_by_tool,
        },
    })
}

fn aggregate_committed(
    event: &MetricEvent,
    total_commits: &mut u32,
    total_ai_lines: &mut u32,
    total_human_lines: &mut u32,
    commit_tool_counts: &mut HashMap<String, u32>,
) {
    let ai_vecs = sparse_get_vec_u32(&event.values, committed_pos::AI_ADDITIONS)
        .flatten()
        .unwrap_or_default();
    let total_ai = ai_vecs.first().copied().unwrap_or(0);

    // Only count commits that actually contain AI-attributed lines.
    if total_ai == 0 {
        return;
    }

    *total_commits += 1;
    *total_ai_lines += total_ai;

    let human = sparse_get_u32(&event.values, committed_pos::HUMAN_ADDITIONS)
        .flatten()
        .unwrap_or(0);
    *total_human_lines += human;

    // Per-tool breakdown: index 0 = "all" aggregate, 1+ = per tool::model.
    let pairs = sparse_get_vec_string(&event.values, committed_pos::TOOL_MODEL_PAIRS)
        .flatten()
        .unwrap_or_default();
    for (i, pair) in pairs.iter().enumerate().skip(1) {
        let tool = pair.split("::").next().unwrap_or(pair).to_string();
        let ai_for_tool = ai_vecs.get(i).copied().unwrap_or(0);
        if ai_for_tool > 0 {
            *commit_tool_counts.entry(tool).or_insert(0) += ai_for_tool;
        }
    }
}

fn aggregate_checkpoint(
    event: &MetricEvent,
    total_checkpoints: &mut u32,
    ai_lines_added: &mut u32,
    human_lines_added: &mut u32,
    files_edited: &mut HashSet<String>,
) {
    *total_checkpoints += 1;

    let kind = sparse_get_string(&event.values, checkpoint_pos::KIND)
        .flatten()
        .unwrap_or_default();
    let file_path = sparse_get_string(&event.values, checkpoint_pos::FILE_PATH)
        .flatten()
        .unwrap_or_default();
    let lines_added = sparse_get_u32(&event.values, checkpoint_pos::LINES_ADDED)
        .flatten()
        .unwrap_or(0);

    if !file_path.is_empty() {
        files_edited.insert(file_path);
    }

    match kind.as_str() {
        "ai_agent" | "ai_tab" => *ai_lines_added += lines_added,
        "known_human" => *human_lines_added += lines_added,
        _ => {}
    }
}

fn aggregate_session(
    event: &MetricEvent,
    session_ids: &mut HashSet<String>,
    session_tool_counts: &mut HashMap<String, u32>,
) {
    let session_id = sparse_get_string(&event.attrs, attr_pos::SESSION_ID).flatten();
    let tool = sparse_get_string(&event.attrs, attr_pos::TOOL)
        .flatten()
        .unwrap_or_else(|| "unknown".to_string());

    if let Some(sid) = session_id
        && session_ids.insert(sid)
    {
        *session_tool_counts.entry(tool).or_insert(0) += 1;
    }
}
