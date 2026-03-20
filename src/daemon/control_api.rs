use crate::commands::checkpoint_agent::agent_presets::AgentRunResult;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "method", content = "params")]
pub enum ControlRequest {
    #[serde(rename = "trace.ingest")]
    TraceIngest { payload: Value, wait: Option<bool> },
    #[serde(rename = "checkpoint.run")]
    CheckpointRun {
        request: Box<CheckpointRunRequest>,
        wait: Option<bool>,
    },
    #[serde(rename = "env.override")]
    EnvOverride {
        repo_working_dir: String,
        env: HashMap<String, String>,
        wait: Option<bool>,
    },
    #[serde(rename = "status.family")]
    StatusFamily { repo_working_dir: String },
    #[serde(rename = "snapshot.family")]
    SnapshotFamily { repo_working_dir: String },
    #[serde(rename = "barrier.applied_through_seq")]
    BarrierAppliedThroughSeq { repo_working_dir: String, seq: u64 },
    #[serde(rename = "wait.family_idle")]
    WaitFamilyIdle {
        repo_working_dir: String,
        timeout_ms: Option<u64>,
    },
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
    pub applied_seq: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ControlResponse {
    pub fn ok(seq: Option<u64>, applied_seq: Option<u64>, data: Option<Value>) -> Self {
        Self {
            ok: true,
            seq,
            applied_seq,
            data,
            error: None,
        }
    }

    pub fn err(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            seq: None,
            applied_seq: None,
            data: None,
            error: Some(msg.into()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FamilyStatus {
    pub family_key: String,
    pub latest_seq: u64,
    pub cursor: u64,
    pub backlog: u64,
    pub effect_queue_depth: usize,
    pub active_trace_connections: usize,
    pub pending_roots: usize,
    pub last_error: Option<String>,
}
