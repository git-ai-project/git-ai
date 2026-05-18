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
/// BUG FIX: Changed to store a Vec to handle multiple rapid operations on the same repo.
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

    // Debounce buffer for rewrites: repo_path → Vec<pending rewrites>
    // BUG FIX: Changed to Vec to handle multiple rapid successive operations
    let mut pending_rewrites: HashMap<PathBuf, Vec<PendingRewrite>> = HashMap::new();

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
            .filter(|(_, pending_list)| {
                // Check if the oldest pending operation has expired
                pending_list
                    .first()
                    .map(|p| now.duration_since(p.scheduled_at) >= REWRITE_DEBOUNCE)
                    .unwrap_or(false)
            })
            .map(|(k, _)| k.clone())
            .collect();

        for repo_path in ready_keys {
            if let Some(pending_list) = pending_rewrites.remove(&repo_path) {
                // BUG FIX: Process ALL pending operations, not just the last one
                for pending in pending_list {
                    dispatch_rewrite(&repo_path, &pending.kind, &pending.argv, daemon_stats);
                }
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
                            // BUG FIX: Append to the list instead of overwriting
                            // Debounce rewrites: rebase generates many rapid events
                            pending_rewrites
                                .entry(resolved)
                                .or_default()
                                .push(PendingRewrite {
                                    kind: kind.clone(),
                                    argv: argv.clone(),
                                    scheduled_at: Instant::now(),
                                });
                        }
                        DetectedOperation::Stash {
                            ref repo_path,
                            ref argv,
                        } => {
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
    // BUG FIX: Process all pending operations in each repo's list
    for (repo_path, pending_list) in pending_rewrites.drain() {
        for pending in pending_list {
            dispatch_rewrite(&repo_path, &pending.kind, &pending.argv, daemon_stats);
        }
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
            sweep_and_upload_transcripts(resolved, telemetry);

            eprintln!(
                "[git-ai daemon] successfully processed commit in {}",
                resolved.display()
            );
        }
        Ok(false) => {
            daemon_stats.commits_skipped.fetch_add(1, Ordering::Relaxed);
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

    let git = |args: &[&str]| -> Option<String> {
        crate::git_cmd::git_in_repo(repo_path, args).ok()
    };

    let commit_sha = match git(&["rev-parse", "HEAD"]) {
        Some(sha) => sha,
        None => return,
    };

    let remote_url = strip_url_credentials(&git(&["remote", "get-url", "origin"]).unwrap_or_default());

    // Read the authorship note we just wrote
    let note_content = git(&["notes", "--ref=ai", "show", &commit_sha]);

    // Emit Committed metric event
    let mut values = SparseArray::new();
    values.insert("0".to_string(), serde_json::json!(1)); // commit count

    let mut attrs = SparseArray::new();
    attrs.insert(
        "0".to_string(),
        serde_json::json!(env!("CARGO_PKG_VERSION")),
    );
    if !remote_url.is_empty() {
        attrs.insert("1".to_string(), serde_json::json!(remote_url));
    }
    attrs.insert(
        "2".to_string(),
        serde_json::json!(&commit_sha[..7.min(commit_sha.len())]),
    );

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

fn sweep_and_upload_transcripts(repo_path: &std::path::Path, telemetry: &TelemetryHandle) {
    let git_dir = repo_path.join(".git");
    if !git_dir.is_dir() {
        return;
    }

    let updates = crate::transcripts::sweep::sweep_transcripts(&git_dir);
    for update in updates {
        if update.new_events.is_empty() {
            continue;
        }
        let content = serde_json::json!({
            "type": "transcript",
            "agent": update.agent,
            "session_id": update.session_id,
            "events": update.new_events,
        });
        let mut metadata = std::collections::HashMap::new();
        metadata.insert("agent".to_string(), update.agent.clone());
        metadata.insert("session_id".to_string(), update.session_id.clone());
        telemetry.submit_cas(content, metadata);
    }
}

/// Strip credentials from a git remote URL.
/// Handles both HTTPS URLs (https://user:token@host/...) and SSH URLs.
fn strip_url_credentials(url: &str) -> String {
    if let Some(rest) = url.strip_prefix("https://") {
        if let Some(at_pos) = rest.find('@') {
            return format!("https://{}", &rest[at_pos + 1..]);
        }
    } else if let Some(rest) = url.strip_prefix("http://") {
        if let Some(at_pos) = rest.find('@') {
            return format!("http://{}", &rest[at_pos + 1..]);
        }
    }
    url.to_string()
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
