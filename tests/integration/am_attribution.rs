use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;

/// Test that `git format-patch` embeds an X-Git-AI-Attribution header
/// in the patch file when the commit has an AI attribution note.
#[test]
fn test_format_patch_embeds_attribution_header() {
    let source = TestRepo::new();

    // format-patch/am hooks only run in wrapper (non-async) mode
    if source.mode().uses_daemon() {
        return;
    }

    // Create initial commit
    let mut file = source.filename("file.txt");
    file.set_contents(crate::lines!["Initial content"]);
    source.stage_all_and_commit("Initial commit").unwrap();

    // Create an AI-authored commit
    file.insert_at(1, crate::lines!["AI generated line".ai()]);
    source.stage_all_and_commit("Add AI content").unwrap();

    // Verify the source commit has AI attribution
    let source_head = source
        .git(&["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();
    let source_note = source
        .read_authorship_note(&source_head)
        .expect("source commit should have attribution note");
    assert!(
        !source_note.is_empty(),
        "source attribution note should not be empty"
    );

    // Export the last commit as a patch
    let patch_dir = source.path().join("patches");
    std::fs::create_dir_all(&patch_dir).unwrap();
    source
        .git(&["format-patch", "HEAD~1", "-o", patch_dir.to_str().unwrap()])
        .unwrap();

    // Read the patch file and verify it contains the X-Git-AI-Attribution header
    let patch_files: Vec<_> = std::fs::read_dir(&patch_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "patch"))
        .collect();
    assert_eq!(patch_files.len(), 1, "Should have exactly one patch file");

    let patch_content = std::fs::read_to_string(patch_files[0].path()).unwrap();
    assert!(
        patch_content.contains("X-Git-AI-Attribution:"),
        "Patch file should contain X-Git-AI-Attribution header.\nPatch content:\n{}",
        patch_content
    );
}

/// Test that `git am` preserves AI attribution when applying a patch
/// that has the X-Git-AI-Attribution header.
#[test]
fn test_am_preserves_attribution_via_patch_header() {
    let source = TestRepo::new();

    // format-patch/am hooks only run in wrapper (non-async) mode
    if source.mode().uses_daemon() {
        return;
    }

    // Create initial commit in source repo
    let mut file = source.filename("file.txt");
    file.set_contents(crate::lines!["Initial content"]);
    source.stage_all_and_commit("Initial commit").unwrap();

    // Create an AI-authored commit
    file.insert_at(1, crate::lines!["AI generated line".ai()]);
    source.stage_all_and_commit("Add AI content").unwrap();

    // Get source commit stats for comparison
    let source_stats = source.stats().unwrap();
    assert!(
        source_stats.ai_additions > 0,
        "Source commit should have AI additions"
    );

    // Export the last commit as a patch
    let patch_dir = source.path().join("patches");
    std::fs::create_dir_all(&patch_dir).unwrap();
    source
        .git(&["format-patch", "HEAD~1", "-o", patch_dir.to_str().unwrap()])
        .unwrap();

    // Verify the header was embedded
    let patch_files: Vec<_> = std::fs::read_dir(&patch_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "patch"))
        .collect();
    assert_eq!(patch_files.len(), 1);
    let patch_path = patch_files[0].path();

    // Create a destination repo with the same initial state
    let dest = TestRepo::new();
    let mut dest_file = dest.filename("file.txt");
    dest_file.set_contents(crate::lines!["Initial content"]);
    dest.stage_all_and_commit("Initial commit").unwrap();

    // Apply the patch with git am
    dest.git(&["am", patch_path.to_str().unwrap()]).unwrap();

    // Verify the destination commit has AI attribution
    let dest_head = dest.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    let dest_note = dest
        .read_authorship_note(&dest_head)
        .expect("destination commit should have attribution note after git am");

    // Parse and verify the attribution
    let dest_log =
        git_ai::authorship::authorship_log_serialization::AuthorshipLog::deserialize_from_string(
            &dest_note,
        )
        .expect("should parse destination attribution note");

    assert!(
        !dest_log.metadata.prompts.is_empty(),
        "Destination commit should have prompt records from source"
    );

    // Verify stats match
    let dest_stats = dest.stats().unwrap();
    assert_eq!(
        dest_stats.ai_additions, source_stats.ai_additions,
        "Destination ai_additions should match source"
    );
}

/// Test that `git am` works gracefully on a plain patch without the
/// X-Git-AI-Attribution header (should not crash, ai_additions=0).
#[test]
fn test_am_without_header_shows_zero_ai() {
    // Create a source repo to generate a valid patch (without AI attribution header)
    let source = TestRepo::new();

    // format-patch/am hooks only run in wrapper (non-async) mode
    if source.mode().uses_daemon() {
        return;
    }

    let mut src_file = source.filename("file.txt");
    src_file.set_contents(crate::lines!["Initial content"]);
    source.stage_all_and_commit("Initial commit").unwrap();

    // Add a human-only line
    src_file.insert_at(1, crate::lines!["Plain human line".human()]);
    source.stage_all_and_commit("Add plain line").unwrap();

    // Export as patch
    let patch_dir = source.path().join("patches");
    std::fs::create_dir_all(&patch_dir).unwrap();
    source
        .git(&["format-patch", "HEAD~1", "-o", patch_dir.to_str().unwrap()])
        .unwrap();

    let patch_files: Vec<_> = std::fs::read_dir(&patch_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "patch"))
        .map(|e| e.path())
        .collect();
    assert_eq!(patch_files.len(), 1);
    let patch_path = &patch_files[0];

    // Strip any X-Git-AI-Attribution header that might have been added
    let content = std::fs::read_to_string(patch_path).unwrap();
    let filtered: Vec<&str> = content
        .lines()
        .filter(|line| !line.starts_with("X-Git-AI-Attribution:"))
        .collect();
    std::fs::write(patch_path, filtered.join("\n") + "\n").unwrap();

    // Verify the header was removed
    let content = std::fs::read_to_string(patch_path).unwrap();
    assert!(
        !content.contains("X-Git-AI-Attribution:"),
        "Patch should not contain attribution header after stripping"
    );

    // Create a destination repo and apply the plain patch
    let dest = TestRepo::new();
    let mut dest_file = dest.filename("file.txt");
    dest_file.set_contents(crate::lines!["Initial content"]);
    dest.stage_all_and_commit("Initial commit").unwrap();
    let initial_sha = dest.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();

    // Apply the patch with git am — should not crash
    let result = dest.git(&["am", patch_path.to_str().unwrap()]);
    assert!(
        result.is_ok(),
        "git am should succeed on a plain patch: {:?}",
        result
    );

    // Verify the commit was created
    let head = dest.git(&["rev-parse", "HEAD"]).unwrap().trim().to_string();
    assert_ne!(head, initial_sha, "HEAD should have changed");

    // ai_additions should be 0 for a plain patch without attribution header
    let stats = dest.stats().unwrap();
    assert_eq!(
        stats.ai_additions, 0,
        "Plain patch should have 0 AI additions"
    );
}

/// Test that multiple patches exported with format-patch all get attribution
/// headers and that git am preserves all of them.
#[test]
fn test_am_preserves_attribution_multiple_patches() {
    let source = TestRepo::new();

    // format-patch/am hooks only run in wrapper (non-async) mode
    if source.mode().uses_daemon() {
        return;
    }

    // Create initial commit
    let mut file = source.filename("file.txt");
    file.set_contents(crate::lines!["Initial content"]);
    source.stage_all_and_commit("Initial commit").unwrap();

    // Create first AI commit
    file.insert_at(1, crate::lines!["AI line 1".ai()]);
    source.stage_all_and_commit("Add AI line 1").unwrap();

    // Create second AI commit
    file.insert_at(2, crate::lines!["AI line 2".ai()]);
    source.stage_all_and_commit("Add AI line 2").unwrap();

    // Export both commits as patches
    let patch_dir = source.path().join("patches");
    std::fs::create_dir_all(&patch_dir).unwrap();
    source
        .git(&["format-patch", "HEAD~2", "-o", patch_dir.to_str().unwrap()])
        .unwrap();

    // Verify we got 2 patch files
    let mut patch_files: Vec<_> = std::fs::read_dir(&patch_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "patch"))
        .map(|e| e.path())
        .collect();
    patch_files.sort();
    assert_eq!(patch_files.len(), 2, "Should have exactly two patch files");

    // Verify both have attribution headers
    for patch_file in &patch_files {
        let content = std::fs::read_to_string(patch_file).unwrap();
        assert!(
            content.contains("X-Git-AI-Attribution:"),
            "Patch {} should contain X-Git-AI-Attribution header",
            patch_file.display()
        );
    }

    // Create destination repo and apply both patches
    let dest = TestRepo::new();
    let mut dest_file = dest.filename("file.txt");
    dest_file.set_contents(crate::lines!["Initial content"]);
    dest.stage_all_and_commit("Initial commit").unwrap();

    for patch_file in &patch_files {
        dest.git(&["am", patch_file.to_str().unwrap()]).unwrap();
    }

    // Verify the last commit has AI attribution
    let dest_stats = dest.stats().unwrap();
    assert!(
        dest_stats.ai_additions > 0,
        "Destination should have AI additions after applying patches"
    );
}
