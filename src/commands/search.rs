use std::collections::HashMap;
use std::process::{self, Command, Stdio};

use git_ai::core::authorship_log::AuthorshipLog;

use crate::commands::blame::load_authorship_note;
use crate::commands::helpers::git_cmd;

/// Attribution category for a line.
#[derive(Debug, Clone, PartialEq)]
enum Attribution {
    Ai,
    Human,
    Untracked,
}

impl std::fmt::Display for Attribution {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Attribution::Ai => write!(f, "AI"),
            Attribution::Human => write!(f, "HUMAN"),
            Attribution::Untracked => write!(f, "UNTRACKED"),
        }
    }
}

pub fn handle_search(args: &[String]) {
    let mut ai_only = false;
    let mut human_only = false;
    let mut show_help = false;
    let mut pattern: Option<String> = None;
    let mut extra_grep_args: Vec<String> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        match arg {
            "--help" | "-h" => {
                show_help = true;
            }
            "--ai-only" => {
                ai_only = true;
            }
            "--human-only" => {
                human_only = true;
            }
            "-i" | "--ignore-case" => {
                extra_grep_args.push(arg.to_string());
            }
            "-w" | "--word-regexp" => {
                extra_grep_args.push(arg.to_string());
            }
            "-l" | "--files-with-matches" => {
                extra_grep_args.push(arg.to_string());
            }
            s if s.starts_with('-') => {
                extra_grep_args.push(arg.to_string());
            }
            _ => {
                if pattern.is_none() {
                    pattern = Some(arg.to_string());
                } else {
                    // Additional positional args passed as paths to git grep
                    extra_grep_args.push("--".to_string());
                    extra_grep_args.push(arg.to_string());
                    // Collect remaining positional args as paths
                    i += 1;
                    while i < args.len() {
                        extra_grep_args.push(args[i].clone());
                        i += 1;
                    }
                    break;
                }
            }
        }
        i += 1;
    }

    if show_help {
        println!("usage: git-ai search [--ai-only] [--human-only] <pattern> [<path>...]");
        println!();
        println!("Grep with attribution context. Uses git grep under the hood.");
        println!();
        println!("For each matching line, looks up the attribution from the git note");
        println!("on the commit that last touched that line.");
        println!();
        println!("Output format: file:line:[AI|HUMAN|UNTRACKED]: content");
        println!();
        println!("Options:");
        println!("  --ai-only        Only show lines attributed to AI");
        println!("  --human-only     Only show lines attributed to humans");
        println!("  -i, --ignore-case  Case-insensitive search");
        println!("  -w, --word-regexp  Match whole words only");
        return;
    }

    if ai_only && human_only {
        eprintln!("error: --ai-only and --human-only are mutually exclusive");
        process::exit(1);
    }

    let search_pattern = match pattern {
        Some(p) => p,
        None => {
            eprintln!("usage: git-ai search <pattern>");
            eprintln!("Run 'git-ai search --help' for more information.");
            process::exit(1);
        }
    };

    // Run git grep to find matching lines with line numbers
    let mut grep_args: Vec<&str> = vec!["grep", "-n"];
    for arg in &extra_grep_args {
        grep_args.push(arg.as_str());
    }
    grep_args.push(&search_pattern);

    let grep_output = Command::new("/usr/bin/git")
        .args(&grep_args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output();

    let grep_output = match grep_output {
        Ok(output) => {
            if !output.status.success() {
                let code = output.status.code().unwrap_or(1);
                if code == 1 {
                    // No matches found — git grep exits 1 for no match
                    return;
                }
                let stderr = String::from_utf8_lossy(&output.stderr);
                eprintln!("error: git grep failed: {}", stderr.trim());
                process::exit(1);
            }
            String::from_utf8_lossy(&output.stdout).to_string()
        }
        Err(e) => {
            eprintln!("error: failed to run git grep: {}", e);
            process::exit(1);
        }
    };

    // Parse git grep output: "file:line:content"
    let mut matches: Vec<GrepMatch> = Vec::new();
    for line in grep_output.lines() {
        if let Some(parsed) = parse_grep_line(line) {
            matches.push(parsed);
        }
    }

    if matches.is_empty() {
        return;
    }

    // Group matches by file to batch blame lookups
    let mut by_file: HashMap<String, Vec<&GrepMatch>> = HashMap::new();
    for m in &matches {
        by_file.entry(m.file.clone()).or_default().push(m);
    }

    // For each file, use git blame -p to find the commit for each line,
    // then look up attribution from the authorship note
    let mut results: Vec<(String, u32, Attribution, String)> = Vec::new();

    for (file, file_matches) in &by_file {
        // Get blame data for each line we care about
        let line_attributions = match get_line_attributions(file, file_matches) {
            Ok(attrs) => attrs,
            Err(e) => {
                eprintln!("warning: could not get attributions for '{}': {}", file, e);
                HashMap::new()
            }
        };

        for m in file_matches {
            if let Some(attr) = line_attributions.get(&m.line) {
                results.push((m.file.clone(), m.line, attr.clone(), m.content.clone()));
            } else {
                results.push((
                    m.file.clone(),
                    m.line,
                    Attribution::Untracked,
                    m.content.clone(),
                ));
            }
        }
    }

    // Sort by file, then line number for stable output
    results.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));

    // Compute column widths for alignment
    let max_file_len = results.iter().map(|r| r.0.len()).max().unwrap_or(0);
    let max_line_len = results
        .iter()
        .map(|r| r.1.to_string().len())
        .max()
        .unwrap_or(0);

    // Output filtered results
    for (file, line, attr, content) in &results {
        if ai_only && *attr != Attribution::Ai {
            continue;
        }
        if human_only && *attr != Attribution::Human {
            continue;
        }
        println!(
            "{:<fwidth$}:{:>lwidth$}:{:>9}: {}",
            file,
            line,
            format!("{}", attr),
            content,
            fwidth = max_file_len,
            lwidth = max_line_len,
        );
    }
}

struct GrepMatch {
    file: String,
    line: u32,
    content: String,
}

/// Parse a git grep output line: "file:line:content"
fn parse_grep_line(line: &str) -> Option<GrepMatch> {
    // Format: "path/to/file:linenum:content"
    let first_colon = line.find(':')?;
    let file = &line[..first_colon];
    let rest = &line[first_colon + 1..];
    let second_colon = rest.find(':')?;
    let line_num: u32 = rest[..second_colon].parse().ok()?;
    let content = &rest[second_colon + 1..];
    Some(GrepMatch {
        file: file.to_string(),
        line: line_num,
        content: content.to_string(),
    })
}

/// Get attribution for specific lines in a file using git blame.
fn get_line_attributions(
    file: &str,
    matches: &[&GrepMatch],
) -> Result<HashMap<u32, Attribution>, Box<dyn std::error::Error>> {
    let mut result: HashMap<u32, Attribution> = HashMap::new();

    // Resolve repo-relative file path for note lookups
    let repo_relative_file_path = {
        let prefix = git_cmd(&["rev-parse", "--show-prefix"]).unwrap_or_default();
        let candidate = if prefix.is_empty() {
            file.to_string()
        } else {
            format!("{}{}", prefix, file)
        };
        let p = std::path::PathBuf::from(&candidate);
        let mut components: Vec<String> = Vec::new();
        for comp in p.components() {
            match comp {
                std::path::Component::ParentDir => {
                    components.pop();
                }
                std::path::Component::CurDir => {}
                std::path::Component::Normal(s) => {
                    components.push(s.to_string_lossy().to_string());
                }
                _ => {}
            }
        }
        components.join("/")
    };

    // Build -L ranges for targeted blame (more efficient for sparse matches)
    // If few lines, use individual -L; otherwise blame the whole file
    let blame_output = if matches.len() <= 20 {
        let mut blame_args: Vec<String> = vec!["blame".to_string(), "--line-porcelain".to_string()];
        for m in matches {
            blame_args.push(format!("-L{},{}", m.line, m.line));
        }
        blame_args.push("--".to_string());
        blame_args.push(file.to_string());
        let args_refs: Vec<&str> = blame_args.iter().map(|s| s.as_str()).collect();
        match git_cmd(&args_refs) {
            Ok(output) => output,
            Err(e) => return Err(e.into()),
        }
    } else {
        match git_cmd(&["blame", "--line-porcelain", "--", file]) {
            Ok(output) => output,
            Err(e) => return Err(e.into()),
        }
    };

    // Parse blame output to extract commit SHA and orig line for each line
    let mut commit_notes: HashMap<String, Option<AuthorshipLog>> = HashMap::new();
    let mut cur_sha = String::new();
    let mut cur_orig_line: u32 = 0;
    let mut cur_final_line: u32 = 0;
    let mut cur_author_email = String::new();
    let mut cur_headers: Vec<String> = Vec::new();

    let target_lines: std::collections::HashSet<u32> =
        matches.iter().map(|m| m.line).collect();

    for line in blame_output.lines() {
        if line.is_empty() {
            continue;
        }
        if line.starts_with('\t') {
            // End of a blame block — resolve attribution if this is a line we care about
            if target_lines.contains(&cur_final_line) {
                if !commit_notes.contains_key(&cur_sha) {
                    let note = load_authorship_note(&cur_sha);
                    commit_notes.insert(cur_sha.clone(), note);
                }
                let attr = resolve_attribution(
                    &cur_sha,
                    cur_orig_line,
                    &cur_author_email,
                    &repo_relative_file_path,
                    &commit_notes,
                    &cur_headers,
                );
                result.insert(cur_final_line, attr);
            }
            cur_headers.clear();
            continue;
        }
        if let Some(rest) = line.strip_prefix("author-mail ") {
            cur_author_email = rest
                .trim_start_matches('<')
                .trim_end_matches('>')
                .to_string();
            cur_headers.push(line.to_string());
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 3
            && parts[0].len() == 40
            && parts[0].chars().all(|c| c.is_ascii_hexdigit())
        {
            cur_sha = parts[0].to_string();
            cur_orig_line = parts[1].parse().unwrap_or(0);
            cur_final_line = parts[2].parse().unwrap_or(0);
            cur_headers.push(line.to_string());
        } else {
            cur_headers.push(line.to_string());
        }
    }

    Ok(result)
}

/// Determine attribution for a single line given its blame commit and authorship note.
fn resolve_attribution(
    commit_sha: &str,
    orig_line: u32,
    author_email: &str,
    file_path: &str,
    commit_notes: &HashMap<String, Option<AuthorshipLog>>,
    raw_headers: &[String],
) -> Attribution {
    if let Some(Some(authorship_log)) = commit_notes.get(commit_sha) {
        // Extract the original filename from blame porcelain headers
        let orig_filename: Option<&str> = raw_headers.iter().find_map(|h| h.strip_prefix("filename "));

        for file_attest in &authorship_log.attestations {
            let attest_path = file_attest
                .file_path
                .strip_prefix("./")
                .unwrap_or(&file_attest.file_path);
            let query_path = file_path.strip_prefix("./").unwrap_or(file_path);
            let matches = attest_path == query_path
                || orig_filename.is_some_and(|orig| {
                    let orig_clean = orig.strip_prefix("./").unwrap_or(orig);
                    attest_path == orig_clean
                });
            if !matches {
                continue;
            }
            for entry in &file_attest.entries {
                let covers_line = entry.line_ranges.iter().any(|r| r.contains(orig_line));
                if !covers_line {
                    continue;
                }
                if entry.hash.starts_with("h_") {
                    return Attribution::Human;
                }
                // AI: either it's a session, a prompt, or matched by hash
                if entry.hash.starts_with("s_")
                    || authorship_log.metadata.prompts.contains_key(&entry.hash)
                    || authorship_log.metadata.sessions.contains_key(&entry.hash)
                {
                    return Attribution::Ai;
                }
                // Unknown hash — treat as untracked
                return Attribution::Untracked;
            }
        }
    }

    // Fallback: check if author email belongs to a known AI agent
    if crate::commands::blame::detect_agent_from_email(author_email).is_some() {
        return Attribution::Ai;
    }

    Attribution::Untracked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_grep_line_basic() {
        let line = "src/main.rs:42:    let x = 5;";
        let result = parse_grep_line(line).unwrap();
        assert_eq!(result.file, "src/main.rs");
        assert_eq!(result.line, 42);
        assert_eq!(result.content, "    let x = 5;");
    }

    #[test]
    fn test_parse_grep_line_with_colons_in_content() {
        let line = "config.json:10:  \"key\": \"value:with:colons\"";
        let result = parse_grep_line(line).unwrap();
        assert_eq!(result.file, "config.json");
        assert_eq!(result.line, 10);
        assert_eq!(result.content, "  \"key\": \"value:with:colons\"");
    }

    #[test]
    fn test_parse_grep_line_invalid() {
        assert!(parse_grep_line("no colons here").is_none());
        assert!(parse_grep_line("file:notanumber:content").is_none());
    }

    #[test]
    fn test_attribution_display() {
        assert_eq!(format!("{}", Attribution::Ai), "AI");
        assert_eq!(format!("{}", Attribution::Human), "HUMAN");
        assert_eq!(format!("{}", Attribution::Untracked), "UNTRACKED");
    }

    #[test]
    fn test_help_flag() {
        // --help should return without panicking or exiting
        handle_search(&["--help".to_string()]);
    }
}
