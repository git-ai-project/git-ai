#[macro_use]
mod repos;

use std::collections::BTreeMap;

use git_ai::authorship::virtual_attribution::VirtualAttributions;
use repos::test_file::ExpectedLineExt;
use repos::test_repo::TestRepo;

#[test]
fn test_merge_prompts_picking_newest_sums_totals_on_collision() {
    let repo = TestRepo::new();
    let mut file = repo.filename("file.txt");

    file.set_contents(lines!["AI line 1".ai()]);
    let commit1 = repo.stage_all_and_commit("commit 1").unwrap();
    let prompt1 = commit1
        .authorship_log
        .metadata
        .prompts
        .values()
        .next()
        .expect("prompt record should exist")
        .clone();

    file.set_contents(lines!["AI line 1".ai(), "AI line 2".ai()]);
    let commit2 = repo.stage_all_and_commit("commit 2").unwrap();
    let prompt2 = commit2
        .authorship_log
        .metadata
        .prompts
        .values()
        .next()
        .expect("prompt record should exist")
        .clone();

    let mut record1 = prompt1.clone();
    record1.total_additions = 2;
    record1.total_deletions = 1;
    record1.overriden_lines = 3;

    let mut record2 = prompt2.clone();
    record2.total_additions = 5;
    record2.total_deletions = 4;
    record2.overriden_lines = 7;

    let mut source1 = BTreeMap::new();
    source1.insert(
        "collision_hash".to_string(),
        BTreeMap::from([(commit1.commit_sha.clone(), record1.clone())]),
    );

    let mut source2 = BTreeMap::new();
    source2.insert(
        "collision_hash".to_string(),
        BTreeMap::from([(commit2.commit_sha.clone(), record2.clone())]),
    );

    let merged = VirtualAttributions::merge_prompts_picking_newest(&[&source1, &source2]);

    let merged_commits = merged
        .get("collision_hash")
        .expect("merged prompt should exist");
    let merged_record = merged_commits
        .values()
        .next()
        .expect("merged prompt record should exist");

    assert_eq!(
        merged_record.total_additions,
        record1.total_additions + record2.total_additions
    );
    assert_eq!(
        merged_record.total_deletions,
        record1.total_deletions + record2.total_deletions
    );
    assert_eq!(merged_record.overriden_lines, record2.overriden_lines);

    // Newest record should still win for non-accumulated fields.
    assert_eq!(merged_record.agent_id, record2.agent_id);
}

reuse_tests_in_worktree!(test_merge_prompts_picking_newest_sums_totals_on_collision,);
