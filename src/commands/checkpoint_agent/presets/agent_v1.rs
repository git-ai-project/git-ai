use super::{AgentPreset, ParsedHookEvent};
use crate::error::GitAiError;

pub struct AgentV1Preset;

impl AgentPreset for AgentV1Preset {
    fn parse(&self, _hook_input: &str, _trace_id: &str) -> Result<Vec<ParsedHookEvent>, GitAiError> {
        Err(GitAiError::PresetError("Not yet implemented".to_string()))
    }
}
