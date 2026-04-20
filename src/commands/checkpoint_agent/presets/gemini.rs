use super::{AgentPreset, ParsedHookEvent};
use crate::error::GitAiError;

pub struct GeminiPreset;

impl AgentPreset for GeminiPreset {
    fn parse(&self, _hook_input: &str, _trace_id: &str) -> Result<Vec<ParsedHookEvent>, GitAiError> {
        Err(GitAiError::PresetError("Not yet implemented".to_string()))
    }
}
