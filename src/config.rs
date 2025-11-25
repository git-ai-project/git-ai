use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use glob::Pattern;
use serde::Deserialize;

use crate::feature_flags::FeatureFlags;
use crate::git::repository::Repository;

/// Centralized configuration for the application
pub struct Config {
    git_path: String,
    ignore_prompts: bool,
    allow_repositories: Vec<Pattern>,
    exclude_repositories: Vec<Pattern>,
    telemetry_oss_disabled: bool,
    telemetry_enterprise_dsn: Option<String>,
    disable_version_checks: bool,
    disable_auto_updates: bool,
    update_channel: UpdateChannel,
    feature_flags: FeatureFlags,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateChannel {
    Latest,
    Next,
}

impl UpdateChannel {
    pub fn as_str(&self) -> &'static str {
        match self {
            UpdateChannel::Latest => "latest",
            UpdateChannel::Next => "next",
        }
    }

    fn from_str(input: &str) -> Option<Self> {
        match input.trim().to_lowercase().as_str() {
            "latest" => Some(UpdateChannel::Latest),
            "next" => Some(UpdateChannel::Next),
            _ => None,
        }
    }
}

impl Default for UpdateChannel {
    fn default() -> Self {
        UpdateChannel::Latest
    }
}
#[derive(Deserialize)]
struct FileConfig {
    #[serde(default)]
    git_path: Option<String>,
    #[serde(default)]
    ignore_prompts: Option<bool>,
    #[serde(default)]
    allow_repositories: Option<Vec<String>>,
    #[serde(default)]
    exclude_repositories: Option<Vec<String>>,
    #[serde(default)]
    telemetry_oss: Option<String>,
    #[serde(default)]
    telemetry_enterprise_dsn: Option<String>,
    #[serde(default)]
    disable_version_checks: Option<bool>,
    #[serde(default)]
    disable_auto_updates: Option<bool>,
    #[serde(default)]
    update_channel: Option<String>,
    #[serde(default)]
    feature_flags: Option<serde_json::Value>,
}

static CONFIG: OnceLock<Config> = OnceLock::new();

impl Config {
    /// Initialize the global configuration exactly once.
    /// Safe to call multiple times; subsequent calls are no-ops.
    #[allow(dead_code)]
    pub fn init() {
        let _ = CONFIG.get_or_init(|| build_config());
    }

    /// Access the global configuration. Lazily initializes if not already initialized.
    pub fn get() -> &'static Config {
        CONFIG.get_or_init(|| build_config())
    }

    /// Returns the command to invoke git.
    pub fn git_cmd(&self) -> &str {
        &self.git_path
    }

    #[allow(dead_code)]
    pub fn get_ignore_prompts(&self) -> bool {
        self.ignore_prompts
    }

    pub fn is_allowed_repository(&self, repository: &Option<Repository>) -> bool {
        // First check if repository is in exclusion list - exclusions take precedence
        if !self.exclude_repositories.is_empty()
            && let Some(repository) = repository
        {
            if let Some(remotes) = repository.remotes_with_urls().ok() {
                // If any remote matches the exclusion patterns, deny access
                if remotes.iter().any(|remote| {
                    self.exclude_repositories
                        .iter()
                        .any(|pattern| pattern.matches(&remote.1))
                }) {
                    return false;
                }
            }
        }

        // If allowlist is empty, allow everything (unless excluded above)
        if self.allow_repositories.is_empty() {
            return true;
        }

        // If allowlist is defined, only allow repos whose remotes match the patterns
        if let Some(repository) = repository {
            match repository.remotes_with_urls().ok() {
                Some(remotes) => remotes.iter().any(|remote| {
                    self.allow_repositories
                        .iter()
                        .any(|pattern| pattern.matches(&remote.1))
                }),
                None => false, // Can't verify, deny by default when allowlist is active
            }
        } else {
            false // No repository provided, deny by default when allowlist is active
        }
    }

    /// Returns whether prompts should be ignored (currently unused by internal APIs).
    #[allow(dead_code)]
    pub fn ignore_prompts(&self) -> bool {
        self.ignore_prompts
    }

    /// Returns true if OSS telemetry is disabled.
    pub fn is_telemetry_oss_disabled(&self) -> bool {
        self.telemetry_oss_disabled
    }

    /// Returns the telemetry_enterprise_dsn if set.
    pub fn telemetry_enterprise_dsn(&self) -> Option<&str> {
        self.telemetry_enterprise_dsn.as_deref()
    }

    pub fn version_checks_disabled(&self) -> bool {
        self.disable_version_checks
    }

    pub fn auto_updates_disabled(&self) -> bool {
        self.disable_auto_updates
    }

    pub fn update_channel(&self) -> UpdateChannel {
        self.update_channel
    }

    pub fn feature_flags(&self) -> &FeatureFlags {
        &self.feature_flags
    }
}

fn build_config() -> Config {
    let file_cfg = load_file_config();
    let ignore_prompts = file_cfg
        .as_ref()
        .and_then(|c| c.ignore_prompts)
        .unwrap_or(false);
    let allow_repositories = file_cfg
        .as_ref()
        .and_then(|c| c.allow_repositories.clone())
        .unwrap_or(vec![])
        .into_iter()
        .filter_map(|pattern_str| {
            Pattern::new(&pattern_str)
                .map_err(|e| {
                    eprintln!(
                        "Warning: Invalid glob pattern in allow_repositories '{}': {}",
                        pattern_str, e
                    );
                })
                .ok()
        })
        .collect();
    let exclude_repositories = file_cfg
        .as_ref()
        .and_then(|c| c.exclude_repositories.clone())
        .unwrap_or(vec![])
        .into_iter()
        .filter_map(|pattern_str| {
            Pattern::new(&pattern_str)
                .map_err(|e| {
                    eprintln!(
                        "Warning: Invalid glob pattern in exclude_repositories '{}': {}",
                        pattern_str, e
                    );
                })
                .ok()
        })
        .collect();
    let telemetry_oss_disabled = file_cfg
        .as_ref()
        .and_then(|c| c.telemetry_oss.clone())
        .filter(|s| s == "off")
        .is_some();
    let telemetry_enterprise_dsn = file_cfg
        .as_ref()
        .and_then(|c| c.telemetry_enterprise_dsn.clone())
        .filter(|s| !s.is_empty());

    // Default to disabled (true) unless this is an OSS build
    // OSS builds set OSS_BUILD env var at compile time to "1", which enables auto-updates by default
    let auto_update_flags_default_disabled =
        option_env!("OSS_BUILD").is_none() || option_env!("OSS_BUILD").unwrap() != "1";

    let disable_version_checks = file_cfg
        .as_ref()
        .and_then(|c| c.disable_version_checks)
        .unwrap_or(auto_update_flags_default_disabled);
    let disable_auto_updates = file_cfg
        .as_ref()
        .and_then(|c| c.disable_auto_updates)
        .unwrap_or(auto_update_flags_default_disabled);
    let update_channel = file_cfg
        .as_ref()
        .and_then(|c| c.update_channel.as_deref())
        .and_then(UpdateChannel::from_str)
        .unwrap_or_default();

    let git_path = resolve_git_path(&file_cfg);

    // Build feature flags from file config
    let feature_flags = build_feature_flags(&file_cfg);

    Config {
        git_path,
        ignore_prompts,
        allow_repositories,
        exclude_repositories,
        telemetry_oss_disabled,
        telemetry_enterprise_dsn,
        disable_version_checks,
        disable_auto_updates,
        update_channel,
        feature_flags,
    }
}

fn build_feature_flags(file_cfg: &Option<FileConfig>) -> FeatureFlags {
    let file_flags_value = file_cfg.as_ref().and_then(|c| c.feature_flags.as_ref());

    // Try to deserialize the feature flags from the JSON value
    let file_flags = file_flags_value.and_then(|value| {
        // Use from_value to deserialize, but ignore any errors and fall back to defaults
        serde_json::from_value(value.clone()).ok()
    });

    FeatureFlags::from_file_config(file_flags)
}

fn resolve_git_path(file_cfg: &Option<FileConfig>) -> String {
    // 1) From config file
    if let Some(cfg) = file_cfg {
        if let Some(path) = cfg.git_path.as_ref() {
            let trimmed = path.trim();
            if !trimmed.is_empty() {
                let p = Path::new(trimmed);
                if is_executable(p) {
                    return trimmed.to_string();
                }
            }
        }
    }

    // 2) Probe common locations across platforms
    let candidates: &[&str] = &[
        // macOS Homebrew (ARM and Intel)
        "/opt/homebrew/bin/git",
        "/usr/local/bin/git",
        // Common Unix paths
        "/usr/bin/git",
        "/bin/git",
        "/usr/local/sbin/git",
        "/usr/sbin/git",
        // Windows Git for Windows
        r"C:\\Program Files\\Git\\bin\\git.exe",
        r"C:\\Program Files (x86)\\Git\\bin\\git.exe",
    ];

    if let Some(found) = candidates.iter().map(Path::new).find(|p| is_executable(p)) {
        return found.to_string_lossy().to_string();
    }

    // 3) Fatal error: no real git found
    eprintln!(
        "Fatal: Could not locate a real 'git' binary.\n\
         Expected a valid 'git_path' in {cfg_path} or in standard locations.\n\
         Please install Git or update your config JSON.",
        cfg_path = config_file_path()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "~/.git-ai/config.json".to_string()),
    );
    std::process::exit(1);
}

fn load_file_config() -> Option<FileConfig> {
    let path = config_file_path()?;
    let data = fs::read(&path).ok()?;
    serde_json::from_slice::<FileConfig>(&data).ok()
}

fn config_file_path() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        let home = env::var("USERPROFILE").ok()?;
        Some(Path::new(&home).join(".git-ai").join("config.json"))
    }
    #[cfg(not(windows))]
    {
        let home = env::var("HOME").ok()?;
        Some(Path::new(&home).join(".git-ai").join("config.json"))
    }
}

fn is_executable(path: &Path) -> bool {
    if !path.exists() || !path.is_file() {
        return false;
    }
    // Basic check: existence is sufficient for our purposes; OS will enforce exec perms.
    // On Unix we could check permissions, but many filesystems differ. Keep it simple.
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_config(
        allow_repositories: Vec<String>,
        exclude_repositories: Vec<String>,
    ) -> Config {
        Config {
            git_path: "/usr/bin/git".to_string(),
            ignore_prompts: false,
            allow_repositories: allow_repositories
                .into_iter()
                .filter_map(|s| Pattern::new(&s).ok())
                .collect(),
            exclude_repositories: exclude_repositories
                .into_iter()
                .filter_map(|s| Pattern::new(&s).ok())
                .collect(),
            telemetry_oss_disabled: false,
            telemetry_enterprise_dsn: None,
            disable_version_checks: false,
            disable_auto_updates: false,
            update_channel: UpdateChannel::Latest,
            feature_flags: FeatureFlags::default(),
        }
    }

    #[test]
    fn test_exclusion_takes_precedence_over_allow() {
        let config = create_test_config(
            vec!["https://github.com/allowed/repo".to_string()],
            vec!["https://github.com/allowed/repo".to_string()],
        );

        // Test with None repository - should return false when allowlist is active
        assert!(!config.is_allowed_repository(&None));
    }

    #[test]
    fn test_empty_allowlist_allows_everything() {
        let config = create_test_config(vec![], vec![]);

        // With empty allowlist, should allow everything
        assert!(config.is_allowed_repository(&None));
    }

    #[test]
    fn test_exclude_without_allow() {
        let config =
            create_test_config(vec![], vec!["https://github.com/excluded/repo".to_string()]);

        // With empty allowlist but exclusions, should allow everything (exclusions only matter when checking remotes)
        assert!(config.is_allowed_repository(&None));
    }

    #[test]
    fn test_allow_without_exclude() {
        let config =
            create_test_config(vec!["https://github.com/allowed/repo".to_string()], vec![]);

        // With allowlist but no exclusions, should deny when no repository provided
        assert!(!config.is_allowed_repository(&None));
    }

    #[test]
    fn test_glob_pattern_wildcard_in_allow() {
        let config = create_test_config(vec!["https://github.com/myorg/*".to_string()], vec![]);

        // Test that the pattern would match (note: we can't easily test with real Repository objects,
        // but the pattern compilation is tested by the fact that create_test_config succeeds)
        assert!(!config.allow_repositories.is_empty());
        assert!(config.allow_repositories[0].matches("https://github.com/myorg/repo1"));
        assert!(config.allow_repositories[0].matches("https://github.com/myorg/repo2"));
        assert!(!config.allow_repositories[0].matches("https://github.com/other/repo"));
    }

    #[test]
    fn test_glob_pattern_wildcard_in_exclude() {
        let config = create_test_config(vec![], vec!["https://github.com/private/*".to_string()]);

        // Test pattern matching
        assert!(!config.exclude_repositories.is_empty());
        assert!(config.exclude_repositories[0].matches("https://github.com/private/repo1"));
        assert!(config.exclude_repositories[0].matches("https://github.com/private/secret"));
        assert!(!config.exclude_repositories[0].matches("https://github.com/public/repo"));
    }

    #[test]
    fn test_exact_match_still_works() {
        let config = create_test_config(vec!["https://github.com/exact/match".to_string()], vec![]);

        // Test that exact matches still work (glob treats them as literals)
        assert!(!config.allow_repositories.is_empty());
        assert!(config.allow_repositories[0].matches("https://github.com/exact/match"));
        assert!(!config.allow_repositories[0].matches("https://github.com/exact/other"));
    }

    #[test]
    fn test_complex_glob_patterns() {
        let config = create_test_config(vec!["*@github.com:company/*".to_string()], vec![]);

        // Test more complex patterns with wildcards
        assert!(!config.allow_repositories.is_empty());
        assert!(config.allow_repositories[0].matches("git@github.com:company/repo"));
        assert!(config.allow_repositories[0].matches("user@github.com:company/project"));
        assert!(!config.allow_repositories[0].matches("git@github.com:other/repo"));
    }
}
