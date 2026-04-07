//! `git-ai prompts` command suite
//!
//! Creates a local SQLite database (prompts.db) for terminal-friendly prompt analysis.
//! Designed for Claude Code skills and other terminal-based analysis tools.

use crate::authorship::authorship_log::PromptRecord;
use crate::authorship::internal_db::InternalDatabase;
use crate::authorship::transcript::AiTranscript;
use crate::error::GitAiError;
use crate::git::find_repository_in_path;
use crate::git::repository::{Repository, exec_git, exec_git_stdin};
use chrono::{Local, TimeZone};
use rusqlite::{Connection, params};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::env;
use std::time::{SystemTime, UNIX_EPOCH};

/// Schema for the local prompts.db file
const PROMPTS_DB_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS prompts (
    seq_id INTEGER PRIMARY KEY AUTOINCREMENT,
    id TEXT NOT NULL UNIQUE,
    tool TEXT NOT NULL,
    model TEXT NOT NULL,
    external_thread_id TEXT,
    human_author TEXT,
    commit_sha TEXT,
    workdir TEXT,
    total_additions INTEGER,
    total_deletions INTEGER,
    accepted_lines INTEGER,
    overridden_lines INTEGER,
    accepted_rate REAL,
    messages TEXT,
    start_time INTEGER,
    last_time INTEGER,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS pointers (
    name TEXT PRIMARY KEY DEFAULT 'default',
    current_seq_id INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_prompts_id ON prompts(id);
CREATE INDEX IF NOT EXISTS idx_prompts_tool ON prompts(tool);
CREATE INDEX IF NOT EXISTS idx_prompts_human_author ON prompts(human_author);
CREATE INDEX IF NOT EXISTS idx_prompts_start_time ON prompts(start_time);
"#;

/// Prompt whose messages need CAS resolution before writing to prompts.db
struct DeferredPrompt {
    id: String,
    tool: String,
    model: String,
    external_thread_id: String,
    human_author: Option<String>,
    commit_sha: String,
    workdir: String,
    total_additions: u32,
    total_deletions: u32,
    accepted_lines: u32,
    overridden_lines: u32,
    messages_url: String,
    created_at: i64,
    updated_at: i64,
}

/// Canonical pick for a given prompt_id across all live notes — the entry whose
/// associated commit has the most recent committer date.
struct BestPrompt {
    commit_sha: String,
    commit_date: i64,
    workdir: String,
    prompt_record: PromptRecord,
}

/// Output record for `prompts next` command (JSON format)
#[derive(Debug, Serialize)]
pub struct PromptOutput {
    pub seq_id: i64,
    pub id: String,
    pub tool: String,
    pub model: String,
    pub external_thread_id: Option<String>,
    pub human_author: Option<String>,
    pub commit_sha: Option<String>,
    pub workdir: Option<String>,
    pub total_additions: Option<i64>,
    pub total_deletions: Option<i64>,
    pub accepted_lines: Option<i64>,
    pub overridden_lines: Option<i64>,
    pub accepted_rate: Option<f64>,
    pub messages: Option<String>,
    pub start_time: Option<i64>,
    pub last_time: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Main entry point for `git-ai prompts` command
pub fn handle_prompts(args: &[String]) {
    if args.is_empty() {
        // Default: populate command
        handle_populate(&[]);
        return;
    }

    match args[0].as_str() {
        "exec" => handle_exec(&args[1..]),
        "list" => handle_list(&args[1..]),
        "next" => handle_next(&args[1..]),
        "reset" => handle_reset(&args[1..]),
        "count" => handle_count(&args[1..]),
        arg if arg.starts_with('-') => handle_populate(args), // flags for populate
        _ => {
            eprintln!("Unknown subcommand: {}", args[0]);
            eprintln!("Usage: git-ai prompts [exec|list|next|count|reset] [options]");
            std::process::exit(1);
        }
    }
}

/// Handle populate command (default when no subcommand or with flags).
///
/// Discovery is notes-only (squash/rebase resilient): every note in `refs/notes/ai`
/// is enumerated, orphaned notes are dropped, prompts are deduped by id picking the
/// note whose live commit has the most recent committer date, then `--since` filters
/// against that commit date. Message bodies come from inline note JSON, CAS (logged-in
/// only, instance-matching URLs), or local SQLite as a fallback.
fn handle_populate(args: &[String]) {
    let mut since_str: Option<String> = None;
    let mut author: Option<String> = None;
    let mut all_authors = false;

    // Parse arguments
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--since" => {
                if i + 1 >= args.len() {
                    eprintln!("Error: --since requires a value");
                    std::process::exit(1);
                }
                i += 1;
                since_str = Some(args[i].clone());
            }
            "--author" => {
                if i + 1 >= args.len() {
                    eprintln!("Error: --author requires a value");
                    std::process::exit(1);
                }
                i += 1;
                author = Some(args[i].clone());
            }
            "--all-authors" => {
                all_authors = true;
            }
            _ => {
                eprintln!("Unknown option: {}", args[i]);
                std::process::exit(1);
            }
        }
        i += 1;
    }

    // Default: --since 30 (days) if not specified
    let since_str = since_str.unwrap_or_else(|| "30".to_string());
    let since_timestamp = match parse_since_arg(&since_str) {
        Ok(ts) => ts,
        Err(e) => {
            eprintln!("Error parsing --since: {}", e);
            std::process::exit(1);
        }
    };

    // Get author filter
    let author_filter = if all_authors {
        None
    } else if let Some(auth) = author {
        Some(auth)
    } else {
        // Default: current git user.name
        get_current_git_user_name()
    };

    // Workdir is always the current working directory (notes-only discovery means
    // there's no clean way to enumerate other repos without re-introducing the
    // SQLite-discovery path).
    let workdir = match env::current_dir() {
        Ok(p) => p.to_string_lossy().to_string(),
        Err(e) => {
            eprintln!("Failed to get current working directory: {}", e);
            std::process::exit(1);
        }
    };

    // Open/create prompts.db in current directory
    let db_path = "prompts.db";
    let conn = match Connection::open(db_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Failed to open prompts.db: {}", e);
            std::process::exit(1);
        }
    };

    // Initialize schema
    if let Err(e) = conn.execute_batch(PROMPTS_DB_SCHEMA) {
        eprintln!("Failed to initialize schema: {}", e);
        std::process::exit(1);
    }

    // Log filter info
    eprintln!("Fetching prompts...");
    eprintln!(
        "  since: {} ({} days ago)",
        format_timestamp_as_date(since_timestamp),
        since_str
    );
    if let Some(ref author) = author_filter {
        eprintln!("  author: {}", author);
    } else {
        eprintln!("  author: (all)");
    }
    eprintln!("  repo: {}", workdir);

    // Track seen prompt IDs (the notes pipeline already dedupes via BestPrompt, so
    // this is mostly for cross-source bookkeeping in case we add more sources later).
    let mut seen_ids: HashSet<String> = HashSet::new();

    // Notes-driven pipeline: enumerate, filter orphans, group by prompt_id, resolve bodies.
    eprintln!("  git notes:");
    let deferred_prompts = match fetch_from_git_notes(
        &conn,
        since_timestamp,
        author_filter.as_deref(),
        &workdir,
        &mut seen_ids,
    ) {
        Ok((_unique_count, deferred)) => deferred,
        Err(e) => {
            eprintln!("    error - {}", e);
            Vec::new()
        }
    };

    // Resolve message bodies for deferred prompts (CAS + SQLite fallback).
    if !deferred_prompts.is_empty() {
        resolve_cas_messages(&conn, &deferred_prompts);
    }

    // Report actual row count, not seen_ids (which includes prompts skipped for missing messages)
    let db_count = conn
        .query_row("SELECT COUNT(*) FROM prompts", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap_or(0);
    eprintln!("Done. {} prompts in {}", db_count, db_path);
}

/// Handle `exec` subcommand - execute arbitrary SQL
fn handle_exec(args: &[String]) {
    if args.is_empty() {
        eprintln!("Error: exec requires a SQL statement");
        eprintln!("Usage: git-ai prompts exec \"<SQL>\"");
        std::process::exit(1);
    }

    let sql = args.join(" ");
    let conn = match open_prompts_db() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    };

    // Determine if this is a SELECT query (returns rows) or modification query
    let sql_upper = sql.trim().to_uppercase();
    if sql_upper.starts_with("SELECT") {
        // Execute as query and print results
        match conn.prepare(&sql) {
            Ok(mut stmt) => {
                let column_names: Vec<String> =
                    stmt.column_names().iter().map(|s| s.to_string()).collect();

                // Print header
                println!("{}", column_names.join("\t"));

                // Print rows
                let rows = stmt.query_map([], |row| {
                    let values: Vec<String> = (0..column_names.len())
                        .map(|i| {
                            row.get::<_, rusqlite::types::Value>(i)
                                .map(|v| format_value(&v))
                                .unwrap_or_else(|_| "NULL".to_string())
                        })
                        .collect();
                    Ok(values.join("\t"))
                });

                match rows {
                    Ok(rows) => {
                        for row in rows {
                            match row {
                                Ok(line) => println!("{}", line),
                                Err(e) => eprintln!("Error reading row: {}", e),
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("Query error: {}", e);
                        std::process::exit(1);
                    }
                }
            }
            Err(e) => {
                eprintln!("SQL error: {}", e);
                std::process::exit(1);
            }
        }
    } else {
        // Execute as modification (INSERT, UPDATE, DELETE, ALTER, etc.)
        match conn.execute(&sql, []) {
            Ok(rows_affected) => {
                eprintln!("OK. {} rows affected.", rows_affected);
            }
            Err(e) => {
                // Try execute_batch for statements like ALTER TABLE
                if let Err(e2) = conn.execute_batch(&sql) {
                    eprintln!("SQL error: {} (also tried batch: {})", e, e2);
                    std::process::exit(1);
                } else {
                    eprintln!("OK.");
                }
            }
        }
    }
}

/// Handle `list` subcommand - list prompts as TSV
fn handle_list(args: &[String]) {
    let mut columns: Option<Vec<String>> = None;

    // Parse arguments
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--columns" => {
                if i + 1 >= args.len() {
                    eprintln!("Error: --columns requires a value");
                    std::process::exit(1);
                }
                i += 1;
                columns = Some(args[i].split(',').map(|s| s.trim().to_string()).collect());
            }
            _ => {
                eprintln!("Unknown option: {}", args[i]);
                std::process::exit(1);
            }
        }
        i += 1;
    }

    let conn = match open_prompts_db() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    };

    // Build query - concise default columns for terminal output
    let default_columns = "seq_id, tool, model, human_author, commit_sha, \
                           total_additions, total_deletions, accepted_lines, \
                           overridden_lines, accepted_rate, \
                           (last_time - start_time) AS duration";
    let column_list = columns
        .as_ref()
        .map(|cols| cols.join(", "))
        .unwrap_or_else(|| default_columns.to_string());
    let sql = format!("SELECT {} FROM prompts ORDER BY seq_id ASC", column_list);

    match conn.prepare(&sql) {
        Ok(mut stmt) => {
            let column_names: Vec<String> =
                stmt.column_names().iter().map(|s| s.to_string()).collect();

            // Print header
            println!("{}", column_names.join("\t"));

            // Print rows
            let rows = stmt.query_map([], |row| {
                let values: Vec<String> = (0..column_names.len())
                    .map(|i| {
                        row.get::<_, rusqlite::types::Value>(i)
                            .map(|v| format_value(&v))
                            .unwrap_or_else(|_| "NULL".to_string())
                    })
                    .collect();
                Ok(values.join("\t"))
            });

            match rows {
                Ok(rows) => {
                    for row in rows {
                        match row {
                            Ok(line) => println!("{}", line),
                            Err(e) => eprintln!("Error reading row: {}", e),
                        }
                    }
                }
                Err(e) => {
                    eprintln!("Query error: {}", e);
                    std::process::exit(1);
                }
            }
        }
        Err(e) => {
            eprintln!("SQL error: {}", e);
            std::process::exit(1);
        }
    }
}

/// Handle `next` subcommand - return next prompt as JSON
fn handle_next(_args: &[String]) {
    let conn = match open_prompts_db() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    };

    // Get current pointer
    let current_seq_id: i64 = conn
        .query_row(
            "SELECT current_seq_id FROM pointers WHERE name = 'default'",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);

    // Get next prompt
    let result: Result<PromptOutput, rusqlite::Error> = conn.query_row(
        "SELECT seq_id, id, tool, model, external_thread_id, human_author,
                commit_sha, workdir, total_additions, total_deletions,
                accepted_lines, overridden_lines, accepted_rate, messages,
                start_time, last_time, created_at, updated_at
         FROM prompts WHERE seq_id > ?1 ORDER BY seq_id ASC LIMIT 1",
        params![current_seq_id],
        |row| {
            Ok(PromptOutput {
                seq_id: row.get(0)?,
                id: row.get(1)?,
                tool: row.get(2)?,
                model: row.get(3)?,
                external_thread_id: row.get(4)?,
                human_author: row.get(5)?,
                commit_sha: row.get(6)?,
                workdir: row.get(7)?,
                total_additions: row.get(8)?,
                total_deletions: row.get(9)?,
                accepted_lines: row.get(10)?,
                overridden_lines: row.get(11)?,
                accepted_rate: row.get(12)?,
                messages: row.get(13)?,
                start_time: row.get(14)?,
                last_time: row.get(15)?,
                created_at: row.get(16)?,
                updated_at: row.get(17)?,
            })
        },
    );

    match result {
        Ok(prompt) => {
            // Update pointer
            let _ = conn.execute(
                "INSERT INTO pointers (name, current_seq_id) VALUES ('default', ?1)
                 ON CONFLICT(name) DO UPDATE SET current_seq_id = ?1",
                params![prompt.seq_id],
            );

            // Output as JSON
            match serde_json::to_string(&prompt) {
                Ok(json) => println!("{}", json),
                Err(e) => {
                    eprintln!("Error serializing prompt: {}", e);
                    std::process::exit(1);
                }
            }
        }
        Err(rusqlite::Error::QueryReturnedNoRows) => {
            eprintln!("No more prompts. Use 'git-ai prompts reset' to start over.");
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("Error fetching prompt: {}", e);
            std::process::exit(1);
        }
    }
}

/// Handle `reset` subcommand - reset iteration pointer
fn handle_reset(_args: &[String]) {
    let conn = match open_prompts_db() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    };

    match conn.execute(
        "INSERT INTO pointers (name, current_seq_id) VALUES ('default', 0)
         ON CONFLICT(name) DO UPDATE SET current_seq_id = 0",
        [],
    ) {
        Ok(_) => {
            eprintln!("Pointer reset to start. run 'git-ai prompts next' to get the first prompt.");
        }
        Err(e) => {
            eprintln!("Error resetting pointer: {}", e);
            std::process::exit(1);
        }
    }
}

/// Handle `count` subcommand - print total number of prompts
fn handle_count(_args: &[String]) {
    let conn = match open_prompts_db() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    };

    match conn.query_row("SELECT COUNT(*) FROM prompts", [], |row| {
        row.get::<_, i64>(0)
    }) {
        Ok(count) => {
            println!("{}", count);
        }
        Err(e) => {
            eprintln!("Error counting prompts: {}", e);
            std::process::exit(1);
        }
    }
}

// ============================================================================
// Helper functions
// ============================================================================

/// Open existing prompts.db or error
fn open_prompts_db() -> Result<Connection, GitAiError> {
    let db_path = "prompts.db";
    if !std::path::Path::new(db_path).exists() {
        return Err(GitAiError::Generic(
            "prompts.db not found. Run 'git-ai prompts' first to create it.".to_string(),
        ));
    }
    Connection::open(db_path)
        .map_err(|e| GitAiError::Generic(format!("Failed to open database: {}", e)))
}

/// Format a rusqlite Value for TSV output
fn format_value(value: &rusqlite::types::Value) -> String {
    match value {
        rusqlite::types::Value::Null => "NULL".to_string(),
        rusqlite::types::Value::Integer(i) => i.to_string(),
        rusqlite::types::Value::Real(f) => format!("{:.4}", f),
        rusqlite::types::Value::Text(s) => {
            // Escape tabs and newlines for TSV output
            s.replace('\t', "\\t").replace('\n', "\\n")
        }
        rusqlite::types::Value::Blob(b) => format!("<blob {} bytes>", b.len()),
    }
}

/// Get current git user.name from config (used for author filtering)
fn get_current_git_user_name() -> Option<String> {
    let current_dir = env::current_dir().ok()?.to_string_lossy().to_string();
    let repo = find_repository_in_path(&current_dir).ok()?;
    repo.git_author_identity().name.clone()
}

/// All commit SHAs reachable from any ref. One `git rev-list --all` invocation —
/// used to drop notes whose target commit has been orphaned (squash/rebase) without
/// per-commit forks.
fn reachable_commits(repo: &Repository) -> HashSet<String> {
    let mut args = repo.global_args_for_exec();
    args.push("rev-list".to_string());
    args.push("--all".to_string());

    let output = match exec_git(&args) {
        Ok(o) => o,
        Err(_) => return HashSet::new(),
    };

    String::from_utf8(output.stdout)
        .unwrap_or_default()
        .lines()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

/// Bulk-fetch committer timestamps for a set of commit SHAs.
/// Returns map of commit_sha -> unix timestamp. Missing commits are silently dropped.
/// Pattern: `git log --no-walk --format=%H %ct sha1 sha2 ...` in a single invocation.
fn commit_dates_for(repo: &Repository, commit_shas: &[String]) -> HashMap<String, i64> {
    if commit_shas.is_empty() {
        return HashMap::new();
    }

    let mut args = repo.global_args_for_exec();
    args.push("log".to_string());
    args.push("--no-walk".to_string());
    args.push("--format=%H %ct".to_string());
    for sha in commit_shas {
        args.push(sha.clone());
    }

    let output = match exec_git(&args) {
        Ok(o) => o,
        Err(_) => return HashMap::new(),
    };

    let stdout = String::from_utf8(output.stdout).unwrap_or_default();
    let mut map = HashMap::with_capacity(commit_shas.len());
    for line in stdout.lines() {
        let mut parts = line.splitn(2, ' ');
        if let (Some(sha), Some(ts)) = (parts.next(), parts.next())
            && let Ok(ts_i) = ts.trim().parse::<i64>()
        {
            map.insert(sha.to_string(), ts_i);
        }
    }
    map
}

/// Parse --since argument (number of days) into Unix timestamp
fn parse_since_arg(days_str: &str) -> Result<i64, GitAiError> {
    let days: u64 = days_str.parse().map_err(|_| {
        GitAiError::Generic(format!(
            "Invalid --since value: '{}'. Expected number of days (e.g., 30)",
            days_str
        ))
    })?;

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    Ok(now - (days as i64 * 86400))
}

/// Format a unix timestamp as a human-readable date (e.g., "Jan 15, 2025")
fn format_timestamp_as_date(timestamp: i64) -> String {
    match Local.timestamp_opt(timestamp, 0) {
        chrono::LocalResult::Single(dt) => dt.format("%b %d, %Y").to_string(),
        _ => format!("@{}", timestamp),
    }
}

/// Calculate accepted_rate from accepted_lines and overridden_lines
fn calculate_accepted_rate(accepted: Option<u32>, overridden: Option<u32>) -> Option<f64> {
    let accepted = accepted.unwrap_or(0) as f64;
    let overridden = overridden.unwrap_or(0) as f64;
    let total = accepted + overridden;
    if total > 0.0 {
        Some(accepted / total)
    } else {
        None
    }
}

/// Upsert a prompt record into prompts.db
#[allow(clippy::too_many_arguments)]
fn upsert_prompt(
    conn: &Connection,
    id: &str,
    tool: &str,
    model: &str,
    external_thread_id: Option<&str>,
    human_author: Option<&str>,
    commit_sha: Option<&str>,
    workdir: Option<&str>,
    total_additions: Option<u32>,
    total_deletions: Option<u32>,
    accepted_lines: Option<u32>,
    overridden_lines: Option<u32>,
    messages: Option<&str>,
    start_time: Option<i64>,
    last_time: Option<i64>,
    created_at: i64,
    updated_at: i64,
) -> Result<(), GitAiError> {
    let accepted_rate = calculate_accepted_rate(accepted_lines, overridden_lines);

    conn.execute(
        r#"
        INSERT INTO prompts (
            id, tool, model, external_thread_id, human_author,
            commit_sha, workdir, total_additions, total_deletions,
            accepted_lines, overridden_lines, accepted_rate, messages,
            start_time, last_time, created_at, updated_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
        ON CONFLICT(id) DO UPDATE SET
            tool = COALESCE(excluded.tool, tool),
            model = COALESCE(excluded.model, model),
            external_thread_id = COALESCE(excluded.external_thread_id, external_thread_id),
            human_author = COALESCE(excluded.human_author, human_author),
            commit_sha = COALESCE(excluded.commit_sha, commit_sha),
            workdir = COALESCE(excluded.workdir, workdir),
            total_additions = COALESCE(total_additions, 0) + COALESCE(excluded.total_additions, 0),
            total_deletions = COALESCE(total_deletions, 0) + COALESCE(excluded.total_deletions, 0),
            accepted_lines = COALESCE(accepted_lines, 0) + COALESCE(excluded.accepted_lines, 0),
            overridden_lines = COALESCE(overridden_lines, 0) + COALESCE(excluded.overridden_lines, 0),
            accepted_rate = CAST(COALESCE(accepted_lines, 0) + COALESCE(excluded.accepted_lines, 0) AS REAL) /
                NULLIF(COALESCE(accepted_lines, 0) + COALESCE(excluded.accepted_lines, 0) +
                       COALESCE(overridden_lines, 0) + COALESCE(excluded.overridden_lines, 0), 0),
            messages = COALESCE(excluded.messages, messages),
            start_time = MIN(COALESCE(start_time, excluded.start_time), COALESCE(excluded.start_time, start_time)),
            last_time = MAX(COALESCE(last_time, excluded.last_time), COALESCE(excluded.last_time, last_time)),
            updated_at = MAX(updated_at, excluded.updated_at)
        "#,
        params![
            id,
            tool,
            model,
            external_thread_id,
            human_author,
            commit_sha,
            workdir,
            total_additions.map(|v| v as i64),
            total_deletions.map(|v| v as i64),
            accepted_lines.map(|v| v as i64),
            overridden_lines.map(|v| v as i64),
            accepted_rate,
            messages,
            start_time,
            last_time,
            created_at,
            updated_at,
        ],
    )
    .map_err(|e| GitAiError::Generic(format!("Failed to upsert prompt: {}", e)))?;

    Ok(())
}

/// Fetch prompts from git notes and upsert into prompts.db.
///
/// Pipeline (squash/rebase resilient — does NOT pre-filter by `git log --since`):
///   1. Enumerate every note in `refs/notes/ai`.
///   2. Build the set of commits reachable from any ref via one `git rev-list --all`.
///   3. Drop notes whose commit is not reachable (orphaned by squash/rebase).
///   4. Bulk-fetch committer dates for the surviving commits.
///   5. Read the surviving note blobs in one `cat-file --batch`.
///   6. For each (prompt_hash, prompt_record) across all live notes, keep the entry
///      whose commit has the *highest* committer date — that's the canonical placement.
///   7. Apply --since against the canonical commit_date (and --author here too).
///   8. Resolve message bodies: inline → CAS deferred → SQLite fallback.
///
/// Returns (new_count, deferred_prompts). Deferred prompts have a CAS URL that matches
/// our configured instance baseurl; everything else has been resolved or upserted inline.
fn fetch_from_git_notes(
    conn: &Connection,
    since_timestamp: i64,
    author: Option<&str>,
    workdir: &str,
    seen_ids: &mut HashSet<String>,
) -> Result<(usize, Vec<DeferredPrompt>), GitAiError> {
    let mut deferred: Vec<DeferredPrompt> = Vec::new();

    let repo = match find_repository_in_path(workdir) {
        Ok(r) => r,
        Err(_) => return Ok((0, deferred)),
    };
    let global_args = repo.global_args_for_exec();

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    // Step 1: enumerate ALL notes in refs/notes/ai
    let all_notes = get_notes_list(&global_args); // Vec<(blob_sha, commit_sha)>
    if all_notes.is_empty() {
        return Ok((0, deferred));
    }

    // Step 2: build the reachable-commit set in one rev-list call
    let reachable = reachable_commits(&repo);

    // Step 3: partition notes into (live, orphaned)
    let mut live_notes: Vec<(String, String)> = Vec::with_capacity(all_notes.len());
    let mut orphan_count = 0usize;
    for (blob_sha, commit_sha) in all_notes {
        if reachable.contains(&commit_sha) {
            live_notes.push((blob_sha, commit_sha));
        } else {
            orphan_count += 1;
        }
    }
    eprintln!("    found {} notes in history", live_notes.len());
    if orphan_count > 0 {
        crate::utils::debug_log(&format!(
            "{} orphaned notes (skipped, commit no longer reachable)",
            orphan_count
        ));
    }
    if live_notes.is_empty() {
        return Ok((0, deferred));
    }

    // Step 4: bulk-fetch commit dates for the surviving commits (deduped)
    let unique_commits: Vec<String> = {
        let mut seen: HashSet<&String> = HashSet::new();
        let mut out: Vec<String> = Vec::new();
        for (_, commit_sha) in &live_notes {
            if seen.insert(commit_sha) {
                out.push(commit_sha.clone());
            }
        }
        out
    };
    let commit_dates = commit_dates_for(&repo, &unique_commits);

    // Step 5: batch-read the surviving note blobs
    let blob_shas: Vec<String> = live_notes.iter().map(|(b, _)| b.clone()).collect();
    let blob_contents = batch_read_blobs(&global_args, &blob_shas);

    // Step 6: walk all live (prompt_hash, prompt_record) pairs and pick the one whose
    // commit has the highest commit_date for each prompt_hash.
    let mut best: HashMap<String, BestPrompt> = HashMap::new();
    for ((_blob_sha, commit_sha), content) in live_notes.iter().zip(blob_contents.iter()) {
        let commit_date = match commit_dates.get(commit_sha) {
            Some(d) => *d,
            None => continue, // commit vanished between rev-list and log; skip safely
        };

        let authorship_log = match
            crate::authorship::authorship_log_serialization::AuthorshipLog::deserialize_from_string(content)
        {
            Ok(log) => log,
            Err(_) => continue, // unparseable note — skip silently
        };

        for (prompt_hash, prompt_record) in &authorship_log.metadata.prompts {
            match best.get(prompt_hash) {
                Some(existing) if existing.commit_date >= commit_date => {
                    // Existing pick is at least as recent — keep it.
                }
                _ => {
                    best.insert(
                        prompt_hash.clone(),
                        BestPrompt {
                            commit_sha: commit_sha.clone(),
                            commit_date,
                            workdir: workdir.to_string(),
                            prompt_record: prompt_record.clone(),
                        },
                    );
                }
            }
        }
    }

    // Step 7 + 8: apply --since/--author and route message-body resolution
    let mut new_count = 0usize;
    for (prompt_hash, picked) in best {
        if picked.commit_date < since_timestamp {
            continue;
        }

        // --author filter against the picked record's human_author
        if let Some(auth_filter) = author {
            match &picked.prompt_record.human_author {
                Some(human_auth) if human_auth.contains(auth_filter) => {}
                _ => continue,
            }
        }

        let is_new = seen_ids.insert(prompt_hash.clone());

        let pr = &picked.prompt_record;
        if !pr.messages.is_empty() {
            // Inline path: full transcript already in the note
            let transcript = AiTranscript {
                messages: pr.messages.clone(),
            };
            let start_time = transcript.first_message_timestamp_unix();
            let last_time = transcript.last_message_timestamp_unix();
            let created_at = start_time.unwrap_or(now);
            let updated_at = last_time.unwrap_or(created_at);
            let messages_json = serde_json::to_string(&pr.messages).ok();

            upsert_prompt(
                conn,
                &prompt_hash,
                &pr.agent_id.tool,
                &pr.agent_id.model,
                Some(&pr.agent_id.id),
                pr.human_author.as_deref(),
                Some(&picked.commit_sha),
                Some(&picked.workdir),
                Some(pr.total_additions),
                Some(pr.total_deletions),
                Some(pr.accepted_lines),
                Some(pr.overriden_lines),
                messages_json.as_deref(),
                start_time,
                last_time,
                created_at,
                updated_at,
            )?;
        } else if let Some(url) = &pr.messages_url {
            // Defer: resolve_cas_messages decides CAS-fetch vs SQLite-fallback based on URL prefix
            deferred.push(DeferredPrompt {
                id: prompt_hash.clone(),
                tool: pr.agent_id.tool.clone(),
                model: pr.agent_id.model.clone(),
                external_thread_id: pr.agent_id.id.clone(),
                human_author: pr.human_author.clone(),
                commit_sha: picked.commit_sha.clone(),
                workdir: picked.workdir.clone(),
                total_additions: pr.total_additions,
                total_deletions: pr.total_deletions,
                accepted_lines: pr.accepted_lines,
                overridden_lines: pr.overriden_lines,
                messages_url: url.clone(),
                created_at: now,
                updated_at: now,
            });
        } else {
            // No inline messages and no URL — try local SQLite as last resort
            let _ = resolve_one_from_local_db(
                conn,
                &prompt_hash,
                &picked.commit_sha,
                &picked.workdir,
                pr,
                now,
            );
        }

        if is_new {
            new_count += 1;
        }
    }

    eprintln!("    {} unique sessions", new_count);

    Ok((new_count, deferred))
}

/// Resolve CAS messages for deferred prompts, then upsert only the ones that succeed.
///
/// Routing rules per deferred prompt:
///   - `messages_url` starts with our configured `{api_base_url}/cas/` → eligible for CAS fetch.
///   - Otherwise (foreign instance) → routed to `resolve_from_local_db` for SQLite fallback.
///
/// CAS path: cache lookup → batched API fetch (requires auth) → upsert.
/// SQLite path: `InternalDatabase::get_prompt(id)` → upsert if a body exists.
///
/// Per-prompt accounting (NOT per-hash) is reported in the summary line.
/// Skip reasons are aggregated and emitted via `debug_log` (one line per category)
/// only when `GIT_AI_DEBUG=1` is set.
fn resolve_cas_messages(conn: &Connection, deferred: &[DeferredPrompt]) {
    use crate::api::client::{ApiClient, ApiContext};
    use crate::api::types::CasMessagesObject;
    use crate::utils::debug_log;

    // Determine the configured instance prefix once (e.g., "https://api.git-ai.com/cas/")
    let instance_prefix = {
        let base = ApiContext::new(None).base_url;
        format!("{}/cas/", base.trim_end_matches('/'))
    };

    // Partition deferred prompts: ones whose URL points at our instance go to CAS;
    // foreign URLs fall through to local SQLite lookup.
    let mut cas_indices: Vec<usize> = Vec::new();
    let mut foreign_indices: Vec<usize> = Vec::new();
    for (i, dp) in deferred.iter().enumerate() {
        if dp.messages_url.starts_with(&instance_prefix) {
            cas_indices.push(i);
        } else {
            foreign_indices.push(i);
        }
    }

    // Build hash → deferred prompt indices for the CAS-eligible subset.
    // The hash is the last path segment of the URL (`{api_base_url}/cas/{hash}`).
    let mut hash_to_indices: HashMap<String, Vec<usize>> = HashMap::new();
    for &i in &cas_indices {
        if let Some(hash) = deferred[i]
            .messages_url
            .rsplit('/')
            .next()
            .filter(|h| !h.is_empty())
        {
            hash_to_indices.entry(hash.to_string()).or_default().push(i);
        }
    }

    eprintln!("  resolving {} transcripts:", deferred.len());

    // Skip-reason aggregation (BTreeMap so debug output is in stable, sorted order).
    let mut skip_reasons: BTreeMap<&'static str, usize> = BTreeMap::new();

    // Foreign-URL prompts get a single shot at the local SQLite store.
    let foreign_resolved = if !foreign_indices.is_empty() {
        resolve_from_local_db(conn, deferred, &foreign_indices)
    } else {
        HashSet::new()
    };
    let foreign_resolved_count = foreign_resolved.len();
    let foreign_skipped = foreign_indices.len() - foreign_resolved_count;
    if foreign_skipped > 0 {
        *skip_reasons
            .entry("wrong hostname (and no body in local sqlite)")
            .or_insert(0) += foreign_skipped;
    }

    // CAS path counters (per-prompt, not per-hash)
    let mut prompts_from_cache = 0usize;
    let mut prompts_from_api = 0usize;
    let mut prompts_from_local = foreign_resolved_count;

    // Tentative CAS skip reason per index — finalised after the local-DB fallback runs.
    let mut cas_initial_failures: HashMap<usize, &'static str> = HashMap::new();

    if !hash_to_indices.is_empty() {
        // Resolved messages keyed by hash, plus a flag for whether the body came from cache.
        let mut resolved_messages: HashMap<String, String> = HashMap::new();
        let mut hashes_from_cache: HashSet<String> = HashSet::new();

        // Step 1: Check cas_cache for each hash
        let mut hashes_needing_fetch: Vec<String> = Vec::new();
        if let Ok(db_mutex) = InternalDatabase::global()
            && let Ok(db_guard) = db_mutex.lock()
        {
            for hash in hash_to_indices.keys() {
                if let Ok(Some(cached_json)) = db_guard.get_cas_cache(hash)
                    && let Ok(cas_obj) = serde_json::from_str::<CasMessagesObject>(&cached_json)
                    && let Ok(messages_json) = serde_json::to_string(&cas_obj.messages)
                {
                    resolved_messages.insert(hash.clone(), messages_json);
                    hashes_from_cache.insert(hash.clone());
                    continue;
                }
                hashes_needing_fetch.push(hash.clone());
            }
        } else {
            hashes_needing_fetch = hash_to_indices.keys().cloned().collect();
        }

        // Step 2: Batch fetch remaining from CAS API (requires auth)
        if !hashes_needing_fetch.is_empty() {
            let context = ApiContext::new(None);
            if context.auth_token.is_none() {
                debug_log("prompts: no auth token, skipping CAS API fetch");
                // All not-cached, CAS-eligible prompts are tentatively "not logged in".
                for hash in &hashes_needing_fetch {
                    if let Some(indices) = hash_to_indices.get(hash) {
                        for &idx in indices {
                            cas_initial_failures.insert(idx, "not logged in");
                        }
                    }
                }
            } else {
                let client = ApiClient::new(context);
                let mut fetched_so_far = 0usize;
                let fetch_total = hashes_needing_fetch.len();

                for chunk in hashes_needing_fetch.chunks(100) {
                    fetched_so_far += chunk.len();
                    eprint!("\r    fetching {}/{}...", fetched_so_far, fetch_total);

                    let hash_refs: Vec<&str> = chunk.iter().map(|s| s.as_str()).collect();
                    let returned_hashes: HashSet<String> =
                        match client.read_ca_prompt_store(&hash_refs) {
                            Ok(response) => {
                                let mut returned = HashSet::new();
                                for result in &response.results {
                                    if result.status == "ok"
                                        && let Some(content) = &result.content
                                    {
                                        let json_str =
                                            serde_json::to_string(content).unwrap_or_default();
                                        if let Ok(cas_obj) = serde_json::from_value::<
                                            CasMessagesObject,
                                        >(
                                            content.clone()
                                        ) && let Ok(messages_json) =
                                            serde_json::to_string(&cas_obj.messages)
                                        {
                                            resolved_messages
                                                .insert(result.hash.clone(), messages_json);
                                            returned.insert(result.hash.clone());
                                            // Cache for future runs
                                            if let Ok(db_mutex) = InternalDatabase::global()
                                                && let Ok(mut db_guard) = db_mutex.lock()
                                            {
                                                let _ = db_guard
                                                    .set_cas_cache(&result.hash, &json_str);
                                            }
                                        }
                                    } else {
                                        let reason = result.error.as_deref().unwrap_or("error");
                                        debug_log(&format!(
                                            "prompts: CAS hash {} returned error: {}",
                                            result.hash, reason
                                        ));
                                    }
                                }
                                returned
                            }
                            Err(e) => {
                                debug_log(&format!("prompts: CAS batch fetch error: {}", e));
                                HashSet::new()
                            }
                        };

                    // Tag any chunk hash that wasn't returned with the appropriate reason.
                    for hash in chunk {
                        if returned_hashes.contains(hash) {
                            continue;
                        }
                        if let Some(indices) = hash_to_indices.get(hash) {
                            for &idx in indices {
                                cas_initial_failures
                                    .insert(idx, "not found in remote prompt store");
                            }
                        }
                    }
                }
                eprintln!(); // finish the \r line
            }
        }

        // Step 3: Upsert deferred prompts that got a body, count per-prompt by source.
        let mut unresolved_cas_indices: Vec<usize> = Vec::new();
        for (hash, indices) in &hash_to_indices {
            let from_cache = hashes_from_cache.contains(hash);
            let messages_json = resolved_messages.get(hash);
            for &idx in indices {
                if let Some(json) = messages_json {
                    let dp = &deferred[idx];
                    if upsert_prompt(
                        conn,
                        &dp.id,
                        &dp.tool,
                        &dp.model,
                        Some(&dp.external_thread_id),
                        dp.human_author.as_deref(),
                        Some(&dp.commit_sha),
                        Some(&dp.workdir),
                        Some(dp.total_additions),
                        Some(dp.total_deletions),
                        Some(dp.accepted_lines),
                        Some(dp.overridden_lines),
                        Some(json),
                        None, // start_time extracted from messages at query time
                        None, // last_time
                        dp.created_at,
                        dp.updated_at,
                    )
                    .is_ok()
                    {
                        if from_cache {
                            prompts_from_cache += 1;
                        } else {
                            prompts_from_api += 1;
                        }
                    }
                } else {
                    unresolved_cas_indices.push(idx);
                }
            }
        }

        // Step 4: CAS misses fall through to local SQLite (so logged-out users still
        // get bodies for prompts they generated locally).
        let cas_local_resolved = if !unresolved_cas_indices.is_empty() {
            resolve_from_local_db(conn, deferred, &unresolved_cas_indices)
        } else {
            HashSet::new()
        };
        prompts_from_local += cas_local_resolved.len();

        // Step 5: Tally final skip reasons for CAS-eligible prompts that *still* lack a body.
        for &idx in &unresolved_cas_indices {
            if cas_local_resolved.contains(&idx) {
                continue;
            }
            let reason = cas_initial_failures
                .get(&idx)
                .copied()
                .unwrap_or("no body in remote prompt store or local sqlite");
            *skip_reasons.entry(reason).or_insert(0) += 1;
        }
    }

    // Summary line — per-prompt accounting.
    let total_written = prompts_from_cache + prompts_from_api + prompts_from_local;
    let total_skipped = deferred.len() - total_written;

    if prompts_from_cache > 0 {
        eprintln!("    {} cached", prompts_from_cache);
    }
    if prompts_from_api > 0 {
        eprintln!("    + {} fetched from prompt store", prompts_from_api);
    }
    if prompts_from_local > 0 {
        eprintln!("    + {} from local sqlite", prompts_from_local);
    }
    if total_skipped > 0 {
        eprintln!("    {} skipped", total_skipped);
    }

    // Debug-only: one line per skip reason, with counts.
    for (reason, count) in &skip_reasons {
        debug_log(&format!("  {} skipped: {}", count, reason));
    }
}

/// Look up message bodies for a slice of deferred prompts in the local SQLite
/// `prompts` table (`InternalDatabase`). Upserts on hit, silently skips on miss.
/// Returns the set of `deferred` indices that were successfully resolved (so callers
/// can attribute skip reasons to the prompts that *weren't* in the returned set).
fn resolve_from_local_db(
    conn: &Connection,
    deferred: &[DeferredPrompt],
    indices: &[usize],
) -> HashSet<usize> {
    let mut resolved = HashSet::new();
    let db_mutex = match InternalDatabase::global() {
        Ok(m) => m,
        Err(_) => return resolved,
    };
    let db_guard = match db_mutex.lock() {
        Ok(g) => g,
        Err(_) => return resolved,
    };

    for &idx in indices {
        let dp = &deferred[idx];
        let record = match db_guard.get_prompt(&dp.id) {
            Ok(Some(r)) => r,
            _ => continue,
        };
        if record.messages.messages.is_empty() {
            continue;
        }
        let messages_json = match serde_json::to_string(&record.messages) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let start_time = record.messages.first_message_timestamp_unix();
        let last_time = record.messages.last_message_timestamp_unix();
        if upsert_prompt(
            conn,
            &dp.id,
            &dp.tool,
            &dp.model,
            Some(&dp.external_thread_id),
            dp.human_author.as_deref(),
            Some(&dp.commit_sha),
            Some(&dp.workdir),
            Some(dp.total_additions),
            Some(dp.total_deletions),
            Some(dp.accepted_lines),
            Some(dp.overridden_lines),
            Some(&messages_json),
            start_time,
            last_time,
            record.created_at,
            record.updated_at,
        )
        .is_ok()
        {
            resolved.insert(idx);
        }
    }
    resolved
}

/// Single-shot SQLite lookup for a prompt with no `messages` and no `messages_url`.
/// Used by `fetch_from_git_notes` for the rare case where a note has neither inline
/// messages nor a CAS URL — we still try the local DB before giving up.
#[allow(clippy::too_many_arguments)]
fn resolve_one_from_local_db(
    conn: &Connection,
    prompt_id: &str,
    commit_sha: &str,
    workdir: &str,
    pr: &PromptRecord,
    fallback_now: i64,
) -> bool {
    let db_mutex = match InternalDatabase::global() {
        Ok(m) => m,
        Err(_) => return false,
    };
    let db_guard = match db_mutex.lock() {
        Ok(g) => g,
        Err(_) => return false,
    };
    let record = match db_guard.get_prompt(prompt_id) {
        Ok(Some(r)) => r,
        _ => return false,
    };
    if record.messages.messages.is_empty() {
        return false;
    }
    let messages_json = match serde_json::to_string(&record.messages) {
        Ok(s) => s,
        Err(_) => return false,
    };
    let start_time = record.messages.first_message_timestamp_unix();
    let last_time = record.messages.last_message_timestamp_unix();
    let created_at = start_time.unwrap_or(fallback_now);
    let updated_at = last_time.unwrap_or(created_at);
    upsert_prompt(
        conn,
        prompt_id,
        &pr.agent_id.tool,
        &pr.agent_id.model,
        Some(&pr.agent_id.id),
        pr.human_author.as_deref(),
        Some(commit_sha),
        Some(workdir),
        Some(pr.total_additions),
        Some(pr.total_deletions),
        Some(pr.accepted_lines),
        Some(pr.overriden_lines),
        Some(&messages_json),
        start_time,
        last_time,
        created_at,
        updated_at,
    )
    .is_ok()
}

/// Get all notes as (note_blob_sha, commit_sha) pairs
fn get_notes_list(global_args: &[String]) -> Vec<(String, String)> {
    let mut args = global_args.to_vec();
    args.push("notes".to_string());
    args.push("--ref=ai".to_string());
    args.push("list".to_string());

    let output = match exec_git(&args) {
        Ok(output) => output,
        Err(_) => return Vec::new(),
    };

    let stdout = String::from_utf8(output.stdout).unwrap_or_default();

    // Parse notes list output: "<note_blob_sha> <commit_sha>"
    let mut mappings = Vec::new();
    for line in stdout.lines() {
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 2 {
            mappings.push((parts[0].to_string(), parts[1].to_string()));
        }
    }

    mappings
}

/// Read multiple blobs efficiently using cat-file --batch
fn batch_read_blobs(global_args: &[String], blob_shas: &[String]) -> Vec<String> {
    if blob_shas.is_empty() {
        return Vec::new();
    }

    let mut args = global_args.to_vec();
    args.push("cat-file".to_string());
    args.push("--batch".to_string());

    // Prepare stdin: one SHA per line
    let stdin_data = blob_shas.join("\n") + "\n";

    let output = match exec_git_stdin(&args, stdin_data.as_bytes()) {
        Ok(o) => o,
        Err(_) => return Vec::new(),
    };

    // Parse batch output
    parse_cat_file_batch_output(&output.stdout)
}

/// Parse the output of git cat-file --batch
///
/// Format:
/// <sha> <type> <size>\n
/// <content bytes>\n
/// (repeat for each object)
fn parse_cat_file_batch_output(data: &[u8]) -> Vec<String> {
    let mut results = Vec::new();
    let mut pos = 0;

    while pos < data.len() {
        // Find the header line ending with \n
        let header_end = match data[pos..].iter().position(|&b| b == b'\n') {
            Some(idx) => pos + idx,
            None => break,
        };

        let header = match std::str::from_utf8(&data[pos..header_end]) {
            Ok(h) => h,
            Err(_) => {
                pos = header_end + 1;
                continue;
            }
        };

        // Parse header: "<sha> <type> <size>" or "<sha> missing"
        let parts: Vec<&str> = header.split_whitespace().collect();
        if parts.len() < 2 {
            pos = header_end + 1;
            continue;
        }

        if parts[1] == "missing" {
            // Object doesn't exist, skip
            pos = header_end + 1;
            continue;
        }

        if parts.len() < 3 {
            pos = header_end + 1;
            continue;
        }

        let size: usize = match parts[2].parse() {
            Ok(s) => s,
            Err(_) => {
                pos = header_end + 1;
                continue;
            }
        };

        // Content starts after the header newline
        let content_start = header_end + 1;
        let content_end = content_start + size;

        if content_end > data.len() {
            break;
        }

        // Try to parse content as UTF-8
        if let Ok(content) = std::str::from_utf8(&data[content_start..content_end]) {
            results.push(content.to_string());
        }

        // Move past content and the trailing newline
        pos = content_end + 1;
    }

    results
}
