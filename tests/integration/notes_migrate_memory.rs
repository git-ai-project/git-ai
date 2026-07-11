use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use git_ai::config::{NotesBackendConfig, NotesBackendKind};
use std::fs;
use std::io::Write;

#[test]
fn streamed_note_migration_uploads_caches_and_attribution_recovers() {
    let mut server = mockito::Server::new();
    let upload = server
        .mock("POST", "/worker/notes/upload")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"success_count":1,"failure_count":0}"#)
        .create();

    let mut repo = TestRepo::new();
    let mut base = repo.filename("base.txt");
    base.set_contents(lines!["first", "second", "third"]);
    repo.stage_all_and_commit("initial").unwrap();
    base.assert_committed_lines(lines!["first".human(), "second".human(), "third".human(),]);
    drop(base);

    repo.patch_git_ai_config(|patch| {
        patch.notes_backend = Some(NotesBackendConfig {
            kind: NotesBackendKind::Http,
            backend_url: Some(server.url()),
        });
    });
    let output = repo
        .git_ai_with_env(
            &["notes", "migrate"],
            &[("GIT_AI_API_KEY", "notes-migrate-stream-test")],
        )
        .unwrap();
    assert!(
        output.contains("Migration complete: 1 note(s) uploaded successfully"),
        "unexpected migration output: {output}"
    );
    upload.assert();

    let second_output = repo
        .git_ai_with_env(
            &["notes", "migrate"],
            &[("GIT_AI_API_KEY", "notes-migrate-stream-test")],
        )
        .unwrap();
    assert!(
        second_output.contains("All notes already migrated"),
        "unexpected second migration output: {second_output}"
    );

    let mut recovered = repo.filename("recovered.txt");
    recovered.set_contents(lines!["AI recovery".ai()]);
    repo.stage_all_and_commit("AI recovery").unwrap();
    let mut base = repo.filename("base.txt");
    base.assert_committed_lines(lines!["first".human(), "second".human(), "third".human(),]);
    recovered.assert_committed_lines(lines!["AI recovery".ai()]);
}

#[test]
fn oversized_note_is_rejected_before_materialization_and_attribution_recovers() {
    let mut repo = TestRepo::new();
    let mut base = repo.filename("base.txt");
    base.set_contents(lines!["first", "second", "third"]);
    repo.stage_all_and_commit("initial").unwrap();
    base.assert_committed_lines(lines!["first".human(), "second".human(), "third".human(),]);
    drop(base);

    let commit = repo.git(&["rev-parse", "HEAD"]).unwrap();
    let commit = commit.trim();
    let note_path = repo.test_home_path().join("oversized-note.txt");
    let mut note_file = fs::File::create(&note_path).unwrap();
    let chunk = [b'x'; 8192];
    for _ in 0..2048 {
        note_file.write_all(&chunk).unwrap();
    }
    note_file.write_all(b"x").unwrap();
    drop(note_file);

    let note_path = note_path.to_string_lossy().to_string();
    repo.git(&["notes", "--ref=ai", "add", "-f", "-F", &note_path, commit])
        .unwrap();

    repo.patch_git_ai_config(|patch| {
        patch.notes_backend = Some(NotesBackendConfig {
            kind: NotesBackendKind::Http,
            backend_url: Some("http://127.0.0.1:9".to_string()),
        });
    });
    let error = repo
        .git_ai_with_env(
            &["notes", "migrate"],
            &[("GIT_AI_API_KEY", "notes-migrate-memory-test")],
        )
        .expect_err("oversized note migration must fail");
    assert!(
        error.contains("note blob exceeded the 16777216 byte limit"),
        "unexpected migration error: {error}"
    );

    repo.git(&["notes", "--ref=ai", "remove", commit]).unwrap();
    let mut recovered = repo.filename("recovered.txt");
    recovered.set_contents(lines!["AI recovery".ai()]);
    repo.stage_all_and_commit("AI recovery").unwrap();
    let mut base = repo.filename("base.txt");
    base.assert_committed_lines(lines!["first".human(), "second".human(), "third".human(),]);
    recovered.assert_committed_lines(lines!["AI recovery".ai()]);
}
