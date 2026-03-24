use crate::api::{ApiClient, ApiContext, CasObject, CasUploadRequest};
use crate::authorship::internal_db::{CasSyncRecord, InternalDatabase};
use crate::observability::log_error;
use std::collections::HashMap;

const ENV_CAS_FLUSH_WORKER: &str = "GIT_AI_CAS_FLUSH_WORKER";

/// Spawn a background process to flush CAS objects to the server.
///
/// In daemon mode, this is a no-op because CAS objects are sent over the
/// control socket and the daemon's telemetry worker handles upload.
pub fn spawn_background_cas_flush() {
    // In daemon mode the telemetry worker handles CAS flushing — skip the subprocess.
    if crate::daemon::telemetry_handle::daemon_telemetry_available() {
        return;
    }

    let _ =
        crate::utils::spawn_internal_git_ai_subcommand("flush-cas", &[], ENV_CAS_FLUSH_WORKER, &[]);
}

/// Handle the flush-cas command
pub fn handle_flush_cas(_args: &[String]) {
    // Create API client to check login status
    let context = ApiContext::new(None);
    let api_base_url = context.base_url.clone();
    let client = ApiClient::new(context);

    // Skip if using default API and not logged in
    let using_default_api = api_base_url == crate::config::DEFAULT_API_BASE_URL;
    if using_default_api && !client.is_logged_in() && !client.has_api_key() {
        eprintln!("Skipping CAS sync: not logged in and using default API");
        return;
    }

    eprintln!("Starting CAS sync worker...");

    // Get database connection
    let db = match InternalDatabase::global() {
        Ok(db) => db,
        Err(e) => {
            eprintln!("Failed to access database: {}", e);
            std::process::exit(1);
        }
    };

    let mut total_synced = 0;

    loop {
        // Dequeue batch of up to 50 objects
        let batch = {
            let mut db_lock = db.lock().unwrap();
            match db_lock.dequeue_cas_batch(50) {
                Ok(batch) => batch,
                Err(e) => {
                    eprintln!("Error dequeuing batch: {}", e);
                    break;
                }
            }
        };

        // If batch is empty, we're done
        if batch.is_empty() {
            break;
        }

        eprintln!("Processing batch of {} objects...", batch.len());

        // Build batch request with all objects
        let mut cas_objects = Vec::new();
        let mut record_map: HashMap<String, CasSyncRecord> = HashMap::new();

        for record in &batch {
            let content: serde_json::Value = match serde_json::from_str(&record.data) {
                Ok(v) => v,
                Err(e) => {
                    // Mark as failed if we can't parse the JSON
                    let mut db_lock = db.lock().unwrap();
                    let _ = db_lock
                        .update_cas_sync_failure(record.id, &format!("JSON parse error: {}", e));
                    eprintln!(
                        "  ✗ Failed {} (parse error): {}",
                        &record.hash[..16.min(record.hash.len())],
                        e
                    );
                    continue;
                }
            };
            cas_objects.push(CasObject {
                content,
                hash: record.hash.clone(),
                metadata: record.metadata.clone(),
            });
            record_map.insert(record.hash.clone(), record.clone());
        }

        // Skip API call if no valid objects
        if cas_objects.is_empty() {
            continue;
        }

        // Send single batch request
        let request = CasUploadRequest {
            objects: cas_objects,
        };

        match client.upload_cas(request) {
            Ok(response) => {
                // Process each result
                let mut db_lock = db.lock().unwrap();
                for result in response.results {
                    if let Some(record) = record_map.get(&result.hash) {
                        let hash_short = &result.hash[..16.min(result.hash.len())];
                        if result.status == "ok" {
                            // Success - delete from queue
                            if let Err(e) = db_lock.delete_cas_sync_record(record.id) {
                                eprintln!("  ✗ Failed to delete record for {}: {}", hash_short, e);
                            } else {
                                eprintln!("  ✓ Synced {}", hash_short);
                                total_synced += 1;
                            }
                        } else {
                            // Failed - update error
                            let error = result.error.unwrap_or_else(|| "Unknown error".to_string());

                            // Log to Sentry
                            log_error(
                                &crate::error::GitAiError::Generic(error.clone()),
                                Some(serde_json::json!({
                                    "operation": "cas_flush_object",
                                    "api_host": api_base_url,
                                    "object_hash": result.hash,
                                    "object_metadata": record.metadata,
                                    "attempt": record.attempts + 1,
                                })),
                            );

                            if let Err(e) = db_lock.update_cas_sync_failure(record.id, &error) {
                                eprintln!("  ✗ Failed to update error for {}: {}", hash_short, e);
                            } else {
                                eprintln!(
                                    "  ✗ Failed {} (attempt {}): {}",
                                    hash_short,
                                    record.attempts + 1,
                                    error
                                );
                            }
                        }
                    }
                }
                eprintln!(
                    "Batch complete: {} succeeded, {} failed",
                    response.success_count, response.failure_count
                );
            }
            Err(e) => {
                // Log to Sentry (once for entire batch)
                log_error(
                    &e,
                    Some(serde_json::json!({
                        "operation": "cas_flush_batch",
                        "api_host": api_base_url,
                        "batch_size": batch.len(),
                    })),
                );

                // Entire batch failed - mark all as failed
                let error_msg = e.to_string();
                let mut db_lock = db.lock().unwrap();
                for record in batch.iter() {
                    let hash_short = &record.hash[..16.min(record.hash.len())];
                    if let Err(update_err) = db_lock.update_cas_sync_failure(record.id, &error_msg)
                    {
                        eprintln!(
                            "  ✗ Failed to update error for {}: {}",
                            hash_short, update_err
                        );
                    } else {
                        eprintln!(
                            "  ✗ Failed {} (attempt {}): {}",
                            hash_short,
                            record.attempts + 1,
                            error_msg
                        );
                    }
                }
                eprintln!("Batch failed: {}", e);
            }
        }
    }

    if total_synced > 0 {
        eprintln!("\n✓ Successfully synced {} objects", total_synced);
    } else {
        eprintln!("\n○ No objects were synced");
    }
}
