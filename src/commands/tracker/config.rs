use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Deserialize, Serialize)]
pub struct TrackerConfig {
    pub tracker_url: String,
    pub team_id: String,
    pub team_key: String,
    pub blacklist: Vec<String>,
}

pub fn config_path() -> PathBuf {
    crate::mdm::utils::home_dir()
        .join(".git-ai")
        .join("tracker-config.json")
}

pub fn load_config() -> Option<TrackerConfig> {
    let path = config_path();
    if !path.exists() {
        return None;
    }
    let raw = fs::read_to_string(&path).ok()?;
    serde_json::from_str::<TrackerConfig>(&raw).ok()
}

pub fn save_config(config: &TrackerConfig) -> Result<(), String> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("Failed to create config dir: {}", e))?;
    }
    let json = serde_json::to_string_pretty(config)
        .map_err(|e| format!("Failed to serialize config: {}", e))?;
    fs::write(&path, json).map_err(|e| format!("Failed to write config: {}", e))?;
    Ok(())
}

pub fn add_to_blacklist(pattern: &str) -> Result<(), String> {
    let mut config = load_config().ok_or("Config file not found")?;
    if config.blacklist.contains(&pattern.to_string()) {
        return Err(format!("Pattern '{}' already in blacklist", pattern));
    }
    config.blacklist.push(pattern.to_string());
    save_config(&config)?;
    Ok(())
}

pub fn remove_from_blacklist(pattern: &str) -> Result<(), String> {
    let mut config = load_config().ok_or("Config file not found")?;
    let original_len = config.blacklist.len();
    config.blacklist.retain(|p| p != pattern);
    if config.blacklist.len() == original_len {
        return Err(format!("Pattern '{}' not found in blacklist", pattern));
    }
    save_config(&config)?;
    Ok(())
}

pub fn list_blacklist() -> Result<Vec<String>, String> {
    let config = load_config().ok_or("Config file not found")?;
    Ok(config.blacklist)
}
