use crate::config::{FileConfig, load_file_config_public};
use crate::error::GitAiError;
use crate::mdm::hook_installer::{
    HookCheckResult, HookInstaller, HookInstallerParams, InstallResult, UninstallResult,
};
use crate::mdm::utils::home_dir;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Command;

const XCODE_WATCHER_BINARY_NAME: &str = "git-ai-xcode-watcher";
const XCODE_WATCHER_VERSION_FILE: &str = "git-ai-xcode-watcher.version";
const XCODE_WATCHER_LABEL: &str = "com.gitai.xcode-watcher";
const XCODE_WATCHER_MAIN_SWIFT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/agent-support/xcode/Sources/git-ai-xcode-watcher/main.swift"
));
const XCODE_WATCHER_PACKAGE_SWIFT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/agent-support/xcode/Package.swift"
));
const XCODE_ADD_PATH_GUIDANCE: &str =
    "Xcode: Register a workspace with: git-ai xcode add-path /path/to/workspace";

pub struct XcodeInstaller;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LaunchAgentApplyResult {
    pub message: String,
    pub warning: Option<String>,
}

impl XcodeInstaller {
    fn expand_home_path(path: &Path) -> PathBuf {
        if let Ok(stripped) = path.strip_prefix("~") {
            home_dir().join(stripped)
        } else {
            path.to_path_buf()
        }
    }

    pub(crate) fn watcher_binary_path() -> PathBuf {
        home_dir()
            .join(".git-ai")
            .join("bin")
            .join(XCODE_WATCHER_BINARY_NAME)
    }

    fn launch_log_path() -> PathBuf {
        home_dir()
            .join(".git-ai")
            .join("logs")
            .join("xcode-watcher.log")
    }

    pub(crate) fn version_file_path() -> PathBuf {
        home_dir()
            .join(".git-ai")
            .join("bin")
            .join(XCODE_WATCHER_VERSION_FILE)
    }

    pub(crate) fn plist_path() -> PathBuf {
        home_dir()
            .join("Library")
            .join("LaunchAgents")
            .join(format!("{}.plist", XCODE_WATCHER_LABEL))
    }

    fn build_cache_dir() -> PathBuf {
        home_dir()
            .join(".git-ai")
            .join("cache")
            .join("xcode-watcher-build")
    }

    fn built_binary_path() -> PathBuf {
        Self::build_cache_dir()
            .join(".build")
            .join("release")
            .join(XCODE_WATCHER_BINARY_NAME)
    }

    fn developer_dir_looks_like_xcode(path: &str) -> bool {
        path.trim().contains(".app/Contents/Developer")
    }

    fn xcode_select_developer_dir() -> Option<String> {
        let output = Command::new("xcode-select").args(["-p"]).output().ok()?;
        if !output.status.success() {
            return None;
        }

        let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if path.is_empty() { None } else { Some(path) }
    }

    fn xcode_app_fallback_paths() -> [&'static Path; 2] {
        [
            Path::new("/Applications/Xcode.app"),
            Path::new("/Applications/Xcode-beta.app"),
        ]
    }

    fn is_xcode_ide_available_with(
        xcode_select_path: Option<&str>,
        path_exists: impl Fn(&Path) -> bool,
    ) -> bool {
        if let Some(path) = xcode_select_path
            && Self::developer_dir_looks_like_xcode(path)
        {
            return true;
        }

        Self::xcode_app_fallback_paths()
            .iter()
            .any(|path| path_exists(path))
    }

    fn is_xcode_ide_available() -> bool {
        let xcode_select_path = Self::xcode_select_developer_dir();
        Self::is_xcode_ide_available_with(xcode_select_path.as_deref(), Path::exists)
    }

    fn has_any_installation() -> bool {
        Self::watcher_binary_path().exists()
            || Self::plist_path().exists()
            || Self::version_file_path().exists()
            || Self::build_cache_dir().exists()
    }

    fn check_result_for_environment(
        xcode_ide_available: bool,
        has_any_installation: bool,
        hooks_up_to_date: bool,
    ) -> HookCheckResult {
        if !xcode_ide_available && !has_any_installation {
            return HookCheckResult {
                tool_installed: false,
                hooks_installed: false,
                hooks_up_to_date: false,
            };
        }

        HookCheckResult {
            tool_installed: true,
            hooks_installed: has_any_installation,
            hooks_up_to_date,
        }
    }

    fn is_watcher_up_to_date() -> bool {
        match fs::read_to_string(Self::version_file_path()) {
            Ok(version) => version.trim() == env!("CARGO_PKG_VERSION"),
            Err(_) => false,
        }
    }

    fn install_warning(message: impl Into<String>) -> InstallResult {
        InstallResult {
            changed: false,
            diff: None,
            message: message.into(),
        }
    }

    fn compile_watcher(build_dir: &Path) -> Result<(), InstallResult> {
        const MAX_BUILD_ATTEMPTS: usize = 3;

        for attempt in 0..MAX_BUILD_ATTEMPTS {
            let build_output = Command::new("xcrun")
                .args(["swift", "build", "-c", "release"])
                .current_dir(build_dir)
                .output();

            match build_output {
                Err(e) => {
                    return Err(Self::install_warning(format!(
                        "Xcode: Unable to run Swift compiler: {}",
                        e
                    )));
                }
                Ok(output) if output.status.success() => return Ok(()),
                Ok(output) => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    if stderr.contains("Another instance of SwiftPM is already running")
                        && attempt + 1 < MAX_BUILD_ATTEMPTS
                    {
                        std::thread::sleep(std::time::Duration::from_secs(1));
                        continue;
                    }

                    let stderr_preview: String = stderr.chars().take(200).collect();
                    return Err(Self::install_warning(format!(
                        "Xcode: Unable to compile watcher (swift build exit {}): {}",
                        output.status.code().unwrap_or(-1),
                        stderr_preview
                    )));
                }
            }
        }

        Err(Self::install_warning(
            "Xcode: Unable to compile watcher after retrying",
        ))
    }

    fn remove_file_if_exists(path: &Path) -> Result<bool, String> {
        if !path.exists() {
            return Ok(false);
        }

        fs::remove_file(path)
            .map(|_| true)
            .map_err(|e| format!("Unable to remove {}: {}", path.display(), e))
    }

    fn remove_dir_if_exists(path: &Path) -> Result<bool, String> {
        if !path.exists() {
            return Ok(false);
        }

        fs::remove_dir_all(path)
            .map(|_| true)
            .map_err(|e| format!("Unable to remove {}: {}", path.display(), e))
    }

    pub(crate) fn configured_paths_from_file_config(
        file_config: &FileConfig,
    ) -> Result<Vec<PathBuf>, String> {
        let mut paths = Vec::new();
        for raw_path in file_config.xcode_paths.as_deref().unwrap_or(&[]) {
            let trimmed = raw_path.trim();
            if trimmed.is_empty() {
                continue;
            }

            let mut path = Self::expand_home_path(Path::new(trimmed));
            if !path.is_absolute() {
                return Err(format!(
                    "Configured xcode_paths entry '{}' must be an absolute path",
                    trimmed
                ));
            }
            if path.exists()
                && let Ok(canonical) = fs::canonicalize(&path)
            {
                path = canonical;
            }
            paths.push(path);
        }

        Ok(Self::normalize_watch_paths(paths))
    }

    pub(crate) fn configured_paths_from_disk() -> Result<Vec<PathBuf>, String> {
        let file_config = load_file_config_public()?;
        Self::configured_paths_from_file_config(&file_config)
    }

    pub(crate) fn validate_new_watch_path(path: &Path) -> Result<PathBuf, String> {
        let path = Self::expand_home_path(path);
        let metadata = fs::metadata(&path).map_err(|e| {
            format!(
                "Path '{}' does not exist or is not accessible: {}",
                path.display(),
                e
            )
        })?;
        if !metadata.is_dir() {
            return Err(format!("Path '{}' must be a directory", path.display()));
        }

        let canonical = fs::canonicalize(&path)
            .map_err(|e| format!("Unable to resolve path '{}': {}", path.display(), e))?;
        let home = fs::canonicalize(home_dir()).unwrap_or_else(|_| home_dir());

        if canonical == Path::new("/") {
            return Err("Refusing to watch '/'; choose a narrower workspace root".to_string());
        }
        if canonical == home {
            return Err(
                "Refusing to watch your HOME directory; choose a narrower workspace root"
                    .to_string(),
            );
        }
        if canonical == Path::new("/Users") {
            return Err("Refusing to watch '/Users'; choose a narrower workspace root".to_string());
        }

        Ok(canonical)
    }

    pub(crate) fn normalize_watch_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
        let mut sorted_paths = paths;
        sorted_paths.sort_by(|left, right| {
            left.components()
                .count()
                .cmp(&right.components().count())
                .then_with(|| left.as_os_str().cmp(right.as_os_str()))
        });

        let mut normalized = Vec::new();
        for path in sorted_paths {
            if normalized
                .iter()
                .any(|existing: &PathBuf| path == *existing || path.starts_with(existing))
            {
                continue;
            }

            normalized.retain(|existing| !existing.starts_with(&path));
            normalized.push(path);
        }

        normalized.sort_by(|left, right| left.as_os_str().cmp(right.as_os_str()));
        normalized
    }

    pub(crate) fn serialize_watch_paths(paths: &[PathBuf]) -> Option<Vec<String>> {
        if paths.is_empty() {
            None
        } else {
            Some(
                paths
                    .iter()
                    .map(|path| path.to_string_lossy().to_string())
                    .collect(),
            )
        }
    }

    pub(crate) fn validate_paths_for_launch(paths: &[PathBuf]) -> Result<(), String> {
        for path in paths {
            let metadata = fs::metadata(path).map_err(|e| {
                format!(
                    "Configured Xcode watch path '{}' is not accessible: {}",
                    path.display(),
                    e
                )
            })?;
            if !metadata.is_dir() {
                return Err(format!(
                    "Configured Xcode watch path '{}' is not a directory",
                    path.display()
                ));
            }
        }
        Ok(())
    }

    fn plist_xml(paths: &[PathBuf]) -> String {
        fn escape_xml(value: &str) -> String {
            value
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
                .replace('\"', "&quot;")
                .replace('\'', "&apos;")
        }

        let mut args = vec![format!(
            "    <string>{}</string>",
            escape_xml(&Self::watcher_binary_path().to_string_lossy())
        )];
        for path in paths {
            args.push("    <string>--path</string>".to_string());
            args.push(format!(
                "    <string>{}</string>",
                escape_xml(&path.to_string_lossy())
            ));
        }

        let log_path = escape_xml(&Self::launch_log_path().to_string_lossy());

        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{label}</string>
  <key>ProgramArguments</key>
  <array>
{args}
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>StandardOutPath</key>
  <string>{log_path}</string>
  <key>StandardErrorPath</key>
  <string>{log_path}</string>
</dict>
</plist>
"#,
            label = XCODE_WATCHER_LABEL,
            args = args.join("\n"),
            log_path = log_path,
        )
    }

    fn write_launch_agent_plist(paths: &[PathBuf]) -> Result<(), String> {
        if let Some(parent) = Self::plist_path().parent() {
            fs::create_dir_all(parent).map_err(|e| {
                format!(
                    "Unable to create LaunchAgents directory {}: {}",
                    parent.display(),
                    e
                )
            })?;
        }
        if let Some(parent) = Self::launch_log_path().parent() {
            fs::create_dir_all(parent).map_err(|e| {
                format!("Unable to create log directory {}: {}", parent.display(), e)
            })?;
        }

        fs::write(Self::plist_path(), Self::plist_xml(paths))
            .map_err(|e| format!("Unable to write plist: {}", e))
    }

    fn launchctl_domain_target() -> Result<String, String> {
        let uid = unsafe { libc::geteuid() };
        if uid == 0 {
            return Err(
                "Unable to reload watcher automatically from a root or non-GUI session. Run 'git-ai xcode reload' from the target user's login session.".to_string(),
            );
        }
        Ok(format!("gui/{}", uid))
    }

    fn run_launchctl(args: &[String]) -> Result<(), String> {
        let output = Command::new("launchctl")
            .args(args)
            .output()
            .map_err(|e| format!("Unable to run launchctl: {}", e))?;

        if output.status.success() {
            return Ok(());
        }

        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = if !stderr.is_empty() {
            stderr
        } else if !stdout.is_empty() {
            stdout
        } else {
            format!("exit {}", output.status.code().unwrap_or(-1))
        };
        Err(detail)
    }

    fn bootout_launch_agent(domain: &str) -> Result<(), String> {
        let args = vec![
            "bootout".to_string(),
            domain.to_string(),
            Self::plist_path().to_string_lossy().to_string(),
        ];
        match Self::run_launchctl(&args) {
            Ok(()) => Ok(()),
            Err(error)
                if error.contains("Could not find service")
                    || error.contains("No such process")
                    || error.contains("not loaded")
                    || error.contains("service could not be found") =>
            {
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    fn clean_launch_agent() -> Result<LaunchAgentApplyResult, String> {
        let launchctl_warning = match Self::launchctl_domain_target() {
            Ok(domain) => Self::bootout_launch_agent(&domain).err(),
            Err(warning) => Some(warning),
        };

        match fs::remove_file(Self::plist_path()) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(format!("Unable to remove plist: {}", error)),
        }

        Ok(LaunchAgentApplyResult {
            message: "Xcode: Watcher LaunchAgent removed; no Xcode paths are configured"
                .to_string(),
            warning: launchctl_warning,
        })
    }

    pub(crate) fn apply_launch_agent(paths: &[PathBuf]) -> Result<LaunchAgentApplyResult, String> {
        if cfg!(not(target_os = "macos")) {
            return Err("Xcode watcher configuration is only supported on macOS".to_string());
        }

        if paths.is_empty() {
            return Self::clean_launch_agent();
        }

        Self::validate_paths_for_launch(paths)?;

        if !Self::watcher_binary_path().exists() {
            let _ = fs::remove_file(Self::plist_path());
            return Ok(LaunchAgentApplyResult {
                message: format!(
                    "Xcode: Saved {} watch path(s), but watcher binary is not installed yet",
                    paths.len()
                ),
                warning: Some(
                    "Run 'git-ai install-hooks' to install git-ai-xcode-watcher, then run 'git-ai xcode reload'.".to_string(),
                ),
            });
        }

        Self::write_launch_agent_plist(paths)?;

        let domain = match Self::launchctl_domain_target() {
            Ok(domain) => domain,
            Err(warning) => {
                return Ok(LaunchAgentApplyResult {
                    message: format!(
                        "Xcode: LaunchAgent updated with {} watch path(s)",
                        paths.len()
                    ),
                    warning: Some(warning),
                });
            }
        };

        let service = format!("{}/{}", domain, XCODE_WATCHER_LABEL);
        let bootout_warning = Self::bootout_launch_agent(&domain).err();

        let bootstrap_args = vec![
            "bootstrap".to_string(),
            domain.clone(),
            Self::plist_path().to_string_lossy().to_string(),
        ];
        let launchctl_warning = if let Err(error) = Self::run_launchctl(&bootstrap_args) {
            Some(match bootout_warning {
                Some(bootout) => format!(
                    "Bootstrap failed: {} (preceded by bootout warning: {})",
                    error, bootout
                ),
                None => error,
            })
        } else {
            let kickstart_args = vec!["kickstart".to_string(), "-k".to_string(), service];
            Self::run_launchctl(&kickstart_args).err()
        };

        Ok(LaunchAgentApplyResult {
            message: format!(
                "Xcode: LaunchAgent updated with {} watch path(s)",
                paths.len()
            ),
            warning: launchctl_warning.map(|error| {
                format!(
                    "Unable to reload watcher automatically: {}. Run 'git-ai xcode reload' manually.",
                    error
                )
            }),
        })
    }
}

impl HookInstaller for XcodeInstaller {
    fn name(&self) -> &str {
        "Xcode"
    }

    fn id(&self) -> &str {
        "xcode"
    }

    fn uses_config_hooks(&self) -> bool {
        false
    }

    fn check_hooks(&self, _params: &HookInstallerParams) -> Result<HookCheckResult, GitAiError> {
        if cfg!(not(target_os = "macos")) {
            return Ok(HookCheckResult {
                tool_installed: false,
                hooks_installed: false,
                hooks_up_to_date: false,
            });
        }

        let xcode_ide_available = Self::is_xcode_ide_available();
        let has_any_installation = Self::has_any_installation();
        let hooks_up_to_date =
            Self::watcher_binary_path().exists() && Self::is_watcher_up_to_date();

        Ok(Self::check_result_for_environment(
            xcode_ide_available,
            has_any_installation,
            hooks_up_to_date,
        ))
    }

    fn install_hooks(
        &self,
        _params: &HookInstallerParams,
        _dry_run: bool,
    ) -> Result<Option<String>, GitAiError> {
        Ok(None)
    }

    fn uninstall_hooks(
        &self,
        _params: &HookInstallerParams,
        _dry_run: bool,
    ) -> Result<Option<String>, GitAiError> {
        Ok(None)
    }

    fn install_extras(
        &self,
        _params: &HookInstallerParams,
        dry_run: bool,
    ) -> Result<Vec<InstallResult>, GitAiError> {
        let mut results = Vec::new();

        if cfg!(not(target_os = "macos")) {
            return Ok(results);
        }

        if !Self::is_xcode_ide_available() {
            return Ok(results);
        }

        let configured_paths = match Self::configured_paths_from_disk() {
            Ok(paths) => paths,
            Err(error) => {
                results.push(Self::install_warning(format!(
                    "Xcode: Unable to read configured xcode_paths: {}. Run 'git-ai xcode reload' manually after fixing the config.",
                    error
                )));
                Vec::new()
            }
        };

        let watcher_bin = Self::watcher_binary_path();
        if watcher_bin.exists() && Self::is_watcher_up_to_date() {
            results.push(Self::install_warning(
                "Xcode: Watcher already installed and up to date",
            ));
            if configured_paths.is_empty() {
                if Self::plist_path().exists() {
                    match Self::clean_launch_agent() {
                        Ok(apply_result) => {
                            results.push(InstallResult {
                                changed: false,
                                diff: None,
                                message: apply_result.message,
                            });
                            if let Some(warning) = apply_result.warning {
                                results.push(Self::install_warning(format!("Xcode: {}", warning)));
                            }
                        }
                        Err(error) => results.push(Self::install_warning(format!(
                            "Xcode: Unable to remove stale LaunchAgent automatically: {}. Run 'git-ai xcode reload' manually.",
                            error
                        ))),
                    }
                }
                results.push(Self::install_warning(XCODE_ADD_PATH_GUIDANCE));
            } else {
                match Self::apply_launch_agent(&configured_paths) {
                    Ok(apply_result) => {
                        results.push(InstallResult {
                            changed: false,
                            diff: None,
                            message: apply_result.message,
                        });
                        if let Some(warning) = apply_result.warning {
                            results.push(Self::install_warning(format!("Xcode: {}", warning)));
                        }
                    }
                    Err(error) => results.push(Self::install_warning(format!(
                        "Xcode: Unable to reload watcher automatically: {}. Run 'git-ai xcode reload' manually.",
                        error
                    ))),
                }
            }
            return Ok(results);
        }

        if dry_run {
            results.push(InstallResult {
                changed: true,
                diff: None,
                message: "Xcode: Pending watcher compilation and installation".to_string(),
            });
            if configured_paths.is_empty() {
                if Self::plist_path().exists() {
                    results.push(Self::install_warning(
                        "Xcode: Pending stale LaunchAgent removal because no xcode_paths are configured",
                    ));
                }
                results.push(Self::install_warning(XCODE_ADD_PATH_GUIDANCE));
            } else {
                results.push(Self::install_warning(
                    "Xcode: Pending LaunchAgent reload from configured xcode_paths",
                ));
            }
            return Ok(results);
        }

        let build_dir = Self::build_cache_dir();
        let sources_dir = build_dir.join("Sources").join(XCODE_WATCHER_BINARY_NAME);
        if let Err(e) = fs::create_dir_all(&sources_dir) {
            results.push(Self::install_warning(format!(
                "Xcode: Unable to create build cache directory: {}",
                e
            )));
            return Ok(results);
        }

        if let Err(e) = fs::write(build_dir.join("Package.swift"), XCODE_WATCHER_PACKAGE_SWIFT) {
            results.push(Self::install_warning(format!(
                "Xcode: Unable to write Package.swift: {}",
                e
            )));
            return Ok(results);
        }

        if let Err(e) = fs::write(sources_dir.join("main.swift"), XCODE_WATCHER_MAIN_SWIFT) {
            results.push(Self::install_warning(format!(
                "Xcode: Unable to write main.swift: {}",
                e
            )));
            return Ok(results);
        }

        if let Err(result) = Self::compile_watcher(&build_dir) {
            results.push(result);
            return Ok(results);
        }

        let built_binary = Self::built_binary_path();
        if !built_binary.exists() {
            results.push(Self::install_warning(
                "Xcode: Unable to find compiled binary after swift build",
            ));
            return Ok(results);
        }

        if let Some(parent) = watcher_bin.parent()
            && let Err(e) = fs::create_dir_all(parent)
        {
            results.push(Self::install_warning(format!(
                "Xcode: Unable to create binary directory: {}",
                e
            )));
            return Ok(results);
        }

        if let Err(e) = fs::copy(&built_binary, &watcher_bin) {
            results.push(Self::install_warning(format!(
                "Xcode: Unable to install watcher binary: {}",
                e
            )));
            return Ok(results);
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let _ = fs::set_permissions(&watcher_bin, fs::Permissions::from_mode(0o755));
        }

        if let Err(e) = fs::write(Self::version_file_path(), env!("CARGO_PKG_VERSION")) {
            results.push(Self::install_warning(format!(
                "Xcode: Unable to write watcher version file: {}",
                e
            )));
        }

        results.push(InstallResult {
            changed: true,
            diff: None,
            message: format!(
                "Xcode: Watcher binary installed to {}",
                watcher_bin.display()
            ),
        });
        if configured_paths.is_empty() {
            if Self::plist_path().exists() {
                match Self::clean_launch_agent() {
                    Ok(apply_result) => {
                        results.push(InstallResult {
                            changed: false,
                            diff: None,
                            message: apply_result.message,
                        });
                        if let Some(warning) = apply_result.warning {
                            results.push(Self::install_warning(format!("Xcode: {}", warning)));
                        }
                    }
                    Err(error) => results.push(Self::install_warning(format!(
                        "Xcode: Unable to remove stale LaunchAgent automatically: {}. Run 'git-ai xcode reload' manually.",
                        error
                    ))),
                }
            }
            results.push(Self::install_warning(XCODE_ADD_PATH_GUIDANCE));
        } else {
            match Self::apply_launch_agent(&configured_paths) {
                Ok(apply_result) => {
                    results.push(InstallResult {
                        changed: false,
                        diff: None,
                        message: apply_result.message,
                    });
                    if let Some(warning) = apply_result.warning {
                        results.push(Self::install_warning(format!("Xcode: {}", warning)));
                    }
                }
                Err(error) => results.push(Self::install_warning(format!(
                    "Xcode: Unable to reload watcher automatically: {}. Run 'git-ai xcode reload' manually.",
                    error
                ))),
            }
        }

        Ok(results)
    }

    fn uninstall_extras(
        &self,
        _params: &HookInstallerParams,
        dry_run: bool,
    ) -> Result<Vec<UninstallResult>, GitAiError> {
        let mut results = Vec::new();

        if cfg!(not(target_os = "macos")) {
            return Ok(results);
        }

        let plist_path = Self::plist_path();
        let watcher_bin = Self::watcher_binary_path();
        let version_file = Self::version_file_path();
        let build_cache = Self::build_cache_dir();

        let has_anything = plist_path.exists()
            || watcher_bin.exists()
            || version_file.exists()
            || build_cache.exists();

        if !has_anything {
            results.push(UninstallResult {
                changed: false,
                diff: None,
                message: "Xcode: Watcher not installed, nothing to uninstall".to_string(),
            });
            return Ok(results);
        }

        if dry_run {
            results.push(UninstallResult {
                changed: true,
                diff: None,
                message: "Xcode: Pending watcher removal".to_string(),
            });
            return Ok(results);
        }

        if plist_path.exists() {
            let launchctl_warning = match Self::launchctl_domain_target() {
                Ok(domain) => Self::bootout_launch_agent(&domain).err(),
                Err(warning) => Some(warning),
            };

            match Self::remove_file_if_exists(&plist_path) {
                Ok(true) => results.push(UninstallResult {
                    changed: true,
                    diff: None,
                    message: "Xcode: launchd service unloaded and plist removed".to_string(),
                }),
                Ok(false) => {}
                Err(message) => results.push(UninstallResult {
                    changed: false,
                    diff: None,
                    message: format!("Xcode: {}", message),
                }),
            }

            if let Some(warning) = launchctl_warning {
                results.push(UninstallResult {
                    changed: false,
                    diff: None,
                    message: format!(
                        "Xcode: Unable to stop watcher LaunchAgent automatically: {}",
                        warning
                    ),
                });
            }
        }

        match Self::remove_file_if_exists(&watcher_bin) {
            Ok(true) => results.push(UninstallResult {
                changed: true,
                diff: None,
                message: "Xcode: Watcher binary removed".to_string(),
            }),
            Ok(false) => {}
            Err(message) => results.push(UninstallResult {
                changed: false,
                diff: None,
                message: format!("Xcode: {}", message),
            }),
        }

        match Self::remove_file_if_exists(&version_file) {
            Ok(true) => results.push(UninstallResult {
                changed: true,
                diff: None,
                message: "Xcode: Watcher version marker removed".to_string(),
            }),
            Ok(false) => {}
            Err(message) => results.push(UninstallResult {
                changed: false,
                diff: None,
                message: format!("Xcode: {}", message),
            }),
        }

        match Self::remove_dir_if_exists(&build_cache) {
            Ok(true) => results.push(UninstallResult {
                changed: true,
                diff: None,
                message: "Xcode: Build cache removed".to_string(),
            }),
            Ok(false) => {}
            Err(message) => results.push(UninstallResult {
                changed: false,
                diff: None,
                message: format!("Xcode: {}", message),
            }),
        }

        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mdm::hook_installer::{HookInstaller, HookInstallerParams};
    use serial_test::serial;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    fn test_params() -> HookInstallerParams {
        HookInstallerParams {
            binary_path: PathBuf::from("/usr/local/bin/git-ai"),
        }
    }

    fn with_temp_home<F: FnOnce(&Path)>(f: F) {
        let temp = tempdir().unwrap();
        let home = temp.path().to_path_buf();

        let prev_home = std::env::var_os("HOME");
        let prev_userprofile = std::env::var_os("USERPROFILE");

        // SAFETY: tests are serialized via #[serial], so mutating process env is safe.
        unsafe {
            std::env::set_var("HOME", &home);
            std::env::set_var("USERPROFILE", &home);
        }

        f(&home);

        // SAFETY: tests are serialized via #[serial], so restoring process env is safe.
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

    fn with_path_override<F: FnOnce(&Path)>(f: F) {
        let temp = tempdir().unwrap();
        let bin_dir = temp.path().join("bin");
        fs::create_dir_all(&bin_dir).unwrap();

        let prev_path = std::env::var_os("PATH");
        let new_path = match &prev_path {
            Some(existing) => format!("{}:{}", bin_dir.display(), existing.to_string_lossy()),
            None => bin_dir.display().to_string(),
        };

        unsafe {
            std::env::set_var("PATH", &new_path);
        }

        f(&bin_dir);

        unsafe {
            match prev_path {
                Some(value) => std::env::set_var("PATH", value),
                None => std::env::remove_var("PATH"),
            }
        }
    }

    fn write_launchctl_stub(bin_dir: &Path, exit_code: i32, log_path: &Path) {
        let stub_path = bin_dir.join("launchctl");
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" >> '{}'\nexit {}\n",
            log_path.display(),
            exit_code
        );
        fs::write(&stub_path, script).unwrap();
        #[cfg(unix)]
        fs::set_permissions(&stub_path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn write_launchctl_bootout_io_error_stub(bin_dir: &Path, log_path: &Path) {
        let stub_path = bin_dir.join("launchctl");
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" >> '{}'\nif [ \"$1\" = \"bootout\" ]; then\n  echo 'Boot-out failed: 5: Input/output error' >&2\n  exit 5\nfi\nexit 0\n",
            log_path.display(),
        );
        fs::write(&stub_path, script).unwrap();
        #[cfg(unix)]
        fs::set_permissions(&stub_path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn test_xcode_installer_name_and_id() {
        let installer = XcodeInstaller;
        assert_eq!(installer.name(), "Xcode");
        assert_eq!(installer.id(), "xcode");
    }

    #[test]
    fn test_uses_config_hooks_returns_false() {
        let installer = XcodeInstaller;
        assert!(!installer.uses_config_hooks());
    }

    #[test]
    fn test_install_hooks_returns_none() {
        let installer = XcodeInstaller;
        assert_eq!(
            installer.install_hooks(&test_params(), false).unwrap(),
            None
        );
    }

    #[test]
    fn test_uninstall_hooks_returns_none() {
        let installer = XcodeInstaller;
        assert_eq!(
            installer.uninstall_hooks(&test_params(), false).unwrap(),
            None
        );
    }

    #[test]
    #[serial]
    fn test_xcode_paths_are_under_home_directory() {
        with_temp_home(|home| {
            assert_eq!(
                XcodeInstaller::watcher_binary_path(),
                home.join(".git-ai/bin/git-ai-xcode-watcher")
            );
            assert_eq!(
                XcodeInstaller::version_file_path(),
                home.join(".git-ai/bin/git-ai-xcode-watcher.version")
            );
            assert_eq!(
                XcodeInstaller::plist_path(),
                home.join("Library/LaunchAgents/com.gitai.xcode-watcher.plist")
            );
            assert_eq!(
                XcodeInstaller::launch_log_path(),
                home.join(".git-ai/logs/xcode-watcher.log")
            );
            assert_eq!(
                XcodeInstaller::build_cache_dir(),
                home.join(".git-ai/cache/xcode-watcher-build")
            );
        });
    }

    #[test]
    fn test_normalize_watch_paths_collapses_descendants() {
        let parent = PathBuf::from("/tmp/workspace");
        let child = parent.join("AppA");
        let grandchild = child.join("Feature");

        let normalized =
            XcodeInstaller::normalize_watch_paths(vec![grandchild, child, parent.clone()]);

        assert_eq!(normalized, vec![parent]);
    }

    #[test]
    #[serial]
    fn test_validate_new_watch_path_rejects_overly_broad_roots() {
        with_temp_home(|home| {
            let home_error = XcodeInstaller::validate_new_watch_path(home).unwrap_err();
            assert!(home_error.contains("HOME directory"));

            let root_error = XcodeInstaller::validate_new_watch_path(Path::new("/")).unwrap_err();
            assert!(root_error.contains("Refusing to watch '/'"));
        });
    }

    #[test]
    #[serial]
    fn test_configured_paths_from_file_config_canonicalizes_existing_entries() {
        with_temp_home(|home| {
            let workspace = home.join("ios").join("AppA");
            fs::create_dir_all(&workspace).unwrap();

            let file_config = FileConfig {
                xcode_paths: Some(vec![workspace.to_string_lossy().to_string()]),
                ..Default::default()
            };

            let paths = XcodeInstaller::configured_paths_from_file_config(&file_config).unwrap();
            assert_eq!(paths, vec![fs::canonicalize(workspace).unwrap()]);
        });
    }

    #[test]
    #[serial]
    fn test_plist_xml_contains_all_paths_and_log_file() {
        with_temp_home(|home| {
            let path_a = home.join("ios");
            let path_b = home.join("mac");
            let plist = XcodeInstaller::plist_xml(&[path_a.clone(), path_b.clone()]);

            assert!(plist.contains(XCODE_WATCHER_LABEL));
            assert!(plist.contains(&*XcodeInstaller::watcher_binary_path().to_string_lossy()));
            assert!(plist.contains(&*path_a.to_string_lossy()));
            assert!(plist.contains(&*path_b.to_string_lossy()));
            assert!(plist.contains(&*XcodeInstaller::launch_log_path().to_string_lossy()));
        });
    }

    #[test]
    fn test_embedded_sources_not_empty() {
        assert!(!XCODE_WATCHER_MAIN_SWIFT.is_empty());
        assert!(!XCODE_WATCHER_PACKAGE_SWIFT.is_empty());
        assert!(XCODE_WATCHER_PACKAGE_SWIFT.contains("swift-tools-version"));
        assert!(XCODE_WATCHER_MAIN_SWIFT.contains("FSEventStream"));
    }

    #[test]
    fn test_embedded_sources_contain_known_human_preset() {
        assert!(XCODE_WATCHER_MAIN_SWIFT.contains("known_human"));
        assert!(XCODE_WATCHER_MAIN_SWIFT.contains("checkpoint"));
    }

    #[test]
    fn test_failure_messages_use_visibility_keywords() {
        let messages = [
            "Xcode: Unable to create build cache directory: permission denied",
            "Xcode: Unable to write Package.swift: disk full",
            "Xcode: Unable to run Swift compiler: not found",
            "Xcode: Unable to compile watcher (swift build exit 1): error",
            "Xcode: Unable to find compiled binary after swift build",
            "Xcode: Unable to install watcher binary: permission denied",
            "Xcode: Unable to write watcher version file: permission denied",
        ];

        for message in messages {
            assert!(
                message.contains("Unable") || message.contains("Failed"),
                "message '{message}' must contain visibility keywords"
            );
        }
    }

    #[test]
    fn test_developer_dir_looks_like_xcode() {
        assert!(XcodeInstaller::developer_dir_looks_like_xcode(
            "/Applications/Xcode.app/Contents/Developer"
        ));
        assert!(XcodeInstaller::developer_dir_looks_like_xcode(
            "/Applications/Xcode-beta.app/Contents/Developer"
        ));
        assert!(!XcodeInstaller::developer_dir_looks_like_xcode(
            "/Library/Developer/CommandLineTools"
        ));
    }

    #[test]
    fn test_is_xcode_ide_available_with_prefers_xcode_select_path() {
        let available = XcodeInstaller::is_xcode_ide_available_with(
            Some("/opt/Xcodes/Xcode-16.app/Contents/Developer"),
            |_| false,
        );
        assert!(available);
    }

    #[test]
    fn test_is_xcode_ide_available_with_uses_fallback_paths() {
        let available = XcodeInstaller::is_xcode_ide_available_with(
            Some("/Library/Developer/CommandLineTools"),
            |path| path == Path::new("/Applications/Xcode-beta.app"),
        );
        assert!(available);
    }

    #[test]
    fn test_is_xcode_ide_available_with_rejects_clt_only() {
        let available = XcodeInstaller::is_xcode_ide_available_with(
            Some("/Library/Developer/CommandLineTools"),
            |_| false,
        );
        assert!(!available);
    }

    #[test]
    fn test_check_result_matrix() {
        let unavailable = XcodeInstaller::check_result_for_environment(false, false, false);
        assert!(!unavailable.tool_installed);
        assert!(!unavailable.hooks_installed);
        assert!(!unavailable.hooks_up_to_date);

        let fresh_install = XcodeInstaller::check_result_for_environment(true, false, false);
        assert!(fresh_install.tool_installed);
        assert!(!fresh_install.hooks_installed);
        assert!(!fresh_install.hooks_up_to_date);

        let residual = XcodeInstaller::check_result_for_environment(false, true, false);
        assert!(residual.tool_installed);
        assert!(residual.hooks_installed);
        assert!(!residual.hooks_up_to_date);

        let up_to_date = XcodeInstaller::check_result_for_environment(true, true, true);
        assert!(up_to_date.tool_installed);
        assert!(up_to_date.hooks_installed);
        assert!(up_to_date.hooks_up_to_date);
    }

    #[test]
    #[serial]
    fn test_is_watcher_up_to_date_reads_version_file() {
        with_temp_home(|_| {
            let version_path = XcodeInstaller::version_file_path();
            fs::create_dir_all(version_path.parent().unwrap()).unwrap();
            fs::write(&version_path, env!("CARGO_PKG_VERSION")).unwrap();

            assert!(XcodeInstaller::is_watcher_up_to_date());

            fs::write(&version_path, "0.0.0").unwrap();
            assert!(!XcodeInstaller::is_watcher_up_to_date());
        });
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[serial]
    fn test_check_hooks_reports_residual_installation_without_xcode() {
        with_temp_home(|_| {
            let installer = XcodeInstaller;
            let build_cache = XcodeInstaller::build_cache_dir();
            fs::create_dir_all(&build_cache).unwrap();

            let result = installer.check_hooks(&test_params()).unwrap();
            assert!(result.tool_installed);
            assert!(result.hooks_installed);
            assert!(!result.hooks_up_to_date);
        });
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[serial]
    fn test_uninstall_extras_removes_residual_artifacts() {
        with_temp_home(|home| {
            let installer = XcodeInstaller;

            let plist_path = XcodeInstaller::plist_path();
            let watcher_bin = XcodeInstaller::watcher_binary_path();
            let version_file = XcodeInstaller::version_file_path();
            let build_cache = XcodeInstaller::build_cache_dir();

            fs::create_dir_all(plist_path.parent().unwrap()).unwrap();
            fs::create_dir_all(watcher_bin.parent().unwrap()).unwrap();
            fs::create_dir_all(build_cache.join(".build")).unwrap();
            fs::write(&plist_path, "plist").unwrap();
            fs::write(&watcher_bin, "binary").unwrap();
            fs::write(&version_file, env!("CARGO_PKG_VERSION")).unwrap();
            fs::write(build_cache.join(".build/placeholder"), "cache").unwrap();

            let results = installer.uninstall_extras(&test_params(), false).unwrap();

            assert!(
                results
                    .iter()
                    .any(|result| result.message.contains("Watcher binary removed"))
            );
            assert!(
                results
                    .iter()
                    .any(|result| result.message.contains("Build cache removed"))
            );
            assert!(home.join(".git-ai/bin").exists());
            assert!(!plist_path.exists());
            assert!(!watcher_bin.exists());
            assert!(!version_file.exists());
            assert!(!build_cache.exists());
        });
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[serial]
    fn test_uninstall_extras_reports_version_file_only_cleanup() {
        with_temp_home(|_| {
            let installer = XcodeInstaller;
            let version_file = XcodeInstaller::version_file_path();
            fs::create_dir_all(version_file.parent().unwrap()).unwrap();
            fs::write(&version_file, "stale-version").unwrap();

            let results = installer.uninstall_extras(&test_params(), false).unwrap();

            assert!(
                results
                    .iter()
                    .any(|result| result.message.contains("Watcher version marker removed"))
            );
            assert!(!version_file.exists());
        });
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[serial]
    fn test_apply_launch_agent_warns_when_binary_missing() {
        with_temp_home(|home| {
            let workspace = home.join("ios");
            fs::create_dir_all(&workspace).unwrap();

            let result = XcodeInstaller::apply_launch_agent(&[workspace]).unwrap();
            assert!(result.message.contains("watch path"));
            assert!(
                result
                    .warning
                    .as_deref()
                    .unwrap()
                    .contains("git-ai install-hooks")
            );
            assert!(!XcodeInstaller::plist_path().exists());
        });
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[serial]
    fn test_apply_launch_agent_writes_plist_and_calls_launchctl() {
        with_temp_home(|home| {
            with_path_override(|bin_dir| {
                let workspace = home.join("ios");
                let launchctl_log = home.join("launchctl.log");
                fs::create_dir_all(&workspace).unwrap();
                fs::create_dir_all(XcodeInstaller::watcher_binary_path().parent().unwrap())
                    .unwrap();
                fs::write(XcodeInstaller::watcher_binary_path(), "#!/bin/sh\n").unwrap();
                #[cfg(unix)]
                fs::set_permissions(
                    XcodeInstaller::watcher_binary_path(),
                    fs::Permissions::from_mode(0o755),
                )
                .unwrap();
                write_launchctl_stub(bin_dir, 0, &launchctl_log);

                let result =
                    XcodeInstaller::apply_launch_agent(std::slice::from_ref(&workspace)).unwrap();
                assert!(result.warning.is_none());
                assert!(XcodeInstaller::plist_path().exists());

                let plist = fs::read_to_string(XcodeInstaller::plist_path()).unwrap();
                assert!(plist.contains(&*workspace.to_string_lossy()));

                let log = fs::read_to_string(launchctl_log).unwrap();
                assert!(log.contains("bootout"));
                assert!(log.contains("bootstrap"));
                assert!(log.contains("kickstart"));
            });
        });
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[serial]
    fn test_apply_launch_agent_with_no_paths_removes_plist_and_calls_bootout() {
        with_temp_home(|home| {
            with_path_override(|bin_dir| {
                let launchctl_log = home.join("launchctl.log");
                fs::create_dir_all(XcodeInstaller::plist_path().parent().unwrap()).unwrap();
                fs::write(XcodeInstaller::plist_path(), "stale plist").unwrap();
                write_launchctl_stub(bin_dir, 0, &launchctl_log);

                let result = XcodeInstaller::apply_launch_agent(&[]).unwrap();
                assert!(result.warning.is_none());
                assert!(!XcodeInstaller::plist_path().exists());

                let log = fs::read_to_string(launchctl_log).unwrap();
                assert!(log.contains("bootout"));
            });
        });
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[serial]
    fn test_apply_launch_agent_returns_warning_when_launchctl_fails() {
        with_temp_home(|home| {
            with_path_override(|bin_dir| {
                let workspace = home.join("ios");
                let launchctl_log = home.join("launchctl.log");
                fs::create_dir_all(&workspace).unwrap();
                fs::create_dir_all(XcodeInstaller::watcher_binary_path().parent().unwrap())
                    .unwrap();
                fs::write(XcodeInstaller::watcher_binary_path(), "#!/bin/sh\n").unwrap();
                #[cfg(unix)]
                fs::set_permissions(
                    XcodeInstaller::watcher_binary_path(),
                    fs::Permissions::from_mode(0o755),
                )
                .unwrap();
                write_launchctl_stub(bin_dir, 1, &launchctl_log);

                let result = XcodeInstaller::apply_launch_agent(&[workspace]).unwrap();
                assert!(
                    result
                        .warning
                        .as_deref()
                        .unwrap()
                        .contains("Unable to reload watcher automatically")
                );
                assert!(XcodeInstaller::plist_path().exists());
            });
        });
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[serial]
    fn test_apply_launch_agent_ignores_bootout_io_error_if_restart_succeeds() {
        with_temp_home(|home| {
            with_path_override(|bin_dir| {
                let workspace = home.join("ios");
                let launchctl_log = home.join("launchctl.log");
                fs::create_dir_all(&workspace).unwrap();
                fs::create_dir_all(XcodeInstaller::watcher_binary_path().parent().unwrap())
                    .unwrap();
                fs::write(XcodeInstaller::watcher_binary_path(), "#!/bin/sh\n").unwrap();
                #[cfg(unix)]
                fs::set_permissions(
                    XcodeInstaller::watcher_binary_path(),
                    fs::Permissions::from_mode(0o755),
                )
                .unwrap();
                write_launchctl_bootout_io_error_stub(bin_dir, &launchctl_log);

                let result = XcodeInstaller::apply_launch_agent(&[workspace]).unwrap();
                assert!(result.warning.is_none());
                assert!(XcodeInstaller::plist_path().exists());

                let log = fs::read_to_string(launchctl_log).unwrap();
                assert!(log.contains("bootout"));
                assert!(log.contains("bootstrap"));
                assert!(log.contains("kickstart"));
            });
        });
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[serial]
    fn test_install_extras_cleans_stale_launch_agent_when_no_paths_are_configured() {
        with_temp_home(|home| {
            if !XcodeInstaller::is_xcode_ide_available() {
                return;
            }

            with_path_override(|bin_dir| {
                let launchctl_log = home.join("launchctl.log");
                let installer = XcodeInstaller;

                fs::create_dir_all(XcodeInstaller::watcher_binary_path().parent().unwrap())
                    .unwrap();
                fs::write(XcodeInstaller::watcher_binary_path(), "#!/bin/sh\n").unwrap();
                #[cfg(unix)]
                fs::set_permissions(
                    XcodeInstaller::watcher_binary_path(),
                    fs::Permissions::from_mode(0o755),
                )
                .unwrap();
                fs::write(
                    XcodeInstaller::version_file_path(),
                    env!("CARGO_PKG_VERSION"),
                )
                .unwrap();
                fs::create_dir_all(XcodeInstaller::plist_path().parent().unwrap()).unwrap();
                fs::write(XcodeInstaller::plist_path(), "stale plist").unwrap();
                write_launchctl_stub(bin_dir, 0, &launchctl_log);

                let results = installer.install_extras(&test_params(), false).unwrap();

                assert!(results.iter().any(|result| {
                    !result.changed
                        && result
                            .message
                            .contains("Watcher LaunchAgent removed; no Xcode paths are configured")
                }));
                assert!(results.iter().any(|result| {
                    !result.changed && result.message.contains("git-ai xcode add-path")
                }));
                assert!(!XcodeInstaller::plist_path().exists());

                let log = fs::read_to_string(launchctl_log).unwrap();
                assert!(log.contains("bootout"));
            });
        });
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[serial]
    fn test_uninstall_extras_uses_bootout_for_launch_agent() {
        with_temp_home(|home| {
            with_path_override(|bin_dir| {
                let installer = XcodeInstaller;
                let plist_path = XcodeInstaller::plist_path();
                let launchctl_log = home.join("launchctl.log");

                fs::create_dir_all(plist_path.parent().unwrap()).unwrap();
                fs::write(&plist_path, "plist").unwrap();
                write_launchctl_stub(bin_dir, 0, &launchctl_log);

                let results = installer.uninstall_extras(&test_params(), false).unwrap();

                assert!(
                    results
                        .iter()
                        .any(|result| result.message.contains("launchd service unloaded"))
                );
                assert!(!plist_path.exists());

                let log = fs::read_to_string(launchctl_log).unwrap();
                assert!(log.contains("bootout"));
                assert!(!log.contains("unload"));
            });
        });
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[serial]
    fn test_install_extras_builds_binary_and_is_idempotent() {
        with_temp_home(|_| {
            if !XcodeInstaller::is_xcode_ide_available() {
                return;
            }

            let installer = XcodeInstaller;
            let first_results = installer.install_extras(&test_params(), false).unwrap();

            assert!(XcodeInstaller::watcher_binary_path().exists());
            assert!(XcodeInstaller::version_file_path().exists());
            assert!(XcodeInstaller::build_cache_dir().exists());
            assert!(first_results.iter().any(|result| {
                result.changed && result.message.contains("Watcher binary installed")
            }));
            assert!(first_results.iter().any(|result| {
                !result.changed && result.message.contains("git-ai xcode add-path")
            }));

            let check_result = installer.check_hooks(&test_params()).unwrap();
            assert!(check_result.tool_installed);
            assert!(check_result.hooks_installed);
            assert!(check_result.hooks_up_to_date);

            let second_results = installer.install_extras(&test_params(), false).unwrap();
            assert!(second_results.iter().any(|result| !result.changed
                && result.message.contains("already installed and up to date")));
            assert!(
                second_results
                    .iter()
                    .any(|result| !result.changed
                        && result.message.contains("git-ai xcode add-path"))
            );

            let uninstall_results = installer.uninstall_extras(&test_params(), false).unwrap();
            assert!(uninstall_results.iter().any(|result| {
                result.changed && result.message.contains("Watcher binary removed")
            }));
            assert!(!XcodeInstaller::watcher_binary_path().exists());
            assert!(!XcodeInstaller::version_file_path().exists());
            assert!(!XcodeInstaller::build_cache_dir().exists());
        });
    }
}
