//! In-memory aggregation of local_events for `git-ai activity`.

use crate::error::GitAiError;
use crate::metrics::attrs::attr_pos;
use crate::metrics::db::MetricsDatabase;
use crate::metrics::events::{checkpoint_pos, committed_pos, session_event_pos};
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
    pub tokens: TokenSummary,
    /// Activity bucketed by day/week/month depending on period.
    pub buckets: Vec<BucketStats>,
    /// AI lines committed per hour of day (local time), 24 elements.
    pub hourly: Vec<u32>,
    /// AI lines committed per day of week (local time), 7 elements: Mon=0 … Sun=6.
    pub daily: Vec<u32>,
}

#[derive(Debug, Default, Serialize)]
pub struct TokenSummary {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
    /// Estimated cost in USD, summed across models with known pricing.
    pub estimated_cost_usd: f64,
    /// Per-model breakdown, sorted by total tokens descending.
    pub by_model: Vec<TokenModelStat>,
    /// Week-over-week spend comparison (current 7 days vs previous 7 days).
    /// None when either week has no cost data (e.g. viewing a period < 14 days
    /// or when pricing is unavailable for all models).
    pub wow_spend: Option<WowSpend>,
}

/// Week-over-week spend comparison.
#[derive(Debug, Serialize)]
pub struct WowSpend {
    pub this_week_usd: f64,
    pub last_week_usd: f64,
    /// Percentage change: positive = up, negative = down.
    pub change_pct: f64,
}

#[derive(Debug, Default, Serialize)]
pub struct TokenModelStat {
    pub model: String,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
    /// Estimated cost in USD; None if the model has no pricing entry.
    pub estimated_cost_usd: Option<f64>,
    /// Cache hit ratio: cache_read / (cache_read + cache_creation), 0.0–1.0.
    /// None when neither cache_read nor cache_creation is non-zero (model
    /// doesn't use prompt caching, e.g. codex).
    pub cache_hit_ratio: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct BucketStats {
    pub label: String,
    pub ai_lines: u32,
    pub commit_count: u32,
    /// Total git diff additions in this bucket (across all commits).
    pub diff_added_lines: u32,
    /// Lines attributed to AI or known-human in this bucket.
    pub attributed_lines: u32,
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
    /// Per-tool AI line counts (tool · model label), sorted descending.
    pub by_tool: Vec<(String, u32)>,
    /// Per-tool acceptance rate: committed AI lines / checkpoint AI lines (0–100).
    /// Only includes tools where both sides have data and the rate is ≤ 100%.
    /// Sorted by tool name.
    pub acceptance_by_tool: Vec<(String, u32)>,
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
    pub yield_stats: YieldStats,
}

/// Classifies sessions by whether they were followed by a commit within
/// a short window — a proxy for "did this AI session actually ship work?"
#[derive(Debug, Default, Serialize)]
pub struct YieldStats {
    /// Sessions followed by at least one commit within `YIELD_WINDOW_SECS`.
    pub shipped: u32,
    /// Sessions with no commit found within the window.
    pub abandoned: u32,
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
    // Checkpoint AI lines keyed by plain tool name, for per-tool acceptance rate.
    let mut checkpoint_ai_by_tool: HashMap<String, u32> = HashMap::new();
    // Committed AI lines keyed by plain tool name (extracted from tool::model pairs).
    let mut committed_ai_by_plain_tool: HashMap<String, u32> = HashMap::new();

    let mut session_ids: HashSet<String> = HashSet::new();
    let mut session_tool_counts: HashMap<String, u32> = HashMap::new();

    // Claude-shaped token usage keyed by assistant message id. Value is
    // (model, accum, record_ts). `record_ts` is the Unix timestamp of the
    // first event that introduced this message id — used for WoW bucketing.
    let mut message_usage: HashMap<String, (String, TokenAccum, u32)> = HashMap::new();

    // Codex-shaped token usage keyed by session id. Codex reports cumulative
    // session totals (total_token_usage) on each token_count event, so we keep
    // the per-session max rather than summing.
    let mut codex_sessions: HashMap<String, CodexSessionAccum> = HashMap::new();

    // bucket_key -> accumulated stats
    let mut bucket_map: HashMap<String, BucketAccum> = HashMap::new();
    // bucket_key -> sort key (for ordering)
    let mut bucket_order: HashMap<String, i64> = HashMap::new();

    let mut hourly: Vec<u32> = vec![0u32; 24];
    let mut daily: Vec<u32> = vec![0u32; 7];

    // Yield classification: track the latest timestamp seen per session, and
    // all commit timestamps, then correlate after the loop.
    let mut session_last_ts: HashMap<String, u32> = HashMap::new();
    let mut commit_timestamps: Vec<u32> = Vec::new();

    for record in &records {
        let event: MetricEvent = match serde_json::from_str(&record.event_json) {
            Ok(e) => e,
            Err(_) => continue,
        };

        match record.event_id {
            1 => {
                commit_timestamps.push(record.ts);
                let c = aggregate_committed(
                    &event,
                    &mut total_commits,
                    &mut total_ai_lines,
                    &mut total_human_lines,
                    &mut total_diff_added,
                    &mut commit_tool_counts,
                    &mut committed_ai_by_plain_tool,
                );

                // Bucket every commit that added lines so coverage spans all
                // committed code, not just AI commits.
                if c.diff_added > 0 {
                    let local_dt = ts_to_local(record.ts);
                    if c.ai_lines > 0 {
                        hourly[local_dt.hour() as usize] += c.ai_lines;
                        // Weekday: Mon=0 … Sun=6 (chrono's num_days_from_monday).
                        daily[local_dt.weekday().num_days_from_monday() as usize] += c.ai_lines;
                    }

                    let (key, order_key) = bucket_key(&local_dt, granularity);
                    let entry = bucket_map.entry(key.clone()).or_default();
                    entry.ai_lines += c.ai_lines;
                    // Count AI commits only, to match the AI-lines bar.
                    if c.ai_lines > 0 {
                        entry.commit_count += 1;
                    }
                    entry.diff_added += c.diff_added;
                    entry.attributed += c.ai_lines + c.human_lines;
                    bucket_order.entry(key).or_insert(order_key);
                }
            }
            4 => aggregate_checkpoint(
                &event,
                &mut total_checkpoints,
                &mut ai_lines_added,
                &mut human_lines_added,
                &mut files_edited,
                &mut checkpoint_ai_by_tool,
            ),
            5 => {
                aggregate_session(&event, &mut session_ids, &mut session_tool_counts);

                // Track last-seen timestamp per session for yield classification.
                if let Some(sid) =
                    sparse_get_string(&event.attrs, attr_pos::SESSION_ID).flatten()
                {
                    let entry = session_last_ts.entry(sid).or_insert(0);
                    *entry = (*entry).max(record.ts);
                }
                let tool = sparse_get_string(&event.attrs, attr_pos::TOOL)
                    .flatten()
                    .unwrap_or_default();
                if tool == "codex" {
                    aggregate_codex_tokens(&event, record.ts, &mut codex_sessions);
                } else {
                    aggregate_session_tokens(&event, record.ts, &mut message_usage);
                }
            }
            _ => {}
        }
    }

    // Yield classification: for each unique session, check if a commit landed
    // within 4 hours of the session's last observed event.
    //
    // Limitation: local_events aggregates activity across all repos globally,
    // so a commit in repo-A can incorrectly "claim" a nearby session that was
    // working in repo-B. Fixing this properly requires storing the repo path
    // on both session and committed events (a future schema change).
    const YIELD_WINDOW_SECS: u32 = 4 * 3600;
    commit_timestamps.sort_unstable();
    let mut yield_shipped = 0u32;
    let mut yield_abandoned = 0u32;
    for (_sid, last_ts) in &session_last_ts {
        let window_end = last_ts.saturating_add(YIELD_WINDOW_SECS);
        // Find the first commit at or after this session's last event.
        let pos = commit_timestamps.partition_point(|&t| t < *last_ts);
        if commit_timestamps.get(pos).map_or(false, |&t| t <= window_end) {
            yield_shipped += 1;
        } else {
            yield_abandoned += 1;
        }
    }

    // Per-tool acceptance rate: committed AI lines / checkpoint AI lines.
    let mut acceptance_by_tool: Vec<(String, u32)> = committed_ai_by_plain_tool
        .iter()
        .filter_map(|(tool, &committed)| {
            let checkpoint = *checkpoint_ai_by_tool.get(tool)?;
            let pct = (committed * 100).checked_div(checkpoint)?;
            if pct <= 100 { Some((tool.clone(), pct)) } else { None }
        })
        .collect();
    acceptance_by_tool.sort_by(|(a, _), (b, _)| a.cmp(b));

    let mut commit_by_tool: Vec<(String, u32)> = commit_tool_counts.into_iter().collect();
    commit_by_tool.sort_by_key(|&(_, count)| Reverse(count));

    let mut session_by_tool: Vec<(String, u32)> = session_tool_counts.into_iter().collect();
    session_by_tool.sort_by_key(|&(_, count)| Reverse(count));

    let now_ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as u32;
    let tokens = build_token_summary(message_usage, codex_sessions, now_ts, since_ts);

    // Map by order key for fill_buckets to look up real data.
    let bucket_by_order: HashMap<i64, BucketAccum> = bucket_map
        .into_iter()
        .map(|(label, accum)| (bucket_order[&label], accum))
        .collect();

    // Fill in empty buckets between since_ts and now so the chart has no gaps.
    let filled = fill_buckets(bucket_by_order, since_ts, granularity);

    Ok(LocalActivityStats {
        period_label,
        commits: CommitSummary {
            total: total_commits,
            ai_lines: total_ai_lines,
            human_lines: total_human_lines,
            diff_added_lines: total_diff_added,
            by_tool: commit_by_tool,
            acceptance_by_tool,
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
            yield_stats: YieldStats { shipped: yield_shipped, abandoned: yield_abandoned },
        },
        tokens,
        buckets: filled,
        hourly,
        daily,
    })
}

/// Per-model token accumulator.
#[derive(Debug, Default, Clone)]
struct TokenAccum {
    input: u64,
    output: u64,
    cache_read: u64,
    cache_creation: u64,
}

/// Per-session codex accumulator. Codex reports *cumulative* session totals on
/// each `token_count` event, so we track the max of each raw field. The model
/// name arrives on a separate event (`payload.model`), captured when seen.
#[derive(Debug, Default, Clone)]
struct CodexSessionAccum {
    model: Option<String>,
    /// Unix timestamp of the latest token-usage event seen for this session
    /// (WoW bucketing).
    last_usage_ts: u32,
    /// Cumulative input tokens (includes cached).
    input_tokens: u64,
    /// Cumulative cached input tokens (subset of input_tokens).
    cached_input_tokens: u64,
    /// Cumulative output tokens (includes reasoning).
    output_tokens: u64,
}

/// Per-million-token pricing for a model (USD).
struct ModelPricing {
    input: f64,
    output: f64,
    cache_write: f64,
    cache_read: f64,
}

/// Built-in pricing estimate, matched by substring of the model id.
/// Rates are public Anthropic list prices (USD per million tokens) and are
/// only an estimate — they go stale as pricing changes.
fn pricing_for(model: &str) -> Option<ModelPricing> {
    let m = model.to_lowercase();
    if m.contains("opus") {
        Some(ModelPricing { input: 15.0, output: 75.0, cache_write: 18.75, cache_read: 1.5 })
    } else if m.contains("sonnet") {
        Some(ModelPricing { input: 3.0, output: 15.0, cache_write: 3.75, cache_read: 0.3 })
    } else if m.contains("haiku") {
        Some(ModelPricing { input: 0.8, output: 4.0, cache_write: 1.0, cache_read: 0.08 })
    } else if m.contains("gpt") {
        // OpenAI GPT-5 family estimate; cache_write unused (codex reports no
        // cache-creation tokens).
        Some(ModelPricing { input: 1.25, output: 10.0, cache_write: 1.25, cache_read: 0.125 })
    } else {
        None
    }
}

fn estimate_cost(acc: &TokenAccum, pricing: &ModelPricing) -> f64 {
    (acc.input as f64 * pricing.input
        + acc.output as f64 * pricing.output
        + acc.cache_creation as f64 * pricing.cache_write
        + acc.cache_read as f64 * pricing.cache_read)
        / 1_000_000.0
}

/// Shorten a model id for display: strip a trailing "-YYYYMMDD" date snapshot
/// (e.g. "claude-haiku-4-5-20251001" -> "claude-haiku-4-5").
fn shorten_model(model: &str) -> String {
    match model.rsplit_once('-') {
        Some((head, tail)) if tail.len() == 8 && tail.chars().all(|c| c.is_ascii_digit()) => {
            head.to_string()
        }
        _ => model.to_string(),
    }
}

/// Fold a set of message-usage entries into a per-model cost estimate (USD).
/// Used to compute each WoW half independently.
fn cost_for_message_slice(entries: impl Iterator<Item = (String, TokenAccum)>) -> f64 {
    let mut model_totals: HashMap<String, TokenAccum> = HashMap::new();
    for (model, acc) in entries {
        let e = model_totals.entry(model).or_default();
        e.input += acc.input;
        e.output += acc.output;
        e.cache_read += acc.cache_read;
        e.cache_creation += acc.cache_creation;
    }
    model_totals
        .iter()
        .filter_map(|(model, acc)| pricing_for(model).map(|p| estimate_cost(acc, &p)))
        .sum()
}

fn build_token_summary(
    message_usage: HashMap<String, (String, TokenAccum, u32)>,
    codex_sessions: HashMap<String, CodexSessionAccum>,
    now_ts: u32,
    since_ts: u32,
) -> TokenSummary {
    // Week-over-week split: "this week" = last 7 days, "last week" = 7–14 days ago.
    // Only meaningful when the query window covers at least 14 days; otherwise
    // last-week events were never fetched and last_week_cost would be 0 by
    // omission rather than by fact.
    let this_week_start = now_ts.saturating_sub(7 * 24 * 3600);
    let last_week_start = now_ts.saturating_sub(14 * 24 * 3600);
    let wow_eligible = since_ts <= last_week_start;

    let mut this_week_msgs: Vec<(String, TokenAccum)> = Vec::new();
    let mut last_week_msgs: Vec<(String, TokenAccum)> = Vec::new();

    // Fold per-message (deduped, max) usage into per-model totals.
    let mut model_tokens: HashMap<String, TokenAccum> = HashMap::new();
    for (_id, (model, acc, ts)) in message_usage {
        let entry = model_tokens.entry(model.clone()).or_default();
        entry.input += acc.input;
        entry.output += acc.output;
        entry.cache_read += acc.cache_read;
        entry.cache_creation += acc.cache_creation;

        if ts >= this_week_start {
            this_week_msgs.push((model, acc));
        } else if ts >= last_week_start {
            last_week_msgs.push((model, acc));
        }
    }

    // Fold per-session codex totals into per-model totals, mapping codex's
    // field semantics onto ours: codex input_tokens *includes* cached, so the
    // non-cached input is the difference; cached maps to cache_read; codex has
    // no cache-creation concept.
    let mut this_week_codex: Vec<(String, TokenAccum)> = Vec::new();
    let mut last_week_codex: Vec<(String, TokenAccum)> = Vec::new();

    for (_sid, acc) in codex_sessions {
        let model = acc.model.clone().unwrap_or_else(|| "codex".to_string());
        let mapped = TokenAccum {
            input: acc.input_tokens.saturating_sub(acc.cached_input_tokens),
            output: acc.output_tokens,
            cache_read: acc.cached_input_tokens,
            cache_creation: 0,
        };
        let entry = model_tokens.entry(model.clone()).or_default();
        entry.input += mapped.input;
        entry.output += mapped.output;
        entry.cache_read += mapped.cache_read;

        if acc.last_usage_ts >= this_week_start {
            this_week_codex.push((model, mapped));
        } else if acc.last_usage_ts >= last_week_start {
            last_week_codex.push((model, mapped));
        }
    }

    // Compute WoW spend from the two half-slices.
    let this_week_cost =
        cost_for_message_slice(this_week_msgs.into_iter().chain(this_week_codex));
    let last_week_cost =
        cost_for_message_slice(last_week_msgs.into_iter().chain(last_week_codex));

    let wow_spend = if wow_eligible && (this_week_cost > 0.0 || last_week_cost > 0.0) {
        let change_pct = if last_week_cost > 0.0 {
            (this_week_cost - last_week_cost) / last_week_cost * 100.0
        } else {
            f64::INFINITY
        };
        Some(WowSpend { this_week_usd: this_week_cost, last_week_usd: last_week_cost, change_pct })
    } else {
        None
    };

    let mut summary = TokenSummary::default();
    let mut by_model: Vec<TokenModelStat> = Vec::new();

    for (model, acc) in model_tokens {
        summary.input += acc.input;
        summary.output += acc.output;
        summary.cache_read += acc.cache_read;
        summary.cache_creation += acc.cache_creation;

        let cost = pricing_for(&model).map(|p| estimate_cost(&acc, &p));
        if let Some(c) = cost {
            summary.estimated_cost_usd += c;
        }

        let cache_total = acc.cache_read + acc.cache_creation;
        let cache_hit_ratio = if cache_total > 0 {
            Some(acc.cache_read as f64 / cache_total as f64)
        } else {
            None
        };

        by_model.push(TokenModelStat {
            model: shorten_model(&model),
            input: acc.input,
            output: acc.output,
            cache_read: acc.cache_read,
            cache_creation: acc.cache_creation,
            estimated_cost_usd: cost,
            cache_hit_ratio,
        });
    }

    by_model.sort_by_key(|m| Reverse(m.input + m.output + m.cache_read + m.cache_creation));
    summary.by_model = by_model;
    summary.wow_spend = wow_spend;
    summary
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
    mut data_map: HashMap<i64, BucketAccum>,
    since_ts: u32,
    granularity: BucketGranularity,
) -> Vec<BucketStats> {
    let now = Local::now();
    if since_ts == 0 && data_map.is_empty() {
        return Vec::new();
    }
    let since_date = if since_ts == 0 {
        let earliest_order = data_map.keys().copied().min();
        earliest_order
            .and_then(|order| bucket_start_date(order, granularity))
            .unwrap_or_else(|| now.date_naive())
    } else {
        ts_to_local(since_ts).date_naive()
    };

    let make = |label: String, accum: BucketAccum| BucketStats {
        label,
        ai_lines: accum.ai_lines,
        commit_count: accum.commit_count,
        diff_added_lines: accum.diff_added,
        attributed_lines: accum.attributed,
    };

    // Generate all expected bucket keys between since and now.
    let mut result = Vec::new();
    match granularity {
        BucketGranularity::Daily => {
            let mut day = since_date;
            let today = now.date_naive();
            while day <= today {
                let order = day.num_days_from_ce() as i64;
                let label = day.format("%b %d").to_string();
                result.push(make(label, data_map.remove(&order).unwrap_or_default()));
                day = day.succ_opt().unwrap_or(today);
            }
        }
        BucketGranularity::Weekly => {
            let weekday = since_date.weekday().num_days_from_monday() as i64;
            let mut monday: NaiveDate = since_date - chrono::Duration::days(weekday);
            let today = now.date_naive();
            while monday <= today {
                let order = monday.num_days_from_ce() as i64;
                let sunday = monday + chrono::Duration::days(6);
                let label = format!("{} – {}", monday.format("%b %d"), sunday.format("%b %d"));
                result.push(make(label, data_map.remove(&order).unwrap_or_default()));
                monday = monday
                    .checked_add_signed(chrono::Duration::weeks(1))
                    .unwrap_or(today);
            }
        }
        BucketGranularity::Monthly => {
            let mut year = since_date.year();
            let mut month = since_date.month();
            let now_year = now.year();
            let now_month = now.month();
            loop {
                let order = year as i64 * 12 + (month - 1) as i64;
                let date = NaiveDate::from_ymd_opt(year, month, 1).unwrap();
                let label = date.format("%b %Y").to_string();
                result.push(make(label, data_map.remove(&order).unwrap_or_default()));
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

fn bucket_start_date(order: i64, granularity: BucketGranularity) -> Option<NaiveDate> {
    match granularity {
        BucketGranularity::Daily | BucketGranularity::Weekly => {
            NaiveDate::from_num_days_from_ce_opt(order.try_into().ok()?)
        }
        BucketGranularity::Monthly => {
            let year = order.div_euclid(12);
            let month0 = order.rem_euclid(12);
            NaiveDate::from_ymd_opt(year.try_into().ok()?, (month0 + 1).try_into().ok()?, 1)
        }
    }
}

/// Per-bucket accumulator for the activity-over-time chart.
#[derive(Debug, Default, Clone)]
struct BucketAccum {
    ai_lines: u32,
    commit_count: u32,
    diff_added: u32,
    attributed: u32,
}

/// Per-commit contribution returned by `aggregate_committed` for bucketing.
struct CommitContribution {
    ai_lines: u32,
    human_lines: u32,
    diff_added: u32,
}

fn aggregate_committed(
    event: &MetricEvent,
    total_commits: &mut u32,
    total_ai_lines: &mut u32,
    total_human_lines: &mut u32,
    total_diff_added: &mut u32,
    commit_tool_counts: &mut HashMap<String, u32>,
    committed_ai_by_plain_tool: &mut HashMap<String, u32>,
) -> CommitContribution {
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

    let contribution = CommitContribution {
        ai_lines: total_ai,
        human_lines: human,
        diff_added,
    };

    // Only count the commit and accumulate AI lines when AI was involved.
    if total_ai == 0 {
        return contribution;
    }

    *total_commits += 1;
    *total_ai_lines += total_ai;

    // Per-tool breakdown: index 0 = "all" aggregate, 1+ = per tool::model.
    // Parse pairs once and use them for both the display label map and the
    // plain-tool map used for acceptance rate — no second parse needed.
    let pairs = sparse_get_vec_string(&event.values, committed_pos::TOOL_MODEL_PAIRS)
        .flatten()
        .unwrap_or_default();
    for (i, pair) in pairs.iter().enumerate().skip(1) {
        let ai_for_tool = ai_vecs.get(i).copied().unwrap_or(0);
        if ai_for_tool > 0 {
            *commit_tool_counts.entry(format_tool_model(pair)).or_insert(0) += ai_for_tool;
            let plain_tool = pair.split_once("::").map(|(t, _)| t).unwrap_or(pair);
            *committed_ai_by_plain_tool.entry(plain_tool.to_string()).or_insert(0) += ai_for_tool;
        }
    }

    contribution
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
    checkpoint_ai_by_tool: &mut HashMap<String, u32>,
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
        "ai_agent" | "ai_tab" => {
            *ai_lines_added += lines_added;
            if lines_added > 0 {
                let tool = sparse_get_string(&event.attrs, attr_pos::TOOL)
                    .flatten()
                    .unwrap_or_else(|| "unknown".to_string());
                *checkpoint_ai_by_tool.entry(tool).or_insert(0) += lines_added;
            }
        }
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

/// Extract token usage from a session event's raw transcript JSON (position 0).
/// Only assistant messages carry usage. Keyed by message id, keeping the
/// field-wise max across re-emitted copies (streaming partials report lower
/// counts than the final message). `record_ts` is stored on first insertion
/// for week-over-week bucketing.
fn aggregate_session_tokens(
    event: &MetricEvent,
    record_ts: u32,
    message_usage: &mut HashMap<String, (String, TokenAccum, u32)>,
) {
    let Some(raw) = event.values.get(&session_event_pos::RAW_JSON.to_string()) else {
        return;
    };
    let Some(message) = raw.get("message") else {
        return;
    };
    if message.get("role").and_then(|r| r.as_str()) != Some("assistant") {
        return;
    }
    let Some(usage) = message.get("usage") else {
        return;
    };
    let Some(id) = message.get("id").and_then(|i| i.as_str()) else {
        return;
    };

    let model = message
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("unknown")
        .to_string();

    let get = |key: &str| usage.get(key).and_then(|v| v.as_u64()).unwrap_or(0);

    let (_, acc, _ts) = message_usage
        .entry(id.to_string())
        .or_insert_with(|| (model, TokenAccum::default(), record_ts));
    // Field-wise max: input/cache are fixed per message; output grows while
    // streaming, so the final (largest) value is authoritative.
    acc.input = acc.input.max(get("input_tokens"));
    acc.output = acc.output.max(get("output_tokens"));
    acc.cache_read = acc.cache_read.max(get("cache_read_input_tokens"));
    acc.cache_creation = acc.cache_creation.max(get("cache_creation_input_tokens"));
}

/// Extract token usage from a codex session event. Codex emits `token_count`
/// events carrying cumulative `payload.info.total_token_usage`, and reports its
/// model on a separate event via `payload.model`. Both are keyed by session id;
/// cumulative totals are tracked as a per-session max.
fn aggregate_codex_tokens(
    event: &MetricEvent,
    record_ts: u32,
    codex_sessions: &mut HashMap<String, CodexSessionAccum>,
) {
    let Some(session_id) = sparse_get_string(&event.attrs, attr_pos::SESSION_ID).flatten() else {
        return;
    };
    let Some(raw) = event.values.get(&session_event_pos::RAW_JSON.to_string()) else {
        return;
    };
    let Some(payload) = raw.get("payload") else {
        return;
    };

    let entry = codex_sessions.entry(session_id).or_default();

    // Capture the model name when it appears (not on token_count events).
    if let Some(model) = payload.get("model").and_then(|m| m.as_str())
        && entry.model.is_none()
    {
        entry.model = Some(model.to_string());
    }

    // Cumulative session totals; keep the running max.
    if let Some(usage) = payload.get("info").and_then(|i| i.get("total_token_usage")) {
        let get = |key: &str| usage.get(key).and_then(|v| v.as_u64()).unwrap_or(0);
        entry.last_usage_ts = entry.last_usage_ts.max(record_ts);
        entry.input_tokens = entry.input_tokens.max(get("input_tokens"));
        entry.cached_input_tokens = entry.cached_input_tokens.max(get("cached_input_tokens"));
        entry.output_tokens = entry.output_tokens.max(get("output_tokens"));
    }
}
