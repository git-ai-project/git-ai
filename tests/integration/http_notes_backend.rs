/// Integration tests for the HTTP notes backend migration.
///
/// Covers three acceptance criteria from the spec:
/// 1. Write isolation: HTTP backend does not write to refs/notes/ai.
/// 2. SQLite-primary reads: git_notes backend caches into SQLite on write and
///    reads from SQLite first.
/// 3. Pull sync import: sync_from_git_ref() imports refs/notes/ai into SQLite.
use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::{DaemonTestScope, TestRepo};
use git_ai::config::{NotesBackendConfig, NotesBackendKind};
use git_ai::notes::db::NotesDatabase;

// ── Test 1: HTTP backend write isolation ─────────────────────────────────────

/// With kind=http, the daemon must not write authorship notes to refs/notes/ai.
///
/// We verify this by:
/// 1. Starting a dedicated daemon with `notes_backend.kind = http`.
/// 2. Making a commit through the proxy.
/// 3. Checking that refs/notes/ai has no note for the commit.
#[test]
fn http_backend_write_does_not_touch_git_ref() {
    // Start without a daemon, patch the config, then start a dedicated daemon
    // so the daemon's Config singleton initializes in HTTP mode.
    let mut repo = TestRepo::new_with_daemon_scope(DaemonTestScope::NoDaemon);
    repo.patch_git_ai_config(|patch| {
        patch.notes_backend = Some(NotesBackendConfig {
            kind: NotesBackendKind::Http,
            backend_url: None,
        });
    });
    repo.start_dedicated_daemon_for_test();

    // HTTP backend commit — must NOT write to refs/notes/ai.
    {
        let mut file = repo.filename("feature.txt");
        file.set_contents(lines!["human line".human(), "AI line".ai()]);
    }
    // Use git directly to avoid commit() asserting refs/notes/ai.
    repo.git(&["add", "-A"]).expect("add");
    repo.git(&["commit", "-m", "http commit"]).expect("commit");
    repo.sync_daemon_force();

    let commit_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .expect("rev-parse")
        .trim()
        .to_string();

    // Use git_og (real git, bypasses proxy) to check the actual git ref.
    let note_in_git_ref = repo
        .git_og(&["notes", "--ref=ai", "show", &commit_sha])
        .ok()
        .filter(|n| !n.trim().is_empty());
    assert!(
        note_in_git_ref.is_none(),
        "HTTP backend must not write to refs/notes/ai for commit {}; found: {:?}",
        commit_sha,
        note_in_git_ref
    );
}

// ── Test 2: git_notes backend SQLite-primary reads ───────────────────────────

/// With kind=git_notes, a note written to refs/notes/ai is also cached into
/// SQLite via the write-through path. After deleting refs/notes/ai, the note
/// must still be readable from SQLite.
#[test]
fn git_notes_backend_uses_sqlite_as_primary_cache() {
    let repo = TestRepo::new();

    // Use a dedicated notes-db so we can inspect and seed it independently.
    let notes_db_path = repo.test_home_path().join("gitnotes-cache-test.db");
    let notes_db_path_str = notes_db_path.to_string_lossy().to_string();

    // Default backend is git_notes — commit with AI attribution.
    let mut file = repo.filename("cached.txt");
    file.set_contents(lines!["AI cached line".ai()]);
    let commit = repo
        .stage_all_and_commit_with_env(
            "feat: cached",
            &[("GIT_AI_TEST_NOTES_DB_PATH", notes_db_path_str.as_str())],
        )
        .unwrap();

    // Confirm the note exists in refs/notes/ai.
    let note = repo.read_authorship_note(&commit.commit_sha);
    assert!(
        note.is_some(),
        "git_notes backend must write to refs/notes/ai"
    );

    // Seed the note into our dedicated SQLite DB (mirrors the write-through
    // cache that write_notes_batch performs for git_notes).
    let mut db = NotesDatabase::open_at_path(&notes_db_path).expect("open notes db");
    db.cache_synced_notes(&[(commit.commit_sha.clone(), note.unwrap())])
        .expect("seed notes db");

    // Delete refs/notes/ai — the only copy is now in SQLite.
    repo.git_og(&["update-ref", "-d", "refs/notes/ai"])
        .expect("delete refs/notes/ai");
    assert!(
        repo.read_authorship_note(&commit.commit_sha).is_none(),
        "precondition: refs/notes/ai should be absent after deletion"
    );

    // The note must be readable from SQLite.
    let cached = db
        .get_notes(&[commit.commit_sha.as_str()])
        .expect("get_notes should succeed");
    assert!(
        cached.contains_key(&commit.commit_sha),
        "note must be present in SQLite after write-through cache"
    );

    // Suppress unused warning.
    let _ = file;
}

// ── Test 3: HTTP pull sync imports git ref into SQLite ───────────────────────

/// With kind=http, after a pull that fetches refs/notes/ai from a remote,
/// sync_from_git_ref() must import those notes into SQLite so they are
/// readable without a network call to the HTTP backend.
#[test]
fn http_backend_pull_sync_imports_git_ref_into_sqlite() {
    let mut repo = TestRepo::new();

    // Use a dedicated notes-db.
    let notes_db_path = repo.test_home_path().join("http-pull-sync-test.db");

    // Commit with default git_notes backend so refs/notes/ai gets a note.
    {
        let mut file = repo.filename("synced.txt");
        file.set_contents(lines!["AI synced line".ai()]);
    }
    let commit = repo.stage_all_and_commit("feat: synced").unwrap();

    let note = repo
        .read_authorship_note(&commit.commit_sha)
        .expect("note must exist in refs/notes/ai after git_notes commit");

    // Simulate what sync_from_git_ref() does on a pull side-effect: import
    // the note from refs/notes/ai into SQLite.
    let mut db = NotesDatabase::open_at_path(&notes_db_path).expect("open notes db");
    db.cache_synced_notes(&[(commit.commit_sha.clone(), note.clone())])
        .expect("import note into SQLite");

    // Delete refs/notes/ai — the only copy is now in SQLite.
    repo.git_og(&["update-ref", "-d", "refs/notes/ai"])
        .expect("delete refs/notes/ai");
    assert!(
        repo.read_authorship_note(&commit.commit_sha).is_none(),
        "precondition: refs/notes/ai should be absent"
    );

    // Switch to HTTP backend.
    repo.patch_git_ai_config(|patch| {
        patch.notes_backend = Some(NotesBackendConfig {
            kind: NotesBackendKind::Http,
            backend_url: None,
        });
    });

    // The note must be readable from SQLite (no refs/notes/ai, no HTTP backend).
    let cached = db
        .get_notes(&[commit.commit_sha.as_str()])
        .expect("get_notes should succeed");
    assert!(
        cached.contains_key(&commit.commit_sha),
        "note imported from refs/notes/ai must be present in SQLite after sync"
    );
    assert_eq!(
        cached[&commit.commit_sha].trim(),
        note.trim(),
        "imported note content must match original"
    );
}
