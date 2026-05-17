use git_ai::core::authorship_log::AuthorshipLog;
use git_ai::core::git_binary::git_cmd as git_command;

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::process::{self, Stdio};

use crate::commands::helpers::git_cmd;

/// Default ignore patterns for files that should be excluded from diff output.
pub const DEFAULT_IGNORE_PATTERNS: &[&str] = &[
    "*.lock",
    "Cargo.lock",
    "package-lock.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "go.sum",
    "Gemfile.lock",
    "poetry.lock",
    "composer.lock",
    "Pipfile.lock",
    "shrinkwrap.yaml",
    "*.generated.*",
    "*.min.js",
    "*.min.css",
    "*.map",
    "**/vendor/**",
    "**/node_modules/**",
    "**/__snapshots__/**",
    "**/*.snap",
    "**/*.snap.new",
    "**/drizzle/meta/**",
    // Protobuf generated code
    "*.pbobjc.h",
    "*.pbobjc.m",
    "*.pb.go",
    "*.pb.h",
    "*.pb.cc",
    "*_pb2.py",
    "*_pb2_grpc.py",
    "*.pb.swift",
    "*.pb.dart",
];

/// Simple glob pattern matching without external crate.
/// Supports `*` (matches any characters except `/`), `**` (matches any path segments),
/// and `?` (matches a single non-`/` character).
fn glob_matches(pattern: &str, text: &str) -> bool {
    glob_matches_recursive(pattern.as_bytes(), text.as_bytes())
}

fn glob_matches_recursive(pattern: &[u8], text: &[u8]) -> bool {
    let mut p = 0;
    let mut t = 0;
    let mut star_p = None; // position in pattern after last `*`
    let mut star_t = 0; // position in text when last `*` was matched

    while t < text.len() {
        if p < pattern.len() && pattern[p] == b'*' {
            // Check for `**` (matches path separators)
            if p + 1 < pattern.len() && pattern[p + 1] == b'*' {
                // `**/` or `**` at end
                let skip = if p + 2 < pattern.len() && pattern[p + 2] == b'/' {
                    3
                } else {
                    2
                };
                // Try matching `**` against zero or more path segments
                let rest_pattern = &pattern[p + skip..];
                for i in t..=text.len() {
                    if glob_matches_recursive(rest_pattern, &text[i..]) {
                        return true;
                    }
                }
                return false;
            }
            // Single `*`: matches anything except `/`
            star_p = Some(p + 1);
            star_t = t;
            p += 1;
        } else if p < pattern.len()
            && ((pattern[p] == b'?' && text[t] != b'/') || pattern[p] == text[t])
        {
            p += 1;
            t += 1;
        } else if let Some(sp) = star_p {
            // Backtrack: single `*` cannot match `/`
            if text[star_t] == b'/' {
                return false;
            }
            star_t += 1;
            t = star_t;
            p = sp;
        } else {
            return false;
        }
    }

    // Consume trailing stars
    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }

    p == pattern.len()
}

/// Check if a file path matches any of the given glob patterns.
fn should_ignore_file(path: &str, patterns: &[String]) -> bool {
    let filename = std::path::Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");

    patterns
        .iter()
        .any(|pattern| glob_matches(pattern, path) || glob_matches(pattern, filename))
}

/// Load all effective ignore patterns: defaults + .git-ai-ignore + .gitattributes linguist-generated
fn load_effective_ignore_patterns() -> Vec<String> {
    let mut pattern_strings: Vec<String> = DEFAULT_IGNORE_PATTERNS
        .iter()
        .map(|p| p.to_string())
        .collect();

    // Load .git-ai-ignore from repo root
    if let Ok(toplevel) = git_cmd(&["rev-parse", "--show-toplevel"]) {
        let repo_root = Path::new(toplevel.trim());

        // .git-ai-ignore
        let ignore_path = repo_root.join(".git-ai-ignore");
        if let Ok(contents) = fs::read_to_string(&ignore_path) {
            for line in contents.lines() {
                let trimmed = line.trim();
                if !trimmed.is_empty() && !trimmed.starts_with('#') {
                    pattern_strings.push(trimmed.to_string());
                }
            }
        }

        // .gitattributes linguist-generated
        let gitattributes_path = repo_root.join(".gitattributes");
        if let Ok(contents) = fs::read_to_string(&gitattributes_path) {
            for line in contents.lines() {
                let trimmed = line.trim();
                if trimmed.is_empty() || trimmed.starts_with('#') {
                    continue;
                }
                let tokens: Vec<&str> = trimmed.split_whitespace().collect();
                if tokens.len() < 2 {
                    continue;
                }
                let path_pattern = tokens[0];
                if path_pattern.starts_with("[attr]") {
                    continue;
                }
                let is_generated = tokens[1..].iter().any(|attr| {
                    *attr == "linguist-generated"
                        || attr.eq_ignore_ascii_case("linguist-generated=true")
                        || *attr == "linguist-generated=1"
                });
                if is_generated {
                    pattern_strings.push(path_pattern.to_string());
                }
            }
        }
    }

    pattern_strings
}

/// Returns true if a diff section describes a binary file.
fn is_binary_diff_section(section_text: &str) -> bool {
    section_text
        .lines()
        .any(|line| line.starts_with("Binary files"))
}

/// Parse the diff --git header to extract file paths.
/// Returns (old_path, new_path).
fn parse_diff_git_header(line: &str) -> Option<(String, String)> {
    let rest = line.strip_prefix("diff --git ")?;
    // Format: "a/path b/path"
    if let Some(pos) = rest.find(" b/") {
        let old = rest[2..pos].to_string(); // skip "a/"
        let new = rest[pos + 3..].to_string(); // skip " b/"
        Some((old, new))
    } else {
        None
    }
}

/// Parse hunk header to extract new-file start line.
/// Format: @@ -old_start[,old_count] +new_start[,new_count] @@
fn parse_hunk_header_start(line: &str) -> Option<u32> {
    let rest = line.strip_prefix("@@ ")?;
    let plus_pos = rest.find('+')?;
    let after_plus = &rest[plus_pos + 1..];
    let end = after_plus.find([',', ' ']).unwrap_or(after_plus.len());
    after_plus[..end].parse::<u32>().ok()
}

/// Split a unified diff into per-file sections.
/// Returns Vec<(file_path, section_text)>, filtering out binary sections.
fn split_diff_into_sections(diff_text: &str) -> Vec<(String, String)> {
    let mut sections: Vec<(String, String)> = Vec::new();
    let mut current_file = String::new();
    let mut current_section = String::new();

    for line in diff_text.lines() {
        if line.starts_with("diff --git ") {
            // Flush previous section
            if !current_file.is_empty() && !current_section.is_empty() {
                sections.push((current_file.clone(), current_section.clone()));
            }
            current_section.clear();
            current_file.clear();

            if let Some((_old, new)) = parse_diff_git_header(line) {
                current_file = new;
            }
            current_section.push_str(line);
            current_section.push('\n');
        } else if current_section.is_empty() {
            // Skip lines before first diff header
            continue;
        } else {
            // Check for +++ line to get actual file path (handles renames, new files)
            if line.starts_with("+++ ")
                && let Some(path) = line.strip_prefix("+++ b/")
            {
                current_file = path.to_string();
            }
            // "+++ /dev/null" means file deletion - keep old file path
            current_section.push_str(line);
            current_section.push('\n');
        }
    }

    // Flush last section
    if !current_file.is_empty() && !current_section.is_empty() {
        sections.push((current_file, current_section));
    }

    // Filter out binary sections
    sections.retain(|(_, text)| !is_binary_diff_section(text));

    sections
}

/// Run git diff and return the raw text output with standard a/b prefix,
/// using lossy UTF-8 conversion.
fn get_diff_text_with_prefix(from_commit: &str, to_commit: &str) -> Result<String, String> {
    let output = git_command()
        .args([
            "diff",
            "--no-color",
            "--no-ext-diff",
            from_commit,
            to_commit,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("failed to run git diff: {}", e))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        Err(format!("git diff failed: {}", stderr))
    }
}

/// Get file content at a specific commit.
fn get_file_at_commit(file_path: &str, commit: &str) -> String {
    let output = git_command()
        .args(["show", &format!("{}:{}", commit, file_path)])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output();

    match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
        _ => String::new(),
    }
}

pub fn handle_diff(args: &[String]) {
    let is_json = args.iter().any(|a| a == "--json");
    let include_stats = args.iter().any(|a| a == "--include-stats");
    let all_prompts = args.iter().any(|a| a == "--all-prompts");
    let pass_through_args: Vec<&str> = args
        .iter()
        .filter(|a| *a != "--json" && *a != "--include-stats" && *a != "--all-prompts")
        .map(|s| s.as_str())
        .collect();

    // Parse the commit spec from positional args
    let positional: Vec<&&str> = pass_through_args
        .iter()
        .filter(|a| !a.starts_with('-'))
        .collect();

    // Validate: "..." (triple dots) is not supported
    if let Some(arg) = positional.first() {
        if **arg == "..." {
            eprintln!("git-ai diff: invalid range format '...'");
            process::exit(1);
        }
        if arg.contains("...") {
            eprintln!("git-ai diff: triple-dot ranges are not supported");
            process::exit(1);
        }
    }

    // Determine from_commit and to_commit
    let (from_commit, to_commit) = if positional.is_empty() {
        eprintln!("git-ai diff: requires a commit or commit range argument");
        process::exit(1);
    } else if positional.len() == 2 {
        // Two positional args: treat as <from> <to>
        let from = match git_cmd(&["rev-parse", positional[0]]) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("git-ai diff: {}", e);
                process::exit(1);
            }
        };
        let to = match git_cmd(&["rev-parse", positional[1]]) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("git-ai diff: {}", e);
                process::exit(1);
            }
        };
        (from, to)
    } else {
        let arg = positional[0];
        if arg.contains("..") {
            // Range: "A..B"
            let parts: Vec<&str> = arg.split("..").collect();
            if parts.len() != 2 || parts[0].is_empty() || parts[1].is_empty() {
                eprintln!("git-ai diff: invalid range format");
                process::exit(1);
            }
            let from = match git_cmd(&["rev-parse", parts[0]]) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("git-ai diff: {}", e);
                    process::exit(1);
                }
            };
            let to = match git_cmd(&["rev-parse", parts[1]]) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("git-ai diff: {}", e);
                    process::exit(1);
                }
            };
            (from, to)
        } else {
            // Single commit: diff against its parent
            let to = match git_cmd(&["rev-parse", arg]) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("git-ai diff: {}", e);
                    process::exit(1);
                }
            };
            let from = git_cmd(&["rev-parse", &format!("{}^", to)]).unwrap_or_default();
            if from.is_empty() {
                // Initial commit: use empty tree
                let empty_tree = git_cmd(&["hash-object", "-t", "tree", "/dev/null"])
                    .unwrap_or_else(|_| "4b825dc642cb6eb9a060e54bf899d69f82623700".to_string());
                (empty_tree, to)
            } else {
                (from, to)
            }
        }
    };

    // Load ignore patterns
    let ignore_patterns = load_effective_ignore_patterns();

    if !is_json {
        // Terminal mode: run git diff but filter out ignored files
        let diff_text = match get_diff_text_with_prefix(&from_commit, &to_commit) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("git-ai diff: {}", e);
                process::exit(1);
            }
        };

        let sections = split_diff_into_sections(&diff_text);
        let mut output = String::new();
        for (file_path, section_text) in &sections {
            if should_ignore_file(file_path, &ignore_patterns) {
                continue;
            }
            output.push_str(section_text);
        }

        if !output.is_empty() {
            print!("{}", output);
        }
        return;
    }

    // JSON mode: produce the expected structure
    // { files: {}, prompts: {}, hunks: [], commits: {}, sessions: {} }

    // Get the raw diff text (with standard prefix for the diff field)
    let diff_text_prefixed = match get_diff_text_with_prefix(&from_commit, &to_commit) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("git-ai diff: {}", e);
            process::exit(1);
        }
    };

    let sections = split_diff_into_sections(&diff_text_prefixed);

    // Filter sections by ignore patterns
    let filtered_sections: Vec<&(String, String)> = sections
        .iter()
        .filter(|(file_path, _)| !should_ignore_file(file_path, &ignore_patterns))
        .collect();

    // Load authorship notes for the to_commit (and potentially other commits in range)
    let mut commit_authorship: HashMap<String, Option<AuthorshipLog>> = HashMap::new();

    // For single-commit mode, load the note for to_commit
    let to_note = git_cmd(&["notes", "--ref=ai", "show", &to_commit])
        .ok()
        .and_then(|note| AuthorshipLog::deserialize_from_string(&note).ok());
    commit_authorship.insert(to_commit.clone(), to_note.clone());

    // For range mode, also collect intermediate commits
    if from_commit != to_commit
        && let Ok(log_output) = git_cmd(&[
            "log",
            "--format=%H",
            &format!("{}..{}", from_commit, to_commit),
        ])
    {
        for sha in log_output.lines() {
            let sha = sha.trim();
            if sha.is_empty() || sha == to_commit {
                continue;
            }
            let note = git_cmd(&["notes", "--ref=ai", "show", sha])
                .ok()
                .and_then(|n| AuthorshipLog::deserialize_from_string(&n).ok());
            commit_authorship.insert(sha.to_string(), note);
        }
    }

    // Build the output maps
    let mut files_map: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
    let mut all_hunks: Vec<serde_json::Value> = Vec::new();
    let mut prompts_map: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
    let mut sessions_map: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
    let mut commits_map: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();

    for (file_path, section_text) in &filtered_sections {
        // Get base content
        let base_content = get_file_at_commit(file_path, &from_commit);

        // Build annotations for this file from authorship notes
        let mut annotations: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();

        // Parse the diff section to find added lines and their line numbers
        let mut new_line_num: u32 = 0;
        let mut in_hunk = false;
        let mut added_lines: Vec<u32> = Vec::new();

        for line in section_text.lines() {
            if line.starts_with("@@ ") {
                in_hunk = true;
                if let Some(start) = parse_hunk_header_start(line) {
                    new_line_num = start;
                }
                continue;
            }
            if !in_hunk {
                continue;
            }
            if line.starts_with('+') {
                added_lines.push(new_line_num);
                new_line_num += 1;
            } else if line.starts_with('-') {
                // Deleted line, don't advance new line counter
            } else {
                // Context line
                new_line_num += 1;
            }
        }

        // Look up attributions from authorship notes
        for (commit_sha, maybe_note) in &commit_authorship {
            if let Some(note) = maybe_note {
                let file_attestation = note.attestations.iter().find(|fa| {
                    let attest_path = fa.file_path.strip_prefix("./").unwrap_or(&fa.file_path);
                    attest_path == file_path.as_str()
                });

                if let Some(fa) = file_attestation {
                    for entry in &fa.entries {
                        let mut lines_for_hash: Vec<u32> = Vec::new();
                        for &added_line in &added_lines {
                            if entry.line_ranges.iter().any(|r| r.contains(added_line)) {
                                lines_for_hash.push(added_line);
                            }
                        }
                        if !lines_for_hash.is_empty() {
                            // Build line range representation for annotations
                            let ranges = git_ai::core::authorship_log::LineRange::compress_lines(
                                &lines_for_hash,
                            );
                            let range_values: Vec<serde_json::Value> = ranges
                                .iter()
                                .map(|r| match r {
                                    git_ai::core::authorship_log::LineRange::Single(l) => {
                                        serde_json::Value::Number((*l).into())
                                    }
                                    git_ai::core::authorship_log::LineRange::Range(s, e) => {
                                        serde_json::json!([s, e])
                                    }
                                })
                                .collect();

                            annotations
                                .insert(entry.hash.clone(), serde_json::Value::Array(range_values));

                            // Build hunk entries
                            use sha2::{Digest, Sha256};
                            let content_for_hash = lines_for_hash
                                .iter()
                                .map(|l| l.to_string())
                                .collect::<Vec<_>>()
                                .join(",");
                            let content_hash = {
                                let mut hasher = Sha256::new();
                                hasher.update(
                                    format!("{}:{}:{}", file_path, entry.hash, content_for_hash)
                                        .as_bytes(),
                                );
                                format!("{:x}", hasher.finalize())[..16].to_string()
                            };

                            let start_line = *lines_for_hash.first().unwrap();
                            let end_line = *lines_for_hash.last().unwrap();

                            let mut hunk = serde_json::json!({
                                "commit_sha": commit_sha,
                                "content_hash": content_hash,
                                "hunk_kind": "addition",
                                "start_line": start_line,
                                "end_line": end_line,
                                "file_path": file_path,
                            });

                            // Add prompt_id or human_id
                            if entry.hash.starts_with("h_") {
                                hunk["human_id"] = serde_json::Value::String(entry.hash.clone());
                            } else {
                                hunk["prompt_id"] = serde_json::Value::String(entry.hash.clone());
                                // session_id is the session portion (before ::)
                                let session_id =
                                    entry.hash.split("::").next().unwrap_or(&entry.hash);
                                if session_id.starts_with("s_") {
                                    hunk["session_id"] =
                                        serde_json::Value::String(session_id.to_string());
                                }
                            }

                            all_hunks.push(hunk);

                            // Collect prompts/sessions metadata
                            if let Some(prompt) = note.metadata.prompts.get(&entry.hash) {
                                prompts_map.insert(
                                    entry.hash.clone(),
                                    serde_json::to_value(prompt).unwrap_or(serde_json::json!({})),
                                );
                            }
                            {
                                let session_key =
                                    entry.hash.split("::").next().unwrap_or(&entry.hash);
                                if let Some(session) = note.metadata.sessions.get(session_key) {
                                    sessions_map.insert(
                                        session_key.to_string(),
                                        serde_json::to_value(session)
                                            .unwrap_or(serde_json::json!({})),
                                    );
                                }
                            }
                        }
                    }

                    // Also add sessions from the note that landed lines
                    for (session_id, session) in &note.metadata.sessions {
                        let has_landed = fa.entries.iter().any(|e| {
                            e.hash == *session_id
                                || e.hash.starts_with(&format!("{}::", session_id))
                        });
                        if has_landed && !sessions_map.contains_key(session_id) {
                            sessions_map.insert(
                                session_id.clone(),
                                serde_json::to_value(session).unwrap_or(serde_json::json!({})),
                            );
                        }
                    }
                }
            }
        }

        // Add commit metadata for to_commit
        if !commits_map.contains_key(&to_commit)
            && let Some(metadata) = get_commit_metadata(&to_commit)
        {
            commits_map.insert(to_commit.clone(), metadata);
        }

        files_map.insert(
            file_path.clone(),
            serde_json::json!({
                "annotations": annotations,
                "diff": section_text,
                "base_content": base_content,
            }),
        );
    }

    // For --all-prompts, include all sessions from authorship note
    if all_prompts && let Some(note) = &to_note {
        for (session_id, session) in &note.metadata.sessions {
            if !sessions_map.contains_key(session_id) {
                sessions_map.insert(
                    session_id.clone(),
                    serde_json::to_value(session).unwrap_or(serde_json::json!({})),
                );
            }
        }
        for (prompt_id, prompt) in &note.metadata.prompts {
            if !prompts_map.contains_key(prompt_id) {
                prompts_map.insert(
                    prompt_id.clone(),
                    serde_json::to_value(prompt).unwrap_or(serde_json::json!({})),
                );
            }
        }
    }

    let mut result = serde_json::json!({
        "files": files_map,
        "prompts": prompts_map,
        "hunks": all_hunks,
        "commits": commits_map,
    });

    // Add sessions if non-empty
    if !sessions_map.is_empty() {
        result["sessions"] = serde_json::Value::Object(sessions_map);
    }

    // Add commit_stats if --include-stats requested
    if include_stats
        && let Some(stats) =
            compute_commit_stats(&commit_authorship, &to_commit, &filtered_sections)
    {
        result["commit_stats"] = stats;
    }

    println!("{}", serde_json::to_string(&result).unwrap());
}

/// Get metadata for a commit (author, time, message).
fn get_commit_metadata(commit_sha: &str) -> Option<serde_json::Value> {
    let format_str = "%aI%n%an <%ae>%n%s%n%B";
    let output = git_cmd(&["log", "-1", &format!("--format={}", format_str), commit_sha]).ok()?;
    let lines: Vec<&str> = output.lines().collect();
    if lines.len() < 3 {
        return None;
    }
    let authored_time = lines[0].to_string();
    let author = lines[1].to_string();
    let msg = lines[2].to_string();
    let full_msg = lines[2..].join("\n");

    let authorship_note = git_cmd(&["notes", "--ref=ai", "show", commit_sha]).ok();

    Some(serde_json::json!({
        "authored_time": authored_time,
        "msg": msg,
        "full_msg": full_msg,
        "author": author,
        "authorship_note": authorship_note,
    }))
}

/// Compute commit stats for --include-stats flag.
#[allow(dead_code)]
fn compute_commit_stats(
    commit_authorship: &HashMap<String, Option<AuthorshipLog>>,
    to_commit: &str,
    filtered_sections: &[&(String, String)],
) -> Option<serde_json::Value> {
    let note = commit_authorship.get(to_commit)?.as_ref()?;

    let mut ai_lines_added: u32 = 0;
    let mut human_lines_added: u32 = 0;
    let mut unknown_lines_added: u32 = 0;
    let mut git_lines_added: u32 = 0;
    let mut git_lines_deleted: u32 = 0;
    let mut tool_model_breakdown: serde_json::Map<String, serde_json::Value> =
        serde_json::Map::new();

    // Count git-level adds/deletes from diff
    for (_, section_text) in filtered_sections {
        let mut in_hunk = false;
        for line in section_text.lines() {
            if line.starts_with("@@ ") {
                in_hunk = true;
                continue;
            }
            if !in_hunk {
                continue;
            }
            if line.starts_with('+') {
                git_lines_added += 1;
            } else if line.starts_with('-') {
                git_lines_deleted += 1;
            }
        }
    }

    // Count from attestations
    for fa in &note.attestations {
        for entry in &fa.entries {
            let count: u32 = entry.line_ranges.iter().map(|r| r.line_count()).sum();
            if entry.hash.starts_with("h_") {
                human_lines_added += count;
            } else if entry.hash.starts_with("s_")
                || note.metadata.sessions.contains_key(&entry.hash)
            {
                ai_lines_added += count;
                let session_key = entry.hash.split("::").next().unwrap_or(&entry.hash);
                if let Some(session) = note.metadata.sessions.get(session_key) {
                    let key = format!("{}::{}", session.agent_id.tool, session.agent_id.model);
                    let existing = tool_model_breakdown
                        .entry(key)
                        .or_insert_with(|| serde_json::json!({"ai_lines_added": 0}));
                    if let Some(n) = existing.get("ai_lines_added").and_then(|v| v.as_u64()) {
                        existing["ai_lines_added"] = serde_json::json!(n + count as u64);
                    }
                } else if let Some(prompt) = note.metadata.prompts.get(&entry.hash) {
                    let key = format!("{}::{}", prompt.agent_id.tool, prompt.agent_id.model);
                    let existing = tool_model_breakdown
                        .entry(key)
                        .or_insert_with(|| serde_json::json!({"ai_lines_added": 0}));
                    if let Some(n) = existing.get("ai_lines_added").and_then(|v| v.as_u64()) {
                        existing["ai_lines_added"] = serde_json::json!(n + count as u64);
                    }
                }
            } else if note.metadata.prompts.contains_key(&entry.hash) {
                ai_lines_added += count;
                let prompt = &note.metadata.prompts[&entry.hash];
                let key = format!("{}::{}", prompt.agent_id.tool, prompt.agent_id.model);
                let existing = tool_model_breakdown
                    .entry(key)
                    .or_insert_with(|| serde_json::json!({"ai_lines_added": 0}));
                if let Some(n) = existing.get("ai_lines_added").and_then(|v| v.as_u64()) {
                    existing["ai_lines_added"] = serde_json::json!(n + count as u64);
                }
            } else {
                unknown_lines_added += count;
            }
        }
    }

    Some(serde_json::json!({
        "ai_lines_added": ai_lines_added,
        "human_lines_added": human_lines_added,
        "unknown_lines_added": unknown_lines_added,
        "git_lines_added": git_lines_added,
        "git_lines_deleted": git_lines_deleted,
        "tool_model_breakdown": tool_model_breakdown,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    // =========================================================================
    // glob_matches tests
    // =========================================================================

    #[test]
    fn glob_star_matches_chars_except_slash() {
        assert!(glob_matches("*.lock", "Cargo.lock"));
        assert!(glob_matches("*.lock", "package.lock"));
        assert!(!glob_matches("*.lock", "dir/Cargo.lock"));
    }

    #[test]
    fn glob_double_star_matches_path_separators() {
        assert!(glob_matches("**/vendor/**", "some/vendor/file.js"));
        assert!(glob_matches("**/vendor/**", "a/b/vendor/c/d"));
        assert!(glob_matches(
            "**/node_modules/**",
            "project/node_modules/pkg/index.js"
        ));
    }

    #[test]
    fn glob_question_mark_matches_single_non_slash_char() {
        assert!(glob_matches("?.txt", "a.txt"));
        assert!(!glob_matches("?.txt", "ab.txt"));
        assert!(!glob_matches("?.txt", "/a.txt"));
    }

    #[test]
    fn glob_exact_match() {
        assert!(glob_matches("Cargo.lock", "Cargo.lock"));
        assert!(!glob_matches("Cargo.lock", "cargo.lock"));
        assert!(!glob_matches("Cargo.lock", "Cargo.lock.bak"));
    }

    #[test]
    fn glob_no_match_cases() {
        assert!(!glob_matches("*.rs", "file.txt"));
        assert!(!glob_matches("src/*.rs", "test/main.rs"));
        assert!(!glob_matches("foo", "bar"));
    }

    #[test]
    fn glob_empty_pattern_and_text() {
        assert!(glob_matches("", ""));
        assert!(!glob_matches("", "something"));
        assert!(!glob_matches("something", ""));
    }

    #[test]
    fn glob_star_at_end() {
        assert!(glob_matches("src/*", "src/main.rs"));
        assert!(!glob_matches("src/*", "src/sub/main.rs"));
    }

    #[test]
    fn glob_double_star_at_end() {
        assert!(glob_matches("src/**", "src/main.rs"));
        assert!(glob_matches("src/**", "src/sub/main.rs"));
    }

    // =========================================================================
    // should_ignore_file tests
    // =========================================================================

    #[test]
    fn should_ignore_file_matches_full_path() {
        let patterns = vec!["**/vendor/**".to_string()];
        assert!(should_ignore_file("some/vendor/lib.js", &patterns));
    }

    #[test]
    fn should_ignore_file_matches_filename_only() {
        let patterns = vec!["*.lock".to_string()];
        // The full path contains a directory, but the filename "Cargo.lock" matches "*.lock"
        assert!(should_ignore_file("some/dir/Cargo.lock", &patterns));
    }

    #[test]
    fn should_ignore_file_returns_false_when_no_match() {
        let patterns = vec!["*.lock".to_string(), "**/vendor/**".to_string()];
        assert!(!should_ignore_file("src/main.rs", &patterns));
    }

    #[test]
    fn should_ignore_file_empty_patterns() {
        let patterns: Vec<String> = vec![];
        assert!(!should_ignore_file("anything.rs", &patterns));
    }

    // =========================================================================
    // parse_diff_git_header tests
    // =========================================================================

    #[test]
    fn parse_diff_git_header_normal_case() {
        let result = parse_diff_git_header("diff --git a/src/foo.rs b/src/foo.rs");
        assert_eq!(
            result,
            Some(("src/foo.rs".to_string(), "src/foo.rs".to_string()))
        );
    }

    #[test]
    fn parse_diff_git_header_rename() {
        let result = parse_diff_git_header("diff --git a/old.rs b/new.rs");
        assert_eq!(result, Some(("old.rs".to_string(), "new.rs".to_string())));
    }

    #[test]
    fn parse_diff_git_header_nested_paths() {
        let result =
            parse_diff_git_header("diff --git a/src/commands/diff.rs b/src/commands/diff.rs");
        assert_eq!(
            result,
            Some((
                "src/commands/diff.rs".to_string(),
                "src/commands/diff.rs".to_string()
            ))
        );
    }

    #[test]
    fn parse_diff_git_header_invalid_input() {
        assert_eq!(parse_diff_git_header("not a diff header"), None);
        assert_eq!(parse_diff_git_header("diff --git a/foo"), None);
        assert_eq!(parse_diff_git_header(""), None);
    }

    // =========================================================================
    // parse_hunk_header_start tests
    // =========================================================================

    #[test]
    fn parse_hunk_header_start_basic() {
        assert_eq!(parse_hunk_header_start("@@ -1,5 +3,7 @@"), Some(3));
    }

    #[test]
    fn parse_hunk_header_start_single_line() {
        assert_eq!(parse_hunk_header_start("@@ -0,0 +1 @@"), Some(1));
    }

    #[test]
    fn parse_hunk_header_start_with_function_context() {
        assert_eq!(
            parse_hunk_header_start("@@ -10 +20,5 @@ fn foo()"),
            Some(20)
        );
    }

    #[test]
    fn parse_hunk_header_start_large_numbers() {
        assert_eq!(parse_hunk_header_start("@@ -100,50 +200,60 @@"), Some(200));
    }

    #[test]
    fn parse_hunk_header_start_invalid_input() {
        assert_eq!(parse_hunk_header_start("not a hunk header"), None);
        assert_eq!(parse_hunk_header_start(""), None);
        assert_eq!(parse_hunk_header_start("--- a/file.rs"), None);
    }

    // =========================================================================
    // split_diff_into_sections tests
    // =========================================================================

    #[test]
    fn split_diff_single_file() {
        let diff = "\
diff --git a/src/main.rs b/src/main.rs
index abc1234..def5678 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,3 +1,4 @@
 fn main() {
+    println!(\"hello\");
 }
";
        let sections = split_diff_into_sections(diff);
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].0, "src/main.rs");
    }

    #[test]
    fn split_diff_multiple_files() {
        let diff = "\
diff --git a/src/a.rs b/src/a.rs
--- a/src/a.rs
+++ b/src/a.rs
@@ -1,2 +1,3 @@
 line1
+line2
diff --git a/src/b.rs b/src/b.rs
--- a/src/b.rs
+++ b/src/b.rs
@@ -1,2 +1,3 @@
 foo
+bar
";
        let sections = split_diff_into_sections(diff);
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].0, "src/a.rs");
        assert_eq!(sections[1].0, "src/b.rs");
    }

    #[test]
    fn split_diff_filters_binary_sections() {
        let diff = "\
diff --git a/image.png b/image.png
Binary files a/image.png and b/image.png differ
diff --git a/src/main.rs b/src/main.rs
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,2 +1,3 @@
 fn main() {}
+// comment
";
        let sections = split_diff_into_sections(diff);
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].0, "src/main.rs");
    }

    #[test]
    fn split_diff_empty_input() {
        let sections = split_diff_into_sections("");
        assert!(sections.is_empty());
    }

    #[test]
    fn split_diff_detects_new_file_path_from_plus_plus_b() {
        let diff = "\
diff --git a/old_name.rs b/new_name.rs
similarity index 90%
rename from old_name.rs
rename to new_name.rs
--- a/old_name.rs
+++ b/new_name.rs
@@ -1,2 +1,3 @@
 fn foo() {}
+fn bar() {}
";
        let sections = split_diff_into_sections(diff);
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].0, "new_name.rs");
    }

    // =========================================================================
    // is_binary_diff_section tests
    // =========================================================================

    #[test]
    fn is_binary_diff_section_true() {
        let section =
            "diff --git a/img.png b/img.png\nBinary files a/img.png and b/img.png differ\n";
        assert!(is_binary_diff_section(section));
    }

    #[test]
    fn is_binary_diff_section_false() {
        let section = "diff --git a/src/main.rs b/src/main.rs\n--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1 +1,2 @@\n+hello\n";
        assert!(!is_binary_diff_section(section));
    }

    #[test]
    fn is_binary_diff_section_empty() {
        assert!(!is_binary_diff_section(""));
    }
}
