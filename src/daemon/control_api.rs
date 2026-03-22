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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CheckpointRunRequest {
    #[serde(default)]
    pub repo_working_dir: String,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub show_working_log: Option<bool>,
    #[serde(default)]
    pub reset: Option<bool>,
    #[serde(default)]
    pub quiet: Option<bool>,
    #[serde(default)]
    pub is_pre_commit: Option<bool>,
    #[serde(default)]
    pub agent_run_result: Option<AgentRunResult>,
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
