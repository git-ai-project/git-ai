use crate::commands::checkpoint_agent::agent_presets::AgentRunResult;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "method", content = "params")]
pub enum ControlRequest {
    #[serde(rename = "checkpoint.run")]
    CheckpointRun {
        request: Box<CheckpointRunRequest>,
        wait: Option<bool>,
    },
    #[serde(rename = "status.family")]
    StatusFamily { repo_working_dir: String },
    #[serde(rename = "shutdown")]
    Shutdown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "request_type", rename_all = "snake_case")]
pub enum CheckpointRunRequest {
    Live(LiveCheckpointRunRequest),
    Captured(CapturedCheckpointRunRequest),
}

impl CheckpointRunRequest {
    pub fn repo_working_dir(&self) -> &str {
        match self {
            Self::Live(request) => &request.repo_working_dir,
            Self::Captured(request) => &request.repo_working_dir,
        }
    }

    pub fn is_pre_commit(&self) -> bool {
        match self {
            Self::Live(request) => request.is_pre_commit.unwrap_or(false),
            Self::Captured(_) => false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LiveCheckpointRunRequest {
    #[serde(default)]
    pub repo_working_dir: String,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub reset: Option<bool>,
    #[serde(default)]
    pub quiet: Option<bool>,
    #[serde(default)]
    pub is_pre_commit: Option<bool>,
    #[serde(default)]
    pub agent_run_result: Option<AgentRunResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CapturedCheckpointRunRequest {
    #[serde(default)]
    pub repo_working_dir: String,
    #[serde(default)]
    pub capture_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ControlResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ControlResponse {
    pub fn ok(seq: Option<u64>, data: Option<Value>) -> Self {
        Self {
            ok: true,
            seq,
            data,
            error: None,
        }
    }

    pub fn err(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            seq: None,
            data: None,
            error: Some(msg.into()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FamilyStatus {
    pub family_key: String,
    pub latest_seq: u64,
    pub last_error: Option<String>,
}
