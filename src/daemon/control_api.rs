use crate::commands::checkpoint_agent::bash_tool::{InflightBashAgentContext, StatEntry};
use crate::commands::checkpoint_agent::orchestrator::CheckpointRequest;
use crate::daemon::domain::RepoContext;
use crate::metrics::MetricEvent;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "method", content = "params")]
pub enum ControlRequest {
    #[serde(rename = "checkpoint.run")]
    CheckpointRun {
        request: Box<CheckpointRequest>,
    },
    #[serde(rename = "bash.begin_invocation")]
    BashBeginInvocation {
        repo_working_dir: String,
        invocation_id: String,
        agent_context: InflightBashAgentContext,
        stat_snapshot: BashStatSnapshot,
    },
    #[serde(rename = "bash.complete_invocation")]
    BashCompleteInvocation {
        repo_working_dir: String,
        invocation_id: String,
    },
    #[serde(rename = "status.family")]
    StatusFamily { repo_working_dir: String },
    #[serde(rename = "telemetry.submit")]
    SubmitTelemetry { envelopes: Vec<TelemetryEnvelope> },
    #[serde(rename = "cas.submit")]
    SubmitCas { records: Vec<CasSyncPayload> },
    #[serde(rename = "wrapper.pre_state")]
    WrapperPreState {
        invocation_id: String,
        repo_working_dir: String,
        repo_context: RepoContext,
    },
    #[serde(rename = "wrapper.post_state")]
    WrapperPostState {
        invocation_id: String,
        repo_working_dir: String,
        repo_context: RepoContext,
    },
    #[serde(rename = "snapshot.watermarks")]
    SnapshotWatermarks { repo_working_dir: String },
    #[serde(rename = "shutdown")]
    Shutdown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BashStatSnapshot {
    pub entries: HashMap<PathBuf, StatEntry>,
    pub repo_root: PathBuf,
    #[serde(default)]
    pub effective_worktree_wm: Option<u128>,
    #[serde(default)]
    pub per_file_wm: HashMap<String, u128>,
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

/// A telemetry envelope sent from client to daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TelemetryEnvelope {
    Error {
        timestamp: String,
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        context: Option<Value>,
    },
    Performance {
        timestamp: String,
        operation: String,
        duration_ms: u128,
        #[serde(skip_serializing_if = "Option::is_none")]
        context: Option<Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tags: Option<std::collections::HashMap<String, String>>,
    },
    Message {
        timestamp: String,
        message: String,
        level: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        context: Option<Value>,
    },
    Metrics {
        events: Vec<MetricEvent>,
    },
}

/// A CAS object payload sent from client to daemon for background upload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CasSyncPayload {
    pub hash: String,
    pub data: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<String>,
}
