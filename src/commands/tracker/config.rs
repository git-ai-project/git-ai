use serde::Deserialize;
use std::fs;
use std::path::PathBuf;

#[derive(Deserialize)]
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
