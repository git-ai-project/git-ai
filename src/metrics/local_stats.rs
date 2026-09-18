//! In-memory aggregation of persisted metric events for `git-ai usage`.

/// How long after a session's last message a subsequent commit is attributed
/// to that session for yield and ai_lines_committed calculations.
const YIELD_WINDOW_SECS: u32 = 4 * 3600;

use crate::error::GitAiError;
use crate::metrics::attrs::attr_pos;
use crate::metrics::db::{MetricHistoryRecord, MetricsDatabase};
use crate::metrics::events::{checkpoint_pos, committed_pos, token_usage_pos};
use crate::metrics::pos_encoded::{
    sparse_get_string, sparse_get_u32, sparse_get_u64, sparse_get_vec_string, sparse_get_vec_u32,
};
use crate::metrics::types::MetricEvent;
use chrono::{DateTime, Datelike, Local, NaiveDate, TimeZone, Timelike};
use serde::Serialize;
use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

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
    /// AI lines committed per calendar day (local time), sparse — only days with
    /// AI activity are present. Drives the contribution-calendar heatmap.
    pub calendar: Vec<DayActivity>,
    /// First day rendered in the calendar grid (window start, or earliest activity
    /// for the all-time window).
    pub calendar_start: NaiveDate,
    /// Last day rendered in the calendar grid (today, local time).
    pub calendar_end: NaiveDate,
    /// Derived headline stats for the compact summary block.
    pub summary: ActivitySummary,
}

/// AI lines committed on a single local calendar day.
#[derive(Debug, Clone, Serialize)]
pub struct DayActivity {
    pub date: NaiveDate,
    pub ai_lines: u32,
    /// Estimated token spend (USD) on this day, summed across models with known
    /// pricing. Lines and spend diverge — a low-lines day can still be expensive.
    #[serde(default)]
    pub estimated_cost_usd: f64,
}

/// Derived headline statistics for the `git-ai usage` summary block.
#[derive(Debug, Default, Serialize)]
pub struct ActivitySummary {
    /// Distinct local days with AI activity in the window.
    pub active_days: u32,
    /// Days from the first active day through today, inclusive.
    pub total_days: u32,
    /// Longest run of consecutive active days.
    pub longest_streak: u32,
    /// Trailing run of consecutive active days, counted only when it reaches
    /// today or yesterday (one-day grace).
    pub current_streak: u32,
    /// Day with the most AI lines (earliest wins ties).
    pub most_active_day: Option<DayActivity>,
    /// Longest session duration (last event − first event) in seconds.
    pub longest_session_secs: u32,
    /// Top model by total tokens (already shortened). None when no token data.
    pub favorite_model: Option<String>,
}

/// Token/spend totals from the v2 token-usage dataset (TokenUsage events,
/// id 9), whose buckets carry authoritatively priced costs.
#[derive(Debug, Default, Serialize)]
pub struct TokenSummary {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    /// Cache-write tokens (serialized as `cache_creation` for JSON stability).
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
    /// Percentage change: positive = up, negative = down. None when last week
    /// was zero and this week has spend.
    pub change_pct: Option<f64>,
    pub new_this_week: bool,
}

#[derive(Debug, Default, Serialize)]
pub struct TokenModelStat {
    pub model: String,
    pub sessions: u32,
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
    /// Commits that include at least one AI-attributed line. Human-only commits
    /// are not counted here; use the diff/human stats for full commit coverage.
    pub total: u32,
    pub ai_lines: u32,
    pub human_lines: u32,
    /// Total lines added across all commits (git diff additions), used to
    /// measure attribution coverage: lines not attributed to AI or known-human
    /// are "untracked" holes in the data.
    pub diff_added_lines: u32,
    /// Per-tool AI line counts (tool · model label), sorted descending.
    pub by_tool: Vec<(String, u32)>,
    /// Per-tool acceptance rate: committed AI lines / checkpoint AI lines, as a
    /// percentage. Values >100 indicate incomplete checkpoint data (e.g. events
    /// recorded before the repo_url backfill). Sorted by tool name.
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

/// Event types useful for local usage stats.
const USAGE_EVENT_IDS: &[u16] = &[
    1, // Committed
    4, // Checkpoint
    5, // SessionEvent
    9, // TokenUsage
];

/// Acquire the global DB lock and fetch metric history for the given window.
fn fetch_metric_history(
    since_ts: u32,
    repo_filter: Option<&str>,
) -> Result<Vec<MetricHistoryRecord>, GitAiError> {
    let db = MetricsDatabase::global()?;
    let db_lock = db
        .lock()
        .map_err(|_| GitAiError::Generic("metrics DB lock poisoned".to_string()))?;
    db_lock.get_metric_history(since_ts, repo_filter, USAGE_EVENT_IDS)
}

/// Aggregate metric history since `since_ts` (Unix seconds) into activity stats.
///
/// When `repo_filter` is `Some(url)`, only events from that repository are
/// aggregated. When `None`, events from all repositories are included.
pub fn compute_activity(
    since_ts: u32,
    period_label: String,
    granularity: BucketGranularity,
    repo_filter: Option<&str>,
) -> Result<LocalActivityStats, GitAiError> {
    let records = fetch_metric_history(since_ts, repo_filter)?;
    let refs: Vec<&MetricHistoryRecord> = records.iter().collect();
    compute_activity_from_records(&refs, since_ts, period_label, granularity)
}

/// Aggregate a pre-fetched slice of `MetricHistoryRecord`s into activity stats.
///
/// Separated from `compute_activity` so callers that already hold all events
/// (e.g. `compute_repo_summaries`) can avoid re-fetching from the DB per repo.
fn compute_activity_from_records(
    records: &[&MetricHistoryRecord],
    since_ts: u32,
    period_label: String,
    granularity: BucketGranularity,
) -> Result<LocalActivityStats, GitAiError> {
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

    // v2 token-usage buckets (event id 9), keeping only the latest emitted
    // revision per bucket key.
    let mut token_buckets: HashMap<TokenBucketKey, TokenBucket> = HashMap::new();

    // bucket_key -> accumulated stats
    let mut bucket_map: HashMap<String, BucketAccum> = HashMap::new();
    // bucket_key -> sort key (for ordering)
    let mut bucket_order: HashMap<String, i64> = HashMap::new();

    let mut hourly: Vec<u32> = vec![0u32; 24];
    let mut daily: Vec<u32> = vec![0u32; 7];
    // AI lines committed per local calendar day (sorted; sparse — only days with
    // AI activity). Drives the contribution calendar and all derived day stats.
    let mut ai_lines_by_day: BTreeMap<NaiveDate, u32> = BTreeMap::new();

    // Yield classification: track the latest timestamp seen per session, and
    // all commit timestamps, then correlate after the loop.
    let mut session_last_ts: HashMap<String, u32> = HashMap::new();
    // First timestamp seen per session, for longest-session duration.
    let mut session_first_ts: HashMap<String, u32> = HashMap::new();
    let mut commit_timestamps: Vec<u32> = Vec::new();

    for record in records {
        let event = &record.event;

        match record.event_id {
            1 => {
                commit_timestamps.push(record.ts);
                let c = aggregate_committed(
                    event,
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
                        *ai_lines_by_day.entry(local_dt.date_naive()).or_insert(0) += c.ai_lines;
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
                event,
                &mut total_checkpoints,
                &mut ai_lines_added,
                &mut human_lines_added,
                &mut files_edited,
                &mut checkpoint_ai_by_tool,
            ),
            5 => {
                aggregate_session(event, &mut session_ids, &mut session_tool_counts);

                // Track first/last-seen timestamp per session for yield
                // classification and longest-session duration.
                if let Some(sid) = sparse_get_string(&event.attrs, attr_pos::SESSION_ID).flatten() {
                    let last = session_last_ts.entry(sid.clone()).or_insert(0);
                    *last = (*last).max(record.ts);
                    let first = session_first_ts.entry(sid).or_insert(record.ts);
                    *first = (*first).min(record.ts);
                }
            }
            9 => aggregate_token_usage(event, &mut token_buckets),
            _ => {}
        }
    }

    // Yield classification: for each unique session, check if a commit landed
    // within 4 hours of the session's last observed event.
    //
    // Limitation: the all-repos view aggregates activity globally, so a commit
    // in repo-A can incorrectly "claim" a nearby session from repo-B. The
    // per-repo view avoids this by grouping on repo_url before aggregation.

    commit_timestamps.sort_unstable();
    let mut yield_shipped = 0u32;
    let mut yield_abandoned = 0u32;
    for last_ts in session_last_ts.values() {
        let window_end = last_ts.saturating_add(YIELD_WINDOW_SECS);
        // Find the first commit at or after this session's last event.
        let pos = commit_timestamps.partition_point(|&t| t < *last_ts);
        if commit_timestamps.get(pos).is_some_and(|&t| t <= window_end) {
            yield_shipped += 1;
        } else {
            yield_abandoned += 1;
        }
    }

    // Per-tool acceptance rate: committed AI lines / checkpoint AI lines.
    // Values >100 indicate incomplete checkpoint data (e.g. checkpoint events
    // aged out of the window while committed events remain). u32::MAX is the
    // sentinel for "no checkpoint events at all" — same display path as >100.
    let mut acceptance_by_tool: Vec<(String, u32)> = committed_ai_by_plain_tool
        .iter()
        .map(|(tool, &committed)| {
            let pct = match checkpoint_ai_by_tool.get(tool).copied() {
                Some(checkpoint) if checkpoint > 0 => (committed as u64 * 100)
                    .checked_div(checkpoint as u64)
                    .map(|p| p as u32)
                    .unwrap_or(u32::MAX),
                _ => u32::MAX,
            };
            (tool.clone(), pct)
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
    let (tokens, cost_by_day) = build_token_summary(token_buckets, now_ts, since_ts);

    // Map by order key for fill_buckets to look up real data.
    let bucket_by_order: HashMap<i64, BucketAccum> = bucket_map
        .into_iter()
        .map(|(label, accum)| (bucket_order[&label], accum))
        .collect();

    // Fill in empty buckets between since_ts and now so the chart has no gaps.
    let filled = fill_buckets(bucket_by_order, since_ts, granularity);

    // ── Derived calendar + summary stats ──
    let calendar_end = Local::now().date_naive();
    // Earliest day with any activity — AI lines OR token spend — so a spend-only
    // day before the first AI-line day isn't clipped from the all-time window.
    let first_active = ai_lines_by_day
        .keys()
        .next()
        .copied()
        .into_iter()
        .chain(cost_by_day.keys().next().copied())
        .min();
    let calendar_start = if since_ts == 0 {
        first_active.unwrap_or(calendar_end)
    } else {
        ts_to_local(since_ts).date_naive()
    };

    let active_days = ai_lines_by_day.len() as u32;
    // Denominator for "active days X/Y": the length of the selected window in
    // days. For the all-time window (since_ts == 0) there is no fixed length, so
    // fall back to days elapsed since the first active day.
    let total_days = if since_ts == 0 {
        first_active
            .map(|first| ((calendar_end - first).num_days() + 1).max(0) as u32)
            .unwrap_or(0)
    } else {
        (now_ts.saturating_sub(since_ts) / 86_400).max(1)
    };
    let (longest_streak, current_streak) = compute_streaks(&ai_lines_by_day, calendar_end);
    let most_active_day = ai_lines_by_day
        .iter()
        .fold(None::<(NaiveDate, u32)>, |best, (&d, &v)| match best {
            Some((_, bv)) if bv >= v => best,
            _ => Some((d, v)),
        })
        .map(|(date, ai_lines)| DayActivity {
            date,
            ai_lines,
            estimated_cost_usd: cost_by_day.get(&date).copied().unwrap_or(0.0),
        });
    let longest_session_secs = session_last_ts
        .iter()
        .map(|(sid, &last)| last.saturating_sub(*session_first_ts.get(sid).unwrap_or(&last)))
        .max()
        .unwrap_or(0);
    // Union of AI-line days and spend days: a spend-heavy / low-lines day must
    // appear so the second heatmap row can surface it.
    let mut all_days: BTreeSet<NaiveDate> = ai_lines_by_day.keys().copied().collect();
    all_days.extend(cost_by_day.keys().copied());
    let calendar: Vec<DayActivity> = all_days
        .iter()
        .map(|&date| DayActivity {
            date,
            ai_lines: ai_lines_by_day.get(&date).copied().unwrap_or(0),
            estimated_cost_usd: cost_by_day.get(&date).copied().unwrap_or(0.0),
        })
        .collect();

    let summary = ActivitySummary {
        active_days,
        total_days,
        longest_streak,
        current_streak,
        most_active_day,
        longest_session_secs,
        favorite_model: tokens.by_model.first().map(|m| m.model.clone()),
    };

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
            yield_stats: YieldStats {
                shipped: yield_shipped,
                abandoned: yield_abandoned,
            },
        },
        tokens,
        buckets: filled,
        hourly,
        daily,
        calendar,
        calendar_start,
        calendar_end,
        summary,
    })
}

/// Compute (longest, current) consecutive-active-day streaks from a sorted set of
/// active days. A run extends only when consecutive days differ by exactly one
/// calendar day (DST-proof `NaiveDate` arithmetic). The current streak is the
/// trailing run, counted only when it reaches today or yesterday (one-day grace).
fn compute_streaks(days: &BTreeMap<NaiveDate, u32>, today: NaiveDate) -> (u32, u32) {
    let mut longest = 0u32;
    let mut run = 0u32;
    let mut prev: Option<NaiveDate> = None;
    for &d in days.keys() {
        run = match prev {
            Some(p) if (d - p).num_days() == 1 => run + 1,
            _ => 1,
        };
        longest = longest.max(run);
        prev = Some(d);
    }
    let last = match prev {
        Some(p) => p,
        None => return (0, 0),
    };
    let yesterday = today.pred_opt().unwrap_or(today);
    let current = if last == today || last == yesterday {
        run
    } else {
        0
    };
    (longest, current)
}

/// Bucket key for v2 token-usage events: (session_id, model, speed, bucket_ts).
type TokenBucketKey = (String, String, u32, u64);

/// Latest revision of a v2 token-usage bucket (event id 9). Cost was priced
/// authoritatively at emission time (`src/token_usage/cost.rs`), so readers
/// only sum it.
#[derive(Debug, Default, Clone)]
struct TokenBucket {
    emitted_seq: u64,
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
    cost_micro_usd: u64,
}

impl TokenBucket {
    /// A zeroed correction revision (the bucket's entries were all removed).
    /// Cost is checked too: a transcript-priced entry can carry a positive
    /// `costUSD` with zero token counters, and that spend must survive.
    fn is_empty(&self) -> bool {
        self.input == 0
            && self.output == 0
            && self.cache_read == 0
            && self.cache_write == 0
            && self.cost_micro_usd == 0
    }
}

/// Fold a TokenUsage event into the per-bucket map. Buckets are re-emitted
/// with a strictly increasing `emitted_seq` whenever their aggregate changes
/// (including corrections down to zero), so only the highest revision per
/// bucket key is authoritative.
fn aggregate_token_usage(event: &MetricEvent, buckets: &mut HashMap<TokenBucketKey, TokenBucket>) {
    let Some(bucket_ts) = sparse_get_u64(&event.values, token_usage_pos::BUCKET_TS).flatten()
    else {
        return;
    };
    let session_id = sparse_get_string(&event.attrs, attr_pos::SESSION_ID)
        .flatten()
        .unwrap_or_default();
    let model = sparse_get_string(&event.attrs, attr_pos::MODEL)
        .flatten()
        .unwrap_or_else(|| "unknown".to_string());
    let speed = sparse_get_u32(&event.values, token_usage_pos::SPEED)
        .flatten()
        .unwrap_or(0);

    let get = |pos: usize| sparse_get_u64(&event.values, pos).flatten().unwrap_or(0);
    let revision = TokenBucket {
        emitted_seq: get(token_usage_pos::EMITTED_SEQ),
        input: get(token_usage_pos::INPUT_TOKENS),
        output: get(token_usage_pos::OUTPUT_TOKENS),
        cache_read: get(token_usage_pos::CACHE_READ_TOKENS),
        cache_write: get(token_usage_pos::CACHE_WRITE_TOKENS),
        cost_micro_usd: get(token_usage_pos::EST_COST_MICRO_USD),
    };

    let entry = buckets
        .entry((session_id, model, speed, bucket_ts))
        .or_default();
    if revision.emitted_seq >= entry.emitted_seq {
        *entry = revision;
    }
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

/// Returns the aggregate token summary plus a per-local-day spend map (USD),
/// derived from the same bucket data so the daily series reconciles with the
/// headline total.
fn build_token_summary(
    token_buckets: HashMap<TokenBucketKey, TokenBucket>,
    now_ts: u32,
    since_ts: u32,
) -> (TokenSummary, BTreeMap<NaiveDate, f64>) {
    // Per-day spend, bucketed by the usage bucket's local date.
    let mut cost_by_day: BTreeMap<NaiveDate, f64> = BTreeMap::new();
    // Week-over-week split: "this week" = last 7 days, "last week" = 7–14 days ago.
    // Only meaningful when the query window covers at least 14 days; otherwise
    // last-week events were never fetched and last_week_cost would be 0 by
    // omission rather than by fact.
    let this_week_start = now_ts.saturating_sub(7 * 24 * 3600) as u64;
    let last_week_start = now_ts.saturating_sub(14 * 24 * 3600) as u64;
    let wow_eligible = since_ts as u64 <= last_week_start;

    let mut this_week_cost = 0.0f64;
    let mut last_week_cost = 0.0f64;

    // Fold the latest bucket revisions into per-model totals. Key by
    // shorten_model() so date-snapshot variants (e.g. claude-sonnet-4-6-20250101
    // and claude-sonnet-4-6-20250201) are folded into a single display row.
    let mut model_tokens: HashMap<String, TokenBucket> = HashMap::new();
    let mut model_session_ids: HashMap<String, HashSet<String>> = HashMap::new();
    for ((sid, model, _speed, bucket_ts), bucket) in token_buckets {
        // A zeroed latest revision deletes the bucket's contribution.
        if bucket.is_empty() {
            continue;
        }
        // The DB window filters on emission time; a late re-emission can carry
        // a bucket whose usage predates the window. Keep any bucket that
        // overlaps the window, including the partial one straddling its start.
        let bucket_end = bucket_ts + crate::token_usage::types::BUCKET_SECS as u64;
        if since_ts > 0 && bucket_end <= since_ts as u64 {
            continue;
        }

        let cost = bucket.cost_micro_usd as f64 / 1e6;
        if cost > 0.0 {
            let day_ts = bucket_ts.min(u32::MAX as u64) as u32;
            *cost_by_day
                .entry(ts_to_local(day_ts).date_naive())
                .or_insert(0.0) += cost;
        }
        if bucket_ts >= this_week_start {
            this_week_cost += cost;
        } else if bucket_ts >= last_week_start {
            last_week_cost += cost;
        }

        let short = shorten_model(&model);
        let entry = model_tokens.entry(short.clone()).or_default();
        entry.input += bucket.input;
        entry.output += bucket.output;
        entry.cache_read += bucket.cache_read;
        entry.cache_write += bucket.cache_write;
        entry.cost_micro_usd += bucket.cost_micro_usd;

        if !sid.is_empty() {
            model_session_ids.entry(short).or_default().insert(sid);
        }
    }

    let wow_spend = if wow_eligible && (this_week_cost > 0.0 || last_week_cost > 0.0) {
        let (change_pct, new_this_week) = if last_week_cost > 0.0 {
            (
                Some((this_week_cost - last_week_cost) / last_week_cost * 100.0),
                false,
            )
        } else {
            (None, true)
        };
        Some(WowSpend {
            this_week_usd: this_week_cost,
            last_week_usd: last_week_cost,
            change_pct,
            new_this_week,
        })
    } else {
        None
    };

    let mut summary = TokenSummary::default();
    let mut by_model: Vec<TokenModelStat> = Vec::new();

    for (model, acc) in model_tokens {
        summary.input += acc.input;
        summary.output += acc.output;
        summary.cache_read += acc.cache_read;
        summary.cache_creation += acc.cache_write;

        // None preserves the "no pricing" display for models the token-usage
        // pipeline couldn't price (their buckets carry a zero cost).
        let cost = (acc.cost_micro_usd > 0).then_some(acc.cost_micro_usd as f64 / 1e6);
        if let Some(c) = cost {
            summary.estimated_cost_usd += c;
        }

        let cache_total = acc.cache_read + acc.cache_write;
        let cache_hit_ratio = if cache_total > 0 {
            Some(acc.cache_read as f64 / cache_total as f64)
        } else {
            None
        };

        let sessions = model_session_ids
            .get(&model)
            .map(|s| s.len() as u32)
            .unwrap_or(0);
        by_model.push(TokenModelStat {
            model, // already shortened at insertion into model_tokens
            sessions,
            input: acc.input,
            output: acc.output,
            cache_read: acc.cache_read,
            cache_creation: acc.cache_write,
            estimated_cost_usd: cost,
            cache_hit_ratio,
        });
    }

    by_model.sort_by_key(|m| Reverse(m.input + m.output + m.cache_read + m.cache_creation));
    summary.by_model = by_model;
    summary.wow_spend = wow_spend;
    (summary, cost_by_day)
}

fn ts_to_local(ts: u32) -> DateTime<Local> {
    Local
        .timestamp_opt(ts as i64, 0)
        .single()
        .unwrap_or_else(Local::now)
}

/// Produce the display label for a bucket whose "anchor" date (Monday for
/// weekly, 1st for monthly, the day itself for daily) is `date`.
fn bucket_label(date: NaiveDate, granularity: BucketGranularity) -> String {
    match granularity {
        BucketGranularity::Daily => date.format("%b %d").to_string(),
        BucketGranularity::Weekly => {
            let sunday = date + chrono::Duration::days(6);
            format!("{} – {}", date.format("%b %d"), sunday.format("%b %d"))
        }
        BucketGranularity::Monthly => date.format("%b %Y").to_string(),
    }
}

fn bucket_key(dt: &DateTime<Local>, granularity: BucketGranularity) -> (String, i64) {
    match granularity {
        BucketGranularity::Daily => {
            let date = dt.date_naive();
            let order = date.num_days_from_ce() as i64;
            (bucket_label(date, granularity), order)
        }
        BucketGranularity::Weekly => {
            // ISO week: key on Monday of the week.
            let weekday = dt.weekday().num_days_from_monday() as i64;
            let monday = dt.date_naive() - chrono::Duration::days(weekday);
            let order = monday.num_days_from_ce() as i64;
            (bucket_label(monday, granularity), order)
        }
        BucketGranularity::Monthly => {
            let order = dt.year() as i64 * 12 + dt.month0() as i64;
            (bucket_label(dt.date_naive(), granularity), order)
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
                result.push(make(
                    bucket_label(day, granularity),
                    data_map.remove(&order).unwrap_or_default(),
                ));
                day = day.succ_opt().unwrap_or(today);
            }
        }
        BucketGranularity::Weekly => {
            let weekday = since_date.weekday().num_days_from_monday() as i64;
            let mut monday: NaiveDate = since_date - chrono::Duration::days(weekday);
            let today = now.date_naive();
            while monday <= today {
                let order = monday.num_days_from_ce() as i64;
                result.push(make(
                    bucket_label(monday, granularity),
                    data_map.remove(&order).unwrap_or_default(),
                ));
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
                let Some(date) = NaiveDate::from_ymd_opt(year, month, 1) else {
                    break;
                };
                let label = bucket_label(date, granularity);
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

    // Only count the commit toward the AI-commits total when AI was involved.
    // Human-only commits still contribute to human_lines and diff_added above.
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
            *commit_tool_counts
                .entry(format_tool_model(pair))
                .or_insert(0) += ai_for_tool;
            let plain_tool = pair.split_once("::").map(|(t, _)| t).unwrap_or(pair);
            *committed_ai_by_plain_tool
                .entry(plain_tool.to_string())
                .or_insert(0) += ai_for_tool;
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

// ─── Per-repository breakdown ─────────────────────────────────────────────────

/// Summary of activity for a single repository.
#[derive(Debug, Serialize)]
pub struct RepoActivitySummary {
    /// Normalised repository URL (e.g. `github.com/org/repo`).
    pub repo_url: String,
    pub ai_lines: u32,
    pub commits: u32,
    pub sessions: u32,
    pub estimated_cost_usd: f64,
}

/// Aggregate a pre-fetched slice of events into a per-repository breakdown.
fn repo_summaries_from_records(
    all_records: &[MetricHistoryRecord],
    since_ts: u32,
    granularity: BucketGranularity,
) -> Result<Vec<RepoActivitySummary>, GitAiError> {
    // Group records by repo_url, skipping events with no repo (NULL) — these
    // predate repo_url emission and have no meaningful identity to display.
    let mut by_repo: HashMap<&str, Vec<&MetricHistoryRecord>> = HashMap::new();
    for record in all_records {
        if let Some(ref url) = record.repo_url {
            by_repo.entry(url.as_str()).or_default().push(record);
        }
    }

    let mut summaries: Vec<RepoActivitySummary> = by_repo
        .into_iter()
        .filter_map(|(url, records)| {
            let stats =
                compute_activity_from_records(&records, since_ts, String::new(), granularity)
                    .ok()?;
            Some(RepoActivitySummary {
                repo_url: url.to_string(),
                ai_lines: stats.commits.ai_lines,
                commits: stats.commits.total,
                sessions: stats.sessions.total,
                estimated_cost_usd: stats.tokens.estimated_cost_usd,
            })
        })
        .collect();

    summaries.sort_by_key(|s| std::cmp::Reverse(s.ai_lines));
    Ok(summaries)
}

/// Fetch events once and compute overall activity stats and the per-repo
/// breakdown from the same snapshot, ensuring the two views are consistent.
pub fn compute_all(
    since_ts: u32,
    period_label: String,
    granularity: BucketGranularity,
    repo_filter: Option<&str>,
) -> Result<(LocalActivityStats, Vec<RepoActivitySummary>), GitAiError> {
    let records = fetch_metric_history(since_ts, repo_filter)?;
    let refs: Vec<&MetricHistoryRecord> = records.iter().collect();
    let stats = compute_activity_from_records(&refs, since_ts, period_label, granularity)?;
    let repos = repo_summaries_from_records(&records, since_ts, granularity)?;
    Ok((stats, repos))
}

/// Compute a per-repository breakdown for the given time window.
///
/// Fetches all matching events in a single DB query, groups them in memory by
/// `repo_url`, and aggregates each group — O(n) instead of O(n × repos).
/// Sorted by `ai_lines` descending.
pub fn compute_repo_summaries(
    since_ts: u32,
    granularity: BucketGranularity,
    repo_filter: Option<&str>,
) -> Result<Vec<RepoActivitySummary>, GitAiError> {
    let all_records = fetch_metric_history(since_ts, repo_filter)?;
    repo_summaries_from_records(&all_records, since_ts, granularity)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::attrs::EventAttributes;
    use crate::metrics::events::{
        CheckpointValues, CommittedValues, SessionEventValues, TokenUsageValues,
    };
    use crate::metrics::pos_encoded::{PosEncoded, sparse_get_string};
    use serde_json::json;

    fn now_ts() -> u32 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as u32
    }

    fn attrs(
        repo_url: Option<&str>,
        tool: &str,
        session_id: Option<&str>,
    ) -> crate::metrics::types::SparseArray {
        let mut attrs = EventAttributes::with_version("test").tool(tool);
        if let Some(repo_url) = repo_url {
            attrs = attrs.repo_url(repo_url);
        }
        if let Some(session_id) = session_id {
            attrs = attrs.session_id(session_id);
        }
        attrs.to_sparse()
    }

    fn record(event: MetricEvent) -> MetricHistoryRecord {
        let repo_url = sparse_get_string(&event.attrs, attr_pos::REPO_URL).flatten();
        MetricHistoryRecord {
            event_id: event.event_id,
            ts: event.timestamp,
            repo_url,
            event,
        }
    }

    fn committed(
        ts: u32,
        repo_url: &str,
        ai: u32,
        human: u32,
        diff_added: u32,
    ) -> MetricHistoryRecord {
        let values = CommittedValues::new()
            .human_additions(human)
            .git_diff_added_lines(diff_added)
            .tool_model_pairs(vec![
                "all".to_string(),
                "claude::claude-sonnet-4-6".to_string(),
            ])
            .ai_additions(vec![ai, ai]);
        record(MetricEvent::with_timestamp(
            ts,
            &values,
            attrs(Some(repo_url), "claude", None),
        ))
    }

    fn checkpoint(ts: u32, repo_url: &str, lines_added: u32) -> MetricHistoryRecord {
        let values = CheckpointValues::new()
            .kind("ai_agent")
            .file_path("src/main.rs")
            .lines_added(lines_added);
        record(MetricEvent::with_timestamp(
            ts,
            &values,
            attrs(Some(repo_url), "claude", Some("session-1")),
        ))
    }

    fn claude_session(ts: u32, repo_url: Option<&str>, session_id: &str) -> MetricHistoryRecord {
        claude_session_with_model(ts, repo_url, session_id, "claude-sonnet-4-6-20250101")
    }

    fn claude_session_with_model(
        ts: u32,
        repo_url: Option<&str>,
        session_id: &str,
        model: &str,
    ) -> MetricHistoryRecord {
        let values = SessionEventValues::new(json!({
            "message": {
                "id": "msg-1",
                "role": "assistant",
                "model": model,
                "usage": {
                    "input_tokens": 100,
                    "output_tokens": 50,
                    "cache_read_input_tokens": 20,
                    "cache_creation_input_tokens": 10
                }
            }
        }));
        record(MetricEvent::with_timestamp(
            ts,
            &values,
            attrs(repo_url, "claude", Some(session_id)),
        ))
    }

    /// A v2 TokenUsage (event id 9) bucket revision. `ts` is the emission time
    /// (the record timestamp); `bucket_ts` is the 5-minute usage bucket.
    #[allow(clippy::too_many_arguments)]
    fn token_usage(
        ts: u32,
        repo_url: Option<&str>,
        session_id: &str,
        model: &str,
        bucket_ts: u64,
        emitted_seq: u64,
        [input, output, cache_read, cache_write]: [u64; 4],
        cost_micro_usd: u64,
    ) -> MetricHistoryRecord {
        let values = TokenUsageValues::new()
            .bucket_ts(bucket_ts)
            .input_tokens(input)
            .output_tokens(output)
            .cache_read_tokens(cache_read)
            .cache_write_tokens(cache_write)
            .total_tokens(input + output + cache_read + cache_write)
            .est_cost_micro_usd(cost_micro_usd)
            .message_count(1)
            .emitted_seq(emitted_seq)
            .speed(0);
        let mut event_attrs = EventAttributes::with_version("test")
            .tool("claude")
            .model(model)
            .session_id(session_id);
        if let Some(repo_url) = repo_url {
            event_attrs = event_attrs.repo_url(repo_url);
        }
        record(MetricEvent::with_timestamp(
            ts,
            &values,
            event_attrs.to_sparse(),
        ))
    }

    #[test]
    fn compute_activity_aggregates_commits_checkpoints_sessions_and_tokens() {
        let now = now_ts();
        let repo = "github.com/acme/project";
        let session_ts = now.saturating_sub(600);
        let commit_ts = now.saturating_sub(300);
        // The session event still embeds raw usage JSON (100/50/20/10); the
        // token stats must come from the TokenUsage event alone.
        let records = [
            claude_session(session_ts, Some(repo), "session-1"),
            token_usage(
                session_ts + 5,
                Some(repo),
                "session-1",
                "claude-sonnet-4-6-20250101",
                (session_ts as u64 / 300) * 300,
                1,
                [1000, 500, 200, 100],
                2_500_000,
            ),
            checkpoint(session_ts + 10, repo, 12),
            committed(commit_ts, repo, 10, 2, 12),
        ];
        let refs: Vec<&MetricHistoryRecord> = records.iter().collect();

        let stats = compute_activity_from_records(
            &refs,
            now.saturating_sub(24 * 3600),
            "last 1 day".to_string(),
            BucketGranularity::Daily,
        )
        .unwrap();

        assert_eq!(stats.commits.total, 1);
        assert_eq!(stats.commits.ai_lines, 10);
        assert_eq!(stats.commits.human_lines, 2);
        assert_eq!(stats.commits.diff_added_lines, 12);
        assert_eq!(
            stats.commits.by_tool,
            vec![("claude · sonnet-4-6".to_string(), 10)]
        );
        assert_eq!(
            stats.commits.acceptance_by_tool,
            vec![("claude".to_string(), 83)]
        );
        assert_eq!(stats.checkpoints.total, 1);
        assert_eq!(stats.checkpoints.ai_lines_added, 12);
        assert_eq!(stats.checkpoints.files_edited, 1);
        assert_eq!(stats.sessions.total, 1);
        assert_eq!(stats.sessions.yield_stats.shipped, 1);
        assert_eq!(stats.sessions.yield_stats.abandoned, 0);
        assert_eq!(stats.tokens.input, 1000);
        assert_eq!(stats.tokens.output, 500);
        assert_eq!(stats.tokens.cache_read, 200);
        assert_eq!(stats.tokens.cache_creation, 100);
        assert!((stats.tokens.estimated_cost_usd - 2.5).abs() < 1e-12);
        assert_eq!(stats.tokens.by_model[0].model, "claude-sonnet-4-6");
        assert_eq!(stats.tokens.by_model[0].sessions, 1);
        assert!(stats.buckets.iter().any(|bucket| bucket.ai_lines == 10));
    }

    #[test]
    fn token_buckets_keep_only_the_highest_emitted_seq_revision() {
        let now = now_ts();
        let bucket_a = (now.saturating_sub(600) as u64 / 300) * 300;
        let bucket_b = bucket_a - 300;
        let records = [
            // Bucket A: a stale large revision superseded by a corrected one.
            token_usage(
                now - 500,
                None,
                "session-1",
                "claude-sonnet-4-6",
                bucket_a,
                7,
                [9000, 9000, 9000, 9000],
                9_000_000,
            ),
            token_usage(
                now - 400,
                None,
                "session-1",
                "claude-sonnet-4-6",
                bucket_a,
                8,
                [100, 50, 20, 10],
                1_000_000,
            ),
            // Bucket B: latest revision zeroes the bucket out entirely.
            token_usage(
                now - 500,
                None,
                "session-1",
                "claude-sonnet-4-6",
                bucket_b,
                7,
                [500, 500, 0, 0],
                2_000_000,
            ),
            token_usage(
                now - 400,
                None,
                "session-1",
                "claude-sonnet-4-6",
                bucket_b,
                8,
                [0, 0, 0, 0],
                0,
            ),
        ];
        let refs: Vec<&MetricHistoryRecord> = records.iter().collect();

        let stats = compute_activity_from_records(
            &refs,
            now.saturating_sub(24 * 3600),
            "last 1 day".to_string(),
            BucketGranularity::Daily,
        )
        .unwrap();

        assert_eq!(stats.tokens.input, 100);
        assert_eq!(stats.tokens.output, 50);
        assert_eq!(stats.tokens.cache_read, 20);
        assert_eq!(stats.tokens.cache_creation, 10);
        assert!((stats.tokens.estimated_cost_usd - 1.0).abs() < 1e-12);
    }

    #[test]
    fn token_costs_come_from_event_micro_usd() {
        let now = now_ts();
        let bucket_ts = (now.saturating_sub(600) as u64 / 300) * 300;
        // One priced model and one the pipeline couldn't price (cost 0).
        let records = [
            token_usage(
                now - 500,
                None,
                "session-1",
                "claude-fable-5-20260607",
                bucket_ts,
                1,
                [100, 50, 20, 10],
                1_234_567,
            ),
            token_usage(
                now - 400,
                None,
                "session-2",
                "mystery-model",
                bucket_ts,
                1,
                [40, 0, 0, 0],
                0,
            ),
        ];
        let refs: Vec<&MetricHistoryRecord> = records.iter().collect();

        let stats = compute_activity_from_records(
            &refs,
            now.saturating_sub(24 * 3600),
            "last 1 day".to_string(),
            BucketGranularity::Daily,
        )
        .unwrap();

        assert!((stats.tokens.estimated_cost_usd - 1.234_567).abs() < 1e-12);
        // Sorted by total tokens: the priced model first.
        assert_eq!(stats.tokens.by_model[0].model, "claude-fable-5");
        assert_eq!(
            stats.tokens.by_model[0].estimated_cost_usd,
            Some(stats.tokens.estimated_cost_usd)
        );
        // Unpriced usage keeps its tokens but reports no cost.
        assert_eq!(stats.tokens.by_model[1].model, "mystery-model");
        assert_eq!(stats.tokens.by_model[1].input, 40);
        assert_eq!(stats.tokens.by_model[1].estimated_cost_usd, None);
    }

    #[test]
    fn cost_only_token_buckets_keep_their_spend() {
        let now = now_ts();
        let bucket_ts = (now.saturating_sub(600) as u64 / 300) * 300;
        // A transcript-priced entry can carry costUSD with zero token
        // counters; the spend must not be mistaken for a zeroed correction.
        let records = [token_usage(
            now - 500,
            None,
            "session-1",
            "claude-sonnet-4-6",
            bucket_ts,
            1,
            [0, 0, 0, 0],
            750_000,
        )];
        let refs: Vec<&MetricHistoryRecord> = records.iter().collect();

        let stats = compute_activity_from_records(
            &refs,
            now.saturating_sub(24 * 3600),
            "last 1 day".to_string(),
            BucketGranularity::Daily,
        )
        .unwrap();

        assert!((stats.tokens.estimated_cost_usd - 0.75).abs() < 1e-12);
        assert_eq!(stats.tokens.by_model[0].model, "claude-sonnet-4-6");
        assert_eq!(stats.tokens.by_model[0].estimated_cost_usd, Some(0.75));
    }

    #[test]
    fn token_bucket_straddling_the_window_start_is_included() {
        let now = now_ts();
        // since_ts falls strictly inside the bucket's 5-minute span: usage
        // after the boundary must not disappear with the bucket.
        let bucket_ts = (now.saturating_sub(24 * 3600) as u64 / 300) * 300;
        let since_ts = bucket_ts as u32 + 100;
        let records = [token_usage(
            now - 60,
            None,
            "session-1",
            "claude-sonnet-4-6",
            bucket_ts,
            1,
            [100, 50, 0, 0],
            1_000_000,
        )];
        let refs: Vec<&MetricHistoryRecord> = records.iter().collect();

        let stats = compute_activity_from_records(
            &refs,
            since_ts,
            "last 1 day".to_string(),
            BucketGranularity::Daily,
        )
        .unwrap();

        assert_eq!(stats.tokens.input, 100);
        assert_eq!(stats.tokens.output, 50);
        assert!((stats.tokens.estimated_cost_usd - 1.0).abs() < 1e-12);
    }

    #[test]
    fn token_buckets_outside_the_window_are_excluded() {
        let now = now_ts();
        let since_ts = now.saturating_sub(24 * 3600);
        // Emitted just now (inside the fetch window) but the usage itself
        // predates the window: a late re-emission must not leak in.
        let stale_bucket = (since_ts as u64 / 300) * 300 - 3600;
        let records = [token_usage(
            now - 60,
            None,
            "session-1",
            "claude-sonnet-4-6",
            stale_bucket,
            5,
            [100, 50, 0, 0],
            1_000_000,
        )];
        let refs: Vec<&MetricHistoryRecord> = records.iter().collect();

        let stats = compute_activity_from_records(
            &refs,
            since_ts,
            "last 1 day".to_string(),
            BucketGranularity::Daily,
        )
        .unwrap();

        assert_eq!(stats.tokens.input, 0);
        assert_eq!(stats.tokens.output, 0);
        assert_eq!(stats.tokens.estimated_cost_usd, 0.0);
        assert!(stats.tokens.by_model.is_empty());
    }

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    #[test]
    fn streaks_empty_is_zero() {
        let days: BTreeMap<NaiveDate, u32> = BTreeMap::new();
        assert_eq!(compute_streaks(&days, day(2026, 4, 10)), (0, 0));
    }

    #[test]
    fn streaks_longest_breaks_on_gap_and_current_requires_recency() {
        // Three-day run, a gap, then a two-day run ending "today".
        let mut days: BTreeMap<NaiveDate, u32> = BTreeMap::new();
        for d in [1u32, 2, 3, 7, 8] {
            days.insert(day(2026, 4, d), 5);
        }
        let today = day(2026, 4, 8);
        let (longest, current) = compute_streaks(&days, today);
        assert_eq!(longest, 3);
        assert_eq!(current, 2);
    }

    #[test]
    fn streaks_current_zero_when_trailing_run_is_stale() {
        let mut days: BTreeMap<NaiveDate, u32> = BTreeMap::new();
        for d in [1u32, 2, 3] {
            days.insert(day(2026, 4, d), 5);
        }
        // Today is well past the last active day → current streak is 0.
        let (longest, current) = compute_streaks(&days, day(2026, 4, 20));
        assert_eq!(longest, 3);
        assert_eq!(current, 0);
    }

    #[test]
    fn streaks_current_counts_with_yesterday_grace() {
        let mut days: BTreeMap<NaiveDate, u32> = BTreeMap::new();
        for d in [4u32, 5, 6] {
            days.insert(day(2026, 4, d), 5);
        }
        // Last active day is yesterday relative to today → still counts.
        let (longest, current) = compute_streaks(&days, day(2026, 4, 7));
        assert_eq!(longest, 3);
        assert_eq!(current, 3);
    }

    #[test]
    fn derived_summary_from_records() {
        let now = Local
            .from_local_datetime(
                &Local::now()
                    .date_naive()
                    .and_hms_opt(12, 0, 0)
                    .expect("local noon should exist"),
            )
            .single()
            .expect("local noon should be unambiguous")
            .timestamp() as u32;
        let repo = "github.com/acme/project";
        // Two sessions: one spanning ~1h, one a single event.
        let session_start = now.saturating_sub(7200);
        let records = [
            claude_session(session_start, Some(repo), "session-1"),
            claude_session(session_start + 3600, Some(repo), "session-1"),
            claude_session(now.saturating_sub(60), Some(repo), "session-2"),
            token_usage(
                now.saturating_sub(60),
                Some(repo),
                "session-1",
                "claude-sonnet-4-6-20250101",
                (now.saturating_sub(600) as u64 / 300) * 300,
                1,
                [100, 50, 20, 10],
                1_000_000,
            ),
            committed(now.saturating_sub(300), repo, 40, 0, 40),
            committed(now.saturating_sub(120), repo, 10, 0, 10),
        ];
        let refs: Vec<&MetricHistoryRecord> = records.iter().collect();

        let stats = compute_activity_from_records(
            &refs,
            now.saturating_sub(24 * 3600),
            "last 1 day".to_string(),
            BucketGranularity::Daily,
        )
        .unwrap();

        // Both commits land on the same local day → one active day, 50 AI lines.
        assert_eq!(stats.summary.active_days, 1);
        assert_eq!(stats.calendar.len(), 1);
        assert_eq!(stats.calendar[0].ai_lines, 50);
        assert_eq!(stats.summary.total_days, 1);
        assert_eq!(stats.summary.longest_streak, 1);
        assert_eq!(stats.summary.current_streak, 1);
        let most = stats.summary.most_active_day.as_ref().unwrap();
        assert_eq!(most.ai_lines, 50);
        // Longest session spans the two session-1 events (~3600s); session-2 is 0.
        assert_eq!(stats.summary.longest_session_secs, 3600);
        assert_eq!(
            stats.summary.favorite_model.as_deref(),
            Some("claude-sonnet-4-6")
        );
    }

    #[test]
    fn repo_summaries_group_records_by_repo_and_skip_unknown_repo() {
        let now = now_ts();
        let repo = "github.com/acme/project";
        let records = [
            committed(now.saturating_sub(300), repo, 8, 0, 8),
            claude_session(now.saturating_sub(200), Some(repo), "session-1"),
            token_usage(
                now.saturating_sub(150),
                Some(repo),
                "session-1",
                "claude-sonnet-4-6",
                (now.saturating_sub(300) as u64 / 300) * 300,
                1,
                [100, 50, 0, 0],
                1_000_000,
            ),
            claude_session(now.saturating_sub(100), None, "session-unknown"),
        ];

        let summaries = repo_summaries_from_records(
            &records,
            now.saturating_sub(24 * 3600),
            BucketGranularity::Daily,
        )
        .unwrap();

        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].repo_url, repo);
        assert_eq!(summaries[0].ai_lines, 8);
        assert_eq!(summaries[0].commits, 1);
        assert_eq!(summaries[0].sessions, 1);
        assert!(summaries[0].estimated_cost_usd > 0.0);
    }
}
