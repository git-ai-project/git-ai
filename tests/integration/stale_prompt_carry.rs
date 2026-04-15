use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;
use std::fs;

/// Regression test for bug where an AI prompt from a prior commit keeps appearing
/// in every subsequent human-only commit's git notes.
///
/// Scenario from user report:
/// 1. Commit A uses AI (pi + claude) — both prompts appear in note (correct)
/// 2. Commit B is 100% human on a different file — old claude prompt still shows up (BUG)
/// 3. Commit C is 100% human on yet another file — old claude prompt still shows up (BUG)
///
/// Root cause: `to_authorship_log_and_initial_working_log` adds ALL prompts from
/// `self.prompts` to `authorship_log.metadata.prompts` (lines 1312-1322) without
/// filtering to only prompts referenced by committed lines.
#[test]
fn test_stale_prompt_not_carried_to_subsequent_human_commits() {
    let repo = TestRepo::new();

    // Step 1: Create initial base commit (human)
    let mut base_file = repo.filename("base.txt");
    base_file.set_contents(crate::lines!["Base content"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // Step 2: AI edits a file — this creates a prompt in the note
    let mut ai_file = repo.filename("pi.md");
    ai_file.set_contents(crate::lines![
        "AI line 1".ai(),
        "AI line 2".ai(),
        "AI line 3".ai(),
    ]);
    let ai_commit = repo.stage_all_and_commit("AI commit with pi demo").unwrap();

    // Verify the AI commit has prompts (sanity check)
    assert!(
        !ai_commit.authorship_log.metadata.prompts.is_empty(),
        "AI commit should have at least one prompt"
    );
    let ai_prompt_ids: Vec<String> = ai_commit
        .authorship_log
        .metadata
        .prompts
        .keys()
        .cloned()
        .collect();

    // Step 3: Human-only commit on a completely different file
    let mut human_file = repo.filename("canada.md");
    human_file.set_contents(crate::lines![
        "O Canada!",
        "Our home and native land!",
        "True patriot love in all of us command.",
    ]);
    let human_commit = repo
        .stage_all_and_commit("Human-only commit on new file")
        .unwrap();

    // THE BUG CHECK: The human-only commit should NOT contain any of the AI prompts
    for prompt_id in &ai_prompt_ids {
        assert!(
            !human_commit
                .authorship_log
                .metadata
                .prompts
                .contains_key(prompt_id),
            "Stale AI prompt '{}' should NOT appear in human-only commit note.\n\
             Prompts in human commit: {:?}",
            prompt_id,
            human_commit.authorship_log.metadata.prompts.keys().collect::<Vec<_>>()
        );
    }

    // Step 4: Another human-only commit on yet another file
    let mut human_file2 = repo.filename("new-file.md");
    human_file2.set_contents(crate::lines!["Hello safety"]);
    let human_commit2 = repo
        .stage_all_and_commit("Another human-only commit")
        .unwrap();

    // Also should not have stale prompts
    for prompt_id in &ai_prompt_ids {
        assert!(
            !human_commit2
                .authorship_log
                .metadata
                .prompts
                .contains_key(prompt_id),
            "Stale AI prompt '{}' should NOT appear in second human-only commit note.\n\
             Prompts in second human commit: {:?}",
            prompt_id,
            human_commit2.authorship_log.metadata.prompts.keys().collect::<Vec<_>>()
        );
    }
}

/// Test that a prompt IS correctly included when AI lines are actually committed.
/// This is the complementary test — ensures we don't over-filter.
#[test]
fn test_prompt_present_when_ai_lines_committed() {
    let repo = TestRepo::new();

    let mut base_file = repo.filename("base.txt");
    base_file.set_contents(crate::lines!["Base content"]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // AI edits a file
    let mut ai_file = repo.filename("code.rs");
    ai_file.set_contents(crate::lines![
        "fn hello() {".ai(),
        "    println!(\"hello\");".ai(),
        "}".ai(),
    ]);
    let ai_commit = repo.stage_all_and_commit("AI adds code").unwrap();

    // The AI commit must contain the prompt
    assert!(
        !ai_commit.authorship_log.metadata.prompts.is_empty(),
        "AI commit should have prompts when AI lines are committed"
    );
}

/// Test that unstaged AI lines carry their prompt to INITIAL but don't pollute
/// the committed note of a human-only commit.
#[test]
fn test_unstaged_ai_lines_prompt_not_in_human_commit_note() {
    let repo = TestRepo::new();

    let mut base_file = repo.filename("base.txt");
    base_file.set_contents(crate::lines!["Base content", ""]);
    repo.stage_all_and_commit("Initial commit").unwrap();

    // AI adds lines to base.txt
    base_file.insert_at(1, crate::lines!["AI line".ai(), "AI line 2".ai()]);

    // Stage only the AI additions
    base_file.stage();

    // Human adds more unstaged content
    base_file.insert_at(3, crate::lines!["unstaged ai".ai()]);

    // Commit only the staged AI lines
    let first_commit = repo.commit("Commit with AI lines").unwrap();
    let ai_prompt_ids: Vec<String> = first_commit
        .authorship_log
        .metadata
        .prompts
        .keys()
        .cloned()
        .collect();
    assert!(!ai_prompt_ids.is_empty(), "First commit should have AI prompts");

    // Create a human-only file WITHOUT using set_contents (which would stage everything
    // via `git add -A`). Write the file directly and stage only it to keep base.txt's
    // unstaged AI changes out of this commit.
    let human_file_path = repo.path().join("human.txt");
    fs::write(&human_file_path, "Pure human content\n").unwrap();
    repo.git(&["add", "human.txt"]).unwrap();
    let human_commit = repo.commit("Human-only commit while unstaged AI exists").unwrap();

    // The human-only commit should NOT contain AI prompts even though
    // unstaged AI lines exist in the working directory
    for prompt_id in &ai_prompt_ids {
        assert!(
            !human_commit
                .authorship_log
                .metadata
                .prompts
                .contains_key(prompt_id),
            "AI prompt '{}' from unstaged lines should NOT appear in human-only commit note.\n\
             Prompts in human commit: {:?}",
            prompt_id,
            human_commit.authorship_log.metadata.prompts.keys().collect::<Vec<_>>()
        );
    }

}
