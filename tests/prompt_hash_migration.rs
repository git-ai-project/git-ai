#[macro_use]
mod repos;
use repos::test_file::ExpectedLineExt;
use repos::test_repo::TestRepo;
use serde_json::Value;
use std::fs;

/// Helper function to truncate 16-char prompt hashes to 7 chars in checkpoint files
fn truncate_checkpoint_hashes(repo: &TestRepo, commit_sha: &str) {
    let repo_path = repo.path();
    let checkpoint_file = repo_path
        .join(".git")
        .join("ai")
        .join("working_logs")
        .join(commit_sha)
        .join("checkpoints.jsonl");

    if !checkpoint_file.exists() {
        return;
    }

    let content = fs::read_to_string(&checkpoint_file).expect("Failed to read checkpoint file");

    let mut modified_lines = Vec::new();
    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }

        let mut checkpoint: Value =
            serde_json::from_str(line).expect("Failed to parse checkpoint JSON");

        // Modify entries in the checkpoint
        if let Some(entries) = checkpoint.get_mut("entries").and_then(|e| e.as_array_mut()) {
            for entry in entries {
                // Truncate author_ids in attributions
                if let Some(attributions) =
                    entry.get_mut("attributions").and_then(|a| a.as_array_mut())
                {
                    for attr in attributions {
                        if let Some(author_id) =
                            attr.get_mut("author_id").and_then(|id| id.as_str())
                            && author_id.len() == 16
                        {
                            attr["author_id"] = Value::String(author_id[..7].to_string());
                        }
                    }
                }

                // Truncate author_ids in line_attributions
                if let Some(line_attrs) = entry
                    .get_mut("line_attributions")
                    .and_then(|a| a.as_array_mut())
                {
                    for line_attr in line_attrs {
                        if let Some(author_id) =
                            line_attr.get_mut("author_id").and_then(|id| id.as_str())
                            && author_id.len() == 16
                        {
                            line_attr["author_id"] = Value::String(author_id[..7].to_string());
                        }
                        // Also truncate overrode field if present
                        if let Some(overrode) =
                            line_attr.get_mut("overrode").and_then(|o| o.as_str())
                            && overrode.len() == 16
                        {
                            line_attr["overrode"] = Value::String(overrode[..7].to_string());
                        }
                    }
                }
            }
        }

        modified_lines
            .push(serde_json::to_string(&checkpoint).expect("Failed to serialize checkpoint"));
    }

    // Write back the modified checkpoints
    let new_content = modified_lines.join("\n") + "\n";
    fs::write(&checkpoint_file, new_content).expect("Failed to write modified checkpoint file");
}

/// Verify that all prompt IDs in an authorship log are 16 chars long
fn verify_prompt_ids_are_16_chars(
    authorship_log: &git_ai::authorship::authorship_log_serialization::AuthorshipLog,
) {
    // Check all prompt IDs in metadata.prompts
    for prompt_id in authorship_log.metadata.prompts.keys() {
        assert_eq!(
            prompt_id.len(),
            16,
            "Prompt ID '{}' should be 16 chars long, but is {} chars",
            prompt_id,
            prompt_id.len()
        );
    }

    // Check all hash values in attestations
    for attestation in &authorship_log.attestations {
        for entry in &attestation.entries {
            assert_eq!(
                entry.hash.len(),
                16,
                "Attestation hash '{}' should be 16 chars long, but is {} chars",
                entry.hash,
                entry.hash.len()
            );
        }
    }
}

/// Verify that all AI author_ids in checkpoints are 16 chars long (after migration)
/// This ensures no 7-char hashes remain after migration
fn verify_checkpoint_hashes_are_16_chars(repo: &TestRepo, commit_sha: &str) {
    let repo_path = repo.path();
    let checkpoint_file = repo_path
        .join(".git")
        .join("ai")
        .join("working_logs")
        .join(commit_sha)
        .join("checkpoints.jsonl");

    if !checkpoint_file.exists() {
        return;
    }

    let content = fs::read_to_string(&checkpoint_file).expect("Failed to read checkpoint file");

    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }

        let checkpoint: Value =
            serde_json::from_str(line).expect("Failed to parse checkpoint JSON");

        if let Some(entries) = checkpoint.get("entries").and_then(|e| e.as_array()) {
            for entry in entries {
                // Check attributions
                if let Some(attributions) = entry.get("attributions").and_then(|a| a.as_array()) {
                    for attr in attributions {
                        if let Some(author_id) = attr.get("author_id").and_then(|id| id.as_str()) {
                            // Skip "human" - it's not a hash
                            if author_id != "human" {
                                assert_eq!(
                                    author_id.len(),
                                    16,
                                    "Author ID '{}' in attributions should be 16 chars long (migration failed), but is {} chars",
                                    author_id,
                                    author_id.len()
                                );
                            }
                        }
                    }
                }

                // Check line_attributions
                if let Some(line_attrs) = entry.get("line_attributions").and_then(|a| a.as_array())
                {
                    for line_attr in line_attrs {
                        if let Some(author_id) =
                            line_attr.get("author_id").and_then(|id| id.as_str())
                        {
                            // Skip "human" - it's not a hash
                            if author_id != "human" {
                                assert_eq!(
                                    author_id.len(),
                                    16,
                                    "Author ID '{}' in line_attributions should be 16 chars long (migration failed), but is {} chars",
                                    author_id,
                                    author_id.len()
                                );
                            }
                        }
                        // Check overrode field - after migration, should be 16 chars if present
                        if let Some(overrode) = line_attr.get("overrode").and_then(|o| o.as_str()) {
                            assert_eq!(
                                overrode.len(),
                                16,
                                "Overrode ID '{}' should be 16 chars long (migration failed), but is {} chars",
                                overrode,
                                overrode.len()
                            );
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn test_prompt_hash_migration_ai_adds_lines_multiple_commits() {
    // Test AI adding lines across multiple commits
    let repo = TestRepo::new();
    let mut file = repo.filename("test.ts");

    file.set_contents(lines!["base_line", ""]);

    repo.stage_all_and_commit("Initial commit").unwrap();

    file.insert_at(
        1,
        lines!["ai_line1".ai(), "ai_line2".ai(), "ai_line3".ai(),],
    );

    let first_commit = repo.stage_all_and_commit("AI adds first batch").unwrap();
    let first_commit_sha = &first_commit.commit_sha;

    // Manually truncate checkpoint hashes to 7 chars
    truncate_checkpoint_hashes(&repo, first_commit_sha);

    file.insert_at(4, lines!["ai_line4".ai(), "ai_line5".ai(),]);

    let second_commit = repo.stage_all_and_commit("AI adds second batch").unwrap();

    // Verify that all prompt IDs are 16 chars in both commits
    verify_prompt_ids_are_16_chars(&first_commit.authorship_log);
    verify_prompt_ids_are_16_chars(&second_commit.authorship_log);

    // Verify checkpoint files also have 16-char hashes (migration should have happened during second commit)
    verify_checkpoint_hashes_are_16_chars(&repo, first_commit_sha);
    verify_checkpoint_hashes_are_16_chars(&repo, &second_commit.commit_sha);

    file.assert_lines_and_blame(lines![
        "base_line".human(),
        "ai_line1".ai(),
        "ai_line2".ai(),
        "ai_line3".ai(),
        "ai_line4".ai(),
        "ai_line5".ai(),
    ]);
}

#[test]
fn test_prompt_hash_migration_ai_adds_then_commits_in_batches() {
    // AI adds lines in multiple batches, committing separately
    let repo = TestRepo::new();
    let mut file = repo.filename("test.ts");

    file.set_contents(lines!["line1", "line2", "line3", "line4", ""]);

    repo.stage_all_and_commit("Initial commit").unwrap();

    // AI adds first batch of lines
    file.insert_at(4, lines!["ai_line5".ai(), "ai_line6".ai(), "ai_line7".ai()]);
    file.stage();

    let first_commit = repo.commit("Add lines 5-7").unwrap();
    let first_commit_sha = &first_commit.commit_sha;

    // Manually truncate checkpoint hashes to 7 chars
    truncate_checkpoint_hashes(&repo, first_commit_sha);

    // AI adds second batch of lines
    file.insert_at(
        7,
        lines!["ai_line8".ai(), "ai_line9".ai(), "ai_line10".ai()],
    );

    let second_commit = repo.stage_all_and_commit("Add lines 8-10").unwrap();

    // Verify that all prompt IDs are 16 chars in both commits
    verify_prompt_ids_are_16_chars(&first_commit.authorship_log);
    verify_prompt_ids_are_16_chars(&second_commit.authorship_log);

    // Verify checkpoint files also have 16-char hashes (migration should have happened during second commit)
    verify_checkpoint_hashes_are_16_chars(&repo, first_commit_sha);
    verify_checkpoint_hashes_are_16_chars(&repo, &second_commit.commit_sha);

    file.assert_lines_and_blame(lines![
        "line1".human(),
        "line2".human(),
        "line3".human(),
        "line4".human(),
        "ai_line5".ai(),
        "ai_line6".ai(),
        "ai_line7".ai(),
        "ai_line8".ai(),
        "ai_line9".ai(),
        "ai_line10".ai(),
    ]);
}

#[test]
fn test_prompt_hash_migration_unstaged_ai_lines_saved_to_working_log() {
    // Test that unstaged AI-authored lines are saved to the working log for the next commit
    let repo = TestRepo::new();
    let mut file = repo.filename("test.ts");

    file.set_contents(lines!["line1", "line2", "line3", ""]);

    repo.stage_all_and_commit("Initial commit").unwrap();

    // AI adds lines 4-7 and stages some
    file.insert_at(3, lines!["ai_line4".ai(), "ai_line5".ai()]);
    file.stage();

    // Commit only the staged lines
    let first_commit = repo.commit("Partial AI commit").unwrap();
    let first_commit_sha = &first_commit.commit_sha;

    // The commit should only have lines 4-5
    assert_eq!(first_commit.authorship_log.attestations.len(), 1);

    // Manually truncate checkpoint hashes to 7 chars
    truncate_checkpoint_hashes(&repo, first_commit_sha);

    // AI adds more lines that won't be staged
    file.insert_at(5, lines!["ai_line6".ai(), "ai_line7".ai()]);

    // Now stage and commit the remaining lines
    file.stage();
    let second_commit = repo.commit("Commit remaining AI lines").unwrap();

    // The second commit should also attribute lines 6-7 to AI
    assert_eq!(second_commit.authorship_log.attestations.len(), 1);

    // Verify that after migration, all prompt IDs are 16 chars
    verify_prompt_ids_are_16_chars(&first_commit.authorship_log);
    verify_prompt_ids_are_16_chars(&second_commit.authorship_log);

    // Verify checkpoint files also have 16-char hashes (migration should have happened)
    verify_checkpoint_hashes_are_16_chars(&repo, first_commit_sha);
    verify_checkpoint_hashes_are_16_chars(&repo, &second_commit.commit_sha);

    // Final state should have all AI lines attributed
    file.assert_lines_and_blame(lines![
        "line1".human(),
        "line2".human(),
        "line3".human(),
        "ai_line4".ai(),
        "ai_line5".ai(),
        "ai_line6".ai(),
        "ai_line7".ai(),
    ]);
}

reuse_tests_in_worktree!(
    test_prompt_hash_migration_ai_adds_lines_multiple_commits,
    test_prompt_hash_migration_ai_adds_then_commits_in_batches,
    test_prompt_hash_migration_unstaged_ai_lines_saved_to_working_log,
);
