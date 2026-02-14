use crate::commands::core_hooks::{
    INSTALLED_HOOKS, PREVIOUS_HOOKS_PATH_FILE, managed_core_hooks_dir, write_core_hook_scripts,
};
use crate::commands::flush_metrics_db::spawn_background_metrics_db_flush;
use crate::error::GitAiError;
use crate::mdm::agents::get_all_installers;
use crate::mdm::git_client_installer::GitClientInstallerParams;
use crate::mdm::git_clients::get_all_git_client_installers;
use crate::mdm::hook_installer::HookInstallerParams;
use crate::mdm::skills_installer;
use crate::mdm::spinner::{Spinner, print_diff};
use crate::mdm::utils::{get_current_binary_path, git_shim_path};
use crate::utils::GIT_AI_SKIP_CORE_HOOKS_ENV;
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

/// Installation status for a tool
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallStatus {
    /// Tool was not detected on the machine
    NotFound,
    /// Hooks/extensions were successfully installed or updated
    Installed,
    /// Hooks/extensions were already up to date
    AlreadyInstalled,
    /// Installation attempted but failed
    Failed,
}

impl InstallStatus {
    /// Convert status to string representation
    pub fn as_str(&self) -> &'static str {
        match self {
            InstallStatus::NotFound => "not_found",
            InstallStatus::Installed => "installed",
            InstallStatus::AlreadyInstalled => "already_installed",
            InstallStatus::Failed => "failed",
        }
    }
}

/// Detailed install result for metrics tracking
#[derive(Debug, Clone)]
pub struct InstallResult {
    pub status: InstallStatus,
    pub error: Option<String>,
    pub warnings: Vec<String>,
}

impl InstallResult {
    pub fn installed() -> Self {
        Self {
            status: InstallStatus::Installed,
            error: None,
            warnings: Vec::new(),
        }
    }

    pub fn already_installed() -> Self {
        Self {
            status: InstallStatus::AlreadyInstalled,
            error: None,
            warnings: Vec::new(),
        }
    }

    pub fn not_found() -> Self {
        Self {
            status: InstallStatus::NotFound,
            error: None,
            warnings: Vec::new(),
        }
    }

    pub fn failed(msg: impl Into<String>) -> Self {
        Self {
            status: InstallStatus::Failed,
            error: Some(msg.into()),
            warnings: Vec::new(),
        }
    }

    #[allow(dead_code)]
    pub fn with_warning(mut self, warning: impl Into<String>) -> Self {
        self.warnings.push(warning.into());
        self
    }

    /// Get message for ClickHouse (error if failed, else joined warnings)
    pub fn message_for_metrics(&self) -> Option<String> {
        if let Some(err) = &self.error {
            Some(err.clone())
        } else if !self.warnings.is_empty() {
            Some(self.warnings.join("; "))
        } else {
            None
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

const GIT_CORE_HOOKS_STATUS_ID: &str = "git-core-hooks";

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

    // Spawn background processes to flush metrics
    crate::observability::spawn_background_flush();
    spawn_background_metrics_db_flush();

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
    let mut any_checked = true;
    let mut has_changes = false;
    let mut statuses: HashMap<String, InstallStatus> = HashMap::new();
    // Track detailed results for metrics (tool_id, result)
    let mut detailed_results: Vec<(String, InstallResult)> = Vec::new();

    // Install skills first (these are global, not per-agent)
    // Skills are always nuked and reinstalled fresh (silently)
    if let Ok(result) = skills_installer::install_skills(dry_run, verbose)
        && result.changed
    {
        has_changes = true;
    }

    // Ensure git symlinks for Fork compatibility
    if let Err(e) = crate::mdm::ensure_git_symlinks() {
        eprintln!("Warning: Failed to create git symlinks: {}", e);
    }

    // === Git Core Hooks ===
    println!("\n\x1b[1mGit Core Hooks\x1b[0m");
    let core_spinner = Spinner::new("Git: checking core.hooksPath");
    core_spinner.start();
    match install_git_core_hooks(params, dry_run) {
        Ok(Some(diff)) => {
            if dry_run {
                core_spinner.pending("Git: Pending core hook updates");
            } else {
                core_spinner.success("Git: Core hooks configured");
            }
            if verbose {
                println!();
                print_diff(&diff);
            }
            has_changes = true;
            statuses.insert(
                GIT_CORE_HOOKS_STATUS_ID.to_string(),
                InstallStatus::Installed,
            );
            detailed_results.push((
                GIT_CORE_HOOKS_STATUS_ID.to_string(),
                InstallResult::installed(),
            ));
        }
        Ok(None) => {
            core_spinner.success("Git: Core hooks already up to date");
            statuses.insert(
                GIT_CORE_HOOKS_STATUS_ID.to_string(),
                InstallStatus::AlreadyInstalled,
            );
            detailed_results.push((
                GIT_CORE_HOOKS_STATUS_ID.to_string(),
                InstallResult::already_installed(),
            ));
        }
        Err(e) => {
            core_spinner.error("Git: Failed to configure core hooks");
            let error_msg = e.to_string();
            eprintln!("  Error: {}", error_msg);
            statuses.insert(GIT_CORE_HOOKS_STATUS_ID.to_string(), InstallStatus::Failed);
            detailed_results.push((
                GIT_CORE_HOOKS_STATUS_ID.to_string(),
                InstallResult::failed(error_msg),
            ));
        }
    }

    // === Coding Agents ===
    println!("\n\x1b[1mCoding Agents\x1b[0m");

    let installers = get_all_installers();

    for installer in installers {
        let name = installer.name();
        let id = installer.id();

        // Check if tool is installed and hooks status
        match installer.check_hooks(params) {
            Ok(check_result) => {
                if !check_result.tool_installed {
                    statuses.insert(id.to_string(), InstallStatus::NotFound);
                    detailed_results.push((id.to_string(), InstallResult::not_found()));
                    continue;
                }

                any_checked = true;

                // Install/update hooks (only for tools that use config file hooks)
                if installer.uses_config_hooks() {
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
                            detailed_results.push((id.to_string(), InstallResult::installed()));
                        }
                        Ok(None) => {
                            spinner.success(&format!("{}: Hooks already up to date", name));
                            statuses.insert(id.to_string(), InstallStatus::AlreadyInstalled);
                            detailed_results
                                .push((id.to_string(), InstallResult::already_installed()));
                        }
                        Err(e) => {
                            let error_msg = e.to_string();
                            spinner.error(&format!("{}: Failed to update hooks", name));
                            eprintln!("  Error: {}", error_msg);
                            statuses.insert(id.to_string(), InstallStatus::NotFound);
                            detailed_results
                                .push((id.to_string(), InstallResult::failed(error_msg)));
                        }
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
                            } else if result.message.contains("Unable")
                                || result.message.contains("manually")
                            {
                                let extra_spinner = Spinner::new(&result.message);
                                extra_spinner.start();
                                extra_spinner.pending(&result.message);
                            }
                            if verbose && let Some(diff) = result.diff {
                                println!();
                                print_diff(&diff);
                            }

                            // Capture warning-like messages for metrics
                            if (result.message.contains("Unable")
                                || result.message.contains("manually")
                                || result.message.contains("Failed"))
                                && let Some((_, detail)) = detailed_results
                                    .iter_mut()
                                    .find(|(tool_id, _)| tool_id == id)
                            {
                                detail.warnings.push(result.message.clone());
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("  Error installing extras for {}: {}", name, e);
                        // Capture extras error as a warning on the tool's result
                        if let Some((_, detail)) = detailed_results
                            .iter_mut()
                            .find(|(tool_id, _)| tool_id == id)
                        {
                            detail.warnings.push(format!("Extras install error: {}", e));
                        }
                    }
                }
            }
            Err(version_error) => {
                let error_msg = version_error.to_string();
                any_checked = true;
                let spinner = Spinner::new(&format!("{}: checking version", name));
                spinner.start();
                spinner.error(&format!("{}: Version check failed", name));
                eprintln!("  Error: {}", error_msg);
                eprintln!("  Please update {} to continue using git-ai hooks", name);
                statuses.insert(id.to_string(), InstallStatus::NotFound);
                detailed_results.push((id.to_string(), InstallResult::failed(error_msg)));
            }
        }
    }

    if !any_checked {
        println!("No compatible coding agents detected. Nothing to install.");
    }

    // === Git Clients ===
    let git_client_installers = get_all_git_client_installers();
    if !git_client_installers.is_empty() {
        println!("\n\x1b[1mGit Clients\x1b[0m");

        let git_client_params = GitClientInstallerParams {
            git_shim_path: git_shim_path(),
        };

        for installer in git_client_installers {
            let name = installer.name();
            let id = installer.id();

            match installer.check_client(&git_client_params) {
                Ok(check_result) => {
                    if !check_result.client_installed {
                        statuses.insert(id.to_string(), InstallStatus::NotFound);
                        detailed_results.push((id.to_string(), InstallResult::not_found()));
                        continue;
                    }

                    any_checked = true;

                    let spinner = Spinner::new(&format!("{}: checking preferences", name));
                    spinner.start();

                    match installer.install_prefs(&git_client_params, dry_run) {
                        Ok(Some(diff)) => {
                            if dry_run {
                                spinner.pending(&format!("{}: Pending updates", name));
                            } else {
                                spinner.success(&format!("{}: Preferences updated", name));
                            }
                            if verbose {
                                println!();
                                print_diff(&diff);
                            }
                            has_changes = true;
                            statuses.insert(id.to_string(), InstallStatus::Installed);
                            detailed_results.push((id.to_string(), InstallResult::installed()));
                        }
                        Ok(None) => {
                            spinner.success(&format!("{}: Preferences already up to date", name));
                            statuses.insert(id.to_string(), InstallStatus::AlreadyInstalled);
                            detailed_results
                                .push((id.to_string(), InstallResult::already_installed()));
                        }
                        Err(e) => {
                            let error_msg = e.to_string();
                            spinner.error(&format!("{}: Failed to update preferences", name));
                            eprintln!("  Error: {}", error_msg);
                            statuses.insert(id.to_string(), InstallStatus::NotFound);
                            detailed_results
                                .push((id.to_string(), InstallResult::failed(error_msg)));
                        }
                    }
                }
                Err(e) => {
                    let error_msg = e.to_string();
                    any_checked = true;
                    let spinner = Spinner::new(&format!("{}: checking", name));
                    spinner.start();
                    spinner.error(&format!("{}: Check failed", name));
                    eprintln!("  Error: {}", error_msg);
                    statuses.insert(id.to_string(), InstallStatus::NotFound);
                    detailed_results.push((id.to_string(), InstallResult::failed(error_msg)));
                }
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

    // Emit metrics for each agent/git_client result (only if not dry-run)
    if !dry_run {
        emit_install_hooks_metrics(&detailed_results);
    }

    Ok(statuses)
}

/// Emit metrics events for install-hooks results
fn emit_install_hooks_metrics(results: &[(String, InstallResult)]) {
    use crate::metrics::{EventAttributes, InstallHooksValues};

    let attrs = EventAttributes::with_version(env!("CARGO_PKG_VERSION"));

    for (tool_id, result) in results {
        let mut values = InstallHooksValues::new()
            .tool_id(tool_id.clone())
            .status(result.status.as_str().to_string());

        if let Some(msg) = result.message_for_metrics() {
            values = values.message(msg);
        } else {
            values = values.message_null();
        }

        crate::metrics::record(values, attrs.clone());
    }
}

async fn async_run_uninstall(
    params: &HookInstallerParams,
    dry_run: bool,
    verbose: bool,
) -> Result<HashMap<String, InstallStatus>, GitAiError> {
    let mut any_checked = true;
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

    // === Git Core Hooks ===
    println!("\n\x1b[1mGit Core Hooks\x1b[0m");
    let core_spinner = Spinner::new("Git: checking core.hooksPath");
    core_spinner.start();
    match uninstall_git_core_hooks(dry_run) {
        Ok(Some(diff)) => {
            if dry_run {
                core_spinner.pending("Git: Pending core hook removal");
            } else {
                core_spinner.success("Git: Core hooks removed");
            }
            if verbose {
                println!();
                print_diff(&diff);
            }
            has_changes = true;
            statuses.insert(
                GIT_CORE_HOOKS_STATUS_ID.to_string(),
                InstallStatus::Installed,
            );
        }
        Ok(None) => {
            core_spinner.success("Git: Core hooks already removed");
            statuses.insert(
                GIT_CORE_HOOKS_STATUS_ID.to_string(),
                InstallStatus::AlreadyInstalled,
            );
        }
        Err(e) => {
            core_spinner.error("Git: Failed to remove core hooks");
            eprintln!("  Error: {}", e);
            statuses.insert(GIT_CORE_HOOKS_STATUS_ID.to_string(), InstallStatus::Failed);
        }
    }

    // === Coding Agents ===
    println!("\n\x1b[1mCoding Agents\x1b[0m");

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
                            if verbose && let Some(diff) = result.diff {
                                println!();
                                print_diff(&diff);
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

    // === Git Clients ===
    let git_client_installers = get_all_git_client_installers();
    if !git_client_installers.is_empty() {
        println!("\n\x1b[1mGit Clients\x1b[0m");

        let git_client_params = GitClientInstallerParams {
            git_shim_path: git_shim_path(),
        };

        for installer in git_client_installers {
            let name = installer.name();
            let id = installer.id();

            match installer.check_client(&git_client_params) {
                Ok(check_result) => {
                    if !check_result.client_installed {
                        statuses.insert(id.to_string(), InstallStatus::NotFound);
                        continue;
                    }

                    if !check_result.prefs_configured {
                        statuses.insert(id.to_string(), InstallStatus::NotFound);
                        continue;
                    }

                    any_checked = true;

                    let spinner = Spinner::new(&format!("{}: removing preferences", name));
                    spinner.start();

                    match installer.uninstall_prefs(&git_client_params, dry_run) {
                        Ok(Some(diff)) => {
                            if dry_run {
                                spinner.pending(&format!("{}: Pending removal", name));
                            } else {
                                spinner.success(&format!("{}: Preferences removed", name));
                            }
                            if verbose {
                                println!();
                                print_diff(&diff);
                            }
                            has_changes = true;
                            statuses.insert(id.to_string(), InstallStatus::Installed);
                        }
                        Ok(None) => {
                            spinner.success(&format!("{}: No preferences to remove", name));
                            statuses.insert(id.to_string(), InstallStatus::AlreadyInstalled);
                        }
                        Err(e) => {
                            spinner.error(&format!("{}: Failed to remove preferences", name));
                            eprintln!("  Error: {}", e);
                            statuses.insert(id.to_string(), InstallStatus::NotFound);
                        }
                    }
                }
                Err(e) => {
                    any_checked = true;
                    let spinner = Spinner::new(&format!("{}: checking", name));
                    spinner.start();
                    spinner.error(&format!("{}: Check failed", name));
                    eprintln!("  Error: {}", e);
                    statuses.insert(id.to_string(), InstallStatus::NotFound);
                }
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

fn install_git_core_hooks(
    params: &HookInstallerParams,
    dry_run: bool,
) -> Result<Option<String>, GitAiError> {
    let hooks_dir = managed_core_hooks_dir()?;
    let desired_hooks_path = hooks_dir.to_string_lossy().to_string();
    let current_hooks_path = git_config_get_global("core.hooksPath")?;
    let scripts_up_to_date = core_hook_scripts_up_to_date(&hooks_dir, &params.binary_path);

    let config_needs_update = current_hooks_path.as_deref() != Some(desired_hooks_path.as_str());
    if !config_needs_update && scripts_up_to_date {
        return Ok(None);
    }

    if !dry_run {
        fs::create_dir_all(&hooks_dir)?;

        // Preserve the user's pre-install core.hooksPath once so uninstall can restore it.
        if config_needs_update {
            let previous_path_file = hooks_dir.join(PREVIOUS_HOOKS_PATH_FILE);
            if !previous_path_file.exists() {
                write_previous_hooks_path(&previous_path_file, current_hooks_path.as_deref())?;
            }
        }

        write_core_hook_scripts(&hooks_dir, &params.binary_path)?;

        if config_needs_update {
            git_config_set_global("core.hooksPath", &desired_hooks_path)?;
        }
    }

    let mut diff = String::new();
    if config_needs_update {
        diff.push_str(&format_config_change_diff(
            current_hooks_path.as_deref(),
            Some(desired_hooks_path.as_str()),
        ));
    }
    if !scripts_up_to_date {
        if !diff.is_empty() {
            diff.push('\n');
        }
        diff.push_str(&format_core_hook_scripts_diff(&hooks_dir, true));
    }

    Ok(Some(diff))
}

fn uninstall_git_core_hooks(dry_run: bool) -> Result<Option<String>, GitAiError> {
    let hooks_dir = managed_core_hooks_dir()?;
    let managed_hooks_path = hooks_dir.to_string_lossy().to_string();
    let current_hooks_path = git_config_get_global("core.hooksPath")?;
    let previous_path_file = hooks_dir.join(PREVIOUS_HOOKS_PATH_FILE);
    let previous_hooks_path = read_previous_hooks_path(&previous_path_file)?;

    let config_points_to_managed =
        current_hooks_path.as_deref() == Some(managed_hooks_path.as_str());
    let hooks_dir_exists = hooks_dir.exists();

    if !config_points_to_managed && !hooks_dir_exists {
        return Ok(None);
    }

    let mut diff = String::new();
    if config_points_to_managed {
        diff.push_str(&format_config_change_diff(
            current_hooks_path.as_deref(),
            previous_hooks_path.as_deref(),
        ));
    }
    if hooks_dir_exists {
        if !diff.is_empty() {
            diff.push('\n');
        }
        diff.push_str(&format_core_hook_scripts_diff(&hooks_dir, false));
    }

    if !dry_run {
        if config_points_to_managed {
            if let Some(previous_hooks_path) = previous_hooks_path {
                git_config_set_global("core.hooksPath", &previous_hooks_path)?;
            } else {
                let _ = git_config_unset_global("core.hooksPath");
            }
        }

        if hooks_dir_exists {
            fs::remove_dir_all(&hooks_dir)?;
        }
    }

    Ok(Some(diff))
}

fn core_hook_scripts_up_to_date(hooks_dir: &Path, binary_path: &Path) -> bool {
    if !hooks_dir.exists() {
        return false;
    }

    let binary = binary_path.to_string_lossy().replace('\\', "/");
    INSTALLED_HOOKS.iter().all(|hook| {
        let hook_path = hooks_dir.join(hook);
        let content = fs::read_to_string(&hook_path).unwrap_or_default();
        hook_path.exists()
            && content.contains(&format!("hook {}", hook))
            && content.contains(&binary)
    })
}

fn git_config_get_global(key: &str) -> Result<Option<String>, GitAiError> {
    let args = vec![
        "config".to_string(),
        "--global".to_string(),
        "--get".to_string(),
        key.to_string(),
    ];
    let output = run_git_command(&args)?;

    if output.status.success() {
        let value = String::from_utf8(output.stdout)?.trim().to_string();
        if value.is_empty() {
            Ok(None)
        } else {
            Ok(Some(value))
        }
    } else if output.status.code() == Some(1) {
        Ok(None)
    } else {
        Err(git_cli_error(args, output))
    }
}

fn git_config_set_global(key: &str, value: &str) -> Result<(), GitAiError> {
    let args = vec![
        "config".to_string(),
        "--global".to_string(),
        key.to_string(),
        value.to_string(),
    ];
    let output = run_git_command(&args)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(git_cli_error(args, output))
    }
}

fn git_config_unset_global(key: &str) -> Result<(), GitAiError> {
    let args = vec![
        "config".to_string(),
        "--global".to_string(),
        "--unset".to_string(),
        key.to_string(),
    ];
    let output = run_git_command(&args)?;
    if output.status.success() || output.status.code() == Some(5) {
        // Exit 5 means "not found", which is effectively already unset.
        Ok(())
    } else {
        Err(git_cli_error(args, output))
    }
}

fn run_git_command(args: &[String]) -> Result<Output, GitAiError> {
    let output = Command::new(crate::config::Config::get().git_cmd())
        .args(args)
        .env(GIT_AI_SKIP_CORE_HOOKS_ENV, "1")
        .output()?;
    Ok(output)
}

fn git_cli_error(args: Vec<String>, output: Output) -> GitAiError {
    GitAiError::GitCliError {
        code: output.status.code(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        args,
    }
}

fn format_config_change_diff(previous: Option<&str>, next: Option<&str>) -> String {
    let before = previous.unwrap_or("<unset>");
    let after = next.unwrap_or("<unset>");
    format!(
        "--- git config --global core.hooksPath\n+++ git config --global core.hooksPath\n-{}\n+{}\n",
        before, after
    )
}

fn format_core_hook_scripts_diff(hooks_dir: &Path, installing: bool) -> String {
    let mut diff = format!("--- {}\n+++ {}\n", hooks_dir.display(), hooks_dir.display());
    for hook in INSTALLED_HOOKS {
        let sign = if installing { '+' } else { '-' };
        diff.push_str(&format!("{}{}\n", sign, hooks_dir.join(hook).display()));
    }
    diff
}

fn write_previous_hooks_path(
    path: &Path,
    previous_hooks_path: Option<&str>,
) -> Result<(), GitAiError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, previous_hooks_path.unwrap_or_default())?;
    Ok(())
}

fn read_previous_hooks_path(path: &Path) -> Result<Option<String>, GitAiError> {
    if !path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(path)?;
    let value = content.trim().to_string();
    if value.is_empty() {
        Ok(None)
    } else {
        Ok(Some(value))
    }
}
