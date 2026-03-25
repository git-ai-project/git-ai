use crate::repos::test_repo::TestRepo;
use git_ai::feature_flags::FeatureFlags;
use std::fs;

fn write_file(repo: &TestRepo, name: &str, contents: &str) {
    let path = repo.path().join(name);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).ok();
    }
    fs::write(path, contents).unwrap();
}

/// When cloud_default_ai_attribution is enabled and there is a previous AI checkpoint,
/// a subsequent human checkpoint should be attributed to the most recent AI agent.
#[test]
fn test_cloud_attribution_human_uses_most_recent_ai_agent() {
    let mut repo = TestRepo::new_dedicated_daemon();
    repo.set_feature_flags(FeatureFlags {
        cloud_default_ai_attribution: true,
        ..FeatureFlags::default()
    });

    // Initial commit
    write_file(&repo, "test.txt", "line 1\n");
    repo.stage_all_and_commit("initial").unwrap();

    // AI checkpoint: creates a checkpoint with agent_id (mock_ai)
    write_file(&repo, "test.txt", "line 1\nai line\n");
    repo.git_ai(&["checkpoint", "mock_ai"]).unwrap();

    // Human checkpoint with cloud attribution enabled
    write_file(&repo, "test.txt", "line 1\nai line\nhuman line\n");
    repo.git_ai(&["checkpoint"]).unwrap();

    // Read checkpoints from the working log
    let working_log = repo.current_working_logs();
    let checkpoints = working_log.read_all_checkpoints().unwrap();

    // With cloud_default_ai_attribution enabled, the human checkpoint is promoted
    // to AiAgent kind and attributed to the most recent AI agent.
    let cloud_cp = checkpoints.last().expect("Should have checkpoints");

    assert_eq!(
        cloud_cp.kind,
        git_ai::authorship::working_log::CheckpointKind::AiAgent,
        "Cloud-attributed checkpoint should have AiAgent kind"
    );
    assert!(
        cloud_cp.agent_id.is_some(),
        "Cloud-attributed checkpoint should have agent_id"
    );
}

/// When cloud_default_ai_attribution is disabled, human checkpoints should NOT
/// have an agent_id set (normal behavior).
#[test]
fn test_cloud_attribution_disabled_no_agent_id_on_human() {
    let mut repo = TestRepo::new_dedicated_daemon();
    repo.set_feature_flags(FeatureFlags {
        cloud_default_ai_attribution: false,
        ..FeatureFlags::default()
    });

    // Initial commit
    write_file(&repo, "test.txt", "line 1\n");
    repo.stage_all_and_commit("initial").unwrap();

    // AI checkpoint
    write_file(&repo, "test.txt", "line 1\nai line\n");
    repo.git_ai(&["checkpoint", "mock_ai"]).unwrap();

    // Human checkpoint with cloud attribution disabled
    write_file(&repo, "test.txt", "line 1\nai line\nhuman line\n");
    repo.git_ai(&["checkpoint"]).unwrap();

    let working_log = repo.current_working_logs();
    let checkpoints = working_log.read_all_checkpoints().unwrap();

    let human_cp = checkpoints
        .iter()
        .rev()
        .find(|cp| cp.kind == git_ai::authorship::working_log::CheckpointKind::Human)
        .expect("Should have a human checkpoint");

    // With feature disabled, human checkpoint should NOT have agent_id
    assert!(
        human_cp.agent_id.is_none(),
        "Human checkpoint should NOT have agent_id when cloud_default_ai_attribution is disabled"
    );
}

/// When cloud_default_ai_attribution is enabled but there are no previous AI checkpoints,
/// the human checkpoint should fall back to cloud env detection. The detected tool depends
/// on the environment (e.g., "devin" if /opt/.devin exists, "unknown" otherwise).
#[test]
fn test_cloud_attribution_no_ai_checkpoint_uses_fallback() {
    let mut repo = TestRepo::new_dedicated_daemon();
    repo.set_feature_flags(FeatureFlags {
        cloud_default_ai_attribution: true,
        ..FeatureFlags::default()
    });

    // Initial commit
    write_file(&repo, "test.txt", "line 1\n");
    repo.stage_all_and_commit("initial").unwrap();

    // Only human checkpoint - no AI checkpoints at all
    write_file(&repo, "test.txt", "line 1\nhuman line\n");
    repo.git_ai(&["checkpoint"]).unwrap();

    let working_log = repo.current_working_logs();
    let checkpoints = working_log.read_all_checkpoints().unwrap();

    let cloud_cp = checkpoints.last().expect("Should have a checkpoint");

    // With no previous AI checkpoint, should be promoted to AiAgent with fallback.
    assert_eq!(
        cloud_cp.kind,
        git_ai::authorship::working_log::CheckpointKind::AiAgent,
        "Cloud-attributed checkpoint should have AiAgent kind"
    );
    assert!(
        cloud_cp.agent_id.is_some(),
        "Cloud-attributed checkpoint should have fallback agent_id"
    );

    let agent_id = cloud_cp.agent_id.as_ref().unwrap();
    assert_eq!(
        agent_id.id, "cloud-default",
        "Fallback agent_id should have id 'cloud-default'"
    );
    // The tool depends on the environment: "devin" if /opt/.devin exists, "unknown" otherwise.
    assert!(
        !agent_id.tool.is_empty(),
        "Fallback agent_id should have a non-empty tool name"
    );
}

/// When cloud_default_ai_attribution is enabled and CLOUD_AGENT_TOOL env var is set,
/// the fallback (no previous AI checkpoint) should use that tool name.
#[test]
fn test_cloud_attribution_fallback_uses_cloud_agent_tool_env() {
    let mut repo = TestRepo::new_dedicated_daemon();
    repo.set_feature_flags(FeatureFlags {
        cloud_default_ai_attribution: true,
        ..FeatureFlags::default()
    });

    // Initial commit
    write_file(&repo, "test.txt", "line 1\n");
    repo.stage_all_and_commit("initial").unwrap();

    // Human checkpoint with CLOUD_AGENT_TOOL env var set (no AI checkpoint)
    write_file(&repo, "test.txt", "line 1\nhuman line\n");
    repo.git_ai_with_env(
        &["checkpoint"],
        &[("CLOUD_AGENT_TOOL", "custom-cloud-tool")],
    )
    .unwrap();

    let working_log = repo.current_working_logs();
    let checkpoints = working_log.read_all_checkpoints().unwrap();

    let cloud_cp = checkpoints.last().expect("Should have a checkpoint");

    assert_eq!(
        cloud_cp.kind,
        git_ai::authorship::working_log::CheckpointKind::AiAgent
    );
    assert!(cloud_cp.agent_id.is_some());

    let agent_id = cloud_cp.agent_id.as_ref().unwrap();
    assert_eq!(
        agent_id.tool, "custom-cloud-tool",
        "Fallback should use CLOUD_AGENT_TOOL env var value"
    );
    assert_eq!(agent_id.id, "cloud-default");
}

/// When cloud_default_ai_attribution is enabled with CURSOR_AGENT=1 env var,
/// the fallback should detect cursor-agent as the tool.
#[test]
fn test_cloud_attribution_fallback_detects_cursor_agent() {
    let mut repo = TestRepo::new_dedicated_daemon();
    repo.set_feature_flags(FeatureFlags {
        cloud_default_ai_attribution: true,
        ..FeatureFlags::default()
    });

    // Initial commit
    write_file(&repo, "test.txt", "line 1\n");
    repo.stage_all_and_commit("initial").unwrap();

    // Human checkpoint with CURSOR_AGENT=1 (no AI checkpoint)
    write_file(&repo, "test.txt", "line 1\nhuman line\n");
    repo.git_ai_with_env(&["checkpoint"], &[("CURSOR_AGENT", "1")])
        .unwrap();

    let working_log = repo.current_working_logs();
    let checkpoints = working_log.read_all_checkpoints().unwrap();

    let cloud_cp = checkpoints.last().expect("Should have a checkpoint");

    assert_eq!(
        cloud_cp.kind,
        git_ai::authorship::working_log::CheckpointKind::AiAgent
    );
    assert!(cloud_cp.agent_id.is_some());

    let agent_id = cloud_cp.agent_id.as_ref().unwrap();
    assert_eq!(
        agent_id.tool, "cursor-agent",
        "Fallback should detect cursor-agent from CURSOR_AGENT=1"
    );
}

/// When cloud_default_ai_attribution is enabled with CLAUDE_CODE_REMOTE=true env var,
/// the fallback should detect claude-web as the tool.
#[test]
fn test_cloud_attribution_fallback_detects_claude_web() {
    let mut repo = TestRepo::new_dedicated_daemon();
    repo.set_feature_flags(FeatureFlags {
        cloud_default_ai_attribution: true,
        ..FeatureFlags::default()
    });

    // Initial commit
    write_file(&repo, "test.txt", "line 1\n");
    repo.stage_all_and_commit("initial").unwrap();

    // Human checkpoint with CLAUDE_CODE_REMOTE=true (no AI checkpoint)
    write_file(&repo, "test.txt", "line 1\nhuman line\n");
    repo.git_ai_with_env(&["checkpoint"], &[("CLAUDE_CODE_REMOTE", "true")])
        .unwrap();

    let working_log = repo.current_working_logs();
    let checkpoints = working_log.read_all_checkpoints().unwrap();

    let cloud_cp = checkpoints.last().expect("Should have a checkpoint");

    assert_eq!(
        cloud_cp.kind,
        git_ai::authorship::working_log::CheckpointKind::AiAgent
    );
    assert!(cloud_cp.agent_id.is_some());

    let agent_id = cloud_cp.agent_id.as_ref().unwrap();
    assert_eq!(
        agent_id.tool, "claude-web",
        "Fallback should detect claude-web from CLAUDE_CODE_REMOTE=true"
    );
}

/// When cloud_default_ai_attribution is enabled and there are multiple AI checkpoints,
/// the human checkpoint should use the MOST RECENT one's agent_id.
#[test]
fn test_cloud_attribution_uses_most_recent_of_multiple_ai_checkpoints() {
    let mut repo = TestRepo::new_dedicated_daemon();
    repo.set_feature_flags(FeatureFlags {
        cloud_default_ai_attribution: true,
        ..FeatureFlags::default()
    });

    // Initial commit
    write_file(&repo, "test.txt", "line 1\n");
    repo.stage_all_and_commit("initial").unwrap();

    // First AI checkpoint
    write_file(&repo, "test.txt", "line 1\nfirst ai line\n");
    repo.git_ai(&["checkpoint", "mock_ai"]).unwrap();

    // Second AI checkpoint
    write_file(&repo, "test.txt", "line 1\nfirst ai line\nsecond ai line\n");
    repo.git_ai(&["checkpoint", "mock_ai"]).unwrap();

    // Human checkpoint - should be attributed to the most recent AI agent
    write_file(
        &repo,
        "test.txt",
        "line 1\nfirst ai line\nsecond ai line\nhuman line\n",
    );
    repo.git_ai(&["checkpoint"]).unwrap();

    let working_log = repo.current_working_logs();
    let checkpoints = working_log.read_all_checkpoints().unwrap();

    let cloud_cp = checkpoints.last().expect("Should have checkpoints");

    assert_eq!(
        cloud_cp.kind,
        git_ai::authorship::working_log::CheckpointKind::AiAgent,
        "Cloud-attributed checkpoint should have AiAgent kind"
    );
    assert!(
        cloud_cp.agent_id.is_some(),
        "Cloud-attributed checkpoint should have agent_id from most recent AI checkpoint"
    );
}

/// Verify that AI checkpoints are NOT affected by cloud_default_ai_attribution.
/// The flag only modifies human checkpoint behavior.
#[test]
fn test_cloud_attribution_does_not_affect_ai_checkpoints() {
    let mut repo = TestRepo::new_dedicated_daemon();
    repo.set_feature_flags(FeatureFlags {
        cloud_default_ai_attribution: true,
        ..FeatureFlags::default()
    });

    // Initial commit
    write_file(&repo, "test.txt", "line 1\n");
    repo.stage_all_and_commit("initial").unwrap();

    // AI checkpoint should work normally
    write_file(&repo, "test.txt", "line 1\nai line\n");
    repo.git_ai(&["checkpoint", "mock_ai"]).unwrap();

    let working_log = repo.current_working_logs();
    let checkpoints = working_log.read_all_checkpoints().unwrap();

    // The AI checkpoint should be an AI checkpoint regardless of the feature flag
    let ai_checkpoints: Vec<_> = checkpoints
        .iter()
        .filter(|cp| cp.kind != git_ai::authorship::working_log::CheckpointKind::Human)
        .collect();

    assert!(
        !ai_checkpoints.is_empty(),
        "Should have at least one AI checkpoint"
    );
}

/// When cloud_default_ai_attribution is explicitly disabled via set_feature_flags(false),
/// human checkpoints should NOT get attribution even with prior AI checkpoints.
#[test]
fn test_cloud_attribution_explicit_disable() {
    let mut repo = TestRepo::new_dedicated_daemon();
    repo.set_feature_flags(FeatureFlags {
        cloud_default_ai_attribution: false,
        ..FeatureFlags::default()
    });

    // Initial commit
    write_file(&repo, "test.txt", "line 1\n");
    repo.stage_all_and_commit("initial").unwrap();

    // AI checkpoint
    write_file(&repo, "test.txt", "line 1\nai line\n");
    repo.git_ai(&["checkpoint", "mock_ai"]).unwrap();

    // Human edit
    write_file(&repo, "test.txt", "line 1\nai line\nhuman line\n");
    repo.git_ai(&["checkpoint"]).unwrap();

    let working_log = repo.current_working_logs();
    let checkpoints = working_log.read_all_checkpoints().unwrap();

    let human_cp = checkpoints
        .iter()
        .rev()
        .find(|cp| cp.kind == git_ai::authorship::working_log::CheckpointKind::Human)
        .expect("Should have a human checkpoint");

    assert!(
        human_cp.agent_id.is_none(),
        "Human checkpoint should NOT have agent_id when flag is explicitly disabled"
    );
}

/// Verify the feature flag correctly propagates through TestRepo injection.
/// With the flag enabled, a human-only checkpoint (no prior AI) should still
/// get a fallback agent_id with cloud-default id.
#[test]
fn test_feature_flag_injection_via_test_repo() {
    let mut repo = TestRepo::new_dedicated_daemon();
    repo.set_feature_flags(FeatureFlags {
        cloud_default_ai_attribution: true,
        ..FeatureFlags::default()
    });

    // Initial commit
    write_file(&repo, "test.txt", "line 1\n");
    repo.stage_all_and_commit("initial").unwrap();

    // Human checkpoint only (no AI checkpoint exists)
    write_file(&repo, "test.txt", "line 1\nhuman line\n");
    repo.git_ai(&["checkpoint"]).unwrap();

    let working_log = repo.current_working_logs();
    let checkpoints = working_log.read_all_checkpoints().unwrap();

    let cloud_cp = checkpoints.last().expect("Should have a checkpoint");

    // The flag is enabled via feature flag injection, so checkpoint is promoted to AiAgent
    assert_eq!(
        cloud_cp.kind,
        git_ai::authorship::working_log::CheckpointKind::AiAgent,
        "Feature flag injection should promote to AiAgent kind"
    );
    assert!(
        cloud_cp.agent_id.is_some(),
        "Feature flag injection should work through TestRepo - expected fallback agent_id"
    );

    let agent_id = cloud_cp.agent_id.as_ref().unwrap();
    assert_eq!(agent_id.id, "cloud-default");
}

/// When a CLOUD_AGENT_* prefix env var is set (other than CLOUD_AGENT_TOOL),
/// the fallback should detect "cloud-agent" as the generic tool name.
#[test]
fn test_cloud_attribution_fallback_detects_generic_cloud_agent_prefix() {
    let mut repo = TestRepo::new_dedicated_daemon();
    repo.set_feature_flags(FeatureFlags {
        cloud_default_ai_attribution: true,
        ..FeatureFlags::default()
    });

    // Initial commit
    write_file(&repo, "test.txt", "line 1\n");
    repo.stage_all_and_commit("initial").unwrap();

    // Human checkpoint with a CLOUD_AGENT_* prefix env var (no AI checkpoint)
    write_file(&repo, "test.txt", "line 1\nhuman line\n");
    repo.git_ai_with_env(&["checkpoint"], &[("CLOUD_AGENT_CUSTOM", "yes")])
        .unwrap();

    let working_log = repo.current_working_logs();
    let checkpoints = working_log.read_all_checkpoints().unwrap();

    let cloud_cp = checkpoints.last().expect("Should have a checkpoint");

    assert_eq!(
        cloud_cp.kind,
        git_ai::authorship::working_log::CheckpointKind::AiAgent
    );
    assert!(cloud_cp.agent_id.is_some());

    let agent_id = cloud_cp.agent_id.as_ref().unwrap();
    assert_eq!(
        agent_id.tool, "cloud-agent",
        "Fallback should detect 'cloud-agent' from CLOUD_AGENT_* prefix env var"
    );
    assert_eq!(agent_id.id, "cloud-default");
}
