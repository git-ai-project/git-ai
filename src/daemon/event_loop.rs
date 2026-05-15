//! Event processing loop for the daemon.
//!
//! Reads trace2 events from a channel, feeds them to CommitDetector,
//! and processes any detected commits via `post_commit_worker`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use super::commit_detector::{CommitDetector, DetectedOperation, RewriteKind};
use super::post_commit_worker;
use super::repo_resolver::RepoPathResolver;
use super::rewrite_worker;
use super::stash_worker;
use super::stats;
use super::telemetry_worker::TelemetryHandle;
use super::trace2_events::Trace2Event;

/// Debounce window for rewrite operations (rebase produces many individual
/// commit events in rapid succession — we only need to process once at the end).
const REWRITE_DEBOUNCE: Duration = Duration::from_millis(500);

/// Pending rewrite that is being debounced.
struct PendingRewrite {
    kind: RewriteKind,
    argv: Vec<String>,
    scheduled_at: Instant,
}

/// Run the event processing loop.
///
/// Reads events from the channel, feeds them to `CommitDetector`,
/// and processes any detected commits. Loops until `shutdown` is set.
///
/// This function blocks the calling thread.
pub fn run_event_loop(
    event_rx: Receiver<Trace2Event>,
    shutdown: Arc<AtomicBool>,
    telemetry: TelemetryHandle,
) {
    let mut detector = CommitDetector::new();
    let mut resolver = RepoPathResolver::new();
    let mut last_prune = Instant::now();
    let prune_interval = Duration::from_secs(60);
    let stale_threshold = Duration::from_secs(120);
    let recv_timeout = Duration::from_millis(50);

    // Debounce buffer for rewrites: repo_path → pending rewrite
    let mut pending_rewrites: HashMap<PathBuf, PendingRewrite> = HashMap::new();

    let daemon_stats = stats::get();

    eprintln!("[git-ai daemon] event loop started");

    loop {
        if shutdown.load(Ordering::Relaxed) {
            eprintln!("[git-ai daemon] event loop shutting down");
            break;
        }

        // Process any debounced rewrites that are ready
        let now = Instant::now();
        let ready_keys: Vec<PathBuf> = pending_rewrites
            .iter()
            .filter(|(_, pending)| now.duration_since(pending.scheduled_at) >= REWRITE_DEBOUNCE)
            .map(|(k, _)| k.clone())
            .collect();

        for repo_path in ready_keys {
            if let Some(pending) = pending_rewrites.remove(&repo_path) {
                dispatch_rewrite(&repo_path, &pending.kind, &pending.argv, daemon_stats);
            }
        }

        match event_rx.recv_timeout(recv_timeout) {
            Ok(event) => {
                daemon_stats
                    .trace2_events_received
                    .fetch_add(1, Ordering::Relaxed);

                if let Some(operation) = detector.process_event_full(event) {
                    match operation {
                        DetectedOperation::Commit { ref repo_path } => {
                            let resolved = resolver.resolve(repo_path);
                            dispatch_commit(&resolved, daemon_stats, &telemetry);
                        }
                        DetectedOperation::Rewrite {
                            ref repo_path,
                            ref kind,
                            ref argv,
                        } => {
                            let resolved = resolver.resolve(repo_path);
                            // Debounce rewrites: rebase generates many rapid events
                            pending_rewrites.insert(
                                resolved,
                                PendingRewrite {
                                    kind: kind.clone(),
                                    argv: argv.clone(),
                                    scheduled_at: Instant::now(),
                                },
                            );
                        }
                        DetectedOperation::Stash { ref repo_path, ref argv } => {
                            let resolved = resolver.resolve(repo_path);
                            dispatch_stash(&resolved, argv, daemon_stats);
                        }
                        DetectedOperation::StashPop { ref repo_path } => {
                            let resolved = resolver.resolve(repo_path);
                            dispatch_stash_pop(&resolved, daemon_stats);
                        }
                    }
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                eprintln!("[git-ai daemon] event channel disconnected, shutting down");
                break;
            }
        }

        if last_prune.elapsed() >= prune_interval {
            detector.prune_stale(stale_threshold);
            resolver.prune();
            last_prune = Instant::now();
        }
    }

    // Flush remaining pending rewrites on shutdown
    for (repo_path, pending) in pending_rewrites.drain() {
        dispatch_rewrite(&repo_path, &pending.kind, &pending.argv, daemon_stats);
    }

    eprintln!("[git-ai daemon] event loop exited");
}

fn dispatch_commit(
    resolved: &std::path::Path,
    daemon_stats: &stats::DaemonStats,
    telemetry: &TelemetryHandle,
) {
    eprintln!("[git-ai daemon] commit detected in {}", resolved.display());
    match post_commit_worker::process_commit(resolved) {
        Ok(true) => {
            daemon_stats
                .commits_processed
                .fetch_add(1, Ordering::Relaxed);

            emit_commit_telemetry(resolved, telemetry);

            eprintln!(
                "[git-ai daemon] successfully processed commit in {}",
                resolved.display()
            );
        }
        Ok(false) => {
            daemon_stats
                .commits_skipped
                .fetch_add(1, Ordering::Relaxed);
            eprintln!(
                "[git-ai daemon] skipped commit in {} (already noted or no data)",
                resolved.display()
            );
        }
        Err(e) => {
            daemon_stats.errors.fetch_add(1, Ordering::Relaxed);
            eprintln!(
                "[git-ai daemon] error processing commit in {}: {}",
                resolved.display(),
                e
            );
        }
    }
}

fn emit_commit_telemetry(repo_path: &std::path::Path, telemetry: &TelemetryHandle) {
    use super::telemetry_types::{MetricEvent, MetricEventId, SparseArray};
    use std::process::{Command, Stdio};

    let git = |args: &[&str]| -> Option<String> {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo_path)
            .args(args)
            .env("GIT_TRACE2_EVENT", "0")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        if output.status.success() {
            Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
        } else {
            None
        }
    };

    let commit_sha = match git(&["rev-parse", "HEAD"]) {
        Some(sha) => sha,
        None => return,
    };

    let remote_url = git(&["remote", "get-url", "origin"]).unwrap_or_default();

    // Read the authorship note we just wrote
    let note_content = git(&["notes", "--ref=ai", "show", &commit_sha]);

    // Emit Committed metric event
    let mut values = SparseArray::new();
    values.insert("0".to_string(), serde_json::json!(1)); // commit count

    let mut attrs = SparseArray::new();
    attrs.insert("0".to_string(), serde_json::json!(env!("CARGO_PKG_VERSION")));
    if !remote_url.is_empty() {
        attrs.insert("1".to_string(), serde_json::json!(remote_url));
    }
    attrs.insert("2".to_string(), serde_json::json!(&commit_sha[..7.min(commit_sha.len())]));

    telemetry.submit_metric(MetricEvent::new(MetricEventId::Committed, values, attrs));

    // Upload authorship note as CAS object
    if let Some(ref note) = note_content {
        let content = serde_json::json!({
            "type": "authorship_note",
            "raw": note,
        });
        let mut metadata = std::collections::HashMap::new();
        metadata.insert("commit".to_string(), commit_sha);
        if !remote_url.is_empty() {
            metadata.insert("repo".to_string(), remote_url);
        }
        telemetry.submit_cas(content, metadata);
    }
}

fn dispatch_rewrite(
    resolved: &std::path::Path,
    kind: &RewriteKind,
    argv: &[String],
    daemon_stats: &stats::DaemonStats,
) {
    eprintln!(
        "[git-ai daemon] rewrite ({:?}) detected in {}",
        kind,
        resolved.display()
    );
    match rewrite_worker::process_rewrite(resolved, kind, argv) {
        Ok(copied) => {
            if copied > 0 {
                daemon_stats
                    .rewrites_processed
                    .fetch_add(1, Ordering::Relaxed);
                eprintln!(
                    "[git-ai daemon] rewrite: propagated {} note(s) in {}",
                    copied,
                    resolved.display()
                );
            }
        }
        Err(e) => {
            daemon_stats.errors.fetch_add(1, Ordering::Relaxed);
            eprintln!(
                "[git-ai daemon] error processing rewrite in {}: {}",
                resolved.display(),
                e
            );
        }
    }
}

fn dispatch_stash(resolved: &std::path::Path, argv: &[String], daemon_stats: &stats::DaemonStats) {
    eprintln!(
        "[git-ai daemon] stash push detected in {}",
        resolved.display()
    );
    match stash_worker::process_stash_push(resolved, argv) {
        Ok(()) => {
            eprintln!(
                "[git-ai daemon] stash: saved attributions in {}",
                resolved.display()
            );
        }
        Err(e) => {
            daemon_stats.errors.fetch_add(1, Ordering::Relaxed);
            eprintln!(
                "[git-ai daemon] error processing stash push in {}: {}",
                resolved.display(),
                e
            );
        }
    }
}

fn dispatch_stash_pop(resolved: &std::path::Path, daemon_stats: &stats::DaemonStats) {
    eprintln!(
        "[git-ai daemon] stash pop/apply detected in {}",
        resolved.display()
    );
    match stash_worker::process_stash_pop(resolved) {
        Ok(()) => {
            eprintln!(
                "[git-ai daemon] stash: restored attributions in {}",
                resolved.display()
            );
        }
        Err(e) => {
            daemon_stats.errors.fetch_add(1, Ordering::Relaxed);
            eprintln!(
                "[git-ai daemon] error processing stash pop in {}: {}",
                resolved.display(),
                e
            );
        }
    }
}
