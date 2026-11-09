use std::fs;

use crate::repos::test_file::ExpectedLineExt;
use crate::repos::test_repo::TestRepo;

/// Regression test for https://github.com/git-ai-project/git-ai/issues/2480.
///
/// A KnownHuman (IDE) edit followed by an untracked edit and an agent's
/// pre-edit `human` checkpoint must not lose the `h_` attribution: the
/// `human` checkpoint fast path used to write an attribution-empty entry that
/// replaced the KnownHuman attributions, leaving the human line unknown (and
/// later recoverable to an AI session) at commit time.
#[test]
fn test_known_human_line_survives_following_human_checkpoint() {
    let repo = TestRepo::new();
    let file_path = repo.path().join("Program.cs");

    fs::write(&file_path, "Base line\n").unwrap();
    repo.stage_all_and_commit("Initial commit").unwrap();
    let mut file = repo.filename("Program.cs");
    file.assert_committed_lines(crate::lines!["Base line".unattributed_human()]);

    // Human types a line in the IDE (KnownHuman checkpoint).
    fs::write(&file_path, "Base line\nHuman line\n").unwrap();
    repo.git_ai(&["checkpoint", "mock_known_human", "Program.cs"])
        .unwrap();

    // Something else touches the file, then the agent's pre-edit checkpoint fires.
    fs::write(&file_path, "Base line\nHuman line\nUntracked line\n").unwrap();
    repo.git_ai(&["checkpoint", "human", "Program.cs"]).unwrap();

    // The agent appends a line (post-edit AI checkpoint).
    fs::write(
        &file_path,
        "Base line\nHuman line\nUntracked line\nAI line\n",
    )
    .unwrap();
    repo.git_ai(&["checkpoint", "mock_ai", "Program.cs"])
        .unwrap();

    repo.stage_all_and_commit("Human and AI edits").unwrap();
    file.assert_committed_lines(crate::lines![
        "Base line".unattributed_human(),
        "Human line".human(),
        "Untracked line".unattributed_human(),
        "AI line".ai(),
    ]);
}
