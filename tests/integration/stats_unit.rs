use crate::repos::test_repo::TestRepo;
use git_ai::authorship::authorship_log::PromptRecord;
use git_ai::authorship::authorship_log::{LineRange, SessionRecord};
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

    // Test WITH ignore - should exclude lockfile
    let ignore_patterns = vec!["Cargo.lock".to_string()];
    let stats_without_lockfile =
        stats_for_commit_stats(&gitai_repo, &head_sha, &ignore_patterns).unwrap();
    assert_eq!(stats_without_lockfile.git_diff_added_lines, 1); // Only 1 source line
    assert_eq!(stats_without_lockfile.ai_additions, 1);
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

fn session_record(session_id: &str, model: &str) -> SessionRecord {
    SessionRecord {
        agent_id: AgentId {
            tool: "mock_ai".to_string(),
            id: session_id.to_string(),
            model: model.to_string(),
        },
        human_author: None,
        custom_attributes: None,
    }
}

fn session_log(sessions: &[(&str, &str)], entries: &[(&str, Vec<LineRange>)]) -> AuthorshipLog {
    let mut log = AuthorshipLog::new();
    for (session_id, model) in sessions {
        log.metadata
            .sessions
            .insert((*session_id).to_string(), session_record(session_id, model));
    }
    let mut file_attestation = FileAttestation::new("foo.rs".to_string());
    for (hash, ranges) in entries {
        file_attestation.add_entry(AttestationEntry::new((*hash).to_string(), ranges.clone()));
    }
    log.attestations.push(file_attestation);
    log
}

fn accepted_counts(log: &AuthorshipLog, lines: Vec<u32>) -> (u32, u32, BTreeMap<String, u32>) {
    let added_lines = HashMap::from([("foo.rs".to_string(), lines)]);
    accepted_lines_from_attestations(Some(log), &added_lines, false)
}

#[test]
fn test_accepted_lines_deduplicates_overlapping_ranges_in_one_entry() {
    let log = session_log(
        &[("s_same-entry", "test-model")],
        &[(
            "s_same-entry::t_first",
            vec![LineRange::Range(1, 4), LineRange::Range(3, 6)],
        )],
    );
    let (accepted, known_human, per_tool) = accepted_counts(&log, (1..=6).collect());

    assert_eq!(accepted, 6);
    assert_eq!(known_human, 0);
    assert_eq!(per_tool.get("mock_ai::test-model"), Some(&6));
}

#[test]
fn test_accepted_lines_deduplicates_same_model_sessions() {
    let log = session_log(
        &[
            ("s_first-session", "test-model"),
            ("s_second-session", "test-model"),
        ],
        &[
            ("s_first-session::t_first", vec![LineRange::Range(1, 6)]),
            ("s_second-session::t_second", vec![LineRange::Range(1, 6)]),
        ],
    );
    let (accepted, known_human, per_tool) = accepted_counts(&log, (1..=6).collect());

    assert_eq!(accepted, 6);
    assert_eq!(known_human, 0);
    assert_eq!(per_tool.get("mock_ai::test-model"), Some(&6));
}

#[test]
fn test_accepted_lines_preserves_distinct_model_overlap_in_breakdown() {
    let log = session_log(
        &[("s_model-a", "model-a"), ("s_model-b", "model-b")],
        &[
            ("s_model-a::t_first", vec![LineRange::Range(1, 4)]),
            ("s_model-b::t_second", vec![LineRange::Range(3, 6)]),
        ],
    );
    let (accepted, known_human, per_tool) = accepted_counts(&log, (1..=6).collect());

    assert_eq!(
        accepted, 6,
        "headline AI count uses the union of covered lines"
    );
    assert_eq!(known_human, 0);
    // Distinct models each retain their independently covered lines.
    assert_eq!(per_tool.get("mock_ai::model-a"), Some(&4));
    assert_eq!(per_tool.get("mock_ai::model-b"), Some(&4));
}

#[test]
fn test_accepted_lines_deduplicates_duplicate_file_attestations() {
    let mut log = session_log(
        &[("s_duplicate-file", "test-model")],
        &[("s_duplicate-file::t_first", vec![LineRange::Range(1, 6)])],
    );
    log.attestations.push(log.attestations[0].clone());

    let (accepted, known_human, per_tool) = accepted_counts(&log, (1..=6).collect());

    assert_eq!(accepted, 6);
    assert_eq!(known_human, 0);
    assert_eq!(per_tool.get("mock_ai::test-model"), Some(&6));
}

#[test]
fn test_accepted_lines_deduplicates_duplicate_known_human_ranges() {
    let mut log = AuthorshipLog::new();
    for _ in 0..2 {
        let mut file_attestation = FileAttestation::new("foo.rs".to_string());
        file_attestation.add_entry(AttestationEntry::new(
            "h_known-human".to_string(),
            vec![LineRange::Range(1, 6)],
        ));
        log.attestations.push(file_attestation);
    }

    let (accepted, known_human, per_tool) = accepted_counts(&log, (1..=6).collect());

    assert_eq!(accepted, 0);
    assert_eq!(known_human, 6);
    assert!(per_tool.is_empty());
}

#[test]
fn test_accepted_lines_deduplicates_legacy_prompt_and_session_for_same_model() {
    let mut log = session_log(
        &[("s_current", "test-model")],
        &[
            ("s_current::t_current", vec![LineRange::Range(1, 6)]),
            ("legacy-prompt", vec![LineRange::Range(1, 6)]),
        ],
    );
    log.metadata.prompts.insert(
        "legacy-prompt".to_string(),
        PromptRecord {
            agent_id: AgentId {
                tool: "mock_ai".to_string(),
                id: "legacy-prompt".to_string(),
                model: "test-model".to_string(),
            },
            human_author: None,
            total_additions: 0,
            total_deletions: 0,
            accepted_lines: 0,
            overriden_lines: 0,
            custom_attributes: None,
            messages_url: None,
        },
    );

    let (accepted, known_human, per_tool) = accepted_counts(&log, (1..=6).collect());

    assert_eq!(accepted, 6);
    assert_eq!(known_human, 0);
    assert_eq!(per_tool.get("mock_ai::test-model"), Some(&6));
}

#[test]
fn test_accepted_lines_without_session_metadata_still_counts_ai() {
    let log = session_log(
        &[],
        &[("s_missing-session::t_missing", vec![LineRange::Range(1, 6)])],
    );

    let (accepted, known_human, per_tool) = accepted_counts(&log, (1..=6).collect());

    assert_eq!(accepted, 6);
    assert_eq!(known_human, 0);
    assert!(per_tool.is_empty());
}

#[test]
fn test_accepted_lines_counts_only_sparse_added_lines_in_large_range() {
    let log = session_log(
        &[("s_sparse", "test-model")],
        &[(
            "s_sparse::t_large-range",
            vec![LineRange::Range(1, u32::MAX)],
        )],
    );
    let sparse_lines = vec![2, 500, u32::MAX];

    let (accepted, known_human, per_tool) = accepted_counts(&log, sparse_lines);

    assert_eq!(accepted, 3);
    assert_eq!(known_human, 0);
    assert_eq!(per_tool.get("mock_ai::test-model"), Some(&3));
}

#[test]
fn test_accepted_lines_is_invariant_to_attestation_order() {
    let sessions = [("s_first", "test-model"), ("s_second", "test-model")];
    let mut in_forward_order = session_log(
        &sessions,
        &[("s_first::t_first", vec![LineRange::Range(1, 4)])],
    );
    let mut in_reverse_order = session_log(
        &sessions,
        &[("s_second::t_second", vec![LineRange::Range(3, 6)])],
    );
    let mut first_file_record = FileAttestation::new("foo.rs".to_string());
    first_file_record.add_entry(AttestationEntry::new(
        "s_first::t_first".to_string(),
        vec![LineRange::Range(1, 4)],
    ));
    let mut second_file_record = FileAttestation::new("foo.rs".to_string());
    second_file_record.add_entry(AttestationEntry::new(
        "s_second::t_second".to_string(),
        vec![LineRange::Range(3, 6)],
    ));
    in_forward_order.attestations.push(second_file_record);
    in_reverse_order.attestations.push(first_file_record);

    let forward = accepted_counts(&in_forward_order, (1..=6).collect());
    let reverse = accepted_counts(&in_reverse_order, (1..=6).collect());

    assert_eq!(forward, reverse);
    assert_eq!(forward.0, 6);
    assert_eq!(forward.2.get("mock_ai::test-model"), Some(&6));
}

#[test]
fn test_accepted_lines_human_first_model_unions_oracle() {
    let log = session_log(
        &[("s_model-x", "model-x"), ("s_model-y", "model-y")],
        &[
            (
                "h_known-human",
                vec![
                    LineRange::Single(2),
                    LineRange::Single(3),
                    LineRange::Single(5),
                ],
            ),
            (
                "s_model-x::t_first",
                vec![
                    LineRange::Single(1),
                    LineRange::Single(2),
                    LineRange::Single(4),
                ],
            ),
            (
                "s_model-y::t_second",
                vec![LineRange::Single(3), LineRange::Single(4)],
            ),
        ],
    );

    let (accepted, known_human, per_tool) = accepted_counts(&log, (1..=6).collect());
    let unknown = 6 - accepted - known_human;

    assert_eq!(accepted, 2);
    assert_eq!(known_human, 3);
    assert_eq!(unknown, 1);
    assert_eq!(per_tool.get("mock_ai::model-x"), Some(&2));
    assert_eq!(per_tool.get("mock_ai::model-y"), Some(&1));
}

#[test]
fn test_accepted_lines_human_coverage_removes_all_overlapped_ai() {
    let log = session_log(
        &[("s_model-x", "model-x")],
        &[
            ("h_known-human", vec![LineRange::Range(1, 6)]),
            ("s_model-x::t_first", vec![LineRange::Range(1, 6)]),
        ],
    );

    let (accepted, known_human, per_tool) = accepted_counts(&log, (1..=6).collect());

    assert_eq!(accepted, 0);
    assert_eq!(known_human, 6);
    assert!(per_tool.is_empty());
}

fn mask_ranges(mask: u8, added_positions: &[u32; 4]) -> Vec<LineRange> {
    added_positions
        .iter()
        .enumerate()
        .filter(|(index, _)| mask & (1u8 << index) != 0)
        .map(|(_, line)| LineRange::Single(*line))
        .collect()
}

fn three_mask_log(
    human_mask: u8,
    model_x_mask: u8,
    model_y_mask: u8,
    added_positions: &[u32; 4],
    reverse_and_duplicate: bool,
) -> AuthorshipLog {
    let mut entries = vec![
        ("h_known-human", mask_ranges(human_mask, added_positions)),
        (
            "s_model-x::t_first",
            mask_ranges(model_x_mask, added_positions),
        ),
        (
            "s_model-y::t_second",
            mask_ranges(model_y_mask, added_positions),
        ),
    ];
    if reverse_and_duplicate {
        entries.reverse();
    }
    let mut log = session_log(
        &[("s_model-x", "model-x"), ("s_model-y", "model-y")],
        &entries,
    );
    if reverse_and_duplicate {
        log.attestations.push(log.attestations[0].clone());
    }
    log
}

#[test]
fn test_accepted_lines_matches_independent_small_mask_oracle() {
    let added_positions = [10, 20, 30, 40];
    for human_mask in 0..16u8 {
        for model_x_mask in 0..16u8 {
            for model_y_mask in 0..16u8 {
                let expected_human = human_mask.count_ones();
                let expected_ai = ((model_x_mask | model_y_mask) & !human_mask).count_ones();
                let expected_model_x = (model_x_mask & !human_mask).count_ones();
                let expected_model_y = (model_y_mask & !human_mask).count_ones();
                let expected_unknown =
                    (!(human_mask | model_x_mask | model_y_mask) & 0b1111).count_ones();

                for reverse_and_duplicate in [false, true] {
                    let log = three_mask_log(
                        human_mask,
                        model_x_mask,
                        model_y_mask,
                        &added_positions,
                        reverse_and_duplicate,
                    );
                    let (accepted, known_human, per_tool) =
                        accepted_counts(&log, added_positions.to_vec());

                    assert_eq!(accepted, expected_ai);
                    assert_eq!(known_human, expected_human);
                    assert_eq!(
                        accepted + known_human + expected_unknown,
                        4,
                        "all actual added positions remain in one headline bucket"
                    );
                    assert_eq!(
                        per_tool.get("mock_ai::model-x").copied(),
                        (expected_model_x > 0).then_some(expected_model_x)
                    );
                    assert_eq!(
                        per_tool.get("mock_ai::model-y").copied(),
                        (expected_model_y > 0).then_some(expected_model_y)
                    );
                }
            }
        }
    }
}

#[test]
fn test_accepted_lines_isolates_file_paths_and_repeated_text_positions() {
    let mut log = AuthorshipLog::new();
    for (session_id, model) in [("s_model-x", "model-x"), ("s_model-y", "model-y")] {
        log.metadata
            .sessions
            .insert(session_id.to_string(), session_record(session_id, model));
    }
    for (file_path, hash, line) in [
        ("first.txt", "s_model-x::t_first", 1),
        ("second.txt", "s_model-y::t_second", 1),
        ("repeated.txt", "s_model-x::t_repeated-first", 1),
        ("repeated.txt", "s_model-y::t_repeated-second", 2),
    ] {
        let mut file = FileAttestation::new(file_path.to_string());
        file.add_entry(AttestationEntry::new(
            hash.to_string(),
            vec![LineRange::Single(line)],
        ));
        log.attestations.push(file);
    }
    let added_lines = HashMap::from([
        ("first.txt".to_string(), vec![1]),
        ("second.txt".to_string(), vec![1]),
        ("repeated.txt".to_string(), vec![1, 2]),
    ]);

    let (accepted, known_human, per_tool) =
        accepted_lines_from_attestations(Some(&log), &added_lines, false);

    assert_eq!(accepted, 4);
    assert_eq!(known_human, 0);
    assert_eq!(per_tool.get("mock_ai::model-x"), Some(&2));
    assert_eq!(per_tool.get("mock_ai::model-y"), Some(&2));
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
