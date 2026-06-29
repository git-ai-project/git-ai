//! `git-ai analyze sessions …` — pull coding sessions from Cube into a scratch
//! SQLite DB, then hand them out one at a time for grading at scale.
//!
//! Two cursors live in the DB, both in the `cursor` table: a **pull cursor**
//! (`fetched`/`target`/`pull_complete` — how far we've paged through Cube) and
//! an **analysis cursor** (`analyzed_seq` — the highest `sessions.seq_id` handed
//! out). `sessions next` advances the analysis cursor by exactly one row inside
//! an `IMMEDIATE` transaction, so concurrent subagents each get a distinct
//! session exactly once with no dependency on anyone marking it complete; it
//! then fetches and persists that session's transcript into `session_events`
//! and returns it inline. `reset` rewinds the analysis cursor to re-run.

use super::cube::{CubeClient, QueryArgs};
use super::take_value;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const SESSIONS_CUBE: &str = "public_v1_sessions";
const EVENTS_CUBE: &str = "public_v1_normalized_events";
const TOKEN_USAGE_CUBE: &str = "public_v1_token_usage";
const SESSION_MODELS_CUBE: &str = "public_v1_session_models";
const PR_SESSIONS_CUBE: &str = "public_v1_pr_sessions";

const DEFAULT_PULL_LIMIT: u64 = 100;
const PULL_BATCH: u64 = 50;
const DEFAULT_MAX_EVENTS: u64 = 2000;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS sessions (
    seq_id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL UNIQUE,
    user_id TEXT,
    agent TEXT,
    repo_url TEXT,
    parent_session_id TEXT,
    child_session_count INTEGER,
    session_start_time INTEGER,
    session_end_time INTEGER,
    generated_lines INTEGER,
    deleted_lines INTEGER,
    net_generated_lines INTEGER,
    generated_sloc INTEGER,
    -- Funnel stages, in order: every line a session committed, then how many of
    -- those reached a PR, got merged, and finally landed in production. The
    -- stage-to-stage "gap" columns (committed_not_pr_opened, …) are derived from
    -- these by `ensure_derived_columns` (see DERIVED_COLUMNS) so an analyst never
    -- has to hand-compute "committed but didn't ship".
    committed_lines INTEGER,
    pr_opened_lines INTEGER,
    merged_lines INTEGER,
    production_lines INTEGER,
    total_checkpoints INTEGER,
    total_events INTEGER,
    usage_minutes INTEGER,
    -- Cross-cube baseline, backfilled at pull time (best-effort) so every
    -- session carries the same comparable columns without re-querying:
    --   models      : comma-joined distinct models used (see session_models)
    --   *_tokens    : per-session token usage (public_v1_token_usage)
    --   cost_usd    : per-session estimated cost (public_v1_token_usage)
    --   pr_count    : how many PRs this session appears in (see session_prs)
    models TEXT,
    input_tokens INTEGER,
    output_tokens INTEGER,
    cache_read_tokens INTEGER,
    cache_creation_tokens INTEGER,
    reasoning_tokens INTEGER,
    cost_usd REAL,
    pr_count INTEGER,
    created_at INTEGER NOT NULL
);

-- One row per (session, model). Backfilled from public_v1_session_models; the
-- distinct models are also denormalized into sessions.models for quick filters.
CREATE TABLE IF NOT EXISTS session_models (
    session_id TEXT NOT NULL REFERENCES sessions(session_id),
    model TEXT NOT NULL,
    event_count INTEGER,
    PRIMARY KEY (session_id, model)
);
CREATE INDEX IF NOT EXISTS idx_session_models_sid ON session_models(session_id);

-- One row per (session, PR) the session contributed to. Backfilled from
-- public_v1_pr_sessions; the count is denormalized into sessions.pr_count.
CREATE TABLE IF NOT EXISTS session_prs (
    session_id TEXT NOT NULL REFERENCES sessions(session_id),
    repo_url TEXT NOT NULL DEFAULT '',
    pr_number INTEGER NOT NULL,
    agent TEXT,
    model_raw TEXT,
    ai_lines INTEGER,
    PRIMARY KEY (session_id, repo_url, pr_number)
);
CREATE INDEX IF NOT EXISTS idx_session_prs_sid ON session_prs(session_id);

CREATE TABLE IF NOT EXISTS session_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL,
    output_seq INTEGER,
    event_time INTEGER,
    event_kind TEXT,
    tool TEXT,
    tool_kind TEXT,
    model TEXT,
    target TEXT,
    text TEXT,
    summary TEXT,
    tool_input TEXT,
    tool_output TEXT,
    UNIQUE(session_id, output_seq, event_time, event_kind, tool)
);
CREATE INDEX IF NOT EXISTS idx_session_events_sid ON session_events(session_id);

-- Two cursors live here:
--   fetched/target/pull_complete : ingestion progress paging through Cube (pull)
--   analyzed_seq                 : the analysis cursor — the highest sessions.seq_id
--                                  handed out by `next`. `next` advances it atomically
--                                  so every row is served exactly once; `reset` sets it
--                                  back to 0 to start a fresh analysis pass.
CREATE TABLE IF NOT EXISTS cursor (
    name TEXT PRIMARY KEY DEFAULT 'default',
    fetched INTEGER NOT NULL DEFAULT 0,
    target INTEGER,
    pull_complete INTEGER NOT NULL DEFAULT 0,
    analyzed_seq INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS meta (
    key TEXT PRIMARY KEY,
    value TEXT
);
"#;

/// Session columns selected for output, in display order.
const SESSION_COLUMNS: &[&str] = &[
    "seq_id",
    "session_id",
    "user_id",
    "agent",
    "repo_url",
    "parent_session_id",
    "child_session_count",
    "session_start_time",
    "session_end_time",
    "generated_lines",
    "deleted_lines",
    "net_generated_lines",
    "generated_sloc",
    "committed_lines",
    "pr_opened_lines",
    "merged_lines",
    "production_lines",
    // Derived funnel gaps (generated columns — see DERIVED_COLUMNS).
    "committed_not_pr_opened",
    "pr_opened_not_merged",
    "merged_not_production",
    "committed_not_production",
    "production_rate",
    "total_checkpoints",
    "total_events",
    "usage_minutes",
    "models",
    "input_tokens",
    "output_tokens",
    "cache_read_tokens",
    "cache_creation_tokens",
    "reasoning_tokens",
    "cost_usd",
    "pr_count",
];

pub fn handle_sessions(args: &[String]) {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("");
    let rest = if args.is_empty() { &[][..] } else { &args[1..] };
    // `--help`/`-h` anywhere prints help, exactly like running `sessions` raw.
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_help();
        return;
    }
    let result = match sub {
        "pull" => cmd_pull(rest),
        "next" => cmd_next(rest),
        "stats" => cmd_stats(rest),
        "reset" => cmd_reset(rest),
        "exec" => cmd_exec(rest),
        "help" | "--help" | "-h" | "" => {
            print_help();
            return;
        }
        other => Err(format!(
            "unknown sessions subcommand: {}\nRun `git-ai analyze sessions --help`.",
            other
        )),
    };
    if let Err(msg) = result {
        eprintln!("Error: {}", msg);
        std::process::exit(1);
    }
}

// ---------------------------------------------------------------------------
// pull
// ---------------------------------------------------------------------------

fn cmd_pull(args: &[String]) -> Result<(), String> {
    let mut db_path: Option<String> = None;
    let mut limit = DEFAULT_PULL_LIMIT;
    let mut since: Option<String> = None;
    let mut repo: Option<String> = None;
    let mut user: Option<String> = None;
    let mut agent: Option<String> = None;
    // Default to top-level sessions only: a session with a parent is a subagent.
    let mut include_subagents = false;
    // Backfill the cross-cube baseline columns (tokens/cost/models/PRs) by
    // default; --no-enrich skips the extra per-batch queries.
    let mut enrich = true;
    // Query-shaping flags, identical to `analyze query`: these layer extra
    // dimensions/measures/filters/order on top of the canonical session columns
    // so you can slice the session population on ANY Cube member.
    let mut extra_measures: Vec<String> = Vec::new();
    let mut extra_dimensions: Vec<String> = Vec::new();
    let mut time_dimension: Option<String> = None;
    let mut granularity: Option<String> = None;
    let mut filters_json: Option<String> = None;
    let mut order: Vec<(String, String)> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--db" => db_path = Some(take_value(args, &mut i, "--db")?),
            "--limit" => {
                limit = take_value(args, &mut i, "--limit")?
                    .parse()
                    .map_err(|_| "--limit must be a number".to_string())?
            }
            "--since" | "--date-range" => since = Some(take_value(args, &mut i, "--since")?),
            "--repo" => repo = Some(take_value(args, &mut i, "--repo")?),
            "--user" => user = Some(take_value(args, &mut i, "--user")?),
            "--agent" => agent = Some(take_value(args, &mut i, "--agent")?),
            "--include-subagents" => include_subagents = true,
            "--no-enrich" => enrich = false,
            "--measures" | "-m" => {
                extra_measures.extend(super::split_csv(&take_value(args, &mut i, "--measures")?));
            }
            "--dimensions" | "-d" => {
                extra_dimensions.extend(super::split_csv(&take_value(
                    args,
                    &mut i,
                    "--dimensions",
                )?));
            }
            "--time-dimension" | "--td" => {
                time_dimension = Some(take_value(args, &mut i, "--time-dimension")?);
            }
            "--granularity" | "-g" => {
                granularity = Some(take_value(args, &mut i, "--granularity")?);
            }
            "--filters" | "-f" => {
                filters_json = Some(take_value(args, &mut i, "--filters")?);
            }
            "--order" | "-o" => {
                order.push(super::parse_order(&take_value(args, &mut i, "--order")?)?);
            }
            // `--help`/`-h` is handled by `handle_sessions` before dispatch.
            other => return Err(format!("unknown pull flag: {}", other)),
        }
        i += 1;
    }

    let path = match db_path {
        Some(p) => PathBuf::from(p),
        None => random_db_path(),
    };
    let client = CubeClient::from_config().map_err(|e| e.to_string())?;
    let conn = open_db(&path).map_err(|e| e.to_string())?;
    init_cursor(&conn, limit).map_err(|e| e.to_string())?;

    // Record provenance.
    set_meta(&conn, "created_at", &now_secs().to_string()).ok();
    set_meta(
        &conn,
        "pull_filters",
        &json!({
            "since": since, "repo": repo, "user": user, "agent": agent,
            "limit": limit, "include_subagents": include_subagents,
            "filters": filters_json, "dimensions": extra_dimensions,
            "measures": extra_measures, "time_dimension": time_dimension,
            "granularity": granularity,
            "order": order.iter().map(|(m, d)| format!("{}:{}", m, d)).collect::<Vec<_>>(),
        })
        .to_string(),
    )
    .ok();

    // The canonical session columns are ALWAYS selected so the SQLite schema
    // stays fully populated; the caller's --dimensions/--measures layer on top.
    let mut dimensions = session_dimensions();
    for d in extra_dimensions {
        if !dimensions.contains(&d) {
            dimensions.push(d);
        }
    }
    let mut measures = session_measures();
    for m in extra_measures {
        if !measures.contains(&m) {
            measures.push(m);
        }
    }

    // Convenience equals-filters (subagent/repo/user/agent) merge with any raw
    // --filters array so both work together and you can slice on ANY member. A
    // non-empty parent_session_id means a subagent session; '' (not NULL) is a
    // top-level user session, so we exclude subagents by default.
    let parent_filter = if include_subagents { None } else { Some("") };
    let convenience = equals_filters(&[
        (
            format!("{}.parent_session_id", SESSIONS_CUBE),
            parent_filter,
        ),
        (format!("{}.repo_url", SESSIONS_CUBE), repo.as_deref()),
        (format!("{}.user_id", SESSIONS_CUBE), user.as_deref()),
        (format!("{}.agent", SESSIONS_CUBE), agent.as_deref()),
    ]);
    let filters = merge_filters(convenience, filters_json.as_deref())?;

    // A --since dateRange needs a time dimension; default to session_start_time
    // unless the caller named their own. Ordering defaults to newest-first but
    // an explicit --order wins.
    let time_dimension = time_dimension.or_else(|| {
        since
            .as_ref()
            .map(|_| format!("{}.session_start_time", SESSIONS_CUBE))
    });
    let order = if order.is_empty() {
        vec![(
            format!("{}.session_start_time", SESSIONS_CUBE),
            "desc".into(),
        )]
    } else {
        order
    };

    let mut fetched: u64 = get_fetched(&conn).map_err(|e| e.to_string())?;
    let mut stored_now = 0usize;

    while fetched < limit {
        let batch = PULL_BATCH.min(limit - fetched);
        let args = QueryArgs {
            measures: measures.clone(),
            dimensions: dimensions.clone(),
            time_dimension: time_dimension.clone(),
            granularity: granularity.clone(),
            date_range: since.clone(),
            filters_json: filters.clone(),
            order: order.clone(),
            limit: Some(batch),
            offset: Some(fetched),
        };
        let query = args.to_query()?;
        let rows = client.load_rows(&query).map_err(|e| e.to_string())?;
        let returned = rows.len() as u64;
        stored_now += insert_sessions(&conn, &rows).map_err(|e| e.to_string())?;
        // Backfill the cross-cube baseline columns for this batch's sessions.
        if enrich {
            let batch_ids: Vec<String> = rows
                .iter()
                .filter_map(|r| member_str(r, SESSIONS_CUBE, "session_id"))
                .collect();
            enrich_sessions(&client, &conn, &batch_ids);
        }
        fetched += returned;
        set_fetched(&conn, fetched).map_err(|e| e.to_string())?;
        // Exhausted: cube returned fewer rows than asked for.
        if returned < batch {
            set_pull_complete(&conn).map_err(|e| e.to_string())?;
            break;
        }
    }
    if fetched >= limit {
        set_pull_complete(&conn).map_err(|e| e.to_string())?;
    }

    let total: i64 = conn
        .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;
    eprintln!(
        "Pulled {} new session(s); {} total in db.",
        stored_now, total
    );
    // The DB path goes to stdout so callers can capture it (`DB=$(… pull)`).
    println!("{}", path.display());
    Ok(())
}

// ---------------------------------------------------------------------------
// next
// ---------------------------------------------------------------------------

fn cmd_next(args: &[String]) -> Result<(), String> {
    let mut db_path: Option<String> = None;
    let mut max_events = DEFAULT_MAX_EVENTS;
    let mut with_transcript = true;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--max-events" => {
                max_events = take_value(args, &mut i, "--max-events")?
                    .parse()
                    .map_err(|_| "--max-events must be a number".to_string())?
            }
            "--no-transcript" => with_transcript = false,
            // `--help`/`-h` is handled by `handle_sessions` before dispatch.
            other if !other.starts_with('-') && db_path.is_none() => {
                db_path = Some(other.to_string())
            }
            other => return Err(format!("unknown next argument: {}", other)),
        }
        i += 1;
    }
    let path = db_path.ok_or("usage: git-ai analyze sessions next <db> [--max-events N]")?;
    let mut conn = open_existing_db(&path)?;

    // Hand out the next session by advancing the analysis cursor exactly one
    // row. `BEGIN IMMEDIATE` takes the write lock before we read the cursor, so
    // concurrent `next` callers are serialized — every row is returned exactly
    // once and only once, with no dependency on the caller ever coming back.
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;
    let pos: i64 = tx
        .query_row(
            "SELECT analyzed_seq FROM cursor WHERE name='default'",
            [],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .unwrap_or(0);
    // First row strictly past the cursor (gap-safe: skips any missing seq_ids).
    let claimed: Option<(i64, String)> = tx
        .query_row(
            "SELECT seq_id, session_id FROM sessions WHERE seq_id > ?1 ORDER BY seq_id LIMIT 1",
            params![pos],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    if let Some((seq_id, _)) = &claimed {
        tx.execute(
            "UPDATE cursor SET analyzed_seq=?1 WHERE name='default'",
            params![seq_id],
        )
        .map_err(|e| e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())?;

    let session_id = match claimed {
        Some((_, id)) => id,
        None => {
            println!("{}", json!({ "done": true }));
            return Ok(());
        }
    };

    // Fetch + persist the transcript (skip if we already have it, or disabled).
    if with_transcript && !has_events(&conn, &session_id).map_err(|e| e.to_string())? {
        match fetch_transcript(&session_id, max_events) {
            Ok(events) => {
                insert_events(&conn, &session_id, &events).map_err(|e| e.to_string())?;
            }
            Err(e) => {
                // Don't lose the claim on a transient transcript-fetch failure;
                // surface a warning and return metadata only.
                eprintln!("Warning: failed to fetch transcript: {}", e);
            }
        }
    }

    let mut out = session_row_json(&conn, &session_id).map_err(|e| e.to_string())?;
    // Attach the PRs this session appears in (empty array if none/un-enriched).
    let prs = session_prs_json(&conn, &session_id).map_err(|e| e.to_string())?;
    out.insert("prs".into(), Value::Array(prs));
    if with_transcript {
        let transcript = transcript_json(&conn, &session_id).map_err(|e| e.to_string())?;
        out.insert("transcript".into(), Value::Array(transcript));
    }
    println!("{}", Value::Object(out));
    Ok(())
}

fn fetch_transcript(session_id: &str, max_events: u64) -> Result<Vec<Value>, String> {
    let client = CubeClient::from_config().map_err(|e| e.to_string())?;
    let filters = json!([{
        "member": format!("{}.session_id", EVENTS_CUBE),
        "operator": "equals",
        "values": [session_id],
    }])
    .to_string();
    let args = QueryArgs {
        dimensions: event_dimensions(),
        filters_json: Some(filters),
        order: vec![
            (format!("{}.event_time", EVENTS_CUBE), "asc".into()),
            (format!("{}.output_seq", EVENTS_CUBE), "asc".into()),
        ],
        limit: Some(max_events),
        ..Default::default()
    };
    let query = args.to_query()?;
    client.load_rows(&query).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// stats / reset / exec
// ---------------------------------------------------------------------------

fn cmd_stats(args: &[String]) -> Result<(), String> {
    let path = single_db_arg(args, "stats")?;
    let conn = open_existing_db(&path)?;

    let total: i64 = conn
        .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;

    let (fetched, target, complete, analyzed_seq): (i64, Option<i64>, i64, i64) = conn
        .query_row(
            "SELECT fetched, target, pull_complete, analyzed_seq FROM cursor WHERE name='default'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .unwrap_or((0, None, 0, 0));
    // Analysis progress: rows at or before the cursor are served; the rest remain.
    let analyzed: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sessions WHERE seq_id <= ?1",
            params![analyzed_seq],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    let events: i64 = conn
        .query_row("SELECT COUNT(*) FROM session_events", [], |r| r.get(0))
        .map_err(|e| e.to_string())?;

    println!("db: {}", path);
    println!("sessions: {} total", total);
    println!(
        "analysis cursor: analyzed_seq={} ({}/{} served, {} remaining)",
        analyzed_seq,
        analyzed,
        total,
        total - analyzed
    );
    println!(
        "pull cursor: fetched={} target={} complete={}",
        fetched,
        target.map(|t| t.to_string()).unwrap_or_else(|| "-".into()),
        if complete != 0 { "yes" } else { "no" }
    );
    println!("transcript events stored: {}", events);
    Ok(())
}

fn cmd_reset(args: &[String]) -> Result<(), String> {
    let mut db_path: Option<String> = None;
    let mut to: i64 = 0;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            // Rewind (or set) the analysis cursor; `next` will re-serve from here.
            "--to" => {
                to = take_value(args, &mut i, "--to")?
                    .parse()
                    .map_err(|_| "--to must be a number".to_string())?
            }
            // `--help`/`-h` is handled by `handle_sessions` before dispatch.
            other if !other.starts_with('-') && db_path.is_none() => {
                db_path = Some(other.to_string())
            }
            other => return Err(format!("unknown reset argument: {}", other)),
        }
        i += 1;
    }
    let path = db_path.ok_or("usage: git-ai analyze sessions reset <db> [--to <seq>]")?;
    let conn = open_existing_db(&path)?;

    conn.execute(
        "UPDATE cursor SET analyzed_seq=?1 WHERE name='default'",
        params![to],
    )
    .map_err(|e| e.to_string())?;

    eprintln!(
        "Analysis cursor reset to {}; `next` will re-serve from there.",
        to
    );
    Ok(())
}

fn cmd_exec(args: &[String]) -> Result<(), String> {
    if args.len() < 2 {
        return Err("usage: git-ai analyze sessions exec <db> \"<SQL>\"".to_string());
    }
    let path = &args[0];
    let sql = &args[1];
    let conn = open_existing_db(path)?;

    let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
    let col_count = stmt.column_count();
    if col_count == 0 {
        drop(stmt);
        let n = conn.execute(sql, []).map_err(|e| e.to_string())?;
        eprintln!("{} row(s) affected.", n);
        return Ok(());
    }

    let names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    println!("{}", names.join("\t"));
    let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
    while let Some(row) = rows.next().map_err(|e| e.to_string())? {
        let cells: Vec<String> = (0..col_count)
            .map(|idx| {
                row.get_ref(idx)
                    .map(value_ref_to_string)
                    .unwrap_or_default()
            })
            .collect();
        println!("{}", cells.join("\t"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Cube query field lists
// ---------------------------------------------------------------------------

/// Fully-qualify a list of cube members as `<cube>.<member>`.
fn qualify(cube: &str, members: &[&str]) -> Vec<String> {
    members.iter().map(|m| format!("{}.{}", cube, m)).collect()
}

fn session_dimensions() -> Vec<String> {
    // session_start_time is a plain dimension (not a bare timeDimension) so that
    // `order` by it is honored — Cube ignores ordering on a timeDimension that
    // has no granularity. The `--since` dateRange still filters via timeDimensions.
    qualify(
        SESSIONS_CUBE,
        &[
            "session_id",
            "user_id",
            "agent",
            "repo_url",
            "parent_session_id",
            "child_session_count",
            "session_start_time",
            "session_end_time",
        ],
    )
}

fn session_measures() -> Vec<String> {
    qualify(
        SESSIONS_CUBE,
        &[
            "total_generated_lines",
            "total_deleted_lines",
            "net_generated_lines",
            "total_generated_sloc",
            "total_committed_lines",
            "total_pr_opened_lines",
            "total_merged_lines",
            "total_production_lines",
            "total_checkpoints",
            "total_events",
            "total_usage_minutes",
        ],
    )
}

fn event_dimensions() -> Vec<String> {
    qualify(
        EVENTS_CUBE,
        &[
            "event_kind",
            "tool",
            "tool_kind",
            "model",
            "target",
            "text",
            "summary",
            "tool_input",
            "tool_output",
            "event_time",
            "output_seq",
        ],
    )
}

/// Build Cube equals-filter objects from (member, optional value) pairs,
/// skipping any pair whose value is `None`. Returned as a `Vec` so callers can
/// merge these convenience filters with a raw `--filters` array before
/// serializing the combined `filters` clause.
fn equals_filters(pairs: &[(String, Option<&str>)]) -> Vec<Value> {
    pairs
        .iter()
        .filter_map(|(member, value)| {
            value.map(|v| json!({ "member": member, "operator": "equals", "values": [v] }))
        })
        .collect()
}

/// Merge the convenience equals-filters with the caller's raw `--filters` JSON
/// array (the full `analyze query` escape hatch) into a single Cube `filters`
/// clause. Returns `None` when there are no filters at all. The raw array must
/// be a JSON array of filter objects; anything else is a user error.
fn merge_filters(mut convenience: Vec<Value>, raw: Option<&str>) -> Result<Option<String>, String> {
    if let Some(raw) = raw {
        let parsed: Value =
            serde_json::from_str(raw).map_err(|e| format!("--filters is not valid JSON: {}", e))?;
        let extra = parsed
            .as_array()
            .ok_or("--filters must be a JSON array of filter objects")?;
        convenience.extend(extra.iter().cloned());
    }
    if convenience.is_empty() {
        Ok(None)
    } else {
        Ok(Some(Value::Array(convenience).to_string()))
    }
}

// ---------------------------------------------------------------------------
// SQLite helpers
// ---------------------------------------------------------------------------

fn open_db(path: &Path) -> Result<Connection, rusqlite::Error> {
    let conn = Connection::open(path)?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
    conn.execute_batch(SCHEMA)?;
    ensure_derived_columns(&conn)?;
    Ok(conn)
}

/// Derived "funnel gap" columns, computed straight from the raw stage measures
/// (`committed_lines` → `pr_opened_lines` → `merged_lines` → `production_lines`)
/// so an analyst can ask "what didn't ship, and where did it fall out?" with a
/// plain SELECT — no hand arithmetic, no reaching for transcripts. Each gap is
/// "lines that reached this stage but not the next"; `committed_not_production`
/// is the headline "committed but never shipped" number, and `production_rate`
/// is the share of committed work that landed in production. NULL bases are
/// coalesced to 0 so the gaps are always clean integers. The *why* behind a gap
/// (still-open PR vs. reverted vs. superseded) is NOT here — that genuinely
/// needs the transcript.
///
/// `(name, sql_type, expression)`. Added as VIRTUAL generated columns, which
/// (unlike STORED) can be introduced via `ALTER TABLE ADD COLUMN`, so old
/// scratch DBs gain them on the next open too.
const DERIVED_COLUMNS: &[(&str, &str, &str)] = &[
    (
        "committed_not_pr_opened",
        "INTEGER",
        "COALESCE(committed_lines, 0) - COALESCE(pr_opened_lines, 0)",
    ),
    (
        "pr_opened_not_merged",
        "INTEGER",
        "COALESCE(pr_opened_lines, 0) - COALESCE(merged_lines, 0)",
    ),
    (
        "merged_not_production",
        "INTEGER",
        "COALESCE(merged_lines, 0) - COALESCE(production_lines, 0)",
    ),
    (
        "committed_not_production",
        "INTEGER",
        "COALESCE(committed_lines, 0) - COALESCE(production_lines, 0)",
    ),
    (
        "production_rate",
        "REAL",
        "CAST(COALESCE(production_lines, 0) AS REAL) / NULLIF(committed_lines, 0)",
    ),
];

/// Idempotently add the derived funnel-gap columns. Skips any that already
/// exist (so it is safe to run on every `open_db`, new DB or old).
fn ensure_derived_columns(conn: &Connection) -> Result<(), rusqlite::Error> {
    for (name, sql_type, expr) in DERIVED_COLUMNS {
        let sql = format!(
            "ALTER TABLE sessions ADD COLUMN {} {} GENERATED ALWAYS AS ({}) VIRTUAL",
            name, sql_type, expr
        );
        match conn.execute(&sql, []) {
            Ok(_) => {}
            // Already present (re-opening an existing DB): nothing to do.
            Err(e) if e.to_string().contains("duplicate column name") => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

fn open_existing_db(path: &str) -> Result<Connection, String> {
    if !Path::new(path).exists() {
        return Err(format!(
            "no such db: {} (run `git-ai analyze sessions pull` first)",
            path
        ));
    }
    open_db(Path::new(path)).map_err(|e| e.to_string())
}

fn init_cursor(conn: &Connection, target: u64) -> Result<(), rusqlite::Error> {
    conn.execute(
        "INSERT INTO cursor (name, fetched, target, pull_complete) VALUES ('default', 0, ?1, 0) \
         ON CONFLICT(name) DO UPDATE SET target=MAX(target, excluded.target), pull_complete=0",
        params![target as i64],
    )?;
    Ok(())
}

fn get_fetched(conn: &Connection) -> Result<u64, rusqlite::Error> {
    let v: i64 = conn
        .query_row("SELECT fetched FROM cursor WHERE name='default'", [], |r| {
            r.get(0)
        })
        .optional()?
        .unwrap_or(0);
    Ok(v.max(0) as u64)
}

fn set_fetched(conn: &Connection, fetched: u64) -> Result<(), rusqlite::Error> {
    conn.execute(
        "UPDATE cursor SET fetched=?1 WHERE name='default'",
        params![fetched as i64],
    )?;
    Ok(())
}

fn set_pull_complete(conn: &Connection) -> Result<(), rusqlite::Error> {
    conn.execute("UPDATE cursor SET pull_complete=1 WHERE name='default'", [])?;
    Ok(())
}

fn set_meta(conn: &Connection, key: &str, value: &str) -> Result<(), rusqlite::Error> {
    conn.execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2) \
         ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![key, value],
    )?;
    Ok(())
}

/// Insert session rows (deduped by session_id). Returns count newly inserted.
fn insert_sessions(conn: &Connection, rows: &[Value]) -> Result<usize, rusqlite::Error> {
    let now = now_secs();
    let mut inserted = 0;
    let tx = conn.unchecked_transaction()?;
    {
        let mut stmt = tx.prepare(
            "INSERT OR IGNORE INTO sessions (
                session_id, user_id, agent, repo_url, parent_session_id,
                child_session_count, session_start_time, session_end_time,
                generated_lines, deleted_lines, net_generated_lines, generated_sloc,
                committed_lines, pr_opened_lines, merged_lines, production_lines,
                total_checkpoints, total_events, usage_minutes, created_at
            ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20)",
        )?;
        for row in rows {
            let session_id = match member_str(row, SESSIONS_CUBE, "session_id") {
                Some(id) if !id.is_empty() => id,
                _ => continue,
            };
            let n = stmt.execute(params![
                session_id,
                member_str(row, SESSIONS_CUBE, "user_id"),
                member_str(row, SESSIONS_CUBE, "agent"),
                member_str(row, SESSIONS_CUBE, "repo_url"),
                member_str(row, SESSIONS_CUBE, "parent_session_id"),
                member_int(row, SESSIONS_CUBE, "child_session_count"),
                member_time(row, SESSIONS_CUBE, "session_start_time"),
                member_time(row, SESSIONS_CUBE, "session_end_time"),
                member_int(row, SESSIONS_CUBE, "total_generated_lines"),
                member_int(row, SESSIONS_CUBE, "total_deleted_lines"),
                member_int(row, SESSIONS_CUBE, "net_generated_lines"),
                member_int(row, SESSIONS_CUBE, "total_generated_sloc"),
                member_int(row, SESSIONS_CUBE, "total_committed_lines"),
                member_int(row, SESSIONS_CUBE, "total_pr_opened_lines"),
                member_int(row, SESSIONS_CUBE, "total_merged_lines"),
                member_int(row, SESSIONS_CUBE, "total_production_lines"),
                member_int(row, SESSIONS_CUBE, "total_checkpoints"),
                member_int(row, SESSIONS_CUBE, "total_events"),
                member_int(row, SESSIONS_CUBE, "total_usage_minutes"),
                now,
            ])?;
            inserted += n;
        }
    }
    tx.commit()?;
    Ok(inserted)
}

// ---------------------------------------------------------------------------
// Cross-cube baseline enrichment (tokens / cost / models / PRs)
// ---------------------------------------------------------------------------

/// Backfill the comparable baseline columns for a batch of freshly-pulled
/// sessions: per-session token usage + cost, the models each session used, and
/// the PRs it appears in. Best-effort — a failure in any one source warns but
/// does not abort the pull, since the core session rows are already persisted.
fn enrich_sessions(client: &CubeClient, conn: &Connection, session_ids: &[String]) {
    if session_ids.is_empty() {
        return;
    }
    if let Err(e) = enrich_token_usage(client, conn, session_ids) {
        eprintln!("Warning: token-usage enrichment failed: {}", e);
    }
    if let Err(e) = enrich_models(client, conn, session_ids) {
        eprintln!("Warning: model enrichment failed: {}", e);
    }
    if let Err(e) = enrich_prs(client, conn, session_ids) {
        eprintln!("Warning: PR enrichment failed: {}", e);
    }
}

/// Cube `equals` filter matching any of `session_ids` (IN semantics).
fn id_filter(cube: &str, session_ids: &[String]) -> String {
    json!([{
        "member": format!("{}.session_id", cube),
        "operator": "equals",
        "values": session_ids,
    }])
    .to_string()
}

fn enrich_token_usage(
    client: &CubeClient,
    conn: &Connection,
    session_ids: &[String],
) -> Result<(), String> {
    let measures = qualify(
        TOKEN_USAGE_CUBE,
        &[
            "total_input_tokens",
            "total_output_tokens",
            "total_cache_read_tokens",
            "total_cache_creation_tokens",
            "total_reasoning_tokens",
            "total_cost",
        ],
    );
    let args = QueryArgs {
        measures,
        dimensions: vec![format!("{}.session_id", TOKEN_USAGE_CUBE)],
        filters_json: Some(id_filter(TOKEN_USAGE_CUBE, session_ids)),
        limit: Some(session_ids.len() as u64),
        ..Default::default()
    };
    let rows = client
        .load_rows(&args.to_query()?)
        .map_err(|e| e.to_string())?;
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    {
        let mut stmt = tx
            .prepare(
                "UPDATE sessions SET input_tokens=?2, output_tokens=?3, \
                 cache_read_tokens=?4, cache_creation_tokens=?5, reasoning_tokens=?6, \
                 cost_usd=?7 WHERE session_id=?1",
            )
            .map_err(|e| e.to_string())?;
        for row in &rows {
            let Some(sid) = member_str(row, TOKEN_USAGE_CUBE, "session_id") else {
                continue;
            };
            stmt.execute(params![
                sid,
                member_int(row, TOKEN_USAGE_CUBE, "total_input_tokens"),
                member_int(row, TOKEN_USAGE_CUBE, "total_output_tokens"),
                member_int(row, TOKEN_USAGE_CUBE, "total_cache_read_tokens"),
                member_int(row, TOKEN_USAGE_CUBE, "total_cache_creation_tokens"),
                member_int(row, TOKEN_USAGE_CUBE, "total_reasoning_tokens"),
                member_float(row, TOKEN_USAGE_CUBE, "total_cost"),
            ])
            .map_err(|e| e.to_string())?;
        }
    }
    tx.commit().map_err(|e| e.to_string())
}

fn enrich_models(
    client: &CubeClient,
    conn: &Connection,
    session_ids: &[String],
) -> Result<(), String> {
    let args = QueryArgs {
        measures: vec![format!("{}.event_count", SESSION_MODELS_CUBE)],
        dimensions: vec![
            format!("{}.session_id", SESSION_MODELS_CUBE),
            format!("{}.model", SESSION_MODELS_CUBE),
        ],
        filters_json: Some(id_filter(SESSION_MODELS_CUBE, session_ids)),
        // A session can use several models; allow generous headroom per id.
        limit: Some((session_ids.len() as u64).saturating_mul(20).max(50)),
        ..Default::default()
    };
    let rows = client
        .load_rows(&args.to_query()?)
        .map_err(|e| e.to_string())?;
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    {
        let mut stmt = tx
            .prepare(
                "INSERT OR REPLACE INTO session_models (session_id, model, event_count) \
                 VALUES (?1,?2,?3)",
            )
            .map_err(|e| e.to_string())?;
        for row in &rows {
            let Some(sid) = member_str(row, SESSION_MODELS_CUBE, "session_id") else {
                continue;
            };
            let Some(model) = member_str(row, SESSION_MODELS_CUBE, "model") else {
                continue;
            };
            stmt.execute(params![
                sid,
                model,
                member_int(row, SESSION_MODELS_CUBE, "event_count"),
            ])
            .map_err(|e| e.to_string())?;
        }
    }
    recompute_models(&tx).map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())
}

fn enrich_prs(
    client: &CubeClient,
    conn: &Connection,
    session_ids: &[String],
) -> Result<(), String> {
    let args = QueryArgs {
        measures: vec![format!("{}.total_ai_lines", PR_SESSIONS_CUBE)],
        dimensions: qualify(
            PR_SESSIONS_CUBE,
            &["session_id", "repo_url", "pr_number", "agent", "model_raw"],
        ),
        filters_json: Some(id_filter(PR_SESSIONS_CUBE, session_ids)),
        // A session can land in several PRs; allow generous headroom per id.
        limit: Some((session_ids.len() as u64).saturating_mul(20).max(50)),
        ..Default::default()
    };
    let rows = client
        .load_rows(&args.to_query()?)
        .map_err(|e| e.to_string())?;
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    {
        let mut stmt = tx
            .prepare(
                "INSERT OR REPLACE INTO session_prs \
                 (session_id, repo_url, pr_number, agent, model_raw, ai_lines) \
                 VALUES (?1,?2,?3,?4,?5,?6)",
            )
            .map_err(|e| e.to_string())?;
        for row in &rows {
            let Some(sid) = member_str(row, PR_SESSIONS_CUBE, "session_id") else {
                continue;
            };
            let Some(pr) = member_int(row, PR_SESSIONS_CUBE, "pr_number") else {
                continue;
            };
            stmt.execute(params![
                sid,
                // repo_url is part of the PK; coalesce to '' so NULLs don't
                // produce duplicate rows for the same PR.
                member_str(row, PR_SESSIONS_CUBE, "repo_url").unwrap_or_default(),
                pr,
                member_str(row, PR_SESSIONS_CUBE, "agent"),
                member_str(row, PR_SESSIONS_CUBE, "model_raw"),
                member_int(row, PR_SESSIONS_CUBE, "total_ai_lines"),
            ])
            .map_err(|e| e.to_string())?;
        }
    }
    recompute_pr_counts(&tx).map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())
}

/// Denormalize the distinct models per session into `sessions.models` (a
/// comma-joined, alphabetically-ordered list) for quick filtering.
fn recompute_models(conn: &Connection) -> Result<(), rusqlite::Error> {
    conn.execute(
        "UPDATE sessions SET models = (
             SELECT group_concat(model, ',') FROM (
                 SELECT model FROM session_models sm
                 WHERE sm.session_id = sessions.session_id ORDER BY model
             )
         ) WHERE session_id IN (SELECT DISTINCT session_id FROM session_models)",
        [],
    )?;
    Ok(())
}

/// Denormalize the count of distinct PRs per session into `sessions.pr_count`.
fn recompute_pr_counts(conn: &Connection) -> Result<(), rusqlite::Error> {
    conn.execute(
        "UPDATE sessions SET pr_count = (
             SELECT COUNT(*) FROM session_prs sp WHERE sp.session_id = sessions.session_id
         ) WHERE session_id IN (SELECT DISTINCT session_id FROM session_prs)",
        [],
    )?;
    Ok(())
}

fn has_events(conn: &Connection, session_id: &str) -> Result<bool, rusqlite::Error> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM session_events WHERE session_id=?1",
        params![session_id],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

fn insert_events(
    conn: &Connection,
    session_id: &str,
    rows: &[Value],
) -> Result<usize, rusqlite::Error> {
    let mut inserted = 0;
    let tx = conn.unchecked_transaction()?;
    {
        let mut stmt = tx.prepare(
            "INSERT OR IGNORE INTO session_events (
                session_id, output_seq, event_time, event_kind, tool, tool_kind,
                model, target, text, summary, tool_input, tool_output
            ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
        )?;
        for row in rows {
            let n = stmt.execute(params![
                session_id,
                member_int(row, EVENTS_CUBE, "output_seq"),
                member_time(row, EVENTS_CUBE, "event_time"),
                // event_kind/tool are part of the dedup key; coalesce to '' so
                // re-fetching is idempotent (SQLite treats NULLs as distinct).
                member_str(row, EVENTS_CUBE, "event_kind").unwrap_or_default(),
                member_str(row, EVENTS_CUBE, "tool").unwrap_or_default(),
                member_str(row, EVENTS_CUBE, "tool_kind"),
                member_str(row, EVENTS_CUBE, "model"),
                member_str(row, EVENTS_CUBE, "target"),
                member_str(row, EVENTS_CUBE, "text"),
                member_str(row, EVENTS_CUBE, "summary"),
                member_str(row, EVENTS_CUBE, "tool_input"),
                member_str(row, EVENTS_CUBE, "tool_output"),
            ])?;
            inserted += n;
        }
    }
    tx.commit()?;
    Ok(inserted)
}

/// Build a JSON object from a SQLite row, keyed by the given (select-order)
/// column names.
fn row_to_json_object(
    row: &rusqlite::Row,
    cols: &[&str],
) -> Result<Map<String, Value>, rusqlite::Error> {
    let mut obj = Map::new();
    for (idx, name) in cols.iter().enumerate() {
        obj.insert((*name).to_string(), value_ref_to_json(row.get_ref(idx)?));
    }
    Ok(obj)
}

/// Read one session row as a JSON object keyed by the public column names.
fn session_row_json(
    conn: &Connection,
    session_id: &str,
) -> Result<Map<String, Value>, rusqlite::Error> {
    let sql = format!(
        "SELECT {} FROM sessions WHERE session_id=?1",
        SESSION_COLUMNS.join(", ")
    );
    conn.query_row(&sql, params![session_id], |row| {
        row_to_json_object(row, SESSION_COLUMNS)
    })
}

/// Read a session's transcript from `session_events`, ordered chronologically.
fn transcript_json(conn: &Connection, session_id: &str) -> Result<Vec<Value>, rusqlite::Error> {
    let cols = [
        "output_seq",
        "event_time",
        "event_kind",
        "tool",
        "tool_kind",
        "model",
        "target",
        "text",
        "summary",
        "tool_input",
        "tool_output",
    ];
    let sql = format!(
        "SELECT {} FROM session_events WHERE session_id=?1 \
         ORDER BY event_time ASC, output_seq ASC, id ASC",
        cols.join(", ")
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![session_id], |row| {
        Ok(Value::Object(row_to_json_object(row, &cols)?))
    })?;
    rows.collect()
}

/// Read the PRs a session appears in from `session_prs`, ordered by PR number.
fn session_prs_json(conn: &Connection, session_id: &str) -> Result<Vec<Value>, rusqlite::Error> {
    let cols = ["repo_url", "pr_number", "agent", "model_raw", "ai_lines"];
    let sql = format!(
        "SELECT {} FROM session_prs WHERE session_id=?1 ORDER BY repo_url, pr_number",
        cols.join(", ")
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![session_id], |row| {
        Ok(Value::Object(row_to_json_object(row, &cols)?))
    })?;
    rows.collect()
}

// ---------------------------------------------------------------------------
// Value extraction / conversion
// ---------------------------------------------------------------------------

/// Look up a cube member (`<cube>.<field>`) in a row.
fn member<'a>(row: &'a Value, cube: &str, field: &str) -> Option<&'a Value> {
    row.get(format!("{}.{}", cube, field))
}

/// Get a cube member (`<cube>.<field>`) from a row as an owned String.
fn member_str(row: &Value, cube: &str, field: &str) -> Option<String> {
    match member(row, cube, field) {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        Some(Value::Number(n)) => Some(n.to_string()),
        _ => None,
    }
}

/// Get a numeric cube member. Cube returns numbers as strings, so parse both.
fn member_int(row: &Value, cube: &str, field: &str) -> Option<i64> {
    match member(row, cube, field) {
        Some(Value::String(s)) => s.trim().parse::<f64>().ok().map(|f| f as i64),
        Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        _ => None,
    }
}

/// Get a numeric cube member as f64 (e.g. cost). Cube returns numbers as
/// strings, so parse both.
fn member_float(row: &Value, cube: &str, field: &str) -> Option<f64> {
    match member(row, cube, field) {
        Some(Value::String(s)) => s.trim().parse::<f64>().ok(),
        Some(Value::Number(n)) => n.as_f64(),
        _ => None,
    }
}

/// Get a time-typed cube member (ISO8601 string) as unix seconds.
fn member_time(row: &Value, cube: &str, field: &str) -> Option<i64> {
    parse_cube_time(member(row, cube, field)?.as_str()?)
}

/// Parse a Cube time value (RFC3339 or `YYYY-MM-DDTHH:MM:SS[.fff]`) to unix secs.
fn parse_cube_time(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Some(dt.timestamp());
    }
    for fmt in [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M:%S%.f",
    ] {
        if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(s, fmt) {
            return Some(dt.and_utc().timestamp());
        }
    }
    None
}

fn value_ref_to_json(v: rusqlite::types::ValueRef) -> Value {
    use rusqlite::types::ValueRef;
    match v {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => json!(i),
        ValueRef::Real(f) => json!(f),
        ValueRef::Text(t) => Value::String(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) => Value::String(String::from_utf8_lossy(b).into_owned()),
    }
}

fn value_ref_to_string(v: rusqlite::types::ValueRef) -> String {
    use rusqlite::types::ValueRef;
    match v {
        ValueRef::Null => String::new(),
        ValueRef::Integer(i) => i.to_string(),
        ValueRef::Real(f) => f.to_string(),
        ValueRef::Text(t) => String::from_utf8_lossy(t).into_owned(),
        ValueRef::Blob(b) => String::from_utf8_lossy(b).into_owned(),
    }
}

// ---------------------------------------------------------------------------
// Misc
// ---------------------------------------------------------------------------

fn single_db_arg(args: &[String], cmd: &str) -> Result<String, String> {
    args.iter()
        .find(|a| !a.starts_with('-'))
        .cloned()
        .ok_or_else(|| format!("usage: git-ai analyze sessions {} <db>", cmd))
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Pick a fresh, scrutable DB path like `git-ai-analysis-001.db` in the temp
/// dir. Scans existing `git-ai-analysis-NNN.db` files and uses the next free
/// number so repeated pulls are easy to tell apart and reference.
fn random_db_path() -> PathBuf {
    let dir = std::env::temp_dir();
    let highest = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name();
            let name = name.to_str()?;
            let num = name.strip_prefix("git-ai-analysis-")?.strip_suffix(".db")?;
            num.parse::<u32>().ok()
        })
        .max()
        .unwrap_or(0);
    // Advance past any existing file (covers gaps / concurrent creators).
    let mut n = highest + 1;
    loop {
        let path = dir.join(format!("git-ai-analysis-{:03}.db", n));
        if !path.exists() {
            return path;
        }
        n += 1;
    }
}

fn print_help() {
    let help = r#"git-ai analyze sessions - pull coding sessions into a scratch DB and grade them

Subcommands:
  pull [flags]            Create/fill a SQLite DB with sessions (latest-first)
  next <db> [flags]       Return the next session + transcript and advance the cursor +1
  stats <db>              Show pull progress and where the analysis cursor is
  reset <db> [--to N]     Rewind the analysis cursor (default 0) to re-run from the start
  exec <db> "<SQL>"       Run SQL against the DB (writeback for analysis columns)

pull flags:
  --db <path>     Target DB (default: a fresh /tmp/git-ai-analysis-NNN.db, printed to stdout)
  --limit <n>     Max sessions to pull (default 100)
  --since <range> Cube dateRange on session_start_time (e.g. "last 30 days")
  --repo <url>    Filter by repo_url (convenience equals-filter)
  --user <id>     Filter by user_id (convenience equals-filter)
  --agent <name>  Filter by agent, e.g. claude-code, cursor (convenience equals-filter)
  --include-subagents  Include subagent sessions (default: top-level sessions only)
  --no-enrich     Skip the per-batch tokens/cost/models/PR backfill (faster, fewer columns)

Analyzing your OWN (or one person's) sessions ("my sessions", "what I did"):
  user_id is opaque — resolve it from an email first, then pull with --user:
    EMAIL=$(git config user.email)
    UID=$(git-ai analyze query --tsv -d public_v1_user_status.user_id \
      -f '[{"member":"public_v1_user_status.author_email","operator":"equals","values":["'"$EMAIL"'"]}]' \
      | tail -n +2 | head -1)
    DB=$(git-ai analyze sessions pull --user "$UID" --since "last 30 days")
  Swap in a different email for someone else. If the lookup is empty, stop and say so.

  Full query surface — same flags as `analyze query`, layered on top of the
  canonical session columns so you can slice the population on ANY member:
  -f, --filters '<json>'    Raw Cube filter array (combines with --repo/--user/--agent)
  -d, --dimensions a,b      Extra dimensions to select alongside the session columns
  -m, --measures a,b        Extra measures to select
      --time-dimension M    Time dimension member (default: session_start_time with --since)
  -g, --granularity G       Granularity for the time dimension
  -o, --order M[:asc|desc]  Order rows (default: session_start_time:desc)

  Examples:
    # only sessions that generated >100 net lines, made with cursor:
    git-ai analyze sessions pull --agent cursor \
      -f '[{"member":"public_v1_sessions.net_generated_lines","operator":"gt","values":["100"]}]'
    # sessions whose work reached production, newest committed first:
    git-ai analyze sessions pull \
      -f '[{"member":"public_v1_sessions.total_production_lines","operator":"gt","values":["0"]}]' \
      -o public_v1_sessions.session_end_time:desc

next flags:
  --max-events <n>   Cap transcript events fetched (default 2000)
  --no-transcript    Return session metadata only (no transcript fetch)

What `sessions next` prints (one JSON object, or {"done": true} when exhausted):
  All the `sessions` columns (session_id, agent, repo_url, generated_lines,
  committed_lines, total_events, models, cost_usd, pr_count, …), plus:
    "prs":        array of the PRs this session appears in (repo_url, pr_number,
                  agent, model_raw, ai_lines)
    "transcript": array of events in chronological order — THE PROMPTS + ACTIONS.
  Each transcript event is a flat object; which fields are set depends on event_kind:
    output_seq    int, ordering within an event_time bucket
    event_time    unix seconds
    event_kind    the message type. The transcript is an interleaved, ordered
                  stream dominated by three kinds:
                    user_message       a human turn (the PROMPT) — has .text
                    assistant_message  the agent's reply prose      — has .text
                    tool_call          an action the agent took — has .tool/.tool_kind/
                                       .target/.tool_input/.tool_output
                  and occasionally: skill_load | skill_mention | skill_invoked |
                  pr_created | context_compacted | turn_aborted | session_model_change
    text          message body (user_message / assistant_message)
    summary       short summary when present
    model         model in effect for the event
    tool          tool name for tool_call (e.g. Edit, Read, Bash, Grep)
    tool_kind     shell | file_read | file_edit | web_search | sub_agent | mcp | other
    target        file/path or other target the tool acted on
    tool_input    arguments passed to the tool (string)
    tool_output   result returned by the tool (string)
  So a user's PROMPTS are the event_kind='user_message' .text values, in order; the
  agent's work is the tool_call / assistant_message stream between them. You do NOT
  need to pull a sample session to learn the shape — it is exactly the above.

reset flags:
  --to <seq>         Cursor position to rewind to (default 0 = re-serve everything)

The work funnel (already columns — DO NOT read transcripts to derive these):
  Every session carries its full delivery funnel as plain columns, in order:
    committed_lines → pr_opened_lines → merged_lines → production_lines
  plus the stage-to-stage GAPS, precomputed so you never hand-derive "what
  didn't ship":
    committed_not_pr_opened    committed locally but never opened in a PR
    pr_opened_not_merged       reached a PR but not merged (open OR torn out)
    merged_not_production      merged but not in production
    committed_not_production   headline: committed that never shipped
    production_rate            share of committed lines that reached production
  So "how much did this session commit that didn't ship, and where did it fall
  out?" is a SELECT, not a transcript read:
    git-ai analyze sessions exec "$DB" \
      "SELECT agent, SUM(committed_not_production), AVG(production_rate) \
       FROM sessions GROUP BY agent ORDER BY 2 DESC"
  What these columns CANNOT tell you is the *why* behind a gap — still-open PR
  vs. reverted-as-bad vs. superseded-by-a-rewrite. That distinction lives only
  in the session narrative, so reach for the transcript ONLY for the why, after
  the funnel columns have told you which sessions leak and by how much.

Baseline schema (every pulled session carries these, no extra work):
  sessions columns include the canonical metrics — agent, generated_lines,
  committed_lines, merged_lines, production_lines, total_events (# turns),
  usage_minutes — PLUS the cross-cube baseline backfilled at pull time:
    models                                     comma-joined distinct models used
    input_tokens, output_tokens,               per-session token usage
      cache_read_tokens, cache_creation_tokens,
      reasoning_tokens
    cost_usd                                   per-session estimated cost
    pr_count                                   # of PRs this session appears in
  Two child tables (foreign-keyed on session_id) hold the detail to JOIN against:
    session_models(session_id, model, event_count)
    session_prs(session_id, repo_url, pr_number, agent, model_raw, ai_lines)
  So you can aggregate immediately, e.g.:
    git-ai analyze sessions exec "$DB" \
      "SELECT agent, SUM(cost_usd), AVG(total_events) FROM sessions GROUP BY agent"
    git-ai analyze sessions exec "$DB" \
      "SELECT model, COUNT(*) FROM session_models GROUP BY model ORDER BY 2 DESC"
  (`sessions next` also returns a "prs" array per session.)

How the analysis cursor works:
  Each session row has an auto-increment seq_id. `next` advances a single cursor
  by one and returns that row — atomically, so every row is handed out EXACTLY
  ONCE across any number of concurrent subagents. Nothing to mark "done": once a
  row is served the cursor has already moved past it. Start a new analysis pass
  with `reset` (cursor → 0). `pull` only adds rows; it never moves the cursor.

Grading workflow:
  1. Pull the sessions:
       DB=$(git-ai analyze sessions pull --limit 100 --since "last 30 days")

  2. DESIGN YOUR ANALYSIS SCHEMA FIRST. Turn the user's question into concrete
     columns and ADD them up front — do not just print findings to chat, persist
     them so they can be aggregated. Decide your own criteria for the task:
       - Grading? Derive the rubric, then make a column per criterion plus an
         overall, e.g. clarity_score, correctness_score, grade, rationale.
       - Categorizing? One column for the label plus a notes/evidence column.
     Add them once (TEXT or INTEGER), before iterating:
       git-ai analyze sessions exec "$DB" "ALTER TABLE sessions ADD COLUMN grade TEXT"
       git-ai analyze sessions exec "$DB" "ALTER TABLE sessions ADD COLUMN clarity_score INTEGER"
       git-ai analyze sessions exec "$DB" "ALTER TABLE sessions ADD COLUMN rationale TEXT"

  3. Dispatch N subagents. Each loops, and MUST write its results back into the
     columns you added (not just return prose). The session_id comes from the
     JSON that `next` prints:
       git-ai analyze sessions next "$DB"          # returns one session + transcript JSON
       # …analyze the transcript against your criteria…
       git-ai analyze sessions exec "$DB" \
         "UPDATE sessions SET grade='A', clarity_score=4, rationale='…' \
          WHERE session_id='<id>'"
     Loop until `next` prints {"done": true}.

  4. Watch progress:  git-ai analyze sessions stats "$DB"   # shows cursor position
  5. Synthesize over the enriched columns:
       git-ai analyze sessions exec "$DB" \
         "SELECT grade, COUNT(*), AVG(clarity_score) FROM sessions \
          WHERE grade IS NOT NULL GROUP BY grade ORDER BY grade"
"#;
    eprint!("{help}");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        // Mirror open_db so the derived funnel-gap columns are present in tests.
        ensure_derived_columns(&conn).unwrap();
        conn
    }

    fn session_row(id: &str) -> Value {
        json!({
            "public_v1_sessions.session_id": id,
            "public_v1_sessions.user_id": "u1",
            "public_v1_sessions.agent": "claude-code",
            "public_v1_sessions.repo_url": "https://example.com/repo",
            "public_v1_sessions.session_start_time": "2024-06-01T12:00:00.000",
            "public_v1_sessions.total_generated_lines": "240",
            "public_v1_sessions.total_production_lines": "180",
            "public_v1_sessions.net_generated_lines": "-5",
        })
    }

    #[test]
    fn insert_sessions_dedupes_by_session_id() {
        let conn = mem_db();
        let rows = vec![session_row("s1"), session_row("s2")];
        assert_eq!(insert_sessions(&conn, &rows).unwrap(), 2);
        // Re-inserting the same ids adds nothing.
        let rows2 = vec![session_row("s1"), session_row("s3")];
        assert_eq!(insert_sessions(&conn, &rows2).unwrap(), 1);
        let total: i64 = conn
            .query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(total, 3);
    }

    #[test]
    fn derived_funnel_gaps_compute_from_stage_columns() {
        let conn = mem_db();
        insert_sessions(&conn, &[session_row("s1")]).unwrap();
        // Set a full funnel: 100 committed → 70 pr_opened → 40 merged → 30 prod.
        conn.execute(
            "UPDATE sessions SET committed_lines=100, pr_opened_lines=70, \
             merged_lines=40, production_lines=30 WHERE session_id='s1'",
            [],
        )
        .unwrap();
        let (c_npr, pr_nm, m_np, c_np, rate): (i64, i64, i64, i64, f64) = conn
            .query_row(
                "SELECT committed_not_pr_opened, pr_opened_not_merged, \
                 merged_not_production, committed_not_production, production_rate \
                 FROM sessions WHERE session_id='s1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .unwrap();
        assert_eq!(c_npr, 30); // 100 - 70
        assert_eq!(pr_nm, 30); // 70 - 40
        assert_eq!(m_np, 10); // 40 - 30
        assert_eq!(c_np, 70); // 100 - 30
        assert!((rate - 0.30).abs() < 1e-9); // 30 / 100

        // NULL stage columns coalesce to 0 (clean integer gaps), and a 0/NULL
        // committed denominator yields a NULL rate rather than a divide error.
        insert_sessions(&conn, &[session_row("s2")]).unwrap();
        let (c_np2, rate2): (i64, Option<f64>) = conn
            .query_row(
                "SELECT committed_not_production, production_rate \
                 FROM sessions WHERE session_id='s2'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        // s2 has production=180 but committed is NULL → 0 - 180 = -180.
        assert_eq!(c_np2, -180);
        assert_eq!(rate2, None);
    }

    #[test]
    fn recompute_models_denormalizes_distinct_models() {
        let conn = mem_db();
        insert_sessions(&conn, &[session_row("s1"), session_row("s2")]).unwrap();
        // Two models for s1 (out of order), one for s2.
        conn.execute(
            "INSERT INTO session_models (session_id, model, event_count) VALUES \
             ('s1','sonnet',3),('s1','opus',1),('s2','opus',2)",
            [],
        )
        .unwrap();
        recompute_models(&conn).unwrap();
        let m1: String = conn
            .query_row(
                "SELECT models FROM sessions WHERE session_id='s1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        // Alphabetically ordered, comma-joined.
        assert_eq!(m1, "opus,sonnet");
        let m2: String = conn
            .query_row(
                "SELECT models FROM sessions WHERE session_id='s2'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(m2, "opus");
    }

    #[test]
    fn recompute_pr_counts_counts_distinct_prs() {
        let conn = mem_db();
        insert_sessions(&conn, &[session_row("s1"), session_row("s2")]).unwrap();
        conn.execute(
            "INSERT INTO session_prs (session_id, repo_url, pr_number, ai_lines) VALUES \
             ('s1','r',1,10),('s1','r',2,20)",
            [],
        )
        .unwrap();
        recompute_pr_counts(&conn).unwrap();
        let (c1, c2): (i64, Option<i64>) = (
            conn.query_row(
                "SELECT pr_count FROM sessions WHERE session_id='s1'",
                [],
                |r| r.get(0),
            )
            .unwrap(),
            conn.query_row(
                "SELECT pr_count FROM sessions WHERE session_id='s2'",
                [],
                |r| r.get(0),
            )
            .unwrap(),
        );
        assert_eq!(c1, 2);
        // s2 has no PRs, so it stays NULL (untouched by recompute).
        assert_eq!(c2, None);
    }

    #[test]
    fn session_numbers_and_time_parse() {
        let conn = mem_db();
        insert_sessions(&conn, &[session_row("s1")]).unwrap();
        let (g, prod, net, start): (i64, i64, i64, i64) = conn
            .query_row(
                "SELECT generated_lines, production_lines, net_generated_lines, session_start_time \
                 FROM sessions WHERE session_id='s1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(g, 240);
        assert_eq!(prod, 180);
        assert_eq!(net, -5);
        // 2024-06-01T12:00:00Z
        assert_eq!(start, 1_717_243_200);
    }

    /// Mirror of cmd_next's atomic cursor advance: read analyzed_seq, take the
    /// first row past it, persist the new position. Returns the served id.
    fn claim_next(conn: &mut Connection) -> Option<String> {
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        let pos: i64 = tx
            .query_row(
                "SELECT analyzed_seq FROM cursor WHERE name='default'",
                [],
                |r| r.get(0),
            )
            .optional()
            .unwrap()
            .unwrap_or(0);
        let row: Option<(i64, String)> = tx
            .query_row(
                "SELECT seq_id, session_id FROM sessions WHERE seq_id > ?1 ORDER BY seq_id LIMIT 1",
                params![pos],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .unwrap();
        if let Some((seq, _)) = &row {
            tx.execute(
                "UPDATE cursor SET analyzed_seq=?1 WHERE name='default'",
                params![seq],
            )
            .unwrap();
        }
        tx.commit().unwrap();
        row.map(|(_, id)| id)
    }

    #[test]
    fn cursor_serves_each_row_exactly_once() {
        let mut conn = mem_db();
        init_cursor(&conn, 100).unwrap();
        insert_sessions(&conn, &[session_row("s1"), session_row("s2")]).unwrap();

        assert_eq!(claim_next(&mut conn), Some("s1".to_string()));
        assert_eq!(claim_next(&mut conn), Some("s2".to_string()));
        // Past the end: nothing more, and it stays exhausted.
        assert_eq!(claim_next(&mut conn), None);
        assert_eq!(claim_next(&mut conn), None);
    }

    #[test]
    fn reset_rewinds_cursor_to_reserve_from_start() {
        let mut conn = mem_db();
        init_cursor(&conn, 100).unwrap();
        insert_sessions(&conn, &[session_row("s1"), session_row("s2")]).unwrap();
        assert_eq!(claim_next(&mut conn), Some("s1".to_string()));
        assert_eq!(claim_next(&mut conn), Some("s2".to_string()));

        // reset <db> sets the cursor back to 0.
        conn.execute("UPDATE cursor SET analyzed_seq=0 WHERE name='default'", [])
            .unwrap();
        // next now re-serves from the top.
        assert_eq!(claim_next(&mut conn), Some("s1".to_string()));
    }

    #[test]
    fn pull_does_not_rewind_analysis_cursor() {
        // Re-running pull (init_cursor) must not reset analysis progress.
        let conn = mem_db();
        init_cursor(&conn, 100).unwrap();
        conn.execute("UPDATE cursor SET analyzed_seq=5 WHERE name='default'", [])
            .unwrap();
        init_cursor(&conn, 100).unwrap();
        let pos: i64 = conn
            .query_row(
                "SELECT analyzed_seq FROM cursor WHERE name='default'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(pos, 5);
    }

    #[test]
    fn events_persist_idempotently_and_round_trip() {
        let conn = mem_db();
        insert_sessions(&conn, &[session_row("s1")]).unwrap();
        let events = vec![
            json!({
                "public_v1_normalized_events.event_kind": "user_message",
                "public_v1_normalized_events.text": "fix the bug",
                "public_v1_normalized_events.event_time": "2024-06-01T12:00:00.000",
                "public_v1_normalized_events.output_seq": "0",
            }),
            json!({
                "public_v1_normalized_events.event_kind": "tool_call",
                "public_v1_normalized_events.tool": "Edit",
                "public_v1_normalized_events.tool_input": "...",
                "public_v1_normalized_events.event_time": "2024-06-01T12:00:05.000",
                "public_v1_normalized_events.output_seq": "1",
            }),
        ];
        assert_eq!(insert_events(&conn, "s1", &events).unwrap(), 2);
        // Idempotent: re-inserting the same events adds nothing.
        assert_eq!(insert_events(&conn, "s1", &events).unwrap(), 0);

        let transcript = transcript_json(&conn, "s1").unwrap();
        assert_eq!(transcript.len(), 2);
        assert_eq!(transcript[0]["event_kind"], json!("user_message"));
        assert_eq!(transcript[0]["text"], json!("fix the bug"));
        assert_eq!(transcript[1]["tool"], json!("Edit"));
    }

    #[test]
    fn cursor_tracks_fetch_progress() {
        let conn = mem_db();
        init_cursor(&conn, 100).unwrap();
        assert_eq!(get_fetched(&conn).unwrap(), 0);
        set_fetched(&conn, 50).unwrap();
        assert_eq!(get_fetched(&conn).unwrap(), 50);
        set_pull_complete(&conn).unwrap();
        let complete: i64 = conn
            .query_row(
                "SELECT pull_complete FROM cursor WHERE name='default'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(complete, 1);
    }

    #[test]
    fn equals_filters_skips_unset() {
        assert!(equals_filters(&[("m".into(), None)]).is_empty());
        let f = equals_filters(&[
            ("public_v1_sessions.agent".into(), Some("cursor")),
            ("public_v1_sessions.repo_url".into(), None),
        ]);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0]["values"][0], json!("cursor"));
    }

    #[test]
    fn merge_filters_combines_convenience_and_raw() {
        // No filters at all -> None.
        assert_eq!(merge_filters(vec![], None).unwrap(), None);

        // Convenience-only passes through.
        let conv = equals_filters(&[("public_v1_sessions.agent".into(), Some("cursor"))]);
        let only_conv = merge_filters(conv.clone(), None).unwrap().unwrap();
        let v: Value = serde_json::from_str(&only_conv).unwrap();
        assert_eq!(v.as_array().unwrap().len(), 1);

        // Convenience + raw --filters are concatenated so both apply, letting you
        // slice on any member (here: net_generated_lines) alongside --agent.
        let raw = r#"[{"member":"public_v1_sessions.net_generated_lines","operator":"gt","values":["100"]}]"#;
        let merged = merge_filters(conv, Some(raw)).unwrap().unwrap();
        let v: Value = serde_json::from_str(&merged).unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["member"], json!("public_v1_sessions.agent"));
        assert_eq!(
            arr[1]["member"],
            json!("public_v1_sessions.net_generated_lines")
        );
        assert_eq!(arr[1]["operator"], json!("gt"));
    }

    #[test]
    fn merge_filters_rejects_non_array_raw() {
        assert!(merge_filters(vec![], Some("not json")).is_err());
        assert!(merge_filters(vec![], Some(r#"{"not":"an array"}"#)).is_err());
    }
}
