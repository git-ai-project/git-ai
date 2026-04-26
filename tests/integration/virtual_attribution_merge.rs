use std::collections::BTreeMap;

use git_ai::authorship::authorship_log::PromptRecord;
use git_ai::authorship::virtual_attribution::VirtualAttributions;
use git_ai::authorship::working_log::AgentId;

#[test]
fn test_merge_prompts_picking_newest_sums_totals_on_collision() {
    // This test verifies the old prompts format merge behavior.
    // Since mock_ai now produces sessions, we manually construct PromptRecord objects.

    let agent_id1 = AgentId {
        tool: "test_tool".to_string(),
        id: "id1".to_string(),
        model: "model1".to_string(),
    };
    let agent_id2 = AgentId {
        tool: "test_tool".to_string(),
        id: "id2".to_string(),
        model: "model2".to_string(),
    };

    let record1 = PromptRecord {
        agent_id: agent_id1.clone(),
        human_author: Some("Author 1".to_string()),
        total_additions: 2,
        total_deletions: 1,
        accepted_lines: 0,
        overriden_lines: 3,
        custom_attributes: None,
    };

    let record2 = PromptRecord {
        agent_id: agent_id2.clone(),
        human_author: Some("Author 2".to_string()),
        total_additions: 5,
        total_deletions: 4,
        accepted_lines: 0,
        overriden_lines: 7,
        custom_attributes: None,
    };

    let mut source1 = BTreeMap::new();
    source1.insert(
        "collision_hash".to_string(),
        BTreeMap::from([("commit1".to_string(), record1.clone())]),
    );

    let mut source2 = BTreeMap::new();
    source2.insert(
        "collision_hash".to_string(),
        BTreeMap::from([("commit2".to_string(), record2.clone())]),
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

crate::reuse_tests_in_worktree!(test_merge_prompts_picking_newest_sums_totals_on_collision,);
