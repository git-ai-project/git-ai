use crate::repos::test_repo::{TestRepo, real_git_executable};
use serde_json::json;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

fn set_sharded_notes(repo: &mut TestRepo, enabled: bool) {
    repo.patch_git_ai_config(|patch| {
        let mut flags = match patch.feature_flags.take() {
            Some(existing) if existing.is_object() => existing,
            _ => json!({}),
        };
        let flags_obj = flags
            .as_object_mut()
            .expect("feature_flags value should be an object");
        flags_obj.insert("sharded_notes".to_string(), json!(enabled));
        patch.feature_flags = Some(flags);
    });
}

fn enable_sharded_notes(repo: &mut TestRepo) {
    set_sharded_notes(repo, true);
}

fn disable_sharded_notes(repo: &mut TestRepo) {
    set_sharded_notes(repo, false);
}

fn git_ref_exists(repo_path: &Path, ref_name: &str) -> bool {
    Command::new("git")
        .args(["-C", repo_path.to_str().unwrap()])
        .args(["show-ref", "--verify", "--quiet", ref_name])
        .status()
        .expect("git should run")
        .success()
}

#[test]
fn test_effective_ignore_patterns_internal_command_json() {
    let repo = TestRepo::new();

    fs::write(
        repo.path().join(".gitattributes"),
        "generated/** linguist-generated=true\n",
    )
    .expect("should write .gitattributes");
    fs::write(repo.path().join(".git-ai-ignore"), "custom/**\n")
        .expect("should write .git-ai-ignore");
    fs::write(repo.path().join("README.md"), "# repo\n").expect("should write README");
    repo.stage_all_and_commit("initial")
        .expect("initial commit");

    let request = json!({
        "user_patterns": ["user/**", "generated/**"],
        "extra_patterns": ["extra/**", "custom/**"]
    })
    .to_string();

    let output = repo
        .git_ai(&["effective-ignore-patterns", "--json", &request])
        .expect("internal command should succeed");
    let parsed: serde_json::Value = serde_json::from_str(output.trim()).expect("valid JSON output");

    let patterns = parsed["patterns"]
        .as_array()
        .expect("patterns should be an array")
        .iter()
        .map(|v| v.as_str().expect("pattern should be a string"))
        .collect::<Vec<_>>();

    assert!(patterns.contains(&"*.lock"));
    assert!(patterns.contains(&"generated/**"));
    assert!(patterns.contains(&"custom/**"));
    assert!(patterns.contains(&"extra/**"));
    assert!(patterns.contains(&"user/**"));

    let generated_count = patterns
        .iter()
        .filter(|pattern| **pattern == "generated/**")
        .count();
    assert_eq!(generated_count, 1);
}

#[test]
fn test_blame_analysis_internal_command_json() {
    let repo = TestRepo::new();

    fs::write(repo.path().join("analysis.txt"), "line1\nline2\nline3\n")
        .expect("should write analysis file");
    repo.stage_all_and_commit("initial")
        .expect("initial commit");

    let request = json!({
        "file_path": "analysis.txt",
        "options": {
            "line_ranges": [[2, 3]],
            "return_human_authors_as_human": true,
            "split_hunks_by_ai_author": false
        }
    })
    .to_string();

    let output = repo
        .git_ai(&["blame-analysis", "--json", &request])
        .expect("internal command should succeed");
    let parsed: serde_json::Value = serde_json::from_str(output.trim()).expect("valid JSON output");

    let line_authors = parsed["line_authors"]
        .as_object()
        .expect("line_authors should be an object");
    assert_eq!(line_authors.len(), 2);
    assert_eq!(
        line_authors.get("2").and_then(|v| v.as_str()),
        Some("human")
    );
    assert_eq!(
        line_authors.get("3").and_then(|v| v.as_str()),
        Some("human")
    );

    assert!(
        parsed["prompt_records"]
            .as_object()
            .expect("prompt_records should be object")
            .is_empty()
    );
    assert!(
        !parsed["blame_hunks"]
            .as_array()
            .expect("blame_hunks should be array")
            .is_empty()
    );
}

#[test]
fn test_internal_machine_commands_emit_json_errors() {
    let repo = TestRepo::new();

    let err = repo
        .git_ai(&["effective-ignore-patterns"])
        .expect_err("missing --json payload should fail");

    let parsed: serde_json::Value = serde_json::from_str(err.trim()).expect("error should be JSON");
    assert!(parsed["error"].as_str().is_some());
}

#[test]
fn test_fetch_and_push_authorship_notes_internal_commands_json() {
    let (mirror, _upstream) = TestRepo::new_with_remote();

    fs::write(mirror.path().join("sync.txt"), "sync authorship notes\n")
        .expect("should write sync file");
    mirror
        .stage_all_and_commit("create note source")
        .expect("commit should succeed");

    let request = json!({
        "remote_name": "origin"
    })
    .to_string();

    let fetch_before = mirror
        .git_ai(&["fetch-authorship-notes", "--json", &request])
        .expect("fetch command should succeed");
    let fetch_before_json: serde_json::Value =
        serde_json::from_str(fetch_before.trim()).expect("fetch output should be JSON");
    assert_eq!(fetch_before_json["notes_existence"], "not_found");

    let push_output = mirror
        .git_ai(&["push-authorship-notes", "--json", &request])
        .expect("push command should succeed");
    let push_json: serde_json::Value =
        serde_json::from_str(push_output.trim()).expect("push output should be JSON");
    assert_eq!(push_json["ok"], true);

    let fetch_after = mirror
        .git_ai(&["fetch_authorship_notes", "--json", &request])
        .expect("fetch alias command should succeed");
    let fetch_after_json: serde_json::Value =
        serde_json::from_str(fetch_after.trim()).expect("fetch output should be JSON");
    assert_eq!(fetch_after_json["notes_existence"], "found");
}

/// Helper to run a raw git command with stdin piped, returning trimmed stdout.
fn git_plumbing(repo_path: &std::path::Path, args: &[&str], stdin_data: Option<&[u8]>) -> String {
    let git = real_git_executable();
    let mut cmd = Command::new(git);
    cmd.arg("-C")
        .arg(repo_path)
        .arg("-c")
        .arg("core.hooksPath=/dev/null")
        .arg("-c")
        .arg("user.name=Test")
        .arg("-c")
        .arg("user.email=test@test.com")
        .args(args);
    if stdin_data.is_some() {
        cmd.stdin(Stdio::piped());
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

    let mut child = cmd.spawn().expect("failed to spawn git plumbing command");

    if let Some(data) = stdin_data {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(data)
            .expect("failed to write stdin");
    }

    let output = child.wait_with_output().expect("failed to wait for git");
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("non-utf8 git output")
        .trim()
        .to_string()
}

/// Reproduces a bug where `git notes merge -s ours` crashes with:
///   Assertion failed: (is_null_oid(&mp->remote)), function diff_tree_remote,
///   file notes-merge.c, line 170.
///
/// This happens when the remote notes tree has mixed fanout — both a flat blob
/// entry (e.g. `aabbccdd…`) AND a subtree entry (e.g. `aa/bbccdd…`) for the
/// same annotated object. The fallback merge should handle this gracefully and
/// the push should succeed.
#[test]
fn test_push_authorship_notes_survives_corrupted_remote_notes_tree() {
    let (mirror, upstream) = TestRepo::new_with_remote();

    // 1. Create a commit on mirror and push it
    fs::write(mirror.path().join("test.txt"), "hello\n").expect("write file");
    let commit = mirror
        .stage_all_and_commit("initial commit")
        .expect("commit should succeed");
    mirror
        .git(&["push", "origin", "main"])
        .expect("push should succeed");
    let commit_sha = commit.commit_sha;

    // 2. Create a corrupted notes tree on upstream with NO common merge base
    //    relative to the mirror's local notes ref. This is the key condition that
    //    triggers the assertion in git's notes-merge.c: when there's no merge base,
    //    git uses an empty tree as the base, and the diff against the corrupted remote
    //    tree encounters the same annotated object twice (once flat, once fanout).
    let prefix = &commit_sha[..2];
    let rest = &commit_sha[2..];

    // Create a note blob on upstream (independent of mirror's notes)
    let note_blob = git_plumbing(
        upstream.path(),
        &["hash-object", "-w", "--stdin"],
        Some(br#"{"author":"remote"}"#),
    );

    // Build inner tree (fanout: prefix/rest -> blob)
    let inner_tree_input = format!("100644 blob {}\t{}\n", note_blob, rest);
    let inner_tree = git_plumbing(
        upstream.path(),
        &["mktree"],
        Some(inner_tree_input.as_bytes()),
    );

    // Build mixed tree: flat entry + subtree entry for same commit
    let mixed_tree_input = format!(
        "100644 blob {}\t{}\n040000 tree {}\t{}\n",
        note_blob, commit_sha, inner_tree, prefix
    );
    let mixed_tree = git_plumbing(
        upstream.path(),
        &["mktree"],
        Some(mixed_tree_input.as_bytes()),
    );

    // Create a root commit (NO parent) — this ensures no common merge base
    // with the mirror's notes ref, which is what triggers the assertion.
    let corrupted_commit = git_plumbing(
        upstream.path(),
        &[
            "commit-tree",
            &mixed_tree,
            "-m",
            "corrupted notes tree (orphan)",
        ],
        None,
    );
    git_plumbing(
        upstream.path(),
        &["update-ref", "refs/notes/ai", &corrupted_commit],
        None,
    );

    // 4. Add a new local note on mirror (so local and remote have diverged).
    //    The git-ai commit hook automatically creates notes, so just making
    //    a new commit is sufficient.
    fs::write(mirror.path().join("test2.txt"), "world\n").expect("write file2");
    let commit2 = mirror
        .stage_all_and_commit("second commit")
        .expect("second commit should succeed");

    // 5. Push authorship notes — this triggers fetch + merge + push.
    //    Without the fallback fix, the merge crashes and the push fails
    //    with "non-fast-forward".
    let request = json!({"remote_name": "origin"}).to_string();
    let push_output = mirror
        .git_ai(&["push-authorship-notes", "--json", &request])
        .expect("push-authorship-notes should succeed despite corrupted remote tree");
    let push_json: serde_json::Value =
        serde_json::from_str(push_output.trim()).expect("push output should be JSON");
    assert_eq!(
        push_json["ok"],
        true,
        "push should succeed via fallback merge, got: {}",
        push_output.trim()
    );

    // 6. Verify both notes are present on upstream after push
    let notes_list = git_plumbing(upstream.path(), &["notes", "--ref=ai", "list"], None);
    assert!(
        notes_list.contains(&commit_sha),
        "upstream should have note for first commit"
    );
    assert!(
        notes_list.contains(&commit2.commit_sha),
        "upstream should have note for second commit"
    );
}

/// Simulates the race condition on busy monorepos where another developer
/// pushes notes between our fetch-merge and push steps, causing a
/// non-fast-forward rejection. The retry loop should re-fetch, re-merge,
/// and push successfully.
#[test]
fn test_push_authorship_notes_retries_on_concurrent_push() {
    let (mirror, upstream) = TestRepo::new_with_remote();

    // 1. Create initial commit and push
    fs::write(mirror.path().join("a.txt"), "a\n").expect("write a");
    let commit1 = mirror
        .stage_all_and_commit("first commit")
        .expect("commit1");
    mirror.git(&["push", "origin", "main"]).expect("push main");

    // 2. Push mirror's initial notes to upstream
    mirror
        .git_og(&["push", "origin", "refs/notes/ai:refs/notes/ai"])
        .expect("push initial notes");

    // 3. Create a second clone that simulates the concurrent pusher
    let clone2_path = std::env::temp_dir().join(format!("concurrent-clone-{}", std::process::id()));
    let _ = fs::remove_dir_all(&clone2_path);
    git_plumbing(
        mirror.path(),
        &[
            "clone",
            upstream.path().to_str().unwrap(),
            clone2_path.to_str().unwrap(),
        ],
        None,
    );
    // Configure clone2 and fetch notes
    git_plumbing(
        &clone2_path,
        &["config", "user.email", "other@test.com"],
        None,
    );
    git_plumbing(&clone2_path, &["config", "user.name", "Other"], None);
    git_plumbing(
        &clone2_path,
        &["fetch", "origin", "+refs/notes/ai:refs/notes/ai"],
        None,
    );

    // 4. Other clone makes a commit with a note and pushes notes to upstream.
    //    This advances remote refs/notes/ai beyond what mirror has fetched.
    fs::write(clone2_path.join("b.txt"), "b\n").expect("write b");
    git_plumbing(&clone2_path, &["add", "b.txt"], None);
    git_plumbing(&clone2_path, &["commit", "-m", "other commit"], None);
    let other_sha = git_plumbing(&clone2_path, &["rev-parse", "HEAD"], None);
    git_plumbing(
        &clone2_path,
        &[
            "notes",
            "--ref=ai",
            "add",
            "-m",
            r#"{"author":"other"}"#,
            &other_sha,
        ],
        None,
    );
    git_plumbing(
        &clone2_path,
        &["push", "origin", "refs/notes/ai:refs/notes/ai"],
        None,
    );

    // 5. Mirror makes another commit (notes auto-created by hook).
    //    Mirror's local refs/notes/ai is now behind remote.
    fs::write(mirror.path().join("c.txt"), "c\n").expect("write c");
    let _commit3 = mirror
        .stage_all_and_commit("mirror commit")
        .expect("commit3");

    // 6. Push authorship notes. The retry loop should:
    //    - Attempt 1: fetch, merge, push → fails (non-fast-forward if
    //      remote was updated between merge and push, or succeeds on first try)
    //    - Attempt 2+: re-fetch, re-merge, push → succeeds
    //    In this test, the remote is already ahead, so the first attempt's
    //    fetch+merge will incorporate the other clone's notes, and push succeeds.
    let request = json!({"remote_name": "origin"}).to_string();
    let push_output = mirror
        .git_ai(&["push-authorship-notes", "--json", &request])
        .expect("push-authorship-notes should succeed after retry");
    let push_json: serde_json::Value =
        serde_json::from_str(push_output.trim()).expect("push output should be JSON");
    assert_eq!(
        push_json["ok"],
        true,
        "push should eventually succeed, got: {}",
        push_output.trim()
    );

    // 7. Verify all notes are present on upstream
    let notes_list = git_plumbing(upstream.path(), &["notes", "--ref=ai", "list"], None);
    assert!(
        notes_list.contains(&commit1.commit_sha),
        "upstream should have note for mirror's first commit"
    );
    assert!(
        notes_list.contains(&other_sha),
        "upstream should have note from concurrent pusher"
    );
}

/// With sharded_notes enabled, push-authorship-notes writes both legacy refs/notes/ai
/// AND shard refs (refs/notes/ai-s/XX) to the upstream.
#[test]
fn test_sharded_notes_dual_write_on_push() {
    let (mut mirror, upstream) = TestRepo::new_with_remote();
    enable_sharded_notes(&mut mirror);

    fs::write(mirror.path().join("shard_test.txt"), "sharded notes test\n")
        .expect("should write file");
    let commit = mirror
        .stage_all_and_commit("add shard test file")
        .expect("commit should succeed");

    let request = json!({ "remote_name": "origin" }).to_string();

    let push_output = mirror
        .git_ai(&["push-authorship-notes", "--json", &request])
        .expect("push should succeed with sharded notes");
    let push_json: serde_json::Value =
        serde_json::from_str(push_output.trim()).expect("valid JSON");
    assert_eq!(push_json["ok"], true);

    // Verify legacy ref exists on upstream
    let legacy_note = git_plumbing(
        upstream.path(),
        &["notes", "--ref=ai", "show", &commit.commit_sha],
        None,
    );
    assert!(
        !legacy_note.trim().is_empty(),
        "legacy notes ref should have the note on upstream"
    );

    // Verify shard ref exists on upstream
    let shard_suffix = &commit.commit_sha[..2];
    let shard_ref = format!("ai-s/{}", shard_suffix);
    let shard_note = git_plumbing(
        upstream.path(),
        &[
            "notes",
            &format!("--ref={}", shard_ref),
            "show",
            &commit.commit_sha,
        ],
        None,
    );
    assert!(
        !shard_note.trim().is_empty(),
        "shard ref {} should have the note on upstream",
        shard_ref
    );

    // Both should have identical content
    assert_eq!(legacy_note.trim(), shard_note.trim());
}

/// A non-sharded client writes notes. A sharded client can read them via union-read
/// (falls back to legacy ref when shard ref doesn't exist).
#[test]
fn test_sharded_notes_union_read_legacy_fallback() {
    let (mut mirror, _upstream) = TestRepo::new_with_remote();

    fs::write(mirror.path().join("legacy.txt"), "legacy only write\n").expect("should write file");
    let commit = mirror
        .stage_all_and_commit("legacy commit")
        .expect("commit should succeed");

    // Push WITHOUT sharded notes (legacy-only write)
    let request = json!({ "remote_name": "origin" }).to_string();
    mirror
        .git_ai(&["push-authorship-notes", "--json", &request])
        .expect("legacy push should succeed");

    // Verify legacy note exists locally
    let legacy_note = git_plumbing(
        mirror.path(),
        &["notes", "--ref=ai", "show", &commit.commit_sha],
        None,
    );
    assert!(!legacy_note.trim().is_empty(), "legacy note should exist");

    // Verify NO shard ref exists
    let shard_suffix = &commit.commit_sha[..2];
    let shard_ref_full = format!("refs/notes/ai-s/{}", shard_suffix);
    assert!(
        !git_ref_exists(mirror.path(), &shard_ref_full),
        "shard ref should NOT exist after legacy-only write"
    );

    // Now do a blame-analysis with sharded notes enabled — it should still find the note
    // via legacy fallback
    let blame_request = json!({
        "file_path": "legacy.txt",
        "options": {
            "line_ranges": [[1, 1]],
            "return_human_authors_as_human": true,
            "split_hunks_by_ai_author": false
        }
    })
    .to_string();

    enable_sharded_notes(&mut mirror);
    let blame_output = mirror
        .git_ai(&["blame-analysis", "--json", &blame_request])
        .expect("blame with sharded notes should succeed");
    let blame_json: serde_json::Value =
        serde_json::from_str(blame_output.trim()).expect("valid blame JSON");

    // The blame should find the note (via legacy fallback) and report line authors
    let line_authors = blame_json["line_authors"]
        .as_object()
        .expect("line_authors should be present");
    assert!(
        !line_authors.is_empty(),
        "blame should find authorship via legacy fallback"
    );
}

/// Mixed-team scenario: one clone pushes with sharding, another without.
/// Both should be able to read each other's notes after fetch.
#[test]
fn test_sharded_notes_mixed_team_interop() {
    let (mut mirror, upstream) = TestRepo::new_with_remote();
    enable_sharded_notes(&mut mirror);

    // Create a second clone of the same upstream
    let base = std::env::temp_dir();
    let clone2_path = base.join(format!("sharded-clone2-{}", std::process::id()));
    let _ = fs::remove_dir_all(&clone2_path);
    let clone_output = Command::new("git")
        .args([
            "clone",
            upstream.path().to_str().unwrap(),
            clone2_path.to_str().unwrap(),
        ])
        .output()
        .expect("clone should succeed");
    assert!(clone_output.status.success(), "clone2 should succeed");

    // Mirror (sharded client) creates a commit and pushes notes
    fs::write(
        mirror.path().join("from_sharded.txt"),
        "from sharded client\n",
    )
    .expect("write file");
    let sharded_commit = mirror
        .stage_all_and_commit("sharded client commit")
        .expect("commit should succeed");
    mirror
        .git_og(&["push", "origin", "HEAD"])
        .expect("push code should succeed");

    let request = json!({ "remote_name": "origin" }).to_string();
    mirror
        .git_ai(&["push-authorship-notes", "--json", &request])
        .expect("sharded push should succeed");

    // Clone2 (non-sharded client) fetches and reads the note via legacy ref
    let _ = Command::new("git")
        .args([
            "-C",
            clone2_path.to_str().unwrap(),
            "pull",
            "origin",
            "main",
        ])
        .output();
    let _ = git_plumbing(
        &clone2_path,
        &["fetch", "origin", "+refs/notes/ai:refs/notes/ai"],
        None,
    );
    let note_content = git_plumbing(
        &clone2_path,
        &["notes", "--ref=ai", "show", &sharded_commit.commit_sha],
        None,
    );
    assert!(
        !note_content.trim().is_empty(),
        "non-sharded client should read notes via legacy ref"
    );

    // Clean up
    let _ = fs::remove_dir_all(&clone2_path);
}

#[test]
fn test_non_sharded_push_does_not_push_existing_shard_refs() {
    let (mut mirror, upstream) = TestRepo::new_with_remote();
    enable_sharded_notes(&mut mirror);

    fs::write(
        mirror.path().join("local-shard-only.txt"),
        "local shard should stay local when flag is off\n",
    )
    .expect("should write file");
    let commit = mirror
        .stage_all_and_commit("create local shard ref")
        .expect("commit should succeed");
    mirror
        .git_og(&["push", "origin", "HEAD"])
        .expect("push code should succeed");

    let shard_ref_full = format!("refs/notes/ai-s/{}", &commit.commit_sha[..2]);
    assert!(
        git_ref_exists(mirror.path(), &shard_ref_full),
        "local shard ref should exist before disabling the flag"
    );
    assert!(
        !git_ref_exists(upstream.path(), &shard_ref_full),
        "upstream should not have the shard ref before notes are pushed"
    );

    disable_sharded_notes(&mut mirror);

    let request = json!({ "remote_name": "origin" }).to_string();
    let push_output = mirror
        .git_ai(&["push-authorship-notes", "--json", &request])
        .expect("legacy-only push should succeed");
    let push_json: serde_json::Value =
        serde_json::from_str(push_output.trim()).expect("valid JSON");
    assert_eq!(push_json["ok"], true);

    let legacy_note = git_plumbing(
        upstream.path(),
        &["notes", "--ref=ai", "show", &commit.commit_sha],
        None,
    );
    assert!(
        !legacy_note.trim().is_empty(),
        "legacy note should still be pushed with the flag disabled"
    );
    assert!(
        !git_ref_exists(upstream.path(), &shard_ref_full),
        "default-off push should not propagate shard refs that already exist locally"
    );
}

#[test]
fn test_non_sharded_fetch_does_not_fetch_shards_from_upstream() {
    let (mut mirror, upstream) = TestRepo::new_with_remote();
    enable_sharded_notes(&mut mirror);

    fs::write(
        mirror.path().join("remote-shard.txt"),
        "remote shard should stay remote when flag is off\n",
    )
    .expect("should write file");
    let commit = mirror
        .stage_all_and_commit("create upstream shard ref")
        .expect("commit should succeed");
    mirror
        .git_og(&["push", "origin", "HEAD"])
        .expect("push code should succeed");

    let request = json!({ "remote_name": "origin" }).to_string();
    mirror
        .git_ai(&["push-authorship-notes", "--json", &request])
        .expect("sharded push should succeed");

    let shard_ref_full = format!("refs/notes/ai-s/{}", &commit.commit_sha[..2]);
    assert!(
        git_ref_exists(upstream.path(), &shard_ref_full),
        "upstream should have the shard ref before the non-sharded fetch"
    );

    let base = std::env::temp_dir();
    let clone2_path = base.join(format!("non-sharded-fetch-clone-{}", std::process::id()));
    let _ = fs::remove_dir_all(&clone2_path);

    let clone_output = Command::new("git")
        .args([
            "clone",
            upstream.path().to_str().unwrap(),
            clone2_path.to_str().unwrap(),
        ])
        .output()
        .expect("clone should succeed");
    assert!(clone_output.status.success(), "clone2 should succeed");

    let clone2 = TestRepo::new_at_path_with_mode(&clone2_path, mirror.mode());
    let fetch_output = clone2
        .git_ai(&["fetch-authorship-notes", "--json", &request])
        .expect("legacy-only fetch should succeed");
    let fetch_json: serde_json::Value =
        serde_json::from_str(fetch_output.trim()).expect("valid JSON");
    assert_eq!(fetch_json["notes_existence"], "found");

    let legacy_note = git_plumbing(
        clone2.path(),
        &["notes", "--ref=ai", "show", &commit.commit_sha],
        None,
    );
    assert!(
        !legacy_note.trim().is_empty(),
        "legacy note should still be fetched with the flag disabled"
    );
    assert!(
        !git_ref_exists(clone2.path(), &shard_ref_full),
        "default-off fetch should not materialize shard refs locally"
    );

    drop(clone2);
    let _ = fs::remove_dir_all(&clone2_path);
}
