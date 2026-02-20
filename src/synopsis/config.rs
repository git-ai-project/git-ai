use std::env;

/// How long the synopsis should be.
#[derive(Debug, Clone, PartialEq)]
pub enum TargetLength {
    /// ~300-500 words
    Brief,
    /// ~800-1500 words
    Standard,
    /// ~1500-3000 words
    Detailed,
}

impl TargetLength {
    pub fn word_range(&self) -> (usize, usize) {
        match self {
            TargetLength::Brief => (300, 500),
            TargetLength::Standard => (800, 1500),
            TargetLength::Detailed => (1500, 3000),
        }
    }
}

/// Which kind of conversation source to use.
#[derive(Debug, Clone, PartialEq)]
pub enum ConversationSourceKind {
    /// Automatically detect from known locations.
    Auto,
    /// Look specifically in Claude Code project directories.
    ClaudeCode,
    /// Do not attempt to load any conversation.
    None,
}

/// Which backend to use for AI generation.
#[derive(Debug, Clone, PartialEq)]
pub enum GenerationBackend {
    /// Call the Anthropic Messages API directly (requires ANTHROPIC_API_KEY).
    AnthropicApi,
    /// Pipe the prompt to the `claude` CLI (`claude --print`).
    /// Uses Claude Code's existing authentication — no separate API key needed.
    ClaudeCli,
}

/// Runtime configuration for synopsis generation.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct SynopsisConfig {
    pub enabled: bool,
    pub model: String,
    pub target_length: TargetLength,
    pub conversation_source: ConversationSourceKind,
    /// Explicit override path for the conversation JSONL file.
    pub conversation_path: Option<String>,
    /// How many minutes before the commit time to include in the conversation window.
    pub conversation_window_minutes: u64,
    /// Maximum number of characters (~4 chars/token) to include from a conversation.
    pub max_conversation_tokens: usize,
    pub diff_context_lines: usize,
    /// Git notes ref name (not the full ref, just the short name after `refs/notes/`).
    pub notes_ref: String,
    pub interactive: bool,
    pub api_key: Option<String>,
    pub api_base_url: String,
    /// Which AI backend to use for generation.
    pub backend: GenerationBackend,
}

impl Default for SynopsisConfig {
    fn default() -> Self {
        let api_key = env::var("ANTHROPIC_API_KEY")
            .ok()
            .or_else(|| env::var("GIT_AI_SYNOPSIS_API_KEY").ok());

        // Standard Anthropic SDK env var; fall back to hardcoded default.
        let api_base_url = env::var("ANTHROPIC_BASE_URL")
            .unwrap_or_else(|_| "https://api.anthropic.com".to_string());

        // "claude" selects the claude-cli backend; anything else (or absent) uses the API.
        let backend = match env::var("GIT_AI_SYNOPSIS_BACKEND")
            .unwrap_or_default()
            .to_lowercase()
            .as_str()
        {
            "claude" | "claude-code" | "claude-cli" => GenerationBackend::ClaudeCli,
            _ => GenerationBackend::AnthropicApi,
        };

        Self {
            enabled: env::var("GIT_AI_SYNOPSIS")
                .map(|v| v == "1" || v.to_lowercase() == "true")
                .unwrap_or(false),
            model: env::var("GIT_AI_SYNOPSIS_MODEL")
                .unwrap_or_else(|_| "claude-opus-4-6".to_string()),
            target_length: TargetLength::Standard,
            conversation_source: ConversationSourceKind::Auto,
            conversation_path: None,
            conversation_window_minutes: 60,
            max_conversation_tokens: 80_000,
            diff_context_lines: 10,
            notes_ref: "ai-synopsis".to_string(),
            interactive: true,
            api_key,
            api_base_url,
            backend,
        }
    }
}
