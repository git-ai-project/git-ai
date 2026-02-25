#![allow(dead_code)]

use crate::repos::test_file::ExpectedLine;
use crate::repos::test_repo::{TestRepo, require_gt_or_skip};

pub fn skip_unless_gt() -> bool {
    if let Some(reason) = require_gt_or_skip() {
        println!("⏭️ {}", reason);
        return true;
    }
    false
}

pub fn init_graphite(repo: &TestRepo) {
    repo.gt(&["init", "--trunk", "main"])
        .expect("gt init --trunk main should succeed");
}

pub fn assert_exact_blame(repo: &TestRepo, filename: &str, expected_lines: Vec<ExpectedLine>) {
    let mut file = repo.filename(filename);
    file.assert_lines_and_blame(expected_lines);
}

pub fn assert_blame_is_unchanged(repo: &TestRepo, filename: &str, before_blame: &str) {
    let after = repo
        .git_ai(&["blame", filename])
        .expect("git-ai blame should succeed after gt operation");
    let before_normalized = normalize_blame(before_blame);
    let after_normalized = normalize_blame(&after);
    assert_eq!(
        before_normalized, after_normalized,
        "blame changed unexpectedly for {}",
        filename
    );
}

pub fn capture_blame(repo: &TestRepo, filename: &str) -> String {
    repo.git_ai(&["blame", filename])
        .expect("git-ai blame should succeed")
}

fn normalize_blame(blame_output: &str) -> Vec<(String, String)> {
    blame_output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(parse_blame_line)
        .collect()
}

fn parse_blame_line(line: &str) -> (String, String) {
    if let Some(start_paren) = line.find('(')
        && let Some(end_paren) = line.find(')')
    {
        let author_section = &line[start_paren + 1..end_paren];
        let content = line[end_paren + 1..].trim();

        let parts: Vec<&str> = author_section.split_whitespace().collect();
        let mut author_parts = Vec::new();
        for part in parts {
            if part.chars().next().unwrap_or('a').is_ascii_digit() {
                break;
            }
            author_parts.push(part);
        }
        let author = author_parts.join(" ");
        return (author, content.to_string());
    }

    ("unknown".to_string(), line.to_string())
}
