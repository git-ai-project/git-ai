use dirs;
use serde_json::Value;

use crate::git::repository::find_repository_in_path;

macro_rules! set_string_field {
    ($file_config:expr, $field:ident, $key:literal, $value:expr) => {{
        $file_config.$field = Some($value.to_string());
        crate::config::save_file_config(&$file_config)?;
        eprintln!("[{}]: {}", $key, $value);
    }};
}

macro_rules! set_bool_field {
    ($file_config:expr, $field:ident, $key:literal, $value:expr) => {{
        let bool_value = parse_bool($value)?;
        $file_config.$field = Some(bool_value);
        crate::config::save_file_config(&$file_config)?;
        eprintln!("[{}]: {}", $key, bool_value);
    }};
}

macro_rules! unset_field {
    ($file_config:expr, $field:ident, $key:literal) => {{
        let old_value = $file_config.$field.take();
        crate::config::save_file_config(&$file_config)?;
        if let Some(v) = old_value {
            eprintln!("- [{}]: {}", $key, v);
        }
    }};
}

macro_rules! opt_string_value {
    ($value:expr) => {{
        if let Some(v) = $value {
            Value::String(v.clone())
        } else {
            Value::Null
        }
    }};
}

macro_rules! vec_or_empty {
    ($value:expr) => {{
        if let Some(v) = $value {
            serde_json::to_value(v).unwrap_or(Value::Array(vec![]))
        } else {
            Value::Array(vec![])
        }
    }};
}

/// Single source of truth for scalar config keys.
///
/// Each entry defines:
/// - external CLI key
/// - FileConfig field name
/// - parser/validation strategy for `set`
/// - expression for `get` / `show` effective value
macro_rules! scalar_config_entries {
    ($m:ident) => {
        $m!("git_path", git_path, [string], get_git_path_value);
        $m!(
            "telemetry_enterprise_dsn",
            telemetry_enterprise_dsn,
            [string],
            get_telemetry_enterprise_dsn_value
        );
        $m!(
            "disable_version_checks",
            disable_version_checks,
            [bool],
            get_disable_version_checks_value
        );
        $m!(
            "disable_auto_updates",
            disable_auto_updates,
            [bool],
            get_disable_auto_updates_value
        );
        $m!(
            "update_channel",
            update_channel,
            [validated_string(validate_update_channel_value)],
            get_update_channel_value
        );
        $m!(
            "prompt_storage",
            prompt_storage,
            [validated_string(validate_prompt_storage_value)],
            get_prompt_storage_value
        );
        $m!(
            "default_prompt_storage",
            default_prompt_storage,
            [validated_string(validate_prompt_storage_value)],
            get_default_prompt_storage_value
        );
        $m!("quiet", quiet, [bool], get_quiet_value);
    };
}

macro_rules! repository_array_entries {
    ($m:ident) => {
        $m!(
            "exclude_prompts_in_repositories",
            exclude_prompts_in_repositories
        );
        $m!("allow_repositories", allow_repositories);
        $m!("exclude_repositories", exclude_repositories);
    };
}

/// Determines the type of pattern value provided
#[derive(Debug, PartialEq)]
enum PatternType {
    /// Global wildcard pattern like "*"
    GlobalWildcard,
    /// URL or git protocol (http://, https://, git@, ssh://, etc.)
    UrlOrGitProtocol,
    /// File path that should be resolved to a repository
    FilePath,
}

/// Detect the type of pattern value
fn detect_pattern_type(value: &str) -> PatternType {
    let trimmed = value.trim();

    // Check for global wildcard
    if trimmed == "*" {
        return PatternType::GlobalWildcard;
    }

    // Check for URL or git protocol patterns
    if trimmed.starts_with("http://")
        || trimmed.starts_with("https://")
        || trimmed.starts_with("git@")
        || trimmed.starts_with("ssh://")
        || trimmed.starts_with("git://")
        || trimmed.contains("://")
        || (trimmed.contains('@') && trimmed.contains(':') && !trimmed.starts_with('/'))
    {
        return PatternType::UrlOrGitProtocol;
    }

    // Check for glob patterns with wildcards (but not just "*")
    // These are patterns like "https://github.com/org/*" or "*@github.com:*"
    if trimmed.contains('*') || trimmed.contains('?') || trimmed.contains('[') {
        return PatternType::UrlOrGitProtocol;
    }

    // Otherwise, treat as file path
    PatternType::FilePath
}

/// Resolve a file path to repository remote URLs
/// Returns the remote URLs for the repository at the given path
fn resolve_path_to_remotes(path: &str) -> Result<Vec<String>, String> {
    // Expand ~ to home directory
    let expanded_path = if path.starts_with("~/") {
        if let Some(home) = dirs::home_dir() {
            format!("{}{}", home.to_string_lossy(), &path[1..])
        } else {
            path.to_string()
        }
    } else {
        path.to_string()
    };

    // Try to find repository at path
    let repo = find_repository_in_path(&expanded_path).map_err(|_| {
        format!(
            "No git repository found at path '{}'. Provide a valid repository path, URL, or glob pattern.",
            path
        )
    })?;

    // Get remotes with URLs
    let remotes = repo
        .remotes_with_urls()
        .map_err(|e| format!("Failed to get remotes for repository at '{}': {}", path, e))?;

    if remotes.is_empty() {
        return Err(format!(
            "Repository at '{}' has no remotes configured. Add a remote first or use a glob pattern.",
            path
        ));
    }

    // Return all remote URLs
    Ok(remotes.into_iter().map(|(_, url)| url).collect())
}

fn print_config_help() {
    eprintln!("git-ai config - View and manage git-ai configuration");
    eprintln!();
    eprintln!("Usage:");
    eprintln!("  git-ai config                Show all config as formatted JSON");
    eprintln!("  git-ai config <key>          Show specific config value");
    eprintln!("  git-ai config set <key> <value>          Set a config value");
    eprintln!("  git-ai config set <key> <value> --add    Add to array (extends existing)");
    eprintln!("  git-ai config --add <key> <value>        Add to array or upsert into object");
    eprintln!("  git-ai config unset <key>    Remove config value (reverts to default)");
    eprintln!();
    eprintln!("Configuration Keys:");
    eprintln!("  git_path                     Path to git binary");
    eprintln!("  exclude_prompts_in_repositories  Repos to exclude prompts from (array)");
    eprintln!("  allow_repositories           Allowed repos (array)");
    eprintln!("  exclude_repositories         Excluded repos (array)");
    eprintln!("  telemetry_oss                OSS telemetry setting (on/off)");
    eprintln!("  telemetry_enterprise_dsn     Enterprise telemetry DSN");
    eprintln!("  disable_version_checks       Disable version checks (bool)");
    eprintln!("  disable_auto_updates         Disable auto updates (bool)");
    eprintln!("  update_channel               Update channel (latest/next)");
    eprintln!("  feature_flags                Feature flags (object)");
    eprintln!("  api_key                      API key for X-API-Key header");
    eprintln!("  prompt_storage               Prompt storage mode (default/notes/local)");
    eprintln!("  include_prompts_in_repositories  Repos to include for prompt storage (array)");
    eprintln!("  default_prompt_storage       Fallback storage mode for non-included repos");
    eprintln!("  quiet                        Suppress chart output after commits (bool)");
    eprintln!();
    eprintln!("Repository Patterns:");
    eprintln!("  For exclude/allow/exclude_prompts_in_repositories, you can provide:");
    eprintln!("    - A glob pattern: \"*\", \"https://github.com/org/*\"");
    eprintln!("    - A URL/git protocol: \"git@github.com:org/repo.git\"");
    eprintln!("    - A file path: \".\" or \"/path/to/repo\" (resolves to repo's remotes)");
    eprintln!();
    eprintln!("Examples:");
    eprintln!("  git-ai config exclude_repositories");
    eprintln!("  git-ai config set disable_auto_updates true");
    eprintln!("  git-ai config set exclude_repositories \"private/*\"");
    eprintln!("  git-ai config set exclude_repositories .         # Uses current repo's remotes");
    eprintln!("  git-ai config --add exclude_repositories \"temp/*\"");
    eprintln!("  git-ai config --add allow_repositories ~/projects/my-repo");
    eprintln!("  git-ai config --add feature_flags.my_flag true");
    eprintln!("  git-ai config unset exclude_repositories");
    eprintln!();
    std::process::exit(0);
}

pub fn handle_config(args: &[String]) {
    if args.is_empty() {
        // Show all config
        if let Err(e) = show_all_config() {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
        return;
    }

    // Check for help flags
    if args[0] == "--help" || args[0] == "-h" || args[0] == "help" {
        print_config_help();
        return;
    }

    // Check for --add flag anywhere in args
    let is_add_mode = args.iter().any(|a| a == "--add");
    let filtered_args: Vec<&String> = args.iter().filter(|a| *a != "--add").collect();

    if filtered_args.is_empty() {
        // Show all config if only --add was passed (which doesn't make sense)
        eprintln!("Error: --add requires <key> <value>");
        eprintln!("Usage: git-ai config --add <key> <value>");
        eprintln!("   or: git-ai config set <key> <value> --add");
        std::process::exit(1);
    }

    match filtered_args[0].as_str() {
        "set" => {
            if filtered_args.len() < 3 {
                eprintln!("Error: set requires <key> <value>");
                eprintln!("Usage: git-ai config set <key> <value>");
                std::process::exit(1);
            }
            let key = filtered_args[1].as_str();
            let value = filtered_args[2].as_str();
            if let Err(e) = set_config_value(key, value, is_add_mode) {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        }
        "unset" => {
            if filtered_args.len() < 2 {
                eprintln!("Error: unset requires <key>");
                eprintln!("Usage: git-ai config unset <key>");
                std::process::exit(1);
            }
            let key = filtered_args[1].as_str();
            if let Err(e) = unset_config_value(key) {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        }
        key => {
            if is_add_mode {
                // git-ai config --add <key> <value>
                if filtered_args.len() < 2 {
                    eprintln!("Error: --add requires <key> <value>");
                    eprintln!("Usage: git-ai config --add <key> <value>");
                    std::process::exit(1);
                }
                let value = filtered_args[1].as_str();
                if let Err(e) = set_config_value(key, value, true) {
                    eprintln!("Error: {}", e);
                    std::process::exit(1);
                }
            } else {
                // Get single value
                if let Err(e) = get_config_value(key) {
                    eprintln!("Error: {}", e);
                    std::process::exit(1);
                }
            }
        }
    }
}

fn get_git_path_value(_: &crate::config::FileConfig, runtime: &crate::config::Config) -> Value {
    Value::String(runtime.git_cmd().to_string())
}

fn get_telemetry_enterprise_dsn_value(
    file: &crate::config::FileConfig,
    _: &crate::config::Config,
) -> Value {
    opt_string_value!(file.telemetry_enterprise_dsn.as_ref())
}

fn get_disable_version_checks_value(
    _: &crate::config::FileConfig,
    runtime: &crate::config::Config,
) -> Value {
    Value::Bool(runtime.version_checks_disabled())
}

fn get_disable_auto_updates_value(
    _: &crate::config::FileConfig,
    runtime: &crate::config::Config,
) -> Value {
    Value::Bool(runtime.auto_updates_disabled())
}

fn get_update_channel_value(
    _: &crate::config::FileConfig,
    runtime: &crate::config::Config,
) -> Value {
    Value::String(runtime.update_channel().as_str().to_string())
}

fn get_prompt_storage_value(
    _: &crate::config::FileConfig,
    runtime: &crate::config::Config,
) -> Value {
    Value::String(runtime.prompt_storage().to_string())
}

fn get_default_prompt_storage_value(
    file: &crate::config::FileConfig,
    _: &crate::config::Config,
) -> Value {
    opt_string_value!(file.default_prompt_storage.as_ref())
}

fn get_quiet_value(_: &crate::config::FileConfig, runtime: &crate::config::Config) -> Value {
    Value::Bool(runtime.is_quiet())
}

fn get_feature_flags_value(runtime: &crate::config::Config) -> Value {
    serde_json::to_value(runtime.get_feature_flags())
        .unwrap_or_else(|_| Value::Object(serde_json::Map::new()))
}

fn get_masked_api_key_value(file: &crate::config::FileConfig) -> Value {
    if let Some(key) = &file.api_key {
        Value::String(mask_api_key(key))
    } else {
        Value::Null
    }
}

fn insert_repository_array_values(
    effective_config: &mut serde_json::Map<String, Value>,
    file_config: &crate::config::FileConfig,
) {
    macro_rules! insert_repo_array {
        ($entry_key:literal, $field:ident) => {
            effective_config.insert(
                $entry_key.to_string(),
                vec_or_empty!(file_config.$field.as_ref()),
            );
        };
    }

    repository_array_entries!(insert_repo_array);
}

fn try_get_repository_array_value(
    key: &str,
    file_config: &crate::config::FileConfig,
) -> Option<Value> {
    macro_rules! get_repo_array {
        ($entry_key:literal, $field:ident) => {
            if key == $entry_key {
                return Some(vec_or_empty!(file_config.$field.as_ref()));
            }
        };
    }

    repository_array_entries!(get_repo_array);
    None
}

fn try_set_repository_array_value(
    key: &str,
    value: &str,
    add_mode: bool,
    file_config: &mut crate::config::FileConfig,
) -> Result<bool, String> {
    macro_rules! set_repo_array {
        ($entry_key:literal, $field:ident) => {
            if key == $entry_key {
                let added = set_repository_array_field(&mut file_config.$field, value, add_mode)?;
                crate::config::save_file_config(file_config)?;
                log_array_changes(&added, add_mode);
                return Ok(true);
            }
        };
    }

    repository_array_entries!(set_repo_array);
    Ok(false)
}

fn try_unset_repository_array_value(
    key: &str,
    file_config: &mut crate::config::FileConfig,
) -> Result<bool, String> {
    macro_rules! unset_repo_array {
        ($entry_key:literal, $field:ident) => {
            if key == $entry_key {
                let old_values = file_config.$field.take();
                crate::config::save_file_config(file_config)?;
                if let Some(items) = old_values {
                    log_array_removals(&items);
                }
                return Ok(true);
            }
        };
    }

    repository_array_entries!(unset_repo_array);
    Ok(false)
}

fn set_include_prompts_in_repositories(
    file_config: &mut crate::config::FileConfig,
    value: &str,
    add_mode: bool,
) -> Result<(), String> {
    let resolved = resolve_repository_value(value)?;
    if add_mode {
        let mut list = file_config
            .include_prompts_in_repositories
            .take()
            .unwrap_or_default();
        for pattern in &resolved {
            if !list.contains(pattern) {
                list.push(pattern.clone());
            }
        }
        file_config.include_prompts_in_repositories = Some(list);
    } else {
        file_config.include_prompts_in_repositories = Some(resolved.clone());
    }
    crate::config::save_file_config(file_config)?;
    for pattern in resolved {
        eprintln!("[include_prompts_in_repositories]: {}", pattern);
    }
    Ok(())
}

fn set_feature_flags_top_level(
    file_config: &mut crate::config::FileConfig,
    value: &str,
    add_mode: bool,
) -> Result<(), String> {
    if add_mode {
        return Err(
            "Cannot use --add with feature_flags at top level. Use dot notation: feature_flags.key"
                .to_string(),
        );
    }

    let json_value: Value = serde_json::from_str(value)
        .map_err(|e| format!("Invalid JSON for feature_flags: {}", e))?;
    if !json_value.is_object() {
        return Err("feature_flags must be a JSON object".to_string());
    }

    file_config.feature_flags = Some(json_value);
    crate::config::save_file_config(file_config)?;
    eprintln!("[feature_flags]: {}", value);
    Ok(())
}

fn set_feature_flags_nested(
    file_config: &mut crate::config::FileConfig,
    key_path: &[String],
    value: &str,
) -> Result<(), String> {
    if key_path.len() < 2 {
        return Err(
            "feature_flags requires a nested key (e.g., feature_flags.some_flag)".to_string(),
        );
    }

    let mut flags = file_config
        .feature_flags
        .take()
        .unwrap_or_else(|| Value::Object(serde_json::Map::new()));

    if !flags.is_object() {
        return Err("feature_flags must be a JSON object".to_string());
    }

    let flags_obj = flags
        .as_object_mut()
        .ok_or_else(|| "feature_flags must be a JSON object".to_string())?;

    let nested_key = key_path[1..].join(".");
    let parsed_value = parse_value(value)?;

    if key_path.len() == 2 {
        flags_obj.insert(key_path[1].clone(), parsed_value);
    } else {
        let mut current = flags_obj;
        for segment in &key_path[1..key_path.len() - 1] {
            current = current
                .entry(segment.clone())
                .or_insert_with(|| Value::Object(serde_json::Map::new()))
                .as_object_mut()
                .ok_or_else(|| format!("Cannot navigate through non-object at {}", segment))?;
        }
        current.insert(key_path.last().unwrap().clone(), parsed_value);
    }

    file_config.feature_flags = Some(flags);
    crate::config::save_file_config(file_config)?;
    eprintln!("+ [{}]: {}", nested_key, value);
    Ok(())
}

fn unset_feature_flags_nested(
    file_config: &mut crate::config::FileConfig,
    key_path: &[String],
    full_key: &str,
) -> Result<(), String> {
    if key_path.len() < 2 {
        return Err(
            "feature_flags requires a nested key (e.g., feature_flags.some_flag)".to_string(),
        );
    }

    let mut flags = file_config
        .feature_flags
        .take()
        .ok_or_else(|| format!("Config key not found: {}", full_key))?;

    if !flags.is_object() {
        return Err("feature_flags must be a JSON object".to_string());
    }

    let flags_obj = flags
        .as_object_mut()
        .ok_or_else(|| "feature_flags must be a JSON object".to_string())?;
    let nested_key = key_path[1..].join(".");

    let removed = if key_path.len() == 2 {
        flags_obj.remove(&key_path[1])
    } else {
        let mut current = flags_obj;
        for segment in &key_path[1..key_path.len() - 1] {
            current = current
                .get_mut(segment)
                .and_then(|v| v.as_object_mut())
                .ok_or_else(|| format!("Config key not found: {}", full_key))?;
        }
        current.remove(key_path.last().unwrap())
    };

    let old_value = removed.ok_or_else(|| format!("Config key not found: {}", full_key))?;
    file_config.feature_flags = Some(flags);
    crate::config::save_file_config(file_config)?;
    eprintln!("- [{}]: {}", nested_key, old_value);
    Ok(())
}

fn try_get_scalar_config_value(
    key: &str,
    file_config: &crate::config::FileConfig,
    runtime_config: &crate::config::Config,
) -> Option<Value> {
    macro_rules! scalar_get_if_arm {
        ($entry_key:literal, $field:ident, [$($kind:tt)+], $getter:ident) => {
            if key == $entry_key {
                return Some($getter(file_config, runtime_config));
            }
        };
    }

    scalar_config_entries!(scalar_get_if_arm);
    None
}

fn try_set_scalar_config_value(
    key: &str,
    value: &str,
    file_config: &mut crate::config::FileConfig,
) -> Result<bool, String> {
    macro_rules! scalar_set_if_arm {
        ($entry_key:literal, $field:ident, [string], $getter:ident) => {
            if key == $entry_key {
                set_string_field!(file_config, $field, $entry_key, value);
                return Ok(true);
            }
        };
        ($entry_key:literal, $field:ident, [bool], $getter:ident) => {
            if key == $entry_key {
                set_bool_field!(file_config, $field, $entry_key, value);
                return Ok(true);
            }
        };
        ($entry_key:literal, $field:ident, [validated_string($validator:path)], $getter:ident) => {
            if key == $entry_key {
                $validator(value)?;
                set_string_field!(file_config, $field, $entry_key, value);
                return Ok(true);
            }
        };
    }

    scalar_config_entries!(scalar_set_if_arm);
    Ok(false)
}

fn try_unset_scalar_config_value(
    key: &str,
    file_config: &mut crate::config::FileConfig,
) -> Result<bool, String> {
    macro_rules! scalar_unset_if_arm {
        ($entry_key:literal, $field:ident, [$($kind:tt)+], $getter:ident) => {
            if key == $entry_key {
                unset_field!(file_config, $field, $entry_key);
                return Ok(true);
            }
        };
    }

    scalar_config_entries!(scalar_unset_if_arm);
    Ok(false)
}

fn show_all_config() -> Result<(), String> {
    let file_config = crate::config::load_file_config_public()?;

    // Build a complete effective config representation
    let mut effective_config = serde_json::Map::new();

    // Get the actual runtime config
    let runtime_config = crate::config::Config::get();

    // Arrays
    insert_repository_array_values(&mut effective_config, &file_config);

    // Booleans with runtime values
    effective_config.insert(
        "telemetry_oss_disabled".to_string(),
        Value::Bool(runtime_config.is_telemetry_oss_disabled()),
    );
    // Scalar keys generated from schema
    macro_rules! scalar_show_insert_arm {
        ($entry_key:literal, $field:ident, [$($kind:tt)+], $getter:ident) => {
            effective_config.insert(
                $entry_key.to_string(),
                $getter(&file_config, runtime_config),
            );
        };
    }
    scalar_config_entries!(scalar_show_insert_arm);

    // include_prompts_in_repositories
    if file_config.include_prompts_in_repositories.is_some() {
        effective_config.insert(
            "include_prompts_in_repositories".to_string(),
            vec_or_empty!(file_config.include_prompts_in_repositories.as_ref()),
        );
    }

    // `default_prompt_storage` and `quiet` are included via scalar_config_entries!.

    // Feature flags - show effective flags with defaults applied
    effective_config.insert(
        "feature_flags".to_string(),
        get_feature_flags_value(runtime_config),
    );

    // API key - show masked value if set
    if file_config.api_key.is_some() {
        effective_config.insert(
            "api_key".to_string(),
            get_masked_api_key_value(&file_config),
        );
    }

    let json = serde_json::to_string_pretty(&effective_config)
        .map_err(|e| format!("Failed to serialize config: {}", e))?;

    println!("{}", json);
    Ok(())
}

fn get_config_value(key: &str) -> Result<(), String> {
    let file_config = crate::config::load_file_config_public()?;
    let runtime_config = crate::config::Config::get();

    let key_path = parse_key_path(key);

    // Handle top-level keys
    if key_path.len() == 1 {
        if let Some(value) =
            try_get_scalar_config_value(key_path[0].as_str(), &file_config, runtime_config)
        {
            let json = serde_json::to_string_pretty(&value)
                .map_err(|e| format!("Failed to serialize value: {}", e))?;
            println!("{}", json);
            return Ok(());
        }

        if let Some(value) = try_get_repository_array_value(key_path[0].as_str(), &file_config) {
            let json = serde_json::to_string_pretty(&value)
                .map_err(|e| format!("Failed to serialize value: {}", e))?;
            println!("{}", json);
            return Ok(());
        }

        let value = match key_path[0].as_str() {
            "telemetry_oss_disabled" => Value::Bool(runtime_config.is_telemetry_oss_disabled()),
            "feature_flags" => get_feature_flags_value(runtime_config),
            "api_key" => get_masked_api_key_value(&file_config),
            "include_prompts_in_repositories" => {
                vec_or_empty!(file_config.include_prompts_in_repositories.as_ref())
            }
            _ => return Err(format!("Unknown config key: {}", key)),
        };

        let json = serde_json::to_string_pretty(&value)
            .map_err(|e| format!("Failed to serialize value: {}", e))?;
        println!("{}", json);
        return Ok(());
    }

    // Handle nested keys (dot notation)
    if key_path[0] == "feature_flags" {
        let feature_flags = get_feature_flags_value(runtime_config);
        let mut current = &feature_flags;
        for segment in &key_path[1..] {
            current = current
                .get(segment)
                .ok_or_else(|| format!("Config key not found: {}", key))?;
        }

        let json = serde_json::to_string_pretty(current)
            .map_err(|e| format!("Failed to serialize value: {}", e))?;
        println!("{}", json);
        return Ok(());
    }

    Err("Nested keys are only supported for feature_flags".to_string())
}

fn set_config_value(key: &str, value: &str, add_mode: bool) -> Result<(), String> {
    let mut file_config = crate::config::load_file_config_public()?;
    let key_path = parse_key_path(key);

    // Handle top-level keys
    if key_path.len() == 1 {
        if try_set_scalar_config_value(key_path[0].as_str(), value, &mut file_config)? {
            return Ok(());
        }

        if try_set_repository_array_value(key_path[0].as_str(), value, add_mode, &mut file_config)?
        {
            return Ok(());
        }

        match key_path[0].as_str() {
            "telemetry_oss" => {
                set_string_field!(file_config, telemetry_oss, "telemetry_oss", value);
            }
            "feature_flags" => {
                set_feature_flags_top_level(&mut file_config, value, add_mode)?;
            }
            "api_key" => {
                file_config.api_key = Some(value.to_string());
                crate::config::save_file_config(&file_config)?;
                let masked = mask_api_key(value);
                eprintln!("[api_key]: {}", masked);
            }
            "include_prompts_in_repositories" => {
                set_include_prompts_in_repositories(&mut file_config, value, add_mode)?;
            }
            _ => return Err(format!("Unknown config key: {}", key)),
        }

        return Ok(());
    }

    // Handle nested keys (dot notation) - only for feature_flags
    if key_path[0] == "feature_flags" {
        set_feature_flags_nested(&mut file_config, &key_path, value)?;
        return Ok(());
    }

    Err("Nested keys are only supported for feature_flags".to_string())
}

fn unset_config_value(key: &str) -> Result<(), String> {
    let mut file_config = crate::config::load_file_config_public()?;
    let key_path = parse_key_path(key);

    // Handle top-level keys
    if key_path.len() == 1 {
        if try_unset_scalar_config_value(key_path[0].as_str(), &mut file_config)? {
            return Ok(());
        }

        if try_unset_repository_array_value(key_path[0].as_str(), &mut file_config)? {
            return Ok(());
        }

        match key_path[0].as_str() {
            "telemetry_oss" => {
                unset_field!(file_config, telemetry_oss, "telemetry_oss");
            }
            "feature_flags" => {
                let old_value = file_config.feature_flags.take();
                crate::config::save_file_config(&file_config)?;
                if let Some(v) = old_value {
                    eprintln!("- [feature_flags]: {}", v);
                }
            }
            "api_key" => {
                let old_value = file_config.api_key.take();
                crate::config::save_file_config(&file_config)?;
                if old_value.is_some() {
                    eprintln!("- [api_key]: ****");
                }
            }
            "include_prompts_in_repositories" => {
                let old_value = file_config.include_prompts_in_repositories.take();
                crate::config::save_file_config(&file_config)?;
                if let Some(v) = old_value {
                    eprintln!("- [include_prompts_in_repositories]: {:?}", v);
                }
            }
            _ => return Err(format!("Unknown config key: {}", key)),
        }

        return Ok(());
    }

    // Handle nested keys (dot notation) - only for feature_flags
    if key_path[0] == "feature_flags" {
        unset_feature_flags_nested(&mut file_config, &key_path, key)?;

        return Ok(());
    }

    Err("Nested keys are only supported for feature_flags".to_string())
}

fn parse_key_path(key: &str) -> Vec<String> {
    key.split('.').map(|s| s.to_string()).collect()
}

/// Set array field for repository patterns (exclude_repositories, allow_repositories, exclude_prompts_in_repositories)
/// This function handles the special logic of detecting if a value is:
///  - A global wildcard pattern like "*"
///  - A URL or git protocol pattern
///  - A file path that should be resolved to repository remotes
///
/// Returns the values that were added/set for logging purposes
fn set_repository_array_field(
    field: &mut Option<Vec<String>>,
    value: &str,
    add_mode: bool,
) -> Result<Vec<String>, String> {
    // Resolve the value(s) to add
    let values_to_add = resolve_repository_value(value)?;

    if add_mode {
        // Add mode: append to existing array
        let mut arr = field.take().unwrap_or_default();
        let added = values_to_add.clone();
        arr.extend(values_to_add);
        *field = Some(arr);
        Ok(added)
    } else {
        // Set mode: try to parse as JSON array, or use resolved values
        if value.starts_with('[') {
            // Parse as JSON array
            let json_value: Value =
                serde_json::from_str(value).map_err(|e| format!("Invalid JSON array: {}", e))?;
            if let Value::Array(arr) = json_value {
                let mut resolved_values = Vec::new();
                for v in arr {
                    if let Value::String(s) = v {
                        let resolved = resolve_repository_value(&s)?;
                        resolved_values.extend(resolved);
                    } else {
                        return Err("Array must contain only strings".to_string());
                    }
                }
                let added = resolved_values.clone();
                *field = Some(resolved_values);
                Ok(added)
            } else {
                Err("Expected a JSON array".to_string())
            }
        } else {
            // Single value - use the resolved values
            let added = values_to_add.clone();
            *field = Some(values_to_add);
            Ok(added)
        }
    }
}

/// Resolve a repository value - returns the actual patterns to store
/// For file paths, resolves to repository remote URLs
/// For URLs/patterns, returns as-is
fn resolve_repository_value(value: &str) -> Result<Vec<String>, String> {
    match detect_pattern_type(value) {
        PatternType::GlobalWildcard | PatternType::UrlOrGitProtocol => {
            // Return as-is
            Ok(vec![value.to_string()])
        }
        PatternType::FilePath => {
            // Resolve to repository remote URLs
            resolve_path_to_remotes(value)
        }
    }
}

/// Log array changes with + prefix for add mode, or just list items for set mode
fn log_array_changes(items: &[String], add_mode: bool) {
    #[allow(clippy::if_same_then_else)]
    if add_mode {
        for item in items {
            eprintln!("+ {}", item);
        }
    } else {
        for item in items {
            eprintln!("+ {}", item);
        }
    }
}

/// Log array removals with - prefix
fn log_array_removals(items: &[String]) {
    for item in items {
        eprintln!("- {}", item);
    }
}

fn parse_bool(value: &str) -> Result<bool, String> {
    match value.to_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => Ok(true),
        "false" | "0" | "no" | "off" => Ok(false),
        _ => Err(format!(
            "Invalid boolean value: '{}'. Expected true/false",
            value
        )),
    }
}

fn parse_value(value: &str) -> Result<Value, String> {
    // Try to parse as JSON first
    if let Ok(json_value) = serde_json::from_str::<Value>(value) {
        return Ok(json_value);
    }

    // Otherwise treat as string
    Ok(Value::String(value.to_string()))
}

/// Mask an API key for display (show first 4 and last 4 chars if long enough)
fn mask_api_key(key: &str) -> String {
    if key.len() > 8 {
        format!("{}...{}", &key[..4], &key[key.len() - 4..])
    } else {
        "****".to_string()
    }
}

/// Validate prompt_storage value
fn validate_prompt_storage_value(value: &str) -> Result<(), String> {
    if value != "default" && value != "notes" && value != "local" {
        return Err(format!(
            "Invalid prompt_storage value '{}'. Expected 'default', 'notes', or 'local'",
            value
        ));
    }
    Ok(())
}

/// Validate update_channel value
fn validate_update_channel_value(value: &str) -> Result<(), String> {
    if value != "latest" && value != "next" {
        return Err("Invalid update_channel value. Expected 'latest' or 'next'".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_prompt_storage_valid_values() {
        for value in ["default", "notes", "local"] {
            let result = validate_prompt_storage_value(value);
            assert!(result.is_ok(), "Expected '{}' to be valid", value);
        }
    }

    #[test]
    fn test_prompt_storage_invalid_value() {
        for value in ["invalid", "defaults", "note", "", "DEFAULT", "NOTES"] {
            let result = validate_prompt_storage_value(value);
            assert!(result.is_err(), "Expected '{}' to be invalid", value);
        }
    }

    #[test]
    fn test_prompt_storage_invalid_value_error_message() {
        let result = validate_prompt_storage_value("invalid");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("invalid"));
        assert!(err.contains("default"));
        assert!(err.contains("notes"));
        assert!(err.contains("local"));
    }

    #[test]
    fn test_parse_bool_valid_true_values() {
        for value in ["true", "1", "yes", "on", "TRUE", "True", "YES", "ON"] {
            let result = parse_bool(value);
            assert!(result.is_ok(), "Expected '{}' to parse as bool", value);
            assert!(result.unwrap(), "Expected '{}' to be true", value);
        }
    }

    #[test]
    fn test_parse_bool_valid_false_values() {
        for value in ["false", "0", "no", "off", "FALSE", "False", "NO", "OFF"] {
            let result = parse_bool(value);
            assert!(result.is_ok(), "Expected '{}' to parse as bool", value);
            assert!(!result.unwrap(), "Expected '{}' to be false", value);
        }
    }

    #[test]
    fn test_parse_bool_invalid_value() {
        let result = parse_bool("invalid");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("Invalid boolean value"));
        assert!(err.contains("invalid"));
    }

    // --- Additional comprehensive tests ---

    #[test]
    fn test_parse_value_json_string() {
        let result = parse_value("\"hello\"").unwrap();
        assert_eq!(result, Value::String("hello".to_string()));
    }

    #[test]
    fn test_parse_value_json_number() {
        let result = parse_value("42").unwrap();
        assert_eq!(result, Value::Number(serde_json::Number::from(42)));
    }

    #[test]
    fn test_parse_value_json_boolean() {
        let result = parse_value("true").unwrap();
        assert_eq!(result, Value::Bool(true));
    }

    #[test]
    fn test_parse_value_json_array() {
        let result = parse_value("[1,2,3]").unwrap();
        assert!(result.is_array());
        let arr = result.as_array().unwrap();
        assert_eq!(arr.len(), 3);
    }

    #[test]
    fn test_parse_value_json_object() {
        let result = parse_value(r#"{"key":"value"}"#).unwrap();
        assert!(result.is_object());
    }

    #[test]
    fn test_parse_value_plain_string() {
        let result = parse_value("plain text").unwrap();
        assert_eq!(result, Value::String("plain text".to_string()));
    }

    #[test]
    fn test_mask_api_key_long() {
        let key = "abcdefghijklmnop";
        let masked = mask_api_key(key);
        assert_eq!(masked, "abcd...mnop");
    }

    #[test]
    fn test_mask_api_key_short() {
        let key = "short";
        let masked = mask_api_key(key);
        assert_eq!(masked, "****");
    }

    #[test]
    fn test_mask_api_key_exactly_eight() {
        let key = "12345678";
        let masked = mask_api_key(key);
        assert_eq!(masked, "****");
    }

    #[test]
    fn test_mask_api_key_nine_chars() {
        let key = "123456789";
        let masked = mask_api_key(key);
        assert_eq!(masked, "1234...6789");
    }

    #[test]
    fn test_parse_key_path_single() {
        let result = parse_key_path("key");
        assert_eq!(result, vec!["key"]);
    }

    #[test]
    fn test_parse_key_path_nested() {
        let result = parse_key_path("parent.child");
        assert_eq!(result, vec!["parent", "child"]);
    }

    #[test]
    fn test_parse_key_path_deeply_nested() {
        let result = parse_key_path("a.b.c.d");
        assert_eq!(result, vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn test_parse_key_path_empty() {
        let result = parse_key_path("");
        assert_eq!(result, vec![""]);
    }

    #[test]
    fn test_detect_pattern_type_global_wildcard() {
        assert_eq!(detect_pattern_type("*"), PatternType::GlobalWildcard);
        assert_eq!(detect_pattern_type(" * "), PatternType::GlobalWildcard);
    }

    #[test]
    fn test_detect_pattern_type_http_url() {
        assert_eq!(
            detect_pattern_type("http://github.com/org/repo"),
            PatternType::UrlOrGitProtocol
        );
        assert_eq!(
            detect_pattern_type("https://github.com/org/repo"),
            PatternType::UrlOrGitProtocol
        );
    }

    #[test]
    fn test_detect_pattern_type_git_ssh() {
        assert_eq!(
            detect_pattern_type("git@github.com:org/repo.git"),
            PatternType::UrlOrGitProtocol
        );
    }

    #[test]
    fn test_detect_pattern_type_ssh_url() {
        assert_eq!(
            detect_pattern_type("ssh://git@github.com/org/repo"),
            PatternType::UrlOrGitProtocol
        );
    }

    #[test]
    fn test_detect_pattern_type_git_protocol() {
        assert_eq!(
            detect_pattern_type("git://github.com/org/repo"),
            PatternType::UrlOrGitProtocol
        );
    }

    #[test]
    fn test_detect_pattern_type_wildcard_in_url() {
        assert_eq!(
            detect_pattern_type("https://github.com/org/*"),
            PatternType::UrlOrGitProtocol
        );
    }

    #[test]
    fn test_detect_pattern_type_question_mark_pattern() {
        assert_eq!(detect_pattern_type("repo-?"), PatternType::UrlOrGitProtocol);
    }

    #[test]
    fn test_detect_pattern_type_bracket_pattern() {
        assert_eq!(
            detect_pattern_type("[abc]def"),
            PatternType::UrlOrGitProtocol
        );
    }

    #[test]
    fn test_detect_pattern_type_file_path_relative() {
        assert_eq!(detect_pattern_type("./path/to/repo"), PatternType::FilePath);
        assert_eq!(detect_pattern_type("path/to/repo"), PatternType::FilePath);
    }

    #[test]
    fn test_detect_pattern_type_file_path_absolute() {
        assert_eq!(detect_pattern_type("/path/to/repo"), PatternType::FilePath);
    }

    #[test]
    fn test_detect_pattern_type_file_path_home() {
        assert_eq!(detect_pattern_type("~/repo"), PatternType::FilePath);
    }

    #[test]
    fn test_detect_pattern_type_single_dot() {
        assert_eq!(detect_pattern_type("."), PatternType::FilePath);
    }

    #[test]
    fn test_detect_pattern_type_double_dot() {
        assert_eq!(detect_pattern_type(".."), PatternType::FilePath);
    }

    #[test]
    fn test_resolve_repository_value_wildcard() {
        let result = resolve_repository_value("*").unwrap();
        assert_eq!(result, vec!["*"]);
    }

    #[test]
    fn test_resolve_repository_value_url() {
        let result = resolve_repository_value("https://github.com/org/repo").unwrap();
        assert_eq!(result, vec!["https://github.com/org/repo"]);
    }

    #[test]
    fn test_resolve_repository_value_git_ssh() {
        let result = resolve_repository_value("git@github.com:org/repo.git").unwrap();
        assert_eq!(result, vec!["git@github.com:org/repo.git"]);
    }

    #[test]
    fn test_log_array_changes_add_mode() {
        let items = vec!["item1".to_string(), "item2".to_string()];
        // Just test that it doesn't panic - output goes to stderr
        log_array_changes(&items, true);
    }

    #[test]
    fn test_log_array_changes_set_mode() {
        let items = vec!["item1".to_string(), "item2".to_string()];
        // Just test that it doesn't panic - output goes to stderr
        log_array_changes(&items, false);
    }

    #[test]
    fn test_log_array_removals() {
        let items = vec!["item1".to_string(), "item2".to_string()];
        // Just test that it doesn't panic - output goes to stderr
        log_array_removals(&items);
    }

    #[test]
    fn test_log_array_changes_empty() {
        let items: Vec<String> = vec![];
        log_array_changes(&items, true);
        log_array_changes(&items, false);
    }

    #[test]
    fn test_log_array_removals_empty() {
        let items: Vec<String> = vec![];
        log_array_removals(&items);
    }

    #[test]
    fn test_parse_bool_case_insensitive() {
        assert!(parse_bool("TRUE").unwrap());
        assert!(parse_bool("True").unwrap());
        assert!(parse_bool("tRuE").unwrap());
        assert!(!parse_bool("FALSE").unwrap());
        assert!(!parse_bool("False").unwrap());
        assert!(!parse_bool("fAlSe").unwrap());
    }

    #[test]
    fn test_parse_bool_numeric() {
        assert!(parse_bool("1").unwrap());
        assert!(!parse_bool("0").unwrap());
    }

    #[test]
    fn test_parse_bool_word_forms() {
        assert!(parse_bool("yes").unwrap());
        assert!(parse_bool("YES").unwrap());
        assert!(parse_bool("on").unwrap());
        assert!(parse_bool("ON").unwrap());
        assert!(!parse_bool("no").unwrap());
        assert!(!parse_bool("NO").unwrap());
        assert!(!parse_bool("off").unwrap());
        assert!(!parse_bool("OFF").unwrap());
    }

    #[test]
    fn test_parse_bool_invalid_number() {
        assert!(parse_bool("2").is_err());
        assert!(parse_bool("-1").is_err());
    }

    #[test]
    fn test_parse_bool_empty_string() {
        assert!(parse_bool("").is_err());
    }

    #[test]
    fn test_parse_bool_whitespace() {
        // Whitespace is not trimmed by parse_bool
        assert!(parse_bool(" true").is_err());
        assert!(parse_bool("true ").is_err());
    }

    #[test]
    fn test_pattern_type_combinations() {
        // Test edge cases with @ and : characters
        assert_eq!(
            detect_pattern_type("user@host:path"),
            PatternType::UrlOrGitProtocol
        );
        assert_eq!(detect_pattern_type("@:"), PatternType::UrlOrGitProtocol);
        // @ but no : means file path
        assert_eq!(detect_pattern_type("file@name"), PatternType::FilePath);
        // : but no @ means file path (unless absolute)
        assert_eq!(detect_pattern_type("file:name"), PatternType::FilePath);
    }

    #[test]
    fn test_pattern_type_custom_protocols() {
        assert_eq!(
            detect_pattern_type("custom://host/path"),
            PatternType::UrlOrGitProtocol
        );
        assert_eq!(
            detect_pattern_type("ftp://host/path"),
            PatternType::UrlOrGitProtocol
        );
    }
}
