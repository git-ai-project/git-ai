use crate::error::GitAiError;
use crate::git::repository::{Repository, exec_git};

#[derive(Debug, Clone, PartialEq)]
pub enum MappingKind {
    /// Commit is unchanged (patch-identical). Note can be copied as-is.
    Identical,
    /// Commit was modified (content changed). Needs hunk-level attribution transfer.
    Modified,
    /// Commit was deleted (dropped during rebase/squash). Attribution lost.
    Deleted,
    /// Commit was added (new commit not in original range).
    Added,
}

#[derive(Debug, Clone)]
pub struct CommitMapping {
    pub kind: MappingKind,
    pub original: Option<String>,
    pub new: Option<String>,
}

/// Run `git range-diff` and parse the output into commit mappings.
///
/// Compares `onto..original_head` vs `onto..new_head` to determine how each
/// commit in the original range maps to the new range.
pub fn run_range_diff(
    repo: &Repository,
    onto: &str,
    original_head: &str,
    new_head: &str,
) -> Result<Vec<CommitMapping>, GitAiError> {
    if original_head == new_head {
        return Ok(Vec::new());
    }

    let mut args = repo.global_args_for_exec();
    args.push("range-diff".to_string());
    args.push("--no-color".to_string());
    args.push("--no-notes".to_string());
    args.push("--no-patch".to_string());
    args.push(format!("{}..{}", onto, original_head));
    args.push(format!("{}..{}", onto, new_head));

    let output = exec_git(&args)?;
    let stdout = String::from_utf8_lossy(&output.stdout);

    parse_range_diff_output(&stdout)
}

/// Parse the raw output of `git range-diff --no-color --no-patch`.
///
/// Each mapping line has the form:
///   `N:  <sha> = M:  <sha> <subject>`   — identical
///   `N:  <sha> ! M:  <sha> <subject>`   — modified
///   `N:  <sha> < -:  ------- <subject>` — deleted
///   `-:  ------- > M:  <sha> <subject>` — added
pub fn parse_range_diff_output(output: &str) -> Result<Vec<CommitMapping>, GitAiError> {
    let mut mappings = Vec::new();

    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || line.starts_with("    ") {
            continue;
        }

        if let Some(mapping) = parse_range_diff_line(trimmed) {
            mappings.push(mapping);
        }
    }

    Ok(mappings)
}

fn parse_range_diff_line(line: &str) -> Option<CommitMapping> {
    let candidates = [
        line.find(" = ").map(|i| (i, '=')),
        line.find(" ! ").map(|i| (i, '!')),
        line.find(" < ").map(|i| (i, '<')),
        line.find(" > ").map(|i| (i, '>')),
    ];
    let &(idx, op) = candidates
        .iter()
        .filter_map(|x| x.as_ref())
        .min_by_key(|(i, _)| *i)?;
    let left_part = &line[..idx];
    let right_part = &line[idx + 3..];

    let left_sha = extract_sha(left_part);
    let right_sha = extract_sha(right_part);

    let mapping = match op {
        '=' => CommitMapping {
            kind: MappingKind::Identical,
            original: left_sha,
            new: right_sha,
        },
        '!' => CommitMapping {
            kind: MappingKind::Modified,
            original: left_sha,
            new: right_sha,
        },
        '<' => CommitMapping {
            kind: MappingKind::Deleted,
            original: left_sha,
            new: None,
        },
        '>' => CommitMapping {
            kind: MappingKind::Added,
            original: None,
            new: right_sha,
        },
        _ => return None,
    };

    Some(mapping)
}

fn extract_sha(part: &str) -> Option<String> {
    for word in part.split_whitespace() {
        if word.len() >= 7 && word.chars().all(|c| c.is_ascii_hexdigit()) {
            return Some(word.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_identical_mapping() {
        let output = "1:  abc1234 = 1:  def5678 Some commit message\n";
        let mappings = parse_range_diff_output(output).unwrap();
        assert_eq!(mappings.len(), 1);
        assert_eq!(mappings[0].kind, MappingKind::Identical);
        assert_eq!(mappings[0].original.as_deref(), Some("abc1234"));
        assert_eq!(mappings[0].new.as_deref(), Some("def5678"));
    }

    #[test]
    fn parse_modified_mapping() {
        let output = "1:  abc1234 ! 1:  def5678 Modified commit\n";
        let mappings = parse_range_diff_output(output).unwrap();
        assert_eq!(mappings.len(), 1);
        assert_eq!(mappings[0].kind, MappingKind::Modified);
        assert_eq!(mappings[0].original.as_deref(), Some("abc1234"));
        assert_eq!(mappings[0].new.as_deref(), Some("def5678"));
    }

    #[test]
    fn parse_deleted_mapping() {
        let output = "1:  abc1234 < -:  ------- Dropped commit\n";
        let mappings = parse_range_diff_output(output).unwrap();
        assert_eq!(mappings.len(), 1);
        assert_eq!(mappings[0].kind, MappingKind::Deleted);
        assert_eq!(mappings[0].original.as_deref(), Some("abc1234"));
        assert!(mappings[0].new.is_none());
    }

    #[test]
    fn parse_added_mapping() {
        let output = "-:  ------- > 1:  def5678 New commit\n";
        let mappings = parse_range_diff_output(output).unwrap();
        assert_eq!(mappings.len(), 1);
        assert_eq!(mappings[0].kind, MappingKind::Added);
        assert!(mappings[0].original.is_none());
        assert_eq!(mappings[0].new.as_deref(), Some("def5678"));
    }

    #[test]
    fn parse_multi_commit_rebase() {
        let output = "\
1:  aaa1111 = 1:  bbb1111 First commit
2:  aaa2222 ! 2:  bbb2222 Second commit (modified)
3:  aaa3333 < -:  ------- Third commit (dropped)
";
        let mappings = parse_range_diff_output(output).unwrap();
        assert_eq!(mappings.len(), 3);
        assert_eq!(mappings[0].kind, MappingKind::Identical);
        assert_eq!(mappings[1].kind, MappingKind::Modified);
        assert_eq!(mappings[2].kind, MappingKind::Deleted);
    }

    #[test]
    fn parse_skips_indented_diff_content() {
        let output = "\
1:  abc1234 ! 1:  def5678 Modified commit
    diff --git a/file.txt b/file.txt
    --- a/file.txt
    +++ b/file.txt
    @@ -1,3 +1,3 @@
";
        let mappings = parse_range_diff_output(output).unwrap();
        assert_eq!(mappings.len(), 1);
        assert_eq!(mappings[0].kind, MappingKind::Modified);
    }

    #[test]
    fn parse_empty_output() {
        let mappings = parse_range_diff_output("").unwrap();
        assert!(mappings.is_empty());
    }

    #[test]
    fn parse_full_length_shas() {
        let output = "1:  a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2 = 1:  f6e5d4c3b2a1f6e5d4c3b2a1f6e5d4c3b2a1f6e5 Commit\n";
        let mappings = parse_range_diff_output(output).unwrap();
        assert_eq!(mappings.len(), 1);
        assert_eq!(
            mappings[0].original.as_deref(),
            Some("a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2")
        );
        assert_eq!(
            mappings[0].new.as_deref(),
            Some("f6e5d4c3b2a1f6e5d4c3b2a1f6e5d4c3b2a1f6e5")
        );
    }

    #[test]
    fn parse_operator_in_subject() {
        let output = "1:  abc1234 ! 1:  def5678 set x = 5\n";
        let mappings = parse_range_diff_output(output).unwrap();
        assert_eq!(mappings.len(), 1);
        assert_eq!(mappings[0].kind, MappingKind::Modified);
        assert_eq!(mappings[0].original.as_deref(), Some("abc1234"));
        assert_eq!(mappings[0].new.as_deref(), Some("def5678"));
    }

    #[test]
    fn parse_squash_pattern() {
        let output = "\
1:  aaa1111 < -:  ------- First commit (squashed away)
2:  aaa2222 < -:  ------- Second commit (squashed away)
3:  aaa3333 ! 1:  bbb1111 Squash result
";
        let mappings = parse_range_diff_output(output).unwrap();
        assert_eq!(mappings.len(), 3);
        assert_eq!(mappings[0].kind, MappingKind::Deleted);
        assert_eq!(mappings[1].kind, MappingKind::Deleted);
        assert_eq!(mappings[2].kind, MappingKind::Modified);
    }
}
