use crate::config::{self, UpdateChannel};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::io::IsTerminal;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

const UPDATE_CHECK_INTERVAL_HOURS: u64 = 24;
const INSTALL_SCRIPT_URL: &str =
    "https://raw.githubusercontent.com/acunniffe/git-ai/main/install.sh";
#[cfg(windows)]
const INSTALL_SCRIPT_PS1_URL: &str =
    "https://raw.githubusercontent.com/acunniffe/git-ai/main/install.ps1";
const RELEASES_API_URL: &str = "https://usegitai.com/api/releases";
const GIT_AI_RELEASE_ENV: &str = "GIT_AI_RELEASE_TAG";
const BACKGROUND_SPAWN_THROTTLE_SECS: u64 = 60;

static UPDATE_NOTICE_EMITTED: AtomicBool = AtomicBool::new(false);
static LAST_BACKGROUND_SPAWN: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, PartialEq)]
enum UpgradeAction {
    UpgradeAvailable,
    AlreadyLatest,
    RunningNewerVersion,
    ForceReinstall,
}

#[derive(Debug, Clone)]
struct ChannelRelease {
    tag: String,
    semver: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UpdateCache {
    last_checked_at: u64,
    available_tag: Option<String>,
    available_semver: Option<String>,
    channel: String,
}

impl UpdateCache {
    fn new(channel: UpdateChannel) -> Self {
        Self {
            last_checked_at: 0,
            available_tag: None,
            available_semver: None,
            channel: channel.as_str().to_string(),
        }
    }

    fn update_available(&self) -> bool {
        self.available_semver.is_some()
    }

    fn matches_channel(&self, channel: UpdateChannel) -> bool {
        self.channel == channel.as_str()
    }
}

#[derive(Debug, Deserialize)]
struct ReleasesResponse {
    latest: String,
    next: String,
}

fn get_update_check_cache_path() -> Option<PathBuf> {
    #[cfg(test)]
    {
        if let Ok(test_cache_dir) = std::env::var("GIT_AI_TEST_CACHE_DIR") {
            return Some(PathBuf::from(test_cache_dir).join(".update_check"));
        }
    }

    dirs::home_dir().map(|home| home.join(".git-ai").join(".update_check"))
}

fn read_update_cache() -> Option<UpdateCache> {
    let path = get_update_check_cache_path()?;
    let bytes = fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn write_update_cache(cache: &UpdateCache) {
    if let Some(path) = get_update_check_cache_path() {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_vec(cache) {
            let _ = fs::write(path, json);
        }
    }
}

fn current_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| Duration::from_secs(0))
        .as_secs()
}

fn should_check_for_updates(channel: UpdateChannel, cache: Option<&UpdateCache>) -> bool {
    let now = current_timestamp();
    match cache {
        Some(cache) if cache.last_checked_at > 0 => {
            // If cache doesn't match the channel, we should check for updates
            if !cache.matches_channel(channel) {
                return true;
            }
            let elapsed = now.saturating_sub(cache.last_checked_at);
            elapsed > UPDATE_CHECK_INTERVAL_HOURS * 3600
        }
        _ => true,
    }
}

fn semver_from_tag(tag: &str) -> String {
    let trimmed = tag.trim().trim_start_matches('v');
    trimmed
        .split(|c| c == '-' || c == '+')
        .next()
        .unwrap_or("")
        .to_string()
}

fn determine_action(force: bool, release: &ChannelRelease, current_version: &str) -> UpgradeAction {
    if force {
        return UpgradeAction::ForceReinstall;
    }

    if release.semver == current_version {
        UpgradeAction::AlreadyLatest
    } else if is_newer_version(&release.semver, current_version) {
        UpgradeAction::UpgradeAvailable
    } else {
        UpgradeAction::RunningNewerVersion
    }
}

fn persist_update_state(channel: UpdateChannel, release: Option<&ChannelRelease>) {
    let mut cache = UpdateCache::new(channel);
    cache.last_checked_at = current_timestamp();
    if let Some(release) = release {
        cache.available_tag = Some(release.tag.clone());
        cache.available_semver = Some(release.semver.clone());
    }
    write_update_cache(&cache);
}

fn releases_endpoint(base: Option<&str>) -> String {
    base.map(|b| format!("{}/releases", b.trim_end_matches('/')))
        .unwrap_or_else(|| RELEASES_API_URL.to_string())
}

fn fetch_release_for_channel(
    api_base_url: Option<&str>,
    channel: UpdateChannel,
) -> Result<ChannelRelease, String> {
    #[cfg(test)]
    if let Some(result) = try_mock_releases(api_base_url, channel) {
        return result;
    }

    let current_version = env!("CARGO_PKG_VERSION");
    let url = releases_endpoint(api_base_url);
    let response = minreq::get(&url)
        .with_header("User-Agent", format!("git-ai/{}", current_version))
        .with_timeout(5)
        .send()
        .map_err(|e| format!("Failed to check for updates: {}", e))?;

    let body = response.as_str()
        .map_err(|e| format!("Failed to read response body: {}", e))?;
    let releases: ReleasesResponse = serde_json::from_str(body)
        .map_err(|e| format!("Failed to parse release response: {}", e))?;

    release_from_response(releases, channel)
}

fn release_from_response(
    releases: ReleasesResponse,
    channel: UpdateChannel,
) -> Result<ChannelRelease, String> {
    let tag_raw = match channel {
        UpdateChannel::Latest => releases.latest,
        UpdateChannel::Next => releases.next,
    };

    let tag = tag_raw.trim().to_string();
    if tag.is_empty() {
        return Err("Release tag not found in response".to_string());
    }

    let semver = semver_from_tag(&tag);
    if semver.is_empty() {
        return Err(format!("Unable to parse semver from tag '{}'", tag));
    }

    Ok(ChannelRelease { tag, semver })
}

#[cfg(test)]
fn try_mock_releases(
    api_base_url: Option<&str>,
    channel: UpdateChannel,
) -> Option<Result<ChannelRelease, String>> {
    let base = api_base_url?;
    let json = base.strip_prefix("mock://")?;
    Some(
        serde_json::from_str::<ReleasesResponse>(json)
            .map_err(|e| format!("Invalid mock releases payload: {}", e))
            .and_then(|releases| release_from_response(releases, channel)),
    )
}

fn run_install_script_for_tag(tag: &str, silent: bool) -> Result<(), String> {
    #[cfg(windows)]
    {
        // On Windows, we need to run the installer detached because the current git-ai
        // binary and shims are in use and need to be replaced. The installer will wait
        // for the files to be released before proceeding.
        let pid = std::process::id();
        let log_dir = dirs::home_dir()
            .ok_or_else(|| "Could not determine home directory".to_string())?
            .join(".git-ai")
            .join("upgrade-logs");
        
        // Ensure the log directory exists
        fs::create_dir_all(&log_dir)
            .map_err(|e| format!("Failed to create log directory: {}", e))?;
        
        let log_file = log_dir.join(format!("upgrade-{}.log", pid));
        let log_path_str = log_file.to_string_lossy().to_string();
        
        // Create an empty log file to ensure it exists
        fs::write(&log_file, format!("Starting upgrade at PID {}\n", pid))
            .map_err(|e| format!("Failed to create log file: {}", e))?;
        
        // PowerShell script that handles its own logging
        // The script captures all output using Start-Transcript
        let ps_script = format!(
            "$logFile = '{}'; \
             Start-Transcript -Path $logFile -Append -Force | Out-Null; \
             Write-Host 'Fetching install script from {}'; \
             try {{ \
                 $ErrorActionPreference = 'Continue'; \
                 $script = Invoke-RestMethod -Uri '{}' -UseBasicParsing; \
                 Write-Host 'Running install script...'; \
                 Invoke-Expression $script; \
                 Write-Host 'Install script completed'; \
             }} catch {{ \
                 Write-Host \"Error: $_\"; \
                 Write-Host \"Stack trace: $($_.ScriptStackTrace)\"; \
             }} finally {{ \
                 Stop-Transcript | Out-Null; \
             }}",
            log_path_str,
            INSTALL_SCRIPT_PS1_URL,
            INSTALL_SCRIPT_PS1_URL
        );
        
        let mut cmd = Command::new("powershell");
        cmd.arg("-NoProfile")
           .arg("-ExecutionPolicy")
           .arg("Bypass")
           .arg("-Command")
           .arg(&ps_script)
           .env(GIT_AI_RELEASE_ENV, tag);

        // Hide the spawned console to prevent any host/UI bleed-through
        cmd.creation_flags(CREATE_NO_WINDOW);

        if silent {
            cmd.stdout(Stdio::null()).stderr(Stdio::null());
        }

        match cmd.spawn() {
            Ok(_) => {
                if !silent {
                    println!("\x1b[1;33mNote: The installation is running in the background on Windows.\x1b[0m");
                    println!("This allows the current git-ai process to exit and release file locks.");
                    println!("Check the log file for progress: {}", log_path_str);
                    println!("The upgrade should complete shortly as long as there are no long-running git or git-ai processes in the background.");
                }
                Ok(())
            }
            Err(e) => Err(format!("Failed to run installation script: {}", e)),
        }
    }

    #[cfg(not(windows))]
    {
        let mut cmd = Command::new("bash");
        cmd.arg("-c")
            .arg(format!("curl -fsSL {} | bash", INSTALL_SCRIPT_URL))
            .env(GIT_AI_RELEASE_ENV, tag);

        if silent {
            cmd.stdout(Stdio::null()).stderr(Stdio::null());
        }

        match cmd.status() {
            Ok(status) => {
                if status.success() {
                    Ok(())
                } else {
                    Err(format!(
                        "Installation script failed with exit code: {:?}",
                        status.code()
                    ))
                }
            }
            Err(e) => {
                Err(format!("Failed to run installation script: {}", e))
            }
        }
    }
}

pub fn run_with_args(args: &[String]) {
    let mut force = false;
    let mut background = false;

    for arg in args {
        match arg.as_str() {
            "--force" => force = true,
            "--background" => background = true, // Undocumented flag for internal use when spawning background process
            _ => {
                eprintln!("Unknown argument: {}", arg);
                eprintln!("Usage: git-ai upgrade [--force]");
                std::process::exit(1);
            }
        }
    }

    run_impl(force, background);
}

fn run_impl(force: bool, background: bool) {
    let config = config::Config::get();
    let channel = config.update_channel();
    let skip_install = background && config.auto_updates_disabled();
    let _ = run_impl_with_url(force, None, channel, skip_install);
}

fn run_impl_with_url(
    force: bool,
    api_base_url: Option<&str>,
    channel: UpdateChannel,
    skip_install: bool,
) -> UpgradeAction {
    let current_version = env!("CARGO_PKG_VERSION");

    println!("Checking for updates (channel: {})...", channel.as_str());

    let release = match fetch_release_for_channel(api_base_url, channel) {
        Ok(release) => release,
        Err(err) => {
            eprintln!("{}", err);
            std::process::exit(1);
        }
    };

    println!("Current version: v{}", current_version);
    println!(
        "Available {} version: v{} (tag {})",
        channel.as_str(),
        release.semver,
        release.tag
    );
    println!();

    let action = determine_action(force, &release, current_version);
    let cache_release = matches!(action, UpgradeAction::UpgradeAvailable);
    persist_update_state(channel, cache_release.then_some(&release));

    match action {
        UpgradeAction::AlreadyLatest => {
            println!("You are already on the latest version!");
            println!();
            println!("To reinstall anyway, run:");
            println!("  \x1b[1;36mgit-ai upgrade --force\x1b[0m");
            return action;
        }
        UpgradeAction::RunningNewerVersion => {
            println!("You are running a newer version than the selected release channel.");
            println!("(This usually means you're running a development build)");
            println!();
            println!("To reinstall the selected release anyway, run:");
            println!("  \x1b[1;36mgit-ai upgrade --force\x1b[0m");
            return action;
        }
        UpgradeAction::ForceReinstall => {
            println!(
                "\x1b[1;33mForce mode enabled - reinstalling {}\x1b[0m",
                release.tag
            );
        }
        UpgradeAction::UpgradeAvailable => {
            println!("\x1b[1;33mA new version is available!\x1b[0m");
        }
    }
    println!();

    if api_base_url.is_some() || skip_install {
        return action;
    }

    println!("Running installation script...");
    println!();

    match run_install_script_for_tag(&release.tag, false) {
        Ok(()) => {
            // On Windows, we spawn the installer in the background and can't verify success
            #[cfg(not(windows))]
            {
                println!("\x1b[1;32m✓\x1b[0m Successfully installed {}!", release.tag);
            }
        }
        Err(err) => {
            eprintln!("{}", err);
            std::process::exit(1);
        }
    }

    action
}

fn print_cached_notice(cache: &UpdateCache) {
    if cache.available_semver.is_none() || cache.available_tag.is_none() {
        return;
    }

    if !std::io::stdout().is_terminal() {
        // Don't print the version check notice if stdout is not a terminal/interactive shell
        return;
    }

    if UPDATE_NOTICE_EMITTED
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return;
    }

    let current_version = env!("CARGO_PKG_VERSION");
    let available_version = cache.available_semver.as_deref().unwrap_or("");

    eprintln!();
    eprintln!(
        "\x1b[1;33mA new version of git-ai is available: \x1b[1;32mv{}\x1b[0m → \x1b[1;32mv{}\x1b[0m",
        current_version, available_version
    );
    eprintln!(
        "\x1b[1;33mRun \x1b[1;36mgit-ai upgrade\x1b[0m \x1b[1;33mto upgrade to the latest version.\x1b[0m"
    );
    eprintln!();
}

pub fn maybe_schedule_background_update_check() {
    let config = config::Config::get();
    if config.version_checks_disabled() {
        return;
    }

    let channel = config.update_channel();
    let cache = read_update_cache();

    if config.auto_updates_disabled() {
        if let Some(cache) = cache.as_ref() {
            if cache.matches_channel(channel) && cache.update_available() {
                print_cached_notice(cache);
            }
        }
    }

    if !should_check_for_updates(channel, cache.as_ref()) {
        return;
    }

    let now = current_timestamp();
    let last_spawn = LAST_BACKGROUND_SPAWN.load(Ordering::SeqCst);
    if now.saturating_sub(last_spawn) < BACKGROUND_SPAWN_THROTTLE_SECS {
        return;
    }

    if spawn_background_upgrade_process() {
        LAST_BACKGROUND_SPAWN.store(now, Ordering::SeqCst);
    }
}

fn spawn_background_upgrade_process() -> bool {
    match crate::utils::current_git_ai_exe() {
        Ok(exe) => {
            let mut cmd = Command::new(exe);
            cmd.arg("upgrade")
                .arg("--background")
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            cmd.spawn().is_ok()
        }
        Err(_) => false,
    }
}

fn is_newer_version(latest: &str, current: &str) -> bool {
    let parse_version =
        |v: &str| -> Vec<u32> { v.split('.').filter_map(|s| s.parse::<u32>().ok()).collect() };

    let latest_parts = parse_version(latest);
    let current_parts = parse_version(current);

    for i in 0..latest_parts.len().max(current_parts.len()) {
        let latest_part = latest_parts.get(i).copied().unwrap_or(0);
        let current_part = current_parts.get(i).copied().unwrap_or(0);

        if latest_part > current_part {
            return true;
        } else if latest_part < current_part {
            return false;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set_test_cache_dir(dir: &tempfile::TempDir) {
        unsafe {
            std::env::set_var("GIT_AI_TEST_CACHE_DIR", dir.path());
        }
    }

    fn clear_test_cache_dir() {
        unsafe {
            std::env::remove_var("GIT_AI_TEST_CACHE_DIR");
        }
    }

    #[test]
    fn test_is_newer_version() {
        assert!(!is_newer_version("1.0.0", "1.0.0"));
        assert!(!is_newer_version("1.0.10", "1.0.10"));

        assert!(is_newer_version("1.0.1", "1.0.0"));
        assert!(is_newer_version("1.0.11", "1.0.10"));
        assert!(!is_newer_version("1.0.0", "1.0.1"));
        assert!(!is_newer_version("1.0.10", "1.0.11"));

        assert!(is_newer_version("1.1.0", "1.0.0"));
        assert!(!is_newer_version("1.0.0", "1.1.0"));

        assert!(is_newer_version("2.0.0", "1.0.0"));
        assert!(is_newer_version("2.0.0", "1.9.9"));
        assert!(!is_newer_version("1.9.9", "2.0.0"));

        assert!(is_newer_version("1.0.0.1", "1.0.0"));
        assert!(!is_newer_version("1.0.0", "1.0.0.1"));

        assert!(is_newer_version("1.10.0", "1.9.0"));
        assert!(is_newer_version("1.0.100", "1.0.99"));
        assert!(is_newer_version("100.200.300", "100.200.299"));
    }

    #[test]
    fn test_semver_from_tag_strips_prefix_and_suffix() {
        assert_eq!(semver_from_tag("v1.2.3"), "1.2.3");
        assert_eq!(semver_from_tag("1.2.3"), "1.2.3");
        assert_eq!(semver_from_tag("v1.2.3-next-abc"), "1.2.3");
    }

    #[test]
    fn test_run_impl_with_url() {
        let temp_dir = tempfile::tempdir().unwrap();
        set_test_cache_dir(&temp_dir);

        let mock_url = |body: &str| format!("mock://{}", body);
        let current = env!("CARGO_PKG_VERSION");

        // Newer version available - should upgrade
        let action = run_impl_with_url(
            false,
            Some(&mock_url(
                r#"{"latest":"v999.0.0","next":"v999.0.0-next-deadbeef"}"#,
            )),
            UpdateChannel::Latest,
            false,
        );
        assert_eq!(action, UpgradeAction::UpgradeAvailable);

        // Same version without --force - already latest
        let same_version_payload = format!(
            "{{\"latest\":\"v{}\",\"next\":\"v{}-next-deadbeef\"}}",
            current, current
        );
        let action = run_impl_with_url(
            false,
            Some(&mock_url(&same_version_payload)),
            UpdateChannel::Latest,
            false,
        );
        assert_eq!(action, UpgradeAction::AlreadyLatest);

        // Same version with --force - force reinstall
        let action = run_impl_with_url(
            true,
            Some(&mock_url(&same_version_payload)),
            UpdateChannel::Latest,
            false,
        );
        assert_eq!(action, UpgradeAction::ForceReinstall);

        // Older version without --force - running newer version
        let action = run_impl_with_url(
            false,
            Some(&mock_url(
                r#"{"latest":"v1.0.9","next":"v1.0.9-next-deadbeef"}"#,
            )),
            UpdateChannel::Latest,
            false,
        );
        assert_eq!(action, UpgradeAction::RunningNewerVersion);

        // Older version with --force - force reinstall
        let action = run_impl_with_url(
            true,
            Some(&mock_url(
                r#"{"latest":"v1.0.9","next":"v1.0.9-next-deadbeef"}"#,
            )),
            UpdateChannel::Latest,
            false,
        );
        assert_eq!(action, UpgradeAction::ForceReinstall);

        clear_test_cache_dir();
    }

    #[test]
    fn test_should_check_for_updates_respects_interval() {
        let now = current_timestamp();
        let mut cache = UpdateCache::new(UpdateChannel::Latest);
        cache.last_checked_at = now;
        assert!(!should_check_for_updates(UpdateChannel::Latest, Some(&cache)));

        let stale_offset = (UPDATE_CHECK_INTERVAL_HOURS * 3600) + 10;
        cache.last_checked_at = now.saturating_sub(stale_offset);
        assert!(should_check_for_updates(UpdateChannel::Latest, Some(&cache)));

        assert!(should_check_for_updates(UpdateChannel::Latest, None));
    }

    #[test]
    fn test_should_check_for_updates_verifies_channel() {
        let now = current_timestamp();
        let mut cache = UpdateCache::new(UpdateChannel::Latest);
        cache.last_checked_at = now;
        
        // Cache matches channel - should respect interval
        assert!(!should_check_for_updates(UpdateChannel::Latest, Some(&cache)));
        
        // Cache doesn't match channel - should check for updates
        assert!(should_check_for_updates(UpdateChannel::Next, Some(&cache)));
    }
}
