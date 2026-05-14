use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlRequest {
    Checkpoint(CheckpointRequest),
    Status(StatusRequest),
    Stats,
    Shutdown,
    Ping,
}

#[derive(Debug, Deserialize)]
pub struct CheckpointRequest {
    pub repo_dir: String,
    pub kind: String,
    pub files: Vec<CheckpointFileEntry>,
    #[serde(default)]
    pub agent: Option<AgentInfo>,
}

#[derive(Debug, Deserialize)]
pub struct CheckpointFileEntry {
    pub path: String,
    #[serde(default)]
    pub content: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AgentInfo {
    pub tool: String,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct StatusRequest {
    pub repo_dir: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ControlResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub processed: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<StatusResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stats: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StatusResponse {
    pub base_commit: String,
    pub checkpoint_count: u32,
    pub files: Vec<String>,
}

impl ControlResponse {
    pub fn ok_processed(count: u32) -> Self {
        Self {
            ok: true,
            processed: Some(count),
            error: None,
            version: None,
            pid: None,
            status: None,
            stats: None,
        }
    }

    pub fn ok_pong() -> Self {
        Self {
            ok: true,
            processed: None,
            error: None,
            version: Some(env!("CARGO_PKG_VERSION").to_string()),
            pid: Some(std::process::id()),
            status: None,
            stats: None,
        }
    }

    pub fn ok_status(status: StatusResponse) -> Self {
        Self {
            ok: true,
            processed: None,
            error: None,
            version: None,
            pid: None,
            status: Some(status),
            stats: None,
        }
    }

    pub fn ok_stats(report: String) -> Self {
        Self {
            ok: true,
            processed: None,
            error: None,
            version: None,
            pid: None,
            status: None,
            stats: Some(report),
        }
    }

    pub fn err(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            processed: None,
            error: Some(msg.into()),
            version: None,
            pid: None,
            status: None,
            stats: None,
        }
    }

    pub fn ok_shutdown() -> Self {
        Self {
            ok: true,
            processed: None,
            error: None,
            version: None,
            pid: None,
            status: None,
            stats: None,
        }
    }
}
