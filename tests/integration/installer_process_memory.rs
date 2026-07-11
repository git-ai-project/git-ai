#[cfg(unix)]
mod unix {
    use crate::repos::test_file::ExpectedLineExt;
    use crate::repos::test_repo::TestRepo;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn oversized_agent_version_output_is_bounded_and_inconclusive() {
        let repo = TestRepo::new();
        let mut file = repo.filename("tracked.txt");
        file.set_contents(lines!["first", "second", "third"]);
        repo.stage_all_and_commit("initial").unwrap();
        file.assert_committed_lines(lines!["first".human(), "second".human(), "third".human(),]);

        let bin_dir = repo.test_home_path().join("bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let claude = bin_dir.join("claude");
        fs::write(&claude, "#!/bin/sh\nyes x | head -c 2097152\n").unwrap();
        let mut permissions = fs::metadata(&claude).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&claude, permissions).unwrap();

        let path = format!(
            "{}:{}",
            bin_dir.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let output = repo
            .git_ai_with_env(&["install", "--dry-run"], &[("PATH", &path)])
            .unwrap();
        assert!(
            output.contains("Claude Code: Pending updates"),
            "unexpected install output: {output}"
        );
        assert!(!output.contains("stdout exceeded"));
        assert!(output.len() < 16 * 1024);

        file.insert_at(3, lines!["AI edit".ai()]);
        repo.stage_all_and_commit("AI edit").unwrap();
        file.assert_committed_lines(lines![
            "first".human(),
            "second".human(),
            "third".ai(),
            "AI edit".ai(),
        ]);
    }
}
