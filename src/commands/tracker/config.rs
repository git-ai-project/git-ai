use std::path::PathBuf;

pub struct TrackerConfig {
    pub tracker_url: String,
    pub team_id: String,
    pub team_key: String,
    pub blacklist: Vec<String>,
}

pub fn load_config() -> Option<TrackerConfig> {
    None
}

pub fn config_path() -> PathBuf {
    PathBuf::new()
}
