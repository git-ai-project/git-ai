//! `git-ai prompts` command suite
//!
//! Creates a local SQLite database (prompts.db) for terminal-friendly prompt analysis.
//! Designed for Claude Code skills and other terminal-based analysis tools.

use crate::authorship::internal_db::InternalDatabase;
use crate::authorship::transcript::AiTranscript;
use crate::error::GitAiError;
use crate::git::find_repository_in_path;
use crate::git::repository::{Repository, exec_git, exec_git_stdin};
use chrono::{Local, TimeZone};
use rusqlite::{Connection, params};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::env;
use std::path::Path;
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

/// Handle populate command (default when no subcommand or with flags)
/// Creates/opens prompts.db and fetches prompts from internal DB and git notes
fn handle_populate(args: &[String]) {
    let mut since_str: Option<String> = None;
    let mut author: Option<String> = None;
    let mut all_authors = false;
    let mut all_repositories = false;

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
            "--all-repositories" => {
                all_repositories = true;
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

    // Get workdir filter (default: current working directory)
    let workdir_filter = if all_repositories {
        None
    } else {
        env::current_dir()
            .ok()
            .map(|p| p.to_string_lossy().to_string())
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
    if let Some(ref workdir) = workdir_filter {
        eprintln!("  workdir: {}", workdir);
    } else {
        eprintln!("  workdir: (all repositories)");
    }

    // Track seen prompt IDs to count only unique prompts
    let mut seen_ids: HashSet<String> = HashSet::new();

    // 1. Fetch from internal DB
    eprintln!("  local prompt store:");
    let workdirs_from_db = match fetch_from_internal_db(
        &conn,
        since_timestamp,
        author_filter.as_deref(),
        workdir_filter.as_deref(),
        &mut seen_ids,
    ) {
        Ok((count, workdirs)) => {
            if workdir_filter.is_some() || workdirs.is_empty() {
                eprintln!("    +{}", count);
            }
            workdirs
        }
        Err(e) => {
            eprintln!("    error - {}", e);
            Vec::new()
        }
    };

    // 2. Fetch from git notes (scans all repos found in internal DB when --all-repositories)
    eprintln!("  git notes:");
    let deferred_prompts = match fetch_from_git_notes(
        &conn,
        since_timestamp,
        author_filter.as_deref(),
        workdir_filter.as_deref(),
        &workdirs_from_db,
        &mut seen_ids,
    ) {
        Ok((count, deferred)) => {
            if workdir_filter.is_some() || workdirs_from_db.is_empty() {
                eprintln!("    +{}", count);
            }
            deferred
        }
        Err(e) => {
            eprintln!("    error - {}", e);
            Vec::new()
        }
    };

    // 3. Fetch CAS messages, then write resolved prompts to DB
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

/// Fetch prompts from internal database and upsert into prompts.db
/// Returns (new_count, list of workdirs found)
fn fetch_from_internal_db(
    conn: &Connection,
    since_timestamp: i64,
    author: Option<&str>,
    workdir: Option<&str>,
    seen_ids: &mut HashSet<String>,
) -> Result<(usize, Vec<String>), GitAiError> {
    let internal_db = InternalDatabase::global()?;
    let db_lock = internal_db
        .lock()
        .map_err(|e| GitAiError::Generic(format!("Failed to lock database: {}", e)))?;

    // Use existing list_prompts method - it supports workdir and since filters
    let prompts = db_lock.list_prompts(workdir, Some(since_timestamp), 100000, 0)?;
    let mut new_count = 0;
    let mut workdir_counts: HashMap<String, usize> = HashMap::new();

    for record in prompts {
        // Filter by author in memory if specified
        if let Some(auth_filter) = author {
            if let Some(ref human_author) = record.human_author {
                if !human_author.contains(auth_filter) {
                    continue;
                }
            } else {
                continue;
            }
        }

        // Track if this is a new prompt
        let is_new = seen_ids.insert(record.id.clone());

        // Track workdir counts (only for new prompts)
        if is_new {
            let wd = record
                .workdir
                .clone()
                .unwrap_or_else(|| "(unknown)".to_string());
            *workdir_counts.entry(wd).or_insert(0) += 1;
            new_count += 1;
        }

        // Skip prompts with no messages — no point writing to analysis DB without content
        if record.messages.messages.is_empty() {
            continue;
        }

        let messages_json = serde_json::to_string(&record.messages).ok();
        let start_time = record.messages.first_message_timestamp_unix();
        let last_time = record.messages.last_message_timestamp_unix();

        upsert_prompt(
            conn,
            &record.id,
            &record.tool,
            &record.model,
            Some(&record.external_thread_id),
            record.human_author.as_deref(),
            record.commit_sha.as_deref(),
            record.workdir.as_deref(),
            record.total_additions,
            record.total_deletions,
            record.accepted_lines,
            record.overridden_lines,
            messages_json.as_deref(),
            start_time,
            last_time,
            record.created_at,
            record.updated_at,
        )?;
    }

    if workdir.is_none() && !workdir_counts.is_empty() {
        eprintln!(
            "    +{} (across {} repositories)",
            new_count,
            workdir_counts.len()
        );
    }

    Ok((new_count, workdir_counts.into_keys().collect()))
}

/// Fetch prompts from git notes and upsert into prompts.db
/// workdirs_from_db: list of workdirs discovered from internal DB (for all-repositories mode)
/// Returns (new_count, deferred_prompts) where deferred_prompts are prompts whose messages
/// need to be fetched from CAS before they can be written to prompts.db
fn fetch_from_git_notes(
    conn: &Connection,
    since_timestamp: i64,
    author: Option<&str>,
    workdir: Option<&str>,
    workdirs_from_db: &[String],
    seen_ids: &mut HashSet<String>,
) -> Result<(usize, Vec<DeferredPrompt>), GitAiError> {
    let mut new_count = 0;
    let mut deferred: Vec<DeferredPrompt> = Vec::new();

    // Determine which workdirs to scan
    let workdirs_to_scan: Vec<String> = if let Some(wd) = workdir {
        vec![wd.to_string()]
    } else {
        // All repositories mode - scan all workdirs found in internal DB
        workdirs_from_db.to_vec()
    };

    if workdirs_to_scan.is_empty() {
        return Ok((0, deferred));
    }

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    for scan_workdir in &workdirs_to_scan {
        let path = Path::new(scan_workdir);
        if !path.exists() {
            continue;
        }

        let repo = match find_repository_in_path(scan_workdir) {
            Ok(r) => r,
            Err(_) => continue,
        };

        // Get commits with notes since timestamp
        let commits_with_notes = get_commits_with_notes_since(&repo, since_timestamp);

        for (commit_sha, note_content) in commits_with_notes {
            // Parse the note content as AuthorshipLog
            if let Ok(authorship_log) =
                crate::authorship::authorship_log_serialization::AuthorshipLog::deserialize_from_string(&note_content)
            {
                for (prompt_hash, prompt_record) in &authorship_log.metadata.prompts {
                    // Apply author filter
                    if let Some(auth_filter) = author {
                        if let Some(human_auth) = &prompt_record.human_author {
                            if !human_auth.contains(auth_filter) {
                                continue;
                            }
                        } else {
                            continue;
                        }
                    }

                    // Track if this is a new prompt
                    let is_new = seen_ids.insert(prompt_hash.clone());

                    if prompt_record.messages.is_empty() {
                        // Messages cleared after CAS upload — defer for CAS resolution
                        if let Some(ref url) = prompt_record.messages_url {
                            deferred.push(DeferredPrompt {
                                id: prompt_hash.clone(),
                                tool: prompt_record.agent_id.tool.clone(),
                                model: prompt_record.agent_id.model.clone(),
                                external_thread_id: prompt_record.agent_id.id.clone(),
                                human_author: prompt_record.human_author.clone(),
                                commit_sha: commit_sha.clone(),
                                workdir: scan_workdir.clone(),
                                total_additions: prompt_record.total_additions,
                                total_deletions: prompt_record.total_deletions,
                                accepted_lines: prompt_record.accepted_lines,
                                overridden_lines: prompt_record.overriden_lines,
                                messages_url: url.clone(),
                                created_at: now,
                                updated_at: now,
                            });
                        }
                        // No messages and no CAS URL — skip entirely
                    } else {
                        // Has messages locally — upsert immediately
                        let transcript = AiTranscript {
                            messages: prompt_record.messages.clone(),
                        };
                        let start_time = transcript.first_message_timestamp_unix();
                        let last_time = transcript.last_message_timestamp_unix();
                        let created_at = start_time.unwrap_or(now);
                        let updated_at = last_time.unwrap_or(created_at);
                        let messages_json = serde_json::to_string(&prompt_record.messages).ok();

                        upsert_prompt(
                            conn,
                            prompt_hash,
                            &prompt_record.agent_id.tool,
                            &prompt_record.agent_id.model,
                            Some(&prompt_record.agent_id.id),
                            prompt_record.human_author.as_deref(),
                            Some(&commit_sha),
                            Some(scan_workdir),
                            Some(prompt_record.total_additions),
                            Some(prompt_record.total_deletions),
                            Some(prompt_record.accepted_lines),
                            Some(prompt_record.overriden_lines),
                            messages_json.as_deref(),
                            start_time,
                            last_time,
                            created_at,
                            updated_at,
                        )?;
                    }

                    if is_new {
                        new_count += 1;
                    }
                }
            }
        }
    }

    Ok((new_count, deferred))
}

/// Resolve CAS messages for deferred prompts, then upsert only the ones that succeed.
/// Checks cas_cache first, then batch-fetches from CAS API (chunks of 100).
/// Silently skips any prompts where resolution fails (auth, 404, network errors).
/// Only makes API calls if the user is logged in; cache lookups work regardless.
fn resolve_cas_messages(conn: &Connection, deferred: &[DeferredPrompt]) {
    use crate::api::client::{ApiClient, ApiContext};
    use crate::api::types::CasMessagesObject;
    use crate::utils::debug_log;

    // Build hash → deferred prompt indices
    // CAS is content-addressed: same hash = same content regardless of which server
    // wrote the URL, so always extract the hash and fetch from current server
    let mut hash_to_indices: HashMap<String, Vec<usize>> = HashMap::new();

    for (i, dp) in deferred.iter().enumerate() {
        // Extract hash from URL (last path segment): .../cas/{hash}
        if let Some(hash) = dp.messages_url.rsplit('/').next().filter(|h| !h.is_empty()) {
            hash_to_indices.entry(hash.to_string()).or_default().push(i);
        }
    }

    if hash_to_indices.is_empty() {
        return;
    }

    let total_to_resolve = hash_to_indices.len();
    eprintln!("  resolving {} transcripts:", total_to_resolve);

    // Resolved messages keyed by hash
    let mut resolved_messages: HashMap<String, String> = HashMap::new();

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
                continue;
            }
            hashes_needing_fetch.push(hash.clone());
        }
    } else {
        hashes_needing_fetch = hash_to_indices.keys().cloned().collect();
    }

    let cached_count = resolved_messages.len();
    if cached_count > 0 {
        eprintln!("    {}/{} from cache", cached_count, total_to_resolve);
    }

    // Step 2: Batch fetch remaining from CAS API (requires auth)
    if !hashes_needing_fetch.is_empty() {
        let context = ApiContext::new(None);
        if context.auth_token.is_none() {
            debug_log("prompts: no auth token, skipping CAS API fetch");
            eprintln!(
                "    {}/{} remaining (skipped, not logged in)",
                hashes_needing_fetch.len(),
                total_to_resolve
            );
        } else {
            let client = ApiClient::new(context);
            let mut fetched_so_far = 0usize;
            let fetch_total = hashes_needing_fetch.len();

            for chunk in hashes_needing_fetch.chunks(100) {
                fetched_so_far += chunk.len();
                eprint!("\r    fetching {}/{}...", fetched_so_far, fetch_total);

                let hash_refs: Vec<&str> = chunk.iter().map(|s| s.as_str()).collect();

                match client.read_ca_prompt_store(&hash_refs) {
                    Ok(response) => {
                        for result in &response.results {
                            if result.status == "ok"
                                && let Some(content) = &result.content
                            {
                                let json_str = serde_json::to_string(content).unwrap_or_default();
                                if let Ok(cas_obj) =
                                    serde_json::from_value::<CasMessagesObject>(content.clone())
                                {
                                    if let Ok(messages_json) =
                                        serde_json::to_string(&cas_obj.messages)
                                    {
                                        resolved_messages
                                            .insert(result.hash.clone(), messages_json);
                                    }
                                    // Cache for future runs
                                    if let Ok(db_mutex) = InternalDatabase::global()
                                        && let Ok(mut db_guard) = db_mutex.lock()
                                    {
                                        let _ = db_guard.set_cas_cache(&result.hash, &json_str);
                                    }
                                }
                            }
                            // Silently skip errors — summary line reports skipped count
                        }
                    }
                    Err(e) => {
                        debug_log(&format!("prompts: CAS batch fetch error: {}", e));
                    }
                }
            }
            eprintln!(); // finish the \r line
        }
    }

    // Step 3: Upsert deferred prompts that got messages
    let mut written = 0usize;
    for (hash, indices) in &hash_to_indices {
        if let Some(messages_json) = resolved_messages.get(hash) {
            for &idx in indices {
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
                    Some(messages_json),
                    None, // start_time extracted from messages at query time
                    None, // last_time
                    dp.created_at,
                    dp.updated_at,
                )
                .is_ok()
                {
                    written += 1;
                }
            }
        }
    }

    let skipped = deferred.len() - written;
    if skipped > 0 {
        eprintln!(
            "    +{} transcripts ({} cached, {} fetched, {} skipped)",
            written,
            cached_count,
            written.saturating_sub(cached_count),
            skipped
        );
    } else {
        eprintln!(
            "    +{} transcripts ({} cached, {} fetched)",
            written,
            cached_count,
            written.saturating_sub(cached_count)
        );
    }
}

/// Get commits with their AI notes since a given time
/// Uses git notes list + cat-file batch (proven pattern from authorship_traversal.rs)
fn get_commits_with_notes_since(repo: &Repository, since_timestamp: i64) -> Vec<(String, String)> {
    let global_args = repo.global_args_for_exec();

    // Step 1: Get commits since timestamp
    let commits_since = get_commits_since(&global_args, since_timestamp);
    if commits_since.is_empty() {
        return Vec::new();
    }
    let commit_set: HashSet<String> = commits_since.into_iter().collect();

    // Step 2: Get all notes mappings (note_blob_sha, commit_sha)
    let note_mappings = get_notes_list(&global_args);

    // Step 3: Filter to notes for commits in our time range
    let filtered: Vec<(String, String)> = note_mappings
        .into_iter()
        .filter(|(_, commit_sha)| commit_set.contains(commit_sha))
        .collect();

    if filtered.is_empty() {
        return Vec::new();
    }

    // Step 4: Batch read the note blobs
    let blob_shas: Vec<String> = filtered.iter().map(|(blob, _)| blob.clone()).collect();
    let contents = batch_read_blobs(&global_args, &blob_shas);

    // Step 5: Pair commit SHAs with note contents
    filtered
        .into_iter()
        .zip(contents)
        .filter(|(_, content)| content.contains('{')) // Only include notes with JSON
        .map(|((_, commit_sha), content)| (commit_sha, content))
        .collect()
}

/// Get all commit SHAs since a timestamp
fn get_commits_since(global_args: &[String], since_timestamp: i64) -> Vec<String> {
    let mut args = global_args.to_vec();
    args.push("log".to_string());
    args.push("--all".to_string());
    args.push("--format=%H".to_string());
    args.push(format!("--since=@{}", since_timestamp));

    let output = match exec_git(&args) {
        Ok(o) => o,
        Err(_) => return Vec::new(),
    };

    String::from_utf8(output.stdout)
        .unwrap_or_default()
        .lines()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
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
