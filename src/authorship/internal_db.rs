use crate::authorship::authorship_log_serialization::generate_short_hash;
use crate::authorship::transcript::AiTranscript;
use crate::authorship::working_log::Checkpoint;
use crate::error::GitAiError;
use crate::utils::debug_log;
use dirs;
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

/// Current schema version (must match MIGRATIONS.len())
const SCHEMA_VERSION: usize = 2;

/// Database migrations - each migration upgrades the schema by one version
/// Migration at index N upgrades from version N to version N+1
const MIGRATIONS: &[&str] = &[
    // Migration 0 -> 1: Initial schema with prompts table
    r#"
    CREATE TABLE prompts (
        id TEXT PRIMARY KEY NOT NULL,
        workdir TEXT,
        tool TEXT NOT NULL,
        model TEXT NOT NULL,
        external_thread_id TEXT NOT NULL,
        messages TEXT NOT NULL,
        commit_sha TEXT,
        agent_metadata TEXT,
        human_author TEXT,
        total_additions INTEGER,
        total_deletions INTEGER,
        accepted_lines INTEGER,
        overridden_lines INTEGER,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL
    );

    CREATE INDEX idx_prompts_tool
        ON prompts(tool);
    CREATE INDEX idx_prompts_external_thread_id
        ON prompts(external_thread_id);
    CREATE INDEX idx_prompts_workdir
        ON prompts(workdir);
    CREATE INDEX idx_prompts_commit_sha
        ON prompts(commit_sha);
    CREATE INDEX idx_prompts_updated_at
        ON prompts(updated_at);
    "#,
    // Migration 1 -> 2: Add CAS sync queue
    r#"
    CREATE TABLE cas_sync_queue (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        hash TEXT NOT NULL UNIQUE,
        data TEXT NOT NULL,
        metadata TEXT NOT NULL DEFAULT '{}',
        status TEXT NOT NULL DEFAULT 'pending' CHECK(status IN ('pending', 'processing')),
        attempts INTEGER NOT NULL DEFAULT 0,
        last_sync_error TEXT,
        last_sync_at INTEGER,
        next_retry_at INTEGER NOT NULL,
        processing_started_at INTEGER,
        created_at INTEGER NOT NULL
    );

    CREATE INDEX idx_cas_sync_queue_status_retry
        ON cas_sync_queue(status, next_retry_at);
    CREATE INDEX idx_cas_sync_queue_hash
        ON cas_sync_queue(hash);
    CREATE INDEX idx_cas_sync_queue_stale_processing
        ON cas_sync_queue(processing_started_at) WHERE status = 'processing';
    "#,
];

/// Global database singleton
static INTERNAL_DB: OnceLock<Mutex<InternalDatabase>> = OnceLock::new();

/// Prompt record for database storage
#[derive(Debug, Clone)]
pub struct PromptDbRecord {
    pub id: String,                                // 16-char short hash
    pub workdir: Option<String>,                   // Repository working directory
    pub tool: String,                              // Agent tool name
    pub model: String,                             // Model name
    pub external_thread_id: String,                // Original agent_id.id
    pub messages: AiTranscript,                    // Transcript
    pub commit_sha: Option<String>,                // Commit SHA (nullable)
    pub agent_metadata: Option<HashMap<String, String>>,  // Agent metadata (transcript paths, etc.)
    pub human_author: Option<String>,              // Human author from checkpoint
    pub total_additions: Option<u32>,              // Line additions from checkpoint stats
    pub total_deletions: Option<u32>,              // Line deletions from checkpoint stats
    pub accepted_lines: Option<u32>,               // Lines accepted in commit (future)
    pub overridden_lines: Option<u32>,             // Lines overridden in commit (future)
    pub created_at: i64,                           // Unix timestamp
    pub updated_at: i64,                           // Unix timestamp
}

impl PromptDbRecord {
    /// Create a new PromptDbRecord from checkpoint data
    pub fn from_checkpoint(
        checkpoint: &Checkpoint,
        workdir: Option<String>,
        commit_sha: Option<String>,
    ) -> Option<Self> {
        let agent_id = checkpoint.agent_id.as_ref()?;
        let transcript = checkpoint.transcript.as_ref()?;

        let short_hash = generate_short_hash(&agent_id.id, &agent_id.tool);

        // Use first message timestamp for created_at, fall back to checkpoint timestamp
        let created_at = transcript
            .first_message_timestamp_unix()
            .unwrap_or(checkpoint.timestamp as i64);

        // Use last message timestamp for updated_at, fall back to checkpoint timestamp
        let updated_at = transcript
            .last_message_timestamp_unix()
            .unwrap_or(checkpoint.timestamp as i64);

        Some(Self {
            id: short_hash,
            workdir,
            tool: agent_id.tool.clone(),
            model: agent_id.model.clone(),
            external_thread_id: agent_id.id.clone(),
            messages: transcript.clone(),
            commit_sha,
            agent_metadata: checkpoint.agent_metadata.clone(),
            human_author: Some(checkpoint.author.clone()),
            total_additions: Some(checkpoint.line_stats.additions),
            total_deletions: Some(checkpoint.line_stats.deletions),
            accepted_lines: None,      // Not yet calculated
            overridden_lines: None,    // Not yet calculated
            created_at,
            updated_at,
        })
    }

    /// Convert PromptDbRecord to PromptRecord
    pub fn to_prompt_record(&self) -> crate::authorship::authorship_log::PromptRecord {
        use crate::authorship::authorship_log::PromptRecord;
        use crate::authorship::working_log::AgentId;

        PromptRecord {
            agent_id: AgentId {
                tool: self.tool.clone(),
                id: self.external_thread_id.clone(),
                model: self.model.clone(),
            },
            human_author: self.human_author.clone(),
            messages: self.messages.messages.clone(),
            total_additions: self.total_additions.unwrap_or(0),
            total_deletions: self.total_deletions.unwrap_or(0),
            accepted_lines: self.accepted_lines.unwrap_or(0),
            overriden_lines: self.overridden_lines.unwrap_or(0),
            messages_url: None,
        }
    }

    /// Extract first user message snippet, truncated to max_length
    pub fn first_message_snippet(&self, max_length: usize) -> String {
        use crate::authorship::transcript::Message;

        // Truncate at a valid UTF-8 character boundary (like floor_char_boundary, but stable)
        fn truncate_to_boundary(s: &str, max_bytes: usize) -> &str {
            if max_bytes >= s.len() {
                return s;
            }
            let mut boundary = max_bytes;
            while !s.is_char_boundary(boundary) && boundary > 0 {
                boundary -= 1;
            }
            &s[..boundary]
        }

        for message in &self.messages.messages {
            if let Message::User { text, .. } = message {
                if text.len() <= max_length {
                    return text.clone();
                } else {
                    return format!("{}...", truncate_to_boundary(text, max_length));
                }
            }
        }

        // Fallback: if no user message, try first assistant message
        for message in &self.messages.messages {
            if let Message::Assistant { text, .. } = message {
                if text.len() <= max_length {
                    return text.clone();
                } else {
                    return format!("{}...", truncate_to_boundary(text, max_length));
                }
            }
        }

        "(No messages)".to_string()
    }

    /// Count total messages in transcript
    pub fn message_count(&self) -> usize {
        self.messages.messages.len()
    }

    /// Format relative time ("1 day ago", "5 days ago", etc.)
    pub fn relative_time(&self) -> String {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        let diff = now - self.updated_at;

        if diff < 60 {
            return format!("{} second{} ago", diff, if diff == 1 { "" } else { "s" });
        }

        let minutes = diff / 60;
        if minutes < 60 {
            return format!("{} minute{} ago", minutes, if minutes == 1 { "" } else { "s" });
        }

        let hours = minutes / 60;
        if hours < 24 {
            return format!("{} hour{} ago", hours, if hours == 1 { "" } else { "s" });
        }

        let days = hours / 24;
        if days < 7 {
            return format!("{} day{} ago", days, if days == 1 { "" } else { "s" });
        }

        let weeks = days / 7;
        if weeks < 4 {
            return format!("{} week{} ago", weeks, if weeks == 1 { "" } else { "s" });
        }

        let months = days / 30;
        if months < 12 {
            return format!("{} month{} ago", months, if months == 1 { "" } else { "s" });
        }

        let years = days / 365;
        format!("{} year{} ago", years, if years == 1 { "" } else { "s" })
    }
}

/// CAS sync queue record
#[derive(Debug, Clone)]
pub struct CasSyncRecord {
    pub id: i64,
    pub hash: String,
    pub data: String,
    pub metadata: HashMap<String, String>,
    pub attempts: u32,
}

/// Database wrapper for internal git-ai storage
pub struct InternalDatabase {
    conn: Connection,
    _db_path: PathBuf,
}

impl InternalDatabase {
    /// Get or initialize the global database
    pub fn global() -> Result<&'static Mutex<InternalDatabase>, GitAiError> {
        // Use get_or_init (stable) instead of get_or_try_init (unstable)
        // Errors during initialization will be logged and returned as Err
        let db_mutex = INTERNAL_DB.get_or_init(|| {
            match Self::new() {
                Ok(db) => Mutex::new(db),
                Err(e) => {
                    // Log error during initialization
                    eprintln!("[Error] Failed to initialize internal database: {}", e);
                    crate::observability::log_error(
                        &e,
                        Some(serde_json::json!({"function": "InternalDatabase::global"}))
                    );
                    // Create a dummy connection that will fail on any operation
                    // This allows the program to continue even if DB init fails
                    let temp_path = std::env::temp_dir().join("git-ai-db-failed");
                    let conn = Connection::open(&temp_path).expect("Failed to create temp DB");
                    Mutex::new(InternalDatabase {
                        conn,
                        _db_path: temp_path,
                    })
                }
            }
        });

        Ok(db_mutex)
    }

    /// Start database initialization in a background thread.
    /// This allows the main thread to continue with other work while
    /// the database connection and schema migrations are prepared.
    ///
    /// The OnceLock guarantees thread-safe initialization - if warmup
    /// completes before any caller needs the DB, they get instant access.
    /// If a caller needs DB before warmup completes, they wait normally.
    pub fn warmup() {
        std::thread::spawn(|| {
            if let Err(e) = Self::global() {
                debug_log(&format!("DB warmup failed: {}", e));
            }
        });
    }

    /// Create a new database connection
    fn new() -> Result<Self, GitAiError> {
        let db_path = Self::database_path()?;

        // Ensure parent directory exists
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Open with WAL mode and performance optimizations
        let conn = Connection::open(&db_path)?;
        conn.execute_batch(
            r#"
            PRAGMA journal_mode=WAL;
            PRAGMA synchronous=NORMAL;
            PRAGMA cache_size=-2000;
            PRAGMA temp_store=MEMORY;
            "#,
        )?;

        let mut db = Self {
            conn,
            _db_path: db_path,
        };
        db.initialize_schema()?;

        Ok(db)
    }

    /// Get database path: ~/.git-ai/internal/db
    /// In test mode, can be overridden via GIT_AI_TEST_DB_PATH environment variable
    fn database_path() -> Result<PathBuf, GitAiError> {
        // Allow test override via environment variable
        #[cfg(any(test, feature = "test-support"))]
        if let Ok(test_path) = std::env::var("GIT_AI_TEST_DB_PATH") {
            return Ok(PathBuf::from(test_path));
        }

        let home = dirs::home_dir()
            .ok_or_else(|| GitAiError::Generic("Could not determine home directory".to_string()))?;
        Ok(home
            .join(".git-ai")
            .join("internal")
            .join("db"))
    }

    /// Initialize schema and handle migrations
    /// This is the ONLY place where schema changes should be made
    /// Failures are FATAL - the program cannot continue without a valid database
    fn initialize_schema(&mut self) -> Result<(), GitAiError> {
        // FAST PATH: Check if database is already at current version
        // This avoids expensive schema operations on every process start
        let version_check: Result<usize, _> = self.conn.query_row(
            "SELECT value FROM schema_metadata WHERE key = 'version'",
            [],
            |row| {
                let version_str: String = row.get(0)?;
                version_str
                    .parse::<usize>()
                    .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
            },
        );

        if let Ok(current_version) = version_check {
            if current_version == SCHEMA_VERSION {
                // Database is up-to-date, no migrations needed
                return Ok(());
            }
            if current_version > SCHEMA_VERSION {
                return Err(GitAiError::Generic(format!(
                    "Database schema version {} is newer than supported version {}. \
                     Please upgrade git-ai to the latest version.",
                    current_version, SCHEMA_VERSION
                )));
            }
            // Fall through to apply missing migrations (current_version < SCHEMA_VERSION)
        }
        // If query failed, table doesn't exist - proceed with full initialization

        // Step 1: Create schema_metadata table (this is the only table we create directly)
        self.conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS schema_metadata (
                key TEXT PRIMARY KEY NOT NULL,
                value TEXT NOT NULL
            );
            "#,
        )?;

        // Step 2: Get current schema version (0 if brand new database)
        let current_version: usize = self
            .conn
            .query_row(
                "SELECT value FROM schema_metadata WHERE key = 'version'",
                [],
                |row| {
                    let version_str: String = row.get(0)?;
                    version_str
                        .parse::<usize>()
                        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
                },
            )
            .unwrap_or(0); // Default to version 0 for new databases

        // Step 3: Apply all missing migrations sequentially
        for target_version in current_version..SCHEMA_VERSION {
            debug_log(&format!(
                "[Migration] Upgrading database from version {} to {}",
                target_version,
                target_version + 1
            ));

            // Apply the migration (FATAL on error)
            self.apply_migration(target_version)?;

            // Update version in database
            if target_version == 0 {
                // First migration (0->1) - insert version
                self.conn.execute(
                    "INSERT INTO schema_metadata (key, value) VALUES ('version', ?1)",
                    params![(target_version + 1).to_string()],
                )?;
            } else {
                // Subsequent migrations (1->2, etc.) - update version
                self.conn.execute(
                    "UPDATE schema_metadata SET value = ?1 WHERE key = 'version'",
                    params![(target_version + 1).to_string()],
                )?;
            }

            debug_log(&format!(
                "[Migration] Successfully upgraded to version {}",
                target_version + 1
            ));
        }

        // Step 5: Verify final version matches expected
        let final_version: usize = self
            .conn
            .query_row(
                "SELECT value FROM schema_metadata WHERE key = 'version'",
                [],
                |row| {
                    let version_str: String = row.get(0)?;
                    version_str
                        .parse::<usize>()
                        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
                },
            )?;

        if final_version != SCHEMA_VERSION {
            return Err(GitAiError::Generic(format!(
                "Migration failed: expected version {} but got version {}",
                SCHEMA_VERSION, final_version
            )));
        }

        Ok(())
    }

    /// Apply a single migration
    /// Migration failures are FATAL - the program cannot continue with a partially migrated database
    fn apply_migration(&mut self, from_version: usize) -> Result<(), GitAiError> {
        if from_version >= MIGRATIONS.len() {
            return Err(GitAiError::Generic(format!(
                "No migration defined for version {} -> {}",
                from_version,
                from_version + 1
            )));
        }

        let migration_sql = MIGRATIONS[from_version];

        // Execute migration in a transaction for atomicity
        let tx = self.conn.transaction()?;
        tx.execute_batch(migration_sql)?;
        tx.commit()?;

        Ok(())
    }

    /// Upsert a prompt record
    pub fn upsert_prompt(&mut self, record: &PromptDbRecord) -> Result<(), GitAiError> {
        let messages_json = serde_json::to_string(&record.messages)?;
        let metadata_json = record
            .agent_metadata
            .as_ref()
            .and_then(|m| serde_json::to_string(m).ok());

        self.conn.execute(
            r#"
            INSERT INTO prompts (
                id, workdir, tool, model, external_thread_id,
                messages, commit_sha, agent_metadata, human_author,
                total_additions, total_deletions, accepted_lines,
                overridden_lines, created_at, updated_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
            ON CONFLICT(id) DO UPDATE SET
                workdir = excluded.workdir,
                model = excluded.model,
                messages = excluded.messages,
                commit_sha = excluded.commit_sha,
                agent_metadata = excluded.agent_metadata,
                human_author = excluded.human_author,
                total_additions = excluded.total_additions,
                total_deletions = excluded.total_deletions,
                accepted_lines = excluded.accepted_lines,
                overridden_lines = excluded.overridden_lines,
                updated_at = excluded.updated_at
            "#,
            params![
                record.id,
                record.workdir,
                record.tool,
                record.model,
                record.external_thread_id,
                messages_json,
                record.commit_sha,
                metadata_json,
                record.human_author,
                record.total_additions,
                record.total_deletions,
                record.accepted_lines,
                record.overridden_lines,
                record.created_at,
                record.updated_at,
            ],
        )?;

        Ok(())
    }

    /// Batch upsert multiple prompts (for post-commit)
    pub fn batch_upsert_prompts(&mut self, records: &[PromptDbRecord]) -> Result<(), GitAiError> {
        if records.is_empty() {
            return Ok(());
        }

        let tx = self.conn.transaction()?;

        {
            // Prepare statement once and reuse for all records (much faster than parsing SQL each time)
            let mut stmt = tx.prepare_cached(
                r#"
                INSERT INTO prompts (
                    id, workdir, tool, model, external_thread_id,
                    messages, commit_sha, agent_metadata, human_author,
                    total_additions, total_deletions, accepted_lines,
                    overridden_lines, created_at, updated_at
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
                ON CONFLICT(id) DO UPDATE SET
                    workdir = excluded.workdir,
                    model = excluded.model,
                    messages = excluded.messages,
                    commit_sha = excluded.commit_sha,
                    agent_metadata = excluded.agent_metadata,
                    human_author = excluded.human_author,
                    total_additions = excluded.total_additions,
                    total_deletions = excluded.total_deletions,
                    accepted_lines = excluded.accepted_lines,
                    overridden_lines = excluded.overridden_lines,
                    updated_at = excluded.updated_at
                "#,
            )?;

            for record in records {
                let messages_json = serde_json::to_string(&record.messages)?;
                let metadata_json = record
                    .agent_metadata
                    .as_ref()
                    .and_then(|m| serde_json::to_string(m).ok());

                stmt.execute(params![
                    record.id,
                    record.workdir,
                    record.tool,
                    record.model,
                    record.external_thread_id,
                    messages_json,
                    record.commit_sha,
                    metadata_json,
                    record.human_author,
                    record.total_additions,
                    record.total_deletions,
                    record.accepted_lines,
                    record.overridden_lines,
                    record.created_at,
                    record.updated_at,
                ])?;
            }
        }

        tx.commit()?;
        Ok(())
    }

    /// Get a prompt by ID
    pub fn get_prompt(&self, id: &str) -> Result<Option<PromptDbRecord>, GitAiError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, workdir, tool, model, external_thread_id, messages,
                    commit_sha, agent_metadata, human_author,
                    total_additions, total_deletions, accepted_lines,
                    overridden_lines, created_at, updated_at
             FROM prompts WHERE id = ?1"
        )?;

        let result = stmt.query_row(params![id], |row| {
            let messages_json: String = row.get(5)?;
            let messages: AiTranscript = serde_json::from_str(&messages_json).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    5,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?;

            let agent_metadata: Option<HashMap<String, String>> = row
                .get::<_, Option<String>>(7)?
                .and_then(|json| serde_json::from_str(&json).ok());

            Ok(PromptDbRecord {
                id: row.get(0)?,
                workdir: row.get(1)?,
                tool: row.get(2)?,
                model: row.get(3)?,
                external_thread_id: row.get(4)?,
                messages,
                commit_sha: row.get(6)?,
                agent_metadata,
                human_author: row.get(8)?,
                total_additions: row.get(9)?,
                total_deletions: row.get(10)?,
                accepted_lines: row.get(11)?,
                overridden_lines: row.get(12)?,
                created_at: row.get(13)?,
                updated_at: row.get(14)?,
            })
        });

        match result {
            Ok(record) => Ok(Some(record)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Get all prompts for a given commit (future use)
    #[allow(dead_code)]
    pub fn get_prompts_by_commit(
        &self,
        commit_sha: &str,
    ) -> Result<Vec<PromptDbRecord>, GitAiError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, workdir, tool, model, external_thread_id, messages,
                    commit_sha, agent_metadata, human_author,
                    total_additions, total_deletions, accepted_lines,
                    overridden_lines, created_at, updated_at
             FROM prompts WHERE commit_sha = ?1"
        )?;

        let rows = stmt.query_map(params![commit_sha], |row| {
            let messages_json: String = row.get(5)?;
            let messages: AiTranscript = serde_json::from_str(&messages_json).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    5,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?;

            let agent_metadata: Option<HashMap<String, String>> = row
                .get::<_, Option<String>>(7)?
                .and_then(|json| serde_json::from_str(&json).ok());

            Ok(PromptDbRecord {
                id: row.get(0)?,
                workdir: row.get(1)?,
                tool: row.get(2)?,
                model: row.get(3)?,
                external_thread_id: row.get(4)?,
                messages,
                commit_sha: row.get(6)?,
                agent_metadata,
                human_author: row.get(8)?,
                total_additions: row.get(9)?,
                total_deletions: row.get(10)?,
                accepted_lines: row.get(11)?,
                overridden_lines: row.get(12)?,
                created_at: row.get(13)?,
                updated_at: row.get(14)?,
            })
        })?;

        let mut records = Vec::new();
        for row in rows {
            records.push(row?);
        }

        Ok(records)
    }

    /// List prompts with optional workdir and since filters, ordered by updated_at DESC
    pub fn list_prompts(
        &self,
        workdir: Option<&str>,
        since: Option<i64>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<PromptDbRecord>, GitAiError> {
        let (query, params): (String, Vec<Box<dyn rusqlite::ToSql>>) = match (workdir, since) {
            (Some(wd), Some(ts)) => (
                "SELECT id, workdir, tool, model, external_thread_id, messages,
                        commit_sha, agent_metadata, human_author,
                        total_additions, total_deletions, accepted_lines,
                        overridden_lines, created_at, updated_at
                 FROM prompts WHERE workdir = ?1 AND updated_at >= ?2 ORDER BY updated_at DESC LIMIT ?3 OFFSET ?4".to_string(),
                vec![Box::new(wd.to_string()), Box::new(ts), Box::new(limit as i64), Box::new(offset as i64)],
            ),
            (Some(wd), None) => (
                "SELECT id, workdir, tool, model, external_thread_id, messages,
                        commit_sha, agent_metadata, human_author,
                        total_additions, total_deletions, accepted_lines,
                        overridden_lines, created_at, updated_at
                 FROM prompts WHERE workdir = ?1 ORDER BY updated_at DESC LIMIT ?2 OFFSET ?3".to_string(),
                vec![Box::new(wd.to_string()), Box::new(limit as i64), Box::new(offset as i64)],
            ),
            (None, Some(ts)) => (
                "SELECT id, workdir, tool, model, external_thread_id, messages,
                        commit_sha, agent_metadata, human_author,
                        total_additions, total_deletions, accepted_lines,
                        overridden_lines, created_at, updated_at
                 FROM prompts WHERE updated_at >= ?1 ORDER BY updated_at DESC LIMIT ?2 OFFSET ?3".to_string(),
                vec![Box::new(ts), Box::new(limit as i64), Box::new(offset as i64)],
            ),
            (None, None) => (
                "SELECT id, workdir, tool, model, external_thread_id, messages,
                        commit_sha, agent_metadata, human_author,
                        total_additions, total_deletions, accepted_lines,
                        overridden_lines, created_at, updated_at
                 FROM prompts ORDER BY updated_at DESC LIMIT ?1 OFFSET ?2".to_string(),
                vec![Box::new(limit as i64), Box::new(offset as i64)],
            ),
        };

        let mut stmt = self.conn.prepare(&query)?;
        let params_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();

        let rows = stmt.query_map(&params_refs[..], |row| {
            let messages_json: String = row.get(5)?;
            let messages: AiTranscript = serde_json::from_str(&messages_json).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    5,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?;

            let agent_metadata: Option<HashMap<String, String>> = row
                .get::<_, Option<String>>(7)?
                .and_then(|json| serde_json::from_str(&json).ok());

            Ok(PromptDbRecord {
                id: row.get(0)?,
                workdir: row.get(1)?,
                tool: row.get(2)?,
                model: row.get(3)?,
                external_thread_id: row.get(4)?,
                messages,
                commit_sha: row.get(6)?,
                agent_metadata,
                human_author: row.get(8)?,
                total_additions: row.get(9)?,
                total_deletions: row.get(10)?,
                accepted_lines: row.get(11)?,
                overridden_lines: row.get(12)?,
                created_at: row.get(13)?,
                updated_at: row.get(14)?,
            })
        })?;

        let mut records = Vec::new();
        for row in rows {
            records.push(row?);
        }

        Ok(records)
    }

    /// Search prompts by message content with optional workdir filter
    pub fn search_prompts(
        &self,
        search_query: &str,
        workdir: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<PromptDbRecord>, GitAiError> {
        let search_pattern = format!("%{}%", search_query);

        let (query, params): (String, Vec<Box<dyn rusqlite::ToSql>>) = match workdir {
            Some(wd) => (
                "SELECT id, workdir, tool, model, external_thread_id, messages,
                        commit_sha, agent_metadata, human_author,
                        total_additions, total_deletions, accepted_lines,
                        overridden_lines, created_at, updated_at
                 FROM prompts WHERE messages LIKE ?1 AND workdir = ?2 ORDER BY updated_at DESC LIMIT ?3 OFFSET ?4".to_string(),
                vec![Box::new(search_pattern), Box::new(wd.to_string()), Box::new(limit as i64), Box::new(offset as i64)],
            ),
            None => (
                "SELECT id, workdir, tool, model, external_thread_id, messages,
                        commit_sha, agent_metadata, human_author,
                        total_additions, total_deletions, accepted_lines,
                        overridden_lines, created_at, updated_at
                 FROM prompts WHERE messages LIKE ?1 ORDER BY updated_at DESC LIMIT ?2 OFFSET ?3".to_string(),
                vec![Box::new(search_pattern), Box::new(limit as i64), Box::new(offset as i64)],
            ),
        };

        let mut stmt = self.conn.prepare(&query)?;
        let params_refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();

        let rows = stmt.query_map(&params_refs[..], |row| {
            let messages_json: String = row.get(5)?;
            let messages: AiTranscript = serde_json::from_str(&messages_json).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    5,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?;

            let agent_metadata: Option<HashMap<String, String>> = row
                .get::<_, Option<String>>(7)?
                .and_then(|json| serde_json::from_str(&json).ok());

            Ok(PromptDbRecord {
                id: row.get(0)?,
                workdir: row.get(1)?,
                tool: row.get(2)?,
                model: row.get(3)?,
                external_thread_id: row.get(4)?,
                messages,
                commit_sha: row.get(6)?,
                agent_metadata,
                human_author: row.get(8)?,
                total_additions: row.get(9)?,
                total_deletions: row.get(10)?,
                accepted_lines: row.get(11)?,
                overridden_lines: row.get(12)?,
                created_at: row.get(13)?,
                updated_at: row.get(14)?,
            })
        })?;

        let mut records = Vec::new();
        for row in rows {
            records.push(row?);
        }

        Ok(records)
    }

    /// Enqueue a CAS object for syncing
    ///
    /// Takes raw JSON data, canonicalizes it (RFC 8785), computes SHA256 hash,
    /// and stores both in the queue.
    ///
    /// Returns the hash of the canonicalized content.
    pub fn enqueue_cas_object(
        &mut self,
        json_data: &serde_json::Value,
        metadata: Option<&HashMap<String, String>>,
    ) -> Result<String, GitAiError> {
        use sha2::{Digest, Sha256};

        // Canonicalize JSON (RFC 8785)
        let canonical = serde_json_canonicalizer::to_string(json_data)
            .map_err(|e| GitAiError::Generic(format!("Failed to canonicalize JSON: {}", e)))?;

        // Hash the canonicalized content
        let mut hasher = Sha256::new();
        hasher.update(canonical.as_bytes());
        let hash = format!("{:x}", hasher.finalize());

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        let metadata_json = serde_json::to_string(metadata.unwrap_or(&HashMap::new()))?;

        self.conn.execute(
            r#"
            INSERT OR IGNORE INTO cas_sync_queue (
                hash, data, metadata, status, attempts, next_retry_at, created_at
            ) VALUES (?1, ?2, ?3, 'pending', 0, ?4, ?4)
            "#,
            params![hash, canonical, metadata_json, now],
        )?;

        Ok(hash)
    }

    /// Dequeue a batch of CAS objects for syncing (with lock acquisition)
    pub fn dequeue_cas_batch(&mut self, batch_size: usize) -> Result<Vec<CasSyncRecord>, GitAiError> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        // Step 1: Recover stale locks (processing for >10 minutes)
        let stale_threshold = now - 600; // 10 minutes
        self.conn.execute(
            r#"
            UPDATE cas_sync_queue
            SET status = 'pending', processing_started_at = NULL
            WHERE status = 'processing'
              AND processing_started_at < ?1
            "#,
            params![stale_threshold],
        )?;

        // Step 2: Atomically lock and fetch batch using UPDATE...RETURNING
        // Note: SQLite's UPDATE...RETURNING is atomic
        let mut stmt = self.conn.prepare(
            r#"
            UPDATE cas_sync_queue
            SET status = 'processing', processing_started_at = ?1
            WHERE id IN (
                SELECT id FROM cas_sync_queue
                WHERE status = 'pending'
                  AND next_retry_at <= ?2
                  AND attempts < 6
                ORDER BY next_retry_at
                LIMIT ?3
            )
            RETURNING id, hash, data, metadata, attempts
            "#,
        )?;

        let rows = stmt.query_map(params![now, now, batch_size], |row| {
            let metadata_json: String = row.get(3)?;
            let metadata: HashMap<String, String> = serde_json::from_str(&metadata_json).unwrap_or_default();
            let hash: String = row.get(1)?;
            let data: String = row.get(2)?;
            Ok(CasSyncRecord {
                id: row.get(0)?,
                hash,
                data,
                attempts: row.get(4)?,
                metadata,
            })
        })?;

        let mut records = Vec::new();
        for row in rows {
            records.push(row?);
        }

        Ok(records)
    }

    /// Delete a CAS sync record (on successful sync)
    pub fn delete_cas_sync_record(&mut self, id: i64) -> Result<(), GitAiError> {
        self.conn.execute(
            "DELETE FROM cas_sync_queue WHERE id = ?",
            params![id],
        )?;
        Ok(())
    }

    /// Update CAS sync record on failure (release lock, increment attempts, set next retry)
    pub fn update_cas_sync_failure(&mut self, id: i64, error: &str) -> Result<(), GitAiError> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        // Get current attempts count to calculate next retry
        let attempts: u32 = self.conn.query_row(
            "SELECT attempts FROM cas_sync_queue WHERE id = ?",
            params![id],
            |row| row.get(0),
        )?;

        let next_retry = calculate_next_retry(attempts + 1, now);

        self.conn.execute(
            r#"
            UPDATE cas_sync_queue
            SET status = 'pending',
                processing_started_at = NULL,
                attempts = attempts + 1,
                last_sync_error = ?1,
                last_sync_at = ?2,
                next_retry_at = ?3
            WHERE id = ?4
            "#,
            params![error, now, next_retry, id],
        )?;

        Ok(())
    }
}

/// Calculate next retry timestamp based on attempt number
fn calculate_next_retry(attempts: u32, now: i64) -> i64 {
    let delay_seconds = match attempts {
        1 => 5 * 60,          // 5 minutes
        2 => 30 * 60,         // 30 minutes
        3 => 2 * 60 * 60,     // 2 hours
        4 => 6 * 60 * 60,     // 6 hours
        5 => 12 * 60 * 60,    // 12 hours
        _ => 24 * 60 * 60,    // 24 hours (attempts >= 6)
    };
    now + delay_seconds
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authorship::transcript::Message;
    use tempfile::TempDir;

    fn create_test_db() -> (InternalDatabase, TempDir) {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");

        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch("PRAGMA journal_mode=WAL;").unwrap();

        let mut db = InternalDatabase {
            conn,
            _db_path: db_path.clone(),
        };
        db.initialize_schema().unwrap();

        (db, temp_dir)
    }

    fn create_test_record() -> PromptDbRecord {
        let mut transcript = AiTranscript::new();
        transcript.add_message(Message::User {
            text: "Test message".to_string(),
            timestamp: None,
        });

        PromptDbRecord {
            id: "abc123def456gh78".to_string(),
            workdir: Some("/test/repo".to_string()),
            tool: "cursor".to_string(),
            model: "claude-sonnet-4.5".to_string(),
            external_thread_id: "test-session-123".to_string(),
            messages: transcript,
            commit_sha: None,
            agent_metadata: None,
            human_author: Some("John Doe".to_string()),
            total_additions: Some(10),
            total_deletions: Some(5),
            accepted_lines: None,
            overridden_lines: None,
            created_at: 1234567890,
            updated_at: 1234567890,
        }
    }

    #[test]
    fn test_initialize_schema() {
        let (db, _temp_dir) = create_test_db();

        // Verify tables exist
        let count: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='prompts'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);

        // Verify schema_metadata exists
        let version: String = db
            .conn
            .query_row(
                "SELECT value FROM schema_metadata WHERE key = 'version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, "2");
    }

    #[test]
    fn test_upsert_prompt() {
        let (mut db, _temp_dir) = create_test_db();
        let record = create_test_record();

        // Insert
        db.upsert_prompt(&record).unwrap();

        // Verify inserted
        let retrieved = db.get_prompt(&record.id).unwrap().unwrap();
        assert_eq!(retrieved.id, record.id);
        assert_eq!(retrieved.tool, record.tool);
        assert_eq!(retrieved.model, record.model);
        assert_eq!(retrieved.external_thread_id, record.external_thread_id);

        // Update
        let mut updated_record = record.clone();
        updated_record.model = "claude-opus-4".to_string();
        updated_record.commit_sha = Some("commit123".to_string());
        updated_record.updated_at = 1234567900;

        db.upsert_prompt(&updated_record).unwrap();

        // Verify updated
        let retrieved = db.get_prompt(&updated_record.id).unwrap().unwrap();
        assert_eq!(retrieved.model, "claude-opus-4");
        assert_eq!(retrieved.commit_sha, Some("commit123".to_string()));
        assert_eq!(retrieved.updated_at, 1234567900);
    }

    #[test]
    fn test_batch_upsert_prompts() {
        let (mut db, _temp_dir) = create_test_db();

        let mut records = Vec::new();
        for i in 0..5 {
            let mut record = create_test_record();
            record.id = format!("prompt{:016}", i);
            record.external_thread_id = format!("session-{}", i);
            records.push(record);
        }

        // Batch insert
        db.batch_upsert_prompts(&records).unwrap();

        // Verify all inserted
        for record in &records {
            let retrieved = db.get_prompt(&record.id).unwrap();
            assert!(retrieved.is_some());
            assert_eq!(retrieved.unwrap().external_thread_id, record.external_thread_id);
        }
    }

    #[test]
    fn test_get_prompt_not_found() {
        let (db, _temp_dir) = create_test_db();
        let result = db.get_prompt("nonexistent").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_get_prompts_by_commit() {
        let (mut db, _temp_dir) = create_test_db();

        let commit_sha = "abc123commit";

        // Create multiple records with same commit_sha
        let mut records = Vec::new();
        for i in 0..3 {
            let mut record = create_test_record();
            record.id = format!("prompt{:016}", i);
            record.commit_sha = Some(commit_sha.to_string());
            records.push(record);
        }

        // Insert records
        db.batch_upsert_prompts(&records).unwrap();

        // Query by commit
        let retrieved = db.get_prompts_by_commit(commit_sha).unwrap();
        assert_eq!(retrieved.len(), 3);

        // Verify all have correct commit_sha
        for record in retrieved {
            assert_eq!(record.commit_sha, Some(commit_sha.to_string()));
        }
    }

    #[test]
    fn test_database_path() {
        let path = InternalDatabase::database_path().unwrap();
        assert!(path.to_string_lossy().contains(".git-ai"));
        assert!(path.to_string_lossy().contains("internal"));
        assert!(path.to_string_lossy().ends_with("db"));
    }

    #[test]
    fn test_stats_fields_populated() {
        use crate::authorship::working_log::{AgentId, Checkpoint, CheckpointKind, CheckpointLineStats};

        let (mut db, _temp_dir) = create_test_db();

        // Create a checkpoint with stats
        let mut checkpoint = Checkpoint::new(
            CheckpointKind::AiAgent,
            "test diff".to_string(),
            "John Doe".to_string(),
            vec![],
        );

        let mut transcript = AiTranscript::new();
        transcript.add_message(Message::User {
            text: "Test".to_string(),
            timestamp: None,
        });

        checkpoint.agent_id = Some(AgentId {
            tool: "cursor".to_string(),
            id: "test-session".to_string(),
            model: "claude-sonnet-4.5".to_string(),
        });
        checkpoint.transcript = Some(transcript);
        checkpoint.line_stats = CheckpointLineStats {
            additions: 42,
            deletions: 13,
            additions_sloc: 35,
            deletions_sloc: 10,
        };

        // Create record from checkpoint
        let record = PromptDbRecord::from_checkpoint(&checkpoint, Some("/test/repo".to_string()), None)
            .expect("Failed to create record from checkpoint");

        // Verify stats fields are populated
        assert_eq!(record.human_author, Some("John Doe".to_string()));
        assert_eq!(record.total_additions, Some(42));
        assert_eq!(record.total_deletions, Some(13));
        assert_eq!(record.accepted_lines, None);
        assert_eq!(record.overridden_lines, None);

        // Upsert and verify persistence
        db.upsert_prompt(&record).unwrap();
        let retrieved = db.get_prompt(&record.id).unwrap().unwrap();

        assert_eq!(retrieved.human_author, Some("John Doe".to_string()));
        assert_eq!(retrieved.total_additions, Some(42));
        assert_eq!(retrieved.total_deletions, Some(13));
        assert_eq!(retrieved.accepted_lines, None);
        assert_eq!(retrieved.overridden_lines, None);
    }

    // CAS sync queue tests

    #[test]
    fn test_cas_sync_queue_schema() {
        let (db, _temp_dir) = create_test_db();

        // Verify cas_sync_queue table exists
        let count: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='cas_sync_queue'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);

        // Verify status column has correct default and check constraint
        let status: String = db
            .conn
            .query_row(
                "SELECT dflt_value FROM pragma_table_info('cas_sync_queue') WHERE name='status'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "'pending'");
    }

    #[test]
    fn test_enqueue_cas_object_with_metadata() {
        let (mut db, _temp_dir) = create_test_db();

        let mut metadata = HashMap::new();
        metadata.insert("key1".to_string(), "value1".to_string());
        metadata.insert("key2".to_string(), "value2".to_string());

        let json_data = serde_json::json!({"test": "data", "number": 123});

        // Enqueue an object with metadata
        let hash = db.enqueue_cas_object(&json_data, Some(&metadata)).unwrap();

        // Verify metadata was stored correctly
        let metadata_json: String = db
            .conn
            .query_row(
                "SELECT metadata FROM cas_sync_queue WHERE hash = ?",
                params![&hash],
                |row| row.get(0),
            )
            .unwrap();

        let stored_metadata: HashMap<String, String> = serde_json::from_str(&metadata_json).unwrap();
        assert_eq!(stored_metadata.get("key1"), Some(&"value1".to_string()));
        assert_eq!(stored_metadata.get("key2"), Some(&"value2".to_string()));

        // Verify dequeue returns metadata correctly
        let batch = db.dequeue_cas_batch(10).unwrap();
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].hash, hash);
        // Data is canonicalized JSON
        let stored_json: serde_json::Value = serde_json::from_str(&batch[0].data).unwrap();
        assert_eq!(stored_json, json_data);
        assert_eq!(batch[0].metadata.get("key1"), Some(&"value1".to_string()));
        assert_eq!(batch[0].metadata.get("key2"), Some(&"value2".to_string()));
    }

    #[test]
    fn test_enqueue_cas_object() {
        let (mut db, _temp_dir) = create_test_db();

        let json_data = serde_json::json!({"key": "value"});

        // Enqueue an object
        let hash = db.enqueue_cas_object(&json_data, None).unwrap();

        // Verify it was inserted with correct defaults
        let (stored_hash, stored_data, metadata, status, attempts): (String, String, String, String, u32) = db
            .conn
            .query_row(
                "SELECT hash, data, metadata, status, attempts FROM cas_sync_queue WHERE hash = ?",
                params![&hash],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
            )
            .unwrap();

        assert_eq!(stored_hash, hash);
        // Data should be canonicalized JSON
        let stored_json: serde_json::Value = serde_json::from_str(&stored_data).unwrap();
        assert_eq!(stored_json, json_data);
        assert_eq!(status, "pending");
        assert_eq!(attempts, 0);
        assert_eq!(metadata, "{}");
    }

    #[test]
    fn test_enqueue_duplicate_hash() {
        let (mut db, _temp_dir) = create_test_db();

        // Same JSON content should produce same hash
        let json_data = serde_json::json!({"same": "content"});

        // Enqueue the same content twice
        let hash1 = db.enqueue_cas_object(&json_data, None).unwrap();
        let hash2 = db.enqueue_cas_object(&json_data, None).unwrap();

        // Both calls should return the same hash
        assert_eq!(hash1, hash2);

        // Verify only one record exists (INSERT OR IGNORE)
        let count: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM cas_sync_queue WHERE hash = ?",
                params![&hash1],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_dequeue_cas_batch() {
        let (mut db, _temp_dir) = create_test_db();

        // Enqueue multiple objects with different content
        db.enqueue_cas_object(&serde_json::json!({"id": 1}), None).unwrap();
        db.enqueue_cas_object(&serde_json::json!({"id": 2}), None).unwrap();
        db.enqueue_cas_object(&serde_json::json!({"id": 3}), None).unwrap();

        // Dequeue batch of 2
        let batch = db.dequeue_cas_batch(2).unwrap();
        assert_eq!(batch.len(), 2);

        // Verify records are marked as processing
        let processing_count: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM cas_sync_queue WHERE status = 'processing'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(processing_count, 2);

        // Verify one is still pending
        let pending_count: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM cas_sync_queue WHERE status = 'pending'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(pending_count, 1);
    }

    #[test]
    fn test_dequeue_respects_next_retry() {
        let (mut db, _temp_dir) = create_test_db();

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        let hash1 = "hash1";
        let hash2 = "hash2";
        let data1 = "data1";
        let data2 = "data2";

        // Insert one record ready to retry (past)
        db.conn.execute(
            "INSERT INTO cas_sync_queue (hash, data, metadata, status, attempts, next_retry_at, created_at) VALUES (?, ?, '{}', 'pending', 0, ?, ?)",
            params![hash1, data1, now - 100, now],
        ).unwrap();

        // Insert one record not ready yet (future)
        db.conn.execute(
            "INSERT INTO cas_sync_queue (hash, data, metadata, status, attempts, next_retry_at, created_at) VALUES (?, ?, '{}', 'pending', 0, ?, ?)",
            params![hash2, data2, now + 1000, now],
        ).unwrap();

        // Dequeue should only return the first one
        let batch = db.dequeue_cas_batch(10).unwrap();
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].hash, hash1);
    }

    #[test]
    fn test_dequeue_locks_records() {
        let (mut db, _temp_dir) = create_test_db();

        let json_data = serde_json::json!({"test": "lock"});
        let hash = db.enqueue_cas_object(&json_data, None).unwrap();

        // Dequeue
        let batch = db.dequeue_cas_batch(10).unwrap();
        assert_eq!(batch.len(), 1);

        // Verify status is 'processing'
        let status: String = db
            .conn
            .query_row(
                "SELECT status FROM cas_sync_queue WHERE hash = ?",
                params![&hash],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "processing");

        // Verify processing_started_at is set
        let processing_started_at: Option<i64> = db
            .conn
            .query_row(
                "SELECT processing_started_at FROM cas_sync_queue WHERE hash = ?",
                params![&hash],
                |row| row.get(0),
            )
            .unwrap();
        assert!(processing_started_at.is_some());

        // Try to dequeue again - should get empty (already locked)
        let batch2 = db.dequeue_cas_batch(10).unwrap();
        assert_eq!(batch2.len(), 0);
    }

    #[test]
    fn test_stale_lock_recovery() {
        let (mut db, _temp_dir) = create_test_db();

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        let hash = "hash1";
        let data = "data1";

        // Insert a record in 'processing' state with old timestamp (>10 minutes ago)
        let stale_time = now - 700; // 11+ minutes ago
        db.conn.execute(
            "INSERT INTO cas_sync_queue (hash, data, metadata, status, attempts, next_retry_at, processing_started_at, created_at) VALUES (?, ?, '{}', 'processing', 0, ?, ?, ?)",
            params![hash, data, now, stale_time, now],
        ).unwrap();

        // Dequeue should recover the stale lock
        let batch = db.dequeue_cas_batch(10).unwrap();
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].hash, hash);
    }

    #[test]
    fn test_max_attempts_limit() {
        let (mut db, _temp_dir) = create_test_db();

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;

        let hash1 = "hash1";
        let hash2 = "hash2";
        let data1 = "data1";
        let data2 = "data2";

        // Insert a record with 6 attempts (max reached)
        db.conn.execute(
            "INSERT INTO cas_sync_queue (hash, data, metadata, status, attempts, next_retry_at, created_at) VALUES (?, ?, '{}', 'pending', 6, ?, ?)",
            params![hash1, data1, now - 100, now],
        ).unwrap();

        // Insert a record with 5 attempts (still eligible)
        db.conn.execute(
            "INSERT INTO cas_sync_queue (hash, data, metadata, status, attempts, next_retry_at, created_at) VALUES (?, ?, '{}', 'pending', 5, ?, ?)",
            params![hash2, data2, now - 100, now],
        ).unwrap();

        // Dequeue should only return the one with 5 attempts
        let batch = db.dequeue_cas_batch(10).unwrap();
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].hash, hash2);
        assert_eq!(batch[0].attempts, 5);
    }

    #[test]
    fn test_update_cas_sync_failure() {
        let (mut db, _temp_dir) = create_test_db();

        db.enqueue_cas_object(&serde_json::json!({"test": "failure"}), None).unwrap();
        let batch = db.dequeue_cas_batch(10).unwrap();
        let record = &batch[0];

        // Update with failure
        db.update_cas_sync_failure(record.id, "test error").unwrap();

        // Verify status is back to 'pending'
        let status: String = db
            .conn
            .query_row(
                "SELECT status FROM cas_sync_queue WHERE id = ?",
                params![record.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "pending");

        // Verify processing_started_at is cleared
        let processing_started_at: Option<i64> = db
            .conn
            .query_row(
                "SELECT processing_started_at FROM cas_sync_queue WHERE id = ?",
                params![record.id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(processing_started_at.is_none());

        // Verify attempts incremented
        let attempts: u32 = db
            .conn
            .query_row(
                "SELECT attempts FROM cas_sync_queue WHERE id = ?",
                params![record.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(attempts, 1);

        // Verify error recorded
        let error: String = db
            .conn
            .query_row(
                "SELECT last_sync_error FROM cas_sync_queue WHERE id = ?",
                params![record.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(error, "test error");
    }

    #[test]
    fn test_delete_cas_sync_record() {
        let (mut db, _temp_dir) = create_test_db();

        db.enqueue_cas_object(&serde_json::json!({"test": "delete"}), None).unwrap();
        let batch = db.dequeue_cas_batch(10).unwrap();
        let record = &batch[0];

        // Delete the record
        db.delete_cas_sync_record(record.id).unwrap();

        // Verify it's gone
        let count: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM cas_sync_queue WHERE id = ?",
                params![record.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_exponential_backoff() {
        let now = 1000000i64;

        // Test each attempt's backoff
        assert_eq!(calculate_next_retry(1, now), now + 5 * 60);           // 5 min
        assert_eq!(calculate_next_retry(2, now), now + 30 * 60);          // 30 min
        assert_eq!(calculate_next_retry(3, now), now + 2 * 60 * 60);      // 2 hours
        assert_eq!(calculate_next_retry(4, now), now + 6 * 60 * 60);      // 6 hours
        assert_eq!(calculate_next_retry(5, now), now + 12 * 60 * 60);     // 12 hours
        assert_eq!(calculate_next_retry(6, now), now + 24 * 60 * 60);     // 24 hours
        assert_eq!(calculate_next_retry(7, now), now + 24 * 60 * 60);     // 24 hours (max)
    }
}
