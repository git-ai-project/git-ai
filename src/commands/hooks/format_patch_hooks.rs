use crate::commands::git_handlers::CommandHooksContext;
use crate::git::cli_parser::ParsedGitInvocation;
use crate::git::repository::Repository;
use crate::utils::debug_log;
use base64::Engine;

/// After `git format-patch` completes, inject X-Git-AI-Attribution headers
/// into each generated patch file. The header contains the base64-encoded
/// authorship note from the source commit.
pub fn post_format_patch_hook(
    _context: &CommandHooksContext,
    parsed_args: &ParsedGitInvocation,
    exit_status: std::process::ExitStatus,
    repository: &mut Repository,
) {
    debug_log("=== FORMAT-PATCH POST-COMMAND HOOK ===");

    if !exit_status.success() {
        debug_log("format-patch failed, skipping attribution embedding");
        return;
    }

    // Find the output directory from -o/--output-directory arg
    let output_dir = match find_output_directory(&parsed_args.command_args, repository) {
        Some(dir) => dir,
        None => {
            debug_log("Could not determine format-patch output directory, skipping");
            return;
        }
    };

    debug_log(&format!("Looking for patch files in: {}", output_dir));

    // Find all .patch files in the output directory
    let patch_files = match std::fs::read_dir(&output_dir) {
        Ok(entries) => {
            let mut files: Vec<std::path::PathBuf> = entries
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|ext| ext == "patch"))
                .collect();
            files.sort();
            files
        }
        Err(e) => {
            debug_log(&format!("Failed to read output directory: {}", e));
            return;
        }
    };

    debug_log(&format!("Found {} patch files", patch_files.len()));

    for patch_path in patch_files {
        if let Err(e) = embed_attribution_in_patch(&patch_path, repository) {
            debug_log(&format!(
                "Failed to embed attribution in {}: {}",
                patch_path.display(),
                e
            ));
        }
    }
}

/// Parse the output directory from format-patch arguments.
/// format-patch uses -o <dir> or --output-directory=<dir>.
/// If no -o is given, patches go to the current directory.
fn find_output_directory(args: &[String], repository: &Repository) -> Option<String> {
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "-o" || arg == "--output-directory" {
            if i + 1 < args.len() {
                return Some(args[i + 1].clone());
            }
        } else if let Some(dir) = arg.strip_prefix("--output-directory=") {
            return Some(dir.to_string());
        } else if arg.starts_with("-o") && arg.len() > 2 {
            // -o<dir> without space
            return Some(arg[2..].to_string());
        }
        i += 1;
    }

    // Default: current working directory (the repo workdir)
    repository
        .workdir()
        .ok()
        .map(|p| p.to_string_lossy().to_string())
}

/// Read a patch file, extract the source commit SHA from the `From <sha>` line,
/// look up its attribution note, and inject it as an X-Git-AI-Attribution header.
fn embed_attribution_in_patch(
    patch_path: &std::path::Path,
    repository: &Repository,
) -> Result<(), String> {
    let content = std::fs::read_to_string(patch_path).map_err(|e| format!("read patch: {}", e))?;

    // Extract the source commit SHA from the first line: "From <sha> Mon Sep 17 ..."
    let source_sha = content
        .lines()
        .next()
        .and_then(|line| {
            let line = line.trim();
            if line.starts_with("From ") {
                // "From <sha> Mon Sep 17 00:00:00 2001"
                line.split_whitespace().nth(1).map(|s| s.to_string())
            } else {
                None
            }
        })
        .ok_or_else(|| "could not extract source SHA from patch".to_string())?;

    debug_log(&format!(
        "Patch {} source commit: {}",
        patch_path.display(),
        source_sha
    ));

    // Look up the attribution note for this commit
    let note = match crate::git::refs::show_authorship_note(repository, &source_sha) {
        Some(note) => note,
        None => {
            debug_log(&format!("No attribution note for {}", source_sha));
            return Ok(());
        }
    };

    if note.trim().is_empty() {
        debug_log(&format!("Empty attribution note for {}", source_sha));
        return Ok(());
    }

    // Base64-encode the note
    let encoded = base64::engine::general_purpose::STANDARD.encode(note.as_bytes());

    // Insert the header after the Subject line (and any continuation lines)
    let mut lines: Vec<&str> = content.lines().collect();
    let mut insert_pos = None;

    for (i, line) in lines.iter().enumerate() {
        if line.starts_with("Subject:") {
            // Find the end of the Subject header (may span multiple lines with leading whitespace)
            let mut j = i + 1;
            while j < lines.len() && (lines[j].starts_with(' ') || lines[j].starts_with('\t')) {
                j += 1;
            }
            insert_pos = Some(j);
            break;
        }
    }

    if let Some(pos) = insert_pos {
        let header_line = format!("X-Git-AI-Attribution: {}", encoded);
        lines.insert(pos, &header_line);

        let new_content = lines.join("\n");
        // Preserve trailing newline if original had one
        let new_content = if content.ends_with('\n') {
            format!("{}\n", new_content)
        } else {
            new_content
        };

        std::fs::write(patch_path, new_content).map_err(|e| format!("write patch: {}", e))?;

        debug_log(&format!(
            "Embedded attribution header in {}",
            patch_path.display()
        ));
    } else {
        debug_log(&format!(
            "Could not find Subject line in {}",
            patch_path.display()
        ));
    }

    Ok(())
}
