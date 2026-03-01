#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TelemetrySignal {
    Explicit,
    Inferred,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionPhase {
    Started,
    Ended,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MessageRole {
    Human,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResponsePhase {
    Started,
    Ended,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolCallPhase {
    Started,
    Ended,
    Failed,
    PermissionRequested,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum McpCallPhase {
    Started,
    Ended,
    Failed,
    PermissionRequested,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SubagentPhase {
    Started,
    Ended,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SkillDetectionMethod {
    Explicit,
    InferredPrompt,
    InferredTool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentSessionTelemetry {
    pub phase: SessionPhase,
    pub reason: Option<String>,
    pub source: Option<String>,
    pub mode: Option<String>,
    pub duration_ms: Option<u64>,
    pub signal: TelemetrySignal,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentMessageTelemetry {
    pub role: MessageRole,
    pub prompt_char_count: Option<u32>,
    pub attachment_count: Option<u32>,
    pub signal: TelemetrySignal,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentResponseTelemetry {
    pub phase: ResponsePhase,
    pub reason: Option<String>,
    pub status: Option<String>,
    pub response_char_count: Option<u32>,
    pub signal: TelemetrySignal,
    pub dedupe_key: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentToolCallTelemetry {
    pub phase: ToolCallPhase,
    pub tool_name: Option<String>,
    pub tool_use_id: Option<String>,
    pub duration_ms: Option<u64>,
    pub failure_type: Option<String>,
    pub signal: TelemetrySignal,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentMcpCallTelemetry {
    pub phase: McpCallPhase,
    pub mcp_server: Option<String>,
    pub tool_name: Option<String>,
    pub transport: Option<String>,
    pub duration_ms: Option<u64>,
    pub failure_type: Option<String>,
    pub signal: TelemetrySignal,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentSkillUsageTelemetry {
    pub skill_name: String,
    pub detection_method: SkillDetectionMethod,
    pub signal: TelemetrySignal,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentSubagentTelemetry {
    pub phase: SubagentPhase,
    pub subagent_id: Option<String>,
    pub subagent_type: Option<String>,
    pub status: Option<String>,
    pub duration_ms: Option<u64>,
    pub result_char_count: Option<u32>,
    pub signal: TelemetrySignal,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentTelemetryEvent {
    Session(AgentSessionTelemetry),
    Message(AgentMessageTelemetry),
    Response(AgentResponseTelemetry),
    ToolCall(AgentToolCallTelemetry),
    McpCall(AgentMcpCallTelemetry),
    SkillUsage(AgentSkillUsageTelemetry),
    Subagent(AgentSubagentTelemetry),
}
