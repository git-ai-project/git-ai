use crate::error::GitAiError;
use crate::mdm::agents::get_all_installers;
use crate::mdm::hook_installer::HookInstallerParams;
use crate::mdm::skills_installer;
use crate::mdm::spinner::{print_diff, Spinner};
use crate::mdm::utils::get_current_binary_path;
use std::collections::HashMap;

/// Installation status for a tool
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallStatus {
    /// Tool was not detected or failed to install
    NotFound,
    /// Hooks/extensions were successfully installed or updated
    Installed,
    /// Hooks/extensions were already up to date
    AlreadyInstalled,
}

impl InstallStatus {
    /// Convert status to string representation
    pub fn as_str(&self) -> &'static str {
        match self {
            InstallStatus::NotFound => "not-found",
            InstallStatus::Installed => "installed",
            InstallStatus::AlreadyInstalled => "already-installed",
        }
    }
}

/// Convert a HashMap of tool statuses to string keys and values
pub fn to_hashmap(statuses: HashMap<String, InstallStatus>) -> HashMap<String, String> {
    statuses
        .into_iter()
        .map(|(k, v)| (k, v.as_str().to_string()))
        .collect()
}

/// Main entry point for install-hooks command
pub fn run(args: &[String]) -> Result<HashMap<String, String>, GitAiError> {
    // Parse flags
    let mut dry_run = false;
    let mut verbose = false;
    for arg in args {
        if arg == "--dry-run" || arg == "--dry-run=true" {
            dry_run = true;
        }
        if arg == "--verbose" || arg == "-v" {
            verbose = true;
        }
    }

    // Get absolute path to the current binary
    let binary_path = get_current_binary_path()?;
    let params = HookInstallerParams { binary_path };

    // Run async operations with smol and convert result
    let statuses = smol::block_on(async_run_install(&params, dry_run, verbose))?;
    Ok(to_hashmap(statuses))
}

/// Main entry point for uninstall-hooks command
pub fn run_uninstall(args: &[String]) -> Result<HashMap<String, String>, GitAiError> {
    // Parse flags
    let mut dry_run = false;
    let mut verbose = false;
    for arg in args {
        if arg == "--dry-run" || arg == "--dry-run=true" {
            dry_run = true;
        }
        if arg == "--verbose" || arg == "-v" {
            verbose = true;
        }
    }

    // Get absolute path to the current binary
    let binary_path = get_current_binary_path()?;
    let params = HookInstallerParams { binary_path };

    // Run async operations with smol and convert result
    let statuses = smol::block_on(async_run_uninstall(&params, dry_run, verbose))?;
    Ok(to_hashmap(statuses))
}

async fn async_run_install(
    params: &HookInstallerParams,
    dry_run: bool,
    verbose: bool,
) -> Result<HashMap<String, InstallStatus>, GitAiError> {
    let mut any_checked = false;
    let mut has_changes = false;
    let mut statuses: HashMap<String, InstallStatus> = HashMap::new();

    // Install skills first (these are global, not per-agent)
    // Skills are always nuked and reinstalled fresh (silently)
    if let Ok(result) = skills_installer::install_skills(dry_run, verbose) {
        if result.changed {
            has_changes = true;
        }
    }

    let installers = get_all_installers();

    for installer in installers {
        let name = installer.name();
        let id = installer.id();

        // Check if tool is installed and hooks status
        match installer.check_hooks(params) {
            Ok(check_result) => {
                if !check_result.tool_installed {
                    statuses.insert(id.to_string(), InstallStatus::NotFound);
                    continue;
                }

                any_checked = true;

                // Install/update hooks
                let spinner = Spinner::new(&format!("{}: checking hooks", name));
                spinner.start();

                match installer.install_hooks(params, dry_run) {
                    Ok(Some(diff)) => {
                        if dry_run {
                            spinner.pending(&format!("{}: Pending updates", name));
                        } else {
                            spinner.success(&format!("{}: Hooks updated", name));
                        }
                        if verbose {
                            println!();
                            print_diff(&diff);
                        }
                        has_changes = true;
                        statuses.insert(id.to_string(), InstallStatus::Installed);
                    }
                    Ok(None) => {
                        spinner.success(&format!("{}: Hooks already up to date", name));
                        statuses.insert(id.to_string(), InstallStatus::AlreadyInstalled);
                    }
                    Err(e) => {
                        spinner.error(&format!("{}: Failed to update hooks", name));
                        eprintln!("  Error: {}", e);
                        statuses.insert(id.to_string(), InstallStatus::NotFound);
                    }
                }

                // Install extras (extensions, git.path, etc.)
                match installer.install_extras(params, dry_run) {
                    Ok(results) => {
                        for result in results {
                            if result.changed {
                                has_changes = true;
                            }
                            if result.changed && !dry_run {
                                let extra_spinner = Spinner::new(&result.message);
                                extra_spinner.start();
                                extra_spinner.success(&result.message);
                            } else if result.changed && dry_run {
                                let extra_spinner = Spinner::new(&result.message);
                                extra_spinner.start();
                                extra_spinner.pending(&result.message);
                            } else if result.message.contains("already") {
                                let extra_spinner = Spinner::new(&result.message);
                                extra_spinner.start();
                                extra_spinner.success(&result.message);
                            } else if result.message.contains("Unable") || result.message.contains("manually") {
                                let extra_spinner = Spinner::new(&result.message);
                                extra_spinner.start();
                                extra_spinner.pending(&result.message);
                            }
                            if verbose {
                                if let Some(diff) = result.diff {
                                    println!();
                                    print_diff(&diff);
                                }
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("  Error installing extras for {}: {}", name, e);
                    }
                }
            }
            Err(version_error) => {
                any_checked = true;
                let spinner = Spinner::new(&format!("{}: checking version", name));
                spinner.start();
                spinner.error(&format!("{}: Version check failed", name));
                eprintln!("  Error: {}", version_error);
                eprintln!("  Please update {} to continue using git-ai hooks", name);
                statuses.insert(id.to_string(), InstallStatus::NotFound);
            }
        }
    }

    if !any_checked {
        println!("No compatible IDEs or agent configurations detected. Nothing to install.");
    } else if has_changes && dry_run {
        println!("\n\x1b[33m⚠ Dry-run mode (default). No changes were made.\x1b[0m");
        println!("To apply these changes, run:");
        println!("\x1b[1m  git-ai install-hooks --dry-run=false\x1b[0m");
    }

    Ok(statuses)
}

async fn async_run_uninstall(
    params: &HookInstallerParams,
    dry_run: bool,
    verbose: bool,
) -> Result<HashMap<String, InstallStatus>, GitAiError> {
    let mut any_checked = false;
    let mut has_changes = false;
    let mut statuses: HashMap<String, InstallStatus> = HashMap::new();

    // Uninstall skills first (these are global, not per-agent, silently)
    if let Ok(result) = skills_installer::uninstall_skills(dry_run, verbose) {
        if result.changed {
            has_changes = true;
            statuses.insert("skills".to_string(), InstallStatus::Installed);
        } else {
            statuses.insert("skills".to_string(), InstallStatus::AlreadyInstalled);
        }
    }

    let installers = get_all_installers();

    for installer in installers {
        let name = installer.name();
        let id = installer.id();

        // Check if tool is installed
        match installer.check_hooks(params) {
            Ok(check_result) => {
                if !check_result.tool_installed {
                    statuses.insert(id.to_string(), InstallStatus::NotFound);
                    continue;
                }

                if !check_result.hooks_installed {
                    statuses.insert(id.to_string(), InstallStatus::NotFound);
                    continue;
                }

                any_checked = true;

                // Uninstall hooks
                let spinner = Spinner::new(&format!("{}: removing hooks", name));
                spinner.start();

                match installer.uninstall_hooks(params, dry_run) {
                    Ok(Some(diff)) => {
                        if dry_run {
                            spinner.pending(&format!("{}: Pending removal", name));
                        } else {
                            spinner.success(&format!("{}: Hooks removed", name));
                        }
                        if verbose {
                            println!();
                            print_diff(&diff);
                        }
                        has_changes = true;
                        statuses.insert(id.to_string(), InstallStatus::Installed);
                    }
                    Ok(None) => {
                        spinner.success(&format!("{}: No hooks to remove", name));
                        statuses.insert(id.to_string(), InstallStatus::AlreadyInstalled);
                    }
                    Err(e) => {
                        spinner.error(&format!("{}: Failed to remove hooks", name));
                        eprintln!("  Error: {}", e);
                        statuses.insert(id.to_string(), InstallStatus::NotFound);
                    }
                }

                // Uninstall extras
                match installer.uninstall_extras(params, dry_run) {
                    Ok(results) => {
                        for result in results {
                            if result.changed {
                                has_changes = true;
                            }
                            if !result.message.is_empty() {
                                let extra_spinner = Spinner::new(&result.message);
                                extra_spinner.start();
                                if result.changed {
                                    extra_spinner.success(&result.message);
                                } else {
                                    extra_spinner.pending(&result.message);
                                }
                            }
                            if verbose {
                                if let Some(diff) = result.diff {
                                    println!();
                                    print_diff(&diff);
                                }
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("  Error uninstalling extras for {}: {}", name, e);
                    }
                }
            }
            Err(e) => {
                eprintln!("  Error checking {}: {}", name, e);
                statuses.insert(id.to_string(), InstallStatus::NotFound);
            }
        }
    }

    if !any_checked {
        println!("No git-ai hooks found to uninstall.");
    } else if has_changes && dry_run {
        println!("\n\x1b[33m⚠ Dry-run mode (default). No changes were made.\x1b[0m");
        println!("To apply these changes, run:");
        println!("\x1b[1m  git-ai uninstall-hooks --dry-run=false\x1b[0m");
    } else if !has_changes {
        println!("All git-ai hooks have been removed.");
    }

    Ok(statuses)
}
