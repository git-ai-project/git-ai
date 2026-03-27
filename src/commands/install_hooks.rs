use crate::commands::flush_metrics_db::spawn_background_metrics_db_flush;
use crate::config;
use crate::daemon::DaemonConfig;
use crate::error::GitAiError;
use crate::mdm::agents::get_all_installers;
use crate::mdm::git_client_installer::GitClientInstallerParams;
use crate::mdm::git_clients::get_all_git_client_installers;
use crate::mdm::hook_installer::HookInstallerParams;
use crate::mdm::skills_installer;
use crate::mdm::spinner::{Spinner, print_diff};
use crate::mdm::utils::{get_current_binary_path, git_shim_path};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const TRACE2_EVENT_TARGET_KEY: &str = "trace2.eventTarget";
const TRACE2_EVENT_NESTING_KEY: &str = "trace2.eventNesting";
const TRACE2_EVENT_NESTING_VALUE: &str = "10";

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

fn print_amp_plugins_note(installer_id: &str) {
    if installer_id == "amp" {
        println!("  Note: Amp plugins are experimental. Run amp with `PLUGINS=all amp`.");
    }
}

fn set_global_git_config_value(git_cmd: &str, key: &str, value: &str) -> Result<(), GitAiError> {
    let status = Command::new(git_cmd)
        .args(["config", "--global", key, value])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(GitAiError::Generic(format!(
            "failed to set global git config key '{}'",
            key
        )))
    }
}

fn ensure_global_git_config_dirs() -> Result<(), GitAiError> {
    if let Ok(path) = std::env::var("GIT_CONFIG_GLOBAL") {
        let config_path = PathBuf::from(path);
        if let Some(parent) = config_path.parent() {
            fs::create_dir_all(parent)?;
        }
    }

    if let Ok(home) = std::env::var("HOME") {
        fs::create_dir_all(home)?;
    }

    Ok(())
}

fn remove_global_git_config_section(git_cmd: &str, section: &str) -> Result<(), GitAiError> {
    let status = Command::new(git_cmd)
        .args(["config", "--global", "--remove-section", section])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    // Exit code 128 means the section doesn't exist, which is fine.
    if status.success() || status.code() == Some(128) {
        Ok(())
    } else {
        Err(GitAiError::Generic(format!(
            "failed to remove global git config section '{}'",
            section
        )))
    }
}

fn maybe_configure_async_mode_daemon_trace2(dry_run: bool) -> Result<(), GitAiError> {
    let runtime_config = config::Config::fresh();

    if !runtime_config.feature_flags().async_mode {
        if !dry_run {
            // Async mode is off — clean up any trace2 config we previously wrote.
            let _ = remove_global_git_config_section(runtime_config.git_cmd(), "trace2");
        }
        return Ok(());
    }

    ensure_global_git_config_dirs()?;

    let daemon_config = DaemonConfig::from_env_or_default_paths()?;
    let event_target = daemon_config.trace2_event_target();

    if dry_run {
        return Ok(());
    }

    // Fully reset any existing trace2 config the user may have set
    // (e.g. trace2.normalTarget, trace2.perfTarget, trace2.configParams, etc.)
    // before writing only the keys we need.
    remove_global_git_config_section(runtime_config.git_cmd(), "trace2")?;

    set_global_git_config_value(
        runtime_config.git_cmd(),
        TRACE2_EVENT_TARGET_KEY,
        &event_target,
    )?;
    set_global_git_config_value(
        runtime_config.git_cmd(),
        TRACE2_EVENT_NESTING_KEY,
        TRACE2_EVENT_NESTING_VALUE,
    )?;
    Ok(())
}

fn maybe_teardown_async_mode(dry_run: bool) {
    if dry_run {
        return;
    }

    let runtime_config = config::Config::fresh();
    if runtime_config.feature_flags().async_mode {
        return;
    }

    // Don't touch daemon inside test harnesses
    if std::env::var_os("GIT_AI_TEST_DB_PATH").is_some()
        || std::env::var_os("GITAI_TEST_DB_PATH").is_some()
    {
        return;
    }

    // Shut down any leftover daemon from when async_mode was enabled.
    // Uses stop_daemon which tries soft shutdown then escalates to hard kill.
    if let Ok(daemon_config) = DaemonConfig::from_env_or_default_paths() {
        let _ =
            crate::commands::daemon::stop_daemon(&daemon_config, std::time::Duration::from_secs(5));
    }
}

fn maybe_ensure_daemon(dry_run: bool) {
    if dry_run {
        return;
    }

    let runtime_config = config::Config::fresh();
    if !runtime_config.feature_flags().async_mode {
        return;
    }

    // Don't touch daemon inside test harnesses
    if std::env::var_os("GIT_AI_TEST_DB_PATH").is_some()
        || std::env::var_os("GITAI_TEST_DB_PATH").is_some()
    {
        return;
    }

    let Ok(daemon_config) = DaemonConfig::from_env_or_default_paths() else {
        return;
    };

    // Restart daemon so it picks up the freshly-written trace2 config.
    // Uses soft shutdown → hard kill escalation if needed.
    if let Err(e) = crate::commands::daemon::restart_daemon(&daemon_config) {
        eprintln!(
            "[git-ai] warning: failed to restart background service: {}",
            e
        );
    }
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

    // In async mode, daemon trace2 config must be in place before any install work starts.
    // If async mode was disabled, tear down any leftover daemon and trace2 config.
    maybe_configure_async_mode_daemon_trace2(dry_run)?;
    maybe_teardown_async_mode(dry_run);
    maybe_ensure_daemon(dry_run);

    // Now that the daemon is (re)started, initialize the telemetry handle so
    // that install-hooks metrics and observability events route through it.
    if config::Config::get().feature_flags().async_mode && !dry_run {
        let _ = crate::daemon::telemetry_handle::init_daemon_telemetry_handle();
    }

    // Get absolute path to the current binary
    let binary_path = get_current_binary_path()?;
    persist_install_api_base_config(&binary_path, dry_run)?;
    let params = HookInstallerParams { binary_path };

    // Run async operations with smol and convert result
    let statuses = smol::block_on(async_run_install(&params, dry_run, verbose))?;

    // Spawn background processes to flush metrics
    crate::observability::spawn_background_flush();
    spawn_background_metrics_db_flush();

    Ok(to_hashmap(statuses))
}

fn persist_install_api_base_config(binary_path: &Path, dry_run: bool) -> Result<bool, GitAiError> {
    if dry_run {
        return Ok(false);
    }

    let api_base = std::env::var("API_BASE").ok().filter(|s| !s.is_empty());
    let Some(api_base) = api_base else {
        return Ok(false);
    };

    let mut file_config = crate::config::load_file_config_public().map_err(GitAiError::Generic)?;
    let mut changed = false;

    if file_config.api_base_url.as_deref() != Some(api_base.as_str()) {
        file_config.api_base_url = Some(api_base);
        changed = true;
    }

    let git_path_missing = file_config
        .git_path
        .as_ref()
        .map(|value| value.trim().is_empty())
        .unwrap_or(true);
    if git_path_missing && let Some(git_path) = detect_install_git_path(binary_path) {
        file_config.git_path = Some(git_path);
        changed = true;
    }

    if !changed {
        return Ok(false);
    }

    crate::config::save_file_config(&file_config).map_err(GitAiError::Generic)?;
    Ok(true)
}

fn detect_install_git_path(binary_path: &Path) -> Option<String> {
    let install_dir = binary_path.parent()?;

    #[cfg(windows)]
    {
        parse_git_og_cmd_path(&fs::read_to_string(install_dir.join("git-og.cmd")).ok()?)
    }

    #[cfg(not(windows))]
    {
        let target = fs::read_link(install_dir.join("git-og")).ok()?;
        let resolved = if target.is_absolute() {
            target
        } else {
            install_dir.join(target)
        };
        Some(resolved.to_string_lossy().to_string())
    }
}

#[cfg(windows)]
fn parse_git_og_cmd_path(contents: &str) -> Option<String> {
    contents.lines().find_map(|line| {
        let start = line.find('"')?;
        let rest = &line[start + 1..];
        let end = rest.find('"')?;
        Some(rest[..end].to_string())
    })
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
                                print_amp_plugins_note(id);
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
                            print_amp_plugins_note(id);
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

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::path::PathBuf;
    use tempfile::tempdir;

    struct EnvVarGuard {
        key: &'static str,
        old: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let old = std::env::var(key).ok();
            // SAFETY: tests marked `serial` avoid concurrent env mutation.
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, old }
        }

        fn remove(key: &'static str) -> Self {
            let old = std::env::var(key).ok();
            // SAFETY: tests marked `serial` avoid concurrent env mutation.
            unsafe {
                std::env::remove_var(key);
            }
            Self { key, old }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            // SAFETY: tests marked `serial` avoid concurrent env mutation.
            unsafe {
                if let Some(old) = &self.old {
                    std::env::set_var(self.key, old);
                } else {
                    std::env::remove_var(self.key);
                }
            }
        }
    }

    fn test_binary_path(install_dir: &Path) -> PathBuf {
        #[cfg(windows)]
        {
            install_dir.join("git-ai.exe")
        }

        #[cfg(not(windows))]
        {
            install_dir.join("git-ai")
        }
    }

    fn write_install_git_marker(install_dir: &Path, git_path: &str) {
        #[cfg(windows)]
        {
            fs::write(
                install_dir.join("git-og.cmd"),
                format!("@echo off\r\n\"{}\" %*\r\n", git_path),
            )
            .unwrap();
        }

        #[cfg(not(windows))]
        {
            std::os::unix::fs::symlink(git_path, install_dir.join("git-og")).unwrap();
        }
    }

    #[test]
    #[serial]
    fn persist_install_api_base_updates_config_and_backfills_git_path() {
        let temp = tempdir().unwrap();
        let install_dir = temp.path().join("bin");
        fs::create_dir_all(&install_dir).unwrap();
        fs::write(test_binary_path(&install_dir), "").unwrap();

        let expected_git_path = if cfg!(windows) {
            r"C:\Program Files\Git\bin\git.exe"
        } else {
            "/opt/custom/bin/git"
        };
        write_install_git_marker(&install_dir, expected_git_path);

        let _home = EnvVarGuard::set("HOME", temp.path().to_str().unwrap());
        #[cfg(windows)]
        let _userprofile = EnvVarGuard::set("USERPROFILE", temp.path().to_str().unwrap());
        let _api_base = EnvVarGuard::set("API_BASE", "https://enterprise.example");

        let changed =
            persist_install_api_base_config(&test_binary_path(&install_dir), false).unwrap();

        assert!(changed);

        let config = crate::config::load_file_config_public().unwrap();
        assert_eq!(
            config.api_base_url.as_deref(),
            Some("https://enterprise.example")
        );
        assert_eq!(config.git_path.as_deref(), Some(expected_git_path));
    }

    #[test]
    #[serial]
    fn persist_install_api_base_preserves_existing_git_path() {
        let temp = tempdir().unwrap();
        let install_dir = temp.path().join("bin");
        fs::create_dir_all(&install_dir).unwrap();
        fs::write(test_binary_path(&install_dir), "").unwrap();
        write_install_git_marker(
            &install_dir,
            if cfg!(windows) {
                r"C:\Program Files\Git\bin\git.exe"
            } else {
                "/opt/custom/bin/git"
            },
        );

        let _home = EnvVarGuard::set("HOME", temp.path().to_str().unwrap());
        #[cfg(windows)]
        let _userprofile = EnvVarGuard::set("USERPROFILE", temp.path().to_str().unwrap());
        let _api_base = EnvVarGuard::set("API_BASE", "https://enterprise.example");

        let existing_git_path = if cfg!(windows) {
            r"D:\PortableGit\bin\git.exe"
        } else {
            "/usr/local/bin/git"
        };
        crate::config::save_file_config(&crate::config::FileConfig {
            git_path: Some(existing_git_path.to_string()),
            ..Default::default()
        })
        .unwrap();

        persist_install_api_base_config(&test_binary_path(&install_dir), false).unwrap();

        let config = crate::config::load_file_config_public().unwrap();
        assert_eq!(
            config.api_base_url.as_deref(),
            Some("https://enterprise.example")
        );
        assert_eq!(config.git_path.as_deref(), Some(existing_git_path));
    }

    #[test]
    #[serial]
    fn persist_install_api_base_skips_without_env_or_in_dry_run() {
        let temp = tempdir().unwrap();
        let install_dir = temp.path().join("bin");
        fs::create_dir_all(&install_dir).unwrap();
        fs::write(test_binary_path(&install_dir), "").unwrap();

        let _home = EnvVarGuard::set("HOME", temp.path().to_str().unwrap());
        #[cfg(windows)]
        let _userprofile = EnvVarGuard::set("USERPROFILE", temp.path().to_str().unwrap());
        let _api_base = EnvVarGuard::remove("API_BASE");

        let changed =
            persist_install_api_base_config(&test_binary_path(&install_dir), false).unwrap();
        assert!(!changed);
        assert!(!temp.path().join(".git-ai").join("config.json").exists());

        let _api_base = EnvVarGuard::set("API_BASE", "https://enterprise.example");
        let changed =
            persist_install_api_base_config(&test_binary_path(&install_dir), true).unwrap();
        assert!(!changed);
        assert!(!temp.path().join(".git-ai").join("config.json").exists());
    }

    #[cfg(windows)]
    #[test]
    fn parse_git_og_cmd_path_extracts_wrapped_git_path() {
        assert_eq!(
            parse_git_og_cmd_path("@echo off\r\n\"C:\\Program Files\\Git\\bin\\git.exe\" %*\r\n"),
            Some("C:\\Program Files\\Git\\bin\\git.exe".to_string())
        );
    }
}
