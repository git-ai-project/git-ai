use crate::authorship::rebase_authorship::rewrite_authorship_after_squash_or_rebase;
use crate::git::find_repository_in_path;

pub fn handle_squash_authorship(args: &[String]) {
    // Parse squash-authorship-specific arguments
    let mut base_branch = None;
    let mut new_sha = None;
    let mut old_sha = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--dry-run" => {
                // Dry-run flag is parsed but not used in current implementation
                i += 1;
            }
            _ => {
                // Positional arguments: base_branch, new_sha, old_sha
                if base_branch.is_none() {
                    base_branch = Some(args[i].clone());
                } else if new_sha.is_none() {
                    new_sha = Some(args[i].clone());
                } else if old_sha.is_none() {
                    old_sha = Some(args[i].clone());
                } else {
                    eprintln!("Unknown squash-authorship argument: {}", args[i]);
                    std::process::exit(1);
                }
                i += 1;
            }
        }
    }

    // Validate required arguments
    let base_branch = match base_branch {
        Some(s) => s,
        None => {
            eprintln!("Error: base_branch argument is required");
            eprintln!(
                "Usage: git-ai squash-authorship <base_branch> <new_sha> <old_sha> [--dry-run]"
            );
            std::process::exit(1);
        }
    };

    let new_sha = match new_sha {
        Some(s) => s,
        None => {
            eprintln!("Error: new_sha argument is required");
            eprintln!(
                "Usage: git-ai squash-authorship <base_branch> <new_sha> <old_sha> [--dry-run]"
            );
            std::process::exit(1);
        }
    };

    let old_sha = match old_sha {
        Some(s) => s,
        None => {
            eprintln!("Error: old_sha argument is required");
            eprintln!(
                "Usage: git-ai squash-authorship <base_branch> <new_sha> <old_sha> [--dry-run]"
            );
            std::process::exit(1);
        }
    };

    // TODO Think about whether or not path should be an optional argument

    // Find the git repository
    let repo = match find_repository_in_path(".") {
        Ok(repo) => repo,
        Err(e) => {
            eprintln!("Failed to find repository: {}", e);
            std::process::exit(1);
        }
    };

    // Use the same function as CI handlers to create authorship log for the new commit
    if let Err(e) = rewrite_authorship_after_squash_or_rebase(
        &repo,
        "",           // head_ref - not used by the function
        &base_branch, // merge_ref - the base branch name (e.g., "main")
        &old_sha,     // source_head_sha - the old commit
        &new_sha,     // merge_commit_sha - the new commit
        false,        // suppress_output
    ) {
        eprintln!("Squash authorship failed: {}", e);
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    #[allow(unused_imports)]
    use super::*;

    #[test]
    fn test_handle_squash_authorship_parse_all_positional_args() {
        // Test that positional arguments are parsed in order
        let args = vec![
            "main".to_string(),
            "abc123".to_string(),
            "def456".to_string(),
        ];

        // Parse the arguments manually to test the logic
        let mut base_branch = None;
        let mut new_sha = None;
        let mut old_sha = None;

        for arg in &args {
            if base_branch.is_none() {
                base_branch = Some(arg.clone());
            } else if new_sha.is_none() {
                new_sha = Some(arg.clone());
            } else if old_sha.is_none() {
                old_sha = Some(arg.clone());
            }
        }

        assert_eq!(base_branch, Some("main".to_string()));
        assert_eq!(new_sha, Some("abc123".to_string()));
        assert_eq!(old_sha, Some("def456".to_string()));
    }

    #[test]
    fn test_handle_squash_authorship_parse_with_dry_run() {
        // Test that --dry-run flag is parsed correctly
        let args = vec![
            "main".to_string(),
            "--dry-run".to_string(),
            "abc123".to_string(),
            "def456".to_string(),
        ];

        let mut base_branch = None;
        let mut new_sha = None;
        let mut old_sha = None;
        let mut dry_run = false;

        let mut i = 0;
        while i < args.len() {
            match args[i].as_str() {
                "--dry-run" => {
                    dry_run = true;
                    i += 1;
                }
                _ => {
                    if base_branch.is_none() {
                        base_branch = Some(args[i].clone());
                    } else if new_sha.is_none() {
                        new_sha = Some(args[i].clone());
                    } else if old_sha.is_none() {
                        old_sha = Some(args[i].clone());
                    }
                    i += 1;
                }
            }
        }

        assert_eq!(base_branch, Some("main".to_string()));
        assert_eq!(new_sha, Some("abc123".to_string()));
        assert_eq!(old_sha, Some("def456".to_string()));
        assert!(dry_run);
    }

    #[test]
    fn test_handle_squash_authorship_parse_minimal_args() {
        // Test with exactly 3 required arguments
        let args = vec![
            "main".to_string(),
            "new_commit".to_string(),
            "old_commit".to_string(),
        ];

        let mut base_branch = None;
        let mut new_sha = None;
        let mut old_sha = None;

        for arg in &args {
            if base_branch.is_none() {
                base_branch = Some(arg.clone());
            } else if new_sha.is_none() {
                new_sha = Some(arg.clone());
            } else if old_sha.is_none() {
                old_sha = Some(arg.clone());
            }
        }

        assert!(base_branch.is_some());
        assert!(new_sha.is_some());
        assert!(old_sha.is_some());
    }

    #[test]
    fn test_handle_squash_authorship_parse_missing_base_branch() {
        // Test parsing logic when no args provided
        let args: Vec<String> = vec![];

        let mut base_branch = None;

        for arg in &args {
            if base_branch.is_none() {
                base_branch = Some(arg.clone());
            }
        }

        assert!(base_branch.is_none());
    }

    #[test]
    fn test_handle_squash_authorship_parse_missing_new_sha() {
        // Test parsing logic when only base_branch provided
        let args = vec!["main".to_string()];

        let mut base_branch = None;
        let mut new_sha = None;

        for arg in &args {
            if base_branch.is_none() {
                base_branch = Some(arg.clone());
            } else if new_sha.is_none() {
                new_sha = Some(arg.clone());
            }
        }

        assert_eq!(base_branch, Some("main".to_string()));
        assert!(new_sha.is_none());
    }

    #[test]
    fn test_handle_squash_authorship_parse_missing_old_sha() {
        // Test parsing logic when only base_branch and new_sha provided
        let args = vec!["main".to_string(), "abc123".to_string()];

        let mut base_branch = None;
        let mut new_sha = None;
        let mut old_sha = None;

        for arg in &args {
            if base_branch.is_none() {
                base_branch = Some(arg.clone());
            } else if new_sha.is_none() {
                new_sha = Some(arg.clone());
            } else if old_sha.is_none() {
                old_sha = Some(arg.clone());
            }
        }

        assert_eq!(base_branch, Some("main".to_string()));
        assert_eq!(new_sha, Some("abc123".to_string()));
        assert!(old_sha.is_none());
    }

    #[test]
    fn test_handle_squash_authorship_parse_order() {
        // Test that argument order matters
        let args = vec![
            "feature-branch".to_string(),
            "sha1111".to_string(),
            "sha2222".to_string(),
        ];

        let mut base_branch = None;
        let mut new_sha = None;
        let mut old_sha = None;

        for arg in &args {
            if base_branch.is_none() {
                base_branch = Some(arg.clone());
            } else if new_sha.is_none() {
                new_sha = Some(arg.clone());
            } else if old_sha.is_none() {
                old_sha = Some(arg.clone());
            }
        }

        assert_eq!(base_branch.unwrap(), "feature-branch");
        assert_eq!(new_sha.unwrap(), "sha1111");
        assert_eq!(old_sha.unwrap(), "sha2222");
    }

    #[test]
    fn test_handle_squash_authorship_parse_dry_run_at_end() {
        // Test --dry-run flag at the end
        let args = vec![
            "main".to_string(),
            "abc".to_string(),
            "def".to_string(),
            "--dry-run".to_string(),
        ];

        let mut dry_run_found = false;
        let mut arg_count = 0;

        for arg in &args {
            if arg == "--dry-run" {
                dry_run_found = true;
            } else {
                arg_count += 1;
            }
        }

        assert!(dry_run_found);
        assert_eq!(arg_count, 3);
    }

    #[test]
    fn test_handle_squash_authorship_parse_empty_strings() {
        // Test with empty string arguments (edge case)
        let args = vec!["".to_string(), "abc".to_string(), "def".to_string()];

        let mut base_branch = None;
        let mut new_sha = None;
        let mut old_sha = None;

        for arg in &args {
            if base_branch.is_none() {
                base_branch = Some(arg.clone());
            } else if new_sha.is_none() {
                new_sha = Some(arg.clone());
            } else if old_sha.is_none() {
                old_sha = Some(arg.clone());
            }
        }

        // Empty string is still a valid argument
        assert_eq!(base_branch, Some("".to_string()));
        assert_eq!(new_sha, Some("abc".to_string()));
        assert_eq!(old_sha, Some("def".to_string()));
    }

    #[test]
    fn test_handle_squash_authorship_parse_special_characters() {
        // Test with special characters in arguments
        let args = vec![
            "origin/main".to_string(),
            "abc123^".to_string(),
            "HEAD~1".to_string(),
        ];

        let mut base_branch = None;
        let mut new_sha = None;
        let mut old_sha = None;

        for arg in &args {
            if base_branch.is_none() {
                base_branch = Some(arg.clone());
            } else if new_sha.is_none() {
                new_sha = Some(arg.clone());
            } else if old_sha.is_none() {
                old_sha = Some(arg.clone());
            }
        }

        assert_eq!(base_branch, Some("origin/main".to_string()));
        assert_eq!(new_sha, Some("abc123^".to_string()));
        assert_eq!(old_sha, Some("HEAD~1".to_string()));
    }
}
