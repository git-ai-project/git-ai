use crate::authorship::authorship_log_serialization::{AuthorshipLog, GIT_AI_VERSION};
use crate::authorship::working_log::AgentId;
use crate::git::find_repository;
use crate::git::refs::show_authorship_note;
use crate::git::repository::{Repository, exec_git};
use std::collections::HashSet;
use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use zip::write::{SimpleFileOptions, ZipWriter};

/// Validate that a string looks like a valid git SHA (hex characters only).
/// Prevents path traversal via crafted SHA values like "../../etc/passwd".
fn validate_hex_sha(sha: &str) -> Result<(), String> {
    if sha.is_empty() {
        return Err("SHA is empty".to_string());
    }
    if !sha.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!("Invalid SHA (non-hex characters): {}", sha));
    }
    Ok(())
}



/// Handle the `logs` command
///
/// Usage: `git-ai logs <commitsha> [-o <outputfolder>]`
///
/// Collects agent transcripts, authorship notes, and commit context into a zip file.
pub fn handle_logs(args: &[String]) {
    let parsed = match parse_args(args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    };

    // Validate commit SHA to prevent path traversal
    if let Err(e) = validate_hex_sha(&parsed.commit_sha) {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }

    let repo = match find_repository(&Vec::<String>::new()) {
        Ok(repo) => repo,
        Err(e) => {
            eprintln!("Failed to find repository: {}", e);
            std::process::exit(1);
        }
    };

    // Read authorship note
    let note_text = match show_authorship_note(&repo, &parsed.commit_sha) {
        Some(text) => text,
        None => {
            eprintln!(
                "No AI authorship data found for commit {}",
                &parsed.commit_sha
            );
            std::process::exit(1);
        }
    };

    // Parse the authorship log to extract agent IDs
    let authorship_log = match AuthorshipLog::deserialize_from_string(&note_text) {
        Ok(log) => log,
        Err(e) => {
            eprintln!("Failed to parse authorship note: {}", e);
            std::process::exit(1);
        }
    };

    // Extract unique agent IDs from prompts
    let mut seen_agents = HashSet::new();
    let mut agents: Vec<(AgentId, Option<std::collections::HashMap<String, String>>)> = Vec::new();
    for prompt_record in authorship_log.metadata.prompts.values() {
        let key = (
            prompt_record.agent_id.tool.clone(),
            prompt_record.agent_id.id.clone(),
        );
        if seen_agents.insert(key) {
            agents.push((
                prompt_record.agent_id.clone(),
                prompt_record.custom_attributes.clone(),
            ));
        }
    }

    if agents.is_empty() {
        eprintln!("No agent data found in authorship note");
        std::process::exit(1);
    }

    let short_sha = &parsed.commit_sha[..7.min(parsed.commit_sha.len())];
    let repo_cwd = match repo.workdir() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Failed to determine repository working directory: {}", e);
            std::process::exit(1);
        }
    };

    // Collect context metadata
    let commit_info = get_commit_info(&repo, &parsed.commit_sha);
    let commit_diff = get_commit_diff(&repo, &parsed.commit_sha);

    // Resolve transcript paths for all agents
    let mut transcript_files: Vec<(String, PathBuf)> = Vec::new(); // (agent_tool, path)

    for (agent_id, custom_attrs) in &agents {
        let paths = resolve_transcript_paths(agent_id, custom_attrs.as_ref(), &repo_cwd);

        for path in paths {
            if path.exists() {
                transcript_files.push((agent_id.tool.clone(), path));
            } else {
                eprintln!(
                    "Warning: transcript not found for {} agent: {}",
                    agent_id.tool,
                    path.display()
                );
            }
        }
    }

    // Collect working log files
    let base_sha = &authorship_log.metadata.base_commit_sha;

    // Validate base_sha to prevent path traversal
    if !base_sha.is_empty() {
        if let Err(e) = validate_hex_sha(base_sha) {
            eprintln!("Warning: invalid base commit SHA in authorship note: {}", e);
            std::process::exit(1);
        }
    }

    let working_log_dir = repo.storage.working_logs.join(base_sha);
    // Also check for debug-mode "old-<sha>" directory
    let old_working_log_dir = repo.storage.working_logs.join(format!("old-{}", base_sha));

    let effective_wl_dir = if working_log_dir.is_dir() {
        Some(&working_log_dir)
    } else if old_working_log_dir.is_dir() {
        Some(&old_working_log_dir)
    } else {
        None
    };

    let mut working_log_files: Vec<(String, PathBuf)> = Vec::new(); // (relative_name, path)
    if let Some(wl_dir) = effective_wl_dir {
        collect_dir_recursive(wl_dir, wl_dir, &mut working_log_files);
    } else if !base_sha.is_empty() {
        eprintln!(
            "Warning: working log directory not found for base commit {} (cleaned up after commit)",
            &base_sha[..7.min(base_sha.len())]
        );
    }

    // Build zip
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let zip_filename = format!("logs-{}-{}.zip", short_sha, timestamp);
    let output_path = parsed.output_dir.join(&zip_filename);

    let zip_file = match fs::File::create(&output_path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("Failed to create zip file: {}", e);
            std::process::exit(1);
        }
    };

    let mut zip = ZipWriter::new(zip_file);
    let options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);

    let prefix = format!("logs-{}", short_sha);

    // metadata/authorship-note.txt
    add_to_zip(&mut zip, &format!("{}/metadata/authorship-note.txt", prefix), note_text.as_bytes(), options);

    // metadata/commit-info.txt
    add_to_zip(&mut zip, &format!("{}/metadata/commit-info.txt", prefix), commit_info.as_bytes(), options);

    // metadata/commit.diff
    add_to_zip(&mut zip, &format!("{}/metadata/commit.diff", prefix), commit_diff.as_bytes(), options);

    // metadata/git-ai-version.txt
    add_to_zip(&mut zip, &format!("{}/metadata/git-ai-version.txt", prefix), GIT_AI_VERSION.as_bytes(), options);

    // working-log/<relative-path>
    for (relative_name, path) in &working_log_files {
        let zip_path = format!("{}/working-log/{}", prefix, relative_name);
        match fs::read(path) {
            Ok(data) => add_to_zip(&mut zip, &zip_path, &data, options),
            Err(e) => eprintln!(
                "Warning: failed to read working log {}: {}",
                path.display(),
                e
            ),
        }
    }

    // transcripts/<agent-tool>/<filename>
    for (agent_tool, path) in &transcript_files {
        let filename = path.file_name().unwrap_or_default().to_string_lossy();
        let zip_path = format!("{}/transcripts/{}/{}", prefix, agent_tool, filename);
        match fs::read(path) {
            Ok(data) => add_to_zip(&mut zip, &zip_path, &data, options),
            Err(e) => eprintln!(
                "Warning: failed to read {}: {}",
                path.display(),
                e
            ),
        }
    }


    if let Err(e) = zip.finish() {
        eprintln!("Failed to finalize zip: {}", e);
        std::process::exit(1);
    }

    // Print summary
    let total_files = transcript_files.len() + working_log_files.len() + 4; // 4 metadata files
    println!("Created {} ({} files)", output_path.display(), total_files);

    let agent_tools: Vec<String> = agents.iter().map(|(a, _)| a.tool.clone()).collect();
    println!("Agents: {}", agent_tools.join(", "));

    if !transcript_files.is_empty() {
        println!("Transcripts: {}", transcript_files.len());
    }
    if !working_log_files.is_empty() {
        println!("Working logs: {}", working_log_files.len());
    }
}

// -- Arg parsing --

struct ParsedArgs {
    commit_sha: String,
    output_dir: PathBuf,
}

fn parse_args(args: &[String]) -> Result<ParsedArgs, String> {
    if args.is_empty() {
        return Err("Usage: git-ai logs <commitsha> [-o <outputfolder>]".to_string());
    }

    let mut commit_sha: Option<String> = None;
    let mut output_dir: Option<PathBuf> = None;

    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];

        if arg == "--help" || arg == "-h" {
            eprintln!("Usage: git-ai logs <commitsha> [-o <outputfolder>]");
            eprintln!();
            eprintln!("Collect agent transcripts and commit context into a zip file.");
            eprintln!();
            eprintln!("Arguments:");
            eprintln!("  <commitsha>       The commit SHA to collect logs for");
            eprintln!("  -o <outputfolder> Output directory (defaults to current directory)");
            std::process::exit(0);
        } else if arg == "-o" || arg == "--output" {
            if i + 1 >= args.len() {
                return Err("-o requires a value".to_string());
            }
            i += 1;
            output_dir = Some(PathBuf::from(&args[i]));
        } else if arg.starts_with('-') {
            return Err(format!("Unknown option: {}", arg));
        } else {
            if commit_sha.is_some() {
                return Err("Only one commit SHA can be specified".to_string());
            }
            commit_sha = Some(arg.clone());
        }

        i += 1;
    }

    let commit_sha = commit_sha.ok_or("logs requires a commit SHA")?;
    let output_dir = output_dir.unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    // Canonicalize the output directory to resolve any ".." or symlink components
    let output_dir = output_dir.canonicalize().map_err(|e| {
        format!("Output directory does not exist or is inaccessible: {} ({})", output_dir.display(), e)
    })?;

    if !output_dir.is_dir() {
        return Err(format!("Output path is not a directory: {}", output_dir.display()));
    }

    Ok(ParsedArgs {
        commit_sha,
        output_dir,
    })
}

// -- Transcript path resolution --

/// Resolve transcript file paths for a given agent.
fn resolve_transcript_paths(
    agent_id: &AgentId,
    custom_attrs: Option<&std::collections::HashMap<String, String>>,
    repo_cwd: &std::path::Path,
) -> Vec<PathBuf> {
    let mut transcripts = Vec::new();

    let home = match dirs::home_dir() {
        Some(h) => h,
        None => {
            eprintln!("Warning: could not determine home directory");
            return transcripts;
        }
    };

    match agent_id.tool.as_str() {
        "claude" => {
            let encoded_cwd = encode_path_claude(repo_cwd);
            let session_path = home
                .join(".claude/projects")
                .join(&encoded_cwd)
                .join(format!("{}.jsonl", &agent_id.id));
            transcripts.push(session_path.clone());

            // Fallback: glob for matching files
            if !session_path.exists() {
                let pattern = home
                    .join(".claude/projects")
                    .join(&encoded_cwd)
                    .join("*.jsonl");
                if let Ok(entries) = glob::glob(&pattern.to_string_lossy()) {
                    for entry in entries.flatten() {
                        if entry
                            .file_stem()
                            .map(|s| s.to_string_lossy().contains(&agent_id.id))
                            .unwrap_or(false)
                        {
                            transcripts.push(entry);
                        }
                    }
                }
            }
        }
        "cursor" => {
            let pattern = home
                .join(".cursor/projects/*/agent-transcripts")
                .join(&agent_id.id)
                .join(format!("{}.jsonl", &agent_id.id));
            if let Ok(entries) = glob::glob(&pattern.to_string_lossy()) {
                for entry in entries.flatten() {
                    transcripts.push(entry);
                }
            }

        }
        "codex" => {
            let codex_home = env::var("CODEX_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|_| home.join(".codex"));

            // Search sessions
            let pattern = codex_home
                .join("sessions/**/rollout-*.jsonl")
                .to_string_lossy()
                .to_string();
            if let Ok(entries) = glob::glob(&pattern) {
                for entry in entries.flatten() {
                    if entry
                        .to_string_lossy()
                        .contains(&agent_id.id)
                    {
                        transcripts.push(entry);
                    }
                }
            }

            // Also check archived sessions
            let archived_pattern = codex_home
                .join("archived_sessions/**/rollout-*.jsonl")
                .to_string_lossy()
                .to_string();
            if let Ok(entries) = glob::glob(&archived_pattern) {
                for entry in entries.flatten() {
                    if entry
                        .to_string_lossy()
                        .contains(&agent_id.id)
                    {
                        transcripts.push(entry);
                    }
                }
            }
        }
        "github-copilot" => {
            let session_dir = home.join(".copilot/session-state");
            if session_dir.is_dir() {
                // Copy all files in the session-state dir
                if let Ok(entries) = fs::read_dir(&session_dir) {
                    for entry in entries.flatten() {
                        transcripts.push(entry.path());
                    }
                }
            }

        }
        "windsurf" => {
            let transcript = home
                .join(".windsurf/transcripts")
                .join(format!("{}.jsonl", &agent_id.id));
            transcripts.push(transcript.clone());

            // Fallback glob
            if !transcript.exists() {
                let pattern = home
                    .join(".windsurf/transcripts/*.jsonl")
                    .to_string_lossy()
                    .to_string();
                if let Ok(entries) = glob::glob(&pattern) {
                    for entry in entries.flatten() {
                        if entry
                            .file_stem()
                            .map(|s| s.to_string_lossy().contains(&agent_id.id))
                            .unwrap_or(false)
                        {
                            transcripts.push(entry);
                        }
                    }
                }
            }
        }
        "gemini" => {
            // Gemini stores transcript_path in custom_attributes during checkpoint
            if let Some(attrs) = custom_attrs {
                if let Some(path) = attrs.get("transcript_path") {
                    transcripts.push(PathBuf::from(path));
                } else {
                    eprintln!("Warning: gemini agent has no transcript_path in metadata");
                }
            } else {
                eprintln!("Warning: gemini agent has no metadata — transcript path unknown");
            }
        }
        "opencode" => {
            // OpenCode stores data in SQLite (skipped — too large for bundle).
            // Check legacy flat-file storage as fallback.
            let storage_path = env::var("GIT_AI_OPENCODE_STORAGE_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|_| home.join(".opencode"));
            let legacy_path = storage_path
                .join("storage/message")
                .join(&agent_id.id);
            if legacy_path.is_dir() {
                if let Ok(entries) = fs::read_dir(&legacy_path) {
                    for entry in entries.flatten() {
                        transcripts.push(entry.path());
                    }
                }
            }
        }
        "continue-cli" => {
            // Check custom_attributes for transcript_path first
            if let Some(attrs) = custom_attrs {
                if let Some(path) = attrs.get("transcript_path") {
                    transcripts.push(PathBuf::from(path));
                    return transcripts;
                }
            }

            // Fallback: glob ~/.continue/projects/**/*.jsonl
            let pattern = home
                .join(".continue/projects/**/*.jsonl")
                .to_string_lossy()
                .to_string();
            if let Ok(entries) = glob::glob(&pattern) {
                for entry in entries.flatten() {
                    if entry
                        .to_string_lossy()
                        .contains(&agent_id.id)
                    {
                        transcripts.push(entry);
                    }
                }
            }
        }
        "droid" => {
            let encoded_cwd = encode_path_droid(repo_cwd);
            let session_path = home
                .join(".factory/sessions")
                .join(&encoded_cwd)
                .join(format!("{}.jsonl", &agent_id.id));
            transcripts.push(session_path);

            // Companion settings file
            let settings_path = home
                .join(".factory/sessions")
                .join(&encoded_cwd)
                .join(format!("{}.settings.json", &agent_id.id));
            if settings_path.exists() {
                transcripts.push(settings_path);
            }
        }
        "amp" => {
            // Check custom_attributes first
            if let Some(attrs) = custom_attrs {
                if let Some(path) = attrs.get("transcript_path") {
                    transcripts.push(PathBuf::from(path));
                    return transcripts;
                }
            }

            let threads_dir = env::var("GIT_AI_AMP_THREADS_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|_| {
                    if cfg!(target_os = "macos") || cfg!(target_os = "linux") {
                        home.join(".local/share/amp/threads")
                    } else {
                        // Windows
                        home.join("AppData/Local/amp/threads")
                    }
                });

            let thread_file = threads_dir.join(format!("{}.json", &agent_id.id));
            transcripts.push(thread_file.clone());

            // If file not found, the ID might be a tool_use_id — search thread files
            if !thread_file.exists() {
                let pattern = threads_dir.join("*.json").to_string_lossy().to_string();
                if let Ok(entries) = glob::glob(&pattern) {
                    for entry in entries.flatten() {
                        if let Ok(content) = fs::read_to_string(&entry) {
                            if content.contains(&agent_id.id) {
                                transcripts.push(entry);
                                break;
                            }
                        }
                    }
                }
            }
        }
        unknown => {
            eprintln!(
                "Warning: unknown agent '{}', skipping transcript collection",
                unknown
            );
        }
    }

    transcripts
}

// -- Path encoding helpers --

/// Encode a path the way Claude Code does: replace non-alphanumeric chars with `-`
fn encode_path_claude(path: &Path) -> String {
    path.to_string_lossy()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect()
}

/// Encode a path the way Droid/Factory does: replace `/` with `-`
fn encode_path_droid(path: &Path) -> String {
    path.to_string_lossy().replace('/', "-")
}

// -- Git helpers --

fn get_commit_info(repo: &Repository, sha: &str) -> String {
    let mut args = repo.global_args_for_exec();
    args.extend(["log".to_string(), "-1".to_string(), "--format=fuller".to_string(), sha.to_string()]);
    match exec_git(&args) {
        Ok(output) => String::from_utf8(output.stdout).unwrap_or_default(),
        Err(e) => format!("Failed to get commit info: {}", e),
    }
}

fn get_commit_diff(repo: &Repository, sha: &str) -> String {
    let mut args = repo.global_args_for_exec();
    args.extend([
        "diff".to_string(),
        format!("{}~1..{}", sha, sha),
    ]);
    match exec_git(&args) {
        Ok(output) => String::from_utf8(output.stdout).unwrap_or_default(),
        Err(e) => format!("Failed to get commit diff: {}", e),
    }
}

// -- Directory collection helper --

/// Recursively collect all files in a directory, producing (relative_path, absolute_path) pairs.
/// Skips the `blobs/` subdirectory since those are full file snapshots that bloat the bundle.
fn collect_dir_recursive(base: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };

    // Canonicalize base once for containment checks
    let canonical_base = match base.canonicalize() {
        Ok(p) => p,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();

        // Verify the path stays within the base directory (guards against symlink escapes)
        if let Ok(canonical) = path.canonicalize() {
            if !canonical.starts_with(&canonical_base) {
                eprintln!(
                    "Warning: skipping path that escapes base directory: {}",
                    path.display()
                );
                continue;
            }
        }

        if path.is_dir() {
            // Skip blobs/ (full file snapshots) and INITIAL (seed attribution state)
            let name = path.file_name().unwrap_or_default();
            if name == "blobs" {
                continue;
            }
            collect_dir_recursive(base, &path, out);
        } else if path.file_name().unwrap_or_default() != "INITIAL" {
            let relative = path
                .strip_prefix(base)
                .unwrap_or(&path)
                .to_string_lossy()
                .to_string();
            out.push((relative, path));
        }
    }
}

// -- Zip helper --

fn add_to_zip(
    zip: &mut ZipWriter<fs::File>,
    path: &str,
    data: &[u8],
    options: SimpleFileOptions,
) {
    if let Err(e) = zip.start_file(path, options) {
        eprintln!("Warning: failed to add {} to zip: {}", path, e);
        return;
    }
    if let Err(e) = zip.write_all(data) {
        eprintln!("Warning: failed to write {} to zip: {}", path, e);
    }
}
