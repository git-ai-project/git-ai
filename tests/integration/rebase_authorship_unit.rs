use crate::repos::test_repo::TestRepo;
use git_ai::authorship::attribution_tracker::{
    Attribution, AttributionTracker, LineAttribution, attributions_to_line_attributions,
};
use git_ai::authorship::authorship_log::{LineRange, PromptRecord};
use git_ai::authorship::authorship_log_serialization::{
    AttestationEntry, AuthorshipLog, FileAttestation, generate_short_hash,
};
use git_ai::authorship::rebase_authorship::{
    build_file_attestation_from_line_attributions, collect_changed_file_contents_from_diff,
    diff_based_line_attribution_transfer, get_pathspecs_from_commits, load_rebase_note_cache,
    parse_cat_file_batch_output_with_oids, rewrite_authorship_after_cherry_pick,
    rewrite_authorship_after_rebase_v2, rewrite_authorship_if_needed,
    transform_attributions_to_final_state, try_fast_path_rebase_note_remap_cached,
    walk_commits_to_base,
};
use git_ai::authorship::virtual_attribution::VirtualAttributions;
use git_ai::authorship::working_log::{AgentId, Checkpoint, CheckpointKind};
use git_ai::error::GitAiError;
use git_ai::git::refs::{notes_add, show_authorship_note};
use git_ai::git::repository::find_repository_in_path;
use git_ai::git::rewrite_log::{RebaseCompleteEvent, RewriteLogEvent};
use std::collections::{HashMap, HashSet};

fn try_fast_path_rebase_note_remap(
    repo: &git_ai::git::repository::Repository,
    original_commits: &[String],
    new_commits: &[String],
    commits_to_process_lookup: &HashSet<&str>,
    tracked_paths: &[String],
) -> Result<bool, GitAiError> {
    let note_cache = load_rebase_note_cache(repo, original_commits, new_commits)?;
    try_fast_path_rebase_note_remap_cached(
        repo,
        original_commits,
        new_commits,
        commits_to_process_lookup,
        tracked_paths,
        &note_cache,
    )
}

fn write_minimal_authorship_note(
    repo: &git_ai::git::repository::Repository,
    commit_sha: &str,
    file_path: &str,
    author_id: &str,
) {
    let mut log = AuthorshipLog::new();
    log.metadata.base_commit_sha = commit_sha.to_string();
    let mut file = FileAttestation::new(file_path.to_string());
    file.add_entry(AttestationEntry::new(
        author_id.to_string(),
        vec![LineRange::Range(1, 1)],
    ));
    log.attestations.push(file);

    let note = log
        .serialize_to_string()
        .expect("serialize authorship note");
    notes_add(repo, commit_sha, &note).expect("write authorship note");
}

#[test]
fn walk_commits_to_base_linear_history_is_bounded_and_ordered() {
    let repo = TestRepo::new();
    std::fs::write(repo.path().join("f.txt"), "a\n").expect("write base");
    repo.git(&["add", "f.txt"]).expect("add");
    repo.stage_all_and_commit("base").expect("commit base");
    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let base = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    std::fs::write(repo.path().join("f.txt"), "a\nb\n").expect("write mid");
    repo.git(&["add", "f.txt"]).expect("add");
    repo.stage_all_and_commit("mid").expect("commit mid");
    let mid = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    std::fs::write(repo.path().join("f.txt"), "a\nb\nc\n").expect("write head");
    repo.git(&["add", "f.txt"]).expect("add");
    repo.stage_all_and_commit("head").expect("commit head");
    let head = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    let commits = walk_commits_to_base(&gitai_repo, &head, &base).expect("walk should succeed");

    // Newest -> oldest; callers reverse() for chronological order.
    assert_eq!(commits, vec![head, mid]);
}

#[test]
fn walk_commits_to_base_merge_history_includes_both_sides_without_full_dag_walk() {
    let repo = TestRepo::new();
    std::fs::write(repo.path().join("base.txt"), "base\n").expect("write base");
    repo.git(&["add", "base.txt"]).expect("add");
    repo.stage_all_and_commit("base").expect("commit base");
    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let base = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();
    let default_branch = repo.current_branch();

    repo.git(&["checkout", "-b", "side"])
        .expect("create side branch");
    std::fs::write(repo.path().join("side.txt"), "side\n").expect("write side");
    repo.git(&["add", "side.txt"]).expect("add");
    repo.stage_all_and_commit("side commit")
        .expect("commit side");
    let side_commit = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    repo.git(&["checkout", &default_branch])
        .expect("switch default branch");
    std::fs::write(repo.path().join("main.txt"), "main\n").expect("write main");
    repo.git(&["add", "main.txt"]).expect("add");
    repo.stage_all_and_commit("main commit")
        .expect("commit main");
    let main_commit = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    repo.git_og(&["merge", "--no-ff", "side", "-m", "merge side"])
        .expect("merge side");
    let merge_head = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    let commits =
        walk_commits_to_base(&gitai_repo, &merge_head, &base).expect("walk should succeed");

    assert_eq!(commits.first(), Some(&merge_head));
    assert_eq!(commits.len(), 3);
    assert!(commits.contains(&main_commit));
    assert!(commits.contains(&side_commit));
    assert!(!commits.contains(&base));
}

#[test]
fn walk_commits_to_base_rejects_non_ancestor_base() {
    let repo = TestRepo::new();
    std::fs::write(repo.path().join("f.txt"), "a\n").expect("write base");
    repo.git(&["add", "f.txt"]).expect("add");
    repo.stage_all_and_commit("base").expect("commit base");
    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();

    std::fs::write(repo.path().join("f.txt"), "a\nb\n").expect("write middle");
    repo.git(&["add", "f.txt"]).expect("add");
    repo.stage_all_and_commit("middle").expect("commit middle");
    let middle = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    std::fs::write(repo.path().join("f.txt"), "a\nb\nc\n").expect("write top");
    repo.git(&["add", "f.txt"]).expect("add");
    repo.stage_all_and_commit("top").expect("commit top");
    let top = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    let err = walk_commits_to_base(&gitai_repo, &middle, &top).expect_err("should fail");
    let msg = err.to_string();
    assert!(
        msg.contains("not an ancestor"),
        "unexpected error message: {}",
        msg
    );
}

#[test]
fn rewrite_authorship_after_cherry_pick_errors_on_mismatched_commit_counts() {
    let repo = TestRepo::new();
    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let err = rewrite_authorship_after_cherry_pick(
        &gitai_repo,
        &["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string()],
        &[
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string(),
            "cccccccccccccccccccccccccccccccccccccccc".to_string(),
        ],
        "human",
    )
    .expect_err("mismatched cherry-pick mapping should fail");

    assert!(
        err.to_string()
            .contains("cherry-pick rewrite commit count mismatch")
    );
}

#[test]
fn get_pathspecs_from_commits_keeps_hex_filenames() {
    let repo = TestRepo::new();
    std::fs::write(repo.path().join("base.txt"), "base\n").expect("write base file");
    repo.git(&["add", "base.txt"]).expect("add");
    repo.stage_all_and_commit("base commit")
        .expect("commit base file");
    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();

    let hex_name = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    std::fs::write(repo.path().join(hex_name), "x\n").expect("write hex file");
    repo.git(&["add", hex_name]).expect("add");
    repo.stage_all_and_commit("hex file commit")
        .expect("commit hex file");
    let commit_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    let paths = get_pathspecs_from_commits(&gitai_repo, &[commit_sha])
        .expect("collect pathspecs from commit");

    assert!(
        paths.iter().any(|p| p == hex_name),
        "hex filename should be retained in pathspecs: {:?}",
        paths
    );
}

#[test]
fn collect_changed_file_contents_from_diff_handles_add_modify_delete_and_filtering() {
    let repo = TestRepo::new();
    std::fs::write(repo.path().join("a.txt"), "a1\n").expect("write a base");
    repo.git(&["add", "a.txt"]).expect("add");
    std::fs::write(repo.path().join("c.txt"), "c1\n").expect("write c base");
    repo.git(&["add", "c.txt"]).expect("add");
    repo.stage_all_and_commit("base").expect("commit base");
    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();

    std::fs::write(repo.path().join("a.txt"), "a2\n").expect("modify a");
    repo.git(&["add", "a.txt"]).expect("add");
    std::fs::write(repo.path().join("b.txt"), "b1\n").expect("add b");
    repo.git(&["add", "b.txt"]).expect("add");
    repo.git_og(&["rm", "c.txt"]).expect("delete c");
    repo.stage_all_and_commit("rewrite")
        .expect("commit rewrite");

    let head_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();
    let head = gitai_repo.find_commit(head_sha).expect("head commit");
    let parent = head.parent(0).expect("parent commit");
    let head_tree = head.tree().expect("head tree");
    let parent_tree = parent.tree().expect("parent tree");
    let diff = gitai_repo
        .diff_tree_to_tree(Some(&parent_tree), Some(&head_tree), None, None)
        .expect("diff tree-to-tree");

    let tracked_all: HashSet<&str> = ["a.txt", "b.txt", "c.txt"].into_iter().collect();
    let (changed, contents) =
        collect_changed_file_contents_from_diff(&gitai_repo, &diff, &tracked_all)
            .expect("collect changed contents");

    assert_eq!(changed.len(), 3);
    assert!(changed.contains("a.txt"));
    assert!(changed.contains("b.txt"));
    assert!(changed.contains("c.txt"));
    assert_eq!(contents.get("a.txt").map(String::as_str), Some("a2\n"));
    assert_eq!(contents.get("b.txt").map(String::as_str), Some("b1\n"));
    assert_eq!(contents.get("c.txt").map(String::as_str), Some(""));

    let tracked_subset: HashSet<&str> = ["a.txt"].into_iter().collect();
    let (subset_changed, subset_contents) =
        collect_changed_file_contents_from_diff(&gitai_repo, &diff, &tracked_subset)
            .expect("collect subset");
    assert_eq!(subset_changed.len(), 1);
    assert!(subset_changed.contains("a.txt"));
    assert_eq!(subset_contents.len(), 1);
    assert_eq!(
        subset_contents.get("a.txt").map(String::as_str),
        Some("a2\n")
    );
}

#[test]
fn parse_cat_file_batch_output_with_oids_parses_empty_and_multiline_blobs() {
    let data = b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa blob 6\nx\ny\nz\nbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb blob 0\n\n";
    let parsed = parse_cat_file_batch_output_with_oids(data).expect("parse cat-file batch output");

    assert_eq!(
        parsed
            .get("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
            .map(String::as_str),
        Some("x\ny\nz\n")
    );
    assert_eq!(
        parsed
            .get("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
            .map(String::as_str),
        Some("")
    );
}

#[test]
fn parse_cat_file_batch_output_with_oids_errors_on_truncated_payload() {
    let truncated = b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa blob 5\nabc";
    let err = parse_cat_file_batch_output_with_oids(truncated).expect_err("should fail");
    assert!(
        err.to_string().contains("truncated"),
        "unexpected error: {}",
        err
    );
}

#[test]
fn fast_path_rebase_note_remap_copies_logs_when_tracked_blobs_match() {
    let repo = TestRepo::new();
    std::fs::write(repo.path().join("ai.txt"), "base\n").expect("write ai base");
    repo.git(&["add", "ai.txt"]).expect("add");
    repo.stage_all_and_commit("base").expect("commit base");
    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let default_branch = repo.current_branch();

    repo.git(&["checkout", "-b", "feature"])
        .expect("create feature branch");
    std::fs::write(repo.path().join("ai.txt"), "base\nfeature\n").expect("write feature ai");
    repo.git(&["add", "ai.txt"]).expect("add");
    repo.stage_all_and_commit("feature ai commit")
        .expect("commit feature ai");
    let original_commit = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();
    write_minimal_authorship_note(&gitai_repo, &original_commit, "ai.txt", "mock_ai");

    repo.git(&["checkout", &default_branch])
        .expect("switch default branch");
    std::fs::write(repo.path().join("unrelated.txt"), "main\n").expect("write unrelated");
    repo.git(&["add", "unrelated.txt"]).expect("add");
    repo.stage_all_and_commit("main unrelated")
        .expect("commit unrelated");

    repo.git_og(&["cherry-pick", &original_commit])
        .expect("cherry-pick feature commit");
    let new_commit = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    let commits_to_process_lookup: HashSet<&str> = [new_commit.as_str()].into_iter().collect();
    let did_remap = try_fast_path_rebase_note_remap(
        &gitai_repo,
        std::slice::from_ref(&original_commit),
        std::slice::from_ref(&new_commit),
        &commits_to_process_lookup,
        &["ai.txt".to_string()],
    )
    .expect("fast-path remap result");

    assert!(did_remap, "expected fast-path remap to trigger");

    let remapped_note_raw =
        show_authorship_note(&gitai_repo, &new_commit).expect("new note content");
    let remapped =
        AuthorshipLog::deserialize_from_string(&remapped_note_raw).expect("parse new note");
    assert_eq!(remapped.metadata.base_commit_sha, new_commit);
    assert_eq!(remapped.attestations.len(), 1);
    assert_eq!(remapped.attestations[0].file_path, "ai.txt");
}

#[test]
fn fast_path_rebase_note_remap_copies_multiple_commits_in_one_pass() {
    let repo = TestRepo::new();
    std::fs::write(repo.path().join("ai.txt"), "base\n").expect("write ai base");
    repo.git(&["add", "ai.txt"]).expect("add");
    repo.stage_all_and_commit("base").expect("commit base");
    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let default_branch = repo.current_branch();

    repo.git(&["checkout", "-b", "feature"])
        .expect("create feature branch");

    let mut original_commits = Vec::new();
    for idx in 1..=2 {
        std::fs::write(
            repo.path().join("ai.txt"),
            format!("base\nfeature {}\n", idx),
        )
        .expect("write feature ai");
        repo.git(&["add", "ai.txt"]).expect("add");
        repo.stage_all_and_commit(&format!("feature ai commit {}", idx))
            .expect("commit feature ai");
        let original_commit = repo
            .git_og(&["rev-parse", "HEAD"])
            .unwrap()
            .trim()
            .to_string();
        write_minimal_authorship_note(&gitai_repo, &original_commit, "ai.txt", "mock_ai");
        original_commits.push(original_commit);
    }

    repo.git(&["checkout", &default_branch])
        .expect("switch default branch");
    std::fs::write(repo.path().join("unrelated.txt"), "main\n").expect("write unrelated");
    repo.git(&["add", "unrelated.txt"]).expect("add");
    repo.stage_all_and_commit("main unrelated")
        .expect("commit unrelated");

    let mut new_commits = Vec::new();
    for original_commit in &original_commits {
        repo.git_og(&["cherry-pick", original_commit])
            .expect("cherry-pick feature commit");
        new_commits.push(
            repo.git_og(&["rev-parse", "HEAD"])
                .unwrap()
                .trim()
                .to_string(),
        );
    }

    let commits_to_process_lookup: HashSet<&str> = new_commits.iter().map(String::as_str).collect();
    let did_remap = try_fast_path_rebase_note_remap(
        &gitai_repo,
        &original_commits,
        &new_commits,
        &commits_to_process_lookup,
        &["ai.txt".to_string()],
    )
    .expect("fast-path remap result");

    assert!(did_remap, "expected fast-path remap to trigger");

    for new_commit in new_commits {
        let remapped_note_raw =
            show_authorship_note(&gitai_repo, &new_commit).expect("new note content");
        let remapped =
            AuthorshipLog::deserialize_from_string(&remapped_note_raw).expect("parse new note");
        assert_eq!(remapped.metadata.base_commit_sha, new_commit);
        assert_eq!(remapped.attestations.len(), 1);
        assert_eq!(remapped.attestations[0].file_path, "ai.txt");
    }
}

#[test]
fn fast_path_rebase_note_remap_declines_when_tracked_blobs_differ() {
    let repo = TestRepo::new();
    std::fs::write(repo.path().join("ai.txt"), "base\n").expect("write ai base");
    repo.git(&["add", "ai.txt"]).expect("add");
    repo.stage_all_and_commit("base").expect("commit base");
    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let default_branch = repo.current_branch();

    repo.git(&["checkout", "-b", "feature"])
        .expect("create feature branch");
    std::fs::write(repo.path().join("ai.txt"), "base\nfeature\n").expect("write feature ai");
    repo.git(&["add", "ai.txt"]).expect("add");
    repo.stage_all_and_commit("feature ai commit")
        .expect("commit feature ai");
    let original_commit = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();
    write_minimal_authorship_note(&gitai_repo, &original_commit, "ai.txt", "mock_ai");

    repo.git(&["checkout", &default_branch])
        .expect("switch default branch");
    std::fs::write(repo.path().join("ai.txt"), "base\nmain-only\n").expect("write divergent ai");
    repo.git(&["add", "ai.txt"]).expect("add");
    repo.stage_all_and_commit("main modifies ai")
        .expect("commit divergent ai");
    let new_commit = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    let commits_to_process_lookup: HashSet<&str> = [new_commit.as_str()].into_iter().collect();
    let did_remap = try_fast_path_rebase_note_remap(
        &gitai_repo,
        std::slice::from_ref(&original_commit),
        std::slice::from_ref(&new_commit),
        &commits_to_process_lookup,
        &["ai.txt".to_string()],
    )
    .expect("fast-path remap result");

    assert!(!did_remap, "expected fast-path remap to decline");
}

#[test]
fn transform_attributions_to_final_state_preserves_unchanged_files() {
    let repo = TestRepo::new();
    std::fs::write(repo.path().join("a.txt"), "aaa\n").expect("write a");
    repo.git(&["add", "a.txt"]).expect("add");
    std::fs::write(repo.path().join("b.txt"), "bbb\n").expect("write b");
    repo.git(&["add", "b.txt"]).expect("add");
    repo.stage_all_and_commit("base").expect("commit base");
    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let base_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    let mut attrs = HashMap::new();
    attrs.insert(
        "a.txt".to_string(),
        (
            vec![Attribution::new(0, 4, "ai-a".to_string(), 1)],
            vec![LineAttribution {
                start_line: 1,
                end_line: 1,
                author_id: "ai-a".to_string(),
                overrode: None,
            }],
        ),
    );
    attrs.insert(
        "b.txt".to_string(),
        (
            vec![Attribution::new(0, 4, "ai-b".to_string(), 1)],
            vec![LineAttribution {
                start_line: 1,
                end_line: 1,
                author_id: "ai-b".to_string(),
                overrode: None,
            }],
        ),
    );

    let mut file_contents = HashMap::new();
    file_contents.insert("a.txt".to_string(), "aaa\n".to_string());
    file_contents.insert("b.txt".to_string(), "bbb\n".to_string());

    let source_va = VirtualAttributions::new(gitai_repo.clone(), base_sha, attrs, file_contents, 1);

    let mut final_state = HashMap::new();
    final_state.insert("a.txt".to_string(), "aaa!\n".to_string());

    let transformed =
        transform_attributions_to_final_state(&source_va, final_state, None).expect("transform");

    assert_eq!(
        transformed
            .get_file_content("b.txt")
            .map(std::string::String::as_str),
        Some("bbb\n")
    );
    assert!(
        transformed.get_line_attributions("b.txt").is_some(),
        "unchanged file should retain attributions"
    );
}

#[test]
fn rebase_complete_migrates_initial_to_new_head() {
    let repo = TestRepo::new();

    std::fs::write(repo.path().join("base.txt"), "base\n").expect("write base");
    repo.git_og(&["add", "base.txt"]).expect("add");
    repo.git_og(&["commit", "-m", "base commit"])
        .expect("commit base");
    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let default_branch = repo.current_branch();

    repo.git_og(&["checkout", "-b", "feature"])
        .expect("create feature branch");
    std::fs::write(repo.path().join("feature.txt"), "feature code\n").expect("write feature");
    repo.git_og(&["add", "feature.txt"]).expect("add");
    repo.git_og(&["commit", "-m", "feature commit"])
        .expect("commit feature");
    let original_head = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    let mut initial_files = HashMap::new();
    initial_files.insert(
        "uncommitted.txt".to_string(),
        vec![LineAttribution {
            start_line: 1,
            end_line: 5,
            author_id: "ai-author-1".to_string(),
            overrode: None,
        }],
    );
    let mut prompts = HashMap::new();
    prompts.insert(
        "ai-author-1".to_string(),
        PromptRecord {
            agent_id: AgentId {
                tool: "test-tool".to_string(),
                id: "session-1".to_string(),
                model: "test-model".to_string(),
            },
            human_author: None,
            total_additions: 5,
            total_deletions: 0,
            accepted_lines: 5,
            overriden_lines: 0,
            custom_attributes: Some(HashMap::from([
                ("employee_id".to_string(), "E100".to_string()),
                ("team".to_string(), "test".to_string()),
            ])),
        },
    );

    let old_wl = gitai_repo
        .storage
        .working_log_for_base_commit(&original_head)
        .unwrap();
    old_wl
        .write_initial_attributions(initial_files.clone(), prompts.clone())
        .expect("write INITIAL");

    let old_initial = old_wl.read_initial_attributions();
    assert_eq!(
        old_initial.files.len(),
        1,
        "INITIAL should exist on old HEAD before rebase"
    );

    repo.git_og(&["checkout", &default_branch])
        .expect("switch default branch");
    std::fs::write(repo.path().join("upstream.txt"), "upstream\n").expect("write upstream");
    repo.git_og(&["add", "upstream.txt"]).expect("add");
    repo.git_og(&["commit", "-m", "upstream commit"])
        .expect("commit upstream");

    // Now simulate the rebased feature commit (same content as original_head but based on upstream)
    std::fs::write(repo.path().join("feature.txt"), "feature code\n").expect("write feature again");
    repo.git_og(&["add", "feature.txt"]).expect("add");
    repo.git_og(&["commit", "-m", "feature commit (rebased)"])
        .expect("commit rebased feature");
    let new_head = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    let rebase_event = RewriteLogEvent::RebaseComplete {
        rebase_complete: RebaseCompleteEvent::new(
            original_head.clone(),
            new_head.clone(),
            false,
            vec![original_head.clone()],
            vec![new_head.clone()],
        ),
    };

    rewrite_authorship_if_needed(
        &gitai_repo,
        &rebase_event,
        "Test User".to_string(),
        &vec![rebase_event.clone()],
        true,
    )
    .expect("rewrite_authorship_if_needed should succeed");

    let new_wl = gitai_repo
        .storage
        .working_log_for_base_commit(&new_head)
        .unwrap();
    let migrated = new_wl.read_initial_attributions();

    assert_eq!(
        migrated.files.len(),
        1,
        "INITIAL should have been migrated to new HEAD"
    );
    assert!(
        migrated.files.contains_key("uncommitted.txt"),
        "migrated INITIAL should contain the uncommitted file"
    );
    let attrs = &migrated.files["uncommitted.txt"];
    assert_eq!(attrs.len(), 1);
    assert_eq!(attrs[0].start_line, 1);
    assert_eq!(attrs[0].end_line, 5);
    assert_eq!(attrs[0].author_id, "ai-author-1");

    assert!(
        migrated.prompts.contains_key("ai-author-1"),
        "migrated INITIAL should preserve prompt records"
    );
}

#[test]
fn rebase_complete_no_initial_is_noop() {
    let repo = TestRepo::new();
    std::fs::write(repo.path().join("base.txt"), "base\n").expect("write base");
    repo.git(&["add", "base.txt"]).expect("add");
    repo.stage_all_and_commit("base commit")
        .expect("commit base");
    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let default_branch = repo.current_branch();

    repo.git(&["checkout", "-b", "feature"])
        .expect("create feature");
    std::fs::write(repo.path().join("feature.txt"), "code\n").expect("write feature");
    repo.git(&["add", "feature.txt"]).expect("add");
    repo.stage_all_and_commit("feature commit")
        .expect("commit feature");
    let original_head = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    repo.git(&["checkout", &default_branch])
        .expect("switch default branch");
    std::fs::write(repo.path().join("upstream.txt"), "upstream\n").expect("write upstream");
    repo.git(&["add", "upstream.txt"]).expect("add");
    repo.stage_all_and_commit("upstream commit")
        .expect("commit upstream");
    let new_head = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    let rebase_event = RewriteLogEvent::RebaseComplete {
        rebase_complete: RebaseCompleteEvent::new(
            original_head.clone(),
            new_head.clone(),
            false,
            vec![original_head.clone()],
            vec![new_head.clone()],
        ),
    };

    rewrite_authorship_if_needed(
        &gitai_repo,
        &rebase_event,
        "Test User".to_string(),
        &vec![rebase_event.clone()],
        true,
    )
    .expect("rewrite_authorship_if_needed should succeed with no INITIAL");

    let new_wl = gitai_repo
        .storage
        .working_log_for_base_commit(&new_head)
        .unwrap();
    let migrated = new_wl.read_initial_attributions();
    assert!(
        migrated.files.is_empty(),
        "no INITIAL should exist on new HEAD when none existed on old HEAD"
    );
}

#[test]
fn rebase_complete_migrates_multi_file_initial() {
    let repo = TestRepo::new();
    std::fs::write(repo.path().join("base.txt"), "base\n").expect("write base");
    repo.git_og(&["add", "base.txt"]).expect("add");
    repo.git_og(&["commit", "-m", "base commit"])
        .expect("commit base");
    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let default_branch = repo.current_branch();

    repo.git_og(&["checkout", "-b", "feature"])
        .expect("create feature");
    std::fs::write(repo.path().join("feature.txt"), "feature\n").expect("write feature");
    repo.git_og(&["add", "feature.txt"]).expect("add");
    repo.git_og(&["commit", "-m", "feature commit"])
        .expect("commit feature");
    let original_head = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    let mut initial_files = HashMap::new();
    initial_files.insert(
        "file_a.py".to_string(),
        vec![LineAttribution {
            start_line: 1,
            end_line: 10,
            author_id: "ai-cursor".to_string(),
            overrode: None,
        }],
    );
    initial_files.insert(
        "file_b.py".to_string(),
        vec![
            LineAttribution {
                start_line: 1,
                end_line: 3,
                author_id: "ai-cursor".to_string(),
                overrode: None,
            },
            LineAttribution {
                start_line: 7,
                end_line: 12,
                author_id: "ai-copilot".to_string(),
                overrode: None,
            },
        ],
    );

    let mut prompts = HashMap::new();
    prompts.insert(
        "ai-cursor".to_string(),
        PromptRecord {
            agent_id: AgentId {
                tool: "cursor".to_string(),
                id: "sess-1".to_string(),
                model: "gpt-4".to_string(),
            },
            human_author: None,
            total_additions: 13,
            total_deletions: 0,
            accepted_lines: 13,
            overriden_lines: 0,
            custom_attributes: Some(HashMap::from([
                ("employee_id".to_string(), "E200".to_string()),
                ("team".to_string(), "platform".to_string()),
            ])),
        },
    );
    prompts.insert(
        "ai-copilot".to_string(),
        PromptRecord {
            agent_id: AgentId {
                tool: "copilot".to_string(),
                id: "sess-2".to_string(),
                model: "gpt-4o".to_string(),
            },
            human_author: None,
            total_additions: 6,
            total_deletions: 0,
            accepted_lines: 6,
            overriden_lines: 0,
            custom_attributes: Some(HashMap::from([
                ("employee_id".to_string(), "E200".to_string()),
                ("team".to_string(), "platform".to_string()),
            ])),
        },
    );

    let old_wl = gitai_repo
        .storage
        .working_log_for_base_commit(&original_head)
        .unwrap();
    old_wl
        .write_initial_attributions(initial_files, prompts)
        .expect("write multi-file INITIAL");

    repo.git_og(&["checkout", &default_branch])
        .expect("switch default branch");
    std::fs::write(repo.path().join("upstream.txt"), "upstream\n").expect("write upstream");
    repo.git_og(&["add", "upstream.txt"]).expect("add");
    repo.git_og(&["commit", "-m", "upstream"])
        .expect("commit upstream");
    let new_head = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    let rebase_event = RewriteLogEvent::RebaseComplete {
        rebase_complete: RebaseCompleteEvent::new(
            original_head.clone(),
            new_head.clone(),
            false,
            vec![original_head.clone()],
            vec![new_head.clone()],
        ),
    };

    rewrite_authorship_if_needed(
        &gitai_repo,
        &rebase_event,
        "Test User".to_string(),
        &vec![rebase_event.clone()],
        true,
    )
    .expect("rewrite should succeed");

    let migrated = gitai_repo
        .storage
        .working_log_for_base_commit(&new_head)
        .unwrap()
        .read_initial_attributions();

    assert_eq!(migrated.files.len(), 2, "both files should be migrated");
    assert!(migrated.files.contains_key("file_a.py"));
    assert!(migrated.files.contains_key("file_b.py"));

    let b_attrs = &migrated.files["file_b.py"];
    assert_eq!(
        b_attrs.len(),
        2,
        "file_b.py should have both attribution ranges"
    );

    assert_eq!(
        migrated.prompts.len(),
        2,
        "both prompt records should be migrated"
    );
    assert!(migrated.prompts.contains_key("ai-cursor"));
    assert!(migrated.prompts.contains_key("ai-copilot"));
}

#[test]
fn rebase_complete_merges_initial_when_both_working_logs_exist() {
    let repo = TestRepo::new();
    std::fs::write(repo.path().join("base.txt"), "base\n").expect("write base");
    repo.git_og(&["add", "base.txt"]).expect("add");
    repo.git_og(&["commit", "-m", "base commit"])
        .expect("commit base");
    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let default_branch = repo.current_branch();

    repo.git_og(&["checkout", "-b", "feature"])
        .expect("create feature");
    std::fs::write(repo.path().join("feature.txt"), "feature\n").expect("write feature");
    repo.git_og(&["add", "feature.txt"]).expect("add");
    repo.git_og(&["commit", "-m", "feature commit"])
        .expect("commit feature");
    let original_head = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    let mut old_initial_files = HashMap::new();
    old_initial_files.insert(
        "old_file.txt".to_string(),
        vec![LineAttribution {
            start_line: 1,
            end_line: 3,
            author_id: "ai-old".to_string(),
            overrode: None,
        }],
    );
    let mut old_prompts = HashMap::new();
    old_prompts.insert(
        "ai-old".to_string(),
        PromptRecord {
            agent_id: AgentId {
                tool: "test-tool".to_string(),
                id: "old-session".to_string(),
                model: "test-model".to_string(),
            },
            human_author: None,
            total_additions: 3,
            total_deletions: 0,
            accepted_lines: 3,
            overriden_lines: 0,
            custom_attributes: Some(HashMap::from([
                ("employee_id".to_string(), "E300".to_string()),
                ("team".to_string(), "infra".to_string()),
            ])),
        },
    );

    let old_wl = gitai_repo
        .storage
        .working_log_for_base_commit(&original_head)
        .unwrap();
    old_wl
        .write_initial_attributions(old_initial_files, old_prompts)
        .expect("write old INITIAL");

    repo.git_og(&["checkout", &default_branch])
        .expect("switch default branch");
    std::fs::write(repo.path().join("upstream.txt"), "upstream\n").expect("write upstream");
    repo.git_og(&["add", "upstream.txt"]).expect("add");
    repo.git_og(&["commit", "-m", "upstream commit"])
        .expect("commit upstream");
    let new_head = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    let new_wl = gitai_repo
        .storage
        .working_log_for_base_commit(&new_head)
        .unwrap();
    let checkpoint = Checkpoint::new(
        CheckpointKind::AiAgent,
        "diff".to_string(),
        "new-author".to_string(),
        vec![],
    );
    new_wl
        .append_checkpoint(&checkpoint)
        .expect("write checkpoint on new HEAD");

    let rebase_event = RewriteLogEvent::RebaseComplete {
        rebase_complete: RebaseCompleteEvent::new(
            original_head.clone(),
            new_head.clone(),
            false,
            vec![original_head.clone()],
            vec![new_head.clone()],
        ),
    };

    rewrite_authorship_if_needed(
        &gitai_repo,
        &rebase_event,
        "Test User".to_string(),
        &vec![rebase_event.clone()],
        true,
    )
    .expect("rewrite should succeed when both working logs exist");

    let merged_wl = gitai_repo
        .storage
        .working_log_for_base_commit(&new_head)
        .unwrap();
    let migrated = merged_wl.read_initial_attributions();

    assert_eq!(
        migrated.files.len(),
        1,
        "INITIAL from old HEAD should be merged into new HEAD"
    );
    assert!(migrated.files.contains_key("old_file.txt"));
    assert!(migrated.prompts.contains_key("ai-old"));

    let checkpoints = merged_wl
        .read_all_checkpoints()
        .expect("read checkpoints on new HEAD");
    assert_eq!(
        checkpoints.len(),
        1,
        "checkpoint on new HEAD should be preserved"
    );
    assert_eq!(checkpoints[0].author, "new-author");

    assert!(
        !gitai_repo.storage.has_working_log(&original_head),
        "old working log should be cleaned up"
    );
}

// Test 18 is very large, I'll continue in the next part

#[test]
fn regression_initial_preserved_through_checkpoint_commit_rebase() {
    let repo = TestRepo::new();

    std::fs::write(
        repo.path().join("app.py"),
        "def main():\n    print('hello')\n",
    )
    .expect("write base app.py");
    repo.git_og(&["add", "app.py"]).expect("add");
    repo.git_og(&["commit", "-m", "initial commit"])
        .expect("initial commit");
    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let default_branch = repo.current_branch();

    repo.git_og(&["checkout", "-b", "feature"])
        .expect("create feature");
    std::fs::write(
        repo.path().join("app.py"),
        "import logging\ndef main():\n    logging.info('Starting')\n    return 42\n",
    )
    .expect("write AI app.py");
    repo.git_og(&["add", "app.py"]).expect("add");
    std::fs::write(
        repo.path().join("utils.py"),
        "def helper():\n    return 'one'\ndef helper_two():\n    return 'two'\n",
    )
    .expect("write AI utils.py");
    repo.git_og(&["add", "utils.py"]).expect("add");

    repo.git_ai(&["checkpoint", "mock_ai", "app.py", "utils.py"])
        .expect("AI checkpoint for both files");

    repo.git_og(&["commit", "-m", "AI feature work"])
        .expect("feature commit");
    let original_head = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    let mut initial_files = HashMap::new();
    initial_files.insert(
        "utils.py".to_string(),
        vec![LineAttribution {
            start_line: 1,
            end_line: 4,
            author_id: "cursor".to_string(),
            overrode: None,
        }],
    );
    let mut prompts = HashMap::new();
    prompts.insert(
        "cursor".to_string(),
        PromptRecord {
            agent_id: AgentId {
                tool: "cursor".to_string(),
                id: "session-1".to_string(),
                model: "test-model".to_string(),
            },
            human_author: None,
            total_additions: 4,
            total_deletions: 0,
            accepted_lines: 4,
            overriden_lines: 0,
            custom_attributes: Some(HashMap::from([
                ("employee_id".to_string(), "E400".to_string()),
                ("team".to_string(), "backend".to_string()),
            ])),
        },
    );
    let old_wl = gitai_repo
        .storage
        .working_log_for_base_commit(&original_head)
        .unwrap();
    old_wl
        .write_initial_attributions(initial_files, prompts)
        .expect("write INITIAL for uncommitted utils.py");

    let pre_rebase_initial = old_wl.read_initial_attributions();
    assert_eq!(
        pre_rebase_initial.files.len(),
        1,
        "INITIAL should exist before rebase"
    );

    repo.git_og(&["checkout", &default_branch])
        .expect("switch to default");
    std::fs::write(repo.path().join("README.md"), "# Test Project\n")
        .expect("write upstream README");
    repo.git_og(&["add", "README.md"]).expect("add");
    repo.git_og(&["commit", "-m", "upstream: add README"])
        .expect("upstream commit");
    let new_head = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    let rebase_event = RewriteLogEvent::RebaseComplete {
        rebase_complete: RebaseCompleteEvent::new(
            original_head.clone(),
            new_head.clone(),
            false,
            vec![original_head.clone()],
            vec![new_head.clone()],
        ),
    };

    rewrite_authorship_if_needed(
        &gitai_repo,
        &rebase_event,
        "Test User".to_string(),
        &vec![rebase_event.clone()],
        true,
    )
    .expect("rewrite should succeed");

    let new_wl = gitai_repo
        .storage
        .working_log_for_base_commit(&new_head)
        .unwrap();
    let migrated = new_wl.read_initial_attributions();

    assert_eq!(
        migrated.files.len(),
        1,
        "INITIAL should be migrated to new HEAD after rebase"
    );
    assert!(
        migrated.files.contains_key("utils.py"),
        "utils.py should be in migrated INITIAL"
    );
    let utils_attrs = &migrated.files["utils.py"];
    assert_eq!(utils_attrs.len(), 1);
    assert_eq!(utils_attrs[0].start_line, 1);
    assert_eq!(utils_attrs[0].end_line, 4);
    assert_eq!(utils_attrs[0].author_id, "cursor");

    assert!(
        migrated.prompts.contains_key("cursor"),
        "cursor prompt record should be migrated"
    );
    assert!(
        !gitai_repo.storage.has_working_log(&original_head),
        "old working log should not exist after rename"
    );
}

#[test]
fn regression_initial_survives_amend_then_rebase() {
    let repo = TestRepo::new();

    std::fs::write(repo.path().join("app.py"), "def main():\n    pass\n").expect("write base");
    repo.git_og(&["add", "app.py"]).expect("add");
    repo.git_og(&["commit", "-m", "base commit"])
        .expect("commit base");
    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();
    let default_branch = repo.current_branch();

    repo.git_og(&["checkout", "-b", "feature"])
        .expect("create feature");
    std::fs::write(
        repo.path().join("app.py"),
        "import logging\ndef main():\n    logging.info('v1')\n    return 1\n",
    )
    .expect("write feature v1");
    repo.git_og(&["add", "app.py"]).expect("add");
    repo.git_og(&["commit", "-m", "feature v1"])
        .expect("commit feature v1");
    let v1_head = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    let mut initial_files = HashMap::new();
    initial_files.insert(
        "utils.py".to_string(),
        vec![LineAttribution {
            start_line: 1,
            end_line: 8,
            author_id: "ai-cursor".to_string(),
            overrode: None,
        }],
    );
    let mut prompts = HashMap::new();
    prompts.insert(
        "ai-cursor".to_string(),
        PromptRecord {
            agent_id: AgentId {
                tool: "cursor".to_string(),
                id: "sess-amend".to_string(),
                model: "gpt-4".to_string(),
            },
            human_author: None,
            total_additions: 8,
            total_deletions: 0,
            accepted_lines: 8,
            overriden_lines: 0,
            custom_attributes: Some(HashMap::from([
                ("employee_id".to_string(), "E400".to_string()),
                ("team".to_string(), "backend".to_string()),
            ])),
        },
    );
    let v1_wl = gitai_repo
        .storage
        .working_log_for_base_commit(&v1_head)
        .unwrap();
    v1_wl
        .write_initial_attributions(initial_files.clone(), prompts.clone())
        .expect("write INITIAL on v1");

    std::fs::write(
        repo.path().join("app.py"),
        "import logging\ndef main():\n    logging.info('v2')\n    return 2\n",
    )
    .expect("write feature v2");
    repo.git_og(&["add", "app.py"]).expect("add");
    repo.git_og(&["commit", "--amend", "-m", "feature v2"])
        .expect("amend commit");
    let amend_sha = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();
    assert_ne!(v1_head, amend_sha, "amend should produce new SHA");

    let amend_event = RewriteLogEvent::RebaseComplete {
        rebase_complete: RebaseCompleteEvent::new(
            v1_head.clone(),
            amend_sha.clone(),
            false,
            vec![v1_head.clone()],
            vec![amend_sha.clone()],
        ),
    };
    rewrite_authorship_if_needed(
        &gitai_repo,
        &amend_event,
        "Test User".to_string(),
        &vec![amend_event.clone()],
        true,
    )
    .expect("amend rewrite should succeed");

    let amend_initial = gitai_repo
        .storage
        .working_log_for_base_commit(&amend_sha)
        .unwrap()
        .read_initial_attributions();
    assert_eq!(amend_initial.files.len(), 1, "INITIAL should survive amend");
    assert!(amend_initial.files.contains_key("utils.py"));

    repo.git_og(&["checkout", &default_branch])
        .expect("switch to default");
    std::fs::write(repo.path().join("upstream.txt"), "upstream change\n").expect("write upstream");
    repo.git_og(&["add", "upstream.txt"]).expect("add");
    repo.git_og(&["commit", "-m", "upstream commit"])
        .expect("commit upstream");
    let rebase_new_head = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    let rebase_event = RewriteLogEvent::RebaseComplete {
        rebase_complete: RebaseCompleteEvent::new(
            amend_sha.clone(),
            rebase_new_head.clone(),
            false,
            vec![amend_sha.clone()],
            vec![rebase_new_head.clone()],
        ),
    };
    rewrite_authorship_if_needed(
        &gitai_repo,
        &rebase_event,
        "Test User".to_string(),
        &vec![rebase_event.clone()],
        true,
    )
    .expect("rebase rewrite should succeed");

    let final_initial = gitai_repo
        .storage
        .working_log_for_base_commit(&rebase_new_head)
        .unwrap()
        .read_initial_attributions();
    assert_eq!(
        final_initial.files.len(),
        1,
        "INITIAL should survive amend + rebase"
    );
    assert!(final_initial.files.contains_key("utils.py"));
    let attrs = &final_initial.files["utils.py"];
    assert_eq!(attrs[0].start_line, 1);
    assert_eq!(attrs[0].end_line, 8);
    assert_eq!(attrs[0].author_id, "ai-cursor");
    assert!(final_initial.prompts.contains_key("ai-cursor"));
}

#[test]
fn diff_based_transfer_equal_content() {
    use git_ai::authorship::rebase_authorship::diff_based_line_attribution_transfer;

    let old = "line1\nline2\nline3\n";
    let new = "line1\nline2\nline3\n";
    let attrs = vec![
        LineAttribution {
            start_line: 1,
            end_line: 1,
            author_id: "ai-a".to_string(),
            overrode: None,
        },
        LineAttribution {
            start_line: 2,
            end_line: 2,
            author_id: "ai-b".to_string(),
            overrode: None,
        },
        LineAttribution {
            start_line: 3,
            end_line: 3,
            author_id: "ai-a".to_string(),
            overrode: None,
        },
    ];
    let result = diff_based_line_attribution_transfer(old, new, &attrs);
    assert_eq!(result.len(), 3);
    assert_eq!(result[0].author_id, "ai-a");
    assert_eq!(result[1].author_id, "ai-b");
    assert_eq!(result[2].author_id, "ai-a");
}

#[test]
fn diff_based_transfer_insertion_shifts_lines() {
    use git_ai::authorship::rebase_authorship::diff_based_line_attribution_transfer;

    let old = "line1\nline2\nline3\n";
    let new = "line1\nnew_line\nline2\nline3\n";
    let attrs = vec![
        LineAttribution {
            start_line: 1,
            end_line: 1,
            author_id: "ai-a".to_string(),
            overrode: None,
        },
        LineAttribution {
            start_line: 2,
            end_line: 2,
            author_id: "ai-b".to_string(),
            overrode: None,
        },
        LineAttribution {
            start_line: 3,
            end_line: 3,
            author_id: "ai-a".to_string(),
            overrode: None,
        },
    ];
    let result = diff_based_line_attribution_transfer(old, new, &attrs);
    // line1 kept (line 1), new_line inserted (line 2, no attr), line2 kept (line 3), line3 kept (line 4)
    assert_eq!(result.len(), 3);
    assert_eq!(result[0].start_line, 1);
    assert_eq!(result[0].author_id, "ai-a");
    assert_eq!(result[1].start_line, 3); // shifted from line 2 to line 3
    assert_eq!(result[1].author_id, "ai-b");
    assert_eq!(result[2].start_line, 4); // shifted from line 3 to line 4
    assert_eq!(result[2].author_id, "ai-a");
}

#[test]
fn diff_based_transfer_deletion_removes_line() {
    use git_ai::authorship::rebase_authorship::diff_based_line_attribution_transfer;

    let old = "line1\nline2\nline3\n";
    let new = "line1\nline3\n";
    let attrs = vec![
        LineAttribution {
            start_line: 1,
            end_line: 1,
            author_id: "ai-a".to_string(),
            overrode: None,
        },
        LineAttribution {
            start_line: 2,
            end_line: 2,
            author_id: "ai-b".to_string(),
            overrode: None,
        },
        LineAttribution {
            start_line: 3,
            end_line: 3,
            author_id: "ai-a".to_string(),
            overrode: None,
        },
    ];
    let result = diff_based_line_attribution_transfer(old, new, &attrs);
    // line1 kept (line 1), line2 deleted, line3 kept (line 2)
    assert_eq!(result.len(), 2);
    assert_eq!(result[0].start_line, 1);
    assert_eq!(result[0].author_id, "ai-a");
    assert_eq!(result[1].start_line, 2);
    assert_eq!(result[1].author_id, "ai-a");
}

#[test]
fn diff_based_transfer_replacement_drops_attribution() {
    use git_ai::authorship::rebase_authorship::diff_based_line_attribution_transfer;

    let old = "line1\nline2\nline3\n";
    let new = "line1\nmodified\nline3\n";
    let attrs = vec![
        LineAttribution {
            start_line: 1,
            end_line: 1,
            author_id: "ai-a".to_string(),
            overrode: None,
        },
        LineAttribution {
            start_line: 2,
            end_line: 2,
            author_id: "ai-b".to_string(),
            overrode: None,
        },
        LineAttribution {
            start_line: 3,
            end_line: 3,
            author_id: "ai-a".to_string(),
            overrode: None,
        },
    ];
    let result = diff_based_line_attribution_transfer(old, new, &attrs);
    // line1 kept (line 1), line2 replaced by "modified" (line 2, no attr), line3 kept (line 3)
    assert_eq!(result.len(), 2);
    assert_eq!(result[0].start_line, 1);
    assert_eq!(result[0].author_id, "ai-a");
    assert_eq!(result[1].start_line, 3);
    assert_eq!(result[1].author_id, "ai-a");
}

#[test]
fn diff_based_transfer_handles_duplicate_lines_correctly() {
    use git_ai::authorship::rebase_authorship::diff_based_line_attribution_transfer;

    // This tests the case that the old content-matching approach got wrong:
    // identical lines from different authors should be tracked by position, not content
    let old = "let x = 42;\nlet y = 0;\nlet x = 42;\n";
    let new = "let x = 42;\nlet z = 1;\nlet y = 0;\nlet x = 42;\n";
    let attrs = vec![
        LineAttribution {
            start_line: 1,
            end_line: 1,
            author_id: "ai-a".to_string(),
            overrode: None,
        },
        LineAttribution {
            start_line: 2,
            end_line: 2,
            author_id: "ai-b".to_string(),
            overrode: None,
        },
        LineAttribution {
            start_line: 3,
            end_line: 3,
            author_id: "ai-c".to_string(),
            overrode: None,
        },
    ];
    let result = diff_based_line_attribution_transfer(old, new, &attrs);
    // line "let x = 42;" (1) kept as line 1 (ai-a)
    // "let z = 1;" inserted (line 2, no attr)
    // "let y = 0;" kept (line 3, ai-b)
    // "let x = 42;" (3) kept as line 4 (ai-c) — NOT ai-a!
    assert_eq!(result.len(), 3);
    assert_eq!(result[0].start_line, 1);
    assert_eq!(result[0].author_id, "ai-a");
    assert_eq!(result[1].start_line, 3);
    assert_eq!(result[1].author_id, "ai-b");
    assert_eq!(result[2].start_line, 4);
    assert_eq!(result[2].author_id, "ai-c");
}

#[test]
fn regression_multi_tool_initial_with_disjoint_files_survives_rebase() {
    let repo = TestRepo::new();

    std::fs::write(repo.path().join("base.txt"), "base\n").expect("write base");
    repo.git_og(&["add", "base.txt"]).expect("add");
    repo.git_og(&["commit", "-m", "base commit"])
        .expect("commit base");
    let default_branch = repo.current_branch();

    repo.git_og(&["checkout", "-b", "feature"])
        .expect("create feature");
    std::fs::write(repo.path().join("committed.py"), "print('committed')\n")
        .expect("write committed");
    repo.git_og(&["add", "committed.py"]).expect("add");
    repo.git_og(&["commit", "-m", "feature commit"])
        .expect("commit feature");
    let original_head = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();

    let mut initial_files = HashMap::new();
    initial_files.insert(
        "cursor_file.py".to_string(),
        vec![LineAttribution {
            start_line: 1,
            end_line: 10,
            author_id: "ai-cursor".to_string(),
            overrode: None,
        }],
    );
    initial_files.insert(
        "copilot_file.py".to_string(),
        vec![
            LineAttribution {
                start_line: 1,
                end_line: 5,
                author_id: "ai-copilot".to_string(),
                overrode: None,
            },
            LineAttribution {
                start_line: 10,
                end_line: 15,
                author_id: "ai-copilot".to_string(),
                overrode: None,
            },
        ],
    );
    initial_files.insert(
        "shared_file.py".to_string(),
        vec![
            LineAttribution {
                start_line: 1,
                end_line: 3,
                author_id: "ai-cursor".to_string(),
                overrode: None,
            },
            LineAttribution {
                start_line: 4,
                end_line: 8,
                author_id: "ai-copilot".to_string(),
                overrode: None,
            },
        ],
    );

    let mut prompts = HashMap::new();
    prompts.insert(
        "ai-cursor".to_string(),
        PromptRecord {
            agent_id: AgentId {
                tool: "cursor".to_string(),
                id: "sess-cursor".to_string(),
                model: "gpt-4".to_string(),
            },
            human_author: None,
            total_additions: 13,
            total_deletions: 0,
            accepted_lines: 13,
            overriden_lines: 0,
            custom_attributes: Some(HashMap::from([
                ("employee_id".to_string(), "E500".to_string()),
                ("team".to_string(), "security".to_string()),
            ])),
        },
    );
    prompts.insert(
        "ai-copilot".to_string(),
        PromptRecord {
            agent_id: AgentId {
                tool: "copilot".to_string(),
                id: "sess-copilot".to_string(),
                model: "gpt-4o".to_string(),
            },
            human_author: None,
            total_additions: 16,
            total_deletions: 0,
            accepted_lines: 16,
            overriden_lines: 0,
            custom_attributes: Some(HashMap::from([
                ("employee_id".to_string(), "E500".to_string()),
                ("team".to_string(), "security".to_string()),
            ])),
        },
    );

    let old_wl = gitai_repo
        .storage
        .working_log_for_base_commit(&original_head)
        .unwrap();
    old_wl
        .write_initial_attributions(initial_files, prompts)
        .expect("write multi-tool INITIAL");

    repo.git_og(&["checkout", &default_branch])
        .expect("switch to default");
    std::fs::write(repo.path().join("upstream.txt"), "upstream\n").expect("write upstream");
    repo.git_og(&["add", "upstream.txt"]).expect("add");
    repo.git_og(&["commit", "-m", "upstream commit"])
        .expect("commit upstream");
    let new_head = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    let rebase_event = RewriteLogEvent::RebaseComplete {
        rebase_complete: RebaseCompleteEvent::new(
            original_head.clone(),
            new_head.clone(),
            false,
            vec![original_head.clone()],
            vec![new_head.clone()],
        ),
    };

    rewrite_authorship_if_needed(
        &gitai_repo,
        &rebase_event,
        "Test User".to_string(),
        &vec![rebase_event.clone()],
        true,
    )
    .expect("rewrite should succeed");

    let migrated = gitai_repo
        .storage
        .working_log_for_base_commit(&new_head)
        .unwrap()
        .read_initial_attributions();

    assert_eq!(
        migrated.files.len(),
        3,
        "all three files should be migrated"
    );
    assert!(migrated.files.contains_key("cursor_file.py"));
    assert!(migrated.files.contains_key("copilot_file.py"));
    assert!(migrated.files.contains_key("shared_file.py"));

    let copilot_attrs = &migrated.files["copilot_file.py"];
    assert_eq!(
        copilot_attrs.len(),
        2,
        "copilot_file.py should have both attribution ranges"
    );
    assert_eq!(copilot_attrs[0].start_line, 1);
    assert_eq!(copilot_attrs[0].end_line, 5);
    assert_eq!(copilot_attrs[1].start_line, 10);
    assert_eq!(copilot_attrs[1].end_line, 15);

    let shared_attrs = &migrated.files["shared_file.py"];
    assert_eq!(
        shared_attrs.len(),
        2,
        "shared_file.py should have attributions from both tools"
    );

    assert_eq!(
        migrated.prompts.len(),
        2,
        "both prompt records should be migrated"
    );
    assert!(migrated.prompts.contains_key("ai-cursor"));
    assert!(migrated.prompts.contains_key("ai-copilot"));

    let cursor_prompt = &migrated.prompts["ai-cursor"];
    assert_eq!(cursor_prompt.agent_id.tool, "cursor");
    assert_eq!(cursor_prompt.total_additions, 13);

    let copilot_prompt = &migrated.prompts["ai-copilot"];
    assert_eq!(copilot_prompt.agent_id.tool, "copilot");
    assert_eq!(copilot_prompt.total_additions, 16);
}

#[test]
fn flatten_prompts_picks_per_commit_record_for_same_session_multi_commit() {
    let repo = TestRepo::new();

    let base_content = "line1\nline2\nline3\nline4\nline5\nline6\nline7\nline8\nline9\nline10\n";
    std::fs::write(repo.path().join("feature.txt"), base_content).expect("write base feature.txt");
    repo.git_og(&["add", "feature.txt"]).expect("add");
    repo.git_og(&["commit", "-m", "base"]).expect("commit base");
    let default_branch = repo.current_branch();

    repo.git_og(&["checkout", "-b", "feature"])
        .expect("create feature branch");
    let content_a =
        "line1\nline2\nai-line3\nai-line4\nai-line5\nai-line6\nai-line7\nline8\nline9\nline10\n";
    std::fs::write(repo.path().join("feature.txt"), content_a).expect("write feature.txt A");
    repo.git_og(&["add", "feature.txt"]).expect("add");
    repo.git_og(&["commit", "-m", "commit-A"])
        .expect("commit A");
    let sha_a = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    let other_content = "ai-line1\nai-line2\nai-line3\nai-line4\nai-line5\nai-line6\nai-line7\nai-line8\nai-line9\nai-line10\n";
    std::fs::write(repo.path().join("other.txt"), other_content).expect("write other.txt B");
    repo.git_og(&["add", "other.txt"]).expect("add");
    repo.git_og(&["commit", "-m", "commit-B"])
        .expect("commit B");
    let sha_b = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    let gitai_repo = find_repository_in_path(repo.path().to_str().unwrap()).unwrap();

    let agent_id = AgentId {
        tool: "claude".to_string(),
        id: "session-flatten-test-abc".to_string(),
        model: "claude-sonnet-4".to_string(),
    };
    let prompt_hash = generate_short_hash(&agent_id.id, &agent_id.tool);

    // Note for commit A: 5 AI lines (feature.txt lines 3-7)
    {
        let mut log = AuthorshipLog::new();
        log.metadata.base_commit_sha = sha_a.clone();
        log.metadata.prompts.insert(
            prompt_hash.clone(),
            PromptRecord {
                agent_id: agent_id.clone(),
                human_author: None,
                total_additions: 5,
                total_deletions: 0,
                accepted_lines: 5,
                overriden_lines: 0,
                custom_attributes: None,
            },
        );
        let mut file = FileAttestation::new("feature.txt".to_string());
        file.add_entry(AttestationEntry::new(
            prompt_hash.clone(),
            vec![LineRange::Range(3, 7)],
        ));
        log.attestations.push(file);
        let note = log.serialize_to_string().expect("serialize note A");
        notes_add(&gitai_repo, &sha_a, &note).expect("write note A");
    }

    // Note for commit B: 10 AI lines (other.txt lines 1-10)
    {
        let mut log = AuthorshipLog::new();
        log.metadata.base_commit_sha = sha_b.clone();
        log.metadata.prompts.insert(
            prompt_hash.clone(),
            PromptRecord {
                agent_id: agent_id.clone(),
                human_author: None,
                total_additions: 10,
                total_deletions: 0,
                accepted_lines: 10,
                overriden_lines: 0,
                custom_attributes: None,
            },
        );
        let mut file = FileAttestation::new("other.txt".to_string());
        file.add_entry(AttestationEntry::new(
            prompt_hash.clone(),
            vec![LineRange::Range(1, 10)],
        ));
        log.attestations.push(file);
        let note = log.serialize_to_string().expect("serialize note B");
        notes_add(&gitai_repo, &sha_b, &note).expect("write note B");
    }

    // Main branch: prepend "header\n" to feature.txt (forces slow path)
    repo.git_og(&["checkout", &default_branch])
        .expect("switch to default branch");
    let main_content = format!("header\n{}", base_content);
    std::fs::write(repo.path().join("feature.txt"), &main_content).expect("write main feature.txt");
    repo.git_og(&["add", "feature.txt"]).expect("add");
    repo.git_og(&["commit", "-m", "main-advance"])
        .expect("commit main advance");

    // Cherry-pick A and B onto main
    repo.git_og(&["cherry-pick", &sha_a])
        .expect("cherry-pick A");
    let new_a = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    repo.git_og(&["cherry-pick", &sha_b])
        .expect("cherry-pick B");
    let new_b = repo
        .git_og(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();

    // Invoke rewrite_authorship_after_rebase_v2
    rewrite_authorship_after_rebase_v2(
        &gitai_repo,
        &sha_b,
        &[sha_a.clone(), sha_b.clone()],
        &[new_a.clone(), new_b.clone()],
        "human-tester",
    )
    .expect("rewrite authorship after rebase");

    // Verify new_A note
    {
        let note_raw = show_authorship_note(&gitai_repo, &new_a).expect("read new_A note");
        let log = AuthorshipLog::deserialize_from_string(&note_raw).expect("parse new_A note");

        let record = log
            .metadata
            .prompts
            .get(&prompt_hash)
            .expect("prompt_hash must be in new_A note metadata");
        assert_eq!(
            record.total_additions, 5,
            "new_A: total_additions should be 5 (from commit A's PromptRecord), got {}",
            record.total_additions
        );

        let file_att = log
            .attestations
            .iter()
            .find(|f| f.file_path == "feature.txt")
            .expect("new_A note must have feature.txt attestation");
        assert_eq!(
            file_att.entries.len(),
            1,
            "feature.txt should have exactly one attestation entry"
        );
        assert_eq!(file_att.entries[0].hash, prompt_hash);
        // header prepended by main shifted AI lines from 3-7 to 4-8
        assert_eq!(
            file_att.entries[0].line_ranges,
            vec![LineRange::Range(4, 8)],
            "feature.txt AI lines must shift by 1 to 4-8 after main prepended 'header\\n'; got {:?}",
            file_att.entries[0].line_ranges
        );
    }

    // Verify new_B note
    {
        let note_raw = show_authorship_note(&gitai_repo, &new_b).expect("read new_B note");
        let log = AuthorshipLog::deserialize_from_string(&note_raw).expect("parse new_B note");

        let record = log
            .metadata
            .prompts
            .get(&prompt_hash)
            .expect("prompt_hash must be in new_B note metadata");
        assert_eq!(
            record.total_additions, 10,
            "new_B: total_additions should be 10 (from commit B's PromptRecord), got {}",
            record.total_additions
        );

        let file_att = log
            .attestations
            .iter()
            .find(|f| f.file_path == "other.txt")
            .expect("new_B note must have other.txt attestation");
        assert_eq!(
            file_att.entries.len(),
            1,
            "other.txt should have exactly one attestation entry"
        );
        assert_eq!(file_att.entries[0].hash, prompt_hash);
        assert_eq!(
            file_att.entries[0].line_ranges,
            vec![LineRange::Range(1, 10)],
            "other.txt AI lines must remain at 1-10 (unchanged by rebase); got {:?}",
            file_att.entries[0].line_ranges
        );
    }
}

#[test]
#[ignore]
fn diff_based_transfer_benchmark() {
    use std::time::Instant;

    let num_files = 20;
    let lines_per_file = 200;
    let num_commits = 100;

    println!("\n=== Diff-Based vs Char-Level Transform Benchmark ===");
    println!(
        "Files: {}, Lines/file: {}, Commits: {}",
        num_files, lines_per_file, num_commits
    );

    let mut file_contents: Vec<String> = Vec::new();
    let mut line_attrs_per_file: Vec<Vec<LineAttribution>> = Vec::new();
    let mut char_attrs_per_file: Vec<Vec<Attribution>> = Vec::new();

    for file_idx in 0..num_files {
        let mut lines = Vec::new();
        let mut line_attrs = Vec::new();
        for line_idx in 0..lines_per_file {
            let content = format!("// AI code module {} line {}", file_idx, line_idx);
            let author = format!("ai-{}", line_idx % 3);
            lines.push(content);
            line_attrs.push(LineAttribution {
                start_line: (line_idx + 1) as u32,
                end_line: (line_idx + 1) as u32,
                author_id: author,
                overrode: None,
            });
        }
        let content = lines.join("\n") + "\n";

        let mut char_attrs = Vec::new();
        let mut char_pos = 0usize;
        for (line_idx, line) in content.lines().enumerate() {
            let line_end = char_pos + line.len() + 1;
            char_attrs.push(Attribution::new(
                char_pos,
                line_end,
                format!("ai-{}", line_idx % 3),
                1,
            ));
            char_pos = line_end;
        }

        file_contents.push(content);
        line_attrs_per_file.push(line_attrs);
        char_attrs_per_file.push(char_attrs);
    }

    let mut all_new_contents: Vec<Vec<String>> = Vec::new();
    let mut prev_contents = file_contents.clone();

    for commit_idx in 0..num_commits {
        let mut new_contents = Vec::new();
        for (file_idx, old_content) in prev_contents.iter().enumerate() {
            let old_lines: Vec<&str> = old_content.lines().collect();
            let mut new_lines: Vec<String> = Vec::new();
            if commit_idx == 0 {
                new_lines.push(format!("// Main header for module {}", file_idx));
                new_lines.push("// Marker".to_string());
            }
            for (line_idx, line) in old_lines.iter().enumerate() {
                if commit_idx == 0 && line_idx % 10 == 5 {
                    new_lines.push(format!("{} MODIFIED", line));
                } else {
                    new_lines.push(line.to_string());
                }
            }
            new_contents.push(new_lines.join("\n") + "\n");
        }
        all_new_contents.push(new_contents.clone());
        prev_contents = new_contents;
    }

    // Benchmark 1: Diff-based transfer
    let start = Instant::now();
    let mut current_line_attrs = line_attrs_per_file.clone();
    let mut current_contents = file_contents.clone();
    for commit_contents in &all_new_contents {
        for file_idx in 0..num_files {
            let new_content = &commit_contents[file_idx];
            let old_content = &current_contents[file_idx];
            let old_attrs = &current_line_attrs[file_idx];
            let new_attrs =
                diff_based_line_attribution_transfer(old_content, new_content, old_attrs);
            current_line_attrs[file_idx] = new_attrs;
            current_contents[file_idx] = new_content.clone();
        }
    }
    let diff_based_duration = start.elapsed();

    // Benchmark 2: Char-level transform
    let tracker = AttributionTracker::new();
    let start = Instant::now();
    let mut current_char_attrs = char_attrs_per_file.clone();
    let mut current_contents2 = file_contents.clone();
    for commit_contents in &all_new_contents {
        for file_idx in 0..num_files {
            let new_content = &commit_contents[file_idx];
            let old_content = &current_contents2[file_idx];
            let old_attrs = &current_char_attrs[file_idx];
            let new_attrs = tracker
                .update_attributions(old_content, new_content, old_attrs, "__DUMMY__", 1)
                .unwrap();
            let _line_attrs = attributions_to_line_attributions(&new_attrs, new_content);
            current_char_attrs[file_idx] = new_attrs;
            current_contents2[file_idx] = new_content.clone();
        }
    }
    let char_level_duration = start.elapsed();

    // Benchmark 3: Full old slow path
    let start = Instant::now();
    let mut full_slow_char_attrs = char_attrs_per_file.clone();
    let mut full_slow_contents = file_contents.clone();
    for commit_contents in &all_new_contents {
        let _cloned_attrs: Vec<Vec<Attribution>> = full_slow_char_attrs.clone();
        let _cloned_contents: Vec<String> = full_slow_contents.clone();
        for file_idx in 0..num_files {
            let new_content = &commit_contents[file_idx];
            let old_content = &full_slow_contents[file_idx];
            let old_attrs = &full_slow_char_attrs[file_idx];
            let new_attrs = tracker
                .update_attributions(old_content, new_content, old_attrs, "__DUMMY__", 1)
                .unwrap();
            let line_attrs = attributions_to_line_attributions(&new_attrs, new_content);
            let _serialized = build_file_attestation_from_line_attributions(
                &format!("file_{}.rs", file_idx),
                &line_attrs,
            );
            full_slow_char_attrs[file_idx] = new_attrs;
            full_slow_contents[file_idx] = new_content.clone();
        }
    }
    let full_slow_duration = start.elapsed();

    // Benchmark 4: Full new path
    let start = Instant::now();
    let mut full_fast_line_attrs = line_attrs_per_file.clone();
    let mut full_fast_contents = file_contents.clone();
    for commit_contents in &all_new_contents {
        for file_idx in 0..num_files {
            let new_content = &commit_contents[file_idx];
            let old_content = &full_fast_contents[file_idx];
            let old_attrs = &full_fast_line_attrs[file_idx];
            let new_attrs =
                diff_based_line_attribution_transfer(old_content, new_content, old_attrs);
            let _serialized = build_file_attestation_from_line_attributions(
                &format!("file_{}.rs", file_idx),
                &new_attrs,
            );
            full_fast_line_attrs[file_idx] = new_attrs;
            full_fast_contents[file_idx] = new_content.clone();
        }
    }
    let full_fast_duration = start.elapsed();

    let transform_speedup = char_level_duration.as_secs_f64() / diff_based_duration.as_secs_f64();
    let pipeline_speedup = full_slow_duration.as_secs_f64() / full_fast_duration.as_secs_f64();

    println!("\n--- Transform-Only Results ---");
    println!(
        "Diff-based transfer (new):    {:>8.1}ms",
        diff_based_duration.as_secs_f64() * 1000.0
    );
    println!(
        "Char-level transform (old):   {:>8.1}ms",
        char_level_duration.as_secs_f64() * 1000.0
    );
    println!("Transform speedup:            {:>8.1}x", transform_speedup);
    println!("\n--- Full Pipeline Results ---");
    println!(
        "New pipeline (diff + serial):  {:>8.1}ms",
        full_fast_duration.as_secs_f64() * 1000.0
    );
    println!(
        "Old pipeline (char + VA + serial): {:>5.1}ms",
        full_slow_duration.as_secs_f64() * 1000.0
    );
    println!("Full pipeline speedup:         {:>8.1}x", pipeline_speedup);

    assert!(
        pipeline_speedup >= 2.0,
        "Expected at least 2x pipeline speedup, got {:.1}x",
        pipeline_speedup
    );
}

#[test]
#[ignore]
fn diff_based_transfer_scaling() {
    use std::time::Instant;

    let num_files = 5;
    let num_commits = 10;
    let file_sizes = [50, 100, 200, 500, 1000, 2000, 5000];

    println!("\n=== Scaling Benchmark: Diff-Based vs Char-Level ===");
    println!(
        "{:>8} {:>12} {:>12} {:>8}",
        "Lines", "Diff(ms)", "CharLvl(ms)", "Speedup"
    );
    println!("{}", "-".repeat(48));

    let tracker = AttributionTracker::new();

    for &lines_per_file in &file_sizes {
        let mut file_contents = Vec::new();
        let mut line_attrs_per_file = Vec::new();
        let mut char_attrs_per_file = Vec::new();

        for file_idx in 0..num_files {
            let mut lines = Vec::new();
            let mut line_attrs = Vec::new();
            for line_idx in 0..lines_per_file {
                lines.push(format!("// AI code module {} line {}", file_idx, line_idx));
                line_attrs.push(LineAttribution {
                    start_line: (line_idx + 1) as u32,
                    end_line: (line_idx + 1) as u32,
                    author_id: format!("ai-{}", line_idx % 3),
                    overrode: None,
                });
            }
            let content = lines.join("\n") + "\n";
            let mut char_attrs = Vec::new();
            let mut pos = 0usize;
            for (li, line) in content.lines().enumerate() {
                let end = pos + line.len() + 1;
                char_attrs.push(Attribution::new(pos, end, format!("ai-{}", li % 3), 1));
                pos = end;
            }
            file_contents.push(content);
            line_attrs_per_file.push(line_attrs);
            char_attrs_per_file.push(char_attrs);
        }

        let mut all_new = Vec::new();
        let mut prev = file_contents.clone();
        for ci in 0..num_commits {
            let mut new_batch = Vec::new();
            for (fi, prev_content) in prev.iter().enumerate().take(num_files) {
                let old_lines: Vec<&str> = prev_content.lines().collect();
                let mut new_lines = Vec::new();
                if ci == 0 {
                    for h in 0..5 {
                        new_lines.push(format!("// Header {} mod {}", h, fi));
                    }
                }
                for (li, line) in old_lines.iter().enumerate() {
                    if ci == 0 && li % 10 == 5 {
                        new_lines.push(format!("{} MOD", line));
                    } else {
                        new_lines.push(line.to_string());
                    }
                }
                new_batch.push(new_lines.join("\n") + "\n");
            }
            all_new.push(new_batch.clone());
            prev = new_batch;
        }

        let start = Instant::now();
        let mut cur_la = line_attrs_per_file.clone();
        let mut cur_c = file_contents.clone();
        for commit_contents in &all_new {
            for fi in 0..num_files {
                let na = diff_based_line_attribution_transfer(
                    &cur_c[fi],
                    &commit_contents[fi],
                    &cur_la[fi],
                );
                cur_la[fi] = na;
                cur_c[fi] = commit_contents[fi].clone();
            }
        }
        let diff_ms = start.elapsed().as_secs_f64() * 1000.0;

        let start = Instant::now();
        let mut cur_ca = char_attrs_per_file.clone();
        let mut cur_c2 = file_contents.clone();
        for commit_contents in &all_new {
            for fi in 0..num_files {
                let na = tracker
                    .update_attributions(
                        &cur_c2[fi],
                        &commit_contents[fi],
                        &cur_ca[fi],
                        "__DUMMY__",
                        1,
                    )
                    .unwrap();
                let _la = attributions_to_line_attributions(&na, &commit_contents[fi]);
                cur_ca[fi] = na;
                cur_c2[fi] = commit_contents[fi].clone();
            }
        }
        let char_ms = start.elapsed().as_secs_f64() * 1000.0;

        let speedup = char_ms / diff_ms;
        println!(
            "{:>8} {:>12.1} {:>12.1} {:>8.1}x",
            lines_per_file, diff_ms, char_ms, speedup
        );
    }
    println!("===================================================\n");
}
