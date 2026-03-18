//! Handle flush-metrics-db command (internal).
//!
//! Drains the metrics database queue by uploading batches to the API.

use crate::api::{ApiClient, ApiContext, upload_metrics_with_retry};
use crate::metrics::db::MetricsDatabase;
use crate::metrics::{MetricEvent, MetricsBatch};

/// Max events per batch upload
const MAX_BATCH_SIZE: usize = 250;
const ENV_FLUSH_METRICS_DB_WORKER: &str = "GIT_AI_FLUSH_METRICS_DB_WORKER";

/// Spawn a background process to flush metrics DB
#[cfg(not(any(test, feature = "test-support")))]
pub fn spawn_background_metrics_db_flush() {
    let _ = crate::utils::spawn_internal_git_ai_subcommand(
        "flush-metrics-db",
        &[],
        ENV_FLUSH_METRICS_DB_WORKER,
        &[],
    );
}

/// No-op in test mode.
#[cfg(any(test, feature = "test-support"))]
pub fn spawn_background_metrics_db_flush() {}

/// Handle the flush-metrics-db command
pub fn handle_flush_metrics_db(_args: &[String]) {
    let is_background_worker = std::env::var(ENV_FLUSH_METRICS_DB_WORKER).as_deref() == Ok("1");
    macro_rules! user_log {
        ($($arg:tt)*) => {
            if !is_background_worker {
                eprintln!($($arg)*);
            }
        };
    }

    // Check conditions: (!using_default_api) || is_logged_in() || has_api_key()
    let context = ApiContext::new(None);
    let api_base_url = context.base_url.clone();
    let client = ApiClient::new(context);

    let using_default_api = api_base_url == crate::config::DEFAULT_API_BASE_URL;
    if using_default_api && !client.is_logged_in() && !client.has_api_key() {
        user_log!("flush-metrics-db: skipping (not logged in and using default API)");
        return;
    }

    // Get database connection
    let db = match MetricsDatabase::global() {
        Ok(db) => db,
        Err(e) => {
            user_log!("flush-metrics-db: failed to open metrics database: {}", e);
            return;
        }
    };

    // Build mirror client once (avoids per-batch subprocess for git identity)
    let mirror_client = ApiContext::mirror().map(ApiClient::new);

    let mut total_uploaded = 0usize;
    let mut total_batches = 0usize;
    let mut total_invalid = 0usize;

    loop {
        // Get batch from DB
        let batch = {
            let db_lock = match db.lock() {
                Ok(lock) => lock,
                Err(e) => {
                    user_log!("flush-metrics-db: failed to acquire db lock: {}", e);
                    break;
                }
            };
            match db_lock.get_batch(MAX_BATCH_SIZE) {
                Ok(batch) => batch,
                Err(e) => {
                    user_log!("flush-metrics-db: failed to read batch: {}", e);
                    break;
                }
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
                total_invalid += 1;
                // Invalid JSON - delete the record
                if let Ok(mut db_lock) = db.lock() {
                    let _ = db_lock.delete_records(&[record.id]);
                }
            }
        }

        if events.is_empty() {
            continue;
        }

        let event_count = events.len();
        let metrics_batch = MetricsBatch::new(events);

        // Upload with retry logic (15s, 60s, 3min backoff)
        match upload_metrics_with_retry(&client, &metrics_batch, "flush_metrics_db") {
            Ok(()) => {
                // Mirror to secondary endpoint (no-op if mirror not configured)
                let mirrored = match &mirror_client {
                    Some(c) => c.upload_metrics(&metrics_batch).map(|_| ()).is_ok(),
                    None => true,
                };

                total_batches += 1;
                // Only delete if mirror also succeeded (or not configured).
                // If mirror failed, leave records for next cycle — primary is idempotent.
                if mirrored {
                    total_uploaded += event_count;
                    if let Ok(mut db_lock) = db.lock() {
                        let _ = db_lock.delete_records(&record_ids);
                    }
                    user_log!(
                        "  ✓ batch {} - uploaded {} events",
                        total_batches,
                        event_count
                    );
                } else {
                    user_log!(
                        "  ⚠ batch {} - uploaded to primary, mirror failed (keeping {} events for retry)",
                        total_batches,
                        event_count
                    );
                    break;
                }
            }
            Err(e) => {
                // All retries failed - keep records in DB for next time
                user_log!(
                    "  ✗ batch upload failed ({} events kept for retry): {}",
                    event_count,
                    e
                );
                break;
            }
        }
    }

    if total_invalid > 0 {
        user_log!(
            "flush-metrics-db: discarded {} invalid record(s)",
            total_invalid
        );
    }

    user_log!(
        "flush-metrics-db: uploaded {} events in {} batch(es)",
        total_uploaded,
        total_batches
    );
}
