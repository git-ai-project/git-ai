use crate::config::{NotesBackendConfig, NotesBackendKind};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};
use std::time::Duration;

const CACHE_VERSION: u32 = 1;
const CACHE_FILE_NAME: &str = "config.enterprise.json";
const FETCH_ENDPOINT: &str = "/worker/config/enterprise";

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(default)]
pub struct EnterpriseConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude_prompts_in_repositories: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_prompts_in_repositories: Option<Vec<String>>,
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
    pub prompt_storage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_prompt_storage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes_backend: Option<NotesBackendConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_streaming_lookback_days: Option<u32>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct EnterpriseConfigOption {
    pub key: &'static str,
    pub value_type: &'static str,
    pub enterprise_compatible: bool,
    pub label: &'static str,
    pub description: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enum_values: Option<&'static [&'static str]>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct EnterpriseConfigCache {
    version: u32,
    api_key_sha256: String,
    config: EnterpriseConfig,
}

#[derive(Debug, Clone)]
pub enum EnterpriseConfigFetchResult {
    Enabled(Box<EnterpriseConfig>),
    Disabled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnterpriseConfigFetchResponse {
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<EnterpriseConfig>,
}

#[derive(Debug, Clone)]
pub enum EnterpriseConfigBootstrapOutcome {
    Disabled,
    Fetched,
    CachedAfterError(String),
    NoApiKey,
}

#[derive(Debug, Clone)]
struct MemoryCache {
    api_key_sha256: String,
    config: EnterpriseConfig,
}

static ENTERPRISE_CONFIG_MEMORY: OnceLock<RwLock<Option<MemoryCache>>> = OnceLock::new();

pub fn enterprise_config_options() -> &'static [EnterpriseConfigOption] {
    &[
        EnterpriseConfigOption {
            key: "exclude_prompts_in_repositories",
            value_type: "string_array",
            enterprise_compatible: true,
            label: "Exclude prompts in repositories",
            description: "Repository patterns where prompts are stored locally only.",
            enum_values: None,
        },
        EnterpriseConfigOption {
            key: "include_prompts_in_repositories",
            value_type: "string_array",
            enterprise_compatible: true,
            label: "Include prompts in repositories",
            description: "Repository patterns where the primary prompt storage policy applies.",
            enum_values: None,
        },
        EnterpriseConfigOption {
            key: "allow_repositories",
            value_type: "string_array",
            enterprise_compatible: true,
            label: "Allow repositories",
            description: "Repository patterns where git-ai is allowed to run.",
            enum_values: None,
        },
        EnterpriseConfigOption {
            key: "exclude_repositories",
            value_type: "string_array",
            enterprise_compatible: true,
            label: "Exclude repositories",
            description: "Repository patterns where git-ai is disabled.",
            enum_values: None,
        },
        EnterpriseConfigOption {
            key: "telemetry_oss",
            value_type: "enum",
            enterprise_compatible: true,
            label: "OSS telemetry",
            description: "Controls OSS telemetry collection.",
            enum_values: Some(&["on", "off"]),
        },
        EnterpriseConfigOption {
            key: "telemetry_enterprise_dsn",
            value_type: "string",
            enterprise_compatible: true,
            label: "Enterprise telemetry DSN",
            description: "Sentry DSN for enterprise telemetry.",
            enum_values: None,
        },
        EnterpriseConfigOption {
            key: "disable_version_checks",
            value_type: "boolean",
            enterprise_compatible: true,
            label: "Disable version checks",
            description: "Disable CLI version check requests.",
            enum_values: None,
        },
        EnterpriseConfigOption {
            key: "disable_auto_updates",
            value_type: "boolean",
            enterprise_compatible: true,
            label: "Disable auto updates",
            description: "Disable daemon-triggered automatic updates.",
            enum_values: None,
        },
        EnterpriseConfigOption {
            key: "update_channel",
            value_type: "enum",
            enterprise_compatible: true,
            label: "Update channel",
            description: "Release channel used for update checks.",
            enum_values: Some(&["latest", "next", "enterprise-latest", "enterprise-next"]),
        },
        EnterpriseConfigOption {
            key: "prompt_storage",
            value_type: "enum",
            enterprise_compatible: true,
            label: "Prompt storage",
            description: "Primary prompt storage mode.",
            enum_values: Some(&["default", "notes", "local"]),
        },
        EnterpriseConfigOption {
            key: "default_prompt_storage",
            value_type: "enum",
            enterprise_compatible: true,
            label: "Default prompt storage",
            description: "Fallback prompt storage mode for repositories outside the include list.",
            enum_values: Some(&["default", "notes", "local"]),
        },
        EnterpriseConfigOption {
            key: "notes_backend",
            value_type: "object",
            enterprise_compatible: true,
            label: "Notes backend",
            description: "Backend used for authorship notes.",
            enum_values: None,
        },
        EnterpriseConfigOption {
            key: "transcript_streaming_lookback_days",
            value_type: "number",
            enterprise_compatible: true,
            label: "Transcript streaming lookback days",
            description: "Number of days transcript streaming should scan; 0 means unlimited.",
            enum_values: None,
        },
    ]
}

pub fn validate_enterprise_config_json(input: &str) -> Result<EnterpriseConfig, String> {
    let value: serde_json::Value =
        serde_json::from_str(input).map_err(|e| format!("invalid enterprise config: {e}"))?;
    reject_unknown_enterprise_config_keys(&value)?;
    let config: EnterpriseConfig =
        serde_json::from_value(value).map_err(|e| format!("invalid enterprise config: {e}"))?;
    validate_enterprise_config(config)
}

fn reject_unknown_enterprise_config_keys(value: &serde_json::Value) -> Result<(), String> {
    let obj = value
        .as_object()
        .ok_or_else(|| "enterprise config must be an object".to_string())?;
    for key in obj.keys() {
        if !enterprise_config_options()
            .iter()
            .any(|option| option.key == key)
        {
            return Err(format!("unknown enterprise config field: {key}"));
        }
    }
    Ok(())
}

pub fn validate_enterprise_config(config: EnterpriseConfig) -> Result<EnterpriseConfig, String> {
    validate_string_list(
        "exclude_prompts_in_repositories",
        &config.exclude_prompts_in_repositories,
    )?;
    validate_string_list(
        "include_prompts_in_repositories",
        &config.include_prompts_in_repositories,
    )?;
    validate_string_list("allow_repositories", &config.allow_repositories)?;
    validate_string_list("exclude_repositories", &config.exclude_repositories)?;

    if let Some(value) = config.telemetry_oss.as_deref()
        && !matches!(value, "on" | "off")
    {
        return Err("telemetry_oss must be 'on' or 'off'".to_string());
    }
    if let Some(value) = config.update_channel.as_deref()
        && !matches!(
            value,
            "latest" | "next" | "enterprise-latest" | "enterprise-next"
        )
    {
        return Err("update_channel is invalid".to_string());
    }
    if let Some(value) = config.prompt_storage.as_deref() {
        validate_prompt_storage("prompt_storage", value)?;
    }
    if let Some(value) = config.default_prompt_storage.as_deref() {
        validate_prompt_storage("default_prompt_storage", value)?;
    }
    if let Some(notes_backend) = &config.notes_backend
        && notes_backend.kind == NotesBackendKind::Http
        && notes_backend
            .backend_url
            .as_ref()
            .is_none_or(|value| value.trim().is_empty())
    {
        return Err("notes_backend.backend_url is required when kind is http".to_string());
    }

    Ok(config)
}

fn validate_string_list(key: &str, values: &Option<Vec<String>>) -> Result<(), String> {
    if let Some(values) = values {
        for value in values {
            if value.trim().is_empty() {
                return Err(format!("{key} cannot contain blank values"));
            }
        }
    }
    Ok(())
}

fn validate_prompt_storage(key: &str, value: &str) -> Result<(), String> {
    if matches!(value, "default" | "notes" | "local") {
        Ok(())
    } else {
        Err(format!("{key} must be 'default', 'notes', or 'local'"))
    }
}

pub fn enterprise_config_cache_path() -> Option<PathBuf> {
    crate::config::internal_dir_path().map(|dir| dir.join(CACHE_FILE_NAME))
}

pub fn effective_cached_config(api_key: Option<&str>) -> Option<EnterpriseConfig> {
    let api_key = api_key?;
    let fingerprint = api_key_fingerprint(api_key);

    if let Ok(guard) = memory_cache().read()
        && let Some(cache) = guard.as_ref()
        && cache.api_key_sha256 == fingerprint
    {
        return Some(cache.config.clone());
    }

    match load_cache_for_api_key(api_key) {
        Ok(config) => config,
        Err(e) => {
            tracing::debug!(error = %e, "failed to load enterprise config cache");
            None
        }
    }
}

pub fn load_cache_for_api_key(api_key: &str) -> Result<Option<EnterpriseConfig>, String> {
    let Some(path) = enterprise_config_cache_path() else {
        clear_memory_cache();
        return Ok(None);
    };

    let data = match fs::read(&path) {
        Ok(data) => data,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            clear_memory_cache();
            return Ok(None);
        }
        Err(e) => return Err(format!("failed to read enterprise config cache: {e}")),
    };

    let cache: EnterpriseConfigCache =
        serde_json::from_slice(&data).map_err(|e| format!("invalid enterprise cache: {e}"))?;
    let fingerprint = api_key_fingerprint(api_key);
    if cache.version != CACHE_VERSION || cache.api_key_sha256 != fingerprint {
        let _ = fs::remove_file(&path);
        clear_memory_cache();
        return Ok(None);
    }

    let config = validate_enterprise_config(cache.config)?;
    set_memory_cache(fingerprint, config.clone());
    Ok(Some(config))
}

pub fn save_cache_for_api_key(api_key: &str, config: &EnterpriseConfig) -> Result<(), String> {
    let path = enterprise_config_cache_path()
        .ok_or_else(|| "could not determine enterprise config cache path".to_string())?;
    let fingerprint = api_key_fingerprint(api_key);
    let config = validate_enterprise_config(config.clone())?;
    let cache = EnterpriseConfigCache {
        version: CACHE_VERSION,
        api_key_sha256: fingerprint.clone(),
        config: config.clone(),
    };
    atomic_write_json(&path, &cache)?;
    set_memory_cache(fingerprint, config);
    Ok(())
}

pub fn clear_cache() -> Result<(), String> {
    clear_memory_cache();
    if let Some(path) = enterprise_config_cache_path() {
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("failed to remove enterprise config cache: {e}")),
        }
    }
    Ok(())
}

fn atomic_write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("failed to create cache dir: {e}"))?;
    }
    let json = serde_json::to_vec_pretty(value)
        .map_err(|e| format!("failed to serialize enterprise cache: {e}"))?;
    let tmp_path = path.with_extension("json.tmp");
    fs::write(&tmp_path, json).map_err(|e| format!("failed to write enterprise cache: {e}"))?;
    fs::rename(&tmp_path, path).map_err(|e| format!("failed to replace enterprise cache: {e}"))?;
    Ok(())
}

pub fn apply_fetch_result(
    api_key: &str,
    result: EnterpriseConfigFetchResult,
) -> Result<EnterpriseConfigBootstrapOutcome, String> {
    match result {
        EnterpriseConfigFetchResult::Enabled(config) => {
            save_cache_for_api_key(api_key, &config)?;
            Ok(EnterpriseConfigBootstrapOutcome::Fetched)
        }
        EnterpriseConfigFetchResult::Disabled => {
            clear_cache()?;
            Ok(EnterpriseConfigBootstrapOutcome::Disabled)
        }
    }
}

pub fn bootstrap_enterprise_config(
    reason: &str,
    timeout: Duration,
) -> Result<EnterpriseConfigBootstrapOutcome, String> {
    let config = crate::config::Config::fresh();
    let Some(api_key) = config.api_key().map(str::to_string) else {
        clear_cache()?;
        return Ok(EnterpriseConfigBootstrapOutcome::NoApiKey);
    };

    match fetch_enterprise_config_once(timeout)
        .and_then(|result| apply_fetch_result(&api_key, result))
    {
        Ok(outcome) => Ok(outcome),
        Err(error) => match load_cache_for_api_key(&api_key) {
            Ok(Some(_)) => {
                tracing::warn!(%reason, %error, "enterprise config fetch failed; using cached config");
                Ok(EnterpriseConfigBootstrapOutcome::CachedAfterError(error))
            }
            Ok(None) => Err(format!(
                "enterprise config bootstrap failed during {reason}: {error}"
            )),
            Err(cache_error) => {
                tracing::debug!(%cache_error, "failed to load enterprise config cache after fetch failure");
                Err(format!(
                    "enterprise config bootstrap failed during {reason}: {error}"
                ))
            }
        },
    }
}

pub fn fetch_enterprise_config_once(
    timeout: Duration,
) -> Result<EnterpriseConfigFetchResult, String> {
    let cfg = crate::config::Config::fresh();
    if cfg.api_key().is_none() {
        return Err("api key is not configured".to_string());
    }

    let context = crate::api::client::ApiContext::new(None).with_timeout(timeout.as_secs().max(1));
    let client = crate::api::client::ApiClient::new(context);
    client.fetch_enterprise_config()
}

fn fetch_enterprise_config_with_retries(
    timeout: Duration,
    retries: usize,
) -> Result<EnterpriseConfigFetchResult, String> {
    let mut last_error = None;
    for attempt in 0..=retries {
        match fetch_enterprise_config_once(timeout) {
            Ok(result) => return Ok(result),
            Err(e) => {
                last_error = Some(e);
                if attempt < retries {
                    std::thread::sleep(Duration::from_secs(1 << attempt.min(3)));
                }
            }
        }
    }
    Err(last_error.unwrap_or_else(|| "enterprise config fetch failed".to_string()))
}

pub fn spawn_enterprise_config_worker(shutdown: std::sync::Arc<tokio::sync::Notify>) {
    tokio::spawn(async move {
        loop {
            let result = tokio::task::spawn_blocking(|| {
                let cfg = crate::config::Config::fresh();
                let Some(api_key) = cfg.api_key().map(str::to_string) else {
                    clear_cache()?;
                    return Ok(());
                };
                fetch_enterprise_config_with_retries(Duration::from_secs(30), 3)
                    .and_then(|result| apply_fetch_result(&api_key, result).map(|_| ()))
            })
            .await;

            if let Err(e) = result.unwrap_or_else(|e| Err(format!("worker panicked: {e}"))) {
                tracing::warn!(error = %e, "enterprise config refresh failed");
            }

            tokio::select! {
                _ = shutdown.notified() => break,
                _ = tokio::time::sleep(Duration::from_secs(30 * 60)) => {}
            }
        }
    });
}

fn api_key_fingerprint(api_key: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(api_key.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn memory_cache() -> &'static RwLock<Option<MemoryCache>> {
    ENTERPRISE_CONFIG_MEMORY.get_or_init(|| RwLock::new(None))
}

fn set_memory_cache(api_key_sha256: String, config: EnterpriseConfig) {
    if let Ok(mut guard) = memory_cache().write() {
        *guard = Some(MemoryCache {
            api_key_sha256,
            config,
        });
    }
}

fn clear_memory_cache() {
    if let Ok(mut guard) = memory_cache().write() {
        *guard = None;
    }
}

pub(crate) const FETCH_ENDPOINT_PATH: &str = FETCH_ENDPOINT;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_unknown_keys() {
        let err = validate_enterprise_config_json(r#"{"api_key":"secret"}"#).unwrap_err();
        assert!(err.contains("unknown enterprise config field"));
    }

    #[test]
    fn response_deserialization_ignores_unknown_keys() {
        let response: EnterpriseConfigFetchResponse = serde_json::from_str(
            r#"{"enabled":true,"config":{"prompt_storage":"local","future_setting":true}}"#,
        )
        .unwrap();
        let config = response.config.unwrap();
        assert_eq!(config.prompt_storage.as_deref(), Some("local"));
    }

    #[test]
    fn rejects_invalid_prompt_storage() {
        let err = validate_enterprise_config_json(r#"{"prompt_storage":"remote"}"#).unwrap_err();
        assert!(err.contains("prompt_storage"));
    }

    #[test]
    fn accepts_policy_config() {
        let cfg = validate_enterprise_config_json(
            r#"{"prompt_storage":"local","disable_auto_updates":true}"#,
        )
        .unwrap();
        assert_eq!(cfg.prompt_storage.as_deref(), Some("local"));
        assert_eq!(cfg.disable_auto_updates, Some(true));
    }
}
