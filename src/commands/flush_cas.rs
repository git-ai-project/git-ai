use crate::authorship::internal_db::InternalDatabase;
use futures::stream::{self, StreamExt};
use std::collections::HashMap;
use std::sync::Arc;

/// Handle the flush-cas command
pub fn handle_flush_cas(args: &[String]) {
    eprintln!("Starting CAS sync worker...");

    // Get database connection
    let db = match InternalDatabase::global() {
        Ok(db) => db,
        Err(e) => {
            eprintln!("Failed to access database: {}", e);
            std::process::exit(1);
        }
    };

    // Process queue in batches until empty
    let total_synced = smol::block_on(async {
        let mut total = 0;
        let db_arc = Arc::new(db);

        loop {
            // Dequeue batch of 10
            let batch = {
                let mut db_lock = db_arc.lock().unwrap();
                match db_lock.dequeue_cas_batch(10) {
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

            // Process batch concurrently
            let results = stream::iter(batch)
                .map(|record| {
                    let db = Arc::clone(&db_arc);

                    smol::unblock(move || {
                        // Convert hash bytes to hex string for display
                        let hash_hex: String = record.hash.iter().map(|b| format!("{:02x}", b)).collect();
                        let hash_short = if hash_hex.len() > 16 {
                            &hash_hex[..16]
                        } else {
                            &hash_hex
                        };

                        // Attempt to sync the object
                        match sync_cas_object(record.hash.clone(), record.data.clone(), record.metadata.clone()) {
                            Ok(()) => {
                                // Success - delete from queue
                                let mut db_lock = db.lock().unwrap();
                                match db_lock.delete_cas_sync_record(record.id) {
                                    Ok(()) => {
                                        eprintln!("  ✓ Synced {}", hash_short);
                                        Ok(())
                                    }
                                    Err(e) => {
                                        eprintln!("  ✗ Failed to delete record for {}: {}", hash_short, e);
                                        Err(format!("Delete failed: {}", e))
                                    }
                                }
                            }
                            Err(e) => {
                                // Failure - update error and retry info
                                let error_msg = e.to_string();
                                let mut db_lock = db.lock().unwrap();
                                match db_lock.update_cas_sync_failure(record.id, &error_msg) {
                                    Ok(()) => {
                                        eprintln!(
                                            "  ✗ Failed {} (attempt {}): {}",
                                            hash_short,
                                            record.attempts + 1,
                                            error_msg
                                        );
                                    }
                                    Err(update_err) => {
                                        eprintln!(
                                            "  ✗ Failed to update error for {}: {}",
                                            hash_short, update_err
                                        );
                                    }
                                }
                                Err(error_msg)
                            }
                        }
                    })
                })
                .buffer_unordered(10)
                .collect::<Vec<_>>()
                .await;

            // Count successes
            let successes = results.iter().filter(|r| r.is_ok()).count();
            total += successes;

            eprintln!("Batch complete: {} succeeded, {} failed", successes, results.len() - successes);
        }

        total
    });

    if total_synced > 0 {
        eprintln!("\n✓ Successfully synced {} objects", total_synced);
    } else {
        eprintln!("\n○ No objects were synced");
    }
}

/// Sync a CAS object to remote storage
///
/// TODO: Implement actual CAS sync to remote storage
/// This will call the backend API to upload the CAS object
/// The `data` parameter contains raw bytes
/// The `metadata` parameter contains string-to-string metadata map
fn sync_cas_object(_hash: Vec<u8>, _data: Vec<u8>, _metadata: HashMap<String, String>) -> Result<(), Box<dyn std::error::Error>> {
    // STUB: For now, just return success
    // In the future, this will:
    // 1. POST to /api/cas or similar endpoint with raw bytes
    // 2. Include metadata in the request (e.g., as JSON in headers or request body)
    // 3. Handle authentication
    // 4. Return appropriate errors on failure

    Ok(())
}
