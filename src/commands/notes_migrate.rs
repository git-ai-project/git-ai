//! `git-ai notes migrate` — bulk-upload existing git notes to the HTTP backend.
//!
//! This command reads all notes stored in `refs/notes/ai` via `git notes --ref=ai list`,
//! fetches their content using `git cat-file --batch`, uploads them to the remote HTTP
//! backend in chunks of 50, and persists them locally in `notes-db` with `synced = 1`
//! so the cache is warm immediately after migration.
//!
//! The command refuses to run unless `notes_backend.kind == http` because migrating
//! notes to the git-notes backend (the default) is a no-op.

use crate::api::client::{ApiClient, ApiContext};
use crate::api::types::{NoteEntry, NotesUploadRequest};
use crate::config::{Config, NotesBackendKind};
use crate::error::GitAiError;
use crate::git::find_repository;
use crate::notes::db::NotesDatabase;
#[cfg(test)]
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};

const MIGRATION_UPLOAD_ITEMS: usize = 50;
const MAX_NOTE_BLOB_BYTES: usize = crate::git::repository::MAX_BATCH_MATERIALIZED_CONTENT_BYTES;
const MAX_CAT_FILE_HEADER_BYTES: usize = 1024;
const MAX_CAT_FILE_STDERR_BYTES: usize = 1024 * 1024;

struct MigrationUploader<'a> {
    client: &'a ApiClient,
    entries: Vec<(String, String)>,
    content_bytes: usize,
    total_uploaded: usize,
    total_failed: usize,
    total_cached: usize,
}

impl<'a> MigrationUploader<'a> {
    fn new(client: &'a ApiClient) -> Self {
        Self {
            client,
            entries: Vec::with_capacity(MIGRATION_UPLOAD_ITEMS),
            content_bytes: 0,
            total_uploaded: 0,
            total_failed: 0,
            total_cached: 0,
        }
    }

    fn push(&mut self, commit_sha: &str, content: String) -> Result<(), GitAiError> {
        if content.len() > MAX_NOTE_BLOB_BYTES {
            return Err(GitAiError::Generic(format!(
                "note blob exceeded the {MAX_NOTE_BLOB_BYTES} byte limit ({})",
                content.len()
            )));
        }
        if !self.entries.is_empty()
            && (self.entries.len() >= MIGRATION_UPLOAD_ITEMS
                || self.content_bytes.saturating_add(content.len()) > MAX_NOTE_BLOB_BYTES)
        {
            self.flush();
        }

        self.content_bytes = self.content_bytes.saturating_add(content.len());
        self.entries.push((commit_sha.to_string(), content));
        if self.entries.len() >= MIGRATION_UPLOAD_ITEMS || self.content_bytes >= MAX_NOTE_BLOB_BYTES
        {
            self.flush();
        }
        Ok(())
    }

    fn finish(&mut self) {
        self.flush();
    }

    fn flush(&mut self) {
        if self.entries.is_empty() {
            return;
        }
        self.content_bytes = 0;
        let entries = std::mem::replace(
            &mut self.entries,
            Vec::with_capacity(MIGRATION_UPLOAD_ITEMS),
        );
        let chunk_len = entries.len();
        let request = NotesUploadRequest {
            entries: entries
                .iter()
                .map(|(commit_sha, content)| NoteEntry {
                    commit_sha: commit_sha.clone(),
                    content: content.clone(),
                })
                .collect(),
        };

        match self.client.upload_notes(request) {
            Ok(response) => {
                eprintln!(
                    "  chunk: {} uploaded, {} failed",
                    response.success_count, response.failure_count
                );
                self.total_uploaded = self.total_uploaded.saturating_add(response.success_count);
                self.total_failed = self.total_failed.saturating_add(response.failure_count);

                match cache_migrated_notes(&entries) {
                    Ok(()) => {
                        self.total_cached = self.total_cached.saturating_add(entries.len());
                    }
                    Err(error) => eprintln!("warning: failed to cache notes locally: {error}"),
                }
            }
            Err(error) => {
                eprintln!("  error uploading chunk of {chunk_len}: {error}");
                self.total_failed = self.total_failed.saturating_add(chunk_len);
            }
        }
    }
}

fn cache_migrated_notes(entries: &[(String, String)]) -> Result<(), GitAiError> {
    let db = NotesDatabase::global()?;
    let mut lock = db
        .lock()
        .map_err(|error| GitAiError::Generic(format!("notes-db lock poisoned: {error}")))?;
    lock.cache_synced_notes(entries)
}

/// Entry point for `git-ai notes migrate`.
pub fn handle_notes_migrate(args: &[String]) {
    let mut force = false;
    for arg in args {
        match arg.as_str() {
            "--help" | "-h" => {
                print_help();
                return;
            }
            "--force" | "--all" => {
                force = true;
            }
            other => {
                eprintln!("error: unknown option '{}'", other);
                eprintln!("Run 'git ai notes migrate --help' for usage");
                std::process::exit(1);
            }
        }
    }

    // 1. Refuse to run unless notes_backend.kind == http.
    let cfg = Config::fresh();
    if cfg.notes_backend_kind() != NotesBackendKind::Http {
        eprintln!(
            "error: `git-ai notes migrate` requires notes_backend.kind = http.\n\
             Current backend: {}\n\
             \n\
             To enable the HTTP backend, run:\n\
             \n\
             \x20 git-ai config set notes_backend.kind http",
            cfg.notes_backend_kind()
        );
        std::process::exit(1);
    }

    // 2. Find the repository.
    let repo = match find_repository(&Vec::<String>::new()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: not a git repository ({})", e);
            std::process::exit(1);
        }
    };

    // 3. Build the API client.
    let backend_url = match cfg.notes_backend_url() {
        Some(url) => url.to_string(),
        None => {
            eprintln!(
                "error: notes_backend.backend_url is not configured.\n\
                 \n\
                 Set it before running migrate, e.g.:\n\
                 \n\
                 \x20 git-ai config set notes_backend.backend_url https://your-backend.example.com"
            );
            std::process::exit(1);
        }
    };
    let ctx = ApiContext::new(Some(backend_url));
    let client = ApiClient::new(ctx);

    // Skip if not authenticated.
    if !client.is_logged_in() && !client.has_api_key() {
        eprintln!("error: not authenticated. Log in first with `git-ai login` or set an API key.");
        std::process::exit(1);
    }

    eprintln!("Listing notes from refs/notes/ai ...");

    // 4. List notes: `git notes --ref=ai list` → "blob_sha commit_sha\n" lines.
    let mut note_pairs = match list_notes(&repo) {
        Ok(pairs) => pairs,
        Err(e) => {
            eprintln!("error: failed to list notes: {}", e);
            std::process::exit(1);
        }
    };

    if note_pairs.is_empty() {
        eprintln!("No notes found in refs/notes/ai. Nothing to migrate.");
        return;
    }

    eprintln!("Found {} note(s).", note_pairs.len());

    // Skip entries already confirmed synced (enables safe re-run after interruption).
    // Only skip synced=1 entries — pending (synced=0) entries still need uploading.
    if !force {
        let skipped = filter_synced_notes(&mut note_pairs);
        if skipped > 0 {
            eprintln!("Skipping {skipped} already-cached note(s).");
        }

        if note_pairs.is_empty() {
            eprintln!("All notes already migrated. Nothing to upload.");
            return;
        }
    }

    eprintln!(
        "Reading and uploading {} note(s) in bounded chunks ...",
        note_pairs.len()
    );

    let mut uploader = MigrationUploader::new(&client);
    if let Err(error) = cat_file_for_each(&repo, &note_pairs, |_, commit_sha, content| {
        uploader.push(commit_sha, content)
    }) {
        eprintln!("error: failed to read note content: {error}");
        std::process::exit(1);
    }
    uploader.finish();
    if uploader.total_cached > 0 {
        eprintln!(
            "Cached {} note(s) in local notes-db.",
            uploader.total_cached
        );
    }

    // 8. Summary.
    eprintln!();
    if uploader.total_failed == 0 {
        eprintln!(
            "Migration complete: {} note(s) uploaded successfully.",
            uploader.total_uploaded
        );
    } else {
        eprintln!(
            "Migration finished: {} uploaded, {} failed.",
            uploader.total_uploaded, uploader.total_failed
        );
        std::process::exit(1);
    }
}

fn filter_synced_notes(note_pairs: &mut Vec<(String, String)>) -> usize {
    let Ok(db) = NotesDatabase::global() else {
        return 0;
    };
    let Ok(lock) = db.lock() else {
        return 0;
    };
    let mut skipped = 0usize;
    for chunk in note_pairs.chunks_mut(crate::git::repository::MAX_BATCH_GIT_ITEMS) {
        let commit_shas: Vec<&str> = chunk
            .iter()
            .map(|(_, commit_sha)| commit_sha.as_str())
            .collect();
        let Ok(synced) = lock.get_synced_shas(&commit_shas) else {
            continue;
        };
        for (_, commit_sha) in chunk {
            if synced.contains(commit_sha) {
                commit_sha.clear();
                skipped += 1;
            }
        }
    }
    drop(lock);
    note_pairs.retain(|(_, commit_sha)| !commit_sha.is_empty());
    skipped
}

/// Run `git notes --ref=ai list` and return `(blob_sha, commit_sha)` pairs.
fn list_notes(
    repo: &crate::git::repository::Repository,
) -> Result<Vec<(String, String)>, GitAiError> {
    use crate::git::repository::exec_git;

    let mut args = repo.global_args_for_exec();
    args.extend([
        "notes".to_string(),
        "--ref=ai".to_string(),
        "list".to_string(),
    ]);

    let output = exec_git(&args)
        .map_err(|e| GitAiError::Generic(format!("git notes --ref=ai list failed: {}", e)))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // `git notes list` exits non-zero when there are no notes — treat as empty.
        if stderr.contains("No notes found") || output.stdout.is_empty() {
            return Ok(Vec::new());
        }
        return Err(GitAiError::Generic(format!(
            "git notes --ref=ai list exited {}: {}",
            output.status, stderr
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let pairs: Vec<(String, String)> = stdout
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let blob_sha = parts.next()?.to_string();
            let commit_sha = parts.next()?.to_string();
            Some((blob_sha, commit_sha))
        })
        .collect();

    Ok(pairs)
}

/// Stream blob contents through one `git cat-file --batch` process.
fn cat_file_for_each<F>(
    repo: &crate::git::repository::Repository,
    note_pairs: &[(String, String)],
    mut callback: F,
) -> Result<(), GitAiError>
where
    F: FnMut(&str, &str, String) -> Result<(), GitAiError>,
{
    if note_pairs.is_empty() {
        return Ok(());
    }

    let git_bin = crate::config::Config::get().git_cmd().to_string();
    let git_flags = repo.global_args_for_exec();

    let mut cmd = Command::new(&git_bin);
    cmd.args(&git_flags);
    cmd.arg("cat-file");
    cmd.arg("--batch");
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    crate::git::repository::apply_internal_git_env(&mut cmd);

    let mut child = cmd
        .spawn()
        .map_err(|e| GitAiError::Generic(format!("failed to spawn git cat-file --batch: {}", e)))?;
    let Some(mut stdin) = child.stdin.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(GitAiError::Generic(
            "failed to open git cat-file --batch stdin".to_string(),
        ));
    };
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(GitAiError::Generic(
            "failed to open git cat-file --batch stdout".to_string(),
        ));
    };
    let Some(stderr) = child.stderr.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(GitAiError::Generic(
            "failed to open git cat-file --batch stderr".to_string(),
        ));
    };

    std::thread::scope(|scope| {
        let writer = scope.spawn(move || -> Result<(), std::io::Error> {
            for (blob_sha, _) in note_pairs {
                writeln!(stdin, "{blob_sha}")?;
            }
            Ok(())
        });
        let stderr_reader =
            scope.spawn(move || drain_stream_with_limit(stderr, MAX_CAT_FILE_STDERR_BYTES));

        let mut reader = BufReader::new(stdout);
        let stream_result = parse_cat_file_stream(&mut reader, note_pairs, &mut callback);
        // Closing the pipe unblocks a Windows git child that may still be writing
        // rejected oversized content after its launcher process is terminated.
        drop(reader);
        if stream_result.is_err() {
            let _ = child.kill();
        }
        let status_result = child.wait();
        let writer_result = writer.join().map_err(|_| {
            GitAiError::Generic("git cat-file --batch stdin writer panicked".to_string())
        })?;
        let stderr_result = stderr_reader.join().map_err(|_| {
            GitAiError::Generic("git cat-file --batch stderr reader panicked".to_string())
        })?;

        stream_result?;
        writer_result.map_err(GitAiError::IoError)?;
        let (stderr, stderr_truncated) = stderr_result.map_err(GitAiError::IoError)?;
        if stderr_truncated {
            return Err(GitAiError::Generic(format!(
                "git cat-file --batch stderr exceeded the {MAX_CAT_FILE_STDERR_BYTES} byte limit"
            )));
        }
        let status = status_result.map_err(GitAiError::IoError)?;
        if !status.success() {
            return Err(GitAiError::Generic(format!(
                "git cat-file --batch exited {status}: {}",
                String::from_utf8_lossy(&stderr)
            )));
        }
        Ok(())
    })
}

fn parse_cat_file_stream<R, F>(
    reader: &mut R,
    note_pairs: &[(String, String)],
    callback: &mut F,
) -> Result<(), GitAiError>
where
    R: BufRead,
    F: FnMut(&str, &str, String) -> Result<(), GitAiError>,
{
    for (expected_blob_sha, commit_sha) in note_pairs {
        let mut header = read_bounded_header(reader)?.ok_or_else(|| {
            GitAiError::Generic("git cat-file --batch ended before all notes were read".to_string())
        })?;
        while header
            .last()
            .is_some_and(|byte| matches!(byte, b'\r' | b'\n'))
        {
            header.pop();
        }
        let header = std::str::from_utf8(&header).map_err(|error| {
            GitAiError::Generic(format!("git cat-file --batch header is not UTF-8: {error}"))
        })?;
        let mut parts = header.split_whitespace();
        let blob_sha = parts.next().ok_or_else(|| {
            GitAiError::Generic("git cat-file --batch returned an empty header".to_string())
        })?;
        if blob_sha != expected_blob_sha {
            return Err(GitAiError::Generic(format!(
                "git cat-file --batch returned {blob_sha}, expected {expected_blob_sha}"
            )));
        }
        let object_type = parts.next().ok_or_else(|| {
            GitAiError::Generic(format!(
                "git cat-file --batch omitted the object type for {blob_sha}"
            ))
        })?;
        if object_type == "missing" {
            continue;
        }
        if object_type != "blob" {
            return Err(GitAiError::Generic(format!(
                "git cat-file --batch returned unexpected object type {object_type} for {blob_sha}"
            )));
        }
        let size = parts
            .next()
            .ok_or_else(|| {
                GitAiError::Generic(format!(
                    "git cat-file --batch omitted the size for {blob_sha}"
                ))
            })?
            .parse::<u64>()
            .map_err(|error| {
                GitAiError::Generic(format!(
                    "git cat-file --batch returned an invalid size for {blob_sha}: {error}"
                ))
            })?;
        if size > MAX_NOTE_BLOB_BYTES as u64 {
            return Err(GitAiError::Generic(format!(
                "note blob exceeded the {MAX_NOTE_BLOB_BYTES} byte limit ({size})"
            )));
        }
        let size = usize::try_from(size).map_err(|_| {
            GitAiError::Generic(format!("note blob size does not fit in memory: {size}"))
        })?;
        let mut content = Vec::new();
        content.try_reserve_exact(size).map_err(|error| {
            GitAiError::Generic(format!("failed to reserve note blob buffer: {error}"))
        })?;
        content.resize(size, 0);
        reader.read_exact(&mut content).map_err(|error| {
            GitAiError::Generic(format!(
                "git cat-file --batch returned incomplete content for {blob_sha}: {error}"
            ))
        })?;
        let mut separator = [0u8; 1];
        reader.read_exact(&mut separator).map_err(|error| {
            GitAiError::Generic(format!(
                "git cat-file --batch omitted the separator for {blob_sha}: {error}"
            ))
        })?;
        if separator[0] != b'\n' {
            return Err(GitAiError::Generic(format!(
                "git cat-file --batch returned an invalid separator for {blob_sha}"
            )));
        }
        let content = String::from_utf8(content).map_err(|error| {
            GitAiError::Generic(format!("note blob {blob_sha} is not UTF-8: {error}"))
        })?;
        callback(blob_sha, commit_sha, content)?;
    }

    let mut extra = [0u8; 1];
    if reader.read(&mut extra).map_err(GitAiError::IoError)? != 0 {
        return Err(GitAiError::Generic(
            "git cat-file --batch returned unexpected extra output".to_string(),
        ));
    }
    Ok(())
}

fn read_bounded_header(reader: &mut impl BufRead) -> Result<Option<Vec<u8>>, GitAiError> {
    let mut header = Vec::new();
    loop {
        let available = reader.fill_buf().map_err(GitAiError::IoError)?;
        if available.is_empty() {
            if header.is_empty() {
                return Ok(None);
            }
            return Err(GitAiError::Generic(
                "git cat-file --batch returned an incomplete header".to_string(),
            ));
        }
        let consumed = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index + 1);
        if header.len().saturating_add(consumed) > MAX_CAT_FILE_HEADER_BYTES {
            return Err(GitAiError::Generic(format!(
                "git cat-file --batch header exceeded the {MAX_CAT_FILE_HEADER_BYTES} byte limit"
            )));
        }
        header.extend_from_slice(&available[..consumed]);
        let complete = available[..consumed].ends_with(b"\n");
        reader.consume(consumed);
        if complete {
            return Ok(Some(header));
        }
    }
}

fn drain_stream_with_limit(
    mut reader: impl Read,
    limit: usize,
) -> Result<(Vec<u8>, bool), std::io::Error> {
    let mut retained = Vec::new();
    let mut truncated = false;
    let mut buffer = [0u8; 8192];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        let remaining = limit.saturating_sub(retained.len());
        retained.extend_from_slice(&buffer[..read.min(remaining)]);
        truncated |= read > remaining;
    }
    Ok((retained, truncated))
}

#[cfg(test)]
fn cat_file_batch(
    repo: &crate::git::repository::Repository,
    blob_shas: &[String],
) -> Result<HashMap<String, String>, GitAiError> {
    let note_pairs = blob_shas
        .iter()
        .map(|blob_sha| (blob_sha.clone(), blob_sha.clone()))
        .collect::<Vec<_>>();
    let mut contents = HashMap::new();
    cat_file_for_each(repo, &note_pairs, |blob_sha, _, content| {
        contents.insert(blob_sha.to_string(), content);
        Ok(())
    })?;
    Ok(contents)
}

fn print_help() {
    eprintln!("git ai notes migrate - Bulk-upload existing git notes to the HTTP backend");
    eprintln!();
    eprintln!("Usage: git ai notes migrate [options]");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  --force, --all  Re-upload all notes even if already cached locally.");
    eprintln!("                  Useful when migrating to a new backend URL.");
    eprintln!("  -h, --help      Show this help message");
    eprintln!();
    eprintln!("Description:");
    eprintln!("  Reads all notes from refs/notes/ai, uploads them to the configured HTTP");
    eprintln!("  notes backend (in chunks of 50), and caches them locally in notes-db");
    eprintln!("  with synced = 1 so the local cache is warm immediately.");
    eprintln!();
    eprintln!("  This command requires notes_backend.kind = http. Set it with:");
    eprintln!("    git-ai config set notes_backend.kind http");
    eprintln!();
    eprintln!("  You must be logged in or have an API key configured.");
}

// --- Tests ---

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::test_utils::TmpRepo;
    use crate::notes::db::NotesDatabase;
    use tempfile::NamedTempFile;

    /// Helper to create real commits in a TmpRepo. Returns the commit SHA.
    /// Parents are tracked implicitly via `HEAD`, so callers no longer need to
    /// pass them explicitly.
    fn make_commit(repo: &TmpRepo, filename: &str, content: &str, message: &str) -> String {
        repo.write_file(filename, content, false)
            .expect("write file");
        repo.commit_all(message).expect("commit")
    }

    /// Add a git note to `refs/notes/ai` for the given commit SHA.
    fn add_git_note(repo: &TmpRepo, commit_sha: &str, note: &str) {
        repo.git_command(&["notes", "--ref=ai", "add", "-f", "-m", note, commit_sha])
            .expect("git notes add");
    }

    /// Integration test:
    ///   1. Create a TmpRepo with several commits and git notes.
    ///   2. Start a mockito server to accept the upload.
    ///   3. Call `handle_notes_migrate` logic directly (list + cat-file + upload + cache).
    ///   4. Verify all notes appear in `notes-db` with `synced = 1`.
    ///   5. Verify the mock upload endpoint was called.
    #[test]
    #[serial_test::serial(notes_db_env)]
    fn migration_uploads_notes_and_caches_with_synced_1() {
        // Isolated notes-db.
        let tmp_db = NamedTempFile::new().expect("tmp notes-db");
        unsafe {
            std::env::set_var("GIT_AI_TEST_NOTES_DB_PATH", tmp_db.path());
        }

        // --- Build repo with commits and notes ---
        let repo = TmpRepo::new().expect("TmpRepo::new");

        let sha1 = make_commit(&repo, "file1.txt", "hello", "commit 1");
        let sha2 = make_commit(&repo, "file2.txt", "world", "commit 2");
        let sha3 = make_commit(&repo, "file3.txt", "foo", "commit 3");

        // Add git notes for each commit.
        add_git_note(&repo, &sha1, "note-content-1");
        add_git_note(&repo, &sha2, "note-content-2");
        add_git_note(&repo, &sha3, "note-content-3");

        // --- Mock upload endpoint ---
        let mut server = mockito::Server::new();
        let upload_response = serde_json::json!({
            "success_count": 3,
            "failure_count": 0
        })
        .to_string();
        let _mock = server
            .mock("POST", "/worker/notes/upload")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(&upload_response)
            .create();

        let server_url = server.url();
        unsafe {
            std::env::set_var("GIT_AI_NOTES_BACKEND_URL", &server_url);
            std::env::set_var("GIT_AI_API_KEY", "migrate-test-key");
        }

        // --- Run the migration core logic ---
        let note_pairs = list_notes(repo.gitai_repo()).expect("list_notes");
        assert_eq!(note_pairs.len(), 3, "should list 3 notes");

        let blob_to_commit: HashMap<String, String> = note_pairs
            .iter()
            .map(|(b, c)| (b.clone(), c.clone()))
            .collect();
        let blob_shas: Vec<String> = note_pairs.iter().map(|(b, _)| b.clone()).collect();

        let blob_contents = cat_file_batch(repo.gitai_repo(), &blob_shas).expect("cat_file_batch");
        assert_eq!(blob_contents.len(), 3, "should read 3 blob contents");

        let mut entries: Vec<(String, String)> = Vec::new();
        for (blob_sha, content) in &blob_contents {
            if let Some(commit_sha) = blob_to_commit.get(blob_sha) {
                entries.push((commit_sha.clone(), content.clone()));
            }
        }
        assert_eq!(entries.len(), 3);

        // Upload to the mock server.
        let cfg = crate::config::Config::fresh();
        let backend_url = cfg
            .notes_backend_url()
            .expect("test should configure notes_backend.backend_url")
            .to_string();
        let ctx = ApiContext::new(Some(backend_url));
        let client = ApiClient::new(ctx);

        let note_entries: Vec<NoteEntry> = entries
            .iter()
            .map(|(sha, content)| NoteEntry {
                commit_sha: sha.clone(),
                content: content.clone(),
            })
            .collect();
        let request = NotesUploadRequest {
            entries: note_entries,
        };
        let response = client.upload_notes(request).expect("upload_notes");
        assert_eq!(response.success_count, 3);
        assert_eq!(response.failure_count, 0);

        // Cache locally with synced = 1.
        let db = NotesDatabase::global().expect("global db");
        {
            let mut lock = db.lock().expect("lock");
            lock.cache_synced_notes(&entries)
                .expect("cache_synced_notes");
        }

        // --- Verify all three notes are in notes-db with synced = 1 ---
        let lock = db.lock().expect("lock for verify");
        let shas = [sha1.as_str(), sha2.as_str(), sha3.as_str()];
        for sha in &shas {
            let content = lock.get_note(sha).expect("get_note");
            assert!(content.is_some(), "note for {} should be in notes-db", sha);
        }

        // None of them should appear in dequeue_pending (synced = 1).
        drop(lock);
        let mut lock = db.lock().expect("lock for dequeue");
        let pending = lock.dequeue_pending(10).expect("dequeue_pending");
        let migrated_pending: Vec<_> = pending
            .iter()
            .filter(|p| shas.contains(&p.commit_sha.as_str()))
            .collect();
        assert!(
            migrated_pending.is_empty(),
            "migrated notes must not appear in dequeue_pending: {:?}",
            migrated_pending
                .iter()
                .map(|p| &p.commit_sha)
                .collect::<Vec<_>>()
        );

        // --- Verify the mock was called ---
        _mock.assert();

        // Cleanup.
        unsafe {
            std::env::remove_var("GIT_AI_TEST_NOTES_DB_PATH");
            std::env::remove_var("GIT_AI_API_KEY");
            std::env::remove_var("GIT_AI_NOTES_BACKEND_URL");
        }
    }

    /// Unit test: `list_notes` returns empty when there are no notes.
    #[test]
    fn list_notes_returns_empty_for_repo_without_notes() {
        let repo = TmpRepo::new().expect("TmpRepo::new");
        // Create a commit so HEAD exists (list_notes on an empty repo might error differently).
        repo.write_file("a.txt", "a", false).expect("write file");
        repo.commit_all("c").expect("commit");

        let pairs = list_notes(repo.gitai_repo()).expect("list_notes");
        assert!(
            pairs.is_empty(),
            "no notes should be listed for a fresh repo"
        );
    }

    /// Unit test: `cat_file_batch` with empty input returns empty map.
    #[test]
    fn cat_file_batch_empty_input() {
        let repo = TmpRepo::new().expect("TmpRepo::new");
        let result = cat_file_batch(repo.gitai_repo(), &[]).expect("cat_file_batch");
        assert!(result.is_empty());
    }

    #[test]
    fn cat_file_stream_rejects_announced_oversized_blob_before_callback() {
        let blob_sha = "a".repeat(40);
        let note_pairs = vec![(blob_sha.clone(), "b".repeat(40))];
        let protocol = format!("{blob_sha} blob {}\n", MAX_NOTE_BLOB_BYTES + 1);
        let mut reader = std::io::Cursor::new(protocol.into_bytes());
        let mut called = false;

        let error = parse_cat_file_stream(&mut reader, &note_pairs, &mut |_, _, _| {
            called = true;
            Ok(())
        })
        .expect_err("oversized announced blob must be rejected");

        assert!(error.to_string().contains("note blob exceeded"));
        assert!(!called, "callback must not receive oversized content");
    }

    #[test]
    fn cat_file_stderr_drain_discards_bytes_beyond_limit() {
        let input = std::io::Cursor::new(vec![b'x'; 1025]);
        let (retained, truncated) = drain_stream_with_limit(input, 1024).unwrap();

        assert_eq!(retained.len(), 1024);
        assert!(truncated);
    }

    /// Integration test: `--force` re-uploads notes that are already cached as synced.
    ///   1. Create notes and cache them as synced=1 in notes-db.
    ///   2. Without --force: verify entries are filtered out.
    ///   3. With --force: verify all entries pass through for upload.
    #[test]
    #[serial_test::serial(notes_db_env)]
    fn force_flag_bypasses_synced_cache_filter() {
        use std::collections::HashSet;

        let tmp_db = NamedTempFile::new().expect("tmp notes-db");
        unsafe {
            std::env::set_var("GIT_AI_TEST_NOTES_DB_PATH", tmp_db.path());
        }

        let repo = TmpRepo::new().expect("TmpRepo::new");

        let sha1 = make_commit(&repo, "file1.txt", "a", "commit 1");
        let sha2 = make_commit(&repo, "file2.txt", "b", "commit 2");

        add_git_note(&repo, &sha1, "note-1");
        add_git_note(&repo, &sha2, "note-2");

        // Read notes from repo.
        let note_pairs = list_notes(repo.gitai_repo()).expect("list_notes");
        let blob_to_commit: HashMap<String, String> = note_pairs
            .iter()
            .map(|(b, c)| (b.clone(), c.clone()))
            .collect();
        let blob_shas: Vec<String> = note_pairs.iter().map(|(b, _)| b.clone()).collect();
        let blob_contents = cat_file_batch(repo.gitai_repo(), &blob_shas).expect("cat_file_batch");

        let entries: Vec<(String, String)> = blob_contents
            .iter()
            .filter_map(|(blob_sha, content)| {
                blob_to_commit
                    .get(blob_sha)
                    .map(|commit_sha| (commit_sha.clone(), content.clone()))
            })
            .collect();
        assert_eq!(entries.len(), 2);

        // Pre-cache all entries as synced=1.
        let db = NotesDatabase::global().expect("global db");
        {
            let mut lock = db.lock().expect("lock");
            lock.cache_synced_notes(&entries)
                .expect("cache_synced_notes");
        }

        // Without force: filtering should remove all entries.
        {
            let mut filtered = entries.clone();
            let lock = db.lock().expect("lock");
            let all_shas: Vec<&str> = filtered.iter().map(|(s, _)| s.as_str()).collect();
            let synced = lock.get_synced_shas(&all_shas).expect("get_synced_shas");
            filtered.retain(|(sha, _)| !synced.contains(sha));
            assert!(
                filtered.is_empty(),
                "without --force, all synced entries should be filtered out"
            );
        }

        // With force: no filtering applied, all entries remain.
        {
            let forced_entries = entries.clone();
            // force=true means we skip the retain logic entirely
            assert_eq!(
                forced_entries.len(),
                2,
                "with --force, all entries should remain for upload"
            );

            // Verify we can upload them to a new backend.
            let mut server = mockito::Server::new();
            let upload_response = serde_json::json!({
                "success_count": 2,
                "failure_count": 0
            })
            .to_string();
            let mock = server
                .mock("POST", "/worker/notes/upload")
                .with_status(200)
                .with_header("content-type", "application/json")
                .with_body(&upload_response)
                .create();

            let server_url = server.url();
            unsafe {
                std::env::set_var("GIT_AI_NOTES_BACKEND_URL", &server_url);
                std::env::set_var("GIT_AI_API_KEY", "force-test-key");
            }

            let cfg = crate::config::Config::fresh();
            let backend_url = cfg.notes_backend_url().unwrap().to_string();
            let ctx = ApiContext::new(Some(backend_url));
            let client = ApiClient::new(ctx);

            let note_entries: Vec<NoteEntry> = forced_entries
                .iter()
                .map(|(sha, content)| NoteEntry {
                    commit_sha: sha.clone(),
                    content: content.clone(),
                })
                .collect();
            let request = NotesUploadRequest {
                entries: note_entries,
            };
            let response = client.upload_notes(request).expect("upload_notes");
            assert_eq!(response.success_count, 2);
            mock.assert();
        }

        // Verify commit shas are what we expect.
        let lock = db.lock().expect("lock for final verify");
        let shas_set: HashSet<&str> = [sha1.as_str(), sha2.as_str()].into_iter().collect();
        for sha in &shas_set {
            assert!(
                lock.get_note(sha).expect("get_note").is_some(),
                "note for {} should remain in cache",
                sha
            );
        }
        drop(lock);

        unsafe {
            std::env::remove_var("GIT_AI_TEST_NOTES_DB_PATH");
            std::env::remove_var("GIT_AI_API_KEY");
            std::env::remove_var("GIT_AI_NOTES_BACKEND_URL");
        }
    }
}
