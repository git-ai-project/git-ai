use crate::config::{load_file_config_public, save_file_config};
#[cfg(target_os = "macos")]
use crate::mdm::agents::XcodeInstaller;
use crate::mdm::utils::home_dir;
use std::path::{Path, PathBuf};

struct CommandOutput {
    lines: Vec<String>,
    warnings: Vec<String>,
}

impl CommandOutput {
    fn new() -> Self {
        Self {
            lines: Vec::new(),
            warnings: Vec::new(),
        }
    }

    fn push_line(&mut self, line: impl Into<String>) {
        self.lines.push(line.into());
    }

    fn push_warning(&mut self, warning: impl Into<String>) {
        self.warnings.push(warning.into());
    }
}

pub fn handle_xcode(args: &[String]) {
    if args.is_empty() || matches!(args[0].as_str(), "help" | "--help" | "-h") {
        print_xcode_help();
        return;
    }

    match run_xcode(args) {
        Ok(output) => {
            for line in output.lines {
                println!("{line}");
            }
            for warning in output.warnings {
                eprintln!("{warning}");
            }
        }
        Err(error) => {
            eprintln!("Error: {error}");
            std::process::exit(1);
        }
    }
}

fn print_xcode_help() {
    eprintln!("git-ai xcode - Manage Xcode watcher paths and LaunchAgent");
    eprintln!();
    eprintln!("Usage:");
    eprintln!("  git-ai xcode add-path <path>");
    eprintln!("  git-ai xcode remove-path <path>");
    eprintln!("  git-ai xcode list-paths");
    eprintln!("  git-ai xcode reload");
    eprintln!();
    eprintln!("Examples:");
    eprintln!("  git-ai xcode add-path ~/work/ios");
    eprintln!("  git-ai xcode remove-path ~/work/ios");
    eprintln!("  git-ai xcode list-paths");
    eprintln!("  git-ai xcode reload");
    eprintln!();
    std::process::exit(0);
}

#[cfg(target_os = "macos")]
fn run_xcode(args: &[String]) -> Result<CommandOutput, String> {
    match args[0].as_str() {
        "add-path" => {
            let path = args
                .get(1)
                .ok_or_else(|| "Usage: git-ai xcode add-path <path>".to_string())?;
            run_add_path(path)
        }
        "remove-path" => {
            let path = args
                .get(1)
                .ok_or_else(|| "Usage: git-ai xcode remove-path <path>".to_string())?;
            run_remove_path(path)
        }
        "list-paths" => run_list_paths(),
        "reload" => run_reload(),
        other => Err(format!("Unknown xcode subcommand: {other}")),
    }
}

#[cfg(not(target_os = "macos"))]
fn run_xcode(_args: &[String]) -> Result<CommandOutput, String> {
    Err("Xcode watcher configuration is only supported on macOS".to_string())
}

#[cfg(target_os = "macos")]
fn run_add_path(path: &str) -> Result<CommandOutput, String> {
    let mut file_config = load_file_config_public()?;
    let existing_paths = XcodeInstaller::configured_paths_from_file_config(&file_config)?;
    let new_path = XcodeInstaller::validate_new_watch_path(Path::new(path))?;

    let mut output = CommandOutput::new();

    if existing_paths.iter().any(|existing| existing == &new_path) {
        output.push_line(format!(
            "Xcode: Watch path already configured: {}",
            new_path.display()
        ));
        apply_launch_agent_output(&existing_paths, &mut output)?;
        return Ok(output);
    }

    if let Some(parent) = existing_paths
        .iter()
        .find(|existing| new_path.starts_with(existing.as_path()))
    {
        output.push_line(format!(
            "Xcode: {} is already covered by existing watch root {}",
            new_path.display(),
            parent.display()
        ));
        apply_launch_agent_output(&existing_paths, &mut output)?;
        return Ok(output);
    }

    let removed_descendants = existing_paths
        .iter()
        .filter(|existing| existing.starts_with(&new_path))
        .count();

    let mut updated_paths = existing_paths.clone();
    updated_paths.push(new_path.clone());
    updated_paths = XcodeInstaller::normalize_watch_paths(updated_paths);

    file_config.xcode_paths = XcodeInstaller::serialize_watch_paths(&updated_paths);
    save_file_config(&file_config)?;

    output.push_line(format!("Xcode: Added watch path {}", new_path.display()));
    if removed_descendants > 0 {
        output.push_line(format!(
            "Xcode: Collapsed {} narrower path(s) under {}",
            removed_descendants,
            new_path.display()
        ));
    }
    apply_launch_agent_output(&updated_paths, &mut output)?;
    Ok(output)
}

#[cfg(target_os = "macos")]
fn run_remove_path(path: &str) -> Result<CommandOutput, String> {
    let mut file_config = load_file_config_public()?;
    let existing_paths = XcodeInstaller::configured_paths_from_file_config(&file_config)?;
    let normalized_target = normalize_remove_target(path)?;

    let updated_paths: Vec<PathBuf> = existing_paths
        .iter()
        .filter(|existing| **existing != normalized_target)
        .cloned()
        .collect();

    let mut output = CommandOutput::new();
    if updated_paths.len() == existing_paths.len() {
        output.push_line(format!(
            "Xcode: Watch path not configured: {}",
            normalized_target.display()
        ));
        return Ok(output);
    }

    file_config.xcode_paths = XcodeInstaller::serialize_watch_paths(&updated_paths);
    save_file_config(&file_config)?;

    output.push_line(format!(
        "Xcode: Removed watch path {}",
        normalized_target.display()
    ));
    apply_launch_agent_output(&updated_paths, &mut output)?;
    Ok(output)
}

#[cfg(target_os = "macos")]
fn run_list_paths() -> Result<CommandOutput, String> {
    let paths = XcodeInstaller::configured_paths_from_disk()?;
    let mut output = CommandOutput::new();

    if paths.is_empty() {
        output.push_line("Xcode: No watch paths configured");
        return Ok(output);
    }

    for path in paths {
        output.push_line(path.display().to_string());
    }

    Ok(output)
}

#[cfg(target_os = "macos")]
fn run_reload() -> Result<CommandOutput, String> {
    let paths = XcodeInstaller::configured_paths_from_disk()?;
    let mut output = CommandOutput::new();
    apply_launch_agent_output(&paths, &mut output)?;
    Ok(output)
}

#[cfg(target_os = "macos")]
fn apply_launch_agent_output(paths: &[PathBuf], output: &mut CommandOutput) -> Result<(), String> {
    let apply_result = XcodeInstaller::apply_launch_agent(paths)?;
    output.push_line(apply_result.message);
    if let Some(warning) = apply_result.warning {
        output.push_warning(format!("Xcode: {warning}"));
    }
    Ok(())
}

fn normalize_remove_target(path: &str) -> Result<PathBuf, String> {
    let raw = Path::new(path);
    let expanded = if let Ok(stripped) = raw.strip_prefix("~") {
        home_dir().join(stripped)
    } else if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| format!("Unable to resolve current directory: {}", e))?
            .join(raw)
    };

    match std::fs::canonicalize(&expanded) {
        Ok(canonical) => Ok(canonical),
        Err(_) => Ok(expanded),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use tempfile::tempdir;

    fn with_temp_home<F: FnOnce(&Path)>(f: F) {
        let temp = tempdir().unwrap();
        let home = temp.path().to_path_buf();

        let prev_home = std::env::var_os("HOME");
        let prev_userprofile = std::env::var_os("USERPROFILE");

        unsafe {
            std::env::set_var("HOME", &home);
            std::env::set_var("USERPROFILE", &home);
        }

        f(&home);

        unsafe {
            match prev_home {
                Some(value) => std::env::set_var("HOME", value),
                None => std::env::remove_var("HOME"),
            }
            match prev_userprofile {
                Some(value) => std::env::set_var("USERPROFILE", value),
                None => std::env::remove_var("USERPROFILE"),
            }
        }
    }

    #[test]
    #[serial]
    fn test_normalize_remove_target_resolves_relative_paths() {
        with_temp_home(|home| {
            let workspace = home.join("workspace");
            std::fs::create_dir_all(&workspace).unwrap();

            let prev_dir = std::env::current_dir().unwrap();
            std::env::set_current_dir(home).unwrap();
            let resolved = normalize_remove_target("workspace").unwrap();
            std::env::set_current_dir(prev_dir).unwrap();

            assert_eq!(resolved, std::fs::canonicalize(workspace).unwrap());
        });
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[serial]
    fn test_add_path_collapse_is_visible_in_saved_config() {
        with_temp_home(|home| {
            let parent = home.join("ios");
            let child = parent.join("AppA");
            std::fs::create_dir_all(&child).unwrap();
            std::fs::create_dir_all(XcodeInstaller::watcher_binary_path().parent().unwrap())
                .unwrap();
            std::fs::write(XcodeInstaller::watcher_binary_path(), "binary").unwrap();

            let mut file_config = load_file_config_public().unwrap();
            file_config.xcode_paths = Some(vec![child.to_string_lossy().to_string()]);
            save_file_config(&file_config).unwrap();

            let output = run_add_path(parent.to_str().unwrap()).unwrap();
            assert!(output.lines.iter().any(|line| line.contains("Collapsed 1")));

            let saved = load_file_config_public().unwrap();
            assert_eq!(
                saved.xcode_paths,
                Some(vec![
                    std::fs::canonicalize(parent)
                        .unwrap()
                        .to_string_lossy()
                        .to_string()
                ])
            );
        });
    }
}
