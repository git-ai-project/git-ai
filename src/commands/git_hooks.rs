use std::fs;
use std::path::Path;
use std::process::Stdio;

use git_ai::core::git_binary::git_cmd;

pub fn handle_git_hooks(args: &[String]) {
    match args.first().map(String::as_str) {
        Some("remove") => remove_hooks(),
        Some("--help") | Some("-h") | Some("help") | None => {
            println!("git-ai git-hooks - Manage legacy git hook symlinks");
            println!();
            println!("Usage:");
            println!("  git-ai git-hooks remove   Remove deprecated git-ai hook symlinks");
        }
        Some(cmd) => {
            eprintln!("git-ai git-hooks: unknown subcommand '{}'", cmd);
            std::process::exit(1);
        }
    }
}

fn remove_hooks() {
    let hooks_path = match get_hooks_dir() {
        Some(p) => p,
        None => {
            eprintln!("Not in a git repository");
            std::process::exit(1);
        }
    };

    if !hooks_path.exists() {
        println!("No hooks directory found. Nothing to remove.");
        return;
    }

    let hook_names = &[
        "applypatch-msg",
        "pre-applypatch",
        "post-applypatch",
        "pre-commit",
        "pre-merge-commit",
        "prepare-commit-msg",
        "commit-msg",
        "post-commit",
        "pre-rebase",
        "post-checkout",
        "post-merge",
        "pre-push",
        "pre-auto-gc",
        "post-rewrite",
        "sendemail-validate",
    ];

    let mut removed = 0;
    for name in hook_names {
        let hook_path = hooks_path.join(name);
        if !hook_path.exists() {
            continue;
        }
        if is_git_ai_hook(&hook_path) {
            if let Err(e) = fs::remove_file(&hook_path) {
                eprintln!("  Failed to remove {}: {}", name, e);
            } else {
                println!("  Removed {}", name);
                removed += 1;
            }
        }
    }

    if removed == 0 {
        println!("No git-ai hook symlinks found.");
    } else {
        println!("Removed {} git-ai hook(s).", removed);
    }

    // Reset core.hooksPath if it points to a git-ai managed directory
    if let Ok(hooks_path_config) = get_git_config("core.hooksPath")
        && (hooks_path_config.contains("git-ai") || hooks_path_config.contains(".git-ai"))
    {
        let _ = unset_git_config("core.hooksPath");
        println!("Reset core.hooksPath");
    }
}

fn is_git_ai_hook(path: &Path) -> bool {
    if path.is_symlink()
        && let Ok(target) = fs::read_link(path)
    {
        let target_str = target.to_string_lossy();
        return target_str.contains("git-ai");
    }
    if let Ok(content) = fs::read_to_string(path) {
        return content.contains("git-ai");
    }
    false
}

fn get_hooks_dir() -> Option<std::path::PathBuf> {
    let output = git_cmd()
        .args(["rev-parse", "--git-dir"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let git_dir = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Some(std::path::PathBuf::from(git_dir).join("hooks"))
}

fn get_git_config(key: &str) -> Result<String, ()> {
    let output = git_cmd()
        .args(["config", "--get", key])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|_| ())?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err(())
    }
}

fn unset_git_config(key: &str) -> Result<(), ()> {
    let output = git_cmd()
        .args(["config", "--unset", key])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|_| ())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(())
    }
}
