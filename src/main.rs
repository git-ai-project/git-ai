use git_ai::core::attribution::{
    Attribution, attributions_to_line_attributions, update_attributions,
};
use git_ai::core::authorship_log::AuthorshipLog;
use git_ai::core::post_commit::generate_authorship_for_commit;
use git_ai::core::working_log::{AgentId, Checkpoint, CheckpointKind, WorkingLogEntry};

use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{self, Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

fn debug_log(msg: &str) {
    if cfg!(debug_assertions) || env::var("GIT_AI_DEBUG").as_deref() == Ok("1") {
        eprintln!("[git-ai] {}", msg);
    }
}

fn git_cmd(args: &[&str]) -> Result<String, String> {
    let output = Command::new("/usr/bin/git")
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("failed to run git: {}", e))?;

    if output.status.success() {
        // Use trim_end (not trim) to preserve leading whitespace in porcelain output
        Ok(String::from_utf8_lossy(&output.stdout)
            .trim_end()
            .to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(format!("git {} failed: {}", args.join(" "), stderr))
    }
}

/// Run a git command from a specific working directory.
fn git_cmd_in(dir: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("/usr/bin/git")
        .args(args)
        .current_dir(dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("failed to run git: {}", e))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout)
            .trim_end()
            .to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(format!("git {} failed: {}", args.join(" "), stderr))
    }
}

/// Given an absolute file path, find the git repository root that contains it.
/// Walks up from the file's parent directory looking for `.git/` (directory or file for worktrees).
fn find_repo_root_for_path(file_path: &Path) -> Option<PathBuf> {
    let start_dir = if file_path.is_dir() {
        file_path.to_path_buf()
    } else {
        file_path.parent()?.to_path_buf()
    };

    let mut current = start_dir.as_path();
    loop {
        let git_path = current.join(".git");
        if git_path.exists() {
            return Some(current.to_path_buf());
        }
        match current.parent() {
            Some(parent) => current = parent,
            None => return None,
        }
    }
}

// ---------------------------------------------------------------------------
// Checkpoint command
// ---------------------------------------------------------------------------

/// Try to route the checkpoint through the daemon's control socket.
/// Returns true if the daemon handled it, false if we need to fall back to local processing.
fn try_checkpoint_via_daemon(args: &[String]) -> bool {
    // Don't route to daemon if explicitly disabled
    if env::var("GIT_AI_NO_DAEMON").as_deref() == Ok("1") {
        return false;
    }

    // Agent presets (claude, cursor, etc.) require local processing with hook input parsing;
    // the daemon control socket doesn't support preset checkpoint requests.
    if let Some(first_arg) = args.first() {
        let name = first_arg.as_str();
        if git_ai::presets::known_presets().contains(&name)
            && !matches!(name, "human" | "mock_ai" | "mock_known_human")
        {
            return false;
        }
    }

    let paths = git_ai::daemon::DaemonPaths::resolve();
    if !paths.control_sock.exists() {
        return false;
    }

    // Parse args to build request
    let mut kind_str: Option<&str> = None;
    let mut file_args: Vec<&str> = Vec::new();
    let mut past_separator = false;

    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        if arg == "--" {
            past_separator = true;
            i += 1;
            continue;
        }
        if past_separator {
            file_args.push(arg);
        } else if kind_str.is_none() && matches!(arg, "human" | "mock_ai" | "mock_known_human") {
            kind_str = Some(arg);
        } else {
            file_args.push(arg);
        }
        i += 1;
    }

    let kind = match kind_str {
        Some("mock_ai") => "ai",
        Some("mock_known_human") => "known_human",
        Some("human") | None => "human",
        _ => "human",
    };

    // Check if any file args are absolute paths (cross-repo scenario)
    let has_absolute_paths = file_args.iter().any(|f| PathBuf::from(f).is_absolute());

    if has_absolute_paths && !file_args.is_empty() {
        // Cross-repo mode: group files by their containing repository and send
        // separate daemon requests per repo
        let mut repo_groups: HashMap<PathBuf, Vec<&str>> = HashMap::new();
        for f in &file_args {
            let p = PathBuf::from(f);
            if !p.is_absolute() {
                // Mix of absolute and relative -- fall back to local processing
                return false;
            }
            if let Some(repo_root) = find_repo_root_for_path(&p) {
                repo_groups.entry(repo_root).or_default().push(f);
            }
        }

        if repo_groups.is_empty() {
            println!("0");
            return true;
        }

        let mut total_processed: u64 = 0;
        for (repo_root, files) in &repo_groups {
            let repo_root_str = repo_root.to_string_lossy().to_string();
            let file_values: Vec<serde_json::Value> = files
                .iter()
                .map(|f| {
                    // Make path relative to this repo root for the daemon
                    let p = PathBuf::from(f);
                    let rel = p
                        .strip_prefix(repo_root)
                        .unwrap_or(&p)
                        .to_string_lossy()
                        .replace('\\', "/");
                    serde_json::json!({"path": rel})
                })
                .collect();

            let mut request = serde_json::json!({
                "type": "checkpoint",
                "repo_dir": repo_root_str,
                "kind": kind,
                "files": file_values,
            });

            if kind == "ai" {
                let ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0);
                request["agent"] = serde_json::json!({
                    "tool": kind_str.unwrap_or("mock_ai"),
                    "id": format!("ai-thread-{}", ts),
                    "model": "unknown"
                });
            }

            let request_str = serde_json::to_string(&request).unwrap_or_default();

            match git_ai::daemon::control_client::send_request(&paths.control_sock, &request_str) {
                Ok(resp) if resp.ok => {
                    total_processed += resp.processed.unwrap_or(0) as u64;
                }
                _ => return false,
            }
        }

        println!("{}", total_processed);
        return true;
    }

    // Standard mode: single repo from CWD
    let repo_root = match git_cmd(&["rev-parse", "--show-toplevel"]) {
        Ok(r) => r,
        Err(_) => return false,
    };

    let files: Vec<serde_json::Value> = if file_args.is_empty() {
        let status_output = git_cmd(&["status", "--porcelain", "-u"]).unwrap_or_default();
        status_output
            .lines()
            .filter(|l| l.len() > 3)
            .map(|l| serde_json::json!({"path": l[3..].trim()}))
            .collect()
    } else {
        file_args
            .iter()
            .map(|f| serde_json::json!({"path": f}))
            .collect()
    };

    if files.is_empty() {
        println!("0");
        return true;
    }

    let mut request = serde_json::json!({
        "type": "checkpoint",
        "repo_dir": repo_root,
        "kind": kind,
        "files": files,
    });

    if kind == "ai" {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        request["agent"] = serde_json::json!({
            "tool": kind_str.unwrap_or("mock_ai"),
            "id": format!("ai-thread-{}", ts),
            "model": "unknown"
        });
    }

    let request_str = serde_json::to_string(&request).unwrap_or_default();

    match git_ai::daemon::control_client::send_request(&paths.control_sock, &request_str) {
        Ok(resp) if resp.ok => {
            println!("{}", resp.processed.unwrap_or(0));
            true
        }
        _ => false,
    }
}

fn handle_checkpoint(args: &[String]) {
    // Try routing through the daemon's control socket for lower latency
    if try_checkpoint_via_daemon(args) {
        return;
    }

    let mut kind_str: Option<&str> = None;
    let mut file_args: Vec<&str> = Vec::new();
    let mut past_separator = false;

    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        if arg == "--" {
            past_separator = true;
            i += 1;
            continue;
        }
        if past_separator {
            file_args.push(arg);
        } else if kind_str.is_none() {
            kind_str = Some(arg);
        } else {
            file_args.push(arg);
        }
        i += 1;
    }

    let agent_name = kind_str.unwrap_or("human");

    // Check if this is a real agent preset (not a simple built-in kind)
    let is_agent_preset = git_ai::presets::known_presets().contains(&agent_name)
        && !matches!(agent_name, "human" | "mock_ai" | "mock_known_human");

    if is_agent_preset {
        handle_agent_checkpoint(agent_name, &file_args);
        return;
    }

    let kind = match agent_name {
        "mock_ai" => CheckpointKind::AiAgent,
        "mock_known_human" => CheckpointKind::KnownHuman,
        _ => CheckpointKind::Human,
    };

    // Check if any file args are absolute paths (cross-repo scenario)
    let has_absolute_paths = file_args.iter().any(|f| PathBuf::from(f).is_absolute());

    if has_absolute_paths && !file_args.is_empty() {
        // Cross-repo mode: group files by their containing repository and process each group
        let mut processed = 0;
        // Group files by repo root
        let mut repo_groups: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
        for f in &file_args {
            let p = PathBuf::from(f);
            if !p.is_absolute() || !p.exists() {
                continue;
            }
            if let Some(repo_root) = find_repo_root_for_path(&p) {
                repo_groups.entry(repo_root).or_default().push(p);
            }
        }

        for (repo_root_path, files) in &repo_groups {
            let git_dir = match git_cmd_in(repo_root_path, &["rev-parse", "--git-dir"]) {
                Ok(d) => {
                    let p = PathBuf::from(&d);
                    if p.is_relative() { repo_root_path.join(p) } else { p }
                }
                Err(_) => continue,
            };
            let base_commit = git_cmd_in(repo_root_path, &["rev-parse", "HEAD"])
                .unwrap_or_else(|_| "initial".to_string());

            for file_path in files {
                processed += process_checkpoint_file(
                    file_path,
                    repo_root_path,
                    &git_dir,
                    &base_commit,
                    kind,
                    kind_str,
                );
            }
        }
        println!("{}", processed);
        write_checkpoint_debug_log(agent_name, processed);
    } else {
        // Standard mode: all files relative to CWD repo
        let git_dir_str = match git_cmd(&["rev-parse", "--git-dir"]) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("git-ai: {}", e);
                process::exit(1);
            }
        };
        let git_dir = PathBuf::from(&git_dir_str);

        let base_commit =
            git_cmd(&["rev-parse", "HEAD"]).unwrap_or_else(|_| "initial".to_string());

        let repo_root =
            git_cmd(&["rev-parse", "--show-toplevel"]).unwrap_or_else(|_| ".".to_string());
        let repo_root_path = PathBuf::from(&repo_root);

        let files_to_process: Vec<PathBuf> = if file_args.is_empty() {
            let status_output = git_cmd(&["status", "--porcelain", "-u"]).unwrap_or_default();
            status_output
                .lines()
                .filter(|l| !l.is_empty())
                .filter_map(|l| {
                    if l.len() > 3 {
                        Some(repo_root_path.join(l[3..].trim()))
                    } else {
                        None
                    }
                })
                .filter(|p| p.exists())
                .collect()
        } else {
            file_args
                .iter()
                .map(|f| repo_root_path.join(f))
                .filter(|p| p.exists())
                .collect()
        };

        let mut processed = 0;

        for file_path in &files_to_process {
            processed += process_checkpoint_file(
                file_path,
                &repo_root_path,
                &git_dir,
                &base_commit,
                kind,
                kind_str,
            );
        }

        println!("{}", processed);

        // Write checkpoint debug log if feature flag is enabled
        write_checkpoint_debug_log(agent_name, processed);
    }
}

/// Write a checkpoint debug log entry if the checkpoint_debug_log feature flag is enabled.
fn write_checkpoint_debug_log(preset_name: &str, event_count: usize) {
    // Check if the feature flag is enabled via GIT_AI_TEST_CONFIG_PATCH
    let enabled = if let Ok(patch_json) = env::var("GIT_AI_TEST_CONFIG_PATCH") {
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&patch_json) {
            parsed["feature_flags"]["checkpoint_debug_log"].as_bool().unwrap_or(false)
        } else {
            false
        }
    } else {
        false
    };

    if !enabled {
        return;
    }

    let home = env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    let log_dir = PathBuf::from(&home).join(".git-ai").join("internal").join("checkpoint-debug-logs");
    if let Err(e) = fs::create_dir_all(&log_dir) {
        debug_log(&format!("failed to create checkpoint debug log dir: {}", e));
        return;
    }

    // Generate today's date for the filename
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = now.as_secs();
    // Simple date calculation (days since epoch)
    let days = secs / 86400;
    let filename = format!("{}.jsonl", days);
    let log_file = log_dir.join(&filename);

    let trace_id = format!("trace-{}", now.as_nanos());
    let timestamp = format!("{}Z", secs);

    let entry = serde_json::json!({
        "preset_name": preset_name,
        "trace_id": trace_id,
        "timestamp": timestamp,
        "event_count": event_count,
        "requests": [],
    });

    use std::io::Write;
    let mut file = match fs::OpenOptions::new().create(true).append(true).open(&log_file) {
        Ok(f) => f,
        Err(e) => {
            debug_log(&format!("failed to open checkpoint debug log: {}", e));
            return;
        }
    };
    let _ = writeln!(file, "{}", entry.to_string());
}

/// Process a single file for checkpoint, writing to the given repo's working log.
/// Returns 1 if processed, 0 if skipped.
fn process_checkpoint_file(
    file_path: &Path,
    repo_root_path: &Path,
    git_dir: &Path,
    base_commit: &str,
    kind: CheckpointKind,
    kind_str: Option<&str>,
) -> usize {
    let relative_path = file_path
        .strip_prefix(repo_root_path)
        .unwrap_or(file_path)
        .to_string_lossy()
        .replace('\\', "/");

    // Skip conflicted files (UU status in merge conflicts)
    if is_file_conflicted(repo_root_path, &relative_path) {
        debug_log(&format!("skipping conflicted file: {}", relative_path));
        return 0;
    }

    // Skip binary files (non-UTF8 content that's being replaced)
    if let Ok(bytes) = fs::read(file_path) {
        // Check if content is binary by looking for null bytes in first 8KB
        let check_len = bytes.len().min(8192);
        if bytes[..check_len].contains(&0) {
            debug_log(&format!("skipping binary file: {}", relative_path));
            return 0;
        }
    }

    // Suppression: skip KnownHuman checkpoints for files with a pending AI edit
    if kind == CheckpointKind::KnownHuman && has_pending_ai_edit(git_dir, &relative_path) {
        debug_log(&format!(
            "suppressing KnownHuman checkpoint for '{}' (pending AI edit)",
            relative_path
        ));
        return 0;
    }

    // For AI checkpoints, clear the pending AI edit marker
    if kind == CheckpointKind::AiAgent {
        clear_pending_ai_edit(git_dir, &relative_path);
    }

    let content = match fs::read_to_string(file_path) {
        Ok(c) => c,
        Err(_) => return 0,
    };

    let blob_sha =
        git_ai::core::working_log::save_blob(git_dir, base_commit, content.as_bytes());

    let existing_checkpoints =
        git_ai::core::working_log::read_checkpoints(git_dir, base_commit);
    let previous_attributions = find_latest_attributions(&existing_checkpoints, &relative_path);

    let previous_content = find_latest_content(
        &existing_checkpoints,
        &relative_path,
        git_dir,
        base_commit,
    );

    let checkpoint_agent_id = if kind == CheckpointKind::AiAgent {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        Some(AgentId {
            tool: kind_str.unwrap_or("mock_ai").to_string(),
            id: format!("ai-thread-{}", ts),
            model: "unknown".to_string(),
        })
    } else {
        None
    };

    // For KnownHuman, resolve the git user identity for both the author_id hash
    // and the checkpoint.author field — they must be consistent.
    let known_human_identity = if kind == CheckpointKind::KnownHuman {
        let name = git_cmd_in(repo_root_path, &["config", "user.name"])
            .unwrap_or_else(|_| "Unknown".to_string());
        let email = git_cmd_in(repo_root_path, &["config", "user.email"])
            .unwrap_or_else(|_| "unknown".to_string());
        Some(format!("{} <{}>", name, email))
    } else {
        None
    };

    let author_id = match &kind {
        CheckpointKind::AiAgent => {
            let aid = checkpoint_agent_id.as_ref().unwrap();
            git_ai::core::authorship_log::generate_session_id(&aid.tool, &aid.id)
        }
        CheckpointKind::KnownHuman => git_ai::core::authorship_log::generate_human_hash(
            known_human_identity.as_deref().unwrap(),
        ),
        CheckpointKind::Human => "human".to_string(),
    };
    let enable_move_detection =
        kind == CheckpointKind::Human || kind == CheckpointKind::KnownHuman;
    let new_attributions = update_attributions(
        &previous_content,
        &content,
        &previous_attributions,
        &author_id,
        enable_move_detection,
    );

    let line_attributions = attributions_to_line_attributions(&content, &new_attributions);

    let entry = WorkingLogEntry {
        file: relative_path.clone(),
        blob_sha,
        attributions: new_attributions,
        line_attributions,
    };

    let checkpoint_author = if let Some(ref identity) = known_human_identity {
        identity.clone()
    } else {
        kind_str.unwrap_or("human").to_string()
    };

    let mut checkpoint = Checkpoint::new(kind, checkpoint_author, vec![entry]);
    checkpoint.agent_id = checkpoint_agent_id.clone();
    if kind == CheckpointKind::AiAgent {
        checkpoint.trace_id = Some(format!(
            "trace-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
    }

    git_ai::core::working_log::append_checkpoint(git_dir, base_commit, &checkpoint);
    1
}

/// Handle checkpoint for real agent presets (cursor, claude, agent-v1, etc.).
/// Reads hook payload from stdin or --hook-input arg, parses it, and processes the resulting events.
fn handle_agent_checkpoint(agent_name: &str, file_args: &[&str]) {
    use git_ai::presets::ParsedHookEvent;

    // Check if --hook-input is provided as a flag in file_args
    let hook_input = {
        let mut input: Option<String> = None;
        let mut i = 0;
        while i < file_args.len() {
            if file_args[i] == "--hook-input" {
                if i + 1 < file_args.len() {
                    let value = file_args[i + 1];
                    if value == "stdin" {
                        break; // fall through to read from stdin
                    }
                    input = Some(value.to_string());
                }
                break;
            }
            i += 1;
        }
        input.unwrap_or_else(|| git_ai::presets::read_stdin())
    };

    let events = match git_ai::presets::parse_hook_input(agent_name, &hook_input) {
        Ok(events) => events,
        Err(e) => {
            debug_log(&format!("preset parse error: {}", e));
            println!("0");
            return;
        }
    };

    let mut processed = 0;

    for event in events {
        let is_pre_file_edit = matches!(&event, ParsedHookEvent::PreFileEdit(_));
        let is_post_file_edit = matches!(&event, ParsedHookEvent::PostFileEdit(_));

        let (kind, cwd, file_paths, agent_id, dirty_files): (CheckpointKind, PathBuf, Vec<PathBuf>, Option<AgentId>, Option<HashMap<PathBuf, String>>) = match event {
            ParsedHookEvent::PreFileEdit(e) => {
                (CheckpointKind::Human, e.context.cwd, e.file_paths, None, e.dirty_files)
            }
            ParsedHookEvent::PostFileEdit(e) => {
                let aid = AgentId {
                    tool: e.context.agent_tool.clone(),
                    id: e.context.agent_session_id.clone(),
                    model: e.context.agent_model.clone(),
                };
                (CheckpointKind::AiAgent, e.context.cwd, e.file_paths, Some(aid), e.dirty_files)
            }
            ParsedHookEvent::PreBashCall(e) => {
                (CheckpointKind::Human, e.context.cwd, vec![], None, None)
            }
            ParsedHookEvent::PostBashCall(e) => {
                let aid = AgentId {
                    tool: e.context.agent_tool.clone(),
                    id: e.context.agent_session_id.clone(),
                    model: e.context.agent_model.clone(),
                };
                (CheckpointKind::AiAgent, e.context.cwd, vec![], Some(aid), None)
            }
            ParsedHookEvent::KnownHumanEdit(e) => {
                (CheckpointKind::KnownHuman, e.cwd, e.file_paths, None, e.dirty_files)
            }
            ParsedHookEvent::UntrackedEdit(e) => {
                (CheckpointKind::Human, e.cwd, e.file_paths, None, None)
            }
        };

        // Filter out --hook-input and its value from file_args
        let actual_file_args: Vec<&str> = {
            let mut result = Vec::new();
            let mut skip_next = false;
            for arg in file_args {
                if skip_next {
                    skip_next = false;
                    continue;
                }
                if *arg == "--hook-input" {
                    skip_next = true;
                    continue;
                }
                result.push(*arg);
            }
            result
        };

        // If preset provided file paths, use those. Otherwise use file_args or scan.
        let raw_files: Vec<PathBuf> = if !file_paths.is_empty() {
            file_paths.clone()
        } else if !actual_file_args.is_empty() {
            actual_file_args.iter().map(|f| {
                let p = PathBuf::from(f);
                if p.is_absolute() { p } else { cwd.join(f) }
            }).collect()
        } else {
            // For bash tools, scan for all modified files from CWD
            let status_output = git_cmd_in(&cwd, &["status", "--porcelain", "-u"]).unwrap_or_default();
            let cwd_repo_root = git_cmd_in(&cwd, &["rev-parse", "--show-toplevel"])
                .unwrap_or_else(|_| cwd.to_string_lossy().to_string());
            let cwd_root = PathBuf::from(&cwd_repo_root);
            status_output.lines()
                .filter(|l| l.len() > 3)
                .map(|l| cwd_root.join(l[3..].trim()))
                .filter(|p| p.exists())
                .collect()
        };

        // Check if files contain absolute paths that might belong to different repos
        let has_absolute = raw_files.iter().any(|f| f.is_absolute());

        if has_absolute {
            // Cross-repo mode: group files by their containing repository
            let mut repo_groups: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
            for fp in &raw_files {
                if !fp.exists() {
                    continue;
                }
                let resolved = if fp.is_absolute() {
                    find_repo_root_for_path(fp)
                } else {
                    find_repo_root_for_path(&cwd.join(fp))
                };
                if let Some(repo_root) = resolved {
                    repo_groups.entry(repo_root).or_default().push(fp.clone());
                }
            }

            for (repo_root_path, files) in &repo_groups {
                let git_dir = match git_cmd_in(repo_root_path, &["rev-parse", "--git-dir"]) {
                    Ok(d) => {
                        let p = PathBuf::from(&d);
                        if p.is_relative() { repo_root_path.join(p) } else { p }
                    }
                    Err(_) => continue,
                };
                let base_commit = git_cmd_in(repo_root_path, &["rev-parse", "HEAD"])
                    .unwrap_or_else(|_| "initial".to_string());

                // For PreFileEdit events, register pending AI edit markers
                if is_pre_file_edit {
                    for fp in files {
                        let rel = fp.strip_prefix(repo_root_path)
                            .unwrap_or(fp)
                            .to_string_lossy()
                            .replace('\\', "/");
                        write_pending_ai_edit(&git_dir, &rel);
                    }
                }

                // For PostFileEdit (AI) events, clear pending AI edit markers
                if is_post_file_edit {
                    for fp in files {
                        let rel = fp.strip_prefix(repo_root_path)
                            .unwrap_or(fp)
                            .to_string_lossy()
                            .replace('\\', "/");
                        clear_pending_ai_edit(&git_dir, &rel);
                    }
                }

                for file_path in files {
                    // Allow processing even if file doesn't exist on disk
                    // when dirty_files provides content (e.g., create_file pre-edit with empty content)
                    let dirty_content = dirty_files.as_ref().and_then(|df| df.get(file_path));
                    if !file_path.exists() && dirty_content.is_none() {
                        continue;
                    }

                    let relative_path = file_path
                        .strip_prefix(repo_root_path)
                        .unwrap_or(file_path)
                        .to_string_lossy()
                        .replace('\\', "/");

                    // Suppression: skip KnownHuman checkpoints for files with a pending AI edit
                    if kind == CheckpointKind::KnownHuman && has_pending_ai_edit(&git_dir, &relative_path) {
                        debug_log(&format!(
                            "suppressing KnownHuman checkpoint for '{}' (pending AI edit)",
                            relative_path
                        ));
                        continue;
                    }

                    // Use dirty_files content if available, otherwise read from disk
                    let content = if let Some(dc) = dirty_content {
                        dc.clone()
                    } else {
                        match fs::read_to_string(file_path) {
                            Ok(c) => c,
                            Err(_) => continue,
                        }
                    };

                    let blob_sha =
                        git_ai::core::working_log::save_blob(&git_dir, &base_commit, content.as_bytes());

                    let existing_checkpoints =
                        git_ai::core::working_log::read_checkpoints(&git_dir, &base_commit);
                    let previous_attributions =
                        find_latest_attributions(&existing_checkpoints, &relative_path);
                    let previous_content = find_latest_content(
                        &existing_checkpoints,
                        &relative_path,
                        &git_dir,
                        &base_commit,
                    );

                    // For KnownHuman, resolve the full git identity (Name <email>)
                    let known_human_identity = if kind == CheckpointKind::KnownHuman {
                        let name = git_cmd_in(repo_root_path, &["config", "user.name"])
                            .unwrap_or_else(|_| "Unknown".to_string());
                        let email = git_cmd_in(repo_root_path, &["config", "user.email"])
                            .unwrap_or_else(|_| "unknown".to_string());
                        Some(format!("{} <{}>", name, email))
                    } else {
                        None
                    };

                    let author_id = match (&kind, &agent_id) {
                        (CheckpointKind::AiAgent, Some(aid)) => {
                            git_ai::core::authorship_log::generate_session_id(&aid.tool, &aid.id)
                        }
                        (CheckpointKind::KnownHuman, _) => {
                            git_ai::core::authorship_log::generate_human_hash(
                                known_human_identity.as_deref().unwrap(),
                            )
                        }
                        _ => "human".to_string(),
                    };

                    let enable_move_detection = kind == CheckpointKind::Human || kind == CheckpointKind::KnownHuman;
                    let new_attributions = update_attributions(
                        &previous_content,
                        &content,
                        &previous_attributions,
                        &author_id,
                        enable_move_detection,
                    );
                    let line_attributions = attributions_to_line_attributions(&content, &new_attributions);

                    let entry = WorkingLogEntry {
                        file: relative_path,
                        blob_sha,
                        attributions: new_attributions,
                        line_attributions,
                    };

                    let checkpoint_author = if let Some(ref aid) = agent_id {
                        aid.tool.clone()
                    } else if let Some(ref identity) = known_human_identity {
                        identity.clone()
                    } else {
                        agent_name.to_string()
                    };

                    let mut checkpoint = Checkpoint::new(kind, checkpoint_author, vec![entry]);
                    checkpoint.agent_id = agent_id.clone();
                    if kind == CheckpointKind::AiAgent {
                        checkpoint.trace_id = Some(format!(
                            "trace-{}",
                            SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .map(|d| d.as_nanos())
                                .unwrap_or(0)
                        ));
                    }

                    git_ai::core::working_log::append_checkpoint(&git_dir, &base_commit, &checkpoint);
                    processed += 1;
                }
            }
        } else {
            // Standard mode: all files relative to CWD repo
            let repo_root_path = cwd;
            let git_dir = match git_cmd_in(&repo_root_path, &["rev-parse", "--git-dir"]) {
                Ok(d) => {
                    let p = PathBuf::from(&d);
                    if p.is_relative() { repo_root_path.join(p) } else { p }
                }
                Err(_) => continue,
            };

            let base_commit = git_cmd_in(&repo_root_path, &["rev-parse", "HEAD"])
                .unwrap_or_else(|_| "initial".to_string());

            let files_to_process = &raw_files;

            // For PreFileEdit events, register pending AI edit markers
            if is_pre_file_edit {
                for fp in files_to_process {
                    let rel = fp.strip_prefix(&repo_root_path)
                        .unwrap_or(fp)
                        .to_string_lossy()
                        .replace('\\', "/");
                    write_pending_ai_edit(&git_dir, &rel);
                }
            }

            // For PostFileEdit (AI) events, clear pending AI edit markers
            if is_post_file_edit {
                for fp in files_to_process {
                    let rel = fp.strip_prefix(&repo_root_path)
                        .unwrap_or(fp)
                        .to_string_lossy()
                        .replace('\\', "/");
                    clear_pending_ai_edit(&git_dir, &rel);
                }
            }

            for file_path in files_to_process {
                // Allow processing even if file doesn't exist on disk
                // when dirty_files provides content (e.g., create_file pre-edit with empty content)
                let dirty_content = dirty_files.as_ref().and_then(|df| df.get(file_path));
                if !file_path.exists() && dirty_content.is_none() {
                    continue;
                }

                let relative_path = file_path
                    .strip_prefix(&repo_root_path)
                    .unwrap_or(file_path)
                    .to_string_lossy()
                    .replace('\\', "/");

                // Suppression: skip KnownHuman checkpoints for files with a pending AI edit
                if kind == CheckpointKind::KnownHuman && has_pending_ai_edit(&git_dir, &relative_path) {
                    debug_log(&format!(
                        "suppressing KnownHuman checkpoint for '{}' (pending AI edit)",
                        relative_path
                    ));
                    continue;
                }

                // Use dirty_files content if available, otherwise read from disk
                let content = if let Some(dc) = dirty_content {
                    dc.clone()
                } else {
                    match fs::read_to_string(file_path) {
                        Ok(c) => c,
                        Err(_) => continue,
                    }
                };

                let blob_sha =
                    git_ai::core::working_log::save_blob(&git_dir, &base_commit, content.as_bytes());

                let existing_checkpoints =
                    git_ai::core::working_log::read_checkpoints(&git_dir, &base_commit);
                let previous_attributions =
                    find_latest_attributions(&existing_checkpoints, &relative_path);
                let previous_content = find_latest_content(
                    &existing_checkpoints,
                    &relative_path,
                    &git_dir,
                    &base_commit,
                );

                // For KnownHuman, resolve the full git identity (Name <email>)
                let known_human_identity = if kind == CheckpointKind::KnownHuman {
                    let name = git_cmd_in(&repo_root_path, &["config", "user.name"])
                        .unwrap_or_else(|_| "Unknown".to_string());
                    let email = git_cmd_in(&repo_root_path, &["config", "user.email"])
                        .unwrap_or_else(|_| "unknown".to_string());
                    Some(format!("{} <{}>", name, email))
                } else {
                    None
                };

                let author_id = match (&kind, &agent_id) {
                    (CheckpointKind::AiAgent, Some(aid)) => {
                        git_ai::core::authorship_log::generate_session_id(&aid.tool, &aid.id)
                    }
                    (CheckpointKind::KnownHuman, _) => {
                        git_ai::core::authorship_log::generate_human_hash(
                            known_human_identity.as_deref().unwrap(),
                        )
                    }
                    _ => "human".to_string(),
                };

                let enable_move_detection = kind == CheckpointKind::Human || kind == CheckpointKind::KnownHuman;
                let new_attributions = update_attributions(
                    &previous_content,
                    &content,
                    &previous_attributions,
                    &author_id,
                    enable_move_detection,
                );
                let line_attributions = attributions_to_line_attributions(&content, &new_attributions);

                let entry = WorkingLogEntry {
                    file: relative_path,
                    blob_sha,
                    attributions: new_attributions,
                    line_attributions,
                };

                let checkpoint_author = if let Some(ref aid) = agent_id {
                    aid.tool.clone()
                } else if let Some(ref identity) = known_human_identity {
                    identity.clone()
                } else {
                    agent_name.to_string()
                };

                let mut checkpoint = Checkpoint::new(kind, checkpoint_author, vec![entry]);
                checkpoint.agent_id = agent_id.clone();
                if kind == CheckpointKind::AiAgent {
                    checkpoint.trace_id = Some(format!(
                        "trace-{}",
                        SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .map(|d| d.as_nanos())
                            .unwrap_or(0)
                    ));
                }

                git_ai::core::working_log::append_checkpoint(&git_dir, &base_commit, &checkpoint);
                processed += 1;
            }
        }
    }

    println!("{}", processed);
}

// ---------------------------------------------------------------------------
// Pending AI edit markers
// ---------------------------------------------------------------------------

/// Directory for pending AI edit markers: .git/ai/pending_ai_edits/
fn pending_ai_edits_dir(git_dir: &Path) -> PathBuf {
    git_dir.join("ai").join("pending_ai_edits")
}

/// Convert a relative file path to a safe marker filename (replace / with __)
fn marker_filename(relative_path: &str) -> String {
    relative_path.replace('/', "__")
}

/// Write a pending AI edit marker for the given file.
fn write_pending_ai_edit(git_dir: &Path, relative_path: &str) {
    let dir = pending_ai_edits_dir(git_dir);
    let _ = fs::create_dir_all(&dir);
    let marker_path = dir.join(marker_filename(relative_path));
    let _ = fs::write(&marker_path, "");
}

/// Check if a file has a pending AI edit marker.
/// Check if a file is in a conflicted state (e.g., UU during merge conflict).
fn is_file_conflicted(repo_root: &Path, relative_path: &str) -> bool {
    let output = Command::new("/usr/bin/git")
        .arg("-C")
        .arg(repo_root)
        .args(["status", "--porcelain", "--", relative_path])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output();
    if let Ok(out) = output {
        let status = String::from_utf8_lossy(&out.stdout);
        for line in status.lines() {
            if line.len() >= 2 {
                let xy = &line[..2];
                // UU = both modified (conflict), AA = both added, etc.
                if xy == "UU" || xy == "AA" || xy == "DU" || xy == "UD" {
                    return true;
                }
            }
        }
    }
    false
}

fn has_pending_ai_edit(git_dir: &Path, relative_path: &str) -> bool {
    let marker_path = pending_ai_edits_dir(git_dir).join(marker_filename(relative_path));
    marker_path.exists()
}

/// Clear the pending AI edit marker for the given file.
fn clear_pending_ai_edit(git_dir: &Path, relative_path: &str) {
    let marker_path = pending_ai_edits_dir(git_dir).join(marker_filename(relative_path));
    let _ = fs::remove_file(&marker_path);
}

fn find_latest_attributions(checkpoints: &[Checkpoint], relative_path: &str) -> Vec<Attribution> {
    for cp in checkpoints.iter().rev() {
        for entry in &cp.entries {
            if entry.file == relative_path && !entry.attributions.is_empty() {
                return entry.attributions.clone();
            }
        }
    }
    Vec::new()
}

fn find_latest_content(
    checkpoints: &[Checkpoint],
    relative_path: &str,
    git_dir: &Path,
    base_commit: &str,
) -> String {
    for cp in checkpoints.iter().rev() {
        for entry in &cp.entries {
            if entry.file == relative_path && !entry.blob_sha.is_empty() {
                if let Some(content) =
                    git_ai::core::working_log::read_blob(git_dir, base_commit, &entry.blob_sha)
                {
                    return content;
                }
            }
        }
    }

    if base_commit != "initial" {
        if let Ok(content) = git_cmd(&["show", &format!("{}:{}", base_commit, relative_path)]) {
            return content;
        }
    }

    String::new()
}

// ---------------------------------------------------------------------------
// Post-commit command (called by .git/hooks/post-commit or explicitly)
// ---------------------------------------------------------------------------

fn handle_post_commit() {
    let git_dir_str = match git_cmd(&["rev-parse", "--git-dir"]) {
        Ok(d) => d,
        Err(_) => return,
    };
    let git_dir =
        std::fs::canonicalize(&git_dir_str).unwrap_or_else(|_| PathBuf::from(&git_dir_str));

    let commit_sha = match git_cmd(&["rev-parse", "HEAD"]) {
        Ok(s) => s,
        Err(_) => return,
    };

    let parent_sha = git_cmd(&["rev-parse", "HEAD~1"]).ok();
    let base_commit = parent_sha.as_deref().unwrap_or("initial");

    let repo_dir = git_cmd(&["rev-parse", "--show-toplevel"])
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    let human_author = git_cmd(&["log", "-1", "--format=%aN <%aE>"])
        .unwrap_or_else(|_| "Unknown <unknown>".to_string());

    let (mut authorship_log, initial_attrs) = match generate_authorship_for_commit(
        &git_dir,
        &repo_dir,
        base_commit,
        &commit_sha,
        &human_author,
    ) {
        Ok(result) => result,
        Err(_) => return,
    };

    // Background cloud agent: when GIT_AI_CLOUD_AGENT=1 is set, attribute all
    // unattributed committed lines to AI. This covers no-hooks agents that don't
    // fire their own checkpoints.
    if env::var("GIT_AI_CLOUD_AGENT").as_deref() == Ok("1") {
        // Only apply on normal commits, not during rebase/cherry-pick
        let is_rewriting = git_dir.join("rebase-merge").exists()
            || git_dir.join("rebase-apply").exists()
            || git_dir.join("CHERRY_PICK_HEAD").exists();

        if !is_rewriting {
            let committed_lines =
                git_ai::core::post_commit::git_diff_committed_lines(&repo_dir, base_commit, &commit_sha);

            // Build a synthetic session ID for the background agent
            let bg_session_id =
                git_ai::core::authorship_log::generate_session_id("cloud-agent", &commit_sha);

            // Determine which committed lines are already attributed
            use std::collections::{HashMap as StdHashMap, HashSet as StdHashSet};
            let mut already_attributed: StdHashMap<&str, StdHashSet<u32>> = StdHashMap::new();
            for file_att in &authorship_log.attestations {
                let line_set = already_attributed
                    .entry(file_att.file_path.as_str())
                    .or_default();
                for entry in &file_att.entries {
                    for range in &entry.line_ranges {
                        match range {
                            git_ai::core::authorship_log::LineRange::Single(l) => {
                                line_set.insert(*l);
                            }
                            git_ai::core::authorship_log::LineRange::Range(s, e) => {
                                for l in *s..=*e {
                                    line_set.insert(l);
                                }
                            }
                        }
                    }
                }
            }

            // For each committed file, find unattributed lines and add them
            let mut bg_attestations: StdHashMap<String, Vec<u32>> = StdHashMap::new();
            for (file_path, lines) in &committed_lines {
                let attributed = already_attributed.get(file_path.as_str());
                for &line in lines {
                    let is_covered = attributed.map(|s| s.contains(&line)).unwrap_or(false);
                    if !is_covered {
                        bg_attestations
                            .entry(file_path.clone())
                            .or_default()
                            .push(line);
                    }
                }
            }

            // Add attestation entries for background agent lines
            if !bg_attestations.is_empty() {
                // Register the session in metadata
                authorship_log.metadata.sessions.insert(
                    bg_session_id.clone(),
                    git_ai::core::authorship_log::SessionRecord {
                        agent_id: git_ai::core::authorship_log::AgentId {
                            tool: "cloud-agent".to_string(),
                            id: commit_sha.clone(),
                            model: "unknown".to_string(),
                        },
                        human_author: Some(human_author.clone()),
                        custom_attributes: None,
                    },
                );

                for (file_path, mut lines) in bg_attestations {
                    lines.sort_unstable();
                    lines.dedup();
                    let ranges = git_ai::core::authorship_log::LineRange::compress_lines(&lines);

                    // Check if there's an existing attestation for this file
                    let existing = authorship_log
                        .attestations
                        .iter_mut()
                        .find(|fa| fa.file_path == file_path);

                    if let Some(file_att) = existing {
                        file_att.entries.push(git_ai::core::authorship_log::AttestationEntry {
                            hash: bg_session_id.clone(),
                            line_ranges: ranges,
                        });
                    } else {
                        authorship_log
                            .attestations
                            .push(git_ai::core::authorship_log::FileAttestation {
                                file_path,
                                entries: vec![git_ai::core::authorship_log::AttestationEntry {
                                    hash: bg_session_id.clone(),
                                    line_ranges: ranges,
                                }],
                            });
                    }
                }
            }
        }
    }

    let note_text = authorship_log.serialize_to_string();
    let result = Command::new("/usr/bin/git")
        .args([
            "notes",
            "--ref=ai",
            "add",
            "-f",
            "-m",
            &note_text,
            &commit_sha,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .status();

    match result {
        Ok(status) if status.success() => {
            debug_log(&format!(
                "wrote authorship note for {}",
                &commit_sha[..7.min(commit_sha.len())]
            ));
        }
        Ok(_) => debug_log("git notes add failed"),
        Err(e) => debug_log(&format!("failed to run git notes: {}", e)),
    }

    if let Some(initial) = initial_attrs {
        git_ai::core::working_log::write_initial_attributions(&git_dir, &commit_sha, &initial);
    }

    git_ai::core::working_log::delete_working_log(&git_dir, base_commit);
}

// ---------------------------------------------------------------------------
// Blame command
// ---------------------------------------------------------------------------

fn handle_blame(args: &[String]) {
    if args.is_empty() {
        eprintln!("usage: git-ai blame <file>");
        process::exit(1);
    }

    // Detect output mode flags (git-ai specific, not passed to git)
    #[derive(PartialEq)]
    enum BlameOutputMode {
        Default,
        Porcelain,
        LinePorcelain,
        Incremental,
        Json,
    }

    let mut output_mode = BlameOutputMode::Default;
    let mut blame_flags: Vec<String> = Vec::new();
    let mut file_path_arg: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--json" {
            output_mode = BlameOutputMode::Json;
            i += 1;
        } else if args[i] == "--porcelain" {
            output_mode = BlameOutputMode::Porcelain;
            i += 1;
        } else if args[i] == "--line-porcelain" {
            output_mode = BlameOutputMode::LinePorcelain;
            i += 1;
        } else if args[i] == "--incremental" {
            output_mode = BlameOutputMode::Incremental;
            i += 1;
        } else if args[i] == "-L" {
            if i + 1 < args.len() {
                blame_flags.push(args[i].clone());
                blame_flags.push(args[i + 1].clone());
                i += 2;
            } else {
                eprintln!("git-ai blame: -L requires a range argument");
                process::exit(1);
            }
        } else if args[i].starts_with("-L") {
            blame_flags.push(args[i].clone());
            i += 1;
        } else if args[i].starts_with('-') {
            blame_flags.push(args[i].clone());
            i += 1;
        } else {
            file_path_arg = Some(args[i].clone());
            i += 1;
        }
    }

    let file_path = match file_path_arg {
        Some(p) => p,
        None => {
            eprintln!("usage: git-ai blame <file>");
            process::exit(1);
        }
    };

    // Resolve the file path to repo-relative for authorship note lookups.
    // git blame resolves from cwd, but authorship notes store paths relative to repo root.
    let repo_relative_file_path = {
        let prefix = git_cmd(&["rev-parse", "--show-prefix"]).unwrap_or_default();
        let candidate = if prefix.is_empty() {
            file_path.clone()
        } else {
            format!("{}{}", prefix, file_path)
        };
        // Normalize: resolve .. and . components
        let p = PathBuf::from(&candidate);
        let mut components: Vec<String> = Vec::new();
        for comp in p.components() {
            match comp {
                std::path::Component::ParentDir => { components.pop(); }
                std::path::Component::CurDir => {}
                std::path::Component::Normal(s) => { components.push(s.to_string_lossy().to_string()); }
                _ => {}
            }
        }
        components.join("/")
    };

    // Build the git blame command (always use --line-porcelain for parsing)
    let mut blame_args: Vec<&str> = vec!["blame", "--line-porcelain"];
    for flag in &blame_flags {
        blame_args.push(flag.as_str());
    }
    blame_args.push("--");
    blame_args.push(&file_path);

    let blame_output = match git_cmd(&blame_args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("git-ai blame: {}", e);
            process::exit(1);
        }
    };

    let mut lines: Vec<BlameLineData> = Vec::new();
    let mut cur_sha = String::new();
    let mut cur_orig_line: u32 = 0;
    let mut cur_final_line: u32 = 0;
    let mut cur_author = String::new();
    let mut cur_author_email = String::new();
    let mut cur_author_time: i64 = 0;
    let mut cur_author_tz = String::new();
    let mut cur_headers: Vec<String> = Vec::new();

    for line in blame_output.lines() {
        if line.is_empty() {
            continue;
        }
        if line.starts_with('\t') {
            lines.push(BlameLineData {
                commit_sha: cur_sha.clone(),
                orig_line: cur_orig_line,
                final_line: cur_final_line,
                author: cur_author.clone(),
                author_email: cur_author_email.clone(),
                author_time: cur_author_time,
                author_tz: cur_author_tz.clone(),
                content: line[1..].to_string(),
                raw_headers: cur_headers.clone(),
            });
            cur_headers.clear();
            continue;
        }
        if let Some(rest) = line.strip_prefix("author-mail ") {
            cur_author_email = rest.trim_start_matches('<').trim_end_matches('>').to_string();
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

    let mut commit_notes: HashMap<String, Option<AuthorshipLog>> = HashMap::new();
    for blame_line in &lines {
        if !commit_notes.contains_key(&blame_line.commit_sha) {
            let note = load_authorship_note(&blame_line.commit_sha);
            commit_notes.insert(blame_line.commit_sha.clone(), note);
        }
    }

    match output_mode {
        BlameOutputMode::Json => {
            blame_output_json(&lines, &repo_relative_file_path, &commit_notes);
        }
        BlameOutputMode::Porcelain
        | BlameOutputMode::LinePorcelain
        | BlameOutputMode::Incremental => {
            blame_output_porcelain(&lines, &repo_relative_file_path, &commit_notes);
        }
        BlameOutputMode::Default => {
            blame_output_default(&lines, &repo_relative_file_path, &commit_notes);
        }
    }
}

/// Detect if an author email belongs to a known AI agent.
fn detect_agent_from_email(email: &str) -> Option<&'static str> {
    let email_lower = email.to_lowercase();
    if email_lower == "noreply@anthropic.com" {
        return Some("claude");
    }
    if email_lower == "noreply@openai.com" {
        return Some("codex");
    }
    if email_lower.contains("copilot") {
        return Some("github-copilot");
    }
    if email_lower.contains("devin") {
        return Some("devin");
    }
    if email_lower.ends_with("@cursor.com") {
        return Some("cursor");
    }
    None
}

struct BlameLineData {
    commit_sha: String,
    orig_line: u32,
    final_line: u32,
    author: String,
    author_email: String,
    author_time: i64,
    author_tz: String,
    content: String,
    raw_headers: Vec<String>,
}

fn resolve_line_author(
    commit_sha: &str,
    orig_line: u32,
    git_author: &str,
    author_email: &str,
    file_path: &str,
    commit_notes: &HashMap<String, Option<AuthorshipLog>>,
    raw_headers: &[String],
) -> String {
    let (author, _) = resolve_line_author_with_prompt(
        commit_sha, orig_line, git_author, author_email, file_path, commit_notes, raw_headers,
    );
    author
}

fn resolve_line_author_with_prompt(
    commit_sha: &str,
    orig_line: u32,
    git_author: &str,
    author_email: &str,
    file_path: &str,
    commit_notes: &HashMap<String, Option<AuthorshipLog>>,
    raw_headers: &[String],
) -> (String, Option<String>) {
    if let Some(Some(authorship_log)) = commit_notes.get(commit_sha) {
        // Extract the original filename from blame porcelain headers (handles renames)
        let orig_filename: Option<&str> = raw_headers.iter().find_map(|h| {
            h.strip_prefix("filename ")
        });

        for file_attest in &authorship_log.attestations {
            let attest_path = file_attest
                .file_path
                .strip_prefix("./")
                .unwrap_or(&file_attest.file_path);
            let query_path = file_path.strip_prefix("./").unwrap_or(file_path);
            // Match against the queried file path OR the original filename from blame
            let matches = attest_path == query_path
                || orig_filename.map_or(false, |orig| {
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
                if let Some(prompt) = authorship_log.metadata.prompts.get(&entry.hash) {
                    return (prompt.agent_id.tool.clone(), Some(entry.hash.clone()));
                }
                if entry.hash.starts_with("h_") {
                    return (git_author.to_string(), None);
                }
                if entry.hash.starts_with("s_") {
                    if let Some(session) = authorship_log.metadata.sessions.get(&entry.hash) {
                        return (session.agent_id.tool.clone(), Some(entry.hash.clone()));
                    }
                }
            }
        }
    }
    if let Some(agent_name) = detect_agent_from_email(author_email) {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(commit_sha.as_bytes());
        hasher.update(b"_agent_email_");
        hasher.update(author_email.as_bytes());
        let hash_bytes = hasher.finalize();
        let prompt_hash = format!("{:x}", hash_bytes).chars().take(16).collect::<String>();
        return (agent_name.to_string(), Some(prompt_hash));
    }
    (git_author.to_string(), None)
}

fn blame_output_default(
    lines: &[BlameLineData],
    file_path: &str,
    commit_notes: &HashMap<String, Option<AuthorshipLog>>,
) {
    let line_num_width = lines.len().to_string().len();
    let mut max_author_width = 0;
    for bl in lines {
        let a = resolve_line_author(&bl.commit_sha, bl.orig_line, &bl.author, &bl.author_email, file_path, commit_notes, &bl.raw_headers);
        max_author_width = max_author_width.max(a.len());
    }
    for bl in lines {
        let short_sha = &bl.commit_sha[..7.min(bl.commit_sha.len())];
        let display_author = resolve_line_author(&bl.commit_sha, bl.orig_line, &bl.author, &bl.author_email, file_path, commit_notes, &bl.raw_headers);
        let date_str = format_blame_date(bl.author_time, &bl.author_tz);
        println!("{} ({:<width$} {} {:>lwidth$}) {}", short_sha, display_author, date_str, bl.final_line, bl.content, width = max_author_width, lwidth = line_num_width);
    }
}

fn blame_output_porcelain(
    lines: &[BlameLineData],
    file_path: &str,
    commit_notes: &HashMap<String, Option<AuthorshipLog>>,
) {
    for bl in lines {
        let display_author = resolve_line_author(&bl.commit_sha, bl.orig_line, &bl.author, &bl.author_email, file_path, commit_notes, &bl.raw_headers);
        for header in &bl.raw_headers {
            if header.starts_with("author ") && !header.starts_with("author-") {
                println!("author {}", display_author);
            } else {
                println!("{}", header);
            }
        }
        println!("\t{}", bl.content);
    }
}

fn blame_output_json(
    lines: &[BlameLineData],
    file_path: &str,
    commit_notes: &HashMap<String, Option<AuthorshipLog>>,
) {
    use std::collections::BTreeMap;
    let mut line_authors: BTreeMap<u32, String> = BTreeMap::new();
    let mut prompts: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();

    for bl in lines {
        let (author_display, prompt_hash) = resolve_line_author_with_prompt(
            &bl.commit_sha, bl.orig_line, &bl.author, &bl.author_email, file_path, commit_notes, &bl.raw_headers,
        );
        if let Some(hash) = &prompt_hash {
            line_authors.insert(bl.final_line, hash.clone());
            if !prompts.contains_key(hash) {
                if let Some(Some(log)) = commit_notes.get(&bl.commit_sha) {
                    if let Some(prompt) = log.metadata.prompts.get(hash) {
                        prompts.insert(hash.clone(), serde_json::json!({
                            "agent_id": { "tool": prompt.agent_id.tool, "model": prompt.agent_id.model, "id": prompt.agent_id.id },
                            "accepted_lines": prompt.accepted_lines,
                            "total_additions": prompt.total_additions,
                            "overriden_lines": prompt.overriden_lines,
                            "total_deletions": prompt.total_deletions,
                        }));
                    }
                }
                if !prompts.contains_key(hash) {
                    if let Some(agent_name) = detect_agent_from_email(&bl.author_email) {
                        let total_lines = lines.iter().filter(|l| l.commit_sha == bl.commit_sha).count() as u64;
                        let tool_name = format!("{}-agent", agent_name.replace("github-", ""));
                        prompts.insert(hash.clone(), serde_json::json!({
                            "agent_id": { "tool": tool_name, "model": "unknown", "id": bl.commit_sha },
                            "accepted_lines": total_lines,
                            "total_additions": total_lines,
                            "overriden_lines": 0u64,
                            "total_deletions": 0u64,
                        }));
                    }
                }
            }
        } else {
            line_authors.insert(bl.final_line, author_display);
        }
    }

    let mut lines_map: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
    let entries: Vec<(u32, &String)> = line_authors.iter().map(|(k, v)| (*k, v)).collect();
    if !entries.is_empty() {
        let mut range_start = entries[0].0;
        let mut range_end = entries[0].0;
        let mut range_author = entries[0].1;
        for entry in entries.iter().skip(1) {
            if entry.1 == range_author && entry.0 == range_end + 1 {
                range_end = entry.0;
            } else {
                let key = if range_start == range_end { format!("{}", range_start) } else { format!("{}-{}", range_start, range_end) };
                lines_map.insert(key, serde_json::Value::String(range_author.clone()));
                range_start = entry.0;
                range_end = entry.0;
                range_author = entry.1;
            }
        }
        let key = if range_start == range_end { format!("{}", range_start) } else { format!("{}-{}", range_start, range_end) };
        lines_map.insert(key, serde_json::Value::String(range_author.clone()));
    }

    let output = serde_json::json!({ "lines": lines_map, "prompts": prompts });
    println!("{}", serde_json::to_string_pretty(&output).unwrap_or_default());
}

fn load_authorship_note(commit_sha: &str) -> Option<AuthorshipLog> {
    let note_content = git_cmd(&["notes", "--ref=ai", "show", commit_sha]).ok()?;
    AuthorshipLog::deserialize_from_string(&note_content).ok()
}

fn format_blame_date(author_time: i64, author_tz: &str) -> String {
    let offset_secs: i64 = if author_tz.len() == 5 {
        let sign: i64 = if author_tz.starts_with('+') { 1 } else { -1 };
        let hours: i64 = author_tz[1..3].parse().unwrap_or(0);
        let mins: i64 = author_tz[3..5].parse().unwrap_or(0);
        sign * (hours * 3600 + mins * 60)
    } else {
        0
    };

    let local_time = author_time + offset_secs;
    let days_since_epoch = local_time.div_euclid(86400);
    let time_of_day = local_time.rem_euclid(86400);

    let hours = time_of_day / 3600;
    let mins = (time_of_day % 3600) / 60;
    let secs = time_of_day % 60;

    let (year, month, day) = days_to_ymd(days_since_epoch);

    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02} {}",
        year, month, day, hours, mins, secs, author_tz
    )
}

fn days_to_ymd(days: i64) -> (i64, u32, u32) {
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

// ---------------------------------------------------------------------------
// Install command
// ---------------------------------------------------------------------------

fn handle_install() {
    // --- Step 1: Kill v1 daemon if running ---
    kill_v1_daemon_if_running();

    // --- Step 2: Install local post-commit hook (for fallback / non-daemon use) ---
    let git_dir_str = match git_cmd(&["rev-parse", "--git-dir"]) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("git-ai install: not in a git repository: {}", e);
            process::exit(1);
        }
    };

    let hooks_dir = PathBuf::from(&git_dir_str).join("hooks");
    fs::create_dir_all(&hooks_dir).unwrap_or_else(|e| {
        eprintln!("git-ai install: failed to create hooks dir: {}", e);
        process::exit(1);
    });

    let hook_path = hooks_dir.join("post-commit");
    let hook_content = "#!/bin/sh\ngit-ai post-commit\n";
    fs::write(&hook_path, hook_content).unwrap_or_else(|e| {
        eprintln!("git-ai install: failed to write hook: {}", e);
        process::exit(1);
    });

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&hook_path, fs::Permissions::from_mode(0o755)).unwrap_or_else(|e| {
            eprintln!("git-ai install: failed to chmod hook: {}", e);
            process::exit(1);
        });
    }

    println!("git-ai: installed post-commit hook");

    // --- Step 3: Configure global trace2 to point to the v2 daemon socket ---
    configure_trace2_global();
}

/// Stop the v1 daemon if it is running.
/// Reads the PID file from ~/.git-ai/internal/daemon/daemon.pid.json,
/// sends SIGTERM, and waits up to 5s for exit.
fn kill_v1_daemon_if_running() {
    let home = match env::var("HOME") {
        Ok(h) => h,
        Err(_) => return,
    };

    let pid_path = PathBuf::from(&home)
        .join(".git-ai")
        .join("internal")
        .join("daemon")
        .join("daemon.pid.json");

    if !pid_path.exists() {
        return;
    }

    let content = match fs::read_to_string(&pid_path) {
        Ok(c) => c,
        Err(_) => return,
    };

    // Minimal JSON parsing for {"pid": N, ...}
    let pid: u32 = match extract_pid_from_json(&content) {
        Some(p) => p,
        None => return,
    };

    // Check if the process is alive
    #[cfg(unix)]
    {
        let alive = unsafe { libc::kill(pid as i32, 0) } == 0;
        if !alive {
            let _ = fs::remove_file(&pid_path);
            return;
        }

        eprintln!("[git-ai] stopping v1 daemon (pid {})...", pid);
        unsafe {
            libc::kill(pid as i32, libc::SIGTERM);
        }

        // Wait up to 5s for exit
        for _ in 0..50 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            let still_alive = unsafe { libc::kill(pid as i32, 0) } == 0;
            if !still_alive {
                eprintln!("[git-ai] v1 daemon stopped");
                let _ = fs::remove_file(&pid_path);
                return;
            }
        }

        eprintln!(
            "[git-ai] warning: v1 daemon (pid {}) did not exit within 5s",
            pid
        );
    }

    #[cfg(not(unix))]
    {
        let _ = pid;
    }
}

/// Extract "pid" value from a minimal JSON object like {"pid":1234,...}
fn extract_pid_from_json(json: &str) -> Option<u32> {
    let pattern = "\"pid\":";
    let idx = json.find(pattern)?;
    let after = json[idx + pattern.len()..].trim_start();
    let end = after
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(after.len());
    if end == 0 {
        return None;
    }
    after[..end].parse().ok()
}

/// Configure git's global trace2 event target to point to the v2 daemon socket.
/// This is what makes git send events to the daemon without any proxy/wrapper.
fn configure_trace2_global() {
    let socket_path = resolve_trace2_socket_path();
    let target = format!("af_unix:stream:{}", socket_path.display());

    // Set trace2.eventTarget
    match git_cmd(&["config", "--global", "trace2.eventTarget", &target]) {
        Ok(_) => {}
        Err(e) => {
            eprintln!("git-ai install: failed to set trace2.eventTarget: {}", e);
            return;
        }
    }

    // Set trace2.eventNesting (need enough depth to see command details)
    match git_cmd(&["config", "--global", "trace2.eventNesting", "10"]) {
        Ok(_) => {}
        Err(e) => {
            eprintln!("git-ai install: failed to set trace2.eventNesting: {}", e);
            return;
        }
    }

    println!(
        "git-ai: configured trace2 event target -> {}",
        socket_path.display()
    );
}

/// Resolve the trace2 socket path.
/// Uses the same logic as DaemonPaths: ~/.git-ai/internal/daemon/trace2.sock
/// unless the path is too long (>= 100 chars), in which case it hashes to /tmp.
fn resolve_trace2_socket_path() -> PathBuf {
    let home = env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    let base_dir = PathBuf::from(&home)
        .join(".git-ai")
        .join("internal")
        .join("daemon");
    let candidate = base_dir.join("trace2.sock");

    if candidate.to_string_lossy().len() >= 100 {
        // Hash the base dir to create a short /tmp path (matching DaemonPaths logic)
        use sha2::Digest;
        let mut hasher = sha2::Sha256::new();
        hasher.update(base_dir.to_string_lossy().as_bytes());
        let hash = hasher.finalize();
        let short_hash: String = hash[..8].iter().map(|b| format!("{:02x}", b)).collect();
        PathBuf::from(format!("/tmp/git-ai-d-{}", short_hash)).join("trace2.sock")
    } else {
        candidate
    }
}

// ---------------------------------------------------------------------------
// Status command (stub)
// ---------------------------------------------------------------------------

fn handle_status(args: &[String]) {
    if args.iter().any(|a| a == "--json") {
        println!("{{}}");
    } else {
        println!("No uncommitted attributions.");
    }
}

// ---------------------------------------------------------------------------
// Stats command
// ---------------------------------------------------------------------------

fn handle_stats(args: &[String]) {
    let is_json = args.iter().any(|a| a == "--json");
    let commit_ref = args
        .iter()
        .find(|a| !a.starts_with('-'))
        .map(|s| s.as_str())
        .unwrap_or("HEAD");

    let commit_sha = match git_cmd(&["rev-parse", commit_ref]) {
        Ok(s) => s,
        Err(_) => {
            if is_json {
                println!("{{}}");
            } else {
                println!("No stats available.");
            }
            return;
        }
    };

    let note = match git_cmd(&["notes", "--ref=ai", "show", &commit_sha]) {
        Ok(n) => n,
        Err(_) => {
            if is_json {
                println!("{{}}");
            } else {
                println!("No stats available.");
            }
            return;
        }
    };

    let log = match git_ai::core::authorship_log::AuthorshipLog::deserialize_from_string(&note) {
        Ok(l) => l,
        Err(_) => {
            if is_json {
                println!("{{}}");
            } else {
                println!("No stats available.");
            }
            return;
        }
    };

    let mut ai_additions: u64 = 0;
    let mut human_additions: u64 = 0;

    for file_att in &log.attestations {
        for entry in &file_att.entries {
            let count: u64 = entry
                .line_ranges
                .iter()
                .map(|r| r.line_count() as u64)
                .sum();
            if entry.hash.starts_with("h_") {
                human_additions += count;
            } else {
                ai_additions += count;
            }
        }
    }

    if is_json {
        println!(
            "{{\"ai_additions\":{},\"human_additions\":{},\"files\":{{\"total\":{{}}}}}}",
            ai_additions, human_additions
        );
    } else {
        println!("AI additions: {}", ai_additions);
        println!("Human additions: {}", human_additions);
    }
}

// ---------------------------------------------------------------------------
// Post-rewrite command (called after rebase/amend to copy authorship notes)
// ---------------------------------------------------------------------------

fn handle_post_rewrite(args: &[String]) {
    // The post-rewrite hook receives old-sha new-sha pairs on stdin.
    // If --stdin is passed, read from stdin. Otherwise, try to infer from reflog.
    let use_stdin = args.iter().any(|a| a == "--stdin");

    let mappings: Vec<(String, String)> = if use_stdin {
        use std::io::BufRead;
        std::io::stdin()
            .lock()
            .lines()
            .filter_map(|line| {
                let line = line.ok()?;
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 2 {
                    Some((parts[0].to_string(), parts[1].to_string()))
                } else {
                    None
                }
            })
            .collect()
    } else if args.len() >= 2 {
        // Direct old-sha new-sha pairs as arguments
        let mut pairs = Vec::new();
        let mut i = 0;
        let filtered: Vec<&String> = args
            .iter()
            .filter(|a| *a != "rebase" && *a != "amend")
            .collect();
        while i + 1 < filtered.len() {
            pairs.push((filtered[i].clone(), filtered[i + 1].clone()));
            i += 2;
        }
        pairs
    } else {
        Vec::new()
    };

    for (old_sha, new_sha) in &mappings {
        // Try to read the authorship note from the old commit
        let note = match git_cmd(&["notes", "--ref=ai", "show", old_sha]) {
            Ok(n) => n,
            Err(_) => continue,
        };

        if note.trim().is_empty() {
            continue;
        }

        // Update the base_commit_sha in the note metadata to point to the new commit
        let updated_note = if let Ok(mut log) = AuthorshipLog::deserialize_from_string(&note) {
            log.metadata.base_commit_sha = new_sha.clone();
            log.serialize_to_string()
        } else {
            note
        };

        // Write the note to the new commit
        let result = Command::new("/usr/bin/git")
            .args([
                "notes",
                "--ref=ai",
                "add",
                "-f",
                "-m",
                &updated_note,
                new_sha,
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .status();

        match result {
            Ok(status) if status.success() => {
                debug_log(&format!(
                    "copied authorship note {} -> {}",
                    &old_sha[..7.min(old_sha.len())],
                    &new_sha[..7.min(new_sha.len())]
                ));
            }
            _ => {
                debug_log(&format!(
                    "failed to copy note from {} to {}",
                    &old_sha[..7.min(old_sha.len())],
                    &new_sha[..7.min(new_sha.len())]
                ));
            }
        }
    }

    if mappings.is_empty() {
        debug_log("post-rewrite: no mappings provided");
    }
}

// ---------------------------------------------------------------------------
// Fetch-notes command
// ---------------------------------------------------------------------------

fn handle_fetch_notes(args: &[String]) {
    let mut remote: Option<String> = None;
    let mut is_json = false;
    let mut show_help = false;

    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        match arg {
            "--help" | "-h" => {
                show_help = true;
            }
            "--json" => {
                is_json = true;
            }
            "--remote" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("error: --remote requires a value");
                    process::exit(1);
                }
                if remote.is_some() {
                    eprintln!("error: remote specified more than once");
                    process::exit(1);
                }
                remote = Some(args[i].clone());
            }
            s if s.starts_with("--remote=") => {
                let val = s.strip_prefix("--remote=").unwrap();
                if val.is_empty() {
                    eprintln!("error: --remote requires a value");
                    process::exit(1);
                }
                if remote.is_some() {
                    eprintln!("error: remote specified more than once");
                    process::exit(1);
                }
                remote = Some(val.to_string());
            }
            s if s.starts_with('-') => {
                eprintln!("error: unknown option '{}'", s);
                process::exit(1);
            }
            _ => {
                if remote.is_some() {
                    eprintln!("error: remote specified more than once");
                    process::exit(1);
                }
                remote = Some(arg.to_string());
            }
        }
        i += 1;
    }

    if show_help {
        println!("usage: git-ai fetch-notes [--remote <name>] [--json]");
        println!();
        println!("Synchronously fetch AI authorship notes from a remote repository.");
        println!();
        println!("Options:");
        println!("  --remote <name>  Remote to fetch from (default: origin)");
        println!("  --json           Output in JSON format");
        return;
    }

    let remote_name = remote.unwrap_or_else(|| "origin".to_string());

    let result = Command::new("/usr/bin/git")
        .args(["fetch", &remote_name, "refs/notes/ai:refs/notes/ai"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output();

    match result {
        Ok(output) if output.status.success() => {
            if is_json {
                println!(
                    "{}",
                    serde_json::json!({
                        "status": "found",
                        "remote": remote_name,
                        "notes_ref": "refs/notes/ai"
                    })
                );
            } else {
                println!("Fetched authorship notes from '{}' — done", remote_name);
            }
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            if stderr.contains("couldn't find remote ref") {
                if is_json {
                    println!(
                        "{}",
                        serde_json::json!({
                            "status": "not_found",
                            "remote": remote_name,
                            "notes_ref": "refs/notes/ai",
                            "message": "no notes found on remote"
                        })
                    );
                } else {
                    println!("no notes found on remote '{}'", remote_name);
                }
            } else {
                if is_json {
                    println!(
                        "{}",
                        serde_json::json!({
                            "status": "fetch_failed",
                            "error": stderr.trim(),
                            "remote": remote_name
                        })
                    );
                    process::exit(1);
                } else {
                    eprintln!("error: failed to fetch notes from '{}': {}", remote_name, stderr.trim());
                    process::exit(1);
                }
            }
        }
        Err(e) => {
            if is_json {
                println!(
                    "{}",
                    serde_json::json!({
                        "status": "fetch_failed",
                        "error": format!("{}", e),
                        "remote": remote_name
                    })
                );
            } else {
                eprintln!("error: {}", e);
            }
            process::exit(1);
        }
    }
}

// ---------------------------------------------------------------------------
// Stash commands
// ---------------------------------------------------------------------------

fn handle_stash_save() {
    let git_dir_str = match git_cmd(&["rev-parse", "--git-dir"]) {
        Ok(d) => d,
        Err(_) => return,
    };
    let git_dir = PathBuf::from(&git_dir_str);
    let base_commit = git_cmd(&["rev-parse", "HEAD"]).unwrap_or_else(|_| "initial".to_string());

    // Save current working log state before stash
    let stash_dir = git_dir.join("ai").join("stash_backup");
    let working_log_dir = git_dir.join("ai").join("working_logs").join(&base_commit);

    if working_log_dir.exists() {
        let _ = fs::create_dir_all(&stash_dir);
        // Copy working log to stash backup
        if let Ok(entries) = fs::read_dir(&working_log_dir) {
            for entry in entries.flatten() {
                let dest = stash_dir.join(entry.file_name());
                let _ = fs::copy(entry.path(), dest);
            }
        }
        // Write the base commit SHA for later restoration
        let _ = fs::write(stash_dir.join("base_commit"), &base_commit);
    }
    debug_log("stash-save: preserved working log state");
}

fn handle_stash_restore() {
    let git_dir_str = match git_cmd(&["rev-parse", "--git-dir"]) {
        Ok(d) => d,
        Err(_) => return,
    };
    let git_dir = PathBuf::from(&git_dir_str);
    let current_head = git_cmd(&["rev-parse", "HEAD"]).unwrap_or_else(|_| "initial".to_string());

    let stash_dir = git_dir.join("ai").join("stash_backup");
    if !stash_dir.exists() {
        debug_log("stash-restore: no stash backup found");
        return;
    }

    // Restore working log to current HEAD's working_logs dir
    let working_log_dir = git_dir.join("ai").join("working_logs").join(&current_head);
    let _ = fs::create_dir_all(&working_log_dir);

    if let Ok(entries) = fs::read_dir(&stash_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            if name == "base_commit" {
                continue;
            }
            let dest = working_log_dir.join(&name);
            let _ = fs::copy(entry.path(), dest);
        }
    }

    // Strip h_ attributions for lines in files that are IDENTICAL to HEAD after stash pop.
    // Only when the working tree file equals the HEAD file (meaning the stash didn't
    // actually modify that file), the h_ entries are stale and should be removed.
    // If the file differs from HEAD (user has uncommitted changes), h_ attributions
    // are genuine and must be preserved to prevent gap-fill from claiming human lines.
    let repo_root = git_cmd(&["rev-parse", "--show-toplevel"]).unwrap_or_else(|_| ".".to_string());
    let repo_root_path = PathBuf::from(&repo_root);
    let checkpoints = git_ai::core::working_log::read_checkpoints(&git_dir, &current_head);
    if !checkpoints.is_empty() {
        let mut modified = false;
        let mut new_checkpoints = checkpoints.clone();
        for checkpoint in &mut new_checkpoints {
            for entry in &mut checkpoint.entries {
                if entry.line_attributions.is_empty() {
                    continue;
                }
                // Get HEAD content for this file
                let head_content = git_cmd(&["show", &format!("{}:{}", current_head, entry.file)])
                    .unwrap_or_default();
                if head_content.is_empty() {
                    continue;
                }

                // Get working tree content for this file
                let wt_path = repo_root_path.join(&entry.file);
                let wt_content = fs::read_to_string(&wt_path).unwrap_or_default();

                // Only strip h_ if the file is identical to HEAD (no uncommitted changes)
                if wt_content == head_content {
                    // File wasn't actually modified by the stash — h_ entries are stale
                    for attr in &mut entry.line_attributions {
                        if attr.author_id.starts_with("h_") {
                            attr.author_id = String::new();
                            modified = true;
                        }
                    }
                    entry.line_attributions.retain(|a| !a.author_id.is_empty());
                }
            }
        }
        if modified {
            // Rewrite the checkpoints file
            let checkpoints_path = working_log_dir.join("checkpoints.jsonl");
            let mut content = String::new();
            for cp in &new_checkpoints {
                if let Ok(json) = serde_json::to_string(cp) {
                    content.push_str(&json);
                    content.push('\n');
                }
            }
            let _ = fs::write(&checkpoints_path, &content);
            debug_log("stash-restore: stripped stale h_ attributions");
        }
    }

    // Clean up stash backup
    let _ = fs::remove_dir_all(&stash_dir);
    debug_log("stash-restore: restored working log state");
}

fn handle_stash_restore_ref(args: &[String]) {
    let stash_ref = args.first().map(|s| s.as_str()).unwrap_or("stash@{0}");
    debug_log(&format!("stash-restore-ref: {}", stash_ref));
    handle_stash_restore();
}

// ---------------------------------------------------------------------------
// Post-rewrite-squash command
// ---------------------------------------------------------------------------

fn handle_post_rewrite_squash(args: &[String]) {
    use git_ai::core::attribution::{LineDiffOp, diff_slices};
    use std::collections::{BTreeMap, HashSet};

    // Format: post-rewrite-squash <target_sha> <source1> <source2> ...
    // Merges all source notes into a single combined note on target_sha.
    if args.is_empty() {
        debug_log("post-rewrite-squash: no target SHA provided");
        return;
    }

    let target_sha = &args[0];
    let source_shas = &args[1..];

    if source_shas.is_empty() {
        debug_log("post-rewrite-squash: no source SHAs provided");
        return;
    }

    // Parse all source notes and collect metadata
    let mut parsed_notes: Vec<(String, AuthorshipLog)> = Vec::new();
    let mut all_files: HashSet<String> = HashSet::new();
    let mut merged_sessions: BTreeMap<String, git_ai::core::authorship_log::SessionRecord> = BTreeMap::new();
    let mut merged_humans: BTreeMap<String, git_ai::core::authorship_log::HumanRecord> = BTreeMap::new();
    let mut merged_prompts: BTreeMap<String, git_ai::core::authorship_log::PromptRecord> = BTreeMap::new();

    for source_sha in source_shas {
        debug_log(&format!("post-rewrite-squash: looking up note for source {}", source_sha));
        let note = match git_cmd(&["notes", "--ref=ai", "show", source_sha]) {
            Ok(n) => n,
            Err(e) => {
                debug_log(&format!("post-rewrite-squash: no note for {}: {}", source_sha, e));
                continue;
            }
        };
        if note.trim().is_empty() {
            continue;
        }

        let log = match AuthorshipLog::deserialize_from_string(&note) {
            Ok(l) => l,
            Err(_) => continue,
        };

        // Merge metadata
        for (id, session) in &log.metadata.sessions {
            merged_sessions.entry(id.clone()).or_insert_with(|| session.clone());
        }
        for (id, human) in &log.metadata.humans {
            merged_humans.entry(id.clone()).or_insert_with(|| human.clone());
        }
        for (id, prompt) in &log.metadata.prompts {
            merged_prompts.entry(id.clone()).or_insert_with(|| prompt.clone());
        }

        for att in &log.attestations {
            all_files.insert(att.file_path.clone());
        }
        parsed_notes.push((source_sha.clone(), log));
    }

    if parsed_notes.is_empty() {
        debug_log("post-rewrite-squash: no notes found in source commits");
        return;
    }

    // Sequential replay: for each file, accumulate attributions by diffing
    // consecutive commit contents and transferring line numbers forward.
    // accumulated_attrs: file_path -> (line_number -> author_hash)
    let mut accumulated_attrs: std::collections::HashMap<String, BTreeMap<u32, String>> = std::collections::HashMap::new();
    let mut prev_contents: std::collections::HashMap<String, String> = std::collections::HashMap::new();

    let all_files_vec: Vec<String> = all_files.iter().cloned().collect();

    for (i, (source_sha, authorship_log)) in parsed_notes.iter().enumerate() {
        // Get file contents at this commit
        let mut current_contents: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        for file_path in &all_files_vec {
            let spec = format!("{}:{}", source_sha, file_path);
            if let Ok(content) = git_cmd(&["show", &spec]) {
                current_contents.insert(file_path.clone(), content);
            }
        }

        if i > 0 {
            // Transfer accumulated attributions through diff
            for file_path in &all_files_vec {
                let prev_content = prev_contents.get(file_path);
                let curr_content = current_contents.get(file_path);

                if let (Some(prev_c), Some(curr_c)) = (prev_content, curr_content) {
                    if prev_c == curr_c {
                        continue; // No change, attributions stay the same
                    }
                    if let Some(attrs) = accumulated_attrs.get(file_path) {
                        if attrs.is_empty() {
                            continue;
                        }
                        // Diff old→new and transfer attributions
                        let old_lines: Vec<&str> = prev_c.lines().collect();
                        let new_lines: Vec<&str> = curr_c.lines().collect();
                        let ops = diff_slices(&old_lines, &new_lines);

                        let mut new_attrs: BTreeMap<u32, String> = BTreeMap::new();
                        for op in &ops {
                            if let LineDiffOp::Equal { old_index, new_index, len } = op {
                                for j in 0..*len {
                                    let old_line_num = (*old_index + j + 1) as u32;
                                    let new_line_num = (*new_index + j + 1) as u32;
                                    if let Some(hash) = attrs.get(&old_line_num) {
                                        new_attrs.insert(new_line_num, hash.clone());
                                    }
                                }
                            }
                        }
                        accumulated_attrs.insert(file_path.clone(), new_attrs);
                    }
                }
            }
        }

        // Overlay this commit's note attributions
        for file_attestation in &authorship_log.attestations {
            let file_path = &file_attestation.file_path;
            let entry = accumulated_attrs.entry(file_path.clone()).or_default();
            for att_entry in &file_attestation.entries {
                for line in att_entry.line_ranges.iter().flat_map(|r| r.expand()) {
                    entry.insert(line, att_entry.hash.clone());
                }
            }
        }

        prev_contents = current_contents;
    }

    // Final diff: last source commit content → target commit content
    // (In fixup squash, these should be identical, but handle the general case)
    for file_path in &all_files_vec {
        let spec = format!("{}:{}", target_sha, file_path);
        if let Ok(target_content) = git_cmd(&["show", &spec]) {
            if let Some(prev_c) = prev_contents.get(file_path) {
                if prev_c != &target_content {
                    if let Some(attrs) = accumulated_attrs.get(file_path) {
                        if !attrs.is_empty() {
                            let old_lines: Vec<&str> = prev_c.lines().collect();
                            let new_lines: Vec<&str> = target_content.lines().collect();
                            let ops = diff_slices(&old_lines, &new_lines);

                            let mut new_attrs: BTreeMap<u32, String> = BTreeMap::new();
                            for op in &ops {
                                if let LineDiffOp::Equal { old_index, new_index, len } = op {
                                    for j in 0..*len {
                                        let old_line_num = (*old_index + j + 1) as u32;
                                        let new_line_num = (*new_index + j + 1) as u32;
                                        if let Some(hash) = attrs.get(&old_line_num) {
                                            new_attrs.insert(new_line_num, hash.clone());
                                        }
                                    }
                                }
                            }
                            accumulated_attrs.insert(file_path.clone(), new_attrs);
                        }
                    }
                }
            }
        }
    }

    // Build merged attestations from accumulated attrs
    let mut merged_attestations: Vec<git_ai::core::authorship_log::FileAttestation> = Vec::new();
    for (file_path, attrs) in &accumulated_attrs {
        if attrs.is_empty() {
            continue;
        }
        // Group by hash
        let mut hash_lines: BTreeMap<String, Vec<u32>> = BTreeMap::new();
        for (line, hash) in attrs {
            hash_lines.entry(hash.clone()).or_default().push(*line);
        }
        let mut entries: Vec<git_ai::core::authorship_log::AttestationEntry> = Vec::new();
        for (hash, mut lines) in hash_lines {
            lines.sort();
            entries.push(git_ai::core::authorship_log::AttestationEntry {
                hash,
                line_ranges: git_ai::core::authorship_log::LineRange::compress_lines(&lines),
            });
        }
        merged_attestations.push(git_ai::core::authorship_log::FileAttestation {
            file_path: file_path.clone(),
            entries,
        });
    }

    if merged_attestations.is_empty() && merged_sessions.is_empty() && merged_humans.is_empty() && merged_prompts.is_empty() {
        debug_log("post-rewrite-squash: no notes found in source commits");
        return;
    }

    // Build the merged authorship log
    let merged_log = AuthorshipLog {
        attestations: merged_attestations,
        metadata: git_ai::core::authorship_log::Metadata {
            schema_version: "authorship/3.0.0".to_string(),
            git_ai_version: None,
            base_commit_sha: target_sha.clone(),
            prompts: merged_prompts,
            sessions: merged_sessions,
            humans: merged_humans,
        },
    };

    let merged_note = merged_log.serialize_to_string();

    // Write the merged note to the target commit
    let result = Command::new("/usr/bin/git")
        .args(["notes", "--ref=ai", "add", "-f", "-m", &merged_note, target_sha])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .status();

    match result {
        Ok(status) if status.success() => {
            debug_log(&format!(
                "post-rewrite-squash: merged {} source notes into {}",
                parsed_notes.len(),
                &target_sha[..7.min(target_sha.len())]
            ));
        }
        _ => {
            debug_log(&format!(
                "post-rewrite-squash: failed to write merged note to {}",
                &target_sha[..7.min(target_sha.len())]
            ));
        }
    }
}

// ---------------------------------------------------------------------------
// Internal machine commands (for IDE/CI integrations)
// ---------------------------------------------------------------------------

fn handle_internal_command(cmd: &str, args: &[String]) {
    // All internal machine commands require --json flag
    let is_json = args.iter().any(|a| a == "--json");
    if !is_json {
        eprintln!("{}", serde_json::json!({ "error": format!("internal command '{}' requires --json flag", cmd) }));
        process::exit(1);
    }

    // The request payload is the positional argument after --json (or any arg starting with '{')
    let request_str: Option<&str> = args.iter()
        .skip_while(|a| a.as_str() != "--json")
        .skip(1) // skip --json itself
        .next()
        .map(|s| s.as_str())
        .or_else(|| args.iter().find(|a| a.starts_with('{')).map(|s| s.as_str()));

    match cmd {
        "effective-ignore-patterns" => {
            let repo_root = git_cmd(&["rev-parse", "--show-toplevel"]).unwrap_or_default();
            let mut all_patterns: Vec<String> = DEFAULT_IGNORE_PATTERNS.iter().map(|s| s.to_string()).collect();

            // Read .git-ai-ignore if present
            let ignore_file = PathBuf::from(&repo_root).join(".git-ai-ignore");
            if ignore_file.exists() {
                if let Ok(content) = fs::read_to_string(&ignore_file) {
                    for line in content.lines() {
                        let trimmed = line.trim();
                        if !trimmed.is_empty() && !trimmed.starts_with('#') {
                            all_patterns.push(trimmed.to_string());
                        }
                    }
                }
            }

            // Read .gitattributes for linguist-generated patterns
            let gitattributes_file = PathBuf::from(&repo_root).join(".gitattributes");
            if gitattributes_file.exists() {
                if let Ok(content) = fs::read_to_string(&gitattributes_file) {
                    for line in content.lines() {
                        let trimmed = line.trim();
                        if trimmed.contains("linguist-generated") {
                            if let Some(pattern) = trimmed.split_whitespace().next() {
                                all_patterns.push(pattern.to_string());
                            }
                        }
                    }
                }
            }

            // Include user_patterns and extra_patterns from the request
            if let Some(req) = request_str {
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(req) {
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
            let file = parsed["file_path"].as_str()
                .or_else(|| parsed["file"].as_str())
                .unwrap_or("");
            let _commit = parsed["commit"].as_str().unwrap_or("HEAD");
            let options = &parsed["options"];
            let return_human_as_human = options["return_human_authors_as_human"].as_bool().unwrap_or(false);
            let line_ranges: Vec<(u32, u32)> = options["line_ranges"].as_array()
                .map(|arr| arr.iter().filter_map(|r| {
                    let pair = r.as_array()?;
                    Some((pair.get(0)?.as_u64()? as u32, pair.get(1)?.as_u64()? as u32))
                }).collect())
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
                        if line.is_empty() { continue; }
                        if line.starts_with('\t') {
                            blame_lines.push(BlameLineData {
                                commit_sha: cur_sha.clone(),
                                orig_line: cur_orig_line,
                                final_line: cur_final_line,
                                author: cur_author.clone(),
                                author_email: cur_author_email.clone(),
                                author_time: cur_author_time,
                                author_tz: cur_author_tz.clone(),
                                content: line[1..].to_string(),
                                raw_headers: cur_headers.clone(),
                            });
                            cur_headers.clear();
                            continue;
                        }
                        if let Some(rest) = line.strip_prefix("author-mail ") {
                            cur_author_email = rest.trim_start_matches('<').trim_end_matches('>').to_string();
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
                    let mut line_authors: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
                    let mut prompt_records: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();

                    for bl in &blame_lines {
                        // Filter by line_ranges if specified
                        if !line_ranges.is_empty() {
                            let in_range = line_ranges.iter().any(|(start, end)| {
                                bl.final_line >= *start && bl.final_line <= *end
                            });
                            if !in_range { continue; }
                        }

                        let (author_display, prompt_hash) = resolve_line_author_with_prompt(
                            &bl.commit_sha, bl.orig_line, &bl.author, &bl.author_email, file, &commit_notes, &bl.raw_headers,
                        );

                        let display = if let Some(ref hash) = prompt_hash {
                            // Collect prompt record
                            if !prompt_records.contains_key(hash) {
                                if let Some(Some(log)) = commit_notes.get(&bl.commit_sha) {
                                    if let Some(prompt) = log.metadata.prompts.get(hash) {
                                        prompt_records.insert(hash.clone(), serde_json::json!({
                                            "agent_id": { "tool": prompt.agent_id.tool, "model": prompt.agent_id.model },
                                        }));
                                    }
                                }
                            }
                            author_display
                        } else if return_human_as_human {
                            "human".to_string()
                        } else {
                            author_display
                        };

                        line_authors.insert(bl.final_line.to_string(), serde_json::Value::String(display));
                    }

                    // Build blame hunks
                    let mut blame_hunks: Vec<serde_json::Value> = Vec::new();
                    for bl in &blame_lines {
                        if !line_ranges.is_empty() {
                            let in_range = line_ranges.iter().any(|(start, end)| {
                                bl.final_line >= *start && bl.final_line <= *end
                            });
                            if !in_range { continue; }
                        }
                        blame_hunks.push(serde_json::json!({
                            "commit": bl.commit_sha,
                            "line": bl.final_line,
                            "author": bl.author,
                            "content": bl.content,
                        }));
                    }

                    println!("{}", serde_json::json!({
                        "line_authors": line_authors,
                        "prompt_records": prompt_records,
                        "blame_hunks": blame_hunks,
                    }));
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
                parsed["remote_name"].as_str()
                    .or_else(|| parsed["remote"].as_str())
                    .unwrap_or("origin").to_string()
            } else {
                "origin".to_string()
            };

            // Try to fetch; determine if notes exist on remote
            let result = Command::new("/usr/bin/git")
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
                parsed["remote_name"].as_str()
                    .or_else(|| parsed["remote"].as_str())
                    .unwrap_or("origin").to_string()
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
            let mut last_err = String::new();
            for attempt in 0..3 {
                // On retry attempts (or after first non-fast-forward), fetch and merge
                if attempt > 0 {
                    let _ = Command::new("/usr/bin/git")
                        .args(["fetch", &remote, "+refs/notes/ai:refs/notes/ai-remote/origin"])
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status();
                    // Try to merge remote notes with cat_sort_uniq
                    let merge_ok = Command::new("/usr/bin/git")
                        .args(["notes", "--ref=ai", "merge", "-s", "cat_sort_uniq", "refs/notes/ai-remote/origin"])
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status()
                        .map(|s| s.success())
                        .unwrap_or(false);
                    if !merge_ok {
                        let _ = Command::new("/usr/bin/git")
                            .args(["notes", "--ref=ai", "merge", "--abort"])
                            .stdout(Stdio::null())
                            .stderr(Stdio::null())
                            .status();
                        // Fallback: merge with ours strategy
                        let ours_ok = Command::new("/usr/bin/git")
                            .args(["notes", "--ref=ai", "merge", "-s", "ours", "refs/notes/ai-remote/origin"])
                            .stdout(Stdio::null())
                            .stderr(Stdio::null())
                            .status()
                            .map(|s| s.success())
                            .unwrap_or(false);
                        if !ours_ok {
                            let _ = Command::new("/usr/bin/git")
                                .args(["notes", "--ref=ai", "merge", "--abort"])
                                .stdout(Stdio::null())
                                .stderr(Stdio::null())
                                .status();
                            // All merge strategies failed (corrupted remote tree).
                            // Force push our local notes as last resort.
                            let force_result = Command::new("/usr/bin/git")
                                .args(["push", "--force", &remote, "refs/notes/ai:refs/notes/ai"])
                                .stdout(Stdio::piped())
                                .stderr(Stdio::piped())
                                .output();
                            if let Ok(out) = force_result {
                                if out.status.success() {
                                    println!("{}", serde_json::json!({ "ok": true }));
                                    return;
                                }
                            }
                        }
                    }
                }

                let result = Command::new("/usr/bin/git")
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
                        last_err = String::from_utf8_lossy(&output.stderr).trim().to_string();
                        if last_err.contains("non-fast-forward") || last_err.contains("fetch first") {
                            continue;
                        }
                        break;
                    }
                    Err(e) => {
                        last_err = format!("{}", e);
                        break;
                    }
                }
            }
            // Even if push fails after retries, report ok (best effort)
            println!("{}", serde_json::json!({ "ok": true }));
        }
        _ => {
            eprintln!("{}", serde_json::json!({ "error": format!("unknown internal command: {}", cmd) }));
            process::exit(1);
        }
    }
}

// ---------------------------------------------------------------------------
// CI command (local CI for squash-merge attribution)
// ---------------------------------------------------------------------------

fn handle_ci(args: &[String]) {
    if args.len() < 2 || args[0] != "local" || args[1] != "merge" {
        eprintln!("usage: git-ai ci local merge [options]");
        process::exit(1);
    }

    let ci_args = &args[2..];
    let mut merge_commit_sha = String::new();
    let mut base_ref = String::new();
    let mut _head_ref = String::new();
    let mut head_sha = String::new();
    let mut base_sha = String::new();
    let mut skip_fetch_base = false;
    let mut skip_fetch_notes = false;
    let mut skip_fetch = false;
    let mut skip_push = false;

    let mut i = 0;
    while i < ci_args.len() {
        match ci_args[i].as_str() {
            "--merge-commit-sha" => { i += 1; merge_commit_sha = ci_args.get(i).cloned().unwrap_or_default(); }
            "--base-ref" => { i += 1; base_ref = ci_args.get(i).cloned().unwrap_or_default(); }
            "--head-ref" => { i += 1; _head_ref = ci_args.get(i).cloned().unwrap_or_default(); }
            "--head-sha" => { i += 1; head_sha = ci_args.get(i).cloned().unwrap_or_default(); }
            "--base-sha" => { i += 1; base_sha = ci_args.get(i).cloned().unwrap_or_default(); }
            "--skip-fetch-base" => { skip_fetch_base = true; }
            "--skip-fetch-notes" => { skip_fetch_notes = true; }
            "--skip-fetch" => { skip_fetch = true; skip_fetch_notes = true; skip_fetch_base = true; }
            "--skip-push" => { skip_push = true; }
            _ => {}
        }
        i += 1;
    }

    // Step 1: Fetch authorship notes (unless skipped)
    if skip_fetch || skip_fetch_notes {
        println!("Skipping authorship history fetch (--skip-fetch)");
    } else {
        let fetch_result = Command::new("/usr/bin/git")
            .args(["fetch", "origin", "+refs/notes/ai:refs/notes/ai"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output();
        match fetch_result {
            Ok(output) if !output.status.success() => {
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                if !stderr.contains("couldn't find remote ref") {
                    eprintln!("Error running local CI: failed to fetch authorship notes: {}", stderr.trim());
                    process::exit(1);
                }
            }
            Err(e) => {
                eprintln!("Error running local CI: failed to fetch authorship notes: {}", e);
                process::exit(1);
            }
            _ => {}
        }
    }

    // Step 2: Resolve base ref
    if skip_fetch_base {
        println!("Skipping base branch fetch for {}", base_ref);
        // Verify it exists locally
        let resolve_result = git_cmd(&["rev-parse", "--verify", &base_ref]);
        if resolve_result.is_err() {
            let with_origin = format!("origin/{}", base_ref);
            if git_cmd(&["rev-parse", "--verify", &with_origin]).is_err() {
                eprintln!("Failed to resolve base ref '{}' locally", base_ref);
                process::exit(1);
            }
        }
    } else {
        // Try to fetch the base branch from origin
        let fetch_base_result = Command::new("/usr/bin/git")
            .args(["fetch", "origin", &base_ref])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output();
        match fetch_base_result {
            Ok(output) if !output.status.success() => {
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                eprintln!("Failed to fetch base branch '{}' from origin: {}", base_ref, stderr.trim());
                process::exit(1);
            }
            Err(e) => {
                eprintln!("Failed to fetch base branch '{}' from origin: {}", base_ref, e);
                process::exit(1);
            }
            _ => {}
        }
    }

    // Step 3: Determine if merge commit has AI authorship from head branch commits
    let range = format!("{}..{}", base_sha, head_sha);
    let commits_output = git_cmd(&["log", "--format=%H", &range]).unwrap_or_default();
    let head_commits: Vec<&str> = commits_output.lines().filter(|l| !l.is_empty()).collect();

    let mut has_ai_authorship = false;
    for commit in &head_commits {
        if let Ok(note) = git_cmd(&["notes", "--ref=ai", "show", commit]) {
            if !note.trim().is_empty() {
                has_ai_authorship = true;
                break;
            }
        }
    }

    if !has_ai_authorship {
        if skip_fetch {
            println!("Local CI (merge): skipped fast-forward merge — no AI authorship to track");
        } else {
            println!("Local CI (merge): no AI authorship to track");
        }
    } else {
        for commit in &head_commits {
            if let Ok(note) = git_cmd(&["notes", "--ref=ai", "show", commit]) {
                if !note.trim().is_empty() {
                    let _ = Command::new("/usr/bin/git")
                        .args(["notes", "--ref=ai", "add", "-f", "-m", &note, &merge_commit_sha])
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status();
                    break;
                }
            }
        }
        println!("Local CI (merge): transferred AI authorship to merge commit");
    }

    // Step 4: Push authorship notes (unless skipped)
    if skip_push {
        println!("Skipping authorship push (--skip-push)");
    } else {
        println!("Pushing authorship...");
        let _ = Command::new("/usr/bin/git")
            .args(["push", "origin", "refs/notes/ai:refs/notes/ai"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

// ---------------------------------------------------------------------------
// Entry point — git-ai is ONLY git-ai, never a git proxy/wrapper
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Background daemon commands
// ---------------------------------------------------------------------------

fn handle_bg(args: &[String]) {
    match args.first().map(String::as_str) {
        Some("run") => {
            if let Err(e) = git_ai::daemon::run::run_daemon(true) {
                eprintln!("git-ai bg run: {}", e);
                process::exit(1);
            }
        }
        Some("start") => {
            if let Err(e) = git_ai::daemon::run::run_daemon(false) {
                eprintln!("git-ai bg start: {}", e);
                process::exit(1);
            }
        }
        Some("stop") => {
            if let Err(e) = git_ai::daemon::run::stop_daemon() {
                eprintln!("git-ai bg stop: {}", e);
                process::exit(1);
            }
        }
        Some("status") => {
            git_ai::daemon::run::print_status();
        }
        _ => {
            eprintln!("usage: git-ai bg <run|start|stop|status>");
            process::exit(1);
        }
    }
}

// ---------------------------------------------------------------------------
// Entry point — git-ai is ONLY git-ai, never a git proxy/wrapper
// ---------------------------------------------------------------------------

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();

    match args.first().map(String::as_str) {
        Some("checkpoint") => handle_checkpoint(&args[1..]),
        Some("post-commit") => handle_post_commit(),
        Some("post-rewrite") => handle_post_rewrite(&args[1..]),
        Some("post-rewrite-squash") => handle_post_rewrite_squash(&args[1..]),
        Some("stash-save") => handle_stash_save(),
        Some("stash-restore") => handle_stash_restore(),
        Some("stash-restore-ref") => handle_stash_restore_ref(&args[1..]),
        Some("blame") => handle_blame(&args[1..]),
        Some("diff") => handle_diff(&args[1..]),
        Some("fetch-notes") => handle_fetch_notes(&args[1..]),
        Some("install") => handle_install(),
        Some("status") => handle_status(&args[1..]),
        Some("stats") => handle_stats(&args[1..]),
        Some("bg") => handle_bg(&args[1..]),
        Some("ci") => handle_ci(&args[1..]),
        Some("effective-ignore-patterns") => handle_internal_command("effective-ignore-patterns", &args[1..]),
        Some("blame-analysis") => handle_internal_command("blame-analysis", &args[1..]),
        Some("fetch-authorship-notes") => handle_internal_command("fetch-authorship-notes", &args[1..]),
        Some("fetch_authorship_notes") => handle_internal_command("fetch_authorship_notes", &args[1..]),
        Some("push-authorship-notes") => handle_internal_command("push-authorship-notes", &args[1..]),
        Some("--version") | Some("-v") | Some("version") => {
            println!("git-ai {}", env!("CARGO_PKG_VERSION"));
        }
        Some("--help") | Some("-h") | Some("help") | None => {
            println!("usage: git-ai <command> [<args>]");
            println!();
            println!("Commands:");
            println!("  checkpoint    Record attribution checkpoint");
            println!("  post-commit   Generate authorship note for HEAD commit");
            println!("  post-rewrite  Copy authorship notes after rebase/amend");
            println!("  blame         Show blame with AI/human attribution");
            println!("  diff          Show diff with AI attribution");
            println!("  fetch-notes   Fetch authorship notes from remote");
            println!("  install       Install git hooks for automatic attribution");
            println!("  status        Show uncommitted attribution status");
            println!("  stats         Show commit attribution stats");
            println!("  bg            Daemon lifecycle (run, start, stop, status)");
        }
        Some(cmd) => {
            eprintln!("git-ai: unknown command '{}'", cmd);
            process::exit(1);
        }
    }
}
// ---------------------------------------------------------------------------
// Diff command
// ---------------------------------------------------------------------------

/// Default ignore patterns for files that should be excluded from diff output.
const DEFAULT_IGNORE_PATTERNS: &[&str] = &[
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
        } else if p < pattern.len() && (pattern[p] == b'?' && text[t] != b'/') {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == text[t] {
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
    let end = after_plus
        .find(|c: char| c == ',' || c == ' ')
        .unwrap_or(after_plus.len());
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
            if line.starts_with("+++ ") {
                if let Some(path) = line.strip_prefix("+++ b/") {
                    current_file = path.to_string();
                }
                // "+++ /dev/null" means file deletion - keep old file path
            }
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
    let output = Command::new("/usr/bin/git")
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
    let output = Command::new("/usr/bin/git")
        .args(["show", &format!("{}:{}", commit, file_path)])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output();

    match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
        _ => String::new(),
    }
}

fn handle_diff(args: &[String]) {
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
    if from_commit != to_commit {
        if let Ok(log_output) =
            git_cmd(&["log", "--format=%H", &format!("{}..{}", from_commit, to_commit)])
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
                            let ranges =
                                git_ai::core::authorship_log::LineRange::compress_lines(
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

                            annotations.insert(
                                entry.hash.clone(),
                                serde_json::Value::Array(range_values),
                            );

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
                                    format!(
                                        "{}:{}:{}",
                                        file_path, entry.hash, content_for_hash
                                    )
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
                                hunk["human_id"] =
                                    serde_json::Value::String(entry.hash.clone());
                            } else {
                                hunk["prompt_id"] =
                                    serde_json::Value::String(entry.hash.clone());
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
                                    serde_json::to_value(prompt)
                                        .unwrap_or(serde_json::json!({})),
                                );
                            }
                            if let Some(session) = note.metadata.sessions.get(&entry.hash) {
                                sessions_map.insert(
                                    entry.hash.clone(),
                                    serde_json::to_value(session)
                                        .unwrap_or(serde_json::json!({})),
                                );
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
                                serde_json::to_value(session)
                                    .unwrap_or(serde_json::json!({})),
                            );
                        }
                    }
                }
            }
        }

        // Add commit metadata for to_commit
        if !commits_map.contains_key(&to_commit) {
            if let Some(metadata) = get_commit_metadata(&to_commit) {
                commits_map.insert(to_commit.clone(), metadata);
            }
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
    if all_prompts {
        if let Some(note) = &to_note {
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
    if include_stats {
        if let Some(stats) =
            compute_commit_stats(&commit_authorship, &to_commit, &filtered_sections)
        {
            result["commit_stats"] = stats;
        }
    }

    println!("{}", serde_json::to_string(&result).unwrap());
}

/// Get metadata for a commit (author, time, message).
fn get_commit_metadata(commit_sha: &str) -> Option<serde_json::Value> {
    let format_str = "%aI%n%an <%ae>%n%s%n%B";
    let output =
        git_cmd(&["log", "-1", &format!("--format={}", format_str), commit_sha]).ok()?;
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
                if let Some(session) = note.metadata.sessions.get(&entry.hash) {
                    let key =
                        format!("{}::{}", session.agent_id.tool, session.agent_id.model);
                    let existing = tool_model_breakdown
                        .entry(key)
                        .or_insert_with(|| serde_json::json!({"ai_lines_added": 0}));
                    if let Some(n) =
                        existing.get("ai_lines_added").and_then(|v| v.as_u64())
                    {
                        existing["ai_lines_added"] =
                            serde_json::json!(n + count as u64);
                    }
                } else if let Some(prompt) = note.metadata.prompts.get(&entry.hash) {
                    let key =
                        format!("{}::{}", prompt.agent_id.tool, prompt.agent_id.model);
                    let existing = tool_model_breakdown
                        .entry(key)
                        .or_insert_with(|| serde_json::json!({"ai_lines_added": 0}));
                    if let Some(n) =
                        existing.get("ai_lines_added").and_then(|v| v.as_u64())
                    {
                        existing["ai_lines_added"] =
                            serde_json::json!(n + count as u64);
                    }
                }
            } else if note.metadata.prompts.contains_key(&entry.hash) {
                ai_lines_added += count;
                let prompt = &note.metadata.prompts[&entry.hash];
                let key =
                    format!("{}::{}", prompt.agent_id.tool, prompt.agent_id.model);
                let existing = tool_model_breakdown
                    .entry(key)
                    .or_insert_with(|| serde_json::json!({"ai_lines_added": 0}));
                if let Some(n) =
                    existing.get("ai_lines_added").and_then(|v| v.as_u64())
                {
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
