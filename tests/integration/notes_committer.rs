use crate::repos::test_repo::TestRepo;
use git_ai::config::AuthorConfig;
use std::fs;

/// Commit an AI edit and return the committer (`Name <email>`) of the resulting
/// `refs/notes/ai` tip.
fn commit_ai_edit_and_read_notes_committer(repo: &TestRepo) -> String {
    let file_path = repo.path().join("notes_committer.rs");
    repo.git_ai(&["checkpoint", "human", "notes_committer.rs"])
        .unwrap();
    fs::write(&file_path, "fn ai() {}\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "notes_committer.rs"])
        .unwrap();
    let commit = repo.stage_all_and_commit("AI commit").unwrap();
    assert!(
        repo.read_authorship_note(&commit.commit_sha).is_some(),
        "AI commit should have an authorship note"
    );

    repo.git(&["log", "-1", "--format=%cn <%ce>", "refs/notes/ai"])
        .unwrap()
        .trim()
        .to_string()
}

#[test]
fn test_notes_committer_defaults_to_git_ai_placeholder() {
    let repo = TestRepo::new();

    assert_eq!(
        commit_ai_edit_and_read_notes_committer(&repo),
        "git-ai <git-ai@local>"
    );
}

#[test]
fn test_notes_committer_uses_configured_identity() {
    // The daemon writes the notes and reads config.json from its own home; a
    // shared pool daemon's config is rewritten by concurrent tests.
    let mut repo = TestRepo::new_dedicated_daemon();
    repo.patch_git_ai_config(|patch| {
        patch.notes_committer = Some(AuthorConfig {
            name: Some("Notes Bot".to_string()),
            email: Some("notes-bot@example.com".to_string()),
        });
    });

    assert_eq!(
        commit_ai_edit_and_read_notes_committer(&repo),
        "Notes Bot <notes-bot@example.com>"
    );
}

#[test]
fn test_notes_committer_unset_field_keeps_placeholder() {
    let mut repo = TestRepo::new_dedicated_daemon();
    repo.patch_git_ai_config(|patch| {
        patch.notes_committer = Some(AuthorConfig {
            name: None,
            email: Some("notes-bot@example.com".to_string()),
        });
    });

    assert_eq!(
        commit_ai_edit_and_read_notes_committer(&repo),
        "git-ai <notes-bot@example.com>"
    );
}
