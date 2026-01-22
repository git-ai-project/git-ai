use crate::api::client::ApiContext;
use crate::config::{self, UpdateChannel};
use crate::observability::log_message;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

const UPDATE_CHECK_INTERVAL_HOURS: u64 = 24;
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

impl UpgradeAction {
    fn to_string(&self) -> &str {
        match self {
            UpgradeAction::UpgradeAvailable => "upgrade_available",
            UpgradeAction::AlreadyLatest => "already_latest",
            UpgradeAction::RunningNewerVersion => "running_newer_version",
            UpgradeAction::ForceReinstall => "force_reinstall",
        }
    }
}

#[derive(Debug, Clone)]
struct ChannelRelease {
    tag: String,
    semver: String,
    checksum: String,
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
struct ChannelInfo {
    version: String,
    checksum: String,
}

#[derive(Debug, Deserialize)]
struct ReleasesResponse {
    channels: HashMap<String, ChannelInfo>,
}

fn get_update_check_cache_path() -> Option<PathBuf> {
    #[cfg(test)]
    {
        if let Ok(test_cache_dir) = std::env::var("GIT_AI_TEST_CACHE_DIR") {
            return Some(PathBuf::from(test_cache_dir).join("update_check"));
        }
    }

    crate::config::update_check_path()
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
    let trimmed = tag
        .trim()
        .trim_start_matches("enterprise-")
        .trim_start_matches('v');
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

fn releases_endpoint() -> &'static str {
    "/worker/releases"
}

fn verify_sha256(content: &[u8], expected_hash: &str) -> Result<(), String> {
    let mut hasher = Sha256::new();
    hasher.update(content);
    let actual_hash = format!("{:x}", hasher.finalize());

    if actual_hash.eq_ignore_ascii_case(expected_hash) {
        Ok(())
    } else {
        Err(format!(
            "Checksum mismatch: expected {}, got {}",
            expected_hash, actual_hash
        ))
    }
}

/// Parse SHA256SUMS file content into a map of filename → hash.
/// Format: `<hash>  <filename>` (two spaces between hash and filename)
fn parse_checksums(content: &str) -> HashMap<String, String> {
    let mut checksums = HashMap::new();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        // Format: "<hash>  <filename>" (two spaces)
        if let Some((hash, filename)) = line.split_once("  ") {
            checksums.insert(filename.to_string(), hash.to_string());
        }
    }

    checksums
}

/// Fetch SHA256SUMS from the releases API and verify against expected checksum.
fn fetch_and_verify_checksums(
    api_base_url: &str,
    channel: &str,
    expected_checksum: &str,
) -> Result<HashMap<String, String>, String> {
    let endpoint = format!("/worker/releases/{}/download/SHA256SUMS", channel);

    let response = ApiContext::http_get(&format!("{}{}", api_base_url, endpoint))
        .with_timeout(30)
        .send()
        .map_err(|e| format!("Failed to fetch SHA256SUMS: {}", e))?;

    if response.status_code != 200 {
        return Err(format!(
            "Failed to fetch SHA256SUMS: HTTP {}",
            response.status_code
        ));
    }

    let content = response
        .as_bytes();

    verify_sha256(content, expected_checksum)
        .map_err(|e| format!("SHA256SUMS verification failed: {}", e))?;

    let content_str = std::str::from_utf8(content)
        .map_err(|e| format!("SHA256SUMS is not valid UTF-8: {}", e))?;

    Ok(parse_checksums(content_str))
}

/// Fetch install script from the releases API and verify against checksums.
fn fetch_and_verify_install_script(
    api_base_url: &str,
    channel: &str,
    checksums: &HashMap<String, String>,
) -> Result<String, String> {
    #[cfg(windows)]
    let script_name = "install.ps1";
    #[cfg(not(windows))]
    let script_name = "install.sh";

    let expected_checksum = checksums
        .get(script_name)
        .ok_or_else(|| format!("Checksum for {} not found in SHA256SUMS", script_name))?;

    let endpoint = format!("/worker/releases/{}/download/{}", channel, script_name);

    let response = ApiContext::http_get(&format!("{}{}", api_base_url, endpoint))
        .with_timeout(30)
        .send()
        .map_err(|e| format!("Failed to fetch {}: {}", script_name, e))?;

    if response.status_code != 200 {
        return Err(format!(
            "Failed to fetch {}: HTTP {}",
            script_name, response.status_code
        ));
    }

    let content = response.as_bytes();

    verify_sha256(content, expected_checksum)
        .map_err(|e| format!("{} verification failed: {}", script_name, e))?;

    let script = std::str::from_utf8(content)
        .map_err(|e| format!("{} is not valid UTF-8: {}", script_name, e))?;

    Ok(script.to_string())
}

fn fetch_release_for_channel(
    api_base_url: &str,
    channel: UpdateChannel,
) -> Result<ChannelRelease, String> {
    #[cfg(test)]
    if let Some(result) = try_mock_releases(api_base_url, channel) {
        return result;
    }

    let context = ApiContext::new(Some(api_base_url.to_string())).with_timeout(5);

    let response = context
        .get(releases_endpoint())
        .map_err(|e| format!("Failed to check for updates: {}", e))?;

    let body = response
        .as_str()
        .map_err(|e| format!("Failed to read response body: {}", e))?;
    let releases: ReleasesResponse = serde_json::from_str(body)
        .map_err(|e| format!("Failed to parse release response: {}", e))?;

    release_from_response(releases, channel)
}

fn release_from_response(
    releases: ReleasesResponse,
    channel: UpdateChannel,
) -> Result<ChannelRelease, String> {
    let channel_name = channel.as_str();

    let channel_info = releases
        .channels
        .get(channel_name)
        .ok_or_else(|| format!("Channel '{}' not found in releases", channel_name))?;

    let tag = channel_info.version.trim().to_string();
    if tag.is_empty() {
        return Err("Release tag not found in response".to_string());
    }

    let semver = semver_from_tag(&tag);
    if semver.is_empty() {
        return Err(format!("Unable to parse semver from tag '{}'", tag));
    }

    let checksum = channel_info.checksum.trim().to_string();
    if checksum.is_empty() {
        return Err("Checksum not found in response".to_string());
    }

    Ok(ChannelRelease {
        tag,
        semver,
        checksum,
    })
}

#[cfg(test)]
fn try_mock_releases(
    base: &str,
    channel: UpdateChannel,
) -> Option<Result<ChannelRelease, String>> {
    let json = base.strip_prefix("mock://")?;
    Some(
        serde_json::from_str::<ReleasesResponse>(json)
            .map_err(|e| format!("Invalid mock releases payload: {}", e))
            .and_then(|releases| release_from_response(releases, channel)),
    )
}

fn run_install_script(script_content: &str, tag: &str, silent: bool) -> Result<(), String> {
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

        // Write the install script to a temp file
        let script_path = log_dir.join(format!("install-{}.ps1", pid));
        fs::write(&script_path, script_content)
            .map_err(|e| format!("Failed to write install script: {}", e))?;
        let script_path_str = script_path.to_string_lossy().to_string();

        // Create log file with initial message
        fs::write(&log_file, format!("Starting upgrade at PID {}\n", pid))
            .map_err(|e| format!("Failed to create log file: {}", e))?;

        // PowerShell wrapper that executes the script file with logging
        let ps_wrapper = format!(
            "$logFile = '{}'; \
             Start-Transcript -Path $logFile -Append -Force | Out-Null; \
             Write-Host 'Running verified install script...'; \
             try {{ \
                 $ErrorActionPreference = 'Continue'; \
                 & '{}'; \
                 Write-Host 'Install script completed'; \
             }} catch {{ \
                 Write-Host \"Error: $_\"; \
                 Write-Host \"Stack trace: $($_.ScriptStackTrace)\"; \
             }} finally {{ \
                 Stop-Transcript | Out-Null; \
                 Remove-Item -Path '{}' -Force -ErrorAction SilentlyContinue; \
             }}",
            log_path_str, script_path_str, script_path_str
        );

        let mut cmd = Command::new("powershell");
        cmd.arg("-NoProfile")
            .arg("-ExecutionPolicy")
            .arg("Bypass")
            .arg("-Command")
            .arg(&ps_wrapper)
            .env(GIT_AI_RELEASE_ENV, tag);

        // Hide the spawned console to prevent any host/UI bleed-through
        cmd.creation_flags(CREATE_NO_WINDOW);

        if silent {
            cmd.stdout(Stdio::null()).stderr(Stdio::null());
        }

        match cmd.spawn() {
            Ok(_) => {
                if !silent {
                    println!(
                        "\x1b[1;33mNote: The installation is running in the background on Windows.\x1b[0m"
                    );
                    println!(
                        "This allows the current git-ai process to exit and release file locks."
                    );
                    println!("Check the log file for progress: {}", log_path_str);
                    println!(
                        "The upgrade should complete shortly as long as there are no long-running git or git-ai processes in the background."
                    );
                }
                Ok(())
            }
            Err(e) => Err(format!("Failed to run installation script: {}", e)),
        }
    }

    #[cfg(not(windows))]
    {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;

        // Write script to a temp file
        let temp_dir = std::env::temp_dir();
        let script_path = temp_dir.join(format!("git-ai-install-{}.sh", std::process::id()));

        // Write and make executable
        let mut file = fs::File::create(&script_path)
            .map_err(|e| format!("Failed to create temp script file: {}", e))?;
        file.write_all(script_content.as_bytes())
            .map_err(|e| format!("Failed to write install script: {}", e))?;
        drop(file);

        fs::set_permissions(&script_path, fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("Failed to make script executable: {}", e))?;

        let script_path_str = script_path.to_string_lossy().to_string();

        let mut cmd = Command::new("bash");
        cmd.arg(&script_path_str).env(GIT_AI_RELEASE_ENV, tag);

        if silent {
            cmd.stdout(Stdio::null()).stderr(Stdio::null());
        }

        let result = match cmd.status() {
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
            Err(e) => Err(format!("Failed to run installation script: {}", e)),
        };

        // Clean up temp script
        let _ = fs::remove_file(&script_path);

        result
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
    let _ = run_impl_with_url(force, config.api_base_url(), channel, skip_install);
}

fn run_impl_with_url(
    force: bool,
    api_base_url: &str,
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

    log_message(
        "checked_for_update",
        "info",
        Some(serde_json::json!({
            "current_version": current_version,
            "api_base_url": api_base_url,
            "channel": channel.as_str(),
            "result": action.to_string()
        })),
    );

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

    if skip_install {
        return action;
    }

    println!("Fetching and verifying release artifacts...");

    // Fetch and verify SHA256SUMS against the release's master checksum
    let checksums = match fetch_and_verify_checksums(api_base_url, channel.as_str(), &release.checksum) {
        Ok(checksums) => {
            println!("\x1b[1;32m✓\x1b[0m SHA256SUMS verified");
            checksums
        }
        Err(err) => {
            eprintln!("Failed to fetch/verify checksums: {}", err);
            std::process::exit(1);
        }
    };

    // Fetch and verify the install script
    let script_content = match fetch_and_verify_install_script(api_base_url, channel.as_str(), &checksums) {
        Ok(content) => {
            #[cfg(windows)]
            println!("\x1b[1;32m✓\x1b[0m install.ps1 verified");
            #[cfg(not(windows))]
            println!("\x1b[1;32m✓\x1b[0m install.sh verified");
            content
        }
        Err(err) => {
            eprintln!("Failed to fetch/verify install script: {}", err);
            std::process::exit(1);
        }
    };

    println!();
    println!("Running installation script...");
    println!();

    match run_install_script(&script_content, &release.tag, false) {
        Ok(()) => {
            // On Windows, we spawn the installer in the background and can't verify success
            #[cfg(not(windows))]
            {
                println!("\x1b[1;32m✓\x1b[0m Successfully installed {}!", release.tag);
            }

            log_message(
                "upgraded",
                "info",
                Some(serde_json::json!({
                    "release_tag": release.tag,
                    "current_version": current_version,
                    "api_base_url": api_base_url,
                    "channel": channel.as_str()
                })),
            );
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
        assert_eq!(semver_from_tag("enterprise-v1.2.3"), "1.2.3");
        assert_eq!(semver_from_tag("enterprise-v1.2.3-next-abc"), "1.2.3");
    }

    #[test]
    fn test_run_impl_with_url() {
        let temp_dir = tempfile::tempdir().unwrap();
        set_test_cache_dir(&temp_dir);

        let mock_url = |body: &str| format!("mock://{}", body);
        let current = env!("CARGO_PKG_VERSION");
        let test_checksum = "a".repeat(64); // Valid SHA256 length

        // Newer version available - should upgrade
        let action = run_impl_with_url(
            false,
            &mock_url(&format!(
                r#"{{"channels":{{"latest":{{"version":"v999.0.0","checksum":"{}"}},"next":{{"version":"v999.0.0-next-deadbeef","checksum":"{}"}}}}}}"#,
                test_checksum, test_checksum
            )),
            UpdateChannel::Latest,
            true,
        );
        assert_eq!(action, UpgradeAction::UpgradeAvailable);

        // Same version without --force - already latest
        let same_version_payload = format!(
            "{{\"channels\":{{\"latest\":{{\"version\":\"v{}\",\"checksum\":\"{}\"}},\"next\":{{\"version\":\"v{}-next-deadbeef\",\"checksum\":\"{}\"}}}}}}",
            current, test_checksum, current, test_checksum
        );
        let action = run_impl_with_url(
            false,
            &mock_url(&same_version_payload),
            UpdateChannel::Latest,
            true,
        );
        assert_eq!(action, UpgradeAction::AlreadyLatest);

        // Same version with --force - force reinstall
        let action = run_impl_with_url(
            true,
            &mock_url(&same_version_payload),
            UpdateChannel::Latest,
            true,
        );
        assert_eq!(action, UpgradeAction::ForceReinstall);

        // Older version without --force - running newer version
        let action = run_impl_with_url(
            false,
            &mock_url(&format!(
                r#"{{"channels":{{"latest":{{"version":"v1.0.9","checksum":"{}"}},"next":{{"version":"v1.0.9-next-deadbeef","checksum":"{}"}}}}}}"#,
                test_checksum, test_checksum
            )),
            UpdateChannel::Latest,
            true,
        );
        assert_eq!(action, UpgradeAction::RunningNewerVersion);

        // Older version with --force - force reinstall
        let action = run_impl_with_url(
            true,
            &mock_url(&format!(
                r#"{{"channels":{{"latest":{{"version":"v1.0.9","checksum":"{}"}},"next":{{"version":"v1.0.9-next-deadbeef","checksum":"{}"}}}}}}"#,
                test_checksum, test_checksum
            )),
            UpdateChannel::Latest,
            true,
        );
        assert_eq!(action, UpgradeAction::ForceReinstall);

        clear_test_cache_dir();
    }

    #[test]
    fn test_run_impl_with_url_enterprise_channels() {
        let temp_dir = tempfile::tempdir().unwrap();
        set_test_cache_dir(&temp_dir);

        let mock_url = |body: &str| format!("mock://{}", body);
        let current = env!("CARGO_PKG_VERSION");
        let test_checksum = "a".repeat(64); // Valid SHA256 length

        // Newer version available - should upgrade
        let action = run_impl_with_url(
            false,
            &mock_url(&format!(
                r#"{{"channels":{{"enterprise-latest":{{"version":"v999.0.0","checksum":"{}"}},"enterprise-next":{{"version":"v999.0.0-next-deadbeef","checksum":"{}"}}}}}}"#,
                test_checksum, test_checksum
            )),
            UpdateChannel::EnterpriseLatest,
            true,
        );
        assert_eq!(action, UpgradeAction::UpgradeAvailable);

        // Same version without --force - already latest
        let same_version_payload = format!(
            "{{\"channels\":{{\"enterprise-latest\":{{\"version\":\"v{}\",\"checksum\":\"{}\"}},\"enterprise-next\":{{\"version\":\"v{}-next-deadbeef\",\"checksum\":\"{}\"}}}}}}",
            current, test_checksum, current, test_checksum
        );
        let action = run_impl_with_url(
            false,
            &mock_url(&same_version_payload),
            UpdateChannel::EnterpriseLatest,
            true,
        );
        assert_eq!(action, UpgradeAction::AlreadyLatest);

        // Same version with --force - force reinstall
        let action = run_impl_with_url(
            true,
            &mock_url(&same_version_payload),
            UpdateChannel::EnterpriseLatest,
            true,
        );
        assert_eq!(action, UpgradeAction::ForceReinstall);

        // Older version without --force - running newer version
        let action = run_impl_with_url(
            false,
            &mock_url(&format!(
                r#"{{"channels":{{"enterprise-latest":{{"version":"v1.0.9","checksum":"{}"}},"enterprise-next":{{"version":"v1.0.9-next-deadbeef","checksum":"{}"}}}}}}"#,
                test_checksum, test_checksum
            )),
            UpdateChannel::EnterpriseLatest,
            true,
        );
        assert_eq!(action, UpgradeAction::RunningNewerVersion);

        // Older version with --force - force reinstall
        let action = run_impl_with_url(
            true,
            &mock_url(&format!(
                r#"{{"channels":{{"enterprise-latest":{{"version":"v1.0.9","checksum":"{}"}},"enterprise-next":{{"version":"v1.0.9-next-deadbeef","checksum":"{}"}}}}}}"#,
                test_checksum, test_checksum
            )),
            UpdateChannel::EnterpriseLatest,
            true,
        );
        assert_eq!(action, UpgradeAction::ForceReinstall);

        clear_test_cache_dir();
    }

    #[test]
    fn test_should_check_for_updates_respects_interval() {
        let now = current_timestamp();
        let mut cache = UpdateCache::new(UpdateChannel::Latest);
        cache.last_checked_at = now;
        assert!(!should_check_for_updates(
            UpdateChannel::Latest,
            Some(&cache)
        ));

        let stale_offset = (UPDATE_CHECK_INTERVAL_HOURS * 3600) + 10;
        cache.last_checked_at = now.saturating_sub(stale_offset);
        assert!(should_check_for_updates(
            UpdateChannel::Latest,
            Some(&cache)
        ));

        assert!(should_check_for_updates(UpdateChannel::Latest, None));
    }

    #[test]
    fn test_should_check_for_updates_verifies_channel() {
        let now = current_timestamp();
        let mut cache = UpdateCache::new(UpdateChannel::Latest);
        cache.last_checked_at = now;

        // Cache matches channel - should respect interval
        assert!(!should_check_for_updates(
            UpdateChannel::Latest,
            Some(&cache)
        ));

        // Cache doesn't match channel - should check for updates
        assert!(should_check_for_updates(UpdateChannel::Next, Some(&cache)));
    }

    #[test]
    fn test_verify_sha256_success() {
        let content = b"hello world";
        // SHA256 of "hello world"
        let expected = "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";
        assert!(verify_sha256(content, expected).is_ok());
    }

    #[test]
    fn test_verify_sha256_case_insensitive() {
        let content = b"hello world";
        let expected_upper = "B94D27B9934D3E08A52E52D7DA7DABFAC484EFE37A5380EE9088F7ACE2EFCDE9";
        assert!(verify_sha256(content, expected_upper).is_ok());
    }

    #[test]
    fn test_verify_sha256_mismatch() {
        let content = b"hello world";
        let wrong_hash = "0000000000000000000000000000000000000000000000000000000000000000";
        let result = verify_sha256(content, wrong_hash);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Checksum mismatch"));
    }

    #[test]
    fn test_verify_sha256_empty_content() {
        let content = b"";
        // SHA256 of empty string
        let expected = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        assert!(verify_sha256(content, expected).is_ok());
    }

    #[test]
    fn test_parse_checksums_valid_format() {
        let content = "594de6cf107e8ffb6efd9029bf727b465ab55a9b4c4c3995eb3e628c857dc423  git-ai-linux-arm64\n\
                       88db3c0c7fc62a815579ec0ca42535c2b83ab18d9e3af8efe345dee96677b1d8  git-ai-linux-x64\n\
                       75d1692d347c3e08a208dc6373df4cee2b5ffd0e2aee62ccb1bb47aae866b2c8  install.sh";

        let checksums = parse_checksums(content);
        assert_eq!(checksums.len(), 3);
        assert_eq!(
            checksums.get("git-ai-linux-arm64"),
            Some(&"594de6cf107e8ffb6efd9029bf727b465ab55a9b4c4c3995eb3e628c857dc423".to_string())
        );
        assert_eq!(
            checksums.get("git-ai-linux-x64"),
            Some(&"88db3c0c7fc62a815579ec0ca42535c2b83ab18d9e3af8efe345dee96677b1d8".to_string())
        );
        assert_eq!(
            checksums.get("install.sh"),
            Some(&"75d1692d347c3e08a208dc6373df4cee2b5ffd0e2aee62ccb1bb47aae866b2c8".to_string())
        );
    }

    #[test]
    fn test_parse_checksums_with_extensions() {
        let content = "23c693a25f4f2e99463c911e67d534ae17cbd9b98513aa65f0ae9da861775d54  git-ai-windows-x64.exe\n\
                       f895af791eb30f6b074b2ab9f0f803e91230b084f5864befcb51ee9ced752adf  install.ps1";

        let checksums = parse_checksums(content);
        assert_eq!(checksums.len(), 2);
        assert!(checksums.contains_key("git-ai-windows-x64.exe"));
        assert!(checksums.contains_key("install.ps1"));
    }

    #[test]
    fn test_parse_checksums_empty_input() {
        let checksums = parse_checksums("");
        assert!(checksums.is_empty());
    }

    #[test]
    fn test_parse_checksums_whitespace_lines() {
        let content = "  \n\nhash  file\n  \n";
        let checksums = parse_checksums(content);
        assert_eq!(checksums.len(), 1);
        assert_eq!(checksums.get("file"), Some(&"hash".to_string()));
    }

    #[test]
    fn test_parse_checksums_ignores_invalid_lines() {
        // Lines with single space or no space should be ignored
        let content = "valid  file1\ninvalid file2\nalsovalid  file3";
        let checksums = parse_checksums(content);
        assert_eq!(checksums.len(), 2);
        assert!(checksums.contains_key("file1"));
        assert!(checksums.contains_key("file3"));
        assert!(!checksums.contains_key("file2"));
    }
}
