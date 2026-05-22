//! In-memory aggregation of local_events for `git-ai activity`.

use crate::error::GitAiError;
use crate::metrics::attrs::attr_pos;
use crate::metrics::db::MetricsDatabase;
use crate::metrics::events::{checkpoint_pos, committed_pos};
use crate::metrics::pos_encoded::{
    sparse_get_string, sparse_get_u32, sparse_get_vec_string, sparse_get_vec_u32,
};
use crate::metrics::types::MetricEvent;
use chrono::{DateTime, Datelike, Local, NaiveDate, TimeZone, Timelike};
use serde::Serialize;
use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Serialize)]
pub struct LocalActivityStats {
    pub period_label: String,
    pub commits: CommitSummary,
    pub checkpoints: CheckpointSummary,
    pub sessions: SessionSummary,
    /// Activity bucketed by day/week/month depending on period.
    pub buckets: Vec<BucketStats>,
    /// AI lines committed per hour of day (local time), 24 elements.
    pub hourly: Vec<u32>,
}

#[derive(Debug, Serialize)]
pub struct BucketStats {
    pub label: String,
    pub ai_lines: u32,
    pub commit_count: u32,
}

#[derive(Debug, Serialize)]
pub struct CommitSummary {
    pub total: u32,
    pub ai_lines: u32,
    pub human_lines: u32,
    /// Total lines added across all commits (git diff additions), used to
    /// measure attribution coverage: lines not attributed to AI or known-human
    /// are "untracked" holes in the data.
    pub diff_added_lines: u32,
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

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BucketGranularity {
    Daily,
    Weekly,
    Monthly,
}

/// Aggregate local_events since `since_ts` (Unix seconds) into activity stats.
pub fn compute_activity(
    since_ts: u32,
    period_label: String,
    granularity: BucketGranularity,
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
    let mut total_diff_added = 0u32;
    let mut commit_tool_counts: HashMap<String, u32> = HashMap::new();

    let mut total_checkpoints = 0u32;
    let mut ai_lines_added = 0u32;
    let mut human_lines_added = 0u32;
    let mut files_edited: HashSet<String> = HashSet::new();

    let mut session_ids: HashSet<String> = HashSet::new();
    let mut session_tool_counts: HashMap<String, u32> = HashMap::new();

    // bucket_key -> (ai_lines, commit_count)
    let mut bucket_map: HashMap<String, (u32, u32)> = HashMap::new();
    // bucket_key -> sort key (for ordering)
    let mut bucket_order: HashMap<String, i64> = HashMap::new();

    let mut hourly: Vec<u32> = vec![0u32; 24];

    for record in &records {
        let event: MetricEvent = match serde_json::from_str(&record.event_json) {
            Ok(e) => e,
            Err(_) => continue,
        };

        match record.event_id {
            1 => {
                let ai_lines_this = aggregate_committed(
                    &event,
                    &mut total_commits,
                    &mut total_ai_lines,
                    &mut total_human_lines,
                    &mut total_diff_added,
                    &mut commit_tool_counts,
                );

                if ai_lines_this > 0 {
                    let local_dt = ts_to_local(record.ts);
                    let hour = local_dt.hour() as usize;
                    hourly[hour] += ai_lines_this;

                    let (key, order_key) = bucket_key(&local_dt, granularity);
                    let entry = bucket_map.entry(key.clone()).or_insert((0, 0));
                    entry.0 += ai_lines_this;
                    entry.1 += 1;
                    bucket_order.entry(key).or_insert(order_key);
                }
            }
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

    // Sort buckets chronologically using their order key.
    let mut bucket_pairs: Vec<(String, i64, u32, u32)> = bucket_map
        .into_iter()
        .map(|(label, (ai, commits))| {
            let order = bucket_order[&label];
            (label, order, ai, commits)
        })
        .collect();
    bucket_pairs.sort_by_key(|&(_, order, _, _)| order);

    // Fill in empty buckets between since_ts and now so the chart has no gaps.
    let filled = fill_buckets(bucket_pairs, since_ts, granularity);

    Ok(LocalActivityStats {
        period_label,
        commits: CommitSummary {
            total: total_commits,
            ai_lines: total_ai_lines,
            human_lines: total_human_lines,
            diff_added_lines: total_diff_added,
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
        buckets: filled,
        hourly,
    })
}

fn ts_to_local(ts: u32) -> DateTime<Local> {
    Local
        .timestamp_opt(ts as i64, 0)
        .single()
        .unwrap_or_else(Local::now)
}

fn bucket_key(dt: &DateTime<Local>, granularity: BucketGranularity) -> (String, i64) {
    match granularity {
        BucketGranularity::Daily => {
            let label = dt.format("%b %d").to_string();
            let order = dt.date_naive().num_days_from_ce() as i64;
            (label, order)
        }
        BucketGranularity::Weekly => {
            // ISO week: key on Monday of the week.
            let weekday = dt.weekday().num_days_from_monday() as i64;
            let monday = dt.date_naive() - chrono::Duration::days(weekday);
            let sunday = monday + chrono::Duration::days(6);
            let label = format!("{} – {}", monday.format("%b %d"), sunday.format("%b %d"));
            let order = monday.num_days_from_ce() as i64;
            (label, order)
        }
        BucketGranularity::Monthly => {
            let label = dt.format("%b %Y").to_string();
            let order = dt.year() as i64 * 12 + dt.month0() as i64;
            (label, order)
        }
    }
}

/// Fill gaps between `since_ts` and today so charts have contiguous buckets.
fn fill_buckets(
    data: Vec<(String, i64, u32, u32)>,
    since_ts: u32,
    granularity: BucketGranularity,
) -> Vec<BucketStats> {
    // Build a map from order_key → (label, ai, commits) from real data.
    let mut data_map: HashMap<i64, (String, u32, u32)> = data
        .into_iter()
        .map(|(label, order, ai, commits)| (order, (label, ai, commits)))
        .collect();

    let now = Local::now();
    let since_dt = ts_to_local(since_ts);

    // Generate all expected bucket keys between since and now.
    let mut result = Vec::new();
    match granularity {
        BucketGranularity::Daily => {
            let mut day = since_dt.date_naive();
            let today = now.date_naive();
            while day <= today {
                let order = day.num_days_from_ce() as i64;
                let label = day.format("%b %d").to_string();
                let (ai, commits) = data_map
                    .remove(&order)
                    .map(|(_, ai, c)| (ai, c))
                    .unwrap_or((0, 0));
                result.push(BucketStats { label, ai_lines: ai, commit_count: commits });
                day = day.succ_opt().unwrap_or(today);
            }
        }
        BucketGranularity::Weekly => {
            let weekday = since_dt.weekday().num_days_from_monday() as i64;
            let mut monday: NaiveDate =
                since_dt.date_naive() - chrono::Duration::days(weekday);
            let today = now.date_naive();
            while monday <= today {
                let order = monday.num_days_from_ce() as i64;
                let sunday = monday + chrono::Duration::days(6);
                let label = format!("{} – {}", monday.format("%b %d"), sunday.format("%b %d"));
                let (ai, commits) = data_map
                    .remove(&order)
                    .map(|(_, ai, c)| (ai, c))
                    .unwrap_or((0, 0));
                result.push(BucketStats { label, ai_lines: ai, commit_count: commits });
                monday = monday
                    .checked_add_signed(chrono::Duration::weeks(1))
                    .unwrap_or(today);
            }
        }
        BucketGranularity::Monthly => {
            let mut year = since_dt.year();
            let mut month = since_dt.month();
            let now_year = now.year();
            let now_month = now.month();
            loop {
                let order = year as i64 * 12 + (month - 1) as i64;
                let date = NaiveDate::from_ymd_opt(year, month, 1).unwrap();
                let label = date.format("%b %Y").to_string();
                let (ai, commits) = data_map
                    .remove(&order)
                    .map(|(_, ai, c)| (ai, c))
                    .unwrap_or((0, 0));
                result.push(BucketStats { label, ai_lines: ai, commit_count: commits });
                if year == now_year && month == now_month {
                    break;
                }
                month += 1;
                if month > 12 {
                    month = 1;
                    year += 1;
                }
            }
        }
    }

    result
}

/// Returns the AI lines for this commit (0 if none).
fn aggregate_committed(
    event: &MetricEvent,
    total_commits: &mut u32,
    total_ai_lines: &mut u32,
    total_human_lines: &mut u32,
    total_diff_added: &mut u32,
    commit_tool_counts: &mut HashMap<String, u32>,
) -> u32 {
    let human = sparse_get_u32(&event.values, committed_pos::HUMAN_ADDITIONS)
        .flatten()
        .unwrap_or(0);
    let diff_added = sparse_get_u32(&event.values, committed_pos::GIT_DIFF_ADDED_LINES)
        .flatten()
        .unwrap_or(0);
    let ai_vecs = sparse_get_vec_u32(&event.values, committed_pos::AI_ADDITIONS)
        .flatten()
        .unwrap_or_default();
    let total_ai = ai_vecs.first().copied().unwrap_or(0);

    // Always accumulate human lines and total diff additions regardless of
    // whether the commit has AI lines (coverage spans all committed code).
    *total_human_lines += human;
    *total_diff_added += diff_added;

    // Only count the commit and accumulate AI lines when AI was involved.
    if total_ai == 0 {
        return 0;
    }

    *total_commits += 1;
    *total_ai_lines += total_ai;

    // Per-tool breakdown: index 0 = "all" aggregate, 1+ = per tool::model.
    let pairs = sparse_get_vec_string(&event.values, committed_pos::TOOL_MODEL_PAIRS)
        .flatten()
        .unwrap_or_default();
    for (i, pair) in pairs.iter().enumerate().skip(1) {
        let label = format_tool_model(pair);
        let ai_for_tool = ai_vecs.get(i).copied().unwrap_or(0);
        if ai_for_tool > 0 {
            *commit_tool_counts.entry(label).or_insert(0) += ai_for_tool;
        }
    }

    total_ai
}

/// Format a "tool::model" pair into a readable "tool · model" label,
/// trimming a redundant tool prefix from the model (e.g. "claude::claude-sonnet-4-6"
/// becomes "claude · sonnet-4-6").
fn format_tool_model(pair: &str) -> String {
    match pair.split_once("::") {
        Some((tool, model)) if !model.is_empty() => {
            let prefix = format!("{tool}-");
            let model = model.strip_prefix(&prefix).unwrap_or(model);
            format!("{tool} · {model}")
        }
        _ => pair.to_string(),
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
