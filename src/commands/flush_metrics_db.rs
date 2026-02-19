//! Handle flush-metrics-db command (internal).
//!
//! Drains the metrics database queue by uploading batches to the API.

use crate::api::{ApiClient, ApiContext, upload_metrics_with_retry};
use crate::metrics::db::MetricsDatabase;
use crate::metrics::{MetricEvent, MetricsBatch};

/// Max events per batch upload
const MAX_BATCH_SIZE: usize = 250;

/// Spawn a background process to flush metrics DB
pub fn spawn_background_metrics_db_flush() {
    use std::process::Command;

    if let Ok(exe) = crate::utils::current_git_ai_exe() {
        let _ = Command::new(exe)
            .arg("flush-metrics-db")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
}

/// Handle the flush-metrics-db command
pub fn handle_flush_metrics_db(_args: &[String]) {
    // Check conditions: (!using_default_api) || is_logged_in()
    let context = ApiContext::new(None);
    let api_base_url = context.base_url.clone();
    let client = ApiClient::new(context);

    let using_default_api = api_base_url == crate::config::DEFAULT_API_BASE_URL;
    if using_default_api && !client.is_logged_in() {
        // Conditions not met - exit silently
        return;
    }

    // Get database connection
    let db = match MetricsDatabase::global() {
        Ok(db) => db,
        Err(_) => return,
    };

    loop {
        // Get batch from DB
        let batch = {
            let db_lock = match db.lock() {
                Ok(lock) => lock,
                Err(_) => break,
            };
            match db_lock.get_batch(MAX_BATCH_SIZE) {
                Ok(batch) => batch,
                Err(_) => break,
            }
        };

        // If batch is empty, we're done
        if batch.is_empty() {
            break;
        }

        // Parse events and build MetricsBatch
        let mut events = Vec::new();
        let mut record_ids = Vec::new();

        for record in &batch {
            if let Ok(event) = serde_json::from_str::<MetricEvent>(&record.event_json) {
                events.push(event);
                record_ids.push(record.id);
            } else {
                // Invalid JSON - delete the record
                if let Ok(mut db_lock) = db.lock() {
                    let _ = db_lock.delete_records(&[record.id]);
                }
            }
        }

        if events.is_empty() {
            continue;
        }

        let metrics_batch = MetricsBatch::new(events);

        // Upload with retry logic (15s, 60s, 3min backoff)
        match upload_metrics_with_retry(&client, &metrics_batch, "flush_metrics_db") {
            Ok(()) => {
                // Success - delete ALL records from this batch
                // Validation errors are logged to Sentry and won't succeed on retry
                if let Ok(mut db_lock) = db.lock() {
                    let _ = db_lock.delete_records(&record_ids);
                }
            }
            Err(_) => {
                // All retries failed - keep records in DB for next time
                break;
            }
        }
    }
}
