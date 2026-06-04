#[macro_use]
#[path = "integration/repos/mod.rs"]
mod repos;

// Graphite-style restacks rewrite commits with `git commit-tree` + `git update-ref`.
// These tests model that plumbing path directly so they do not depend on `gt`.

use git_ai::authorship::authorship_log_serialization::AuthorshipLog;
use git_ai::daemon::open_local_socket_stream_with_timeout;
use git_ai::git::find_repository_in_path;
use git_ai::git::refs::show_authorship_note;
use git_ai::git::repository::Repository as GitAiRepository;
use repos::test_file::ExpectedLineExt;
use repos::test_repo::{TestRepo, new_daemon_test_sync_session_id, real_git_executable};
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output};
use std::time::Duration;

fn setup_initial_commit(repo: &TestRepo) {
    let mut readme = repo.filename("README.md");
    readme.set_contents(lines!["# Test Repo"]);
    repo.stage_all_and_commit("initial commit")
        .expect("initial commit should succeed");
}

fn open_repo(repo: &TestRepo) -> GitAiRepository {
    find_repository_in_path(repo.path().to_str().unwrap())
        .expect("failed to open git-ai repository")
}

fn head_sha(repo: &TestRepo) -> String {
    repo.git(&["rev-parse", "HEAD"])
        .expect("rev-parse HEAD should succeed")
        .trim()
        .to_string()
}

fn assert_note_has_ai_for_file(repo: &TestRepo, commit_sha: &str, file_path: &str) {
    let note = repo
        .read_authorship_note(commit_sha)
        .unwrap_or_else(|| panic!("commit {} should have authorship note", &commit_sha[..8]));
    let log = AuthorshipLog::deserialize_from_string(&note).expect("parse authorship note");
    let attestation = log
        .attestations
        .iter()
        .find(|attestation| attestation.file_path == file_path)
        .unwrap_or_else(|| {
            panic!(
                "commit {} should have attestation for {}: {:?}",
                &commit_sha[..8],
                file_path,
                log.attestations
            )
        });
    assert!(
        attestation.entries.iter().any(|entry| {
            let author_id = entry.hash.split("::").next().unwrap_or(&entry.hash);
            log.metadata.sessions.contains_key(author_id)
                || log.metadata.prompts.contains_key(&entry.hash)
        }),
        "commit {} attestation for {} should contain AI entry: {:?}",
        &commit_sha[..8],
        file_path,
        attestation.entries
    );
}

fn raw_traced_git(repo: &TestRepo, args: &[&str]) -> String {
    let mut command = Command::new(real_git_executable());
    command.arg("-C").arg(repo.path()).args(args);
    command.env("HOME", repo.test_home_path());
    command.env(
        "GIT_CONFIG_GLOBAL",
        repo.test_home_path().join(".gitconfig"),
    );
    command.env("XDG_CONFIG_HOME", repo.test_home_path().join(".config"));
    command.env("GIT_CONFIG_NOSYSTEM", "1");
    command.env(
        "GIT_TRACE2_EVENT",
        git_ai::daemon::DaemonConfig::trace2_event_target_for_path(
            &repo.daemon_trace_socket_path(),
        ),
    );
    command.env(
        "GIT_TRACE2_EVENT_NESTING",
        std::env::var("GIT_AI_TEST_TRACE2_NESTING").unwrap_or_else(|_| "10".to_string()),
    );

    let output = command
        .output()
        .unwrap_or_else(|error| panic!("failed to run raw traced git {:?}: {}", args, error));
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(
        output.status.success(),
        "raw traced git {:?} failed\nstdout: {}\nstderr: {}",
        args,
        stdout,
        stderr
    );
    if stdout.is_empty() {
        stderr
    } else if stderr.is_empty() {
        stdout
    } else {
        format!("{}{}", stdout, stderr)
    }
}

fn raw_untraced_git(repo: &TestRepo, args: &[&str]) -> String {
    repo.git_og_with_env(args, &[("GIT_TRACE2_EVENT", "0")])
        .unwrap_or_else(|error| panic!("raw untraced git {:?} failed: {}", args, error))
}

fn raw_git_trace_to_file(repo: &TestRepo, args: &[&str], trace_path: &Path) -> String {
    let output = raw_git_trace_to_file_output(repo, args, trace_path);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(
        output.status.success(),
        "raw traced git {:?} failed\nstdout: {}\nstderr: {}",
        args,
        stdout,
        stderr
    );
    combined_output(stdout, stderr)
}

fn raw_git_trace_to_file_output(repo: &TestRepo, args: &[&str], trace_path: &Path) -> Output {
    let _ = fs::remove_file(trace_path);
    let mut command = Command::new(real_git_executable());
    command.arg("-C").arg(repo.path()).args(args);
    command.env("HOME", repo.test_home_path());
    command.env(
        "GIT_CONFIG_GLOBAL",
        repo.test_home_path().join(".gitconfig"),
    );
    command.env("XDG_CONFIG_HOME", repo.test_home_path().join(".config"));
    command.env("GIT_CONFIG_NOSYSTEM", "1");
    command.env("GIT_TRACE2_EVENT", trace_path);
    command.env(
        "GIT_TRACE2_EVENT_NESTING",
        std::env::var("GIT_AI_TEST_TRACE2_NESTING").unwrap_or_else(|_| "10".to_string()),
    );

    command
        .output()
        .unwrap_or_else(|error| panic!("failed to run raw traced git {:?}: {}", args, error))
}

fn combined_output(stdout: String, stderr: String) -> String {
    if stdout.is_empty() {
        stderr
    } else if stderr.is_empty() {
        stdout
    } else {
        format!("{}{}", stdout, stderr)
    }
}

fn replay_trace_file_to_daemon(repo: &TestRepo, trace_path: &Path) {
    let trace = fs::read(trace_path)
        .unwrap_or_else(|error| panic!("failed to read {}: {}", trace_path.display(), error));
    let mut stream = open_local_socket_stream_with_timeout(
        &repo.daemon_trace_socket_path(),
        Duration::from_secs(2),
    )
    .expect("connect to daemon trace socket");
    stream
        .write_all(&trace)
        .expect("write delayed trace payload to daemon");
    stream.flush().expect("flush delayed trace payload");
}

fn commit_tree_rewrite_current_branch(
    repo: &TestRepo,
    branch: &str,
    new_parent: &str,
    message: &str,
) -> (String, String) {
    let old_head = head_sha(repo);
    let tree = repo
        .git(&["rev-parse", &format!("{}^{{tree}}", old_head)])
        .expect("rev-parse HEAD^{tree} should succeed")
        .trim()
        .to_string();

    let new_head = repo
        .git(&["commit-tree", &tree, "-p", new_parent, "-m", message])
        .expect("git commit-tree should succeed")
        .trim()
        .to_string();

    repo.git(&[
        "update-ref",
        &format!("refs/heads/{}", branch),
        &new_head,
        &old_head,
    ])
    .expect("git update-ref should succeed");

    (old_head, new_head)
}

fn commit_tree_from_existing_tree(
    repo: &TestRepo,
    treeish: &str,
    new_parent: &str,
    message: &str,
) -> String {
    let tree = repo
        .git(&["rev-parse", &format!("{}^{{tree}}", treeish)])
        .expect("rev-parse tree should succeed")
        .trim()
        .to_string();

    repo.git(&["commit-tree", &tree, "-p", new_parent, "-m", message])
        .expect("git commit-tree should succeed")
        .trim()
        .to_string()
}

fn graphite_style_restack_child_branch(
    repo: &TestRepo,
    branch: &str,
    old_head: &str,
    new_parent: &str,
    message: &str,
) -> String {
    let old_parent = repo
        .git(&["rev-parse", &format!("{}^", old_head)])
        .expect("rev-parse old parent should succeed")
        .trim()
        .to_string();
    let old_grandparent = repo
        .git(&["rev-parse", &format!("{}^", old_parent)])
        .expect("rev-parse old grandparent should succeed")
        .trim()
        .to_string();

    let synthetic_parent = commit_tree_from_existing_tree(repo, new_parent, &old_grandparent, "_");
    let merged_tree = repo
        .git(&[
            "merge-tree",
            "--allow-unrelated-histories",
            &synthetic_parent,
            old_head,
        ])
        .expect("git merge-tree should succeed")
        .trim()
        .to_string();

    let new_head = repo
        .git(&["commit-tree", &merged_tree, "-p", new_parent, "-m", message])
        .expect("git commit-tree for rewritten child should succeed")
        .trim()
        .to_string();

    repo.git(&[
        "update-ref",
        &format!("refs/heads/{}", branch),
        &new_head,
        old_head,
    ])
    .expect("git update-ref should succeed");

    new_head
}

#[test]
fn test_soft_reset_amend_then_branch_move_preserves_squashed_child_attribution() {
    let repo = TestRepo::new();
    setup_initial_commit(&repo);

    repo.git(&["checkout", "-b", "parent"])
        .expect("checkout parent should succeed");
    let mut parent_file = repo.filename("csf_parent.txt");
    parent_file.set_contents(lines!["parent line 1", "parent line 2"]);
    repo.stage_all_and_commit("parent")
        .expect("parent commit should succeed");

    repo.git(&["checkout", "-b", "child"])
        .expect("checkout child should succeed");
    let mut child_file = repo.filename("csf_child.txt");
    child_file.set_contents(lines!["child ai 1".ai()]);
    let child_one = repo
        .stage_all_and_commit("child commit 1")
        .expect("child commit 1 should succeed");

    child_file.set_contents(lines!["child ai 1".ai(), "child ai 2".ai()]);
    repo.stage_all_and_commit("child commit 2")
        .expect("child commit 2 should succeed");

    repo.sync_daemon();
    let baseline = repo.daemon_total_completion_count();

    raw_traced_git(&repo, &["reset", "--soft", &child_one.commit_sha]);
    raw_traced_git(&repo, &["commit", "--amend", "-m", "squashed child"]);
    raw_traced_git(&repo, &["switch", "-C", "parent", "HEAD"]);
    repo.wait_for_daemon_total_completion_count(baseline, baseline + 3);

    parent_file.assert_lines_and_blame(lines!["parent line 1".human(), "parent line 2".human(),]);
    child_file.assert_lines_and_blame(lines!["child ai 1".ai(), "child ai 2".ai()]);
}

#[test]
fn test_back_to_back_raw_commits_do_not_span_later_ref_move() {
    let repo = TestRepo::new();
    setup_initial_commit(&repo);

    fs::write(repo.path().join("first.txt"), "first ai\n").unwrap();
    fs::write(repo.path().join("second.txt"), "second ai\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "first.txt"])
        .unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "second.txt"])
        .unwrap();
    repo.sync_daemon();
    let baseline = repo.daemon_total_completion_count();

    raw_untraced_git(&repo, &["add", "first.txt"]);
    raw_traced_git(&repo, &["commit", "-m", "first raw commit"]);
    let first_commit = head_sha(&repo);

    raw_untraced_git(&repo, &["add", "second.txt"]);
    raw_traced_git(&repo, &["commit", "-m", "second raw commit"]);
    let second_commit = head_sha(&repo);

    repo.wait_for_daemon_total_completion_count(baseline, baseline + 2);

    assert_note_has_ai_for_file(&repo, &first_commit, "first.txt");
    assert_note_has_ai_for_file(&repo, &second_commit, "second.txt");
}

#[test]
fn test_raw_commit_trace2_does_not_record_created_commit_oid() {
    let repo = TestRepo::new();
    setup_initial_commit(&repo);

    fs::write(repo.path().join("trace-only.txt"), "trace only\n").unwrap();
    raw_untraced_git(&repo, &["add", "trace-only.txt"]);

    let trace_dir = tempfile::tempdir().expect("trace temp dir");
    let commit_trace = trace_dir.path().join("commit.trace2");

    raw_git_trace_to_file(&repo, &["commit", "-m", "trace only"], &commit_trace);
    let commit_sha = head_sha(&repo);
    let trace = fs::read_to_string(&commit_trace).expect("read trace2 file");

    assert!(
        !trace.contains(&commit_sha),
        "stock trace2 should not contain the created commit oid"
    );
}

#[test]
fn test_delayed_commit_trace_replay_attributes_matching_commit_not_later_commit() {
    let repo = TestRepo::new();
    setup_initial_commit(&repo);

    fs::write(repo.path().join("first-delayed.txt"), "first delayed ai\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "first-delayed.txt"])
        .unwrap();
    raw_untraced_git(&repo, &["add", "first-delayed.txt"]);
    repo.sync_daemon();
    let baseline = repo.daemon_total_completion_count();

    let trace_dir = tempfile::tempdir().expect("trace temp dir");
    let commit_trace = trace_dir.path().join("commit.trace2");

    raw_git_trace_to_file(&repo, &["commit", "-m", "first delayed"], &commit_trace);
    let first_commit = head_sha(&repo);

    fs::write(repo.path().join("later-delayed.txt"), "later untraced\n").unwrap();
    raw_untraced_git(&repo, &["add", "later-delayed.txt"]);
    raw_untraced_git(&repo, &["commit", "-m", "later untraced commit"]);
    let later_commit = head_sha(&repo);

    replay_trace_file_to_daemon(&repo, &commit_trace);
    repo.wait_for_daemon_total_completion_count(baseline, baseline + 1);

    assert_note_has_ai_for_file(&repo, &first_commit, "first-delayed.txt");
    assert!(
        repo.read_authorship_note(&later_commit).is_none(),
        "delayed commit trace replay must not attach attribution to a later commit"
    );
}

#[cfg(not(windows))]
#[test]
fn test_trace_listener_bootstrap_captures_commit_ref_transition_before_worker_spawn_delay() {
    let repo = TestRepo::new_with_daemon_env(&[(
        "GIT_AI_TEST_TRACE_LISTENER_WORKER_SPAWN_DELAY_MS",
        "200",
    )]);
    fs::write(repo.path().join("README.md"), "base\n").unwrap();
    repo.git_og(&["add", "README.md"]).unwrap();
    repo.git_og(&["commit", "-m", "base"]).unwrap();

    fs::write(
        repo.path().join("bootstrap-race.txt"),
        "bootstrap race ai\n",
    )
    .unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "bootstrap-race.txt"])
        .unwrap();
    repo.git(&["add", "bootstrap-race.txt"]).unwrap();
    let committed = repo.commit("bootstrap race").unwrap();

    assert_note_has_ai_for_file(&repo, &committed.commit_sha, "bootstrap-race.txt");
}

#[test]
#[ignore = "stock trace2 does not record merge --squash source oid after SQUASH_MSG is gone"]
fn test_delayed_squash_merge_trace_replay_preserves_source_attribution() {
    let repo = TestRepo::new();
    let mut file = repo.filename("main.txt");

    file.set_contents(lines!["base"]);
    repo.stage_all_and_commit("base").unwrap();
    let default_branch = repo.current_branch();

    repo.git(&["checkout", "-b", "feature"]).unwrap();
    file.insert_at(1, lines!["feature ai".ai()]);
    repo.stage_all_and_commit("feature ai").unwrap();

    repo.git(&["checkout", &default_branch]).unwrap();
    repo.sync_daemon();
    let baseline = repo.daemon_total_completion_count();

    let trace_dir = tempfile::tempdir().expect("trace temp dir");
    let merge_trace = trace_dir.path().join("merge.trace2");
    let commit_trace = trace_dir.path().join("commit.trace2");

    raw_git_trace_to_file(&repo, &["merge", "--squash", "feature"], &merge_trace);
    raw_git_trace_to_file(&repo, &["commit", "-m", "squash feature"], &commit_trace);
    let squash_commit = head_sha(&repo);

    replay_trace_file_to_daemon(&repo, &merge_trace);
    replay_trace_file_to_daemon(&repo, &commit_trace);
    repo.wait_for_daemon_total_completion_count(baseline, baseline + 2);

    assert_note_has_ai_for_file(&repo, &squash_commit, "main.txt");
}

#[test]
fn test_delayed_stash_apply_trace_replay_preserves_named_stash_attribution() {
    let repo = TestRepo::new();
    let mut readme = repo.filename("README.md");
    readme.set_contents(lines!["base"]);
    repo.stage_all_and_commit("base").unwrap();

    let mut first = repo.filename("first.txt");
    first.set_contents(lines!["first stash ai".ai()]);
    repo.git_ai(&["checkpoint", "mock_ai", "first.txt"])
        .unwrap();
    repo.git(&["stash", "push", "-m", "first"]).unwrap();

    let mut second = repo.filename("second.txt");
    second.set_contents(lines!["second stash ai".ai()]);
    repo.git_ai(&["checkpoint", "mock_ai", "second.txt"])
        .unwrap();
    repo.git(&["stash", "push", "-m", "second"]).unwrap();

    repo.sync_daemon();
    let baseline = repo.daemon_total_completion_count();

    let trace_dir = tempfile::tempdir().expect("trace temp dir");
    let apply_trace = trace_dir.path().join("stash-apply.trace2");

    raw_git_trace_to_file(&repo, &["stash", "apply", "stash@{1}"], &apply_trace);
    repo.git_og(&["stash", "drop", "stash@{1}"])
        .expect("drop applied stash after raw apply");

    replay_trace_file_to_daemon(&repo, &apply_trace);
    repo.wait_for_daemon_total_completion_count(baseline, baseline + 1);

    repo.stage_all_and_commit("apply first stash").unwrap();
    first.assert_committed_lines(lines!["first stash ai".ai()]);
}

#[test]
fn test_delayed_stash_pop_trace_replay_preserves_popped_stash_attribution() {
    let repo = TestRepo::new();
    let mut readme = repo.filename("README.md");
    readme.set_contents(lines!["base"]);
    repo.stage_all_and_commit("base").unwrap();

    let mut first = repo.filename("first.txt");
    first.set_contents(lines!["first stash ai".ai()]);
    repo.git_ai(&["checkpoint", "mock_ai", "first.txt"])
        .unwrap();
    repo.git(&["stash", "push", "-m", "first"]).unwrap();

    let mut second = repo.filename("second.txt");
    second.set_contents(lines!["second stash ai".ai()]);
    repo.git_ai(&["checkpoint", "mock_ai", "second.txt"])
        .unwrap();
    repo.git(&["stash", "push", "-m", "second"]).unwrap();

    repo.sync_daemon();
    let baseline = repo.daemon_total_completion_count();

    let trace_dir = tempfile::tempdir().expect("trace temp dir");
    let pop_trace = trace_dir.path().join("stash-pop.trace2");

    raw_git_trace_to_file(&repo, &["stash", "pop"], &pop_trace);
    repo.git_og(&["stash", "drop", "stash@{0}"])
        .expect("drop remaining stash after raw pop");

    replay_trace_file_to_daemon(&repo, &pop_trace);
    repo.wait_for_daemon_total_completion_count(baseline, baseline + 1);

    repo.stage_all_and_commit("apply second stash").unwrap();
    second.assert_committed_lines(lines!["second stash ai".ai()]);
}

#[test]
#[ignore = "stock trace2 does not record final uncommitted worktree bytes for switch --merge"]
fn test_delayed_switch_merge_trace_replay_does_not_attribute_later_uncheckpointed_edit() {
    let repo = TestRepo::new();
    let mut file = repo.filename("merge-carry.txt");

    file.set_contents(lines!["one", "two"]);
    repo.stage_all_and_commit("base").unwrap();
    let default_branch = repo.current_branch();

    repo.git(&["checkout", "-b", "feature"]).unwrap();
    fs::write(repo.path().join("merge-carry.txt"), "one feature\ntwo\n").unwrap();
    repo.stage_all_and_commit("feature edit").unwrap();

    repo.git(&["checkout", &default_branch]).unwrap();
    fs::write(repo.path().join("merge-carry.txt"), "one\ntwo ai\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "merge-carry.txt"])
        .unwrap();
    repo.sync_daemon();
    let baseline = repo.daemon_total_completion_count();

    let trace_dir = tempfile::tempdir().expect("trace temp dir");
    let switch_trace = trace_dir.path().join("switch-merge.trace2");

    raw_git_trace_to_file(&repo, &["switch", "--merge", "feature"], &switch_trace);
    fs::write(
        repo.path().join("merge-carry.txt"),
        "one feature\ntwo ai\nlater untracked\n",
    )
    .unwrap();

    replay_trace_file_to_daemon(&repo, &switch_trace);
    repo.wait_for_daemon_total_completion_count(baseline, baseline + 1);

    repo.stage_all_and_commit("commit carried merge").unwrap();
    file.assert_committed_lines(lines![
        "one feature".human(),
        "two ai".ai(),
        "later untracked".unattributed_human()
    ]);
}

#[test]
#[ignore = "stock trace2 does not record checkout/switch old and new HEAD oids when replayed after refs moved"]
fn test_delayed_switch_trace_replay_renames_working_log_for_uncommitted_attribution() {
    let repo = TestRepo::new();
    setup_initial_commit(&repo);
    let default_branch = repo.current_branch();

    repo.git(&["checkout", "-b", "feature"]).unwrap();
    fs::write(repo.path().join("feature-only.txt"), "feature only\n").unwrap();
    repo.stage_all_and_commit("feature only").unwrap();
    repo.git(&["checkout", &default_branch]).unwrap();

    let mut file = repo.filename("plain-switch.txt");
    file.set_contents(lines!["plain switch ai".ai()]);
    repo.git_ai(&["checkpoint", "mock_ai", "plain-switch.txt"])
        .unwrap();
    repo.sync_daemon();
    let baseline = repo.daemon_total_completion_count();

    let trace_dir = tempfile::tempdir().expect("trace temp dir");
    let switch_trace = trace_dir.path().join("switch.trace2");

    raw_git_trace_to_file(&repo, &["switch", "feature"], &switch_trace);
    replay_trace_file_to_daemon(&repo, &switch_trace);
    repo.wait_for_daemon_total_completion_count(baseline, baseline + 1);

    repo.stage_all_and_commit("commit after plain switch")
        .unwrap();
    file.assert_committed_lines(lines!["plain switch ai".ai()]);
}

#[test]
#[ignore = "stock trace2 does not record rebased output commit oids"]
fn test_delayed_rebase_trace_replay_preserves_rebased_commit_attribution() {
    let repo = TestRepo::new();
    let mut file = repo.filename("feature.txt");

    file.set_contents(lines!["base"]);
    repo.stage_all_and_commit("base").unwrap();
    let default_branch = repo.current_branch();

    repo.git(&["checkout", "-b", "feature"]).unwrap();
    file.set_contents(lines!["base", "feature ai".ai()]);
    let original_feature = repo.stage_all_and_commit("feature ai").unwrap();

    repo.git(&["checkout", &default_branch]).unwrap();
    fs::write(repo.path().join("upstream.txt"), "upstream\n").unwrap();
    repo.stage_all_and_commit("upstream").unwrap();

    repo.git(&["checkout", "feature"]).unwrap();
    repo.sync_daemon();
    let baseline = repo.daemon_total_completion_count();

    let trace_dir = tempfile::tempdir().expect("trace temp dir");
    let rebase_trace = trace_dir.path().join("rebase.trace2");

    raw_git_trace_to_file(&repo, &["rebase", &default_branch], &rebase_trace);
    let rebased_feature = head_sha(&repo);
    assert_ne!(original_feature.commit_sha, rebased_feature);

    fs::write(repo.path().join("later.txt"), "later\n").unwrap();
    repo.git_og(&["add", "later.txt"]).unwrap();
    repo.git_og(&["commit", "-m", "later untraced commit"])
        .unwrap();

    replay_trace_file_to_daemon(&repo, &rebase_trace);
    repo.wait_for_daemon_total_completion_count(baseline, baseline + 1);

    assert_note_has_ai_for_file(&repo, &rebased_feature, "feature.txt");
}

#[test]
#[ignore = "symbolic reset revs like HEAD~1 are not resolvable from delayed stock trace2 after refs move"]
fn test_delayed_reset_trace_replay_reconstructs_reset_working_log_attribution() {
    let repo = TestRepo::new();
    setup_initial_commit(&repo);

    let mut file = repo.filename("reset-delayed.txt");
    file.set_contents(lines!["reset delayed ai".ai()]);
    let original_commit = repo.stage_all_and_commit("reset delayed ai").unwrap();

    repo.sync_daemon();
    let baseline = repo.daemon_total_completion_count();

    let trace_dir = tempfile::tempdir().expect("trace temp dir");
    let reset_trace = trace_dir.path().join("reset.trace2");

    raw_git_trace_to_file(&repo, &["reset", "--mixed", "HEAD~1"], &reset_trace);
    replay_trace_file_to_daemon(&repo, &reset_trace);
    repo.wait_for_daemon_total_completion_count(baseline, baseline + 1);

    let recommit = repo.stage_all_and_commit("recommit reset work").unwrap();
    assert_ne!(original_commit.commit_sha, recommit.commit_sha);
    file.assert_committed_lines(lines!["reset delayed ai".ai()]);
}

#[test]
fn test_delayed_cherry_pick_trace_replay_preserves_picked_commit_attribution() {
    let repo = TestRepo::new();
    let mut file = repo.filename("picked.txt");

    file.set_contents(lines!["base"]);
    repo.stage_all_and_commit("base").unwrap();
    let default_branch = repo.current_branch();

    repo.git(&["checkout", "-b", "feature"]).unwrap();
    file.set_contents(lines!["base", "picked ai".ai()]);
    let source = repo.stage_all_and_commit("picked ai").unwrap();

    repo.git(&["checkout", &default_branch]).unwrap();
    repo.sync_daemon();
    let baseline = repo.daemon_total_completion_count();

    let trace_dir = tempfile::tempdir().expect("trace temp dir");
    let cherry_pick_trace = trace_dir.path().join("cherry-pick.trace2");

    raw_git_trace_to_file(
        &repo,
        &["cherry-pick", &source.commit_sha],
        &cherry_pick_trace,
    );
    let picked_commit = head_sha(&repo);

    fs::write(repo.path().join("later.txt"), "later\n").unwrap();
    repo.git_og(&["add", "later.txt"]).unwrap();
    repo.git_og(&["commit", "-m", "later untraced commit"])
        .unwrap();

    replay_trace_file_to_daemon(&repo, &cherry_pick_trace);
    repo.wait_for_daemon_total_completion_count(baseline, baseline + 1);

    assert_note_has_ai_for_file(&repo, &picked_commit, "picked.txt");
}

#[test]
fn test_delayed_failed_cherry_pick_with_unresolved_source_does_not_consume_later_pick() {
    let repo = TestRepo::new();
    let mut file = repo.filename("file.txt");

    file.set_contents(lines!["base line"]);
    repo.stage_all_and_commit("initial").unwrap();
    let default_branch = repo.current_branch();

    repo.git(&["checkout", "-b", "feature"]).unwrap();
    file.insert_at(1, lines!["AI line 1".ai()]);
    repo.stage_all_and_commit("AI commit 1").unwrap();
    let source_one = head_sha(&repo);

    file.insert_at(2, lines!["AI line 2".ai()]);
    repo.stage_all_and_commit("AI commit 2").unwrap();
    let source_two = head_sha(&repo);

    repo.git(&["checkout", &default_branch]).unwrap();
    repo.sync_daemon();

    let trace_dir = tempfile::tempdir().expect("trace temp dir");
    let failed_trace = trace_dir.path().join("failed-cherry-pick.trace2");
    let good_trace = trace_dir.path().join("good-cherry-pick.trace2");
    let failed_session = new_daemon_test_sync_session_id();
    let good_session = new_daemon_test_sync_session_id();
    let failed_session_arg = format!("git-ai.testSyncSession={failed_session}");
    let good_session_arg = format!("git-ai.testSyncSession={good_session}");
    let bad_source_arg = format!("{source_one} {source_two}");

    let failed = raw_git_trace_to_file_output(
        &repo,
        &["-c", &failed_session_arg, "cherry-pick", &bad_source_arg],
        &failed_trace,
    );
    assert!(
        !failed.status.success(),
        "combined cherry-pick source should be invalid\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&failed.stdout),
        String::from_utf8_lossy(&failed.stderr)
    );

    raw_git_trace_to_file(
        &repo,
        &["-c", &good_session_arg, "cherry-pick", &source_one],
        &good_trace,
    );
    let picked_commit = head_sha(&repo);

    replay_trace_file_to_daemon(&repo, &failed_trace);
    replay_trace_file_to_daemon(&repo, &good_trace);
    repo.sync_daemon_external_completion_sessions(&[failed_session, good_session]);

    assert_note_has_ai_for_file(&repo, &picked_commit, "file.txt");
    file.assert_committed_lines(lines!["base line".ai(), "AI line 1".ai()]);
}

#[test]
fn test_delayed_commit_trace_uses_committed_tree_not_later_worktree() {
    let repo = TestRepo::new();
    setup_initial_commit(&repo);
    let file_rel = "delayed-commit-race.txt";
    let file_path = repo.path().join(file_rel);

    fs::write(&file_path, "first ai\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", file_rel]).unwrap();
    repo.git_og(&["add", file_rel]).unwrap();
    repo.sync_daemon();

    let trace_dir = tempfile::tempdir().expect("trace temp dir");
    let commit_trace = trace_dir.path().join("commit.trace2");
    raw_git_trace_to_file(&repo, &["commit", "-m", "first ai"], &commit_trace);
    let first_commit = head_sha(&repo);

    fs::write(&file_path, "first ai\nsecond ai\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", file_rel]).unwrap();
    repo.sync_daemon();
    let baseline = repo.daemon_total_completion_count();

    replay_trace_file_to_daemon(&repo, &commit_trace);
    repo.wait_for_daemon_total_completion_count(baseline, baseline + 1);
    repo.sync_daemon();

    let mut file = repo.filename(file_rel);
    file.assert_committed_lines(lines!["first ai".ai()]);

    repo.stage_all_and_commit("second ai")
        .expect("second commit should succeed");
    file.assert_committed_lines(lines!["first ai".ai(), "second ai".ai()]);

    assert_note_has_ai_for_file(&repo, &first_commit, file_rel);
}

#[test]
fn test_commit_tree_update_ref_preserves_authorship_notes_on_reparent() {
    let repo = TestRepo::new();
    setup_initial_commit(&repo);

    repo.git(&["checkout", "-b", "feature"])
        .expect("checkout feature should succeed");

    let mut feature_file = repo.filename("feature.txt");
    feature_file.set_contents(lines!["human line", "ai line".ai()]);
    let feature_commit = repo
        .stage_all_and_commit("feature commit")
        .expect("feature commit should succeed");

    let git_ai_repo = open_repo(&repo);
    assert!(
        show_authorship_note(&git_ai_repo, &feature_commit.commit_sha).is_some(),
        "expected initial feature commit to have an authorship note",
    );

    repo.git(&["checkout", "main"])
        .expect("checkout main should succeed");
    let mut trunk_file = repo.filename("trunk.txt");
    trunk_file.set_contents(lines!["trunk update"]);
    let main_commit = repo
        .stage_all_and_commit("main update")
        .expect("main update should succeed");

    repo.git(&["checkout", "feature"])
        .expect("checkout feature should succeed");
    let (old_head, new_head) = commit_tree_rewrite_current_branch(
        &repo,
        "feature",
        &main_commit.commit_sha,
        "feature commit",
    );

    repo.sync_daemon();

    let git_ai_repo = open_repo(&repo);
    assert!(
        show_authorship_note(&git_ai_repo, &new_head).is_some(),
        "expected rewritten commit {} to preserve authorship note from {}",
        new_head,
        old_head,
    );

    let mut rewritten_file = repo.filename("feature.txt");
    rewritten_file.assert_lines_and_blame(lines!["human line".human(), "ai line".ai()]);
}

#[test]
fn test_commit_tree_update_ref_moves_working_log_to_rewritten_head() {
    let repo = TestRepo::new();
    setup_initial_commit(&repo);

    repo.git(&["checkout", "-b", "feature"])
        .expect("checkout feature should succeed");

    let mut feature_file = repo.filename("feature.txt");
    feature_file.set_contents(lines!["human line", "committed ai".ai()]);
    repo.stage_all_and_commit("feature commit")
        .expect("feature commit should succeed");

    repo.git(&["checkout", "main"])
        .expect("checkout main should succeed");
    let mut trunk_file = repo.filename("trunk.txt");
    trunk_file.set_contents(lines!["trunk update"]);
    let main_commit = repo
        .stage_all_and_commit("main update")
        .expect("main update should succeed");

    repo.git(&["checkout", "feature"])
        .expect("checkout feature should succeed");
    feature_file.set_contents_no_stage(lines![
        "human line",
        "committed ai".ai(),
        "pending ai".ai(),
    ]);

    repo.sync_daemon();

    let old_head = head_sha(&repo);
    let git_ai_repo = open_repo(&repo);
    assert!(
        git_ai_repo.storage.has_working_log(&old_head),
        "expected dirty branch to have a working log before rewrite",
    );

    let (_, new_head) = commit_tree_rewrite_current_branch(
        &repo,
        "feature",
        &main_commit.commit_sha,
        "feature commit",
    );

    repo.sync_daemon();

    let git_ai_repo = open_repo(&repo);
    assert!(
        git_ai_repo.storage.has_working_log(&new_head),
        "expected working log to follow rewritten HEAD from {} to {}",
        old_head,
        new_head,
    );
    assert!(
        !git_ai_repo.storage.has_working_log(&old_head),
        "expected working log for old HEAD {} to be renamed away",
        old_head,
    );

    repo.git(&["add", "-A"]).expect("git add should succeed");
    repo.commit("commit after plumbing rewrite")
        .expect("commit after plumbing rewrite should succeed");

    let mut rewritten_file = repo.filename("feature.txt");
    rewritten_file.assert_lines_and_blame(lines![
        "human line".human(),
        "committed ai".ai(),
        "pending ai".ai(),
    ]);
}

#[test]
fn test_reset_keep_rewrite_preserves_authorship_notes_on_current_branch() {
    let repo = TestRepo::new();
    setup_initial_commit(&repo);

    repo.git(&["checkout", "-b", "feature"])
        .expect("checkout feature should succeed");

    let mut feature_file = repo.filename("feature.txt");
    feature_file.set_contents(lines!["human line", "ai line".ai()]);
    let feature_commit = repo
        .stage_all_and_commit("feature commit")
        .expect("feature commit should succeed");

    let git_ai_repo = open_repo(&repo);
    assert!(
        show_authorship_note(&git_ai_repo, &feature_commit.commit_sha).is_some(),
        "expected initial feature commit to have an authorship note",
    );

    repo.git(&["checkout", "main"])
        .expect("checkout main should succeed");
    let mut trunk_file = repo.filename("trunk.txt");
    trunk_file.set_contents(lines!["trunk update"]);
    let main_commit = repo
        .stage_all_and_commit("main update")
        .expect("main update should succeed");

    repo.git(&["checkout", "feature"])
        .expect("checkout feature should succeed");
    let old_head = head_sha(&repo);
    let new_head =
        commit_tree_from_existing_tree(&repo, &old_head, &main_commit.commit_sha, "feature commit");

    repo.git(&["reset", "--keep", &new_head])
        .expect("git reset --keep should succeed");

    repo.sync_daemon();

    let git_ai_repo = open_repo(&repo);
    assert!(
        show_authorship_note(&git_ai_repo, &new_head).is_some(),
        "expected rewritten current-branch commit {} to preserve authorship note from {}",
        new_head,
        old_head,
    );

    let mut rewritten_file = repo.filename("feature.txt");
    rewritten_file.assert_lines_and_blame(lines!["human line".human(), "ai line".ai()]);
}

#[test]
fn test_update_ref_restack_after_parent_amend_preserves_child_attribution() {
    let repo = TestRepo::new();
    setup_initial_commit(&repo);

    repo.git(&["checkout", "-b", "parent"])
        .expect("checkout parent should succeed");
    let mut parent_file = repo.filename("parent.txt");
    parent_file.set_contents(lines!["parent ai".ai(), "parent human"]);
    let parent_commit = repo
        .stage_all_and_commit("parent")
        .expect("parent commit should succeed");

    repo.git(&["checkout", "-b", "child"])
        .expect("checkout child should succeed");
    let mut child_file = repo.filename("child.txt");
    child_file.set_contents(lines!["child ai".ai(), "child human"]);
    let child_commit = repo
        .stage_all_and_commit("child")
        .expect("child commit should succeed");

    let git_ai_repo = open_repo(&repo);
    assert!(
        show_authorship_note(&git_ai_repo, &child_commit.commit_sha).is_some(),
        "expected initial child commit to have an authorship note",
    );

    repo.git(&["checkout", "parent"])
        .expect("checkout parent should succeed");
    let mut parent_file2 = repo.filename("parent2.txt");
    parent_file2.set_contents(lines!["parent2 ai".ai()]);
    repo.git(&["add", "-A"]).expect("git add should succeed");
    repo.git(&["commit", "--amend", "-m", "modified parent"])
        .expect("git commit --amend should succeed");

    let amended_parent_head = head_sha(&repo);
    assert_ne!(
        amended_parent_head, parent_commit.commit_sha,
        "expected parent amend to rewrite the parent branch"
    );

    let new_child_head = graphite_style_restack_child_branch(
        &repo,
        "child",
        &child_commit.commit_sha,
        &amended_parent_head,
        "child",
    );

    repo.sync_daemon();

    let git_ai_repo = open_repo(&repo);
    assert!(
        show_authorship_note(&git_ai_repo, &new_child_head).is_some(),
        "expected rewritten child commit {} to preserve authorship note from {}",
        new_child_head,
        child_commit.commit_sha,
    );

    repo.git(&["checkout", "child"])
        .expect("checkout child should succeed");
    let mut rewritten_child_file = repo.filename("child.txt");
    rewritten_child_file.assert_lines_and_blame(lines!["child ai".ai(), "child human".human()]);
}

/// Test Graphite-style rebase: replay multiple feature commits via commit-tree,
/// then move the branch with ONE update-ref from old tip to new tip.
///
/// This matches actual `gt sync` behavior where Graphite replays all commits
/// using plumbing commands and issues a single atomic update-ref at the end.
/// git-ai must detect the N-commit rewrite and remap all N authorship notes.
#[test]
fn test_graphite_style_multi_commit_single_update_ref() {
    let repo = TestRepo::new();
    setup_initial_commit(&repo);
    let default_branch = repo.current_branch();

    // Create feature branch with 3 AI commits
    repo.git(&["checkout", "-b", "feature"])
        .expect("checkout feature");

    let mut file_a = repo.filename("a.txt");
    file_a.set_contents(lines!["a1 ai".ai(), "a2 human"]);
    repo.stage_all_and_commit("feat: add file a")
        .expect("feat 1");

    let mut file_b = repo.filename("b.txt");
    file_b.set_contents(lines!["b1 ai".ai(), "b2 ai".ai()]);
    repo.stage_all_and_commit("feat: add file b")
        .expect("feat 2");

    file_a.set_contents(lines!["a1 ai".ai(), "a2 human", "a3 ai".ai()]);
    repo.stage_all_and_commit("feat: extend file a")
        .expect("feat 3");

    // Collect feature commits (oldest to newest)
    let feature_commits_str = repo
        .git(&[
            "rev-list",
            "--reverse",
            &format!("{}..HEAD", default_branch),
        ])
        .expect("rev-list");
    let feature_commits: Vec<&str> = feature_commits_str
        .trim()
        .lines()
        .filter(|l| !l.is_empty())
        .collect();
    assert_eq!(feature_commits.len(), 3, "expected 3 feature commits");

    // Verify all 3 have authorship notes pre-rebase
    let git_ai_repo = open_repo(&repo);
    for &sha in &feature_commits {
        assert!(
            show_authorship_note(&git_ai_repo, sha).is_some(),
            "pre-rebase: commit {} should have authorship note",
            sha
        );
    }

    // Advance main so rebase has new base
    repo.git(&["checkout", &default_branch])
        .expect("checkout main");
    let mut trunk = repo.filename("trunk.txt");
    trunk.set_contents(lines!["trunk line 1"]);
    repo.stage_all_and_commit("main advance 1").expect("main 1");
    trunk.set_contents(lines!["trunk line 1", "trunk line 2"]);
    repo.stage_all_and_commit("main advance 2").expect("main 2");
    let main_tip = head_sha(&repo);

    // Switch back to feature for the replay
    repo.git(&["checkout", "feature"])
        .expect("checkout feature");
    let old_tip = head_sha(&repo);

    // Replay all commits via commit-tree (no update-ref yet)
    let mut new_parent = main_tip.clone();
    for &feature_sha in &feature_commits {
        let old_parent = repo
            .git(&["rev-parse", &format!("{}^", feature_sha)])
            .expect("rev-parse parent")
            .trim()
            .to_string();

        let merged_tree_output = repo
            .git(&[
                "merge-tree",
                "--write-tree",
                "--merge-base",
                &old_parent,
                &new_parent,
                feature_sha,
            ])
            .expect("merge-tree");
        let merged_tree = merged_tree_output
            .trim()
            .lines()
            .next()
            .unwrap()
            .to_string();

        let message = repo
            .git(&["log", "-1", "--format=%s", feature_sha])
            .expect("log message")
            .trim()
            .to_string();

        let new_commit = repo
            .git(&[
                "commit-tree",
                &merged_tree,
                "-p",
                &new_parent,
                "-m",
                &message,
            ])
            .expect("commit-tree")
            .trim()
            .to_string();

        new_parent = new_commit;
    }

    // ONE atomic update-ref (matches Graphite's actual behavior)
    let new_tip = new_parent;
    repo.git(&["update-ref", "refs/heads/feature", &new_tip, &old_tip])
        .expect("update-ref");
    repo.git(&["reset", "--hard", &new_tip]).expect("reset");

    repo.sync_daemon();

    // Verify all 3 rebased commits have authorship notes
    let rebased_commits_str = repo
        .git(&["rev-list", "--reverse", &format!("{}..HEAD", main_tip)])
        .expect("rev-list rebased");
    let rebased_commits: Vec<&str> = rebased_commits_str
        .trim()
        .lines()
        .filter(|l| !l.is_empty())
        .collect();
    assert_eq!(rebased_commits.len(), 3, "expected 3 rebased commits");

    let git_ai_repo = open_repo(&repo);
    for (idx, &sha) in rebased_commits.iter().enumerate() {
        assert!(
            show_authorship_note(&git_ai_repo, sha).is_some(),
            "post-rebase: rebased commit {} (index {}) should have authorship note",
            sha,
            idx
        );
    }

    // Verify attribution on file_b (single-commit, straightforward)
    file_b.assert_lines_and_blame(lines!["b1 ai".ai(), "b2 ai".ai()]);
}

#[test]
fn test_update_ref_head_with_new_content_then_amend_preserves_attribution() {
    use std::fs;

    let repo = TestRepo::new();
    setup_initial_commit(&repo);

    let file_path = repo.path().join("feature.txt");

    // Write AI content and checkpoint
    fs::write(&file_path, "ai line 1\nai line 2\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "feature.txt"])
        .unwrap();

    // Stage
    repo.git(&["add", "-A"]).unwrap();

    // Plumbing: write-tree, commit-tree, update-ref HEAD
    let parent_sha = head_sha(&repo);
    let tree_sha = repo.git(&["write-tree"]).unwrap().trim().to_string();
    let commit_sha = repo
        .git(&[
            "commit-tree",
            &tree_sha,
            "-p",
            &parent_sha,
            "-m",
            "plumbing commit",
        ])
        .unwrap()
        .trim()
        .to_string();
    repo.git(&["update-ref", "HEAD", &commit_sha, &parent_sha])
        .unwrap();

    let mut feature_file = repo.filename("feature.txt");
    feature_file.assert_lines_and_blame(lines!["ai line 1".ai(), "ai line 2".ai()]);
}
