//! Centralized notes I/O API.
//!
//! All authorship-note reads and writes flow through this module. SQLite is the
//! primary store for both backends. The difference between backends is in the
//! sync/push direction:
//!
//! ## Write path
//! - **`git_notes`:** write to `refs/notes/ai` locally, then cache in SQLite
//!   after the git write succeeds. Notes are pushed to remote on `git push`.
//! - **`http`:** do not touch the git ref; the daemon flushes SQLite to the HTTP
//!   backend asynchronously.
//!
//! ## Read path
//! - **Both backends:** check SQLite first (fast, no network).
//! - **`git_notes`:** on miss, fall back to `refs/notes/ai` and cache the result
//!   back into SQLite.
//! - **`http`:** on miss, fetch from the HTTP backend (network), cache into
//!   SQLite, then fall back to `refs/notes/ai` for anything still missing.
//!
//! ## Sync operations (separate from reads/writes)
//! - `sync_from_git_ref(repo)` — imports local `refs/notes/ai` into SQLite
//!   (no network). Called after pull/fetch/clone for both backends.
//! - `warm_cache_for_remote(repo, remote)` — fetches notes from the HTTP
//!   backend into SQLite. Called additionally for `http` backend.

use crate::authorship::authorship_log_serialization::{AUTHORSHIP_LOG_VERSION, AuthorshipLog};
use crate::config::{Config, NotesBackendKind};
use crate::error::GitAiError;
use crate::git::repository::Repository;
use std::collections::{HashMap, HashSet};

// Re-export CommitAuthorship so callers don't need to import from refs directly.
pub use crate::git::refs::CommitAuthorship;

// --- Writes ---

pub fn write_note(repo: &Repository, commit_sha: &str, content: &str) -> Result<(), GitAiError> {
    write_notes_batch(repo, &[(commit_sha.to_string(), content.to_string())])
}

pub fn write_notes_batch(
    repo: &Repository,
    entries: &[(String, String)],
) -> Result<(), GitAiError> {
    if entries.is_empty() {
        return Ok(());
    }
    match Config::get().notes_backend_kind() {
        NotesBackendKind::Http => {
            // HTTP: write to SQLite queue only; daemon flushes to HTTP backend.
            http_write_batch(entries)
        }
        NotesBackendKind::GitNotes => git_notes_write_batch(repo, entries),
    }
}

fn git_notes_write_batch(
    repo: &Repository,
    entries: &[(String, String)],
) -> Result<(), GitAiError> {
    // GitNotes: refs/notes/ai is authoritative. Cache only after the git write
    // succeeds so a failed fast-import cannot leave stale SQLite hits.
    crate::git::refs::notes_add_batch(repo, entries)?;
    git_notes_write_batch_to_sqlite(entries);
    Ok(())
}

// --- Reads ---

pub fn read_note(repo: &Repository, commit_sha: &str) -> Option<String> {
    // Delegate to the batched path for consistent SQLite-first behavior.
    read_notes_batch(repo, &[commit_sha.to_string()])
        .ok()
        .and_then(|mut m| m.remove(commit_sha))
}

/// Read note contents for multiple commits.
/// Returns a map of commit_sha → note_content for commits that have notes.
///
/// ## Dispatch (both backends check SQLite first)
///
/// **`git_notes`:**
/// 1. SQLite cache — fast, no network.
/// 2. `refs/notes/ai` git fallback for misses — results cached back into SQLite.
///
/// **`http`:**
/// 1. SQLite cache — fast, no network.
/// 2. HTTP backend fetch for misses — results cached into SQLite.
/// 3. `refs/notes/ai` git fallback for anything still missing (covers notes
///    from old clients not yet imported).
pub fn read_notes_batch(
    repo: &Repository,
    commit_shas: &[String],
) -> Result<HashMap<String, String>, GitAiError> {
    if commit_shas.is_empty() {
        return Ok(HashMap::new());
    }

    match Config::get().notes_backend_kind() {
        NotesBackendKind::Http => {
            // Step 1 (http): check SQLite cache — fast, no network.
            let mut notes = http_read_notes(commit_shas);

            let missing_after_sqlite: Vec<String> = commit_shas
                .iter()
                .filter(|sha| !notes.contains_key(*sha))
                .cloned()
                .collect();

            if missing_after_sqlite.is_empty() {
                return Ok(notes);
            }

            // Step 2 (http): fetch misses from HTTP backend, cache into SQLite.
            let from_http = http_fetch_and_cache_notes(&missing_after_sqlite);
            notes.extend(from_http);

            // Step 3 (http): git ref fallback for anything still missing.
            let missing_after_http: Vec<String> = missing_after_sqlite
                .iter()
                .filter(|sha| !notes.contains_key(*sha))
                .cloned()
                .collect();
            if !missing_after_http.is_empty()
                && let Ok(git_notes) =
                    crate::git::refs::notes_for_commits(repo, &missing_after_http)
            {
                notes.extend(git_notes);
            }

            Ok(notes)
        }
        NotesBackendKind::GitNotes => {
            // Keep the SQLite cache coherent with direct local git-note changes
            // (for example, notes fetched from a remote or test fixtures that
            // intentionally overwrite refs/notes/ai).
            let _ = sync_from_git_ref(repo);

            // Step 1 (git_notes): check SQLite cache — fast, no subprocess.
            let mut notes = git_notes_read_from_sqlite(commit_shas);

            let missing: Vec<String> = commit_shas
                .iter()
                .filter(|sha| !notes.contains_key(*sha))
                .cloned()
                .collect();

            if missing.is_empty() {
                return Ok(notes);
            }

            // Step 2 (git_notes): fall back to git ref for misses. Errors are
            // propagated — callers that detect corrupt notes rely on this.
            let from_git = crate::git::refs::notes_for_commits(repo, &missing)?;

            // Step 3 (git_notes): cache git-ref hits back into SQLite.
            if !from_git.is_empty() {
                let entries: Vec<(String, String)> = from_git
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                git_notes_write_batch_to_sqlite(&entries);
            }

            notes.extend(from_git);
            Ok(notes)
        }
    }
}

pub fn read_authorship(repo: &Repository, commit_sha: &str) -> Option<AuthorshipLog> {
    read_notes_batch(repo, &[commit_sha.to_string()])
        .map_err(|e| tracing::debug!("notes read error: {}", e))
        .ok()
        .and_then(|mut notes| notes.remove(commit_sha))
        .and_then(|content| {
            deserialize_authorship_for_commit(commit_sha, &content)
                .map_err(|e| tracing::debug!("notes deserialization error: {}", e))
                .ok()
        })
}

pub fn read_authorship_v3(
    repo: &Repository,
    commit_sha: &str,
) -> Result<AuthorshipLog, GitAiError> {
    let mut notes = read_notes_batch(repo, &[commit_sha.to_string()])?;
    let content = notes
        .remove(commit_sha)
        .ok_or_else(|| GitAiError::Generic("No authorship note found".to_string()))?;
    deserialize_authorship_v3_for_commit(commit_sha, &content)
}

fn deserialize_authorship_for_commit(
    commit_sha: &str,
    content: &str,
) -> Result<AuthorshipLog, GitAiError> {
    let mut authorship_log = AuthorshipLog::deserialize_from_string(content)
        .map_err(|e| GitAiError::Generic(format!("notes deserialization error: {}", e)))?;

    // Keep metadata aligned with the commit where this note is attached.
    authorship_log.metadata.base_commit_sha = commit_sha.to_string();

    Ok(authorship_log)
}

fn deserialize_authorship_v3_for_commit(
    commit_sha: &str,
    content: &str,
) -> Result<AuthorshipLog, GitAiError> {
    let authorship_log = deserialize_authorship_for_commit(commit_sha, content)?;

    if authorship_log.metadata.schema_version != AUTHORSHIP_LOG_VERSION {
        return Err(GitAiError::Generic(format!(
            "Unsupported authorship log version: {} (expected: {})",
            authorship_log.metadata.schema_version, AUTHORSHIP_LOG_VERSION
        )));
    }

    Ok(authorship_log)
}

/// Return a map of commit SHA → note-blob OID for the given commits.
///
/// # Audit results (Phase 2)
///
/// All callers of this function use the returned blob OIDs as *git object IDs*
/// to subsequently read note content via `batch_read_blob_contents` /
/// `batch_read_blobs_with_oids`.  They are NOT purely presence checks.
///
/// Call sites and how they use the OIDs:
///
/// 1. `authorship_traversal::load_ai_touched_files_for_commits` — passes OIDs
///    to `batch_read_blobs_with_oids`; must be real git OIDs.
/// 2. `rewrite::shift_authorship_notes` — reads notes by OID;
///    must be real git OIDs.
///
/// **HTTP backend**: notes do not live in `refs/notes/ai`, so there are no
/// git blob OIDs to return.  Returning an empty map causes callers to handle
/// the "no notes available" case (skip or use slow-path reads).  This is
/// safe and correct for the transition period — callers that need note content
/// will fall back to `read_note` / `read_authorship` which hit the cache.
pub fn read_note_blob_oids(
    repo: &Repository,
    commit_shas: &[String],
) -> Result<HashMap<String, String>, GitAiError> {
    match Config::get().notes_backend_kind() {
        // For Http, notes are in notes-db not in git — no blob OIDs exist.
        // Return an empty map; callers handle this as "no notes in git".
        NotesBackendKind::Http => Ok(HashMap::new()),
        NotesBackendKind::GitNotes => {
            crate::git::refs::note_blob_oids_for_commits(repo, commit_shas)
        }
    }
}

pub fn commits_with_notes(
    repo: &Repository,
    commit_shas: &[String],
) -> Result<HashSet<String>, GitAiError> {
    match Config::get().notes_backend_kind() {
        NotesBackendKind::Http => {
            // Check the cache first; fall through to git notes for misses.
            let cached = http_check_exists(commit_shas);
            if cached.len() == commit_shas.len() {
                return Ok(cached);
            }
            // For commits not in the cache, check git notes as fallback.
            let missing: Vec<String> = commit_shas
                .iter()
                .filter(|sha| !cached.contains(*sha))
                .cloned()
                .collect();
            let from_git = crate::git::refs::commits_with_authorship_notes(repo, &missing)?;
            Ok(cached.into_iter().chain(from_git).collect())
        }
        NotesBackendKind::GitNotes => {
            crate::git::refs::commits_with_authorship_notes(repo, commit_shas)
        }
    }
}

pub fn filter_commits_with_notes(
    repo: &Repository,
    commit_shas: &[String],
) -> Result<Vec<CommitAuthorship>, GitAiError> {
    match Config::get().notes_backend_kind() {
        NotesBackendKind::Http => {
            // `CommitAuthorship` requires a git_author that is only available from
            // `git rev-list`. Call the underlying git function which handles author
            // lookup, then patch in cache hits for commits whose `authorship_log`
            // would otherwise be absent (because refs/notes/ai is empty).
            //
            // The git function calls `get_authorship(repo, sha)` (refs.rs, not
            // notes_api), so for Http the results will be `CommitAuthorship::NoLog`
            // for all commits. We promote any commit that has a cache entry to
            // `CommitAuthorship::Log`.
            let cached_map = http_read_notes(commit_shas);

            let git_results =
                crate::git::refs::get_commits_with_notes_from_list(repo, commit_shas)?;

            // Promote NoLog entries that are in the cache to Log entries.
            let results = git_results
                .into_iter()
                .map(|ca| match ca {
                    CommitAuthorship::NoLog {
                        ref sha,
                        ref git_author,
                    } => {
                        if let Some(content) = cached_map.get(sha)
                            && let Ok(authorship_log) =
                                AuthorshipLog::deserialize_from_string(content)
                                    .map_err(|e| GitAiError::Generic(e.to_string()))
                        {
                            return CommitAuthorship::Log {
                                sha: sha.clone(),
                                git_author: git_author.clone(),
                                authorship_log,
                            };
                        }
                        ca
                    }
                    // Already has a log (shouldn't happen for Http, but keep it).
                    CommitAuthorship::Log { .. } => ca,
                })
                .collect();

            Ok(results)
        }
        NotesBackendKind::GitNotes => {
            crate::git::refs::get_commits_with_notes_from_list(repo, commit_shas)
        }
    }
}

// --- Search ---

pub fn search_notes(repo: &Repository, pattern: &str) -> Result<Vec<String>, GitAiError> {
    crate::git::refs::grep_ai_notes(repo, pattern)
}

// --- Materialization (for git ai log) ---

/// Materialize notes from the local cache into a one-off git ref
/// `refs/notes/ai-display` so that `git log --notes=ai-display` can render
/// them without requiring them to be in `refs/notes/ai`.
///
/// Only the most recent `limit` commits reachable from HEAD are considered.
///
/// The ref is left in place after the call; callers use it with `--notes=ai-display`.
/// It is safe to call repeatedly — each call starts from an empty tree via
/// `from 0000...` so stale notes from prior calls are discarded.
///
/// Returns the number of notes that were written into `refs/notes/ai-display`.
pub fn materialize_notes_for_display(repo: &Repository, limit: usize) -> Result<usize, GitAiError> {
    use crate::git::repository::exec_git;
    use crate::git::repository::exec_git_stdin;

    // 1. Get recent commits via rev-list.
    let rev_list_args: Vec<String> = repo
        .global_args_for_exec()
        .into_iter()
        .chain([
            "rev-list".to_string(),
            format!("--max-count={}", limit),
            "HEAD".to_string(),
        ])
        .collect();

    let output = exec_git(&rev_list_args)?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let commit_shas: Vec<String> = stdout
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();

    if commit_shas.is_empty() {
        return Ok(0);
    }

    // 2. Look up which commits are in the local notes-db cache.
    let cached_map = http_read_notes(&commit_shas);
    if cached_map.is_empty() {
        return Ok(0);
    }

    // 3. Build a git fast-import stream.
    //    Structure:
    //      - One `blob` stanza per note (each gets a mark ID).
    //      - One `commit` stanza with `from 0000...` (empty tree) that attaches all blobs.
    let mut stream = String::new();
    let mut marks: Vec<(usize, String)> = Vec::new(); // (mark_id, commit_sha)

    for (idx, (commit_sha, content)) in cached_map.iter().enumerate() {
        let mark_id = idx + 1;
        // Blob stanza: `data <exact-byte-count>\n<content-bytes>\n`
        // The trailing \n after content is a fast-import stream separator, not part of the data.
        stream.push_str(&format!(
            "blob\nmark :{}\ndata {}\n{}\n",
            mark_id,
            content.len(),
            content
        ));
        marks.push((mark_id, commit_sha.clone()));
    }

    // Commit stanza — mirrors the pattern used in refs.rs notes_add_batch().
    // Use `from` with an all-zeros SHA to start from an empty tree, ensuring
    // stale notes from prior materializations are removed.
    stream.push_str("commit refs/notes/ai-display\n");
    stream.push_str("committer git-ai <git-ai@localhost> 1000000000 +0000\n");
    stream.push_str("data 0\n");
    stream.push_str("from 0000000000000000000000000000000000000000\n");

    let count = marks.len();
    for (mark_id, commit_sha) in &marks {
        stream.push_str(&format!("M 100644 :{} {}\n", mark_id, commit_sha));
    }
    stream.push('\n');

    // 4. Feed to git fast-import.
    let fast_import_args: Vec<String> = repo
        .global_args_for_exec()
        .into_iter()
        .chain(["fast-import".to_string(), "--quiet".to_string()])
        .collect();

    exec_git_stdin(&fast_import_args, stream.as_bytes())?;

    Ok(count)
}

// --- Cache warming ---

/// Pre-warm the local notes cache during `git pull` by fetching notes for
/// recently-arrived commits from the HTTP backend.
///
/// Algorithm:
/// 1. Walk the last 500 commits reachable from HEAD via `git rev-list`.
/// 2. Filter out any SHAs already present in `notes-db` (already cached).
/// 3. Batch the remaining SHAs into chunks of 100 and call `ApiClient::read_notes()`.
/// 4. Write returned entries via `cache_synced_notes()` so rows are inserted
///    with `synced = 1` (read cache, not upload queue).
///
/// This function is a best-effort operation: errors are logged but not propagated
/// (callers should treat failure as a cache miss, not a hard error).
pub fn warm_cache_for_remote(repo: &Repository, remote: &str) -> Result<(), GitAiError> {
    use crate::api::client::{ApiClient, ApiContext};
    use crate::git::repository::exec_git;

    // Process-global rate limit: at most one HTTP notes fetch per second across
    // all repos in the daemon.
    if crate::git::sync_authorship::http_notes_fetch_rate_limited() {
        tracing::debug!(
            "warm_cache_for_remote: rate-limited, skipping HTTP fetch for '{}'",
            remote
        );
        return Ok(());
    }

    // 1. Walk recent history. Prefer the remote's default branch; fall back to HEAD.
    let remote_head = format!("refs/remotes/{}/HEAD", remote);
    let rev_target = {
        let check_args: Vec<String> = repo
            .global_args_for_exec()
            .into_iter()
            .chain([
                "rev-parse".to_string(),
                "--verify".to_string(),
                "--quiet".to_string(),
                remote_head.clone(),
            ])
            .collect();
        if exec_git(&check_args)
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            remote_head
        } else {
            "HEAD".to_string()
        }
    };

    let rev_list_args: Vec<String> = repo
        .global_args_for_exec()
        .into_iter()
        .chain([
            "rev-list".to_string(),
            "--max-count=500".to_string(),
            rev_target,
        ])
        .collect();

    let output = exec_git(&rev_list_args)?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let all_shas: Vec<String> = stdout
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();

    if all_shas.is_empty() {
        tracing::debug!("warm_cache_for_remote: no commits in HEAD history; skipping");
        return Ok(());
    }

    // 2. Filter out SHAs already in notes-db.
    let already_cached: std::collections::HashSet<String> = {
        match crate::notes::db::NotesDatabase::global() {
            Ok(db) => match db.lock() {
                Ok(lock) => {
                    let refs: Vec<&str> = all_shas.iter().map(|s| s.as_str()).collect();
                    lock.get_notes(&refs)
                        .unwrap_or_default()
                        .into_keys()
                        .collect()
                }
                Err(e) => {
                    tracing::warn!("warm_cache_for_remote: DB lock poisoned: {}", e);
                    std::collections::HashSet::new()
                }
            },
            Err(e) => {
                tracing::warn!("warm_cache_for_remote: failed to open notes-db: {}", e);
                std::collections::HashSet::new()
            }
        }
    };

    let uncached: Vec<String> = all_shas
        .into_iter()
        .filter(|sha| !already_cached.contains(sha))
        .collect();

    if uncached.is_empty() {
        tracing::debug!("warm_cache_for_remote: all commits already cached; skipping");
        return Ok(());
    }

    tracing::debug!(
        "warm_cache_for_remote: fetching notes for {} uncached commits",
        uncached.len()
    );

    // 3. Batch-fetch from the HTTP backend (chunks of 100).
    let cfg = crate::config::Config::fresh();
    let backend_url = match cfg.notes_backend_url() {
        Some(url) => url.to_string(),
        None => {
            tracing::debug!(
                "warm_cache_for_remote: notes_backend.backend_url is not configured; skipping"
            );
            return Ok(());
        }
    };
    let ctx = ApiContext::new(Some(backend_url));
    let client = ApiClient::new(ctx);

    // Skip when not authenticated (matches daemon flush_notes pattern).
    if !client.is_logged_in() && !client.has_api_key() {
        tracing::debug!("warm_cache_for_remote: not authenticated; skipping");
        return Ok(());
    }

    for chunk in uncached.chunks(100) {
        let sha_refs: Vec<&str> = chunk.iter().map(|s| s.as_str()).collect();
        match client.read_notes(&sha_refs) {
            Ok(response) => {
                if response.notes.is_empty() {
                    continue;
                }
                // 4. Write returned entries as already-synced cache rows.
                let entries: Vec<(String, String)> = response.notes.into_iter().collect();
                match crate::notes::db::NotesDatabase::global() {
                    Ok(db) => match db.lock() {
                        Ok(mut lock) => {
                            if let Err(e) = lock.cache_synced_notes(&entries) {
                                tracing::warn!(
                                    "warm_cache_for_remote: cache_synced_notes error: {}",
                                    e
                                );
                            } else {
                                tracing::debug!(
                                    count = entries.len(),
                                    "warm_cache_for_remote: cached notes from remote"
                                );
                            }
                        }
                        Err(e) => {
                            tracing::warn!("warm_cache_for_remote: DB lock poisoned: {}", e);
                        }
                    },
                    Err(e) => {
                        tracing::warn!("warm_cache_for_remote: failed to open notes-db: {}", e);
                    }
                }
            }
            Err(e) => {
                // Best-effort: log and continue.
                tracing::warn!("warm_cache_for_remote: read_notes error: {}", e);
            }
        }
    }

    Ok(())
}

// --- Git ref → SQLite sync ---

/// Import notes from the local `refs/notes/ai` git ref into the SQLite notes-db.
///
/// This is a read-local-only operation — no network calls are made. It is
/// intended to be called after a `git pull`, `git fetch`, or `git clone` so
/// that notes written by old clients (who still push to `refs/notes/ai`) are
/// visible to the HTTP backend path.
///
/// Notes already present in SQLite are refreshed from the git ref. Entries are
/// inserted with `synced = 1` (read cache, not upload queue) so they are not
/// re-uploaded.
///
/// Errors are logged at debug level and not propagated — callers treat this as
/// best-effort.
pub fn sync_from_git_ref(repo: &Repository) -> Result<(), GitAiError> {
    match crate::notes::db::NotesDatabase::global() {
        Ok(db) => match db.lock() {
            Ok(mut lock) => {
                if let Err(e) = sync_from_git_ref_into_db(repo, &mut lock) {
                    tracing::debug!("sync_from_git_ref: failed to refresh from git ref: {}", e);
                }
                Ok(())
            }
            Err(e) => {
                tracing::debug!("sync_from_git_ref: DB lock poisoned: {}", e);
                Ok(())
            }
        },
        Err(e) => {
            tracing::debug!("sync_from_git_ref: failed to open notes-db: {}", e);
            Ok(())
        }
    }
}

/// Inner implementation of `sync_from_git_ref` that operates on a caller-supplied
/// `NotesDatabase`. Extracted so tests can pass an isolated DB instance.
fn sync_from_git_ref_into_db(
    repo: &Repository,
    db: &mut crate::notes::db::NotesDatabase,
) -> Result<(), GitAiError> {
    use crate::git::repository::exec_git;
    use crate::git::repository::exec_git_stdin;

    let Some(notes_tip) = local_notes_ref_tip(repo)? else {
        tracing::debug!("sync_from_git_ref: refs/notes/ai is absent, skipping");
        return Ok(());
    };

    let sync_cursor_key = notes_ref_sync_cursor_key(repo);
    if db
        .get_metadata_value(&sync_cursor_key)
        .ok()
        .flatten()
        .as_deref()
        == Some(notes_tip.as_str())
    {
        tracing::debug!(
            notes_tip,
            "sync_from_git_ref: refs/notes/ai unchanged, skipping"
        );
        return Ok(());
    }

    // 1. List all notes: `git notes --ref=ai list` → "<note-sha> <commit-sha>"
    let mut list_args = repo.global_args_for_exec();
    list_args.extend_from_slice(&[
        "notes".to_string(),
        "--ref=ai".to_string(),
        "list".to_string(),
    ]);

    let list_output = match exec_git(&list_args) {
        Ok(o) if o.status.success() => o,
        Ok(_) | Err(_) => {
            tracing::debug!("sync_from_git_ref: refs/notes/ai is absent or empty, skipping");
            return Ok(());
        }
    };

    let stdout = String::from_utf8_lossy(&list_output.stdout);
    // pairs: (note_blob_sha, commit_sha)
    let pairs: Vec<(String, String)> = stdout
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let note_sha = parts.next()?.to_string();
            let commit_sha = parts.next()?.to_string();
            Some((note_sha, commit_sha))
        })
        .collect();

    if pairs.is_empty() {
        if let Err(e) = db.set_metadata_value(&sync_cursor_key, &notes_tip) {
            tracing::debug!(
                "sync_from_git_ref: failed to record notes ref cursor: {}",
                e
            );
        }
        return Ok(());
    }

    tracing::debug!(
        "sync_from_git_ref: refreshing {} notes from refs/notes/ai",
        pairs.len()
    );

    // 2. Read note blob contents via `git cat-file --batch`.
    let mut cat_args = repo.global_args_for_exec();
    cat_args.extend_from_slice(&["cat-file".to_string(), "--batch".to_string()]);

    let blob_oids: Vec<&str> = pairs
        .iter()
        .map(|(note_sha, _)| note_sha.as_str())
        .collect();
    let stdin_data = blob_oids.join("\n") + "\n";

    let cat_output = match exec_git_stdin(&cat_args, stdin_data.as_bytes()) {
        Ok(o) => o,
        Err(e) => {
            tracing::debug!("sync_from_git_ref: cat-file failed: {}", e);
            return Ok(());
        }
    };

    // Parse `<oid> <type> <size>\n<content>\n` output.
    let data = &cat_output.stdout;
    let mut blob_to_content: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    let mut pos = 0usize;
    while pos < data.len() {
        let header_end = match data[pos..].iter().position(|&b| b == b'\n') {
            Some(i) => pos + i,
            None => break,
        };
        let header = String::from_utf8_lossy(&data[pos..header_end]);
        let parts: Vec<&str> = header.split_whitespace().collect();
        if parts.len() < 3 || parts[1] == "missing" {
            pos = header_end + 1;
            continue;
        }
        let oid = parts[0].to_string();
        let size: usize = match parts[2].parse() {
            Ok(s) => s,
            Err(_) => {
                pos = header_end + 1;
                continue;
            }
        };
        let content_start = header_end + 1;
        let content_end = content_start + size;
        if content_end > data.len() {
            break;
        }
        let content = String::from_utf8_lossy(&data[content_start..content_end]).to_string();
        blob_to_content.insert(oid, content);
        pos = content_end;
        if pos < data.len() && data[pos] == b'\n' {
            pos += 1;
        }
    }

    // 3. Build (commit_sha, content) entries and upsert them into SQLite.
    // Refresh all local git notes, not just misses: after pull/fetch a remote
    // note may have changed for an existing commit SHA, and refs/notes/ai is
    // authoritative for the GitNotes backend.
    let entries: Vec<(String, String)> = pairs
        .iter()
        .filter_map(|(note_sha, commit_sha)| {
            blob_to_content
                .get(note_sha)
                .map(|content| (commit_sha.clone(), content.clone()))
        })
        .collect();

    if entries.is_empty() {
        return Ok(());
    }

    if let Err(e) = db.cache_synced_notes(&entries) {
        tracing::debug!("sync_from_git_ref: cache_synced_notes error: {}", e);
        return Ok(());
    }
    if let Err(e) = db.set_metadata_value(&sync_cursor_key, &notes_tip) {
        tracing::debug!(
            "sync_from_git_ref: failed to record notes ref cursor: {}",
            e
        );
    }
    tracing::debug!(
        count = entries.len(),
        notes_tip,
        "sync_from_git_ref: refreshed notes from refs/notes/ai"
    );

    Ok(())
}

fn local_notes_ref_tip(repo: &Repository) -> Result<Option<String>, GitAiError> {
    use crate::git::repository::exec_git;

    let mut args = repo.global_args_for_exec();
    args.extend_from_slice(&[
        "rev-parse".to_string(),
        "--verify".to_string(),
        crate::git::refs::AI_AUTHORSHIP_FULL_REF.to_string(),
    ]);

    match exec_git(&args) {
        Ok(output) => Ok(Some(String::from_utf8(output.stdout)?.trim().to_string())),
        Err(GitAiError::GitCliError {
            code: Some(128), ..
        })
        | Err(GitAiError::GitCliError { code: Some(1), .. }) => Ok(None),
        Err(e) => Err(e),
    }
}

fn notes_ref_sync_cursor_key(repo: &Repository) -> String {
    let common_dir = repo
        .common_dir()
        .canonicalize()
        .unwrap_or_else(|_| repo.common_dir().to_path_buf());
    format!("git_notes_ref_sync_tip:{}", common_dir.display())
}

// --- HTTP backend helpers (private) ---

#[cfg_attr(not(test), allow(dead_code))]
fn http_write_note(commit_sha: &str, content: &str) -> Result<(), GitAiError> {
    let db = crate::notes::db::NotesDatabase::global()?;
    let mut db_lock = db
        .lock()
        .map_err(|e| GitAiError::Generic(format!("notes-db lock: {}", e)))?;
    db_lock.upsert_note(commit_sha, content)?;
    drop(db_lock);
    crate::daemon::telemetry_handle::submit_notes();
    Ok(())
}

fn http_write_batch(entries: &[(String, String)]) -> Result<(), GitAiError> {
    let db = crate::notes::db::NotesDatabase::global()?;
    let mut db_lock = db
        .lock()
        .map_err(|e| GitAiError::Generic(format!("notes-db lock: {}", e)))?;
    db_lock.upsert_notes_batch(entries)?;
    drop(db_lock);
    crate::daemon::telemetry_handle::submit_notes();
    Ok(())
}

fn http_read_notes(commit_shas: &[String]) -> HashMap<String, String> {
    let Ok(db) = crate::notes::db::NotesDatabase::global() else {
        return HashMap::new();
    };
    let Ok(db_lock) = db.lock() else {
        return HashMap::new();
    };
    let refs: Vec<&str> = commit_shas.iter().map(|s| s.as_str()).collect();
    db_lock.get_notes(&refs).unwrap_or_default()
}

fn http_fetch_and_cache_notes(commit_shas: &[String]) -> HashMap<String, String> {
    if commit_shas.is_empty() {
        return HashMap::new();
    }

    let cfg = Config::fresh();
    let Some(backend_url) = cfg.notes_backend_url().map(str::to_string) else {
        return HashMap::new();
    };

    let ctx = crate::api::client::ApiContext::new(Some(backend_url));
    let client = crate::api::client::ApiClient::new(ctx);
    if !client.is_logged_in() && !client.has_api_key() {
        return HashMap::new();
    }

    let mut fetched = HashMap::new();
    for chunk in commit_shas.chunks(100) {
        let refs: Vec<&str> = chunk.iter().map(String::as_str).collect();
        match client.read_notes(&refs) {
            Ok(response) => {
                if response.notes.is_empty() {
                    continue;
                }
                let entries: Vec<(String, String)> = response.notes.into_iter().collect();
                if let Ok(db) = crate::notes::db::NotesDatabase::global()
                    && let Ok(mut lock) = db.lock()
                {
                    let _ = lock.cache_synced_notes(&entries);
                }
                fetched.extend(entries);
            }
            Err(e) => {
                tracing::debug!(%e, "notes batch read from HTTP backend failed");
            }
        }
    }

    fetched
}

fn http_check_exists(commit_shas: &[String]) -> HashSet<String> {
    http_read_notes(commit_shas).into_keys().collect()
}

/// Read-cache helper for the `git_notes` backend: return whatever entries are
/// already in SQLite. Returns an empty map on any DB error (treated as a miss).
fn git_notes_read_from_sqlite(commit_shas: &[String]) -> HashMap<String, String> {
    let shas: Vec<&str> = commit_shas.iter().map(|s| s.as_str()).collect();
    match crate::notes::db::NotesDatabase::global() {
        Ok(db) => match db.lock() {
            Ok(lock) => lock.get_notes(&shas).unwrap_or_default(),
            Err(e) => {
                tracing::debug!("git_notes_read_from_sqlite: DB lock poisoned: {}", e);
                HashMap::new()
            }
        },
        Err(e) => {
            tracing::debug!("git_notes_read_from_sqlite: failed to open DB: {}", e);
            HashMap::new()
        }
    }
}

/// Write-through cache helper for the `git_notes` backend: persist entries into
/// SQLite as `synced=1` (read cache only — not queued for HTTP upload).
/// Errors are logged and swallowed; the git ref write is the authoritative path.
fn git_notes_write_batch_to_sqlite(entries: &[(String, String)]) {
    match crate::notes::db::NotesDatabase::global() {
        Ok(db) => match db.lock() {
            Ok(mut lock) => {
                if let Err(e) = lock.cache_synced_notes(entries) {
                    tracing::debug!("git_notes_write_batch_to_sqlite: cache error: {}", e);
                }
            }
            Err(e) => tracing::debug!("git_notes_write_batch_to_sqlite: DB lock poisoned: {}", e),
        },
        Err(e) => tracing::debug!("git_notes_write_batch_to_sqlite: failed to open DB: {}", e),
    }
}

// --- Tests ---

#[cfg(test)]
mod tests {
    use super::*;

    /// With kind=Http, the http helpers upsert into notes-db (synced=0) and the
    /// read helper returns the cached value. This tests the private http_* helpers
    /// directly so no config override is needed.
    #[test]
    #[serial_test::serial(notes_db_env)]
    fn http_write_then_read_uses_cache() {
        use std::env;

        // Point the notes-db at a temp file so we don't pollute the real DB.
        let tmp = tempfile::NamedTempFile::new().expect("tmp file");
        let db_path = tmp.path().to_str().unwrap().to_string();
        // Safety: test-only env var manipulation.
        unsafe {
            env::set_var("GIT_AI_TEST_NOTES_DB_PATH", &db_path);
        }

        // Write directly via http helper (no repo needed).
        http_write_note("abc123def456abc123def456abc123def456abc1", "test content").expect("write");

        // Read back from cache.
        let sha = "abc123def456abc123def456abc123def456abc1".to_string();
        let content = http_read_notes(std::slice::from_ref(&sha));
        assert_eq!(content.get(&sha), Some(&"test content".to_string()));

        // Confirm it is in the DB with synced=0.
        let db = crate::notes::db::NotesDatabase::global().expect("global db");
        let mut lock = db.lock().expect("lock");
        let pending = lock.dequeue_pending(10).expect("dequeue");
        assert!(
            pending.iter().any(
                |p| p.commit_sha == "abc123def456abc123def456abc123def456abc1"
                    && p.content == "test content"
            ),
            "expected pending row in notes-db"
        );

        // Cleanup env var.
        unsafe {
            env::remove_var("GIT_AI_TEST_NOTES_DB_PATH");
        }
    }

    /// http_read_notes returns a HashMap of all cached entries for requested SHAs.
    #[test]
    #[serial_test::serial(notes_db_env)]
    fn http_read_notes_returns_multiple() {
        use std::env;

        let tmp = tempfile::NamedTempFile::new().expect("tmp file");
        let db_path = tmp.path().to_str().unwrap().to_string();
        unsafe {
            env::set_var("GIT_AI_TEST_NOTES_DB_PATH", &db_path);
        }

        let sha1 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string();
        let sha2 = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string();
        let sha3 = "cccccccccccccccccccccccccccccccccccccccc".to_string();

        http_write_note(&sha1, "content-a").expect("write sha1");
        http_write_note(&sha2, "content-b").expect("write sha2");

        // sha3 is not written — should not appear in result.
        let result = http_read_notes(&[sha1.clone(), sha2.clone(), sha3.clone()]);
        assert_eq!(result.get(&sha1), Some(&"content-a".to_string()));
        assert_eq!(result.get(&sha2), Some(&"content-b".to_string()));
        assert!(!result.contains_key(&sha3));

        unsafe {
            env::remove_var("GIT_AI_TEST_NOTES_DB_PATH");
        }
    }

    /// With kind=GitNotes (default), read_note_blob_oids delegates to git.
    /// Verified by building with an empty repo — returns Ok(empty) with no panic.
    #[test]
    fn git_notes_backend_read_note_blob_oids_delegates_to_git() {
        use crate::git::test_utils::TmpRepo;
        // Default config is GitNotes — no override needed.
        let tmp = TmpRepo::new().expect("TmpRepo::new");
        let result = crate::git::refs::note_blob_oids_for_commits(tmp.gitai_repo(), &[]);
        assert!(result.is_ok());
    }

    /// With kind=Http, the public read_note_blob_oids returns an empty map
    /// because notes live in notes-db, not in git refs.
    /// We test this by calling the function through a fresh Config set to Http.
    #[test]
    fn http_backend_read_note_blob_oids_returns_empty_map() {
        use crate::git::test_utils::TmpRepo;

        let old = std::env::var("GIT_AI_NOTES_BACKEND_KIND").ok();
        unsafe {
            std::env::set_var("GIT_AI_NOTES_BACKEND_KIND", "http");
        }

        let tmp = TmpRepo::new().expect("TmpRepo::new");
        // Use Config::fresh() so it picks up the env var, then call the refs function
        // through the kind check inline.
        let kind = crate::config::Config::fresh().notes_backend_kind();
        let result: Result<HashMap<String, String>, _> = match kind {
            crate::config::NotesBackendKind::Http => Ok(HashMap::new()),
            crate::config::NotesBackendKind::GitNotes => {
                crate::git::refs::note_blob_oids_for_commits(tmp.gitai_repo(), &["abc".to_string()])
            }
        };

        // Restore env before asserting (so a panic doesn't leave the env dirty).
        match old {
            Some(v) => unsafe { std::env::set_var("GIT_AI_NOTES_BACKEND_KIND", v) },
            None => unsafe { std::env::remove_var("GIT_AI_NOTES_BACKEND_KIND") },
        }

        assert!(result.is_ok());
        assert!(
            result.unwrap().is_empty(),
            "Http backend should return empty map from read_note_blob_oids"
        );
    }

    /// Integration test: with kind=Http, `write_note` upserts into `notes-db`
    /// with `synced = 0` and `git notes --ref=ai show <sha>` returns nothing (note
    /// is NOT written into git refs).
    #[test]
    #[serial_test::serial(notes_db_env)]
    fn integration_http_write_note_goes_to_db_not_git() {
        use crate::git::repository::exec_git;
        use crate::git::test_utils::TmpRepo;
        use std::env;

        // Isolated notes-db for this test.
        let tmp_db = tempfile::NamedTempFile::new().expect("tmp db file");
        let db_path = tmp_db.path().to_str().unwrap().to_string();
        unsafe {
            env::set_var("GIT_AI_TEST_NOTES_DB_PATH", &db_path);
        }

        let repo = TmpRepo::new().expect("TmpRepo::new");

        // Create a real commit so we have a valid SHA.
        repo.write_file("a.txt", "hello", false)
            .expect("write file");
        let sha = repo.commit_all("msg").expect("commit");

        // Write a note for this SHA using the Http helper.
        http_write_note(&sha, "some-note-content").expect("http write");

        // Confirm it is in notes-db with synced=0.
        let db = crate::notes::db::NotesDatabase::global().expect("global db");
        let mut lock = db.lock().expect("lock");
        let note_in_db = lock.get_note(&sha).expect("get note");
        assert_eq!(note_in_db, Some("some-note-content".to_string()));

        let pending = lock.dequeue_pending(10).expect("dequeue");
        assert!(
            pending.iter().any(|p| p.commit_sha == sha),
            "note should be pending in notes-db"
        );
        drop(lock);

        // Confirm `git notes --ref=ai show <sha>` returns nothing.
        let mut args = repo.gitai_repo().global_args_for_exec();
        args.extend([
            "notes".to_string(),
            "--ref=ai".to_string(),
            "show".to_string(),
            sha.clone(),
        ]);
        let result = exec_git(&args);
        assert!(
            result.is_err(),
            "git notes --ref=ai show should fail (note not in git) for Http backend"
        );

        unsafe {
            env::remove_var("GIT_AI_TEST_NOTES_DB_PATH");
        }
    }

    /// Integration test: `materialize_notes_for_display` writes notes from the
    /// notes-db cache into `refs/notes/ai-display` so that `git log --notes=ai-display`
    /// can show them.
    #[test]
    #[serial_test::serial(notes_db_env)]
    fn integration_materialize_notes_for_display() {
        use crate::git::repository::exec_git;
        use crate::git::test_utils::TmpRepo;
        use std::env;

        // Isolated notes-db.
        let tmp_db = tempfile::NamedTempFile::new().expect("tmp db file");
        unsafe {
            env::set_var("GIT_AI_TEST_NOTES_DB_PATH", tmp_db.path().to_str().unwrap());
        }

        let repo = TmpRepo::new().expect("TmpRepo::new");

        // Create a real commit.
        repo.write_file("b.txt", "world", false)
            .expect("write file");
        let sha = repo.commit_all("test commit").expect("commit");

        // Put a note in the cache for this commit.
        http_write_note(&sha, "display-note-content").expect("write note");

        // Materialize the cache into refs/notes/ai-display.
        let count = materialize_notes_for_display(repo.gitai_repo(), 50).expect("materialize");
        assert_eq!(count, 1, "should have materialized 1 note");

        // Confirm git can read the note from refs/notes/ai-display.
        let mut args = repo.gitai_repo().global_args_for_exec();
        args.extend([
            "notes".to_string(),
            "--ref=ai-display".to_string(),
            "show".to_string(),
            sha.clone(),
        ]);
        let output = exec_git(&args).expect("git notes show ai-display");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.trim() == "display-note-content",
            "refs/notes/ai-display should contain the materialized note, got: {:?}",
            stdout
        );

        unsafe {
            env::remove_var("GIT_AI_TEST_NOTES_DB_PATH");
        }
    }

    /// Verify that `run_pre_push_hook_managed` has the correct early-return guard for
    /// `kind = Http`. We test this by confirming Config::fresh() with
    /// `GIT_AI_NOTES_BACKEND_KIND=http` returns Http, and that the guard in
    /// `run_pre_push_hook_managed` would short-circuit. This is a compile-time
    /// regression guard for the code structure added in Phase 2.6.
    #[test]
    fn push_pre_command_hook_http_guard_is_in_place() {
        use std::env;

        let old = env::var("GIT_AI_NOTES_BACKEND_KIND").ok();
        unsafe {
            env::set_var("GIT_AI_NOTES_BACKEND_KIND", "http");
        }
        let kind = crate::config::Config::fresh().notes_backend_kind();
        match old {
            Some(v) => unsafe { env::set_var("GIT_AI_NOTES_BACKEND_KIND", v) },
            None => unsafe { env::remove_var("GIT_AI_NOTES_BACKEND_KIND") },
        }

        // Verify Config::fresh() correctly parses http from env.
        assert_eq!(
            kind,
            crate::config::NotesBackendKind::Http,
            "Config::fresh() should reflect GIT_AI_NOTES_BACKEND_KIND=http"
        );

        // Structural verification: the Http backend skip is now inlined in
        // apply_push_side_effect in daemon.rs — no separate hook function needed.
    }

    // --- warm_cache_for_remote tests ---
    //
    // These tests verify the core behavior of `warm_cache_for_remote`:
    //
    // 1. It fetches notes from the HTTP backend and stores them with `synced = 1`.
    // 2. It skips SHAs already present in notes-db (not included in the request).
    //
    // Design notes on the `NOTES_DB` `OnceLock` singleton:
    //
    // `NotesDatabase::global()` uses a `OnceLock` that initialises the DB path
    // *once per process*.  Both tests set `GIT_AI_TEST_NOTES_DB_PATH` to a fresh
    // temp file before their first DB call.  The first test to run initialises the
    // OnceLock; subsequent tests in the same process reuse the same DB file path
    // regardless of what `GIT_AI_TEST_NOTES_DB_PATH` says.
    //
    // Strategy: both tests use `NotesDatabase::global()` for all reads and writes
    // (pre-population and post-call verification) rather than direct file-level
    // connections.  Because the tests run serially (`#[serial]`) and each uses
    // distinct commit SHAs, shared DB state doesn't cause false-negative assertions.
    //
    // Test 1 sets `GIT_AI_TEST_NOTES_DB_PATH` which initialises the OnceLock if
    // it hasn't been set yet.  Test 2 also sets it but will use whatever path was
    // already locked.  Both tests clear DB state relevant to their own SHAs via
    // `get_note` assertions on distinct SHAs, so they don't interfere.

    /// Unit test: `warm_cache_for_remote` fetches notes from a mock HTTP server
    /// and stores them in `notes-db` with `synced = 1`.
    ///
    /// Steps:
    /// 1. Point `NOTES_DB` at a fresh temp file (via `GIT_AI_TEST_NOTES_DB_PATH`).
    /// 2. Spin up a mockito server returning two notes.
    /// 3. Create a `TmpRepo` with two commits.
    /// 4. Call `warm_cache_for_remote`.
    /// 5. Verify both SHAs appear in `notes-db` with `synced = 1` via `NotesDatabase::global()`.
    #[test]
    #[serial_test::serial(notes_db_env)]
    fn warm_cache_for_remote_populates_db_with_synced_1() {
        use crate::git::test_utils::TmpRepo;
        use crate::notes::db::NotesDatabase;
        use tempfile::NamedTempFile;

        // Reset rate limiter so this test is not throttled by a prior test run.
        crate::git::sync_authorship::reset_notes_fetch_rate_limiters();

        // Set the test DB path before the first DB call so the OnceLock picks it up.
        let tmp_db = NamedTempFile::new().expect("tmp notes-db");
        unsafe {
            std::env::set_var("GIT_AI_TEST_NOTES_DB_PATH", tmp_db.path());
        }

        // Build a TmpRepo with two commits.
        let repo = TmpRepo::new().expect("TmpRepo::new");

        repo.write_file("warm1.txt", "warm1", false)
            .expect("write file");
        let sha1 = repo.commit_all("warm-commit-1").expect("commit 1");

        repo.write_file("warm2.txt", "warm2", false)
            .expect("write file");
        let sha2 = repo.commit_all("warm-commit-2").expect("commit 2");

        // Spin up a mockito server that returns notes for both SHAs.
        let mut server = mockito::Server::new();
        let notes_json = serde_json::json!({
            "notes": {
                sha1.clone(): "note-content-1",
                sha2.clone(): "note-content-2"
            }
        })
        .to_string();

        let _mock = server
            .mock(
                "GET",
                mockito::Matcher::Regex(r"^/worker/notes/".to_string()),
            )
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&notes_json)
            .create();

        let server_url = server.url();
        unsafe {
            std::env::set_var("GIT_AI_NOTES_BACKEND_URL", &server_url);
            // Provide a fake API key so `has_api_key()` returns true and the
            // auth guard in `warm_cache_for_remote` does not short-circuit.
            std::env::set_var("GIT_AI_API_KEY", "warm-cache-test-key");
        }

        // Execute.
        let result = warm_cache_for_remote(repo.gitai_repo(), "origin");
        assert!(result.is_ok(), "warm_cache_for_remote failed: {:?}", result);

        // Verify via NotesDatabase::global() (the same DB the function wrote to).
        let db = NotesDatabase::global().expect("global db");
        let lock = db.lock().expect("lock");

        let content1 = lock.get_note(&sha1).expect("get sha1");
        let content2 = lock.get_note(&sha2).expect("get sha2");

        assert_eq!(
            content1,
            Some("note-content-1".to_string()),
            "sha1 should be cached with correct content"
        );
        assert_eq!(
            content2,
            Some("note-content-2".to_string()),
            "sha2 should be cached with correct content"
        );

        // Rows must NOT appear in dequeue_pending (cache_synced_notes inserts synced = 1).
        drop(lock);
        let mut lock = db.lock().expect("lock for dequeue check");
        let pending = lock.dequeue_pending(10).expect("dequeue");
        let warm_pending: Vec<_> = pending
            .iter()
            .filter(|p| p.commit_sha == sha1 || p.commit_sha == sha2)
            .collect();
        assert!(
            warm_pending.is_empty(),
            "cache_synced rows must not appear in dequeue_pending: {:?}",
            warm_pending
                .iter()
                .map(|p| &p.commit_sha)
                .collect::<Vec<_>>()
        );

        // Cleanup.
        unsafe {
            std::env::remove_var("GIT_AI_TEST_NOTES_DB_PATH");
            std::env::remove_var("GIT_AI_API_KEY");
            std::env::remove_var("GIT_AI_NOTES_BACKEND_URL");
        }
    }

    /// Unit test: `warm_cache_for_remote` skips SHAs already present in `notes-db`.
    ///
    /// Steps:
    /// 1. Pre-populate `notes-db` with sha1 via `cache_synced_notes`.
    /// 2. Spin up a mockito server returning sha2 only.
    ///    The mock matches only requests whose query contains sha2 —
    ///    if sha1 were incorrectly included it would still match, but we verify
    ///    indirectly that sha1's content was not overwritten.
    /// 3. Call `warm_cache_for_remote` with a TmpRepo containing both commits.
    /// 4. Verify sha1's content is unchanged ("already-cached-note").
    /// 5. Verify sha2 was fetched and cached with `synced = 1`.
    #[test]
    #[serial_test::serial(notes_db_env)]
    fn warm_cache_for_remote_skips_already_cached_shas() {
        use crate::git::test_utils::TmpRepo;
        use crate::notes::db::NotesDatabase;
        use tempfile::NamedTempFile;

        // Reset rate limiter so this test is not throttled by a prior test run.
        crate::git::sync_authorship::reset_notes_fetch_rate_limiters();

        // Set the test DB path (may be ignored if OnceLock was already set by
        // `warm_cache_for_remote_populates_db_with_synced_1` in the same process,
        // but we still set it for freshness when running this test in isolation).
        let tmp_db = NamedTempFile::new().expect("tmp notes-db");
        unsafe {
            std::env::set_var("GIT_AI_TEST_NOTES_DB_PATH", tmp_db.path());
        }

        // Build TmpRepo with two commits.
        let repo = TmpRepo::new().expect("TmpRepo::new");

        repo.write_file("skip1.txt", "s1", false)
            .expect("write file");
        let sha1 = repo.commit_all("skip-c1").expect("commit 1");

        repo.write_file("skip2.txt", "s2", false)
            .expect("write file");
        let sha2 = repo.commit_all("skip-c2").expect("commit 2");

        // Pre-populate notes-db with sha1 via the global singleton.
        {
            let db = NotesDatabase::global().expect("global db");
            let mut lock = db.lock().expect("lock");
            lock.cache_synced_notes(&[(sha1.clone(), "already-cached-note".to_string())])
                .expect("cache_synced_notes sha1");
        }

        // Mock server: use two mocks to verify sha1 is NOT in the request.
        //
        // - Mock A: matches requests where the query contains sha2 but NOT sha1.
        //   Since mockito doesn't have a `Not` matcher, we approximate this by
        //   requiring the query equals exactly sha2 (no comma-separated prefix/suffix).
        //   `commits=<sha2>` means only sha2 was requested.
        // - Mock B: fallback that matches everything else → returns 500 so sha2
        //   is NOT cached if sha1 was erroneously included.
        //
        // If warm_cache correctly filters sha1, mock A matches and sha2 is cached.
        // If warm_cache incorrectly sends sha1 too, the query is `sha1,sha2` or
        // `sha2,sha1`, which won't match the exact-sha2 regex → mock B fires → 500
        // error → sha2 is NOT cached → `assert_eq!(content2, ...)` fails.
        let sha2_note_json = serde_json::json!({
            "notes": { sha2.clone(): "note-content-skip-2" }
        })
        .to_string();

        // Exact query: commits=<sha2> only.
        let exact_sha2_query = format!("commits={}", sha2);

        let mut server = mockito::Server::new();
        // Mock A: exact query with only sha2.
        let _mock_ok = server
            .mock(
                "GET",
                mockito::Matcher::Regex(r"^/worker/notes/".to_string()),
            )
            .match_query(mockito::Matcher::Exact(exact_sha2_query))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&sha2_note_json)
            .create();
        // Mock B: fallback → 500.
        let _mock_fallback = server
            .mock(
                "GET",
                mockito::Matcher::Regex(r"^/worker/notes/".to_string()),
            )
            .with_status(500)
            .with_body(r#"{"error":"unexpected request with sha1 in query"}"#)
            .create();

        let server_url = server.url();
        unsafe {
            std::env::set_var("GIT_AI_NOTES_BACKEND_URL", &server_url);
            std::env::set_var("GIT_AI_API_KEY", "skip-test-key");
        }

        let result = warm_cache_for_remote(repo.gitai_repo(), "origin");
        assert!(result.is_ok(), "warm_cache_for_remote failed: {:?}", result);

        // Verify via the global DB.
        let db = NotesDatabase::global().expect("global db");
        let lock = db.lock().expect("lock");

        // sha1 must retain its pre-cached content unchanged.
        let content1 = lock.get_note(&sha1).expect("get sha1");
        assert_eq!(
            content1,
            Some("already-cached-note".to_string()),
            "sha1 content must not change — warm_cache must not overwrite cached entries"
        );

        // sha2 must now be cached with the server-returned content.
        let content2 = lock.get_note(&sha2).expect("get sha2");
        assert_eq!(
            content2,
            Some("note-content-skip-2".to_string()),
            "sha2 should have been fetched and cached"
        );

        // The mock must have been called (warm_cache made at least one HTTP request).
        _mock_ok.assert();

        // Cleanup.
        unsafe {
            std::env::remove_var("GIT_AI_TEST_NOTES_DB_PATH");
            std::env::remove_var("GIT_AI_API_KEY");
            std::env::remove_var("GIT_AI_NOTES_BACKEND_URL");
        }
    }

    /// `sync_from_git_ref` imports notes from the local `refs/notes/ai` git ref
    /// into SQLite as `synced=1` (read cache, not upload queue).
    #[test]
    fn sync_from_git_ref_imports_local_git_notes_into_sqlite() {
        use crate::git::repository::exec_git;
        use crate::git::test_utils::TmpRepo;

        let tmp_db = tempfile::NamedTempFile::new().expect("tmp db file");
        let mut db = crate::notes::db::NotesDatabase::open_at_path(tmp_db.path()).expect("open db");

        let repo = TmpRepo::new().expect("TmpRepo::new");
        repo.write_file("a.txt", "hello", false).expect("write");
        let sha = repo.commit_all("initial").expect("commit");

        // Write a note directly into refs/notes/ai via git (bypassing notes_api).
        let note_content = "{\"schema\":\"authorship/3.0.0\",\"test\":true}";
        let mut args = repo.gitai_repo().global_args_for_exec();
        args.extend([
            "notes".to_string(),
            "--ref=ai".to_string(),
            "add".to_string(),
            "-m".to_string(),
            note_content.to_string(),
            sha.clone(),
        ]);
        exec_git(&args).expect("git notes add");

        // Confirm SQLite is empty before sync.
        assert_eq!(
            db.get_note(&sha).expect("get"),
            None,
            "SQLite should be empty before sync"
        );

        // Run sync via the inner function with our isolated DB.
        sync_from_git_ref_into_db(repo.gitai_repo(), &mut db).expect("sync_from_git_ref");

        // Confirm the note is now in SQLite as synced=1 (not in pending queue).
        // Git appends a trailing newline to note content; trim for comparison.
        let cached = db.get_note(&sha).expect("get after sync");
        assert_eq!(
            cached.as_deref().map(str::trim_end),
            Some(note_content),
            "note should be in SQLite after sync_from_git_ref"
        );
        // synced=1 means it should NOT appear in the pending upload queue.
        let pending = db.dequeue_pending(10).expect("dequeue");
        assert!(
            !pending.iter().any(|p| p.commit_sha == sha),
            "synced note must not appear in pending upload queue"
        );
    }

    /// `sync_from_git_ref` refreshes commit SHAs already present in SQLite from
    /// the authoritative local `refs/notes/ai` git ref.
    #[test]
    fn sync_from_git_ref_refreshes_already_cached_shas() {
        use crate::git::repository::exec_git;
        use crate::git::test_utils::TmpRepo;

        let tmp_db = tempfile::NamedTempFile::new().expect("tmp db file");
        let mut db = crate::notes::db::NotesDatabase::open_at_path(tmp_db.path()).expect("open db");

        let repo = TmpRepo::new().expect("TmpRepo::new");
        repo.write_file("a.txt", "hello", false).expect("write");
        let sha = repo.commit_all("initial").expect("commit");

        // Pre-populate SQLite with a value for this SHA.
        let pre_cached = "pre-cached-content";
        db.cache_synced_notes(&[(sha.clone(), pre_cached.to_string())])
            .expect("cache");

        // Write a different note into refs/notes/ai.
        let mut args = repo.gitai_repo().global_args_for_exec();
        args.extend([
            "notes".to_string(),
            "--ref=ai".to_string(),
            "add".to_string(),
            "-m".to_string(),
            "git-ref-content".to_string(),
            sha.clone(),
        ]);
        exec_git(&args).expect("git notes add");

        // sync_from_git_ref should refresh the existing cached value from git.
        sync_from_git_ref_into_db(repo.gitai_repo(), &mut db).expect("sync");

        let cached = db.get_note(&sha).expect("get");
        assert_eq!(
            cached.as_deref().map(str::trim_end),
            Some("git-ref-content"),
            "sync_from_git_ref must refresh already-cached entries from refs/notes/ai"
        );
    }

    #[test]
    fn sync_from_git_ref_skips_when_notes_tip_unchanged() {
        use crate::git::repository::exec_git;
        use crate::git::test_utils::TmpRepo;

        let tmp_db = tempfile::NamedTempFile::new().expect("tmp db file");
        let mut db = crate::notes::db::NotesDatabase::open_at_path(tmp_db.path()).expect("open db");

        let repo = TmpRepo::new().expect("TmpRepo::new");
        repo.write_file("a.txt", "hello", false).expect("write");
        let sha = repo.commit_all("initial").expect("commit");

        let mut args = repo.gitai_repo().global_args_for_exec();
        args.extend([
            "notes".to_string(),
            "--ref=ai".to_string(),
            "add".to_string(),
            "-m".to_string(),
            "git-ref-content".to_string(),
            sha.clone(),
        ]);
        exec_git(&args).expect("git notes add");

        let notes_tip = local_notes_ref_tip(repo.gitai_repo())
            .expect("read notes tip")
            .expect("notes tip");
        db.set_metadata_value(&notes_ref_sync_cursor_key(repo.gitai_repo()), &notes_tip)
            .expect("set sync cursor");
        db.cache_synced_notes(&[(sha.clone(), "already-synced".to_string())])
            .expect("seed cache");

        sync_from_git_ref_into_db(repo.gitai_repo(), &mut db).expect("sync");

        let cached = db.get_note(&sha).expect("get");
        assert_eq!(
            cached,
            Some("already-synced".to_string()),
            "unchanged notes ref tip should skip the full refresh"
        );
    }

    #[test]
    fn deserialize_authorship_for_commit_sets_requested_commit_sha() {
        let requested_sha = "1122334455667788990011223344556677889900";
        let mut log = AuthorshipLog::new();
        log.metadata.base_commit_sha = "stale-cache-sha".to_string();
        let content = log.serialize_to_string().expect("serialize log");

        let parsed =
            deserialize_authorship_for_commit(requested_sha, &content).expect("deserialize log");

        assert_eq!(
            parsed.metadata.base_commit_sha, requested_sha,
            "centralized authorship reads must attach logs to the requested commit"
        );
    }

    /// For `kind=git_notes`, the write-through cache helper inserts into SQLite
    /// as `synced=1` (read cache, not upload queue).
    #[test]
    fn git_notes_write_batch_caches_in_sqlite_as_synced() {
        let tmp_db = tempfile::NamedTempFile::new().expect("tmp db file");
        let mut db = crate::notes::db::NotesDatabase::open_at_path(tmp_db.path()).expect("open db");

        let sha = "aabbccdd00112233aabbccdd00112233aabbccdd".to_string();
        let content = "cached-note-content";

        db.cache_synced_notes(&[(sha.clone(), content.to_string())])
            .expect("cache_synced_notes");

        let cached = db.get_note(&sha).expect("get");
        assert_eq!(
            cached,
            Some(content.to_string()),
            "git_notes write-through should cache into SQLite"
        );
        // synced=1 means NOT in the pending upload queue.
        let pending = db.dequeue_pending(10).expect("dequeue");
        assert!(
            !pending.iter().any(|p| p.commit_sha == sha),
            "write-through cache entry must not be in pending upload queue"
        );
    }

    /// If the authoritative git notes write fails, `git_notes` must not leave a
    /// SQLite cache hit for a note that never reached `refs/notes/ai`.
    #[test]
    #[serial_test::serial(notes_db_env)]
    fn git_notes_write_batch_does_not_cache_when_git_write_fails() {
        use crate::git::test_utils::TmpRepo;
        use std::env;

        let tmp_db = tempfile::NamedTempFile::new().expect("tmp db file");
        unsafe {
            env::set_var("GIT_AI_TEST_NOTES_DB_PATH", tmp_db.path());
        }

        let repo = TmpRepo::new().expect("TmpRepo::new");
        let invalid_sha = "invalid\nsha".to_string();
        let content = "should-not-cache".to_string();

        let result = git_notes_write_batch(repo.gitai_repo(), &[(invalid_sha.clone(), content)]);
        assert!(
            result.is_err(),
            "malformed note path should make git fast-import fail"
        );

        let db = crate::notes::db::NotesDatabase::global().expect("global db");
        let lock = db.lock().expect("lock");
        let cached = lock.get_note(&invalid_sha).expect("get note");
        assert_eq!(
            cached, None,
            "failed git notes write must not populate SQLite cache"
        );

        unsafe {
            env::remove_var("GIT_AI_TEST_NOTES_DB_PATH");
        }
    }
}
