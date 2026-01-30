use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use dirs;
use uuid::Uuid;

use glob::Pattern;
use serde::{Deserialize, Serialize};

use crate::feature_flags::FeatureFlags;
use crate::git::repository::Repository;

#[cfg(any(test, feature = "test-support"))]
use std::sync::RwLock;

/// Default API base URL for comparison
pub const DEFAULT_API_BASE_URL: &str = "https://usegitai.com";

pub struct Config {
    git_path: String,
    exclude_prompts_in_repositories: Vec<Pattern>,
    allow_repositories: Vec<Pattern>,
    exclude_repositories: Vec<Pattern>,
    telemetry_oss_disabled: bool,
    telemetry_enterprise_dsn: Option<String>,
    disable_version_checks: bool,
    disable_auto_updates: bool,
    update_channel: UpdateChannel,
    feature_flags: FeatureFlags,
    api_base_url: String,
    prompt_storage: String,
    api_key: Option<String>,
    quiet: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateChannel {
    Latest,
    Next,
    EnterpriseLatest,
    EnterpriseNext,
}

impl UpdateChannel {
    pub fn as_str(&self) -> &'static str {
        match self {
            UpdateChannel::Latest => "latest",
            UpdateChannel::Next => "next",
            UpdateChannel::EnterpriseLatest => "enterprise-latest",
            UpdateChannel::EnterpriseNext => "enterprise-next",
        }
    }

    fn from_str(input: &str) -> Option<Self> {
        match input.trim().to_lowercase().as_str() {
            "latest" => Some(UpdateChannel::Latest),
            "next" => Some(UpdateChannel::Next),
            "enterprise-latest" => Some(UpdateChannel::EnterpriseLatest),
            "enterprise-next" => Some(UpdateChannel::EnterpriseNext),
            _ => None,
        }
    }
}

impl Default for UpdateChannel {
    fn default() -> Self {
        UpdateChannel::Latest
    }
}
#[derive(Deserialize, Serialize, Default)]
pub struct FileConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude_prompts_in_repositories: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_repositories: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude_repositories: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub telemetry_oss: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub telemetry_enterprise_dsn: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disable_version_checks: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disable_auto_updates: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_channel: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feature_flags: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_storage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quiet: Option<bool>,
}

static CONFIG: OnceLock<Config> = OnceLock::new();

#[cfg(any(test, feature = "test-support"))]
static TEST_FEATURE_FLAGS_OVERRIDE: RwLock<Option<FeatureFlags>> = RwLock::new(None);

/// Serializable config patch for test overrides
/// All fields are optional to allow patching only specific properties
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ConfigPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude_prompts_in_repositories: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub telemetry_oss_disabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disable_version_checks: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disable_auto_updates: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_storage: Option<String>,
}

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

    pub fn is_allowed_repository(&self, repository: &Option<Repository>) -> bool {
        // Fetch remotes once and reuse for both exclude and allow checks
        let remotes = repository
            .as_ref()
            .and_then(|repo| repo.remotes_with_urls().ok());

        self.is_allowed_repository_with_remotes(remotes.as_ref())
    }

    /// Helper that accepts pre-fetched remotes to avoid multiple git operations
    fn is_allowed_repository_with_remotes(&self, remotes: Option<&Vec<(String, String)>>) -> bool {
        // First check if repository is in exclusion list - exclusions take precedence
        if !self.exclude_repositories.is_empty() {
            if let Some(remotes) = remotes {
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
        match remotes {
            Some(remotes) => remotes.iter().any(|remote| {
                self.allow_repositories
                    .iter()
                    .any(|pattern| pattern.matches(&remote.1))
            }),
            None => false, // Can't verify, deny by default when allowlist is active
        }
    }

    /// Returns true if prompts should be excluded (not shared) for the given repository.
    /// This uses a blacklist model: empty list = share everywhere, patterns = repos to exclude.
    /// Local repositories (no remotes) are only excluded if wildcard "*" pattern is present.
    pub fn should_exclude_prompts(&self, repository: &Option<Repository>) -> bool {
        // Empty exclusion list = never exclude
        if self.exclude_prompts_in_repositories.is_empty() {
            return false;
        }

        // Check for wildcard "*" pattern - excludes ALL repos including local
        let has_wildcard = self
            .exclude_prompts_in_repositories
            .iter()
            .any(|pattern| pattern.as_str() == "*");
        if has_wildcard {
            return true;
        }

        // Fetch remotes
        let remotes = repository
            .as_ref()
            .and_then(|repo| repo.remotes_with_urls().ok());

        match remotes {
            Some(remotes) => {
                if remotes.is_empty() {
                    // No remotes = local-only repo, not excluded (unless wildcard, handled above)
                    false
                } else {
                    // Has remotes - check if any match exclusion patterns
                    remotes.iter().any(|remote| {
                        self.exclude_prompts_in_repositories
                            .iter()
                            .any(|pattern| pattern.matches(&remote.1))
                    })
                }
            }
            None => false, // Can't get remotes = don't exclude
        }
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

    /// Returns the API base URL
    pub fn api_base_url(&self) -> &str {
        &self.api_base_url
    }

    /// Returns the prompt storage mode: "default", "notes", or "local"
    /// - "default": Messages uploaded via CAS API
    /// - "notes": Messages stored in git notes
    /// - "local": Messages only stored in sqlite (not in notes, not uploaded)
    pub fn prompt_storage(&self) -> &str {
        &self.prompt_storage
    }

    /// Returns the API key if configured
    pub fn api_key(&self) -> Option<&str> {
        self.api_key.as_deref()
    }

    /// Returns true if quiet mode is enabled (suppresses chart output after commits)
    pub fn is_quiet(&self) -> bool {
        self.quiet
    }

    /// Override feature flags for testing purposes.
    /// Only available when the `test-support` feature is enabled or in test mode.
    /// Must be `pub` to work with integration tests in the `tests/` directory.
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_test_feature_flags(flags: FeatureFlags) {
        let mut override_flags = TEST_FEATURE_FLAGS_OVERRIDE
            .write()
            .expect("Failed to acquire write lock on test feature flags");
        *override_flags = Some(flags);
    }

    /// Clear any feature flag overrides.
    /// Only available when the `test-support` feature is enabled or in test mode.
    /// This should be called in test cleanup to reset to default behavior.
    #[cfg(any(test, feature = "test-support"))]
    pub fn clear_test_feature_flags() {
        let mut override_flags = TEST_FEATURE_FLAGS_OVERRIDE
            .write()
            .expect("Failed to acquire write lock on test feature flags");
        *override_flags = None;
    }

    /// Get feature flags, checking for test overrides first.
    /// In test mode, this will return overridden flags if set, otherwise the normal flags.
    #[cfg(any(test, feature = "test-support"))]
    pub fn get_feature_flags(&self) -> FeatureFlags {
        let override_flags = TEST_FEATURE_FLAGS_OVERRIDE
            .read()
            .expect("Failed to acquire read lock on test feature flags");
        override_flags
            .clone()
            .unwrap_or_else(|| self.feature_flags.clone())
    }

    /// Get feature flags (non-test version, just returns a reference).
    #[cfg(not(any(test, feature = "test-support")))]
    pub fn get_feature_flags(&self) -> &FeatureFlags {
        &self.feature_flags
    }
}

fn build_config() -> Config {
    let file_cfg = load_file_config();
    let exclude_prompts_in_repositories = file_cfg
        .as_ref()
        .and_then(|c| c.exclude_prompts_in_repositories.clone())
        .unwrap_or(vec![])
        .into_iter()
        .filter_map(|pattern_str| {
            Pattern::new(&pattern_str)
                .map_err(|e| {
                    eprintln!(
                        "Warning: Invalid glob pattern in exclude_prompts_in_repositories '{}': {}",
                        pattern_str, e
                    );
                })
                .ok()
        })
        .collect();
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

    // Get API base URL from config, env var, or default
    let api_base_url = file_cfg
        .as_ref()
        .and_then(|c| c.api_base_url.clone())
        .or_else(|| env::var("GIT_AI_API_BASE_URL").ok())
        .unwrap_or_else(|| DEFAULT_API_BASE_URL.to_string());

    // Get prompt_storage setting (defaults to "default")
    // Valid values: "default", "notes", "local"
    let prompt_storage = file_cfg
        .as_ref()
        .and_then(|c| c.prompt_storage.clone())
        .unwrap_or_else(|| "default".to_string());
    let prompt_storage = match prompt_storage.as_str() {
        "default" | "notes" | "local" => prompt_storage,
        other => {
            eprintln!(
                "Warning: Invalid prompt_storage value '{}', using 'default'",
                other
            );
            "default".to_string()
        }
    };

    // Get API key from env var or config file (env var takes precedence)
    let api_key = env::var("GIT_AI_API_KEY")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            file_cfg
                .as_ref()
                .and_then(|c| c.api_key.clone())
                .filter(|s| !s.is_empty())
        });

    // Get quiet setting (defaults to false)
    let quiet = file_cfg
        .as_ref()
        .and_then(|c| c.quiet)
        .unwrap_or(false);

    #[cfg(any(test, feature = "test-support"))]
    {
        let mut config = Config {
            git_path,
            exclude_prompts_in_repositories,
            allow_repositories,
            exclude_repositories,
            telemetry_oss_disabled,
            telemetry_enterprise_dsn,
            disable_version_checks,
            disable_auto_updates,
            update_channel,
            feature_flags,
            api_base_url,
            prompt_storage,
            api_key,
            quiet,
        };
        apply_test_config_patch(&mut config);
        config
    }

    #[cfg(not(any(test, feature = "test-support")))]
    Config {
        git_path,
        exclude_prompts_in_repositories,
        allow_repositories,
        exclude_repositories,
        telemetry_oss_disabled,
        telemetry_enterprise_dsn,
        disable_version_checks,
        disable_auto_updates,
        update_channel,
        feature_flags,
        api_base_url,
        prompt_storage,
        api_key,
        quiet,
    }
}

fn build_feature_flags(file_cfg: &Option<FileConfig>) -> FeatureFlags {
    let file_flags_value = file_cfg.as_ref().and_then(|c| c.feature_flags.as_ref());

    // Try to deserialize the feature flags from the JSON value
    let file_flags = file_flags_value.and_then(|value| {
        // Use from_value to deserialize, but ignore any errors and fall back to defaults
        serde_json::from_value(value.clone()).ok()
    });

    FeatureFlags::from_env_and_file(file_flags)
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
    let home = dirs::home_dir()?;
    Some(home.join(".git-ai").join("config.json"))
}

/// Public accessor for config file path
pub fn config_file_path_public() -> Option<PathBuf> {
    config_file_path()
}

/// Returns the path to the git-ai base directory (~/.git-ai)
pub fn git_ai_dir_path() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        let home = env::var("USERPROFILE").ok()?;
        Some(Path::new(&home).join(".git-ai"))
    }
    #[cfg(not(windows))]
    {
        let home = env::var("HOME").ok()?;
        Some(Path::new(&home).join(".git-ai"))
    }
}

/// Returns the path to the internal state directory (~/.git-ai/internal)
/// This is where git-ai stores internal files like distinct_id, update_check, etc.
pub fn internal_dir_path() -> Option<PathBuf> {
    git_ai_dir_path().map(|dir| dir.join("internal"))
}

/// Returns the path to the skills directory (~/.git-ai/skills)
/// This is where git-ai installs skills for Claude Code and other agents
pub fn skills_dir_path() -> Option<PathBuf> {
    git_ai_dir_path().map(|dir| dir.join("skills"))
}

/// Public accessor for ID file path (~/.git-ai/internal/distinct_id)
pub fn id_file_path() -> Option<PathBuf> {
    internal_dir_path().map(|dir| dir.join("distinct_id"))
}

/// Cache for the distinct_id to avoid repeated file reads
static DISTINCT_ID: OnceLock<String> = OnceLock::new();

/// Get or create the distinct_id (UUID) from ~/.git-ai/internal/distinct_id
/// If the file doesn't exist, generates a new UUID and writes it to the file.
/// The result is cached for the lifetime of the process.
pub fn get_or_create_distinct_id() -> String {
    DISTINCT_ID
        .get_or_init(|| {
            let id_path = match id_file_path() {
                Some(path) => path,
                None => return "unknown".to_string(),
            };

            // Try to read existing ID
            if let Ok(existing_id) = fs::read_to_string(&id_path) {
                let trimmed = existing_id.trim();
                if !trimmed.is_empty() {
                    return trimmed.to_string();
                }
            }

            // Generate new UUID
            let new_id = Uuid::new_v4().to_string();

            // Ensure directory exists
            if let Some(parent) = id_path.parent() {
                let _ = fs::create_dir_all(parent);
            }

            // Write the new ID to file
            if let Err(e) = fs::write(&id_path, &new_id) {
                eprintln!("Warning: Failed to write distinct_id file: {}", e);
            }

            new_id
        })
        .clone()
}

/// Returns the path to the update check cache file (~/.git-ai/internal/update_check)
pub fn update_check_path() -> Option<PathBuf> {
    internal_dir_path().map(|dir| dir.join("update_check"))
}

/// Load the raw file config
pub fn load_file_config_public() -> Result<FileConfig, String> {
    let path =
        config_file_path().ok_or_else(|| "Could not determine config file path".to_string())?;

    if !path.exists() {
        // Return empty config if file doesn't exist
        return Ok(FileConfig::default());
    }

    let data = fs::read(&path).map_err(|e| format!("Failed to read config file: {}", e))?;

    serde_json::from_slice::<FileConfig>(&data)
        .map_err(|e| format!("Failed to parse config file: {}", e))
}

/// Save the file config
pub fn save_file_config(config: &FileConfig) -> Result<(), String> {
    let path =
        config_file_path().ok_or_else(|| "Could not determine config file path".to_string())?;

    // Ensure the directory exists
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create config directory: {}", e))?;
    }

    let json = serde_json::to_string_pretty(config)
        .map_err(|e| format!("Failed to serialize config: {}", e))?;

    fs::write(&path, json).map_err(|e| format!("Failed to write config file: {}", e))
}

fn is_executable(path: &Path) -> bool {
    if !path.exists() || !path.is_file() {
        return false;
    }
    // Basic check: existence is sufficient for our purposes; OS will enforce exec perms.
    // On Unix we could check permissions, but many filesystems differ. Keep it simple.
    true
}

/// Apply test config patch from environment variable (test-only)
/// Reads GIT_AI_TEST_CONFIG_PATCH env var containing JSON and applies patches to config
#[cfg(any(test, feature = "test-support"))]
fn apply_test_config_patch(config: &mut Config) {
    if let Ok(patch_json) = env::var("GIT_AI_TEST_CONFIG_PATCH") {
        if let Ok(patch) = serde_json::from_str::<ConfigPatch>(&patch_json) {
            if let Some(patterns) = patch.exclude_prompts_in_repositories {
                config.exclude_prompts_in_repositories = patterns
                    .into_iter()
                    .filter_map(|pattern_str| {
                        Pattern::new(&pattern_str)
                            .map_err(|e| {
                                eprintln!(
                                    "Warning: Invalid test pattern in exclude_prompts_in_repositories '{}': {}",
                                    pattern_str, e
                                );
                            })
                            .ok()
                    })
                    .collect();
            }
            if let Some(telemetry_oss_disabled) = patch.telemetry_oss_disabled {
                config.telemetry_oss_disabled = telemetry_oss_disabled;
            }
            if let Some(disable_version_checks) = patch.disable_version_checks {
                config.disable_version_checks = disable_version_checks;
            }
            if let Some(disable_auto_updates) = patch.disable_auto_updates {
                config.disable_auto_updates = disable_auto_updates;
            }
            if let Some(prompt_storage) = patch.prompt_storage {
                // Validate the value
                if matches!(prompt_storage.as_str(), "default" | "notes" | "local") {
                    config.prompt_storage = prompt_storage;
                } else {
                    eprintln!(
                        "Warning: Invalid test prompt_storage value '{}', ignoring",
                        prompt_storage
                    );
                }
            }
        }
    }
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
            exclude_prompts_in_repositories: vec![],
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
            api_base_url: DEFAULT_API_BASE_URL.to_string(),
            prompt_storage: "default".to_string(),
            api_key: None,
            quiet: false,
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

    // Tests for exclude_prompts_in_repositories (blacklist)

    fn create_test_config_with_exclude_prompts(exclude_prompts_patterns: Vec<String>) -> Config {
        Config {
            git_path: "/usr/bin/git".to_string(),
            exclude_prompts_in_repositories: exclude_prompts_patterns
                .into_iter()
                .filter_map(|s| Pattern::new(&s).ok())
                .collect(),
            allow_repositories: vec![],
            exclude_repositories: vec![],
            telemetry_oss_disabled: false,
            telemetry_enterprise_dsn: None,
            disable_version_checks: false,
            disable_auto_updates: false,
            update_channel: UpdateChannel::Latest,
            feature_flags: FeatureFlags::default(),
            api_base_url: DEFAULT_API_BASE_URL.to_string(),
            prompt_storage: "default".to_string(),
            api_key: None,
            quiet: false,
        }
    }

    #[test]
    fn test_should_exclude_prompts_empty_patterns_returns_false() {
        let config = create_test_config_with_exclude_prompts(vec![]);

        // Empty patterns = share everywhere (blacklist model)
        assert!(!config.should_exclude_prompts(&None));
    }

    #[test]
    fn test_should_exclude_prompts_no_repository_returns_false() {
        let config =
            create_test_config_with_exclude_prompts(vec!["https://github.com/*".to_string()]);

        // Even with patterns, no repository provided = don't exclude (can't verify)
        assert!(!config.should_exclude_prompts(&None));
    }

    #[test]
    fn test_should_exclude_prompts_pattern_matching() {
        let config =
            create_test_config_with_exclude_prompts(vec!["https://github.com/myorg/*".to_string()]);

        // Test that pattern is compiled correctly
        assert!(!config.exclude_prompts_in_repositories.is_empty());
        assert!(config.exclude_prompts_in_repositories[0].matches("https://github.com/myorg/repo1"));
        assert!(config.exclude_prompts_in_repositories[0].matches("https://github.com/myorg/repo2"));
        assert!(!config.exclude_prompts_in_repositories[0].matches("https://github.com/other/repo"));
    }

    #[test]
    fn test_should_exclude_prompts_wildcard_all() {
        let config = create_test_config_with_exclude_prompts(vec!["*".to_string()]);

        // Wildcard * should match any remote URL pattern (exclude all)
        assert!(!config.exclude_prompts_in_repositories.is_empty());
        assert!(config.exclude_prompts_in_repositories[0].matches("https://github.com/any/repo"));
        assert!(config.exclude_prompts_in_repositories[0].matches("git@gitlab.com:any/project"));

        // Wildcard * should also exclude repos without remotes (None case)
        assert!(config.should_exclude_prompts(&None));
    }

    #[test]
    fn test_should_exclude_prompts_local_repo_not_excluded_without_wildcard() {
        // Test 1: Local repo with no patterns configured - never excluded
        let config_no_patterns = create_test_config_with_exclude_prompts(vec![]);
        assert!(!config_no_patterns.should_exclude_prompts(&None));

        // Test 2: Local repo with non-wildcard patterns - not excluded
        // (patterns only match against remotes, local repos have none)
        let config_with_patterns =
            create_test_config_with_exclude_prompts(vec!["https://github.com/*".to_string()]);
        assert!(
            config_with_patterns.exclude_prompts_in_repositories[0]
                .matches("https://github.com/myorg/repo")
        );
        // Non-wildcard patterns should NOT exclude repos without remotes
        assert!(!config_with_patterns.should_exclude_prompts(&None));
    }

    #[test]
    fn test_should_exclude_prompts_respects_patterns_when_remotes_exist() {
        let config =
            create_test_config_with_exclude_prompts(vec!["https://github.com/private/*".to_string()]);

        // Pattern should match private repos (to exclude)
        assert!(config.exclude_prompts_in_repositories[0].matches("https://github.com/private/repo"));
        // Pattern should not match other repos
        assert!(!config.exclude_prompts_in_repositories[0].matches("https://github.com/public/repo"));
    }

    #[test]
    fn test_update_channel_default_is_latest() {
        let channel = UpdateChannel::default();
        assert_eq!(channel, UpdateChannel::Latest);
        assert_eq!(channel.as_str(), "latest");
    }

    #[test]
    fn test_update_channel_enterprise_latest_maps_to_enterprise_latest() {
        let channel = UpdateChannel::from_str("enterprise-latest").unwrap();
        assert_eq!(channel, UpdateChannel::EnterpriseLatest);
        assert_eq!(channel.as_str(), "enterprise-latest");
    }

    #[test]
    fn test_update_channel_enterprise_next_maps_to_enterprise_next() {
        let channel = UpdateChannel::from_str("enterprise-next").unwrap();
        assert_eq!(channel, UpdateChannel::EnterpriseNext);
        assert_eq!(channel.as_str(), "enterprise-next");
    }

    #[test]
    fn test_update_channel_enterprise_latest_parses() {
        let channel = UpdateChannel::from_str("enterprise-latest").unwrap();
        assert_eq!(channel, UpdateChannel::EnterpriseLatest);
        assert_eq!(channel.as_str(), "enterprise-latest");
    }

    #[test]
    fn test_update_channel_enterprise_next_parses() {
        let channel = UpdateChannel::from_str("enterprise-next").unwrap();
        assert_eq!(channel, UpdateChannel::EnterpriseNext);
        assert_eq!(channel.as_str(), "enterprise-next");
    }

    #[test]
    fn test_quiet_default_is_false() {
        let config = create_test_config(vec![], vec![]);
        assert!(!config.is_quiet());
    }

    #[test]
    fn test_quiet_can_be_enabled() {
        let mut config = create_test_config(vec![], vec![]);
        config.quiet = true;
        assert!(config.is_quiet());
    }
}
