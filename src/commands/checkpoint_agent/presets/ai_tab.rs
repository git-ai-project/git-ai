use super::{AgentPreset, ParsedHookEvent};
use crate::error::GitAiError;

pub struct AiTabPreset;

impl AgentPreset for AiTabPreset {
    fn parse(&self, _hook_input: &str, _trace_id: &str) -> Result<Vec<ParsedHookEvent>, GitAiError> {
        Err(GitAiError::PresetError("Not yet implemented".to_string()))
    }
}
