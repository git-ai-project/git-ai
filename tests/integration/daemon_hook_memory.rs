use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use serde_json::json;
use std::fs;

#[cfg(target_os = "linux")]
fn daemon_child_count(repo: &TestRepo) -> usize {
    let pid = repo.daemon_pid().expect("test repo should own a daemon");
    let mut children = std::collections::HashSet::new();
    for task in fs::read_dir(format!("/proc/{pid}/task")).unwrap() {
        let task = task.unwrap();
        let child_list = fs::read_to_string(task.path().join("children")).unwrap_or_default();
        children.extend(child_list.split_whitespace().map(str::to_string));
    }
    children.len()
}

#[test]
#[cfg(target_os = "linux")]
fn post_notes_hook_burst_keeps_children_bounded_and_attribution_working() {
    let hook_dir = tempfile::tempdir().unwrap();
    let marker = hook_dir.path().join("hook-ran");
    let gate = hook_dir.path().join("keep-hook-running");
    fs::write(&gate, "").unwrap();
    let hook_command = format!(
        "touch {}; while [ -e {} ]; do sleep 0.05; done",
        marker.display(),
        gate.display()
    );
    let config_patch = json!({
        "git_ai_hooks": {
            "post_notes_updated": vec![hook_command; 8],
        }
    })
    .to_string();
    let repo = TestRepo::new_with_daemon_env(&[("GIT_AI_TEST_CONFIG_PATCH", &config_patch)]);
    let file_path = repo.path().join("tracked.txt");

    fs::write(&file_path, "ai line\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "tracked.txt"])
        .unwrap();
    repo.stage_all_and_commit("AI commit with hook pressure")
        .unwrap();
    assert!(marker.exists(), "post-notes hook should execute");
    let children = daemon_child_count(&repo);
    let mut file = repo.filename("tracked.txt");
    file.assert_committed_lines(lines!["ai line".ai()]);

    assert!(
        children <= 4,
        "post-notes hook burst left {children} daemon child processes"
    );
    fs::remove_file(&gate).unwrap();

    fs::write(&file_path, "ai line\nsecond ai line\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "tracked.txt"])
        .unwrap();
    repo.stage_all_and_commit("AI edit after hook pressure")
        .unwrap();
    file.assert_committed_lines(lines!["ai line".ai(), "second ai line".ai()]);
}
