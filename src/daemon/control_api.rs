use crate::commands::checkpoint_agent::agent_presets::AgentRunResult;
use crate::daemon::domain::RepoContext;
use crate::metrics::MetricEvent;
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
    #[serde(rename = "shutdown")]
    Shutdown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "request_type", rename_all = "snake_case")]
pub enum CheckpointRunRequest {
    Live(Box<LiveCheckpointRunRequest>),
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
