//! Handle flush-metrics-db command (internal).
//!
//! Drains the metrics database queue by uploading batches to the API.

use crate::api::metrics::MetricsUploadResponse;
use crate::api::{ApiClient, ApiContext, upload_metrics_with_retry};
use crate::metrics::db::MetricsDatabase;
use crate::metrics::{MetricEvent, MetricsBatch};
use std::collections::HashSet;

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

fn split_record_ids_by_response(
    record_ids: &[i64],
    response: &MetricsUploadResponse,
) -> (Vec<i64>, Vec<i64>, usize) {
    let mut failed_indexes = HashSet::new();
    let mut invalid_error_indexes = 0usize;

    for error in &response.errors {
        if error.index < record_ids.len() {
            failed_indexes.insert(error.index);
        } else {
            invalid_error_indexes += 1;
        }
    }

    let successful_ids = record_ids
        .iter()
        .enumerate()
        .filter_map(|(index, id)| (!failed_indexes.contains(&index)).then_some(*id))
        .collect::<Vec<_>>();

    let failed_ids = record_ids
        .iter()
        .enumerate()
        .filter_map(|(index, id)| failed_indexes.contains(&index).then_some(*id))
        .collect::<Vec<_>>();

    (successful_ids, failed_ids, invalid_error_indexes)
}

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

    // Check conditions: (!using_default_api) || is_logged_in()
    let context = ApiContext::new(None);
    let api_base_url = context.base_url.clone();
    let client = ApiClient::new(context);

    let using_default_api = api_base_url == crate::config::DEFAULT_API_BASE_URL;
    if using_default_api && !client.is_logged_in() {
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

    let mut total_uploaded = 0usize;
    let mut total_discarded = 0usize;
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
            Ok(response) => {
                let (successful_ids, failed_ids, invalid_error_indexes) =
                    split_record_ids_by_response(&record_ids, &response);
                let successful_count = successful_ids.len();
                let failed_count = failed_ids.len();
                let total_resolved = successful_count + failed_count;

                total_uploaded += successful_count;
                total_discarded += failed_count;
                total_batches += 1;
                if failed_count == 0 {
                    user_log!(
                        "  ✓ batch {} - uploaded {} events",
                        total_batches,
                        successful_count
                    );
                } else {
                    user_log!(
                        "  ! batch {} - uploaded {} events, {} event(s) failed and were kept for retry",
                        total_batches,
                        successful_count,
                        failed_count
                    );
                }
                if invalid_error_indexes > 0 {
                    user_log!(
                        "  ! batch {} - server returned {} out-of-range error index(es)",
                        total_batches,
                        invalid_error_indexes
                    );
                }

                // Delete records that have been resolved by server response:
                // - accepted events (successful_ids)
                // - explicitly rejected events (failed_ids)
                // Rejected events are validation failures and will not succeed on retry.
                let mut record_ids_to_delete =
                    Vec::with_capacity(successful_ids.len() + failed_ids.len());
                record_ids_to_delete.extend(successful_ids);
                record_ids_to_delete.extend(failed_ids);

                if !record_ids_to_delete.is_empty()
                    && let Ok(mut db_lock) = db.lock()
                {
                    let _ = db_lock.delete_records(&record_ids_to_delete);
                }

                // Avoid a tight loop when we cannot map/delete any records.
                if event_count > 0 && total_resolved == 0 {
                    user_log!(
                        "  ! batch {} - server returned no resolvable indices for this batch; stopping for now",
                        total_batches
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
        "flush-metrics-db: uploaded {} events, discarded {} rejected events in {} batch(es)",
        total_uploaded,
        total_discarded,
        total_batches
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::metrics::MetricsUploadError;

    #[test]
    fn test_split_record_ids_by_response_without_errors() {
        let ids = vec![10, 11, 12];
        let response = MetricsUploadResponse { errors: vec![] };

        let (successful, failed, invalid_indexes) = split_record_ids_by_response(&ids, &response);

        assert_eq!(successful, ids);
        assert!(failed.is_empty());
        assert_eq!(invalid_indexes, 0);
    }

    #[test]
    fn test_split_record_ids_by_response_with_partial_errors() {
        let ids = vec![20, 21, 22, 23];
        let response = MetricsUploadResponse {
            errors: vec![
                MetricsUploadError {
                    index: 1,
                    error: "row failed".to_string(),
                },
                MetricsUploadError {
                    index: 3,
                    error: "row failed".to_string(),
                },
            ],
        };

        let (successful, failed, invalid_indexes) = split_record_ids_by_response(&ids, &response);

        assert_eq!(successful, vec![20, 22]);
        assert_eq!(failed, vec![21, 23]);
        assert_eq!(invalid_indexes, 0);
    }

    #[test]
    fn test_split_record_ids_by_response_ignores_invalid_error_indexes() {
        let ids = vec![30, 31];
        let response = MetricsUploadResponse {
            errors: vec![
                MetricsUploadError {
                    index: 99,
                    error: "bad index".to_string(),
                },
                MetricsUploadError {
                    index: 0,
                    error: "failed".to_string(),
                },
            ],
        };

        let (successful, failed, invalid_indexes) = split_record_ids_by_response(&ids, &response);

        assert_eq!(successful, vec![31]);
        assert_eq!(failed, vec![30]);
        assert_eq!(invalid_indexes, 1);
    }
}
