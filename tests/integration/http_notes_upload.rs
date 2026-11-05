// Under `notes_backend.kind = "http"`, post-commit writes each note to the
// local notes-db and relies on the telemetry flush loop's periodic tick to
// upload it: the `notes.flush` control signal is a no-op inside the daemon
// process, so the tick is the only path for daemon-side writes, and must
// run notes regardless of whether the telemetry buffer has anything else.

use crate::repos::test_repo::TestRepo;
use git_ai::authorship::authorship_log_serialization::AuthorshipLog;
use git_ai::notes::db::NotesDatabase;
use git_ai::notes::reference_server::ReferenceServer;
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

const UPLOAD_DEADLINE: Duration = Duration::from_secs(10);

/// Poll the notes-db for a commit's note landing from post-commit.
fn wait_for_note_in_db(notes_db_path: &Path, sha: &str) -> Option<String> {
    let deadline = Instant::now() + UPLOAD_DEADLINE;
    while Instant::now() < deadline {
        if let Ok(db) = NotesDatabase::open_at_path(notes_db_path)
            && let Ok(Some(content)) = db.get_note(sha)
        {
            return Some(content);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    None
}

/// Poll the reference server for a commit's note arriving via the daemon's
/// periodic flush.
fn wait_for_note_on_server(server: &ReferenceServer, sha: &str) -> Option<String> {
    let deadline = Instant::now() + UPLOAD_DEADLINE;
    while Instant::now() < deadline {
        if let Some(content) = server.store().get(sha) {
            return Some(content);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    None
}

#[test]
fn test_periodic_flush_uploads_notes_with_daemon_log_upload_disabled() {
    let server = ReferenceServer::start("127.0.0.1:0").expect("start notes reference server");
    let backend_url = server.base_url();

    let repo = TestRepo::new_with_daemon_env(&[
        ("GIT_AI_NOTES_BACKEND_KIND", "http"),
        ("GIT_AI_NOTES_BACKEND_URL", backend_url.as_str()),
        ("GIT_AI_API_KEY", "http-notes-upload-test-key"),
        ("GIT_AI_DAEMON_LOG_UPLOAD", "false"),
    ]);
    let notes_db_path = repo
        .test_home_path()
        .join(".git-ai")
        .join("internal")
        .join("notes-db");

    fs::write(repo.path().join("base.txt"), "base\n").unwrap();
    repo.git(&["add", "base.txt"]).unwrap();
    repo.git(&["commit", "-m", "initial commit"]).unwrap();
    repo.sync_daemon();

    fs::write(repo.path().join("ai.txt"), "ai line\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "ai.txt"]).unwrap();
    repo.git(&["add", "ai.txt"]).unwrap();
    repo.git(&["commit", "-m", "AI commit"]).unwrap();
    repo.sync_daemon();

    let head_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    let local_note = wait_for_note_in_db(&notes_db_path, &head_sha)
        .expect("post-commit should write the AI commit's note to the local notes-db");

    let uploaded_note = wait_for_note_on_server(&server, &head_sha).expect(
        "daemon should upload the pending note on its periodic flush without `git-ai await`, \
         even though daemon log upload is disabled",
    );
    assert_eq!(
        uploaded_note, local_note,
        "uploaded note should match the note written locally"
    );

    let log = AuthorshipLog::deserialize_from_string(&uploaded_note).expect("parse uploaded note");
    assert!(
        !log.attestations.is_empty(),
        "uploaded note should carry the AI attestation"
    );
}
