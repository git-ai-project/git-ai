use git_ai::core::authorship_log::AuthorshipLog;
use git_ai::core::git_binary::git_cmd as git_command;

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::process::{self, Stdio};

use crate::commands::blame::{
    BlameLineData, load_authorship_note, resolve_line_author_with_prompt,
};
use crate::commands::diff::DEFAULT_IGNORE_PATTERNS;
use crate::commands::helpers::git_cmd;

pub fn handle_internal_command(cmd: &str, args: &[String]) {
    // All internal machine commands require --json flag
    let is_json = args.iter().any(|a| a == "--json");
    if !is_json {
        eprintln!(
            "{}",
            serde_json::json!({ "error": format!("internal command '{}' requires --json flag", cmd) })
        );
        process::exit(1);
    }

    // The request payload is the positional argument after --json (or any arg starting with '{')
    let request_str: Option<&str> = args
        .iter()
        .skip_while(|a| a.as_str() != "--json")
        .nth(1)
        .map(|s| s.as_str())
        .or_else(|| args.iter().find(|a| a.starts_with('{')).map(|s| s.as_str()));

    match cmd {
        "effective-ignore-patterns" => {
            let repo_root = git_cmd(&["rev-parse", "--show-toplevel"]).unwrap_or_default();
            let mut all_patterns: Vec<String> = DEFAULT_IGNORE_PATTERNS
                .iter()
                .map(|s| s.to_string())
                .collect();

            // Read .git-ai-ignore if present
            let ignore_file = PathBuf::from(&repo_root).join(".git-ai-ignore");
            if ignore_file.exists()
                && let Ok(content) = fs::read_to_string(&ignore_file)
            {
                for line in content.lines() {
                    let trimmed = line.trim();
                    if !trimmed.is_empty() && !trimmed.starts_with('#') {
                        all_patterns.push(trimmed.to_string());
                    }
                }
            }

            // Read .gitattributes for linguist-generated patterns
            let gitattributes_file = PathBuf::from(&repo_root).join(".gitattributes");
            if gitattributes_file.exists()
                && let Ok(content) = fs::read_to_string(&gitattributes_file)
            {
                for line in content.lines() {
                    let trimmed = line.trim();
                    if trimmed.contains("linguist-generated")
                        && let Some(pattern) = trimmed.split_whitespace().next()
                    {
                        all_patterns.push(pattern.to_string());
                    }
                }
            }

            // Include user_patterns and extra_patterns from the request
            if let Some(req) = request_str
                && let Ok(parsed) = serde_json::from_str::<serde_json::Value>(req)
            {
                if let Some(user_pats) = parsed["user_patterns"].as_array() {
                    for p in user_pats {
                        if let Some(s) = p.as_str() {
                            all_patterns.push(s.to_string());
                        }
                    }
                }
                if let Some(extra_pats) = parsed["extra_patterns"].as_array() {
                    for p in extra_pats {
                        if let Some(s) = p.as_str() {
                            all_patterns.push(s.to_string());
                        }
                    }
                }
            }

            // Deduplicate while preserving order
            let mut seen = std::collections::HashSet::new();
            all_patterns.retain(|p| seen.insert(p.clone()));

            println!("{}", serde_json::json!({ "patterns": all_patterns }));
        }
        "blame-analysis" => {
            let req = match request_str {
                Some(r) => r,
                None => {
                    eprintln!("{}", serde_json::json!({ "error": "missing request JSON" }));
                    process::exit(1);
                }
            };
            let parsed: serde_json::Value =
                serde_json::from_str(req).unwrap_or(serde_json::json!({}));
            let file = parsed["file_path"]
                .as_str()
                .or_else(|| parsed["file"].as_str())
                .unwrap_or("");
            let _commit = parsed["commit"].as_str().unwrap_or("HEAD");
            let options = &parsed["options"];
            let return_human_as_human = options["return_human_authors_as_human"]
                .as_bool()
                .unwrap_or(false);
            let line_ranges: Vec<(u32, u32)> = options["line_ranges"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|r| {
                            let pair = r.as_array()?;
                            Some((
                                pair.first()?.as_u64()? as u32,
                                pair.get(1)?.as_u64()? as u32,
                            ))
                        })
                        .collect()
                })
                .unwrap_or_default();

            // Run git blame for full file
            let blame_result = git_cmd(&["blame", "--line-porcelain", "--", file]);
            match blame_result {
                Ok(output) => {
                    // Parse blame output
                    let mut blame_lines: Vec<BlameLineData> = Vec::new();
                    let mut cur_sha = String::new();
                    let mut cur_orig_line: u32 = 0;
                    let mut cur_final_line: u32 = 0;
                    let mut cur_author = String::new();
                    let mut cur_author_email = String::new();
                    let mut cur_author_time: i64 = 0;
                    let mut cur_author_tz = String::new();
                    let mut cur_headers: Vec<String> = Vec::new();

                    for line in output.lines() {
                        if line.is_empty() {
                            continue;
                        }
                        if let Some(content) = line.strip_prefix('\t') {
                            blame_lines.push(BlameLineData {
                                commit_sha: cur_sha.clone(),
                                orig_line: cur_orig_line,
                                final_line: cur_final_line,
                                author: cur_author.clone(),
                                author_email: cur_author_email.clone(),
                                author_time: cur_author_time,
                                author_tz: cur_author_tz.clone(),
                                content: content.to_string(),
                                raw_headers: cur_headers.clone(),
                            });
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
                        if let Some(rest) = line.strip_prefix("author-time ") {
                            cur_author_time = rest.trim().parse().unwrap_or(0);
                            cur_headers.push(line.to_string());
                            continue;
                        }
                        if let Some(rest) = line.strip_prefix("author-tz ") {
                            cur_author_tz = rest.trim().to_string();
                            cur_headers.push(line.to_string());
                            continue;
                        }
                        if let Some(rest) = line.strip_prefix("author ") {
                            cur_author = rest.to_string();
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

                    // Load notes for relevant commits
                    let mut commit_notes: HashMap<String, Option<AuthorshipLog>> = HashMap::new();
                    for bl in &blame_lines {
                        if !commit_notes.contains_key(&bl.commit_sha) {
                            let note = load_authorship_note(&bl.commit_sha);
                            commit_notes.insert(bl.commit_sha.clone(), note);
                        }
                    }

                    // Build line_authors for requested ranges
                    let mut line_authors: serde_json::Map<String, serde_json::Value> =
                        serde_json::Map::new();
                    let mut prompt_records: serde_json::Map<String, serde_json::Value> =
                        serde_json::Map::new();

                    for bl in &blame_lines {
                        // Filter by line_ranges if specified
                        if !line_ranges.is_empty() {
                            let in_range = line_ranges.iter().any(|(start, end)| {
                                bl.final_line >= *start && bl.final_line <= *end
                            });
                            if !in_range {
                                continue;
                            }
                        }

                        let (author_display, prompt_hash) = resolve_line_author_with_prompt(
                            &bl.commit_sha,
                            bl.orig_line,
                            &bl.author,
                            &bl.author_email,
                            file,
                            &commit_notes,
                            &bl.raw_headers,
                        );

                        let display = if let Some(ref hash) = prompt_hash {
                            // Collect prompt record
                            if !prompt_records.contains_key(hash)
                                && let Some(Some(log)) = commit_notes.get(&bl.commit_sha)
                                && let Some(prompt) = log.metadata.prompts.get(hash)
                            {
                                prompt_records.insert(hash.clone(), serde_json::json!({
                                            "agent_id": { "tool": prompt.agent_id.tool, "model": prompt.agent_id.model },
                                        }));
                            }
                            author_display
                        } else if return_human_as_human {
                            "human".to_string()
                        } else {
                            author_display
                        };

                        line_authors.insert(
                            bl.final_line.to_string(),
                            serde_json::Value::String(display),
                        );
                    }

                    // Build blame hunks
                    let mut blame_hunks: Vec<serde_json::Value> = Vec::new();
                    for bl in &blame_lines {
                        if !line_ranges.is_empty() {
                            let in_range = line_ranges.iter().any(|(start, end)| {
                                bl.final_line >= *start && bl.final_line <= *end
                            });
                            if !in_range {
                                continue;
                            }
                        }
                        blame_hunks.push(serde_json::json!({
                            "commit": bl.commit_sha,
                            "line": bl.final_line,
                            "author": bl.author,
                            "content": bl.content,
                        }));
                    }

                    println!(
                        "{}",
                        serde_json::json!({
                            "line_authors": line_authors,
                            "prompt_records": prompt_records,
                            "blame_hunks": blame_hunks,
                        })
                    );
                }
                Err(e) => {
                    eprintln!("{}", serde_json::json!({ "error": e }));
                    process::exit(1);
                }
            }
        }
        "fetch-authorship-notes" | "fetch_authorship_notes" => {
            let remote = if let Some(req) = request_str {
                let parsed: serde_json::Value =
                    serde_json::from_str(req).unwrap_or(serde_json::json!({}));
                parsed["remote_name"]
                    .as_str()
                    .or_else(|| parsed["remote"].as_str())
                    .unwrap_or("origin")
                    .to_string()
            } else {
                "origin".to_string()
            };

            // Try to fetch; determine if notes exist on remote
            let result = git_command()
                .args(["fetch", &remote, "+refs/notes/ai:refs/notes/ai"])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output();

            match result {
                Ok(output) if output.status.success() => {
                    println!("{}", serde_json::json!({ "notes_existence": "found" }));
                }
                Ok(output) => {
                    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                    // "couldn't find remote ref" means notes don't exist
                    if stderr.contains("couldn't find remote ref") || stderr.contains("not found") {
                        println!("{}", serde_json::json!({ "notes_existence": "not_found" }));
                    } else {
                        println!("{}", serde_json::json!({ "notes_existence": "found" }));
                    }
                }
                Err(_) => {
                    println!("{}", serde_json::json!({ "notes_existence": "not_found" }));
                }
            }
        }
        "push-authorship-notes" => {
            let remote = if let Some(req) = request_str {
                let parsed: serde_json::Value =
                    serde_json::from_str(req).unwrap_or(serde_json::json!({}));
                parsed["remote_name"]
                    .as_str()
                    .or_else(|| parsed["remote"].as_str())
                    .unwrap_or("origin")
                    .to_string()
            } else {
                "origin".to_string()
            };

            // Check if local refs/notes/ai exists; if not, nothing to push
            let has_local_notes = git_cmd(&["rev-parse", "--verify", "refs/notes/ai"]).is_ok();
            if !has_local_notes {
                println!("{}", serde_json::json!({ "ok": true }));
                return;
            }

            // Retry up to 3 times for concurrent push (non-fast-forward)
            for attempt in 0..3 {
                // On retry attempts (or after first non-fast-forward), fetch and merge
                if attempt > 0 {
                    let _ = git_command()
                        .args([
                            "fetch",
                            &remote,
                            "+refs/notes/ai:refs/notes/ai-remote/origin",
                        ])
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status();
                    // Try to merge remote notes with cat_sort_uniq
                    let merge_ok = git_command()
                        .args([
                            "notes",
                            "--ref=ai",
                            "merge",
                            "-s",
                            "cat_sort_uniq",
                            "refs/notes/ai-remote/origin",
                        ])
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status()
                        .map(|s| s.success())
                        .unwrap_or(false);
                    if !merge_ok {
                        let _ = git_command()
                            .args(["notes", "--ref=ai", "merge", "--abort"])
                            .stdout(Stdio::null())
                            .stderr(Stdio::null())
                            .status();
                        // Fallback: merge with ours strategy
                        let ours_ok = git_command()
                            .args([
                                "notes",
                                "--ref=ai",
                                "merge",
                                "-s",
                                "ours",
                                "refs/notes/ai-remote/origin",
                            ])
                            .stdout(Stdio::null())
                            .stderr(Stdio::null())
                            .status()
                            .map(|s| s.success())
                            .unwrap_or(false);
                        if !ours_ok {
                            let _ = git_command()
                                .args(["notes", "--ref=ai", "merge", "--abort"])
                                .stdout(Stdio::null())
                                .stderr(Stdio::null())
                                .status();
                            // All merge strategies failed (corrupted remote tree).
                            // Force push our local notes as last resort.
                            let force_result = git_command()
                                .args(["push", "--force", &remote, "refs/notes/ai:refs/notes/ai"])
                                .stdout(Stdio::piped())
                                .stderr(Stdio::piped())
                                .output();
                            if let Ok(out) = force_result
                                && out.status.success()
                            {
                                println!("{}", serde_json::json!({ "ok": true }));
                                return;
                            }
                        }
                    }
                }

                let result = git_command()
                    .args(["push", &remote, "refs/notes/ai:refs/notes/ai"])
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .output();
                match result {
                    Ok(output) if output.status.success() => {
                        println!("{}", serde_json::json!({ "ok": true }));
                        return;
                    }
                    Ok(output) => {
                        let err_msg = String::from_utf8_lossy(&output.stderr).trim().to_string();
                        if err_msg.contains("non-fast-forward") || err_msg.contains("fetch first") {
                            continue;
                        }
                        break;
                    }
                    Err(_e) => {
                        break;
                    }
                }
            }
            // Even if push fails after retries, report ok (best effort)
            println!("{}", serde_json::json!({ "ok": true }));
        }
        _ => {
            eprintln!(
                "{}",
                serde_json::json!({ "error": format!("unknown internal command: {}", cmd) })
            );
            process::exit(1);
        }
    }
}
