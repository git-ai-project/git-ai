use std::env;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use git_ai::core::git_binary::git_cmd as git_command;

pub fn debug_log(msg: &str) {
    if cfg!(debug_assertions) || env::var("GIT_AI_DEBUG").as_deref() == Ok("1") {
        eprintln!("[git-ai] {}", msg);
    }
}

pub fn git_cmd(args: &[&str]) -> Result<String, String> {
    let output = git_command()
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
pub fn git_cmd_in(dir: &Path, args: &[&str]) -> Result<String, String> {
    let output = git_command()
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

/// Like resolve_repo_info but from a specific working directory.
pub fn resolve_repo_info_in(dir: &Path) -> Result<(String, PathBuf, String), String> {
    let output = git_command()
        .args(["rev-parse", "--show-toplevel", "--git-dir", "HEAD"])
        .current_dir(dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("failed to run git rev-parse: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(format!("git rev-parse failed: {}", stderr));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.trim_end().lines().collect();
    if lines.len() < 3 {
        return Err("git rev-parse returned fewer than 3 lines".to_string());
    }

    let toplevel = lines[0].to_string();
    let git_dir_raw = lines[1];
    let git_dir = if Path::new(git_dir_raw).is_relative() {
        PathBuf::from(&toplevel).join(git_dir_raw)
    } else {
        PathBuf::from(git_dir_raw)
    };
    let head_sha = lines[2].to_string();

    Ok((toplevel, git_dir, head_sha))
}

/// Given an absolute file path, find the git repository root that contains it.
/// Walks up from the file's parent directory looking for `.git/` (directory or file for worktrees).
pub fn find_repo_root_for_path(file_path: &Path) -> Option<PathBuf> {
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

/// Discover repo root and resolved git dir from a starting path, without spawning git.
/// Handles both normal repos (.git is a directory) and worktrees (.git is a file).
pub fn discover_repo_and_gitdir(start: &Path) -> Option<(PathBuf, PathBuf)> {
    let mut current = start;
    loop {
        let git_path = current.join(".git");
        if git_path.is_dir() {
            return Some((current.to_path_buf(), git_path));
        }
        if git_path.is_file()
            && let Ok(content) = std::fs::read_to_string(&git_path)
            && let Some(dir) = content.strip_prefix("gitdir: ")
        {
            let dir = dir.trim();
            let resolved = if Path::new(dir).is_relative() {
                current.join(dir)
            } else {
                PathBuf::from(dir)
            };
            return Some((current.to_path_buf(), resolved));
        }
        current = current.parent()?;
    }
}

/// Read HEAD SHA from the git dir filesystem (ref file or packed-refs).
/// Handles worktrees by resolving refs through the common git dir.
/// Returns None if HEAD can't be resolved (e.g., empty repo with no commits).
pub fn read_head_sha(git_dir: &Path) -> Option<String> {
    let head_content = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head_content = head_content.trim();
    if head_content.len() == 40 && head_content.chars().all(|c| c.is_ascii_hexdigit()) {
        return Some(head_content.to_string());
    }
    let ref_path = head_content.strip_prefix("ref: ")?;

    // For worktrees, refs are in the common dir (not the worktree-specific dir)
    let common_dir = resolve_common_dir(git_dir);

    // Try loose ref in common dir first, then in git_dir itself
    for search_dir in [&common_dir, &git_dir.to_path_buf()] {
        let loose_ref = search_dir.join(ref_path);
        if let Ok(sha) = std::fs::read_to_string(&loose_ref) {
            let sha = sha.trim();
            if sha.len() >= 40 {
                return Some(sha[..40].to_string());
            }
        }
    }

    // Check packed-refs in common dir
    let packed_refs = common_dir.join("packed-refs");
    if let Ok(packed) = std::fs::read_to_string(&packed_refs) {
        for line in packed.lines() {
            if line.starts_with('#') || line.starts_with('^') {
                continue;
            }
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 && parts[1] == ref_path {
                return Some(parts[0].to_string());
            }
        }
    }
    None
}

/// Resolve the common git dir (for worktrees, this is the main repo's .git dir).
fn resolve_common_dir(git_dir: &Path) -> PathBuf {
    let commondir_file = git_dir.join("commondir");
    if let Ok(content) = std::fs::read_to_string(&commondir_file) {
        let content = content.trim();
        if Path::new(content).is_relative() {
            return git_dir.join(content);
        }
        return PathBuf::from(content);
    }
    git_dir.to_path_buf()
}
