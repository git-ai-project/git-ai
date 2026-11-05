use crate::repos::test_repo::TestRepo;
use git_ai::authorship::authorship_log::LineRange;
use git_ai::authorship::authorship_log::PromptRecord;
use git_ai::authorship::authorship_log_serialization::AttestationEntry;
use git_ai::authorship::authorship_log_serialization::AuthorshipLog;
use git_ai::authorship::authorship_log_serialization::FileAttestation;
use git_ai::authorship::authorship_log_serialization::generate_short_hash;
use git_ai::authorship::stats::*;
use git_ai::authorship::working_log::AgentId;
use git_ai::git::repository::find_repository_in_path;
use std::collections::BTreeMap;
use std::collections::HashMap;

#[test]
fn test_stats_for_simple_ai_commit() {
    let repo = TestRepo::new();

    std::fs::write(repo.path().join("test.txt"), "Line1\n").unwrap();
    repo.git(&["add", "test.txt"]).unwrap();

    repo.git_ai(&["checkpoint", "mock_known_human", "test.txt"])
        .unwrap();

    repo.stage_all_and_commit("Initial commit").unwrap();

    // AI adds 2 lines
    std::fs::write(repo.path().join("test.txt"), "Line1\nLine 2\nLine 3\n").unwrap();
    repo.git(&["add", "test.txt"]).unwrap();

    repo.git_ai(&["checkpoint", "mock_ai", "test.txt"]).unwrap();

    repo.stage_all_and_commit("AI adds lines").unwrap();

    // Get the commit SHA for the AI commit
    let head_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();

    // Test our stats function
    let stats = stats_for_commit_stats(&gitai_repo, &head_sha, &[]).unwrap();

    // Verify the stats
    assert_eq!(
        stats.human_additions, 0,
        "No human additions in AI-only commit"
    );
    assert_eq!(stats.ai_additions, 2, "AI added 2 lines");
    assert_eq!(stats.ai_accepted, 2, "AI lines were accepted");
    assert_eq!(
        stats.git_diff_added_lines, 2,
        "Git diff shows 2 added lines"
    );
    assert_eq!(
        stats.git_diff_deleted_lines, 0,
        "Git diff shows 0 deleted lines"
    );
}

#[test]
fn test_stats_for_mixed_commit() {
    let repo = TestRepo::new();

    std::fs::write(repo.path().join("test.txt"), "Base line\n").unwrap();
    repo.git(&["add", "test.txt"]).unwrap();

    repo.git_ai(&["checkpoint", "mock_known_human", "test.txt"])
        .unwrap();

    repo.stage_all_and_commit("Initial commit").unwrap();

    // AI adds lines
    std::fs::write(
        repo.path().join("test.txt"),
        "Base line\nAI line 1\nAI line 2\n",
    )
    .unwrap();
    repo.git(&["add", "test.txt"]).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "test.txt"]).unwrap();

    // Human adds lines
    std::fs::write(
        repo.path().join("test.txt"),
        "Base line\nAI line 1\nAI line 2\nHuman line 1\nHuman line 2\n",
    )
    .unwrap();
    repo.git(&["add", "test.txt"]).unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "test.txt"])
        .unwrap();

    repo.stage_all_and_commit("Mixed commit").unwrap();

    let head_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();
    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let stats = stats_for_commit_stats(&gitai_repo, &head_sha, &[]).unwrap();

    // Verify the stats
    // trigger_checkpoint_with_author produces KnownHuman checkpoints (post Task 9),
    // so human-written lines have h_-prefixed attestation entries → human_additions.
    assert_eq!(stats.human_additions, 2, "Human added 2 lines");
    assert_eq!(stats.ai_additions, 2, "AI added 2 lines");
    assert_eq!(stats.ai_accepted, 2, "AI lines were accepted");
    assert_eq!(
        stats.git_diff_added_lines, 4,
        "Git diff shows 4 added lines total"
    );
    assert_eq!(
        stats.git_diff_deleted_lines, 0,
        "Git diff shows 0 deleted lines"
    );
}

#[test]
fn test_stats_for_initial_commit() {
    let repo = TestRepo::new();

    std::fs::write(repo.path().join("test.txt"), "Line1\nLine2\nLine3\n").unwrap();
    repo.git(&["add", "test.txt"]).unwrap();

    repo.git_ai(&["checkpoint", "mock_known_human", "test.txt"])
        .unwrap();

    repo.stage_all_and_commit("Initial commit").unwrap();

    let head_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();
    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let stats = stats_for_commit_stats(&gitai_repo, &head_sha, &[]).unwrap();

    // KnownHuman checkpoints record h_<hash> attributions for all human-edited lines,
    // so they appear as human_additions (not unknown) even on pure-human commits.
    assert_eq!(
        stats.human_additions, 3,
        "All 3 lines should be KnownHuman-attested human_additions"
    );
    assert_eq!(
        stats.unknown_additions, 0,
        "No unattested lines in a KnownHuman-checkpointed commit"
    );
    assert_eq!(stats.ai_additions, 0, "No AI additions in initial commit");
    assert_eq!(stats.ai_accepted, 0, "No AI lines to accept");
    assert_eq!(
        stats.git_diff_added_lines, 3,
        "Git diff shows 3 added lines (initial commit)"
    );
    assert_eq!(
        stats.git_diff_deleted_lines, 0,
        "Git diff shows 0 deleted lines"
    );
}

#[test]
fn test_stats_ignores_single_lockfile() {
    let repo = TestRepo::new();

    // Initial commit
    std::fs::create_dir_all(repo.path().join("src")).unwrap();
    std::fs::write(repo.path().join("src/main.rs"), "fn main() {}\n").unwrap();
    repo.git(&["add", "src/main.rs"]).unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "src/main.rs"])
        .unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Commit that adds source code and a large lockfile
    std::fs::write(
        repo.path().join("src/main.rs"),
        "fn main() {}\nfn helper() {}\n",
    )
    .unwrap();
    repo.git(&["add", "src/main.rs"]).unwrap();
    std::fs::write(repo.path().join("Cargo.lock"), "# lockfile\n".repeat(1000)).unwrap();
    repo.git(&["add", "Cargo.lock"]).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "src/main.rs"])
        .unwrap();
    repo.stage_all_and_commit("Add helper and deps").unwrap();

    let head_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();

    // Test WITHOUT ignore - should count lockfile
    let stats_with_lockfile = stats_for_commit_stats(&gitai_repo, &head_sha, &[]).unwrap();
    assert_eq!(stats_with_lockfile.git_diff_added_lines, 1001); // 1 source + 1000 lockfile
    // The uncheckpointed lockfile lines stay unattested (unknown bucket); each
    // "# lockfile" payload is 10 ASCII scalars/bytes + 1 synthetic LF = 11.
    assert_eq!(stats_with_lockfile.unknown_additions, 1000);
    assert_eq!(stats_with_lockfile.unknown_additions_chars, 11_000);
    assert_eq!(stats_with_lockfile.unknown_additions_bytes, 11_000);
    assert_eq!(stats_with_lockfile.ai_additions_chars, 15);
    assert_eq!(stats_with_lockfile.ai_additions_bytes, 15);

    // Test WITH ignore - should exclude lockfile
    let ignore_patterns = vec!["Cargo.lock".to_string()];
    let stats_without_lockfile =
        stats_for_commit_stats(&gitai_repo, &head_sha, &ignore_patterns).unwrap();
    assert_eq!(stats_without_lockfile.git_diff_added_lines, 1); // Only 1 source line
    assert_eq!(stats_without_lockfile.ai_additions, 1);

    // Ignored files add 0 chars/bytes as well as 0 lines. Only the
    // AI-attributed source line keeps volume: "fn helper() {}" is 14 ASCII
    // scalars/bytes + 1 synthetic LF = 15.
    assert_eq!(stats_without_lockfile.git_diff_added_chars, 15);
    assert_eq!(stats_without_lockfile.git_diff_added_bytes, 15);
    assert_eq!(stats_without_lockfile.ai_additions_chars, 15);
    assert_eq!(stats_without_lockfile.ai_additions_bytes, 15);
    assert_eq!(stats_without_lockfile.ai_accepted_chars, 15);
    assert_eq!(stats_without_lockfile.ai_accepted_bytes, 15);
    assert_eq!(stats_without_lockfile.human_additions_chars, 0);
    assert_eq!(stats_without_lockfile.human_additions_bytes, 0);
    assert_eq!(stats_without_lockfile.unknown_additions_chars, 0);
    assert_eq!(stats_without_lockfile.unknown_additions_bytes, 0);
    assert_eq!(stats_without_lockfile.git_diff_deleted_chars, 0);
    assert_eq!(stats_without_lockfile.git_diff_deleted_bytes, 0);
}

#[test]
fn test_stats_ignores_multiple_lockfiles() {
    let repo = TestRepo::new();

    // Initial commit
    std::fs::write(repo.path().join("README.md"), "# Project\n").unwrap();
    repo.git(&["add", "README.md"]).unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "README.md"])
        .unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Commit that updates multiple lockfiles and one source file
    std::fs::write(repo.path().join("README.md"), "# Project\n## New\n").unwrap();
    repo.git(&["add", "README.md"]).unwrap();
    std::fs::write(repo.path().join("Cargo.lock"), "# cargo\n".repeat(500)).unwrap();
    repo.git(&["add", "Cargo.lock"]).unwrap();
    std::fs::write(repo.path().join("package-lock.json"), "{}\n".repeat(500)).unwrap();
    repo.git(&["add", "package-lock.json"]).unwrap();
    std::fs::write(repo.path().join("yarn.lock"), "# yarn\n".repeat(500)).unwrap();
    repo.git(&["add", "yarn.lock"]).unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "README.md"])
        .unwrap();
    repo.stage_all_and_commit("Update deps").unwrap();

    let head_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();

    // Test WITHOUT ignore - counts all files (1501 lines)
    let stats_all = stats_for_commit_stats(&gitai_repo, &head_sha, &[]).unwrap();
    assert_eq!(stats_all.git_diff_added_lines, 1501);

    // Test WITH ignore - only counts README (1 line)
    let ignore_patterns = vec![
        "Cargo.lock".to_string(),
        "package-lock.json".to_string(),
        "yarn.lock".to_string(),
    ];
    let stats_filtered = stats_for_commit_stats(&gitai_repo, &head_sha, &ignore_patterns).unwrap();
    assert_eq!(stats_filtered.git_diff_added_lines, 1);
    // KnownHuman checkpoints record h_<hash> attributions, so the README line is human_additions.
    assert_eq!(stats_filtered.human_additions, 1);
    assert_eq!(stats_filtered.unknown_additions, 0);
}

#[test]
fn test_stats_with_lockfile_only_commit() {
    let repo = TestRepo::new();

    // Initial commit
    std::fs::create_dir_all(repo.path().join("src")).unwrap();
    std::fs::write(repo.path().join("src/lib.rs"), "pub fn foo() {}\n").unwrap();
    repo.git(&["add", "src/lib.rs"]).unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "src/lib.rs"])
        .unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Commit that ONLY updates lockfiles (common during dependency updates)
    std::fs::write(repo.path().join("Cargo.lock"), "# updated\n".repeat(2000)).unwrap();
    repo.git(&["add", "Cargo.lock"]).unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "Cargo.lock"])
        .unwrap();
    repo.stage_all_and_commit("Update dependencies").unwrap();

    let head_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();

    // Test WITHOUT ignore - shows 2000 lines
    let stats_with = stats_for_commit_stats(&gitai_repo, &head_sha, &[]).unwrap();
    assert_eq!(stats_with.git_diff_added_lines, 2000);

    // Test WITH ignore - shows 0 lines (lockfile-only commit)
    let ignore_patterns = vec!["Cargo.lock".to_string()];
    let stats_without = stats_for_commit_stats(&gitai_repo, &head_sha, &ignore_patterns).unwrap();
    assert_eq!(stats_without.git_diff_added_lines, 0);
    assert_eq!(stats_without.ai_additions, 0);
    assert_eq!(stats_without.human_additions, 0);
}

#[test]
fn test_stats_empty_ignore_patterns() {
    let repo = TestRepo::new();

    // Initial commit
    std::fs::write(repo.path().join("test.txt"), "Line1\n").unwrap();
    repo.git(&["add", "test.txt"]).unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "test.txt"])
        .unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Add lines
    std::fs::write(repo.path().join("test.txt"), "Line1\nLine2\nLine3\n").unwrap();
    repo.git(&["add", "test.txt"]).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "test.txt"]).unwrap();
    repo.stage_all_and_commit("Add lines").unwrap();

    let head_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();

    // Test with empty patterns - should behave same as no filtering
    let stats = stats_for_commit_stats(&gitai_repo, &head_sha, &[]).unwrap();
    assert_eq!(stats.git_diff_added_lines, 2);
    assert_eq!(stats.ai_additions, 2);
}

#[test]
fn test_stats_with_glob_patterns() {
    let repo = TestRepo::new();

    // Initial commit
    std::fs::create_dir_all(repo.path().join("src")).unwrap();
    std::fs::write(repo.path().join("src/lib.rs"), "pub fn foo() {}\n").unwrap();
    repo.git(&["add", "src/lib.rs"]).unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "src/lib.rs"])
        .unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Commit with source code + lockfiles + generated files
    std::fs::write(
        repo.path().join("src/lib.rs"),
        "pub fn foo() {}\npub fn bar() {}\n",
    )
    .unwrap();
    repo.git(&["add", "src/lib.rs"]).unwrap();
    std::fs::write(repo.path().join("Cargo.lock"), "# lock\n".repeat(1000)).unwrap();
    repo.git(&["add", "Cargo.lock"]).unwrap();
    std::fs::write(repo.path().join("package-lock.json"), "{}\n".repeat(500)).unwrap();
    repo.git(&["add", "package-lock.json"]).unwrap();
    std::fs::write(
        repo.path().join("api.generated.ts"),
        "// generated\n".repeat(300),
    )
    .unwrap();
    repo.git(&["add", "api.generated.ts"]).unwrap();
    std::fs::write(
        repo.path().join("schema.generated.js"),
        "// schema\n".repeat(200),
    )
    .unwrap();
    repo.git(&["add", "schema.generated.js"]).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "src/lib.rs"])
        .unwrap();
    repo.stage_all_and_commit("Add code").unwrap();

    let head_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();

    // Test WITHOUT ignore - all files included (2001 lines)
    let stats_all = stats_for_commit_stats(&gitai_repo, &head_sha, &[]).unwrap();
    assert_eq!(stats_all.git_diff_added_lines, 2001);

    // Test WITH glob patterns - only source code (1 line)
    let glob_patterns = vec![
        "*.lock".to_string(),        // Matches Cargo.lock
        "*lock.json".to_string(),    // Matches package-lock.json
        "*.generated.*".to_string(), // Matches *.generated.ts, *.generated.js
    ];
    let stats_filtered = stats_for_commit_stats(&gitai_repo, &head_sha, &glob_patterns).unwrap();
    assert_eq!(stats_filtered.git_diff_added_lines, 1);
    assert_eq!(stats_filtered.ai_additions, 1);

    // Glob-ignored files add 0 chars/bytes as well as 0 lines. Only the
    // AI-attributed source line keeps volume: "pub fn bar() {}" is 15 ASCII
    // scalars/bytes + 1 synthetic LF = 16.
    assert_eq!(stats_filtered.git_diff_added_chars, 16);
    assert_eq!(stats_filtered.git_diff_added_bytes, 16);
    assert_eq!(stats_filtered.ai_additions_chars, 16);
    assert_eq!(stats_filtered.ai_additions_bytes, 16);
    assert_eq!(stats_filtered.ai_accepted_chars, 16);
    assert_eq!(stats_filtered.ai_accepted_bytes, 16);
    assert_eq!(stats_filtered.human_additions_chars, 0);
    assert_eq!(stats_filtered.unknown_additions_chars, 0);
}

#[test]
fn test_accepted_lines_no_authorship_log() {
    let added_lines: HashMap<String, Vec<u32>> = HashMap::new();
    let (accepted, known_human, per_tool) =
        accepted_lines_from_attestations(None, &added_lines, false);
    assert_eq!(accepted, 0);
    assert_eq!(known_human, 0);
    assert!(per_tool.is_empty());
}

#[test]
fn test_accepted_lines_merge_commit() {
    // Even with a real authorship log, merge commits should short-circuit to (0, empty)
    let mut log = AuthorshipLog::new();
    let agent_id = AgentId {
        tool: "cursor".to_string(),
        id: "session_1".to_string(),
        model: "claude-3-sonnet".to_string(),
    };
    let hash = generate_short_hash(&agent_id.id, &agent_id.tool);
    log.metadata.prompts.insert(
        hash.clone(),
        PromptRecord {
            agent_id,
            human_author: None,
            total_additions: 5,
            total_deletions: 0,
            accepted_lines: 5,
            overriden_lines: 0,
            custom_attributes: None,
            messages_url: None,
        },
    );

    let mut file_att = FileAttestation::new("foo.rs".to_string());
    file_att.add_entry(AttestationEntry::new(hash, vec![LineRange::Range(1, 3)]));
    log.attestations.push(file_att);

    let mut added_lines: HashMap<String, Vec<u32>> = HashMap::new();
    added_lines.insert("foo.rs".to_string(), vec![1, 2, 3]);

    let (accepted, known_human, per_tool) =
        accepted_lines_from_attestations(Some(&log), &added_lines, true);
    assert_eq!(accepted, 0);
    assert_eq!(known_human, 0);
    assert!(per_tool.is_empty());
}

#[test]
fn test_accepted_lines_no_matching_files() {
    let mut log = AuthorshipLog::new();
    let agent_id = AgentId {
        tool: "cursor".to_string(),
        id: "session_2".to_string(),
        model: "claude-3-sonnet".to_string(),
    };
    let hash = generate_short_hash(&agent_id.id, &agent_id.tool);
    log.metadata.prompts.insert(
        hash.clone(),
        PromptRecord {
            agent_id,
            human_author: None,
            total_additions: 3,
            total_deletions: 0,
            accepted_lines: 3,
            overriden_lines: 0,
            custom_attributes: None,
            messages_url: None,
        },
    );

    let mut file_att = FileAttestation::new("foo.rs".to_string());
    file_att.add_entry(AttestationEntry::new(hash, vec![LineRange::Range(1, 3)]));
    log.attestations.push(file_att);

    // added_lines has "bar.rs" but NOT "foo.rs"
    let mut added_lines: HashMap<String, Vec<u32>> = HashMap::new();
    added_lines.insert("bar.rs".to_string(), vec![1, 2, 3]);

    let (accepted, known_human, per_tool) =
        accepted_lines_from_attestations(Some(&log), &added_lines, false);
    assert_eq!(accepted, 0);
    assert_eq!(known_human, 0);
    assert!(per_tool.is_empty());
}

#[test]
fn test_accepted_lines_basic_match() {
    let mut log = AuthorshipLog::new();
    let agent_id = AgentId {
        tool: "cursor".to_string(),
        id: "session_3".to_string(),
        model: "claude-3-sonnet".to_string(),
    };
    let hash = generate_short_hash(&agent_id.id, &agent_id.tool);
    log.metadata.prompts.insert(
        hash.clone(),
        PromptRecord {
            agent_id,
            human_author: None,
            total_additions: 3,
            total_deletions: 0,
            accepted_lines: 3,
            overriden_lines: 0,
            custom_attributes: None,
            messages_url: None,
        },
    );

    let mut file_att = FileAttestation::new("foo.rs".to_string());
    file_att.add_entry(AttestationEntry::new(
        hash.clone(),
        vec![LineRange::Range(1, 3)],
    ));
    log.attestations.push(file_att);

    let mut added_lines: HashMap<String, Vec<u32>> = HashMap::new();
    added_lines.insert("foo.rs".to_string(), vec![1, 2, 3]);

    let (accepted, known_human, per_tool) =
        accepted_lines_from_attestations(Some(&log), &added_lines, false);
    assert_eq!(accepted, 3);
    assert_eq!(known_human, 0);

    // Verify per-tool breakdown contains the right key
    let expected_key = "cursor::claude-3-sonnet".to_string();
    assert_eq!(per_tool.get(&expected_key), Some(&3));
}

// --- line_range_overlap_len tests ---

#[test]
fn test_overlap_single_hit() {
    let count = line_range_overlap_len(&LineRange::Single(5), &[3, 5, 7]);
    assert_eq!(count, 1);
}

#[test]
fn test_overlap_single_miss() {
    let count = line_range_overlap_len(&LineRange::Single(4), &[3, 5, 7]);
    assert_eq!(count, 0);
}

#[test]
fn test_overlap_range_full() {
    let count = line_range_overlap_len(&LineRange::Range(3, 7), &[3, 4, 5, 6, 7]);
    assert_eq!(count, 5);
}

#[test]
fn test_overlap_range_partial() {
    // Range [4, 8] intersected with [3, 5, 7, 9]: only 5 and 7 are in range
    let count = line_range_overlap_len(&LineRange::Range(4, 8), &[3, 5, 7, 9]);
    assert_eq!(count, 2);
}

#[test]
fn test_overlap_range_miss() {
    let count = line_range_overlap_len(&LineRange::Range(10, 20), &[1, 2, 3]);
    assert_eq!(count, 0);
}

#[test]
fn test_overlap_range_empty_added() {
    let count = line_range_overlap_len(&LineRange::Range(1, 10), &[]);
    assert_eq!(count, 0);
}

#[test]
fn test_stats_for_merge_commit_skips_ai_acceptance() {
    let repo = TestRepo::new();

    std::fs::write(repo.path().join("test.txt"), "base\n").unwrap();
    repo.git(&["add", "test.txt"]).unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "test.txt"])
        .unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();

    let default_branch = repo.current_branch();
    repo.git(&["checkout", "-b", "feature"]).unwrap();
    std::fs::write(repo.path().join("test.txt"), "base\nfeature line\n").unwrap();
    repo.git(&["add", "test.txt"]).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "test.txt"]).unwrap();
    repo.stage_all_and_commit("Feature change").unwrap();

    repo.git(&["checkout", &default_branch]).unwrap();
    std::fs::write(repo.path().join("main.txt"), "main line\n").unwrap();
    repo.git(&["add", "main.txt"]).unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "main.txt"])
        .unwrap();
    repo.stage_all_and_commit("Main change").unwrap();

    repo.git(&["merge", "feature", "-m", "Merge feature"])
        .unwrap();

    let merge_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let stats = stats_for_commit_stats(&gitai_repo, &merge_sha, &[]).unwrap();

    assert_eq!(stats.ai_accepted, 0);
    assert_eq!(stats.ai_additions, 0);

    // The merge hunk skip also yields 0 AI char/byte volume.
    assert_eq!(stats.ai_additions_chars, 0);
    assert_eq!(stats.ai_additions_bytes, 0);
    assert_eq!(stats.ai_accepted_chars, 0);
    assert_eq!(stats.ai_accepted_bytes, 0);
}

#[test]
fn test_stats_command_nonexistent_commit() {
    let repo = TestRepo::new();

    std::fs::write(repo.path().join("test.txt"), "content\n").unwrap();
    repo.git(&["add", "test.txt"]).unwrap();
    repo.stage_all_and_commit("Commit").unwrap();

    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();

    // Non-existent SHA should error
    let result = stats_command(
        &gitai_repo,
        Some("0000000000000000000000000000000000000000"),
        false,
        &[],
    );
    assert!(result.is_err());
}

#[test]
fn test_stats_command_with_json_output() {
    let repo = TestRepo::new();

    std::fs::write(repo.path().join("test.txt"), "content\n").unwrap();
    repo.git(&["add", "test.txt"]).unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "test.txt"])
        .unwrap();
    repo.stage_all_and_commit("Commit").unwrap();

    let head_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();

    // Should succeed with json output
    let result = stats_command(&gitai_repo, Some(&head_sha), true, &[]);
    assert!(result.is_ok());
}

#[test]
fn test_stats_command_default_to_head() {
    let repo = TestRepo::new();

    std::fs::write(repo.path().join("test.txt"), "content\n").unwrap();
    repo.git(&["add", "test.txt"]).unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "test.txt"])
        .unwrap();
    repo.stage_all_and_commit("Commit").unwrap();

    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();

    // No SHA provided should default to HEAD
    let result = stats_command(&gitai_repo, None, false, &[]);
    assert!(result.is_ok());
}

#[test]
fn test_get_git_diff_stats_binary_files() {
    let repo = TestRepo::new();

    // Create initial commit
    std::fs::write(repo.path().join("text.txt"), "text\n").unwrap();
    repo.git(&["add", "text.txt"]).unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "text.txt"])
        .unwrap();
    repo.stage_all_and_commit("Initial").unwrap();

    // Add binary file (git will detect it as binary if it contains null bytes)
    let binary_content = vec![0u8, 1u8, 2u8, 3u8, 255u8];
    std::fs::write(repo.path().join("binary.bin"), &binary_content).unwrap();
    repo.git(&["add", "binary.bin"]).unwrap();

    repo.stage_all_and_commit("Add binary").unwrap();

    let head_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();

    // Binary files should be handled (shown as "-" in numstat)
    let result = get_git_diff_stats(&gitai_repo, &head_sha, &[]);
    assert!(result.is_ok());

    // Binary hunks carry no line contents, so every git_diff char/byte field
    // stays 0.
    let stats = stats_for_commit_stats(&gitai_repo, &head_sha, &[]).unwrap();
    assert_eq!(stats.git_diff_added_lines, 0);
    assert_eq!(stats.git_diff_deleted_lines, 0);
    assert_eq!(
        stats.git_diff_added_chars, 0,
        "binary hunk has no added contents"
    );
    assert_eq!(
        stats.git_diff_added_bytes, 0,
        "binary hunk has no added contents"
    );
    assert_eq!(stats.git_diff_deleted_chars, 0);
    assert_eq!(stats.git_diff_deleted_bytes, 0);
}

/// Deletion-only commits fill the deleted git_diff char/byte fields from hunk
/// text (payload + synthetic LF) while the added fields stay at 0.
#[test]
fn test_stats_for_commit_stats_deletion_only_volume() {
    let repo = TestRepo::new();

    std::fs::write(repo.path().join("test.txt"), "keep\ndelete me\n").unwrap();
    repo.git(&["add", "test.txt"]).unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "test.txt"])
        .unwrap();
    repo.stage_all_and_commit("Initial").unwrap();

    // Deletion-only edit: remove the second line.
    std::fs::write(repo.path().join("test.txt"), "keep\n").unwrap();
    repo.git(&["add", "test.txt"]).unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "test.txt"])
        .unwrap();
    repo.stage_all_and_commit("Delete a line").unwrap();

    let head_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let stats = stats_for_commit_stats(&gitai_repo, &head_sha, &[]).unwrap();

    assert_eq!(stats.git_diff_added_lines, 0);
    assert_eq!(stats.git_diff_deleted_lines, 1);
    // "delete me" is 9 ASCII scalars/bytes; + 1 synthetic LF = 10 / 10.
    assert_eq!(stats.git_diff_deleted_chars, 10);
    assert_eq!(stats.git_diff_deleted_bytes, 10);
    assert_eq!(stats.git_diff_added_chars, 0);
    assert_eq!(stats.git_diff_added_bytes, 0);
    assert_eq!(stats.human_additions_chars, 0);
    assert_eq!(stats.human_additions_bytes, 0);
    assert_eq!(stats.ai_additions_chars, 0);
    assert_eq!(stats.ai_additions_bytes, 0);
}

/// Added git_diff char/byte fields come from hunk text (payload +
/// synthetic LF per counted line), never from line counts alone.
#[test]
fn test_stats_for_commit_stats_added_volume() {
    let repo = TestRepo::new();

    std::fs::write(repo.path().join("test.txt"), "base\n").unwrap();
    repo.git(&["add", "test.txt"]).unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "test.txt"])
        .unwrap();
    repo.stage_all_and_commit("Initial").unwrap();

    // AI adds one ASCII line and one emoji line.
    std::fs::write(repo.path().join("test.txt"), "base\nhello\n\u{1F389}\n").unwrap();
    repo.git(&["add", "test.txt"]).unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "test.txt"]).unwrap();
    repo.stage_all_and_commit("AI adds lines").unwrap();

    let head_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let stats = stats_for_commit_stats(&gitai_repo, &head_sha, &[]).unwrap();

    assert_eq!(stats.git_diff_added_lines, 2);
    // "hello" = 5 scalars/bytes, "\u{1F389}" = 1 scalar / 4 bytes; each line
    // also counts one synthetic LF: chars = (5+1) + (1+1) = 8,
    // bytes = (5+1) + (4+1) = 11.
    assert_eq!(stats.git_diff_added_chars, 8);
    assert_eq!(stats.git_diff_added_bytes, 11);
    // No deletions in this commit.
    assert_eq!(stats.git_diff_deleted_chars, 0);
    assert_eq!(stats.git_diff_deleted_bytes, 0);
    // Both added lines are AI-attested, so the AI bucket fields equal the
    // git-diff added volume and ai_accepted_* mirrors ai_additions_*.
    assert_eq!(stats.human_additions_chars, 0);
    assert_eq!(stats.human_additions_bytes, 0);
    assert_eq!(stats.unknown_additions_chars, 0);
    assert_eq!(stats.unknown_additions_bytes, 0);
    assert_eq!(stats.ai_additions_chars, 8);
    assert_eq!(stats.ai_additions_bytes, 11);
    assert_eq!(stats.ai_accepted_chars, 8);
    assert_eq!(stats.ai_accepted_bytes, 11);
}

#[test]
fn test_stats_from_authorship_log_no_log() {
    let stats = stats_from_authorship_log(None, 10, 5, 3, 0, &BTreeMap::new());

    assert_eq!(stats.git_diff_added_lines, 10);
    assert_eq!(stats.git_diff_deleted_lines, 5);
    assert_eq!(stats.ai_accepted, 3);
    assert_eq!(stats.ai_additions, 3); // ai_accepted when no mixed
    assert_eq!(stats.human_additions, 0); // no known-human attestations passed
    assert_eq!(stats.unknown_additions, 7); // 10 - 3 (unattested lines)
}

#[test]
fn test_stats_from_authorship_log_mixed_cap() {
    // Test that mixed_additions is capped to remaining added lines
    let mut log = AuthorshipLog::new();
    let agent_id = AgentId {
        tool: "cursor".to_string(),
        id: "session".to_string(),
        model: "claude-3-sonnet".to_string(),
    };
    let hash = generate_short_hash(&agent_id.id, &agent_id.tool);

    // Prompt with 100 overridden lines (way more than the diff)
    log.metadata.prompts.insert(
        hash,
        PromptRecord {
            agent_id,
            human_author: None,
            total_additions: 50,
            total_deletions: 0,
            accepted_lines: 0,
            overriden_lines: 100, // Unrealistically high
            custom_attributes: None,
            messages_url: None,
        },
    );

    // Only 10 lines added, 5 accepted by AI
    let stats = stats_from_authorship_log(Some(&log), 10, 0, 5, 0, &BTreeMap::new());

    assert_eq!(stats.ai_additions, 5); // ai_accepted
    assert_eq!(stats.human_additions, 0); // no known-human attestations passed
}

#[test]
fn test_line_range_overlap_edge_cases() {
    // Empty added_lines
    assert_eq!(line_range_overlap_len(&LineRange::Single(5), &[]), 0);
    assert_eq!(line_range_overlap_len(&LineRange::Range(1, 10), &[]), 0);

    // Range with start == end
    assert_eq!(line_range_overlap_len(&LineRange::Range(5, 5), &[5]), 1);
    assert_eq!(line_range_overlap_len(&LineRange::Range(5, 5), &[4, 6]), 0);

    // Range before all lines
    assert_eq!(
        line_range_overlap_len(&LineRange::Range(1, 2), &[10, 20, 30]),
        0
    );

    // Range after all lines
    assert_eq!(
        line_range_overlap_len(&LineRange::Range(50, 60), &[10, 20, 30]),
        0
    );

    // Range partially overlapping
    assert_eq!(
        line_range_overlap_len(&LineRange::Range(5, 15), &[1, 3, 10, 12, 20]),
        2
    );
}

/// Historic (line-only) `CommitStats` JSON must deserialize with every
/// additive `*_chars` / `*_bytes` field defaulting to 0.
#[test]
fn test_commit_stats_deserializes_historic_line_only_json() {
    let historic_json = r#"{
        "human_additions": 3,
        "unknown_additions": 1,
        "ai_additions": 7,
        "ai_accepted": 5,
        "git_diff_deleted_lines": 2,
        "git_diff_added_lines": 11,
        "tool_model_breakdown": {
            "cursor::claude-3.5-sonnet": {
                "ai_additions": 7,
                "ai_accepted": 5
            }
        }
    }"#;

    let stats: CommitStats = serde_json::from_str(historic_json).unwrap();

    // Line fields round-trip unchanged.
    assert_eq!(stats.human_additions, 3);
    assert_eq!(stats.unknown_additions, 1);
    assert_eq!(stats.ai_additions, 7);
    assert_eq!(stats.ai_accepted, 5);
    assert_eq!(stats.git_diff_deleted_lines, 2);
    assert_eq!(stats.git_diff_added_lines, 11);

    // Every additive char/byte field defaults to 0 when the keys are absent.
    assert_eq!(stats.human_additions_chars, 0);
    assert_eq!(stats.human_additions_bytes, 0);
    assert_eq!(stats.unknown_additions_chars, 0);
    assert_eq!(stats.unknown_additions_bytes, 0);
    assert_eq!(stats.ai_additions_chars, 0);
    assert_eq!(stats.ai_additions_bytes, 0);
    assert_eq!(stats.ai_accepted_chars, 0);
    assert_eq!(stats.ai_accepted_bytes, 0);
    assert_eq!(stats.git_diff_added_chars, 0);
    assert_eq!(stats.git_diff_added_bytes, 0);
    assert_eq!(stats.git_diff_deleted_chars, 0);
    assert_eq!(stats.git_diff_deleted_bytes, 0);

    // Same guarantee for nested tool/model headline stats.
    let tool_stats = stats
        .tool_model_breakdown
        .get("cursor::claude-3.5-sonnet")
        .unwrap();
    assert_eq!(tool_stats.ai_additions, 7);
    assert_eq!(tool_stats.ai_accepted, 5);
    assert_eq!(tool_stats.ai_additions_chars, 0);
    assert_eq!(tool_stats.ai_additions_bytes, 0);
    assert_eq!(tool_stats.ai_accepted_chars, 0);
    assert_eq!(tool_stats.ai_accepted_bytes, 0);

    // The new keys are always serialized, even when zero (no skip_serializing_if).
    let serialized = serde_json::to_string(&stats).unwrap();
    for key in [
        "human_additions_chars",
        "human_additions_bytes",
        "unknown_additions_chars",
        "unknown_additions_bytes",
        "ai_additions_chars",
        "ai_additions_bytes",
        "ai_accepted_chars",
        "ai_accepted_bytes",
        "git_diff_added_chars",
        "git_diff_added_bytes",
        "git_diff_deleted_chars",
        "git_diff_deleted_bytes",
    ] {
        assert!(
            serialized.contains(&format!("\"{}\":0", key)),
            "expected serialized CommitStats to always include `{}` as 0, got: {}",
            key,
            serialized
        );
    }
}

#[test]
fn test_count_line_payload_volume_ascii_plus_synthetic_lf() {
    // Each counted payload contributes its scalar/byte length plus one synthetic
    // LF (U+000A): "hello" is 5 chars / 5 bytes, so volume is 6 / 6 with LF.
    let payloads = vec!["hello".to_string()];
    let (chars, bytes) = count_line_payload_volume(&payloads);
    assert_eq!(chars, 6, "5 ASCII scalars + 1 synthetic LF");
    assert_eq!(bytes, 6, "5 ASCII bytes + 1 synthetic LF");
}

#[test]
fn test_count_line_payload_volume_emoji() {
    // "🎉" is 1 Unicode scalar and 4 UTF-8 bytes; with the synthetic LF it
    // counts as 2 chars / 5 bytes.
    let payloads = vec!["🎉".to_string()];
    let (chars, bytes) = count_line_payload_volume(&payloads);
    assert_eq!(chars, 2, "1 scalar + 1 synthetic LF");
    assert_eq!(bytes, 5, "4 UTF-8 bytes + 1 synthetic LF");
}

#[test]
fn test_count_line_payload_volume_empty_payload_still_counts_lf() {
    // An empty line payload contributes no content but still counts its
    // synthetic LF (1 char / 1 byte).
    let payloads = vec![String::new()];
    let (chars, bytes) = count_line_payload_volume(&payloads);
    assert_eq!(chars, 1, "empty payload still counts 1 LF char");
    assert_eq!(bytes, 1, "empty payload still counts 1 LF byte");
}

#[test]
fn test_count_line_payload_volume_no_crlf_reconstruction() {
    // We never reconstruct CRLF: "line" counts a single synthetic LF (U+000A),
    // not "\r\n", so volume is 5 / 5 — not 6 / 6.
    let payloads = vec!["line".to_string()];
    let (chars, bytes) = count_line_payload_volume(&payloads);
    assert_eq!(chars, 5, "only a synthetic LF is added, never CRLF");
    assert_eq!(bytes, 5, "only a synthetic LF is added, never CRLF");

    // If a payload happens to contain a literal '\r' (e.g. a CRLF file whose
    // diff payload kept the carriage return), it is counted verbatim; we do
    // not strip it or add anything extra.
    let payloads = vec!["line\r".to_string()];
    let (chars, bytes) = count_line_payload_volume(&payloads);
    assert_eq!(chars, 6, "literal CR counted verbatim + 1 synthetic LF");
    assert_eq!(bytes, 6, "literal CR counted verbatim + 1 synthetic LF");
}

#[test]
fn test_count_line_payload_volume_multiple_payloads_sum() {
    let payloads = vec!["abc".to_string(), "🎉".to_string(), String::new()];
    let (chars, bytes) = count_line_payload_volume(&payloads);
    // (3+1) + (1+1) + (0+1) = 7 chars; (3+1) + (4+1) + (0+1) = 10 bytes
    assert_eq!(chars, 7);
    assert_eq!(bytes, 10);
}

#[test]
fn test_count_line_payload_volume_empty_slice_is_zero() {
    let (chars, bytes) = count_line_payload_volume(&[]);
    assert_eq!(chars, 0);
    assert_eq!(bytes, 0);
}

#[test]
fn test_count_line_payload_volume_always_adds_lf() {
    // DiffHunk currently carries no "no newline at end of file" marker, so the
    // helper always adds the synthetic LF for every counted payload. When/if a
    // no-newline marker lands on the hunk, the caller is responsible for
    // omitting the LF for that final payload.
    let payloads = vec!["no trailing newline in file".to_string()];
    let (chars, bytes) = count_line_payload_volume(&payloads);
    assert_eq!(chars, 28, "27 ASCII scalars + 1 synthetic LF");
    assert_eq!(bytes, 28, "27 ASCII bytes + 1 synthetic LF");
}

// --- tool_model_volume_from_attestations tests ---

fn test_log_with_prompt_entry(
    file: &str,
    tool: &str,
    model: &str,
    ranges: Vec<LineRange>,
) -> AuthorshipLog {
    let mut log = AuthorshipLog::new();
    let agent_id = AgentId {
        tool: tool.to_string(),
        id: "session_x".to_string(),
        model: model.to_string(),
    };
    let hash = generate_short_hash(&agent_id.id, &agent_id.tool);
    log.metadata.prompts.insert(
        hash.clone(),
        PromptRecord {
            agent_id,
            human_author: None,
            total_additions: 0,
            total_deletions: 0,
            accepted_lines: 0,
            overriden_lines: 0,
            custom_attributes: None,
            messages_url: None,
        },
    );
    let mut file_att = FileAttestation::new(file.to_string());
    file_att.add_entry(AttestationEntry::new(hash, ranges));
    log.attestations.push(file_att);
    log
}

#[test]
fn test_tool_model_volume_no_authorship_log() {
    let mut payloads: HashMap<String, Vec<(u32, &str)>> = HashMap::new();
    payloads.insert("foo.rs".to_string(), vec![(1, "fn a() {}")]);
    let volume = tool_model_volume_from_attestations(None, &payloads, false);
    assert!(volume.is_empty());
}

#[test]
fn test_tool_model_volume_merge_commit_is_empty() {
    let log = test_log_with_prompt_entry(
        "foo.rs",
        "cursor",
        "claude-3-sonnet",
        vec![LineRange::Range(1, 2)],
    );
    let mut payloads: HashMap<String, Vec<(u32, &str)>> = HashMap::new();
    payloads.insert(
        "foo.rs".to_string(),
        vec![(1, "fn a() {}"), (2, "fn b() {}")],
    );
    let volume = tool_model_volume_from_attestations(Some(&log), &payloads, true);
    assert!(
        volume.is_empty(),
        "merge commits skip attestation like the line headlines"
    );
}

#[test]
fn test_tool_model_volume_basic_match() {
    // Two AI lines from the same tool; volume is payload+LF per counted line.
    // "fn a() {}" is 9 ASCII scalars/bytes -> 10/10 with LF; "🎉" is 1 scalar
    // / 4 bytes -> 2/5 with LF.
    let log = test_log_with_prompt_entry(
        "foo.rs",
        "cursor",
        "claude-3-sonnet",
        vec![LineRange::Range(1, 2)],
    );
    let mut payloads: HashMap<String, Vec<(u32, &str)>> = HashMap::new();
    payloads.insert(
        "foo.rs".to_string(),
        vec![(1, "fn a() {}"), (2, "🎉"), (3, "human line")],
    );

    let volume = tool_model_volume_from_attestations(Some(&log), &payloads, false);
    assert_eq!(volume.len(), 1);
    let (chars, bytes) = volume.get("cursor::claude-3-sonnet").unwrap();
    assert_eq!(*chars, 10 + 2);
    assert_eq!(*bytes, 10 + 5);
}

#[test]
fn test_tool_model_volume_skips_known_human_and_missing_files() {
    // h_ entries never map to an AI tool; attested files absent from the diff
    // payloads contribute nothing.
    let mut log = test_log_with_prompt_entry(
        "absent.rs",
        "cursor",
        "claude-3-sonnet",
        vec![LineRange::Range(1, 2)],
    );
    let mut human_att = FileAttestation::new("foo.rs".to_string());
    human_att.add_entry(AttestationEntry::new(
        "h_deadbeef".to_string(),
        vec![LineRange::Range(1, 2)],
    ));
    log.attestations.push(human_att);

    let mut payloads: HashMap<String, Vec<(u32, &str)>> = HashMap::new();
    payloads.insert(
        "foo.rs".to_string(),
        vec![(1, "fn a() {}"), (2, "fn b() {}")],
    );

    let volume = tool_model_volume_from_attestations(Some(&log), &payloads, false);
    assert!(volume.is_empty());
}

#[test]
fn test_tool_model_volume_sums_one_tool_across_files() {
    // One tool attested in two files must sum both files' payloads, not keep
    // whichever file was folded last.
    let mut log = test_log_with_prompt_entry(
        "a.rs",
        "cursor",
        "claude-3-sonnet",
        vec![LineRange::Single(1)],
    );
    let hash = log.attestations[0].entries[0].hash.clone();
    let mut second_file = FileAttestation::new("b.rs".to_string());
    second_file.add_entry(AttestationEntry::new(hash, vec![LineRange::Range(1, 2)]));
    log.attestations.push(second_file);

    let mut payloads: HashMap<String, Vec<(u32, &str)>> = HashMap::new();
    payloads.insert("a.rs".to_string(), vec![(1, "abc")]);
    payloads.insert("b.rs".to_string(), vec![(1, "defgh"), (2, "🎉")]);

    let volume = tool_model_volume_from_attestations(Some(&log), &payloads, false);
    let (chars, bytes) = volume.get("cursor::claude-3-sonnet").unwrap();
    assert_eq!(*chars, 4 + 6 + 2);
    assert_eq!(*bytes, 4 + 6 + 5);
}

#[test]
fn test_tool_model_volume_counts_each_overlapping_entry() {
    // Two entries from the same tool claiming the same line each count it,
    // matching the per-entry sums of `accepted_lines_from_attestations`.
    let mut log = test_log_with_prompt_entry(
        "foo.rs",
        "cursor",
        "claude-3-sonnet",
        vec![LineRange::Single(1)],
    );
    let hash = log.attestations[0].entries[0].hash.clone();
    log.attestations[0].add_entry(AttestationEntry::new(hash, vec![LineRange::Single(1)]));

    let mut payloads: HashMap<String, Vec<(u32, &str)>> = HashMap::new();
    payloads.insert("foo.rs".to_string(), vec![(1, "abc")]);

    let added_lines_by_file = HashMap::from([("foo.rs".to_string(), vec![1u32])]);
    let (ai_lines, _, per_tool_lines) =
        accepted_lines_from_attestations(Some(&log), &added_lines_by_file, false);
    assert_eq!(ai_lines, 2);
    assert_eq!(per_tool_lines.get("cursor::claude-3-sonnet"), Some(&2));

    let volume = tool_model_volume_from_attestations(Some(&log), &payloads, false);
    assert_eq!(volume.get("cursor::claude-3-sonnet"), Some(&(8, 8)));
}

// --- ai_volume_from_line_tool_models tests ---

#[test]
fn test_ai_volume_from_line_tool_models_sums_blamed_ai_lines_per_tool() {
    // Only lines blamed to an AI tool count; each tool sums across files.
    let ai_tool_model_by_line: HashMap<String, HashMap<u32, String>> = HashMap::from([
        (
            "a.rs".to_string(),
            HashMap::from([
                (1, "cursor::sonnet".to_string()),
                (3, "codex::gpt".to_string()),
            ]),
        ),
        (
            "b.rs".to_string(),
            HashMap::from([(2, "cursor::sonnet".to_string())]),
        ),
        // Lines missing from the payloads (e.g. cap-skipped files) add nothing.
        (
            "capped.rs".to_string(),
            HashMap::from([(1, "cursor::sonnet".to_string())]),
        ),
    ]);
    let mut payloads: HashMap<String, Vec<(u32, &str)>> = HashMap::new();
    payloads.insert(
        "a.rs".to_string(),
        vec![(1, "abc"), (2, "human"), (3, "🎉")],
    );
    payloads.insert("b.rs".to_string(), vec![(1, "untracked"), (2, "defgh")]);

    let (ai, per_tool) = ai_volume_from_line_tool_models(&ai_tool_model_by_line, &payloads);
    assert_eq!(ai, (4 + 2 + 6, 4 + 5 + 6));
    assert_eq!(per_tool.len(), 2);
    assert_eq!(per_tool.get("cursor::sonnet"), Some(&(4 + 6, 4 + 6)));
    assert_eq!(per_tool.get("codex::gpt"), Some(&(2, 5)));
}

// --- added_bucket_volume_from_attestations tests ---

#[test]
fn test_bucket_volume_overlapping_human_and_ai_entries_mirror_line_counts() {
    // A line claimed by both a known-human and an AI entry counts in both
    // buckets, and unknown is total - ai - human (saturating), exactly as the
    // line fields are computed.
    let mut log = test_log_with_prompt_entry(
        "foo.rs",
        "cursor",
        "claude-3-sonnet",
        vec![LineRange::Range(1, 2)],
    );
    log.attestations[0].add_entry(AttestationEntry::new(
        "h_deadbeef".to_string(),
        vec![LineRange::Single(2)],
    ));

    // "abc" -> 4/4, "🎉" -> 2/5, "xy" -> 3/3 (payload + synthetic LF).
    let mut payloads: HashMap<String, Vec<(u32, &str)>> = HashMap::new();
    payloads.insert("foo.rs".to_string(), vec![(1, "abc"), (2, "🎉"), (3, "xy")]);

    let added_lines_by_file = HashMap::from([("foo.rs".to_string(), vec![1u32, 2, 3])]);
    let (ai_lines, human_lines, per_tool) =
        accepted_lines_from_attestations(Some(&log), &added_lines_by_file, false);
    let line_stats = stats_from_authorship_log(Some(&log), 3, 0, ai_lines, human_lines, &per_tool);
    assert_eq!(line_stats.ai_additions, 2);
    assert_eq!(line_stats.human_additions, 1);
    assert_eq!(line_stats.unknown_additions, 0);

    let volume = added_bucket_volume_from_attestations(Some(&log), &payloads, false);
    assert_eq!((volume.ai_chars, volume.ai_bytes), (4 + 2, 4 + 5));
    assert_eq!((volume.human_chars, volume.human_bytes), (2, 5));
    // Totals are 9 chars / 12 bytes: unknown chars = 9 - 6 - 2 = 1 and
    // unknown bytes = 12 - 9 - 5 saturates to 0.
    assert_eq!((volume.unknown_chars, volume.unknown_bytes), (1, 0));
}
