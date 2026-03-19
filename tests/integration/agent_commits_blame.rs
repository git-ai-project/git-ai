//! Tests for agent commit detection in blame.
//!
//! These tests verify that commits made by known AI agents (identified by
//! their author email) are correctly attributed as AI-authored in blame output,
//! even when no explicit authorship note exists.
//!
//! TDD: These tests define the expected behavior BEFORE implementation.
//! They should fail initially and pass once agent commit detection is
//! integrated into overlay_ai_authorship.

use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;

/// Extract the JSON object from git-ai output, stripping any trailing log/migration lines.
fn extract_json(output: &str) -> &str {
    // Find the outermost JSON object: first '{' to its matching '}'
    let start = match output.find('{') {
        Some(i) => i,
        None => return output,
    };
    let mut depth = 0;
    let mut end = start;
    for (i, ch) in output[start..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = start + i + 1;
                    break;
                }
            }
            _ => {}
        }
    }
    &output[start..end]
}

// =============================================================================
// Helper: Create a commit with a specific author email using git_og (no hooks)
// =============================================================================

/// Write a file and commit it with a specific author identity, bypassing git-ai hooks.
/// This creates a commit with NO authorship note, simulating an agent commit.
fn commit_as_agent(
    repo: &TestRepo,
    filename: &str,
    contents: &str,
    author_name: &str,
    author_email: &str,
    message: &str,
) -> String {
    let file_path = repo.path().join(filename);
    // Create parent dirs if needed
    if let Some(parent) = file_path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&file_path, contents).unwrap();

    repo.git_og(&["add", filename]).unwrap();

    let author_arg = format!("{} <{}>", author_name, author_email);
    repo.git_og_with_env(&["commit", "-m", message, "--author", &author_arg], &[])
        .unwrap();

    // Return the commit SHA
    repo.git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string()
}

/// Write a file and commit it as a regular human user (bypassing hooks, no authorship note).
fn commit_as_human(repo: &TestRepo, filename: &str, contents: &str, message: &str) -> String {
    commit_as_agent(
        repo,
        filename,
        contents,
        "Human Developer",
        "human@example.com",
        message,
    )
}

// =============================================================================
// Basic agent detection: each known agent email
// =============================================================================

#[test]
fn test_agent_blame_cursor_email() {
    let repo = TestRepo::new();

    commit_as_agent(
        &repo,
        "test.rs",
        "fn main() {\n    println!(\"hello\");\n}\n",
        "Cursor Agent",
        "cursoragent@cursor.com",
        "feat: add main function",
    );

    let output = repo.git_ai(&["blame", "test.rs"]).unwrap();

    // All lines should show "cursor" as the author (AI agent)
    for line in output.lines() {
        assert!(
            line.contains("cursor"),
            "Expected 'cursor' in blame line, got: {}",
            line
        );
    }
}

#[test]
fn test_agent_blame_copilot_email() {
    let repo = TestRepo::new();

    commit_as_agent(
        &repo,
        "test.rs",
        "const x = 42;\n",
        "Copilot",
        "198982749+Copilot@users.noreply.github.com",
        "feat: add constant",
    );

    let output = repo.git_ai(&["blame", "test.rs"]).unwrap();

    // Should show github-copilot (which contains "copilot")
    for line in output.lines() {
        assert!(
            line.to_lowercase().contains("copilot"),
            "Expected 'copilot' in blame line, got: {}",
            line
        );
    }
}

#[test]
fn test_agent_blame_devin_email() {
    let repo = TestRepo::new();

    commit_as_agent(
        &repo,
        "test.rs",
        "use std::io;\n",
        "devin-ai-integration[bot]",
        "158243242+devin-ai-integration[bot]@users.noreply.github.com",
        "feat: add import",
    );

    let output = repo.git_ai(&["blame", "test.rs"]).unwrap();

    for line in output.lines() {
        assert!(
            line.contains("devin"),
            "Expected 'devin' in blame line, got: {}",
            line
        );
    }
}

#[test]
fn test_agent_blame_claude_email() {
    let repo = TestRepo::new();

    commit_as_agent(
        &repo,
        "test.py",
        "def hello():\n    return 'world'\n",
        "Claude",
        "noreply@anthropic.com",
        "feat: add hello",
    );

    let output = repo.git_ai(&["blame", "test.py"]).unwrap();

    for line in output.lines() {
        assert!(
            line.contains("claude"),
            "Expected 'claude' in blame line, got: {}",
            line
        );
    }
}

#[test]
fn test_agent_blame_codex_email() {
    let repo = TestRepo::new();

    commit_as_agent(
        &repo,
        "test.js",
        "console.log('hello');\n",
        "Codex",
        "noreply@openai.com",
        "feat: add log",
    );

    let output = repo.git_ai(&["blame", "test.js"]).unwrap();

    for line in output.lines() {
        assert!(
            line.contains("codex"),
            "Expected 'codex' in blame line, got: {}",
            line
        );
    }
}

// =============================================================================
// Mixed human + agent commits: line-level blame accuracy
// =============================================================================

#[test]
fn test_agent_blame_mixed_human_then_agent() {
    // Human writes first 2 lines, then agent adds 2 more lines
    let repo = TestRepo::new();

    // Human commit: 2 lines
    commit_as_human(
        &repo,
        "mixed.rs",
        "fn human_fn() {\n    // human code\n",
        "human commit",
    );

    // Agent commit: append 2 more lines
    commit_as_agent(
        &repo,
        "mixed.rs",
        "fn human_fn() {\n    // human code\nfn agent_fn() {\n    // cursor code\n",
        "Cursor Agent",
        "cursoragent@cursor.com",
        "feat: add agent function",
    );

    let output = repo.git_ai(&["blame", "mixed.rs"]).unwrap();
    let blame_lines: Vec<&str> = output.lines().collect();

    assert_eq!(blame_lines.len(), 4, "Should have 4 lines in blame output");

    // Lines 1-2: human (should NOT contain AI tool name)
    assert!(
        !blame_lines[0].contains("cursor"),
        "Line 1 should be human, got: {}",
        blame_lines[0]
    );
    assert!(
        !blame_lines[1].contains("cursor"),
        "Line 2 should be human, got: {}",
        blame_lines[1]
    );

    // Lines 3-4: agent (should contain "cursor")
    assert!(
        blame_lines[2].contains("cursor"),
        "Line 3 should be cursor agent, got: {}",
        blame_lines[2]
    );
    assert!(
        blame_lines[3].contains("cursor"),
        "Line 4 should be cursor agent, got: {}",
        blame_lines[3]
    );
}

#[test]
fn test_agent_blame_agent_then_human() {
    // Agent writes first, then human adds more
    let repo = TestRepo::new();

    // Agent commit: 2 lines
    commit_as_agent(
        &repo,
        "mixed.rs",
        "// generated by claude\nfn generated() {}\n",
        "Claude",
        "noreply@anthropic.com",
        "feat: generated code",
    );

    // Human commit: append 2 more lines
    commit_as_human(
        &repo,
        "mixed.rs",
        "// generated by claude\nfn generated() {}\n// human addition\nfn manual() {}\n",
        "human followup",
    );

    let output = repo.git_ai(&["blame", "mixed.rs"]).unwrap();
    let blame_lines: Vec<&str> = output.lines().collect();

    assert_eq!(blame_lines.len(), 4, "Should have 4 lines");

    // Lines 1-2: agent claude
    assert!(
        blame_lines[0].contains("claude"),
        "Line 1 should be claude, got: {}",
        blame_lines[0]
    );
    assert!(
        blame_lines[1].contains("claude"),
        "Line 2 should be claude, got: {}",
        blame_lines[1]
    );

    // Lines 3-4: human
    assert!(
        !blame_lines[2].contains("claude"),
        "Line 3 should be human, got: {}",
        blame_lines[2]
    );
    assert!(
        !blame_lines[3].contains("claude"),
        "Line 4 should be human, got: {}",
        blame_lines[3]
    );
}

// =============================================================================
// Multiple agents in the same file
// =============================================================================

#[test]
fn test_agent_blame_multiple_agents_same_file() {
    let repo = TestRepo::new();

    // Cursor writes initial code
    commit_as_agent(
        &repo,
        "multi.rs",
        "// cursor code\n",
        "Cursor Agent",
        "cursoragent@cursor.com",
        "cursor commit",
    );

    // Claude adds more code
    commit_as_agent(
        &repo,
        "multi.rs",
        "// cursor code\n// claude code\n",
        "Claude",
        "noreply@anthropic.com",
        "claude commit",
    );

    // Devin adds more code
    commit_as_agent(
        &repo,
        "multi.rs",
        "// cursor code\n// claude code\n// devin code\n",
        "devin-ai-integration[bot]",
        "158243242+devin-ai-integration[bot]@users.noreply.github.com",
        "devin commit",
    );

    let output = repo.git_ai(&["blame", "multi.rs"]).unwrap();
    let blame_lines: Vec<&str> = output.lines().collect();

    assert_eq!(blame_lines.len(), 3, "Should have 3 lines");

    assert!(
        blame_lines[0].contains("cursor"),
        "Line 1 should be cursor, got: {}",
        blame_lines[0]
    );
    assert!(
        blame_lines[1].contains("claude"),
        "Line 2 should be claude, got: {}",
        blame_lines[1]
    );
    assert!(
        blame_lines[2].contains("devin"),
        "Line 3 should be devin, got: {}",
        blame_lines[2]
    );
}

// =============================================================================
// Using assert_lines_and_blame with the TestFile harness
// =============================================================================

#[test]
fn test_agent_blame_assert_lines_cursor() {
    let repo = TestRepo::new();

    commit_as_agent(
        &repo,
        "cursor_file.rs",
        "line1\nline2\nline3\n",
        "Cursor Agent",
        "cursoragent@cursor.com",
        "cursor commit",
    );

    let mut file = crate::repos::test_file::TestFile::new_with_filename(
        repo.path().join("cursor_file.rs"),
        vec![],
        &repo,
    );

    file.assert_lines_and_blame(vec!["line1".ai(), "line2".ai(), "line3".ai()]);
}

#[test]
fn test_agent_blame_assert_lines_mixed_human_agent() {
    let repo = TestRepo::new();

    // Human writes first line
    commit_as_human(&repo, "mixed2.rs", "human line\n", "human commit");

    // Cursor adds two more lines
    commit_as_agent(
        &repo,
        "mixed2.rs",
        "human line\nagent line 1\nagent line 2\n",
        "Cursor Agent",
        "cursoragent@cursor.com",
        "cursor commit",
    );

    let mut file = crate::repos::test_file::TestFile::new_with_filename(
        repo.path().join("mixed2.rs"),
        vec![],
        &repo,
    );

    file.assert_lines_and_blame(vec![
        "human line".human(),
        "agent line 1".ai(),
        "agent line 2".ai(),
    ]);
}

// =============================================================================
// JSON output format: verify prompts are included for agent commits
// =============================================================================

#[test]
fn test_agent_blame_json_output() {
    let repo = TestRepo::new();

    let commit_sha = commit_as_agent(
        &repo,
        "json_test.rs",
        "fn hello() {}\nfn world() {}\n",
        "Cursor Agent",
        "cursoragent@cursor.com",
        "cursor commit",
    );

    let output = repo.git_ai(&["blame", "--json", "json_test.rs"]).unwrap();

    // Should be valid JSON (strip any trailing migration log lines)
    let json_str = extract_json(&output);
    let json: serde_json::Value =
        serde_json::from_str(json_str).expect("Output should be valid JSON");

    // Lines should be present
    assert!(json["lines"].is_object(), "Should have lines object");

    // Prompts should contain at least one entry for the simulated agent prompt
    assert!(json["prompts"].is_object(), "Should have prompts object");

    // The prompts should not be empty (simulated agent prompt should be present)
    let prompts = json["prompts"].as_object().unwrap();
    assert!(
        !prompts.is_empty(),
        "Prompts should contain simulated agent prompt data, got empty. Full JSON: {}",
        output
    );

    // Verify the prompt record contains the correct tool
    let prompt_entry = prompts.values().next().unwrap();
    assert_eq!(
        prompt_entry["agent_id"]["tool"].as_str().unwrap(),
        "cursor",
        "Prompt should have tool=cursor"
    );
    assert_eq!(
        prompt_entry["agent_id"]["model"].as_str().unwrap(),
        "unknown",
        "Prompt should have model=unknown"
    );

    // Verify the agent_id.id is the commit SHA
    assert_eq!(
        prompt_entry["agent_id"]["id"].as_str().unwrap(),
        commit_sha,
        "Prompt agent_id.id should be the commit SHA"
    );
}

#[test]
fn test_agent_blame_json_mixed_human_agent() {
    let repo = TestRepo::new();

    // Human commit
    commit_as_human(&repo, "json_mixed.rs", "human line\n", "human commit");

    // Agent commit
    commit_as_agent(
        &repo,
        "json_mixed.rs",
        "human line\nagent line\n",
        "Cursor Agent",
        "cursoragent@cursor.com",
        "cursor commit",
    );

    let output = repo.git_ai(&["blame", "--json", "json_mixed.rs"]).unwrap();
    let json: serde_json::Value = serde_json::from_str(extract_json(&output)).expect("Valid JSON");

    let lines = json["lines"].as_object().unwrap();
    let prompts = json["prompts"].as_object().unwrap();

    // In JSON mode, line_authors uses prompt hashes as names.
    // AI-authored lines map to a prompt hash that exists in prompts;
    // human-authored lines map to the human author name (not in prompts).

    // Find line 2's value: it may be keyed as "2", "2:2", or part of a range like "1-2"
    let mut line2_prompt_hash: Option<String> = None;
    for (range_key, val) in lines {
        let val_str = val.as_str().unwrap_or("");
        // Check if this range covers line 2
        if range_key == "2" || range_key == "2:2" {
            line2_prompt_hash = Some(val_str.to_string());
        } else if range_key.contains('-') {
            let parts: Vec<&str> = range_key.split('-').collect();
            if parts.len() == 2
                && let (Ok(start), Ok(end)) = (parts[0].parse::<u32>(), parts[1].parse::<u32>())
                && start <= 2
                && end >= 2
            {
                line2_prompt_hash = Some(val_str.to_string());
            }
        }
    }

    let line2_hash = line2_prompt_hash.expect("Line 2 should be present in lines map");

    // The hash should be a key in prompts (indicating AI authorship)
    assert!(
        prompts.contains_key(&line2_hash),
        "Line 2's value '{}' should be a prompt hash in prompts, prompts keys: {:?}",
        line2_hash,
        prompts.keys().collect::<Vec<_>>()
    );

    // The prompt should have tool=cursor
    let prompt = &prompts[&line2_hash];
    assert_eq!(
        prompt["agent_id"]["tool"].as_str().unwrap(),
        "cursor",
        "Prompt should have tool=cursor"
    );
}

// =============================================================================
// Porcelain output format
// =============================================================================

#[test]
fn test_agent_blame_porcelain_output() {
    let repo = TestRepo::new();

    commit_as_agent(
        &repo,
        "porcelain_test.rs",
        "agent line 1\nagent line 2\n",
        "Cursor Agent",
        "cursoragent@cursor.com",
        "cursor commit",
    );

    let output = repo
        .git_ai(&["blame", "--porcelain", "porcelain_test.rs"])
        .unwrap();

    // Porcelain output should contain author fields with the tool name
    assert!(
        output.contains("author cursor"),
        "Porcelain should show 'author cursor', got:\n{}",
        output
    );
}

#[test]
fn test_agent_blame_line_porcelain_output() {
    let repo = TestRepo::new();

    commit_as_agent(
        &repo,
        "line_porcelain_test.rs",
        "line 1\nline 2\n",
        "Claude",
        "noreply@anthropic.com",
        "claude commit",
    );

    let output = repo
        .git_ai(&["blame", "--line-porcelain", "line_porcelain_test.rs"])
        .unwrap();

    // Each line should have an author entry
    let author_count = output.matches("author claude").count();
    assert!(
        author_count >= 2,
        "Line porcelain should have 'author claude' for each line, found {} occurrences",
        author_count
    );
}

// =============================================================================
// Human commits should NOT be affected by agent detection
// =============================================================================

#[test]
fn test_agent_blame_human_email_not_detected_as_agent() {
    let repo = TestRepo::new();

    commit_as_human(&repo, "human.rs", "line 1\nline 2\n", "human commit");

    let output = repo.git_ai(&["blame", "human.rs"]).unwrap();

    // Should show the human author, NOT any AI tool name
    for line in output.lines() {
        assert!(
            !line.contains("cursor")
                && !line.contains("claude")
                && !line.contains("codex")
                && !line.contains("devin")
                && !line.contains("copilot"),
            "Human commit should not show AI tool name, got: {}",
            line
        );
    }
}

#[test]
fn test_agent_blame_similar_email_not_detected() {
    // Emails that look similar to agent emails but aren't exact matches
    let repo = TestRepo::new();

    commit_as_agent(
        &repo,
        "test.rs",
        "line 1\n",
        "NotCursor",
        "cursor@example.com", // NOT cursoragent@cursor.com
        "not cursor commit",
    );

    let output = repo.git_ai(&["blame", "test.rs"]).unwrap();

    // Should NOT be detected as cursor agent
    for line in output.lines() {
        assert!(
            !line.contains("cursor") || line.contains("NotCursor"),
            "Similar email should not trigger agent detection, got: {}",
            line
        );
    }
}

// =============================================================================
// Agent commit with existing authorship note: note should take precedence
// =============================================================================

#[test]
fn test_agent_email_with_authorship_note_uses_note() {
    // When a commit has BOTH an agent email AND an authorship note,
    // the authorship note should take precedence (existing behavior).
    let repo = TestRepo::new();

    // Use the normal TestFile flow which creates authorship notes via checkpoints
    let mut file = repo.filename("noted.rs");
    file.set_contents(crate::lines!["ai line 1".ai(), "human line 1".human(),]);
    repo.stage_all_and_commit("commit with note").unwrap();

    // Verify blame uses the authorship note (mock_ai) not any agent email detection
    let output = repo.git_ai(&["blame", "noted.rs"]).unwrap();
    assert!(
        output.contains("mock_ai"),
        "Should use authorship note tool name (mock_ai), got:\n{}",
        output
    );
}

// =============================================================================
// Edge cases
// =============================================================================

#[test]
fn test_agent_blame_single_line_file() {
    let repo = TestRepo::new();

    commit_as_agent(
        &repo,
        "single.rs",
        "only line\n",
        "Cursor Agent",
        "cursoragent@cursor.com",
        "single line commit",
    );

    let output = repo.git_ai(&["blame", "single.rs"]).unwrap();
    let blame_lines: Vec<&str> = output.lines().collect();

    assert_eq!(blame_lines.len(), 1);
    assert!(
        blame_lines[0].contains("cursor"),
        "Single line should be detected as cursor, got: {}",
        blame_lines[0]
    );
}

#[test]
fn test_agent_blame_large_agent_commit() {
    // Agent creates a file with many lines
    let repo = TestRepo::new();

    let mut content = String::new();
    for i in 1..=50 {
        content.push_str(&format!("line {}\n", i));
    }

    commit_as_agent(
        &repo,
        "large.rs",
        &content,
        "Claude",
        "noreply@anthropic.com",
        "large agent commit",
    );

    let output = repo.git_ai(&["blame", "large.rs"]).unwrap();
    let blame_lines: Vec<&str> = output.lines().collect();

    assert_eq!(blame_lines.len(), 50);

    // All 50 lines should be attributed to claude
    for (i, line) in blame_lines.iter().enumerate() {
        assert!(
            line.contains("claude"),
            "Line {} should be claude, got: {}",
            i + 1,
            line
        );
    }
}

#[test]
fn test_agent_blame_agent_modifies_human_lines() {
    // Human creates file, agent replaces a line in the middle
    let repo = TestRepo::new();

    commit_as_human(
        &repo,
        "modify.rs",
        "line 1\nline 2\nline 3\n",
        "human initial",
    );

    // Agent modifies line 2
    commit_as_agent(
        &repo,
        "modify.rs",
        "line 1\nmodified by agent\nline 3\n",
        "Cursor Agent",
        "cursoragent@cursor.com",
        "agent modifies line 2",
    );

    let output = repo.git_ai(&["blame", "modify.rs"]).unwrap();
    let blame_lines: Vec<&str> = output.lines().collect();

    assert_eq!(blame_lines.len(), 3);

    // Line 1: human
    assert!(
        !blame_lines[0].contains("cursor"),
        "Line 1 should be human, got: {}",
        blame_lines[0]
    );
    // Line 2: agent (modified)
    assert!(
        blame_lines[1].contains("cursor"),
        "Line 2 should be cursor (agent modified), got: {}",
        blame_lines[1]
    );
    // Line 3: human
    assert!(
        !blame_lines[2].contains("cursor"),
        "Line 3 should be human, got: {}",
        blame_lines[2]
    );
}

#[test]
fn test_agent_blame_with_line_range() {
    // Test -L flag with agent commits
    let repo = TestRepo::new();

    commit_as_agent(
        &repo,
        "ranges.rs",
        "line 1\nline 2\nline 3\nline 4\nline 5\n",
        "Cursor Agent",
        "cursoragent@cursor.com",
        "cursor commit",
    );

    let output = repo.git_ai(&["blame", "-L", "2,4", "ranges.rs"]).unwrap();
    let blame_lines: Vec<&str> = output.lines().collect();

    assert_eq!(blame_lines.len(), 3, "Should only show lines 2-4");

    for line in &blame_lines {
        assert!(
            line.contains("cursor"),
            "All lines in range should be cursor, got: {}",
            line
        );
    }
}

#[test]
fn test_agent_blame_interleaved_agents_and_humans() {
    // Complex scenario: human, agent1, human, agent2 all contributing to same file
    let repo = TestRepo::new();

    // Human writes line 1
    commit_as_human(&repo, "interleaved.rs", "human 1\n", "human 1");

    // Cursor adds line 2
    commit_as_agent(
        &repo,
        "interleaved.rs",
        "human 1\ncursor line\n",
        "Cursor Agent",
        "cursoragent@cursor.com",
        "cursor commit",
    );

    // Human adds line 3
    commit_as_human(
        &repo,
        "interleaved.rs",
        "human 1\ncursor line\nhuman 2\n",
        "human 2",
    );

    // Claude adds line 4
    commit_as_agent(
        &repo,
        "interleaved.rs",
        "human 1\ncursor line\nhuman 2\nclaude line\n",
        "Claude",
        "noreply@anthropic.com",
        "claude commit",
    );

    let output = repo.git_ai(&["blame", "interleaved.rs"]).unwrap();
    let blame_lines: Vec<&str> = output.lines().collect();

    assert_eq!(blame_lines.len(), 4);

    // Line 1: human
    assert!(
        !blame_lines[0].contains("cursor") && !blame_lines[0].contains("claude"),
        "Line 1 should be human, got: {}",
        blame_lines[0]
    );
    // Line 2: cursor
    assert!(
        blame_lines[1].contains("cursor"),
        "Line 2 should be cursor, got: {}",
        blame_lines[1]
    );
    // Line 3: human
    assert!(
        !blame_lines[2].contains("cursor") && !blame_lines[2].contains("claude"),
        "Line 3 should be human, got: {}",
        blame_lines[2]
    );
    // Line 4: claude
    assert!(
        blame_lines[3].contains("claude"),
        "Line 4 should be claude, got: {}",
        blame_lines[3]
    );
}

// =============================================================================
// Verify stats in JSON output for simulated agent authorship
// =============================================================================

#[test]
fn test_agent_blame_json_stats() {
    let repo = TestRepo::new();

    commit_as_agent(
        &repo,
        "stats_test.rs",
        "line 1\nline 2\nline 3\n",
        "Cursor Agent",
        "cursoragent@cursor.com",
        "cursor commit",
    );

    let output = repo.git_ai(&["blame", "--json", "stats_test.rs"]).unwrap();
    let json: serde_json::Value = serde_json::from_str(extract_json(&output)).expect("Valid JSON");

    let prompts = json["prompts"].as_object().unwrap();
    assert!(!prompts.is_empty(), "Should have simulated prompt");

    let prompt = prompts.values().next().unwrap();

    // Verify simulated stats
    assert_eq!(
        prompt["accepted_lines"].as_u64().unwrap(),
        3,
        "accepted_lines should equal total lines in commit for this file"
    );
    assert_eq!(
        prompt["total_additions"].as_u64().unwrap(),
        3,
        "total_additions should equal total lines"
    );
    assert_eq!(
        prompt["overriden_lines"].as_u64().unwrap(),
        0,
        "overriden_lines should be 0 (simulated)"
    );
    assert_eq!(
        prompt["total_deletions"].as_u64().unwrap(),
        0,
        "total_deletions should be 0 (simulated)"
    );
}

// =============================================================================
// All agents: comprehensive coverage
// =============================================================================

#[test]
fn test_agent_blame_all_agents_in_separate_files() {
    let repo = TestRepo::new();

    let agents = [
        (
            "cursor_file.rs",
            "Cursor Agent",
            "cursoragent@cursor.com",
            "cursor",
        ),
        (
            "copilot_file.rs",
            "Copilot",
            "198982749+Copilot@users.noreply.github.com",
            "copilot",
        ),
        (
            "devin_file.rs",
            "devin-ai-integration[bot]",
            "158243242+devin-ai-integration[bot]@users.noreply.github.com",
            "devin",
        ),
        (
            "claude_file.rs",
            "Claude",
            "noreply@anthropic.com",
            "claude",
        ),
        ("codex_file.rs", "Codex", "noreply@openai.com", "codex"),
    ];

    for (filename, name, email, expected_tool) in &agents {
        commit_as_agent(
            &repo,
            filename,
            &format!("// written by {}\n", expected_tool),
            name,
            email,
            &format!("{} commit", expected_tool),
        );
    }

    // Verify each file shows the correct tool
    for (filename, _, _, expected_tool) in &agents {
        let output = repo.git_ai(&["blame", filename]).unwrap();
        assert!(
            output.to_lowercase().contains(expected_tool),
            "File {} should show tool '{}', got:\n{}",
            filename,
            expected_tool,
            output
        );
    }
}

// =============================================================================
// Incremental output format
// =============================================================================

#[test]
fn test_agent_blame_incremental_output() {
    let repo = TestRepo::new();

    commit_as_agent(
        &repo,
        "incremental_test.rs",
        "line 1\nline 2\n",
        "Cursor Agent",
        "cursoragent@cursor.com",
        "cursor commit",
    );

    let output = repo
        .git_ai(&["blame", "--incremental", "incremental_test.rs"])
        .unwrap();

    // Incremental format should contain author with the tool name
    assert!(
        output.contains("author cursor"),
        "Incremental output should contain 'author cursor', got:\n{}",
        output
    );
}
