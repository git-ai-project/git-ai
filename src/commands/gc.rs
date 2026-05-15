use std::collections::HashSet;

use crate::commands::helpers::git_cmd;

/// Handle the `git-ai gc` command: remove orphaned authorship notes.
///
/// An orphaned note is one attached to a commit SHA that is no longer reachable
/// from any ref (branch, tag, etc.).
pub fn handle_gc(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let mut dry_run = false;
    let mut verbose = false;

    for arg in args {
        match arg.as_str() {
            "--dry-run" => dry_run = true,
            "--verbose" | "-v" => verbose = true,
            "--help" | "-h" => {
                println!("usage: git-ai gc [--dry-run] [--verbose]");
                println!();
                println!(
                    "Remove orphaned authorship notes (notes attached to unreachable commits)."
                );
                println!();
                println!("Options:");
                println!("  --dry-run   Show what would be removed without removing");
                println!("  --verbose   List each removed note");
                return Ok(());
            }
            other => {
                return Err(format!("unknown option '{}'", other).into());
            }
        }
    }

    // Step 1: Get all commits that have authorship notes
    let noted_commits = list_noted_commits()?;
    if noted_commits.is_empty() {
        println!("No authorship notes found.");
        return Ok(());
    }

    // Step 2: Get all reachable commits (one call, very efficient)
    let reachable = get_reachable_commits()?;

    // Step 3: Find orphaned notes (noted commits not in reachable set)
    let mut orphaned: Vec<&str> = noted_commits
        .iter()
        .filter(|sha| !reachable.contains(sha.as_str()))
        .map(|s| s.as_str())
        .collect();
    orphaned.sort();

    if orphaned.is_empty() {
        println!(
            "No orphaned notes found ({} notes all reachable).",
            noted_commits.len()
        );
        return Ok(());
    }

    // Step 4: Remove orphaned notes (or report in dry-run mode)
    let mut removed = 0;
    for sha in &orphaned {
        if dry_run {
            if verbose {
                println!("would remove: {}", sha);
            }
            removed += 1;
        } else {
            match git_cmd(&["notes", "--ref=ai", "remove", sha]) {
                Ok(_) => {
                    if verbose {
                        println!("removed: {}", sha);
                    }
                    removed += 1;
                }
                Err(e) => {
                    eprintln!("warning: failed to remove note for {}: {}", sha, e);
                }
            }
        }
    }

    let remaining = noted_commits.len() - removed;
    if dry_run {
        println!(
            "Would remove {} orphaned notes ({} notes would remain)",
            removed, remaining
        );
    } else {
        println!(
            "Removed {} orphaned notes ({} notes remain)",
            removed, remaining
        );
    }

    Ok(())
}

/// List all commit SHAs that have authorship notes attached.
fn list_noted_commits() -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let output = match git_cmd(&["notes", "--ref=ai", "list"]) {
        Ok(o) => o,
        Err(e) => {
            // If no notes ref exists, there are no notes
            if e.contains("does not exist") || e.contains("not a valid ref") {
                return Ok(Vec::new());
            }
            return Err(e.into());
        }
    };

    if output.trim().is_empty() {
        return Ok(Vec::new());
    }

    // `git notes list` outputs: <note_blob_sha> <annotated_commit_sha>
    let commits: Vec<String> = output
        .lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 {
                Some(parts[1].to_string())
            } else {
                None
            }
        })
        .collect();

    Ok(commits)
}

/// Get the set of all commits reachable from any ref.
fn get_reachable_commits() -> Result<HashSet<String>, Box<dyn std::error::Error>> {
    let output = git_cmd(&["rev-list", "--all"])?;

    let set: HashSet<String> = output
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect();

    Ok(set)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_orphan_detection_logic() {
        // Simulate: 3 noted commits, only 2 are reachable
        let noted = [
            "aaa1111111111111111111111111111111111111".to_string(),
            "bbb2222222222222222222222222222222222222".to_string(),
            "ccc3333333333333333333333333333333333333".to_string(),
        ];

        let mut reachable = HashSet::new();
        reachable.insert("aaa1111111111111111111111111111111111111".to_string());
        reachable.insert("ccc3333333333333333333333333333333333333".to_string());
        reachable.insert("ddd4444444444444444444444444444444444444".to_string());

        let orphaned: Vec<&str> = noted
            .iter()
            .filter(|sha| !reachable.contains(sha.as_str()))
            .map(|s| s.as_str())
            .collect();

        assert_eq!(orphaned.len(), 1);
        assert_eq!(orphaned[0], "bbb2222222222222222222222222222222222222");
    }

    #[test]
    fn test_all_reachable_means_no_orphans() {
        let noted = [
            "aaa1111111111111111111111111111111111111".to_string(),
            "bbb2222222222222222222222222222222222222".to_string(),
        ];

        let mut reachable = HashSet::new();
        reachable.insert("aaa1111111111111111111111111111111111111".to_string());
        reachable.insert("bbb2222222222222222222222222222222222222".to_string());

        let orphaned: Vec<&str> = noted
            .iter()
            .filter(|sha| !reachable.contains(sha.as_str()))
            .map(|s| s.as_str())
            .collect();

        assert!(orphaned.is_empty());
    }

    #[test]
    fn test_empty_noted_commits() {
        let noted: Vec<String> = vec![];
        let reachable: HashSet<String> = HashSet::new();

        let orphaned: Vec<&str> = noted
            .iter()
            .filter(|sha| !reachable.contains(sha.as_str()))
            .map(|s| s.as_str())
            .collect();

        assert!(orphaned.is_empty());
    }

    #[test]
    fn test_parse_notes_list_output() {
        let output = "abc123def456 1111111111111111111111111111111111111111\nfed987654321 2222222222222222222222222222222222222222\n";

        let commits: Vec<String> = output
            .lines()
            .filter_map(|line| {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 2 {
                    Some(parts[1].to_string())
                } else {
                    None
                }
            })
            .collect();

        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0], "1111111111111111111111111111111111111111");
        assert_eq!(commits[1], "2222222222222222222222222222222222222222");
    }
}
