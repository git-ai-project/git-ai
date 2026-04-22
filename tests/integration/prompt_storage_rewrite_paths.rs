//! Regression tests for a bug where prompt_storage = "default" | "local" is
//! honored by the initial post-commit path but ignored by the rewrite paths
//! (amend, rebase, cherry-pick, merge).
//!
//! Expected behavior (per post_commit.rs):
//! - PromptStorageMode::Default | Local → messages must be stripped before notes_add
//! - PromptStorageMode::Notes         → messages kept, but secrets must be redacted
//!
//! Under the current rebase_authorship.rs, none of these policies are applied
//! on the rewrite paths. These tests pin down that behavior for each entry
//! point so we can validate a shared-policy fix.
use crate::repos::test_repo::TestRepo;
use git_ai::authorship::authorship_log_serialization::AuthorshipLog;
use git_ai::authorship::transcript::{AiTranscript, Message};
use git_ai::git::refs::notes_add;
use std::fs;

/// Set up a test repo configured for `prompt_storage = "local"` (which should
/// always strip messages from notes). `local` is the cleanest mode to test:
/// it deterministically strips without depending on login state / CAS enqueue.
///
/// Uses a dedicated daemon because parallel tests in this file flip between
/// `local` and `notes` modes — sharing a daemon/config across them causes
/// config-patch races where one test's mode leaks into another's commit.
fn repo_with_local_storage() -> TestRepo {
    let mut repo = TestRepo::new_dedicated_daemon();
    repo.patch_git_ai_config(|patch| {
        patch.exclude_prompts_in_repositories = Some(vec![]);
        patch.prompt_storage = Some("local".to_string());
    });
    repo
}

fn repo_with_notes_storage() -> TestRepo {
    let mut repo = TestRepo::new_dedicated_daemon();
    repo.patch_git_ai_config(|patch| {
        patch.exclude_prompts_in_repositories = Some(vec![]);
        patch.prompt_storage = Some("notes".to_string());
    });
    repo
}

/// Agent-v1 checkpoint with a non-empty transcript. Mirrors the helper in
/// tests/integration/ignore_prompts.rs.
fn checkpoint_with_message(repo: &TestRepo, user_text: &str, edited: Vec<String>) {
    let mut transcript = AiTranscript::new();
    transcript.add_message(Message::user(user_text.to_string(), None));
    transcript.add_message(Message::assistant(
        "I'll help you with that.".to_string(),
        None,
    ));

    let hook_input = serde_json::json!({
        "type": "ai_agent",
        "repo_working_dir": repo.path().to_str().unwrap(),
        "edited_filepaths": edited,
        "transcript": transcript,
        "agent_name": "test-agent",
        "model": "test-model",
        "conversation_id": "test-conversation-id",
    });

    repo.git_ai(&[
        "checkpoint",
        "agent-v1",
        "--hook-input",
        &serde_json::to_string(&hook_input).unwrap(),
    ])
    .expect("checkpoint should succeed");
}

fn read_log(repo: &TestRepo, sha: &str) -> AuthorshipLog {
    let note = repo
        .read_authorship_note(sha)
        .unwrap_or_else(|| panic!("expected authorship note on {}", sha));
    AuthorshipLog::deserialize_from_string(&note).expect("parse authorship note")
}

fn assert_messages_empty(log: &AuthorshipLog, ctx: &str) {
    assert!(
        !log.metadata.prompts.is_empty(),
        "{ctx}: expected a prompt record to exist"
    );
    for (id, prompt) in &log.metadata.prompts {
        assert!(
            prompt.messages.is_empty(),
            "{ctx}: prompt {id} leaked {} messages (prompt_storage=\"local\" should strip)",
            prompt.messages.len()
        );
    }
}

/// Control: prompt_storage = "local" strips messages on the initial commit.
/// If this fails, the other tests are not meaningful.
#[test]
fn test_initial_commit_strips_messages_under_local_storage() {
    let repo = repo_with_local_storage();

    let readme = repo.path().join("README.md");
    fs::write(&readme, "# Test\n").unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "initial"]).unwrap();

    let ai_file = repo.path().join("ai.txt");
    fs::write(&ai_file, "AI line 1\nAI line 2\n").unwrap();
    checkpoint_with_message(&repo, "write ai file", vec!["ai.txt".to_string()]);

    repo.git(&["add", "-A"]).unwrap();
    let commit = repo.commit("add ai").expect("commit");

    assert_messages_empty(&commit.authorship_log, "initial commit");
}

/// Amend: rewrite_authorship_after_amend reads the working-log transcript to
/// build a fresh AuthorshipLog and writes it to notes without consulting
/// prompt_storage. Messages leak into the amended note.
#[test]
fn test_amend_strips_messages_under_local_storage() {
    let repo = repo_with_local_storage();

    // Seed: initial non-AI commit so the amend target is not the root.
    let readme = repo.path().join("README.md");
    fs::write(&readme, "# Test\n").unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "initial"]).unwrap();

    // AI commit with a transcript.
    let ai_file = repo.path().join("ai.txt");
    fs::write(&ai_file, "AI line 1\nAI line 2\n").unwrap();
    checkpoint_with_message(&repo, "original prompt", vec!["ai.txt".to_string()]);
    repo.git(&["add", "-A"]).unwrap();
    let original = repo.commit("add ai").expect("commit");
    assert_messages_empty(&original.authorship_log, "pre-amend commit (sanity)");

    // Amend by adding another AI line with another transcript checkpoint.
    fs::write(&ai_file, "AI line 1\nAI line 2\nAI line 3\n").unwrap();
    checkpoint_with_message(&repo, "amended prompt", vec!["ai.txt".to_string()]);
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "--amend", "-m", "add ai (amended)"])
        .unwrap();

    let amended_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let amended_log = read_log(&repo, &amended_sha);
    assert_messages_empty(&amended_log, "amended commit");
}

/// Rebase: slow-path rebase builds a new note per rebased commit. When the
/// attribution state comes from the blame-based fallback (not from existing
/// notes), prompts are populated directly from working-log transcripts and
/// messages leak through.
#[test]
fn test_rebase_strips_messages_under_local_storage() {
    let repo = repo_with_local_storage();

    let base = repo.path().join("base.txt");
    fs::write(&base, "base\n").unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "initial"]).unwrap();
    let default_branch = repo.current_branch();

    // Feature branch from the initial commit.
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    let ai_file = repo.path().join("ai.txt");
    fs::write(&ai_file, "AI line 1\nAI line 2\n").unwrap();
    checkpoint_with_message(&repo, "feature prompt", vec!["ai.txt".to_string()]);
    repo.git(&["add", "-A"]).unwrap();
    let feature_commit = repo.commit("feature ai commit").expect("commit");
    assert_messages_empty(
        &feature_commit.authorship_log,
        "feature commit (before rebase)",
    );

    // Advance default branch with a non-conflicting commit.
    repo.git(&["checkout", &default_branch]).unwrap();
    let other = repo.path().join("other.txt");
    fs::write(&other, "other\n").unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "main advances"]).unwrap();

    // Rebase feature onto the advanced default branch.
    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["rebase", &default_branch]).unwrap();
    let rebased_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let rebased_log = read_log(&repo, &rebased_sha);
    assert_messages_empty(&rebased_log, "rebased commit");
}

/// Cherry-pick: rewrite_authorship_after_cherry_pick assembles an authorship
/// log per cherry-picked commit from VirtualAttributions — which loads
/// prompts (with messages) and writes straight to notes.
#[test]
fn test_cherry_pick_strips_messages_under_local_storage() {
    let repo = repo_with_local_storage();

    let base = repo.path().join("base.txt");
    fs::write(&base, "base\n").unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "initial"]).unwrap();
    let default_branch = repo.current_branch();

    // Build feature branch with an AI commit carrying a transcript.
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    let ai_file = repo.path().join("ai.txt");
    fs::write(&ai_file, "AI line 1\nAI line 2\n").unwrap();
    checkpoint_with_message(&repo, "cherry prompt", vec!["ai.txt".to_string()]);
    repo.git(&["add", "-A"]).unwrap();
    let feature_commit = repo.commit("feature ai commit").expect("commit");
    assert_messages_empty(&feature_commit.authorship_log, "feature commit (sanity)");
    let feature_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Back to default branch and cherry-pick the feature commit onto it.
    repo.git(&["checkout", &default_branch]).unwrap();
    repo.git(&["cherry-pick", &feature_sha]).unwrap();

    let picked_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let picked_log = read_log(&repo, &picked_sha);
    assert_messages_empty(&picked_log, "cherry-picked commit");
}

/// Squash-merge: rewrite_authorship_after_squash_merge merges VirtualAttributions
/// from both branches and writes a fresh note. Prompts carry messages through.
#[test]
fn test_squash_merge_strips_messages_under_local_storage() {
    let repo = repo_with_local_storage();

    let base = repo.path().join("main.txt");
    fs::write(&base, "line 1\nline 2\nline 3\n").unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "initial"]).unwrap();
    let default_branch = repo.current_branch();

    repo.git(&["checkout", "-b", "feature"]).unwrap();
    fs::write(&base, "line 1\nline 2\nline 3\n// AI feature line\n").unwrap();
    checkpoint_with_message(&repo, "feature prompt", vec!["main.txt".to_string()]);
    repo.git(&["add", "-A"]).unwrap();
    let feature_commit = repo.commit("feature ai commit").expect("commit");
    assert_messages_empty(&feature_commit.authorship_log, "feature commit (sanity)");

    repo.git(&["checkout", &default_branch]).unwrap();
    repo.git(&["merge", "--squash", "feature"]).unwrap();
    let squash_commit = repo.commit("squashed feature").expect("commit");

    let squashed_sha = squash_commit.commit_sha.clone();
    let squashed_log = read_log(&repo, &squashed_sha);
    assert_messages_empty(&squashed_log, "squash-merge commit");
}

// The tests below mimic the production leak pattern: a pre-existing source
// note with messages populated (as written by older git-ai versions, before
// the append-time transcript strip landed) gets rebased / cherry-picked /
// squash-merged forward. Under the current policy the new note must strip
// those messages — otherwise the leak propagates with every rewrite.

fn inject_leaky_source_note(repo: &TestRepo, commit_sha: &str, session_text: &str) {
    let note = repo
        .read_authorship_note(commit_sha)
        .expect("source commit should have a note to mutate");
    let mut log = AuthorshipLog::deserialize_from_string(&note).expect("parse source note");
    assert!(
        !log.metadata.prompts.is_empty(),
        "setup: expected at least one prompt on source commit {}",
        commit_sha
    );
    for prompt in log.metadata.prompts.values_mut() {
        prompt.messages = vec![
            Message::user(session_text.to_string(), None),
            Message::assistant("I'll help you with that.".to_string(), None),
        ];
    }
    let mutated = log.serialize_to_string().expect("serialize mutated note");
    let git_ai_repo = git_ai::git::find_repository_in_path(repo.path().to_str().unwrap())
        .expect("find repository");
    notes_add(&git_ai_repo, commit_sha, &mutated).expect("overwrite source note with messages");

    // Sanity: the mutated note now carries messages.
    let re_read = repo
        .read_authorship_note(commit_sha)
        .expect("note exists after mutation");
    let verify = AuthorshipLog::deserialize_from_string(&re_read).expect("re-parse mutated note");
    assert!(
        verify
            .metadata
            .prompts
            .values()
            .any(|p| !p.messages.is_empty()),
        "mutated note should have non-empty messages",
    );
}

/// Leaky upstream: a commit gets rewritten (rebased). Under Local storage the
/// rewritten note must strip messages even though the source note leaked.
#[test]
fn test_rebase_strips_leaky_upstream_messages() {
    let repo = repo_with_local_storage();

    let base = repo.path().join("base.txt");
    fs::write(&base, "base\n").unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "initial"]).unwrap();
    let default_branch = repo.current_branch();

    repo.git(&["checkout", "-b", "feature"]).unwrap();
    let ai_file = repo.path().join("ai.txt");
    fs::write(&ai_file, "AI line 1\nAI line 2\n").unwrap();
    checkpoint_with_message(&repo, "feature prompt", vec!["ai.txt".to_string()]);
    repo.git(&["add", "-A"]).unwrap();
    let feature_commit = repo.commit("feature ai commit").expect("commit");
    assert_messages_empty(&feature_commit.authorship_log, "feature commit (baseline)");

    // Simulate an older git-ai version that left messages on the source note.
    inject_leaky_source_note(&repo, &feature_commit.commit_sha, "leaky upstream prompt");

    // Advance default branch and rebase.
    repo.git(&["checkout", &default_branch]).unwrap();
    fs::write(repo.path().join("other.txt"), "other\n").unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "main advances"]).unwrap();
    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["rebase", &default_branch]).unwrap();

    let rebased_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let rebased_log = read_log(&repo, &rebased_sha);
    assert_messages_empty(&rebased_log, "rebased commit (leaky upstream)");
}

/// Leaky upstream + cherry-pick: the picked commit's note must not inherit
/// messages from the source note under Local storage.
#[test]
fn test_cherry_pick_strips_leaky_upstream_messages() {
    let repo = repo_with_local_storage();

    fs::write(repo.path().join("base.txt"), "base\n").unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "initial"]).unwrap();
    let default_branch = repo.current_branch();

    repo.git(&["checkout", "-b", "feature"]).unwrap();
    let ai_file = repo.path().join("ai.txt");
    fs::write(&ai_file, "AI line 1\nAI line 2\n").unwrap();
    checkpoint_with_message(&repo, "cherry prompt", vec!["ai.txt".to_string()]);
    repo.git(&["add", "-A"]).unwrap();
    let feature_commit = repo.commit("feature ai commit").expect("commit");
    assert_messages_empty(&feature_commit.authorship_log, "feature commit (baseline)");

    inject_leaky_source_note(&repo, &feature_commit.commit_sha, "leaky upstream prompt");

    let feature_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    repo.git(&["checkout", &default_branch]).unwrap();
    repo.git(&["cherry-pick", &feature_sha]).unwrap();

    let picked_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let picked_log = read_log(&repo, &picked_sha);
    assert_messages_empty(&picked_log, "cherry-picked commit (leaky upstream)");
}

/// Leaky upstream + squash-merge: this is the exact production pattern seen
/// in the DB — branch notes clean, squash-merge commit mixed/leaked. With
/// the fix the squash commit strips inherited messages.
#[test]
fn test_squash_merge_strips_leaky_upstream_messages() {
    let repo = repo_with_local_storage();

    let base = repo.path().join("main.txt");
    fs::write(&base, "line 1\nline 2\nline 3\n").unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "initial"]).unwrap();
    let default_branch = repo.current_branch();

    repo.git(&["checkout", "-b", "feature"]).unwrap();
    fs::write(&base, "line 1\nline 2\nline 3\n// AI feature line\n").unwrap();
    checkpoint_with_message(&repo, "feature prompt", vec!["main.txt".to_string()]);
    repo.git(&["add", "-A"]).unwrap();
    let feature_commit = repo.commit("feature ai commit").expect("commit");
    assert_messages_empty(&feature_commit.authorship_log, "feature commit (baseline)");

    inject_leaky_source_note(&repo, &feature_commit.commit_sha, "leaky upstream prompt");

    repo.git(&["checkout", &default_branch]).unwrap();
    repo.git(&["merge", "--squash", "feature"]).unwrap();
    let squash_commit = repo.commit("squashed feature").expect("commit");

    let squashed_log = read_log(&repo, &squash_commit.commit_sha);
    assert_messages_empty(&squashed_log, "squash-merge commit (leaky upstream)");
}

/// Notes mode: storage policy says messages SHOULD be kept in notes. Verify
/// rewrite paths honor that too (i.e. the shared helper does not over-strip).
#[test]
fn test_notes_mode_keeps_messages_through_rebase() {
    let repo = repo_with_notes_storage();

    fs::write(repo.path().join("base.txt"), "base\n").unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "initial"]).unwrap();
    let default_branch = repo.current_branch();

    repo.git(&["checkout", "-b", "feature"]).unwrap();
    let ai_file = repo.path().join("ai.txt");
    fs::write(&ai_file, "AI line 1\nAI line 2\n").unwrap();
    checkpoint_with_message(&repo, "notes-mode prompt", vec!["ai.txt".to_string()]);
    repo.git(&["add", "-A"]).unwrap();
    let feature_commit = repo.commit("feature ai commit").expect("commit");

    // Under Notes mode the initial note should retain messages.
    let initial_had_messages = feature_commit
        .authorship_log
        .metadata
        .prompts
        .values()
        .any(|p| !p.messages.is_empty());
    assert!(
        initial_had_messages,
        "precondition: Notes mode should keep messages on initial commit"
    );

    repo.git(&["checkout", &default_branch]).unwrap();
    fs::write(repo.path().join("other.txt"), "other\n").unwrap();
    repo.git(&["add", "-A"]).unwrap();
    repo.git(&["commit", "-m", "main advances"]).unwrap();
    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["rebase", &default_branch]).unwrap();

    let rebased_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let rebased_log = read_log(&repo, &rebased_sha);
    assert!(
        rebased_log
            .metadata
            .prompts
            .values()
            .any(|p| !p.messages.is_empty()),
        "rebased commit under Notes mode should still carry messages: {:?}",
        rebased_log
            .metadata
            .prompts
            .values()
            .map(|p| p.messages.len())
            .collect::<Vec<_>>()
    );
}

crate::reuse_tests_in_worktree!(
    test_initial_commit_strips_messages_under_local_storage,
    test_amend_strips_messages_under_local_storage,
    test_rebase_strips_messages_under_local_storage,
    test_cherry_pick_strips_messages_under_local_storage,
    test_squash_merge_strips_messages_under_local_storage,
    test_rebase_strips_leaky_upstream_messages,
    test_cherry_pick_strips_leaky_upstream_messages,
    test_squash_merge_strips_leaky_upstream_messages,
    test_notes_mode_keeps_messages_through_rebase,
);
