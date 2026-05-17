use serde_json::Value;
use std::fs;
use std::path::PathBuf;

fn config_path() -> Result<PathBuf, String> {
    git_ai::api::home_dir()
        .map(|h| h.join(".git-ai").join("config.json"))
        .ok_or_else(|| "unable to determine home directory".to_string())
}

fn load_config() -> Result<Value, String> {
    let path = config_path()?;
    match fs::read_to_string(&path) {
        Ok(contents) => {
            serde_json::from_str(&contents).map_err(|e| format!("invalid config JSON: {}", e))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Value::Object(Default::default())),
        Err(e) => Err(format!("failed to read config: {}", e)),
    }
}

fn save_config(config: &Value) -> Result<(), String> {
    let path = config_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create config directory: {}", e))?;
    }
    let json =
        serde_json::to_string_pretty(config).map_err(|e| format!("serialization error: {}", e))?;
    fs::write(&path, json).map_err(|e| format!("failed to write config: {}", e))
}

fn print_help() {
    println!("git-ai config - View and manage configuration");
    println!();
    println!("Usage:");
    println!("  git-ai config                  Show all config as formatted JSON");
    println!("  git-ai config <key>            Show specific config value");
    println!("  git-ai config set <key> <val>  Set a config value");
    println!("  git-ai config set <key> <val> --add  Add to array");
    println!("  git-ai config --add <key> <val>      Add to array or upsert");
    println!("  git-ai config unset <key>      Remove config value");
    println!();
    println!("Configuration Keys:");
    println!("  git_path                     Path to git binary");
    println!("  exclude_repositories         Excluded repos (array)");
    println!("  allow_repositories           Allowed repos (array)");
    println!("  telemetry_oss                OSS telemetry (on/off)");
    println!("  disable_version_checks       Disable version checks (bool)");
    println!("  disable_auto_updates         Disable auto updates (bool)");
    println!("  update_channel               Update channel (latest/next)");
    println!("  feature_flags.<key>          Feature flags (nested)");
    println!("  api_key                      API key");
    println!("  prompt_storage               Prompt storage mode");
    println!("  quiet                        Suppress chart output (bool)");
    println!("  git_ai_hooks.<hook>          Hook commands (nested)");
    println!();
    println!("Examples:");
    println!("  git-ai config set quiet true");
    println!("  git-ai config set api_key sk-...");
    println!("  git-ai config --add exclude_repositories \"private/*\"");
    println!("  git-ai config set feature_flags.auth_keyring true");
    println!("  git-ai config unset api_key");
}

pub fn handle_config(args: &[String]) {
    if args.is_empty() {
        match show_all() {
            Ok(()) => {}
            Err(e) => {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        }
        return;
    }

    if args[0] == "--help" || args[0] == "-h" || args[0] == "help" {
        print_help();
        return;
    }

    let is_add = args.iter().any(|a| a == "--add");
    let filtered: Vec<&String> = args.iter().filter(|a| *a != "--add").collect();

    if filtered.is_empty() {
        eprintln!("Error: --add requires <key> <value>");
        std::process::exit(1);
    }

    let result = match filtered[0].as_str() {
        "set" => {
            if filtered.len() < 3 {
                eprintln!("Error: set requires <key> <value>");
                eprintln!("Usage: git-ai config set <key> <value>");
                std::process::exit(1);
            }
            set_value(filtered[1], filtered[2], is_add)
        }
        "unset" => {
            if filtered.len() < 2 {
                eprintln!("Error: unset requires <key>");
                std::process::exit(1);
            }
            unset_value(filtered[1])
        }
        key => {
            if is_add {
                if filtered.len() < 2 {
                    eprintln!("Error: --add requires <key> <value>");
                    std::process::exit(1);
                }
                set_value(key, filtered[1], true)
            } else {
                get_value(key)
            }
        }
    };

    if let Err(e) = result {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }
}

fn show_all() -> Result<(), String> {
    let config = load_config()?;
    let json =
        serde_json::to_string_pretty(&config).map_err(|e| format!("serialization error: {}", e))?;
    println!("{}", json);
    Ok(())
}

fn get_value(key: &str) -> Result<(), String> {
    let config = load_config()?;
    let segments: Vec<&str> = key.split('.').collect();

    let mut current = &config;
    for segment in &segments {
        current = current
            .get(*segment)
            .ok_or_else(|| format!("key not found: {}", key))?;
    }

    let json =
        serde_json::to_string_pretty(current).map_err(|e| format!("serialization error: {}", e))?;
    println!("{}", json);
    Ok(())
}

fn set_value(key: &str, value: &str, add_mode: bool) -> Result<(), String> {
    let mut config = load_config()?;
    let segments: Vec<&str> = key.split('.').collect();

    let parsed_value = parse_value(value);

    if add_mode {
        let target = navigate_to_parent_mut(&mut config, &segments)?;
        let last = *segments.last().unwrap();

        match target.get_mut(last) {
            Some(existing) if existing.is_array() => {
                existing.as_array_mut().unwrap().push(parsed_value.clone());
            }
            Some(_) => {
                return Err(format!(
                    "key '{}' exists but is not an array; cannot use --add",
                    key
                ));
            }
            None => {
                target
                    .as_object_mut()
                    .unwrap()
                    .insert(last.to_string(), Value::Array(vec![parsed_value.clone()]));
            }
        }
    } else {
        let target = navigate_to_parent_mut(&mut config, &segments)?;
        let last = *segments.last().unwrap();
        target
            .as_object_mut()
            .unwrap()
            .insert(last.to_string(), parsed_value.clone());
    }

    save_config(&config)?;

    if key == "api_key" {
        let masked = mask_value(value);
        println!("[{}]: {}", key, masked);
    } else {
        println!("[{}]: {}", key, value);
    }
    Ok(())
}

fn unset_value(key: &str) -> Result<(), String> {
    let mut config = load_config()?;
    let segments: Vec<&str> = key.split('.').collect();

    let target = navigate_to_parent_mut(&mut config, &segments)?;
    let last = *segments.last().unwrap();

    if target.as_object_mut().unwrap().remove(last).is_none() {
        return Err(format!("key not found: {}", key));
    }

    save_config(&config)?;
    println!("Unset [{}]", key);
    Ok(())
}

fn navigate_to_parent_mut<'a>(
    config: &'a mut Value,
    segments: &[&str],
) -> Result<&'a mut Value, String> {
    let mut current = config;
    for segment in &segments[..segments.len() - 1] {
        if !current.is_object() {
            return Err(format!("cannot navigate into non-object at '{}'", segment));
        }
        let obj = current.as_object_mut().unwrap();
        if !obj.contains_key(*segment) {
            obj.insert(segment.to_string(), Value::Object(serde_json::Map::new()));
        }
        current = obj.get_mut(*segment).unwrap();
    }
    if !current.is_object() {
        return Err("cannot set value on non-object".to_string());
    }
    Ok(current)
}

fn parse_value(s: &str) -> Value {
    if s == "true" {
        return Value::Bool(true);
    }
    if s == "false" {
        return Value::Bool(false);
    }
    if s == "null" {
        return Value::Null;
    }
    if let Ok(n) = s.parse::<i64>() {
        return Value::Number(n.into());
    }
    if let Ok(n) = s.parse::<f64>()
        && let Some(num) = serde_json::Number::from_f64(n)
    {
        return Value::Number(num);
    }
    if let Ok(v) = serde_json::from_str::<Value>(s)
        && (v.is_array() || v.is_object())
    {
        return v;
    }
    Value::String(s.to_string())
}

fn mask_value(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() <= 8 {
        return "*".repeat(chars.len());
    }
    let prefix: String = chars[..4].iter().collect();
    let suffix: String = chars[chars.len() - 4..].iter().collect();
    format!("{}...{}", prefix, suffix)
}
