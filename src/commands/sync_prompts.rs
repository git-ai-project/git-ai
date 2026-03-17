use crate::authorship::internal_db::{InternalDatabase, PromptDbRecord};
use crate::authorship::prompt_utils::{PromptUpdateResult, update_prompt_from_tool};
use crate::error::GitAiError;
use crate::observability::log_error;
use chrono::{DateTime, NaiveDate};
use std::cmp::min;
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn handle_sync_prompts(args: &[String]) {
    let mut since: Option<String> = None;
    let mut workdir: Option<String> = None;

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
                since = Some(args[i].clone());
            }
            "--workdir" => {
                if i + 1 >= args.len() {
                    eprintln!("Error: --workdir requires a value");
                    std::process::exit(1);
                }
                i += 1;
                workdir = Some(args[i].clone());
            }
            _ => {
                eprintln!("Error: Unknown argument: {}", args[i]);
                eprintln!("Usage: git-ai sync-prompts [--since <time>] [--workdir <path>]");
                std::process::exit(1);
            }
        }
        i += 1;
    }

    // Parse since into timestamp
    let since_timestamp = if let Some(since_str) = since {
        match parse_since_arg(&since_str) {
            Ok(ts) => Some(ts),
            Err(e) => {
                eprintln!("Error parsing --since: {}", e);
                std::process::exit(1);
            }
        }
    } else {
        None
    };

    // Run sync
    if let Err(e) = sync_prompts(since_timestamp, workdir.as_deref()) {
        eprintln!("Sync failed: {}", e);
        std::process::exit(1);
    }
}

fn parse_since_arg(since_str: &str) -> Result<i64, GitAiError> {
    // Try parsing as relative duration first (1d, 2h, 1w)
    if let Ok(duration) = humantime::parse_duration(since_str) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        return Ok(now - duration.as_secs() as i64);
    }

    // Try parsing as Unix timestamp
    if let Ok(timestamp) = since_str.parse::<i64>() {
        return Ok(timestamp);
    }

    // Try parsing as ISO8601/RFC3339
    if let Ok(dt) = DateTime::parse_from_rfc3339(since_str) {
        return Ok(dt.timestamp());
    }

    // Try parsing as simple date (YYYY-MM-DD)
    if let Ok(dt) = NaiveDate::parse_from_str(since_str, "%Y-%m-%d") {
        let datetime = dt.and_hms_opt(0, 0, 0).unwrap();
        return Ok(datetime.and_utc().timestamp());
    }

    Err(GitAiError::Generic(format!(
        "Invalid --since format: '{}'. Supported formats: '1d', '2h', Unix timestamp, ISO8601, or YYYY-MM-DD",
        since_str
    )))
}

fn sync_prompts(since_timestamp: Option<i64>, workdir: Option<&str>) -> Result<(), GitAiError> {
    eprintln!("Starting prompt sync...");

    let db = InternalDatabase::global()?;
    let mut db_lock = db
        .lock()
        .map_err(|e| GitAiError::Generic(format!("Failed to lock database: {}", e)))?;

    // Query prompts
    let prompts = if let Some(since) = since_timestamp {
        eprintln!("Syncing prompts updated since Unix timestamp {}", since);
        db_lock.list_prompts(workdir, Some(since), 10000, 0)?
    } else {
        eprintln!("Syncing all prompts in database");
        db_lock.list_prompts(workdir, None, 10000, 0)?
    };

    if prompts.is_empty() {
        eprintln!("No prompts to sync");
        return Ok(());
    }

    eprintln!("Found {} prompts to process", prompts.len());

    // Deduplicate by agent_id (keep latest per conversation)
    let prompts_to_update = deduplicate_by_agent_id(&prompts);
    eprintln!("Updating {} unique conversations", prompts_to_update.len());

    // Update each prompt
    let mut updated_records = Vec::new();
    let mut success_count = 0;
    let mut skip_count = 0;
    let mut error_count = 0;

    for record in prompts_to_update {
        match update_prompt_record(&record) {
            Ok(Some(updated_record)) => {
                eprintln!(
                    "  ✓ Updated {} ({}/{})",
                    &record.id[..8],
                    record.tool,
                    &record.external_thread_id[..min(16, record.external_thread_id.len())]
                );
                updated_records.push(updated_record);
                success_count += 1;
            }
            Ok(None) => {
                skip_count += 1;
            }
            Err(e) => {
                eprintln!("  ✗ Failed {} ({}): {}", &record.id[..8], record.tool, e);
                log_error(
                    &e,
                    Some(serde_json::json!({
                        "operation": "sync_prompts",
                        "prompt_id": record.id,
                        "tool": record.tool,
                    })),
                );
                error_count += 1;
            }
        }
    }

    // Batch upsert updated records
    if !updated_records.is_empty() {
        eprintln!(
            "\nBatch upserting {} updated prompts...",
            updated_records.len()
        );
        db_lock.batch_upsert_prompts(&updated_records)?;
    }

    eprintln!(
        "\n✓ Sync complete: {} updated, {} skipped, {} failed",
        success_count, skip_count, error_count
    );

    Ok(())
}

fn deduplicate_by_agent_id(prompts: &[PromptDbRecord]) -> Vec<PromptDbRecord> {
    let mut latest_by_agent: HashMap<String, PromptDbRecord> = HashMap::new();

    for record in prompts {
        let key = format!("{}:{}", record.tool, record.external_thread_id);

        // Keep the record with latest updated_at
        latest_by_agent
            .entry(key)
            .and_modify(|existing| {
                if record.updated_at > existing.updated_at {
                    *existing = record.clone();
                }
            })
            .or_insert_with(|| record.clone());
    }

    latest_by_agent.into_values().collect()
}

fn update_prompt_record(record: &PromptDbRecord) -> Result<Option<PromptDbRecord>, GitAiError> {
    // Use shared update_prompt_from_tool from prompt_updater module
    let result = update_prompt_from_tool(
        &record.tool,
        &record.external_thread_id,
        record.agent_metadata.as_ref(),
        &record.model,
    );

    match result {
        PromptUpdateResult::Updated(new_transcript, new_model) => {
            // Check if transcript actually changed
            if new_transcript == record.messages {
                return Ok(None); // No actual change
            }

            // Use last message timestamp for updated_at, fall back to now if unavailable
            let updated_at = new_transcript
                .last_message_timestamp_unix()
                .unwrap_or_else(|| {
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_secs() as i64
                });

            let mut updated_record = record.clone();
            updated_record.messages = new_transcript;
            updated_record.model = new_model;
            updated_record.updated_at = updated_at;

            Ok(Some(updated_record))
        }
        PromptUpdateResult::Unchanged => Ok(None),
        PromptUpdateResult::Failed(e) => Err(e),
    }
}

/// Sync recent prompts silently (for share command pre-refresh).
/// This refreshes the database with the latest transcript data before showing/uploading.
///
/// Args:
///   limit: Maximum number of prompts to sync
///
/// Returns Ok(()) on success. Errors on individual prompts are silently ignored.
pub fn sync_recent_prompts_silent(limit: usize) -> Result<(), GitAiError> {
    let db = InternalDatabase::global()?;
    let mut db_lock = db
        .lock()
        .map_err(|e| GitAiError::Generic(format!("Failed to lock database: {}", e)))?;

    // Get most recent prompts (no workdir filter, no since filter)
    let prompts = db_lock.list_prompts(None, None, limit, 0)?;

    if prompts.is_empty() {
        return Ok(());
    }

    // Deduplicate by agent_id (keep latest per conversation)
    let prompts_to_update = deduplicate_by_agent_id(&prompts);

    // Update each prompt, collecting successful updates
    let mut updated_records = Vec::new();

    for record in prompts_to_update {
        if let Ok(Some(updated_record)) = update_prompt_record(&record) {
            updated_records.push(updated_record);
        }
        // Silently skip errors and unchanged prompts
    }

    // Batch upsert updated records
    if !updated_records.is_empty() {
        db_lock.batch_upsert_prompts(&updated_records)?;
    }

    Ok(())
}
