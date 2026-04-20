// Critical regression tests for old-format/new-format coexistence during cutover scenarios
//
// These tests verify that git-ai correctly handles:
// 1. Old-format authorship notes (bare 16-char hex hashes, prompts-only metadata)
// 2. New-format authorship notes (s_::t_ hashes, sessions metadata)
// 3. Mixed scenarios where both formats coexist in the same note or across operations
//
// Format detection: checkpoint.trace_id.is_some() determines which format is used.

use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use git_ai::authorship::authorship_log_serialization::AuthorshipLog;
use git_ai::git::refs::notes_add;
use serde_json::Value;
use std::fs;

// Test 1: Old format note can be read and deserializes correctly
#[test]
fn test_old_format_note_can_be_attached_and_read() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut file = repo.filename("test.txt");
    file.set_contents(crate::lines!["Human line", "AI line".ai()]);
    let commit = repo.stage_all_and_commit("Initial commit").unwrap();

    // Replace with an old-format note (using "cursor" as tool name)
    let old_hash = "5a1b2c3d4e5f6789"; // 16-char bare hex
    let base_sha = &commit.commit_sha;
    let old_note = format!(
        r#"test.txt
  {} 2-2
---
{{
  "schema_version": "authorship/3.0.0",
  "git_ai_version": "1.3.3",
  "base_commit_sha": "{}",
  "prompts": {{
    "{}": {{
      "agent_id": {{"tool": "cursor", "id": "old_session", "model": "gpt-4"}},
      "human_author": null,
      "messages": [],
      "total_additions": 1,
      "total_deletions": 0,
      "accepted_lines": 1,
      "overriden_lines": 0
    }}
  }}
}}"#,
        old_hash, base_sha, old_hash
    );

    // Attach old-format note
    let git_ai_repo = git_ai::git::find_repository_in_path(repo.path().to_str().unwrap())
        .expect("find repository");
    notes_add(&git_ai_repo, base_sha, &old_note).expect("add old-format note");

    // Verify old format note is present and reads correctly
    let read_note = repo
        .read_authorship_note(base_sha)
        .expect("should have note");
    let log = AuthorshipLog::deserialize_from_string(&read_note)
        .expect("should deserialize old note");

    // Verify structure
    assert_eq!(log.metadata.prompts.len(), 1, "should have 1 prompt");
    assert_eq!(
        log.metadata.sessions.len(),
        0,
        "should have no sessions (old format)"
    );

    // Verify old prompt metadata
    let prompt = log
        .metadata
        .prompts
        .get(old_hash)
        .expect("old hash should be in prompts");
    assert_eq!(prompt.agent_id.tool, "cursor");
    assert_eq!(prompt.total_additions, 1);
    assert_eq!(prompt.accepted_lines, 1);

    // Verify attestation uses old format
    assert_eq!(log.attestations.len(), 1);
    assert_eq!(log.attestations[0].entries.len(), 1);
    assert_eq!(log.attestations[0].entries[0].hash, old_hash);

    // Verify blame works with old format note
    file.assert_committed_lines(crate::lines!["Human line".human(), "AI line".ai(),]);
}

// Test 2: Note with both old and new format attestations deserializes and blame works
#[test]
fn test_mixed_format_note_with_both_prompts_and_sessions() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut file = repo.filename("test.txt");
    file.set_contents(crate::lines!["Line 1", "Line 2".ai(), "Line 3".ai()]);
    let commit = repo.stage_all_and_commit("Initial commit").unwrap();

    // Replace with a mixed-format note that has BOTH prompts and sessions
    let old_hash = "abcd1234ef567890"; // 16-char hex for old format
    // The new hash will be extracted from the original note
    let original_note = repo
        .read_authorship_note(&commit.commit_sha)
        .expect("should have original note");
    let original_log = AuthorshipLog::deserialize_from_string(&original_note)
        .expect("parse original note");

    // Get the new-format session ID from the original note
    let new_hash = if !original_log.metadata.sessions.is_empty() {
        original_log.metadata.sessions.keys().next().unwrap().clone()
    } else {
        "s_1234567890abcd".to_string() // fallback
    };

    let mixed_note = format!(
        r#"test.txt
  {} 2-2
  {}::t_fedcba0987654321 3-3
---
{{
  "schema_version": "authorship/3.0.0",
  "git_ai_version": "1.3.3",
  "base_commit_sha": "{}",
  "prompts": {{
    "{}": {{
      "agent_id": {{"tool": "cursor", "id": "old_session", "model": "gpt-4"}},
      "human_author": null,
      "messages": [],
      "total_additions": 1,
      "total_deletions": 0,
      "accepted_lines": 1,
      "overriden_lines": 0
    }}
  }},
  "sessions": {{
    "{}": {{
      "agent_id": {{"tool": "mock_ai", "id": "new_session", "model": "gpt-4"}},
      "human_author": null,
      "messages": []
    }}
  }}
}}"#,
        old_hash, new_hash, commit.commit_sha, old_hash, new_hash
    );

    // Attach mixed-format note
    let git_ai_repo = git_ai::git::find_repository_in_path(repo.path().to_str().unwrap())
        .expect("find repository");
    notes_add(&git_ai_repo, &commit.commit_sha, &mixed_note).expect("add mixed-format note");

    // Read and verify the note
    let read_note = repo
        .read_authorship_note(&commit.commit_sha)
        .expect("should have note");
    let log = AuthorshipLog::deserialize_from_string(&read_note).expect("should parse note");

    // Verify both prompts and sessions are present
    assert_eq!(log.metadata.prompts.len(), 1, "should have 1 prompt");
    assert_eq!(log.metadata.sessions.len(), 1, "should have 1 session");

    // Verify attestations have both formats
    assert_eq!(log.attestations.len(), 1);
    assert_eq!(
        log.attestations[0].entries.len(),
        2,
        "should have 2 attestation entries"
    );

    let mut has_old_format = false;
    let mut has_new_format = false;
    for entry in &log.attestations[0].entries {
        if entry.hash.len() == 16 && !entry.hash.contains("::") {
            has_old_format = true;
        }
        if entry.hash.contains("::t_") {
            has_new_format = true;
        }
    }
    assert!(has_old_format, "should have old-format attestation");
    assert!(has_new_format, "should have new-format attestation");

    // Verify blame works for both formats
    file.assert_committed_lines(crate::lines![
        "Line 1".human(),
        "Line 2".ai(),
        "Line 3".ai(),
    ]);
}

// Test 3: Rebase chain with old and new format notes
#[test]
fn test_rebase_chain_with_old_and_new_format_notes() {
    let repo = TestRepo::new();

    // Create base commit on main
    let mut base = repo.filename("base.txt");
    base.set_contents(crate::lines!["Base line"]);
    repo.stage_all_and_commit("Base commit").unwrap();
    let default_branch = repo.current_branch();

    // Create feature branch
    repo.git(&["checkout", "-b", "feature"]).unwrap();

    // Commit A with AI content on feature
    let mut file_a = repo.filename("file_a.txt");
    file_a.set_contents(crate::lines!["Human line A", "AI line A".ai()]);
    let commit_a = repo.stage_all_and_commit("Commit A").unwrap();

    // Replace commit A's note with old-format note (using "claude" as tool name)
    let old_hash_a = "1111222233334444";
    let old_note_a = format!(
        r#"file_a.txt
  {} 2-2
---
{{
  "schema_version": "authorship/3.0.0",
  "git_ai_version": "1.3.3",
  "base_commit_sha": "{}",
  "prompts": {{
    "{}": {{
      "agent_id": {{"tool": "claude", "id": "old_agent", "model": "claude-3.5"}},
      "human_author": null,
      "messages": [],
      "total_additions": 1,
      "total_deletions": 0,
      "accepted_lines": 1,
      "overriden_lines": 0
    }}
  }}
}}"#,
        old_hash_a, commit_a.commit_sha, old_hash_a
    );
    let git_ai_repo = git_ai::git::find_repository_in_path(repo.path().to_str().unwrap())
        .expect("find repository");
    notes_add(&git_ai_repo, &commit_a.commit_sha, &old_note_a).expect("add old-format note A");

    // Commit B with AI content (will use new format naturally)
    let mut file_b = repo.filename("file_b.txt");
    file_b.set_contents(crate::lines!["Human line B", "AI line B".ai()]);
    repo.stage_all_and_commit("Commit B").unwrap();

    // Go back to main, add unrelated commit
    repo.git(&["checkout", &default_branch]).unwrap();
    let mut other = repo.filename("other.txt");
    other.set_contents(crate::lines!["Other line"]);
    repo.stage_all_and_commit("Other commit").unwrap();

    // Rebase feature onto main
    repo.git(&["checkout", "feature"]).unwrap();
    repo.git(&["rebase", &default_branch]).unwrap();

    // Find the two rebased commits (A' and B')
    let log_output = repo
        .git(&["log", "--oneline", "--no-decorate", "-2"])
        .unwrap();
    let lines: Vec<&str> = log_output.trim().lines().collect();
    assert_eq!(lines.len(), 2, "should have 2 commits");

    // Get commit SHAs (most recent first)
    let commit_b_prime_sha = lines[0].split_whitespace().next().unwrap();
    let commit_a_prime_sha = lines[1].split_whitespace().next().unwrap();

    // Verify commit A' still has prompts (old format preserved)
    let note_a_prime = repo
        .read_authorship_note(commit_a_prime_sha)
        .expect("commit A' should have note");
    let log_a_prime =
        AuthorshipLog::deserialize_from_string(&note_a_prime).expect("parse note A'");
    assert!(
        !log_a_prime.metadata.prompts.is_empty(),
        "commit A' should have prompts"
    );
    assert_eq!(
        log_a_prime.metadata.sessions.len(),
        0,
        "commit A' should not have sessions (old format)"
    );

    // Verify old prompt data preserved
    assert!(
        log_a_prime.metadata.prompts.contains_key(old_hash_a),
        "old hash should be preserved"
    );

    // Verify commit B' still has sessions (new format preserved)
    let note_b_prime = repo
        .read_authorship_note(commit_b_prime_sha)
        .expect("commit B' should have note");
    let log_b_prime =
        AuthorshipLog::deserialize_from_string(&note_b_prime).expect("parse note B'");
    assert!(
        !log_b_prime.metadata.sessions.is_empty(),
        "commit B' should have sessions (new format)"
    );

    // Verify blame works correctly on both commits
    repo.git(&["checkout", commit_a_prime_sha]).unwrap();
    file_a.assert_committed_lines(crate::lines!["Human line A".human(), "AI line A".ai(),]);

    repo.git(&["checkout", commit_b_prime_sha]).unwrap();
    file_b.assert_committed_lines(crate::lines!["Human line B".human(), "AI line B".ai(),]);
}

// Test 4: Cherry-pick old format note with AI lines preserved
#[test]
fn test_cherry_pick_old_format_note_with_ai_lines_preserved() {
    let repo = TestRepo::new();

    // Create initial commit on main
    let mut base = repo.filename("base.txt");
    base.set_contents(crate::lines!["Base line"]);
    repo.stage_all_and_commit("Initial commit").unwrap();
    let default_branch = repo.current_branch();

    // Create source branch
    repo.git(&["checkout", "-b", "source"]).unwrap();

    // Add AI content and commit
    let mut file = repo.filename("source.txt");
    file.set_contents(crate::lines!["Human line", "AI line".ai()]);
    let source_commit = repo.stage_all_and_commit("Source commit").unwrap();

    // Replace with old-format note INCLUDING attestation (using "copilot" as tool name)
    let old_hash = "9876543210fedcba";
    let old_note = format!(
        r#"source.txt
  {} 2-2
---
{{
  "schema_version": "authorship/3.0.0",
  "git_ai_version": "1.3.3",
  "base_commit_sha": "{}",
  "prompts": {{
    "{}": {{
      "agent_id": {{"tool": "copilot", "id": "cherry_agent", "model": "gpt-4"}},
      "human_author": null,
      "messages": [],
      "total_additions": 1,
      "total_deletions": 0,
      "accepted_lines": 1,
      "overriden_lines": 0
    }}
  }}
}}"#,
        old_hash, source_commit.commit_sha, old_hash
    );
    let git_ai_repo = git_ai::git::find_repository_in_path(repo.path().to_str().unwrap())
        .expect("find repository");
    notes_add(&git_ai_repo, &source_commit.commit_sha, &old_note).expect("add old-format note");

    // Go back to main and cherry-pick
    repo.git(&["checkout", &default_branch]).unwrap();
    repo.git(&["cherry-pick", &source_commit.commit_sha])
        .unwrap();

    // Get cherry-picked commit SHA
    let picked_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Verify cherry-picked commit has prompts (not sessions)
    let picked_note = repo
        .read_authorship_note(&picked_sha)
        .expect("cherry-picked commit should have note");
    let picked_log =
        AuthorshipLog::deserialize_from_string(&picked_note).expect("parse cherry-picked note");

    assert!(
        !picked_log.metadata.prompts.is_empty(),
        "cherry-picked commit should have prompts"
    );
    // Note: cherry-pick may add sessions if there are new changes; we primarily care that prompts are preserved
    assert!(
        picked_log.metadata.prompts.contains_key(old_hash),
        "old hash should be preserved in cherry-pick"
    );

    // Verify AI lines correctly attributed
    file.assert_committed_lines(crate::lines!["Human line".human(), "AI line".ai(),]);
}

// Test 5: Verify that sessions-format is the default for all new operations
// This test documents that the current system produces sessions, not prompts
#[test]
fn test_current_system_produces_sessions_not_prompts() {
    let repo = TestRepo::new();

    // Create commit with AI content using standard helpers
    let mut file = repo.filename("test.txt");
    file.set_contents(crate::lines!["Line 1", "AI line".ai()]);
    repo.stage_all_and_commit("AI commit").unwrap();

    // Read note
    let sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let note = repo
        .read_authorship_note(&sha)
        .expect("should have note");
    let log = AuthorshipLog::deserialize_from_string(&note).expect("parse note");

    // Should have sessions, NOT prompts (this is the new default)
    assert!(
        log.metadata.prompts.is_empty(),
        "new system should not produce prompts"
    );
    assert!(
        !log.metadata.sessions.is_empty(),
        "new system should produce sessions"
    );

    // Verify attestations use session format (s_::t_)
    let mut has_session_format = false;
    for file_att in &log.attestations {
        for entry in &file_att.entries {
            if entry.hash.starts_with("s_") && entry.hash.contains("::t_") {
                has_session_format = true;
                break;
            }
        }
    }
    assert!(
        has_session_format,
        "attestations should use session format (s_::t_)"
    );

    // Verify blame works
    file.assert_committed_lines(crate::lines!["Line 1".human(), "AI line".ai(),]);
}

// Test 6: Old format note roundtrips through operations without corruption
#[test]
fn test_old_format_note_roundtrips_without_corruption() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut file = repo.filename("test.txt");
    file.set_contents(crate::lines!["Line 1"]);
    let _initial_commit = repo.stage_all_and_commit("Initial").unwrap();

    // Create commit with AI content
    file.set_contents(crate::lines!["Line 1", "AI line".ai()]);
    let ai_commit = repo.stage_all_and_commit("AI commit").unwrap();

    // Replace with genuine old-format note with stats
    let old_hash = "0123456789abcdef";
    let old_note = format!(
        r#"test.txt
  {} 2-2
---
{{
  "schema_version": "authorship/3.0.0",
  "git_ai_version": "1.3.0",
  "base_commit_sha": "{}",
  "prompts": {{
    "{}": {{
      "agent_id": {{"tool": "roundtrip_tool", "id": "roundtrip_agent", "model": "roundtrip_model"}},
      "human_author": null,
      "messages": [],
      "total_additions": 42,
      "total_deletions": 7,
      "accepted_lines": 35,
      "overriden_lines": 3
    }}
  }},
  "humans": {{
    "h_fedcba9876543210": {{
      "author": "Test User <test@example.com>"
    }}
  }}
}}"#,
        old_hash, ai_commit.commit_sha, old_hash
    );
    let git_ai_repo = git_ai::git::find_repository_in_path(repo.path().to_str().unwrap())
        .expect("find repository");
    notes_add(&git_ai_repo, &ai_commit.commit_sha, &old_note).expect("add old-format note");

    // Read it back
    let note_v1 = repo
        .read_authorship_note(&ai_commit.commit_sha)
        .expect("should have note");
    let log_v1 = AuthorshipLog::deserialize_from_string(&note_v1).expect("parse note v1");

    // Verify structure
    assert_eq!(log_v1.metadata.prompts.len(), 1);
    assert_eq!(log_v1.metadata.sessions.len(), 0);
    assert_eq!(log_v1.metadata.humans.len(), 1);

    // Verify stats preserved
    let prompt_v1 = log_v1.metadata.prompts.get(old_hash).expect("should have old hash");
    assert_eq!(prompt_v1.total_additions, 42);
    assert_eq!(prompt_v1.total_deletions, 7);
    assert_eq!(prompt_v1.accepted_lines, 35);
    assert_eq!(prompt_v1.overriden_lines, 3);

    // Serialize and deserialize again (roundtrip)
    let serialized = log_v1.serialize_to_string().expect("serialize");
    let log_v2 = AuthorshipLog::deserialize_from_string(&serialized).expect("parse note v2");

    // Verify structure unchanged
    assert_eq!(log_v2.metadata.prompts.len(), 1);
    assert_eq!(log_v2.metadata.sessions.len(), 0);
    assert_eq!(log_v2.metadata.humans.len(), 1);

    // Verify stats still preserved
    let prompt_v2 = log_v2.metadata.prompts.get(old_hash).expect("should still have old hash");
    assert_eq!(prompt_v2.total_additions, 42);
    assert_eq!(prompt_v2.total_deletions, 7);
    assert_eq!(prompt_v2.accepted_lines, 35);
    assert_eq!(prompt_v2.overriden_lines, 3);

    // Verify serialized output doesn't contain "sessions" key
    assert!(
        !serialized.contains("\"sessions\""),
        "should not add sessions key to old-format note"
    );
}

// Test 7: Reset with old format notes
#[test]
fn test_reset_preserves_old_format_notes_in_working_log() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut file = repo.filename("test.txt");
    file.set_contents(crate::lines!["Line 1"]);
    repo.stage_all_and_commit("Initial").unwrap();

    // Create commit with AI content
    file.set_contents(crate::lines!["Line 1", "AI line".ai()]);
    let commit = repo.stage_all_and_commit("AI commit").unwrap();

    // Replace with old-format note (using "windsurf" as tool name)
    let old_hash = "aabbccddeeff1122";
    let old_note = format!(
        r#"test.txt
  {} 2-2
---
{{
  "schema_version": "authorship/3.0.0",
  "git_ai_version": "1.3.3",
  "base_commit_sha": "{}",
  "prompts": {{
    "{}": {{
      "agent_id": {{"tool": "windsurf", "id": "reset_agent", "model": "claude-3.5"}},
      "human_author": null,
      "messages": [],
      "total_additions": 1,
      "total_deletions": 0,
      "accepted_lines": 1,
      "overriden_lines": 0
    }}
  }}
}}"#,
        old_hash, commit.commit_sha, old_hash
    );
    let git_ai_repo = git_ai::git::find_repository_in_path(repo.path().to_str().unwrap())
        .expect("find repository");
    notes_add(&git_ai_repo, &commit.commit_sha, &old_note).expect("add old-format note");

    // Reset --soft to un-commit but keep changes staged
    repo.git(&["reset", "--soft", "HEAD~1"]).unwrap();

    // Re-commit
    repo.commit("Recommit").unwrap();

    // Verify note is preserved with prompts
    let new_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let new_note = repo
        .read_authorship_note(&new_sha)
        .expect("should have note after reset");
    let new_log = AuthorshipLog::deserialize_from_string(&new_note).expect("parse note");

    // Should have prompts from the old format note
    assert!(
        !new_log.metadata.prompts.is_empty(),
        "should preserve prompts after reset"
    );

    // Verify AI attribution still works
    file.assert_committed_lines(crate::lines!["Line 1".human(), "AI line".ai(),]);
}

// Test 8: Verify that new checkpoints always produce sessions, never prompts
#[test]
fn test_new_checkpoints_always_produce_sessions() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut file = repo.filename("test.txt");
    file.set_contents(crate::lines!["Line 1"]);
    repo.stage_all_and_commit("Initial").unwrap();

    // Use the standard helper which calls mock_ai checkpoint
    file.set_contents(crate::lines!["Line 1", "AI line".ai()]);
    repo.stage_all_and_commit("AI commit").unwrap();

    // Read note
    let sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let note = repo
        .read_authorship_note(&sha)
        .expect("should have note");
    let log = AuthorshipLog::deserialize_from_string(&note).expect("parse note");

    // Should have sessions, NOT prompts
    assert!(
        log.metadata.prompts.is_empty(),
        "new checkpoints should not produce prompts"
    );
    assert!(
        !log.metadata.sessions.is_empty(),
        "new checkpoints should produce sessions"
    );

    // Verify session format in attestations (s_::t_)
    let mut has_session_format = false;
    for file_att in &log.attestations {
        for entry in &file_att.entries {
            if entry.hash.starts_with("s_") && entry.hash.contains("::t_") {
                has_session_format = true;
                break;
            }
        }
    }
    assert!(
        has_session_format,
        "attestations should use session format (s_::t_)"
    );
}

// Test 9: Amend a commit that has an old-format note, with new-format checkpoints in the working log.
// This simulates: user had git-ai old version, made a commit (old prompts note), then upgraded git-ai,
// makes new edits (which produce session-format checkpoints), and amends the commit.
// The post-amend note must have BOTH old prompts AND new sessions.
#[test]
fn test_amend_old_prompts_commit_with_new_session_checkpoints() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("example.txt");

    // Step 1: Create initial commit with AI content
    let initial = "Human line 1\nAI old line\n";
    fs::write(&file_path, initial).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "example.txt"])
        .unwrap();
    let commit = repo.stage_all_and_commit("Initial commit").unwrap();

    // Step 2: Replace the note with an old-format note (simulating pre-upgrade git-ai)
    let old_hash = "deadbeef12345678"; // 16-char bare hex (old format)
    let old_note = format!(
        r#"example.txt
  {} 2-2
---
{{
  "schema_version": "authorship/3.0.0",
  "git_ai_version": "1.2.0",
  "base_commit_sha": "{}",
  "prompts": {{
    "{}": {{
      "agent_id": {{"tool": "cursor", "id": "old_session_abc", "model": "gpt-4"}},
      "human_author": null,
      "messages": [],
      "total_additions": 1,
      "total_deletions": 0,
      "accepted_lines": 1,
      "overriden_lines": 0
    }}
  }}
}}"#,
        old_hash, commit.commit_sha, old_hash
    );
    let git_ai_repo = git_ai::git::find_repository_in_path(repo.path().to_str().unwrap())
        .expect("find repository");
    notes_add(&git_ai_repo, &commit.commit_sha, &old_note).expect("attach old-format note");

    // Step 3: Make new edits and checkpoint with new-format (mock_ai produces trace_id)
    let edited = "Human line 1\nAI old line\nAI new line\n";
    fs::write(&file_path, edited).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "example.txt"])
        .unwrap();

    // Step 4: Amend the commit (this triggers the amend rewrite pipeline)
    repo.git(&["add", "."]).unwrap();
    repo.git(&["commit", "--amend", "-m", "Amended commit"])
        .unwrap();

    // Step 5: Read the post-amend note and verify BOTH formats are present
    let amended_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let note = repo
        .read_authorship_note(&amended_sha)
        .expect("amended commit should have note");
    let log = AuthorshipLog::deserialize_from_string(&note).expect("parse amended note");

    // The old-format prompt from the original note should still be there
    // (it was referenced by an attestation for line 2 which still exists)
    assert!(
        !log.metadata.prompts.is_empty(),
        "amended note should preserve old prompts from original note"
    );
    assert!(
        log.metadata.prompts.contains_key(old_hash),
        "old prompt hash should be preserved in amended note"
    );

    // The new checkpoint (with trace_id) should have produced a session
    assert!(
        !log.metadata.sessions.is_empty(),
        "amended note should have sessions from new checkpoint"
    );

    // Verify attestations include both formats
    let mut has_old_format_att = false;
    let mut has_new_format_att = false;
    for file_att in &log.attestations {
        for entry in &file_att.entries {
            if entry.hash == old_hash {
                has_old_format_att = true;
            }
            if entry.hash.starts_with("s_") && entry.hash.contains("::t_") {
                has_new_format_att = true;
            }
        }
    }
    assert!(
        has_old_format_att,
        "amended note should have old-format attestation hash"
    );
    assert!(
        has_new_format_att,
        "amended note should have new-format (s_::t_) attestation hash"
    );

    // Verify blame works correctly
    let mut file = repo.filename("example.txt");
    file.assert_committed_lines(crate::lines![
        "Human line 1".human(),
        "AI old line".ai(),
        "AI new line".ai(),
    ]);
}

// Test 10: Mixed working log where old-format checkpoints (trace_id: null, bare hex author_ids)
// coexist with new-format checkpoints (trace_id: Some, s_::t_ author_ids) in the SAME commit.
// This simulates: user upgrades git-ai mid-session. The working log has checkpoints from before
// the upgrade (no trace_id) and after the upgrade (with trace_id). On commit, old entries should
// go to prompts and new entries should go to sessions.
#[test]
fn test_mixed_working_log_old_and_new_checkpoints_produce_both_prompts_and_sessions() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("mixed.txt");

    // Step 1: Create a base commit (human only)
    let base = "Base line\n";
    fs::write(&file_path, base).unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "mixed.txt"])
        .unwrap();
    let base_commit = repo.stage_all_and_commit("Base commit").unwrap();

    // Step 2: Make an AI edit using current (new-format) checkpoint
    let edit1 = "Base line\nAI line from old version\n";
    fs::write(&file_path, edit1).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "mixed.txt"])
        .unwrap();

    // Step 3: Manipulate the checkpoints.jsonl to downgrade the FIRST AI checkpoint
    // to old format (remove trace_id, replace s_::t_ author_ids with bare hex)
    let working_log = repo.current_working_logs();
    let checkpoints_file = working_log.dir.join("checkpoints.jsonl");
    assert!(
        checkpoints_file.exists(),
        "checkpoints.jsonl should exist after checkpoint"
    );

    let content = fs::read_to_string(&checkpoints_file).expect("read checkpoints.jsonl");
    let mut modified_lines = Vec::new();

    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let mut checkpoint: Value =
            serde_json::from_str(line).expect("parse checkpoint JSON");

        // Find AI checkpoints and downgrade the first one we find
        let kind = checkpoint
            .get("kind")
            .and_then(|k| k.as_str())
            .unwrap_or("");

        if kind == "AiAgent" && checkpoint.get("trace_id").and_then(|t| t.as_str()).is_some() {
            // Compute the correct old-format author_id from agent_id fields
            // (this is what the old system would have stored)
            let agent_tool = checkpoint
                .get("agent_id")
                .and_then(|a| a.get("tool"))
                .and_then(|t| t.as_str())
                .unwrap_or("");
            let agent_id_str = checkpoint
                .get("agent_id")
                .and_then(|a| a.get("id"))
                .and_then(|i| i.as_str())
                .unwrap_or("");
            let old_author_id =
                git_ai::authorship::authorship_log_serialization::generate_short_hash(
                    agent_id_str,
                    agent_tool,
                );

            // Downgrade: remove trace_id, replace s_::t_ author_ids with old-format hash
            checkpoint["trace_id"] = Value::Null;

            if let Some(entries) = checkpoint.get_mut("entries").and_then(|e| e.as_array_mut()) {
                for entry in entries {
                    if let Some(attributions) =
                        entry.get_mut("attributions").and_then(|a| a.as_array_mut())
                    {
                        for attr in attributions {
                            if let Some(author_id) =
                                attr.get("author_id").and_then(|id| id.as_str())
                                && author_id.starts_with("s_")
                            {
                                attr["author_id"] = Value::String(old_author_id.clone());
                            }
                        }
                    }
                    if let Some(line_attrs) = entry
                        .get_mut("line_attributions")
                        .and_then(|a| a.as_array_mut())
                    {
                        for line_attr in line_attrs {
                            if let Some(author_id) =
                                line_attr.get("author_id").and_then(|id| id.as_str())
                                && author_id.starts_with("s_")
                            {
                                line_attr["author_id"] = Value::String(old_author_id.clone());
                            }
                        }
                    }
                }
            }
        }

        modified_lines
            .push(serde_json::to_string(&checkpoint).expect("serialize modified checkpoint"));
    }
    let new_content = modified_lines.join("\n") + "\n";
    fs::write(&checkpoints_file, new_content).expect("write modified checkpoints.jsonl");

    // Step 4: Make ANOTHER edit with new-format checkpoint (upgrade happened mid-session)
    let edit2 = "Base line\nAI line from old version\nAI line from new version\n";
    fs::write(&file_path, edit2).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "mixed.txt"])
        .unwrap();

    // Step 5: Commit - this should produce a note with BOTH prompts and sessions
    repo.git(&["add", "."]).unwrap();
    repo.commit("Mixed format commit").unwrap();

    // Step 6: Verify the resulting note
    let commit_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    assert_ne!(
        commit_sha, base_commit.commit_sha,
        "should be a new commit"
    );

    let note = repo
        .read_authorship_note(&commit_sha)
        .expect("mixed commit should have note");
    let log = AuthorshipLog::deserialize_from_string(&note).expect("parse mixed note");

    // Old-format checkpoint (trace_id: null, bare hex) should produce a prompt
    assert!(
        !log.metadata.prompts.is_empty(),
        "old-format checkpoint (no trace_id) should produce a prompt entry, got: prompts={:?}",
        log.metadata.prompts
    );

    // New-format checkpoint (trace_id: Some, s_::t_) should produce a session
    assert!(
        !log.metadata.sessions.is_empty(),
        "new-format checkpoint (with trace_id) should produce a session entry, got: sessions={:?}",
        log.metadata.sessions
    );

    // Verify attestations have both formats
    let mut has_old_att = false;
    let mut has_new_att = false;
    for file_att in &log.attestations {
        for entry in &file_att.entries {
            if !entry.hash.starts_with("s_")
                && !entry.hash.starts_with("h_")
                && entry.hash.len() == 16
            {
                has_old_att = true;
            }
            if entry.hash.starts_with("s_") && entry.hash.contains("::t_") {
                has_new_att = true;
            }
        }
    }
    assert!(
        has_old_att,
        "attestations should include old-format (bare hex) hash, got: {:?}",
        log.attestations
    );
    assert!(
        has_new_att,
        "attestations should include new-format (s_::t_) hash, got: {:?}",
        log.attestations
    );

    // The old-format prompt key should match an attestation hash (both are generate_short_hash output)
    let prompt_key = log.metadata.prompts.keys().next().unwrap();
    assert_eq!(
        prompt_key.len(),
        16,
        "prompt key should be 16 chars (old format)"
    );
    assert!(
        !prompt_key.starts_with("s_"),
        "prompt key should not have session prefix"
    );

    // Verify blame works correctly for all lines
    let mut file = repo.filename("mixed.txt");
    file.assert_committed_lines(crate::lines![
        "Base line".human(),
        "AI line from old version".ai(),
        "AI line from new version".ai(),
    ]);
}

// Test 11: Reset --soft of a commit with old-format note, then make new AI edits (sessions),
// then re-commit. The working log after reset has INITIAL from old note (bare hex prompts).
// New checkpoints produce sessions. Re-commit must have BOTH prompts and sessions.
#[test]
fn test_reset_soft_old_note_then_new_session_checkpoints() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("reset_test.txt");

    // Step 1: Create initial commit (needed as parent)
    let base = "Base line\n";
    fs::write(&file_path, base).unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "reset_test.txt"])
        .unwrap();
    repo.stage_all_and_commit("Base commit").unwrap();

    // Step 2: Create second commit with AI content
    let second = "Base line\nOld AI line\n";
    fs::write(&file_path, second).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "reset_test.txt"])
        .unwrap();
    let commit = repo.stage_all_and_commit("AI commit").unwrap();

    // Step 3: Replace the note with old-format (simulating pre-upgrade git-ai)
    let old_hash = "f1e2d3c4b5a69788";
    let old_note = format!(
        r#"reset_test.txt
  {} 2-2
---
{{
  "schema_version": "authorship/3.0.0",
  "git_ai_version": "1.2.0",
  "base_commit_sha": "{}",
  "prompts": {{
    "{}": {{
      "agent_id": {{"tool": "cursor", "id": "old_reset_session", "model": "gpt-4"}},
      "human_author": null,
      "messages": [],
      "total_additions": 1,
      "total_deletions": 0,
      "accepted_lines": 1,
      "overriden_lines": 0
    }}
  }}
}}"#,
        old_hash, commit.commit_sha, old_hash
    );
    let git_ai_repo = git_ai::git::find_repository_in_path(repo.path().to_str().unwrap())
        .expect("find repository");
    notes_add(&git_ai_repo, &commit.commit_sha, &old_note).expect("attach old-format note");

    // Step 4: Reset --soft HEAD~1 (uncommit, triggers working log reconstruction with old prompts)
    repo.git(&["reset", "--soft", "HEAD~1"]).unwrap();

    // Step 5: Make new AI edits (produces session-format checkpoints)
    let third = "Base line\nOld AI line\nNew session AI line\n";
    fs::write(&file_path, third).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "reset_test.txt"])
        .unwrap();

    // Step 6: Re-commit
    repo.git(&["add", "."]).unwrap();
    repo.commit("Re-committed with new edits").unwrap();

    // Step 7: Verify the resulting note has BOTH formats
    let new_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let note = repo
        .read_authorship_note(&new_sha)
        .expect("re-committed commit should have note");
    let log = AuthorshipLog::deserialize_from_string(&note).expect("parse note");

    // Old-format prompt from the reset commit's note should be preserved
    assert!(
        !log.metadata.prompts.is_empty(),
        "re-committed note should have prompts from old-format note (via reset reconstruction)"
    );

    // New-format session from the fresh checkpoint should be present
    assert!(
        !log.metadata.sessions.is_empty(),
        "re-committed note should have sessions from new checkpoint"
    );

    // Verify attestations have both formats
    let mut has_old_att = false;
    let mut has_new_att = false;
    for file_att in &log.attestations {
        for entry in &file_att.entries {
            if !entry.hash.starts_with("s_")
                && !entry.hash.starts_with("h_")
                && entry.hash.len() == 16
            {
                has_old_att = true;
            }
            if entry.hash.starts_with("s_") && entry.hash.contains("::t_") {
                has_new_att = true;
            }
        }
    }
    assert!(has_old_att, "should have old-format attestation hash");
    assert!(has_new_att, "should have new-format (s_::t_) attestation hash");

    // Verify blame
    let mut file = repo.filename("reset_test.txt");
    file.assert_committed_lines(crate::lines![
        "Base line".human(),
        "Old AI line".ai(),
        "New session AI line".ai(),
    ]);
}

// Test 12: Squash merge a feature branch where some commits have old-format notes
// and others have new-format notes. The squashed commit must contain BOTH prompts and sessions.
#[test]
fn test_squash_merge_mixed_format_commits() {
    let repo = TestRepo::new();

    // Step 1: Create base commit on main
    let mut base = repo.filename("base.txt");
    base.set_contents(crate::lines!["Base line"]);
    repo.stage_all_and_commit("Base commit").unwrap();
    let default_branch = repo.current_branch();

    // Step 2: Create feature branch
    repo.git(&["checkout", "-b", "feature-mixed"]).unwrap();

    // Step 3: Commit C1 with AI content, then replace with old-format note
    let mut file_a = repo.filename("feature_a.txt");
    file_a.set_contents(crate::lines!["Human A", "AI A".ai()]);
    let commit_a = repo.stage_all_and_commit("Feature commit A").unwrap();

    let old_hash = "aaaa1111bbbb2222";
    let old_note = format!(
        r#"feature_a.txt
  {} 2-2
---
{{
  "schema_version": "authorship/3.0.0",
  "git_ai_version": "1.2.0",
  "base_commit_sha": "{}",
  "prompts": {{
    "{}": {{
      "agent_id": {{"tool": "windsurf", "id": "old_squash_session", "model": "gpt-4"}},
      "human_author": null,
      "messages": [],
      "total_additions": 1,
      "total_deletions": 0,
      "accepted_lines": 1,
      "overriden_lines": 0
    }}
  }}
}}"#,
        old_hash, commit_a.commit_sha, old_hash
    );
    let git_ai_repo = git_ai::git::find_repository_in_path(repo.path().to_str().unwrap())
        .expect("find repository");
    notes_add(&git_ai_repo, &commit_a.commit_sha, &old_note).expect("attach old-format note");

    // Step 4: Commit C2 with AI content using standard helpers (produces new-format/sessions)
    let mut file_b = repo.filename("feature_b.txt");
    file_b.set_contents(crate::lines!["Human B", "AI B".ai()]);
    repo.stage_all_and_commit("Feature commit B").unwrap();

    // Step 5: Switch to main, squash merge
    repo.git(&["checkout", &default_branch]).unwrap();
    repo.git(&["merge", "--squash", "feature-mixed"]).unwrap();
    repo.commit("Squash merge mixed formats").unwrap();

    // Step 6: Verify squashed commit note has BOTH prompts and sessions
    let squash_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let note = repo
        .read_authorship_note(&squash_sha)
        .expect("squash commit should have note");
    let log = AuthorshipLog::deserialize_from_string(&note).expect("parse squash note");

    // C1's old-format prompts should be preserved
    assert!(
        !log.metadata.prompts.is_empty(),
        "squash note should have prompts from old-format commit C1"
    );

    // C2's new-format sessions should be present
    assert!(
        !log.metadata.sessions.is_empty(),
        "squash note should have sessions from new-format commit C2"
    );

    // Verify both file attestations work for blame
    file_a.assert_committed_lines(crate::lines!["Human A".human(), "AI A".ai(),]);
    file_b.assert_committed_lines(crate::lines!["Human B".human(), "AI B".ai(),]);
}

// Test 13: Stash and pop a mixed-format working log.
// The working log has old-format checkpoints (downgraded, no trace_id) + new-format checkpoints.
// After stash push + pop + commit, the note should have BOTH prompts and sessions.
#[test]
fn test_stash_pop_mixed_format_working_log() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("stash_test.txt");

    // Step 1: Create base commit
    let base = "Base line\n";
    fs::write(&file_path, base).unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "stash_test.txt"])
        .unwrap();
    repo.stage_all_and_commit("Base commit").unwrap();

    // Step 2: Make an AI edit (new format checkpoint)
    let edit1 = "Base line\nAI old line\n";
    fs::write(&file_path, edit1).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "stash_test.txt"])
        .unwrap();

    // Step 3: Downgrade that checkpoint to old format
    let working_log = repo.current_working_logs();
    let checkpoints_file = working_log.dir.join("checkpoints.jsonl");
    assert!(checkpoints_file.exists(), "checkpoints.jsonl should exist");

    let content = fs::read_to_string(&checkpoints_file).expect("read checkpoints");
    let mut modified_lines = Vec::new();
    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let mut checkpoint: Value = serde_json::from_str(line).expect("parse checkpoint");
        let kind = checkpoint.get("kind").and_then(|k| k.as_str()).unwrap_or("");
        if kind == "AiAgent" && checkpoint.get("trace_id").and_then(|t| t.as_str()).is_some() {
            let agent_tool = checkpoint
                .get("agent_id")
                .and_then(|a| a.get("tool"))
                .and_then(|t| t.as_str())
                .unwrap_or("");
            let agent_id_str = checkpoint
                .get("agent_id")
                .and_then(|a| a.get("id"))
                .and_then(|i| i.as_str())
                .unwrap_or("");
            let old_author_id =
                git_ai::authorship::authorship_log_serialization::generate_short_hash(
                    agent_id_str,
                    agent_tool,
                );
            checkpoint["trace_id"] = Value::Null;
            if let Some(entries) = checkpoint.get_mut("entries").and_then(|e| e.as_array_mut()) {
                for entry in entries {
                    if let Some(attributions) =
                        entry.get_mut("attributions").and_then(|a| a.as_array_mut())
                    {
                        for attr in attributions {
                            if let Some(author_id) =
                                attr.get("author_id").and_then(|id| id.as_str())
                                && author_id.starts_with("s_")
                            {
                                attr["author_id"] = Value::String(old_author_id.clone());
                            }
                        }
                    }
                    if let Some(line_attrs) = entry
                        .get_mut("line_attributions")
                        .and_then(|a| a.as_array_mut())
                    {
                        for line_attr in line_attrs {
                            if let Some(author_id) =
                                line_attr.get("author_id").and_then(|id| id.as_str())
                                && author_id.starts_with("s_")
                            {
                                line_attr["author_id"] = Value::String(old_author_id.clone());
                            }
                        }
                    }
                }
            }
        }
        modified_lines.push(serde_json::to_string(&checkpoint).expect("serialize"));
    }
    fs::write(&checkpoints_file, modified_lines.join("\n") + "\n").expect("write");

    // Step 4: Make another AI edit (new format)
    let edit2 = "Base line\nAI old line\nAI new line\n";
    fs::write(&file_path, edit2).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "stash_test.txt"])
        .unwrap();

    // Step 5: Stash
    repo.git(&["stash", "push", "-u"]).unwrap();

    // Step 6: Pop
    repo.git(&["stash", "pop"]).unwrap();

    // Step 7: Commit
    repo.git(&["add", "."]).unwrap();
    repo.commit("After stash pop").unwrap();

    // Step 8: Verify the note
    let sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let note = repo
        .read_authorship_note(&sha)
        .expect("post-stash commit should have note");
    let log = AuthorshipLog::deserialize_from_string(&note).expect("parse note");

    // Old-format checkpoint should produce prompt
    assert!(
        !log.metadata.prompts.is_empty(),
        "post-stash note should have prompts from old-format checkpoint"
    );

    // New-format checkpoint should produce session
    assert!(
        !log.metadata.sessions.is_empty(),
        "post-stash note should have sessions from new-format checkpoint"
    );

    // Verify blame
    let mut file = repo.filename("stash_test.txt");
    file.assert_committed_lines(crate::lines![
        "Base line".human(),
        "AI old line".ai(),
        "AI new line".ai(),
    ]);
}

// Test 14: Rebase a commit with old-format note that conflicts, AI resolves the conflict
// (producing new session-format checkpoints). The resulting note should have sessions from
// the conflict resolution. This documents that build_note_from_conflict_wl uses the
// working log's format, not the original commit's note format.
#[test]
fn test_rebase_conflict_old_note_ai_resolves_with_sessions() {
    let repo = TestRepo::new();

    // Step 1: Create base commit
    let mut file = repo.filename("conflict.txt");
    file.set_contents(crate::lines!["Original line"]);
    repo.stage_all_and_commit("Base commit").unwrap();
    let default_branch = repo.current_branch();

    // Step 2: Create feature branch with AI content
    repo.git(&["checkout", "-b", "feature-conflict"]).unwrap();
    file.set_contents(crate::lines!["Original line", "AI feature line".ai()]);
    let feature_commit = repo.stage_all_and_commit("Feature commit").unwrap();

    // Replace with old-format note
    let old_hash = "cccc3333dddd4444";
    let old_note = format!(
        r#"conflict.txt
  {} 2-2
---
{{
  "schema_version": "authorship/3.0.0",
  "git_ai_version": "1.2.0",
  "base_commit_sha": "{}",
  "prompts": {{
    "{}": {{
      "agent_id": {{"tool": "claude", "id": "old_rebase_session", "model": "claude-3.5"}},
      "human_author": null,
      "messages": [],
      "total_additions": 1,
      "total_deletions": 0,
      "accepted_lines": 1,
      "overriden_lines": 0
    }}
  }}
}}"#,
        old_hash, feature_commit.commit_sha, old_hash
    );
    let git_ai_repo = git_ai::git::find_repository_in_path(repo.path().to_str().unwrap())
        .expect("find repository");
    notes_add(&git_ai_repo, &feature_commit.commit_sha, &old_note)
        .expect("attach old-format note");

    // Step 3: Go back to main, make conflicting change
    repo.git(&["checkout", &default_branch]).unwrap();
    file.set_contents(crate::lines!["Modified base line"]);
    repo.stage_all_and_commit("Conflicting commit on main").unwrap();

    // Step 4: Rebase feature onto main (will conflict)
    repo.git(&["checkout", "feature-conflict"]).unwrap();
    let rebase_result = repo.git(&["rebase", &default_branch]);
    assert!(rebase_result.is_err(), "rebase should conflict");

    // Step 5: AI resolves the conflict by writing the merged file and checkpointing
    // (using set_contents which calls checkpoint mock_ai + stages the file)
    file.set_contents(crate::lines!["Modified base line".human(), "AI resolved line".ai()]);
    repo.git_with_env(&["rebase", "--continue"], &[("GIT_EDITOR", "true")], None)
        .unwrap();

    // Step 6: Verify the rebased commit's note
    repo.sync_daemon_force();
    let rebased_sha = repo.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let note = repo
        .read_authorship_note(&rebased_sha)
        .expect("rebased commit should have note");
    let log = AuthorshipLog::deserialize_from_string(&note).expect("parse rebased note");

    // The conflict was re-resolved from scratch, so build_note_from_conflict_wl
    // creates the note from the conflict working log. The AI checkpoint used new-format
    // (with trace_id), so the result should have sessions.
    assert!(
        !log.metadata.sessions.is_empty(),
        "rebased note should have sessions from AI conflict resolution checkpoint"
    );

    // Verify AI attribution on the resolved line
    file.assert_committed_lines(crate::lines![
        "Modified base line".human(),
        "AI resolved line".ai(),
    ]);
}

// Test 15: show-prompt with an old-format prompt ID finds it in metadata.prompts
#[test]
fn test_show_prompt_finds_old_format_prompt_by_id() {
    let repo = TestRepo::new();

    // Create commit with AI content
    let mut file = repo.filename("test.txt");
    file.set_contents(crate::lines!["Human line", "AI line".ai()]);
    let commit = repo.stage_all_and_commit("AI commit").unwrap();

    // Replace with old-format note
    let old_hash = "abcd1234efgh5678";
    let old_note = format!(
        r#"test.txt
  {} 2-2
---
{{
  "schema_version": "authorship/3.0.0",
  "git_ai_version": "1.2.0",
  "base_commit_sha": "{}",
  "prompts": {{
    "{}": {{
      "agent_id": {{"tool": "cursor", "id": "show_prompt_session", "model": "gpt-4"}},
      "human_author": null,
      "messages": [],
      "total_additions": 5,
      "total_deletions": 2,
      "accepted_lines": 3,
      "overriden_lines": 1
    }}
  }}
}}"#,
        old_hash, commit.commit_sha, old_hash
    );
    let git_ai_repo = git_ai::git::find_repository_in_path(repo.path().to_str().unwrap())
        .expect("find repository");
    notes_add(&git_ai_repo, &commit.commit_sha, &old_note).expect("attach old-format note");

    // show-prompt with --commit should find the old-format prompt
    let output = repo
        .git_ai(&["show-prompt", old_hash, "--commit", "HEAD"])
        .expect("show-prompt should find old-format prompt");

    let json: Value = serde_json::from_str(output.trim()).unwrap();
    assert_eq!(json["prompt_id"].as_str(), Some(old_hash));
    assert_eq!(json["prompt"]["agent_id"]["tool"].as_str(), Some("cursor"));
    assert_eq!(json["prompt"]["agent_id"]["model"].as_str(), Some("gpt-4"));
}

// Test 16: show-prompt searches history for old-format prompt
#[test]
fn test_show_prompt_finds_old_format_prompt_in_history() {
    let repo = TestRepo::new();

    // Create commit with AI content
    let mut file = repo.filename("test.txt");
    file.set_contents(crate::lines!["Human line", "AI line".ai()]);
    let commit = repo.stage_all_and_commit("AI commit").unwrap();

    // Replace with old-format note
    let old_hash = "1122334455667788";
    let old_note = format!(
        r#"test.txt
  {} 2-2
---
{{
  "schema_version": "authorship/3.0.0",
  "git_ai_version": "1.2.0",
  "base_commit_sha": "{}",
  "prompts": {{
    "{}": {{
      "agent_id": {{"tool": "windsurf", "id": "history_session", "model": "claude-3.5"}},
      "human_author": null,
      "messages": [],
      "total_additions": 10,
      "total_deletions": 3,
      "accepted_lines": 7,
      "overriden_lines": 2
    }}
  }}
}}"#,
        old_hash, commit.commit_sha, old_hash
    );
    let git_ai_repo = git_ai::git::find_repository_in_path(repo.path().to_str().unwrap())
        .expect("find repository");
    notes_add(&git_ai_repo, &commit.commit_sha, &old_note).expect("attach old-format note");

    // show-prompt without --commit should search history and find it
    let output = repo
        .git_ai(&["show-prompt", old_hash])
        .expect("show-prompt should find old-format prompt in history");

    let json: Value = serde_json::from_str(output.trim()).unwrap();
    assert_eq!(json["prompt_id"].as_str(), Some(old_hash));
    assert_eq!(json["prompt"]["agent_id"]["tool"].as_str(), Some("windsurf"));
}

// Test 17: git-ai stats --json correctly reads old-format prompt stats
// (total_additions, total_deletions, overriden_lines)
#[test]
fn test_stats_json_reads_old_format_prompt_stats() {
    let repo = TestRepo::new();

    // Create commit with AI content
    let mut file = repo.filename("test.txt");
    file.set_contents(crate::lines!["Human line", "AI line".ai()]);
    let commit = repo.stage_all_and_commit("AI commit").unwrap();

    // Replace with old-format note that has specific stats
    let old_hash = "aabb11223344ccdd";
    let old_note = format!(
        r#"test.txt
  {} 2-2
---
{{
  "schema_version": "authorship/3.0.0",
  "git_ai_version": "1.2.0",
  "base_commit_sha": "{}",
  "prompts": {{
    "{}": {{
      "agent_id": {{"tool": "cursor", "id": "stats_session", "model": "gpt-4o"}},
      "human_author": null,
      "messages": [],
      "total_additions": 15,
      "total_deletions": 5,
      "accepted_lines": 8,
      "overriden_lines": 3
    }}
  }}
}}"#,
        old_hash, commit.commit_sha, old_hash
    );
    let git_ai_repo = git_ai::git::find_repository_in_path(repo.path().to_str().unwrap())
        .expect("find repository");
    notes_add(&git_ai_repo, &commit.commit_sha, &old_note).expect("attach old-format note");

    // Run git-ai stats --json
    let output = repo.git_ai(&["stats", "--json"]).expect("stats should work");
    let json: Value = serde_json::from_str(output.trim()).unwrap();

    // Stats should reflect the old-format prompt's total_additions/total_deletions
    let total_ai_additions = json["total_ai_additions"].as_u64().unwrap_or(0);
    let total_ai_deletions = json["total_ai_deletions"].as_u64().unwrap_or(0);

    assert_eq!(
        total_ai_additions, 15,
        "stats should read total_additions from old-format prompt"
    );
    assert_eq!(
        total_ai_deletions, 5,
        "stats should read total_deletions from old-format prompt"
    );

    // Verify tool_model_breakdown includes the old-format prompt's agent
    // and propagates stats correctly
    let breakdown = &json["tool_model_breakdown"];
    let tool_stats = breakdown
        .get("cursor::gpt-4o")
        .expect("tool_model_breakdown should include old-format prompt's tool::model");
    assert_eq!(
        tool_stats["total_ai_additions"].as_u64().unwrap_or(0),
        15,
        "tool breakdown should have total_ai_additions from old-format prompt"
    );
    assert_eq!(
        tool_stats["total_ai_deletions"].as_u64().unwrap_or(0),
        5,
        "tool breakdown should have total_ai_deletions from old-format prompt"
    );
    assert_eq!(
        tool_stats["mixed_additions"].as_u64().unwrap_or(0),
        3,
        "tool breakdown should have overriden_lines from old-format prompt"
    );
}

// Test 18: git-ai diff --json --all-prompts with new-format (sessions-only) commit
// includes non-landing sessions in the output
#[test]
fn test_diff_json_all_prompts_includes_sessions() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut file = repo.filename("test.txt");
    file.set_contents(crate::lines!["Line 1"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Create second commit with AI content (produces sessions, not prompts)
    file.set_contents(crate::lines!["Line 1", "AI line".ai()]);
    let commit = repo.stage_all_and_commit("AI commit").unwrap();

    // Run git-ai diff --json --all-prompts
    let output = repo
        .git_ai(&["diff", "--json", "--all-prompts", &commit.commit_sha])
        .expect("diff --json --all-prompts should work");
    let json: Value = serde_json::from_str(output.trim()).unwrap();

    // The prompts array in diff output should include the session (converted to prompt format)
    let prompts = json["prompts"].as_object();
    assert!(
        prompts.is_some() && !prompts.unwrap().is_empty(),
        "diff --json --all-prompts should include sessions as prompts, got: {:?}",
        json["prompts"]
    );

    // The prompt should have mock_ai as the tool
    let first_prompt = prompts.unwrap().values().next().unwrap();
    assert_eq!(
        first_prompt["agent_id"]["tool"].as_str(),
        Some("mock_ai"),
        "session should be included as prompt with correct agent_id"
    );
}

// Test 19: git-ai diff --json --all-prompts with old-format (prompts-only) commit
// includes old prompts in the output
#[test]
fn test_diff_json_all_prompts_includes_old_format_prompts() {
    let repo = TestRepo::new();

    // Create initial commit
    let mut file = repo.filename("test.txt");
    file.set_contents(crate::lines!["Line 1"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Create second commit with AI content
    file.set_contents(crate::lines!["Line 1", "AI line".ai()]);
    let commit = repo.stage_all_and_commit("AI commit").unwrap();

    // Replace with old-format note
    let old_hash = "5566778899aabbcc";
    let old_note = format!(
        r#"test.txt
  {} 2-2
---
{{
  "schema_version": "authorship/3.0.0",
  "git_ai_version": "1.2.0",
  "base_commit_sha": "{}",
  "prompts": {{
    "{}": {{
      "agent_id": {{"tool": "copilot", "id": "diff_session", "model": "gpt-4"}},
      "human_author": null,
      "messages": [],
      "total_additions": 5,
      "total_deletions": 0,
      "accepted_lines": 1,
      "overriden_lines": 0
    }}
  }}
}}"#,
        old_hash, commit.commit_sha, old_hash
    );
    let git_ai_repo = git_ai::git::find_repository_in_path(repo.path().to_str().unwrap())
        .expect("find repository");
    notes_add(&git_ai_repo, &commit.commit_sha, &old_note).expect("attach old-format note");

    // Run git-ai diff --json --all-prompts
    let output = repo
        .git_ai(&["diff", "--json", "--all-prompts", &commit.commit_sha])
        .expect("diff --json --all-prompts should work");
    let json: Value = serde_json::from_str(output.trim()).unwrap();

    // The prompts should include the old-format prompt
    let prompts = json["prompts"].as_object();
    assert!(
        prompts.is_some() && !prompts.unwrap().is_empty(),
        "diff --json --all-prompts should include old-format prompts"
    );
    assert!(
        prompts.unwrap().contains_key(old_hash),
        "old prompt hash should be present in diff --all-prompts output"
    );
}
