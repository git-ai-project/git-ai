use crate::error::GitAiError;
use crate::synopsis::config::{GenerationBackend, SynopsisConfig};
use crate::synopsis::conversation::render_conversation;
use crate::synopsis::types::SynopsisInput;

/// Generate a synopsis using whichever backend is configured.
pub fn generate_synopsis(
    input: &SynopsisInput,
    config: &SynopsisConfig,
) -> Result<String, GitAiError> {
    let prompt = build_prompt(input, config);
    match config.backend {
        GenerationBackend::ClaudeCli => generate_via_claude_cli(&prompt, config),
        GenerationBackend::AnthropicApi => generate_via_api(&prompt, config),
    }
}

/// Call the `claude` CLI with `--print` to generate the synopsis.
///
/// This uses Claude Code's existing authentication — no separate API key is
/// required. The model flag is passed when set; if the `claude` binary is not
/// on PATH the error is surfaced clearly.
fn generate_via_claude_cli(prompt: &str, config: &SynopsisConfig) -> Result<String, GitAiError> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let mut cmd = Command::new("claude");
    cmd.arg("--print");
    // Pass model if it looks like a real model name (non-empty, not the placeholder default
    // that users might not have changed).
    if !config.model.is_empty() {
        cmd.arg("--model").arg(&config.model);
    }
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| {
        GitAiError::Generic(format!(
            "Failed to launch `claude` CLI: {}. Is Claude Code installed and on your PATH?",
            e
        ))
    })?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(prompt.as_bytes())
            .map_err(GitAiError::IoError)?;
    }

    let output = child.wait_with_output().map_err(GitAiError::IoError)?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(GitAiError::Generic(format!(
            "`claude --print` failed (exit {}): {}",
            output.status.code().unwrap_or(-1),
            stderr.trim()
        )));
    }

    let text = String::from_utf8(output.stdout).map_err(GitAiError::FromUtf8Error)?;
    Ok(text.trim().to_string())
}

/// Call the Anthropic Messages API directly.
fn generate_via_api(prompt: &str, config: &SynopsisConfig) -> Result<String, GitAiError> {
    let api_key = config.api_key.as_deref().ok_or_else(|| {
        GitAiError::Generic(
            "No API key found. Set ANTHROPIC_API_KEY, or use --via-claude to generate \
             via the Claude Code CLI instead."
                .to_string(),
        )
    })?;

    let request_body = build_request_body(&config.model, prompt);
    let request_json = serde_json::to_string(&request_body).map_err(GitAiError::JsonError)?;

    let url = format!("{}/v1/messages", config.api_base_url);

    let response = minreq::post(&url)
        .with_header("x-api-key", api_key)
        .with_header("anthropic-version", "2023-06-01")
        .with_header("content-type", "application/json")
        .with_body(request_json)
        .with_timeout(300)
        .send()
        .map_err(|e| GitAiError::Generic(format!("HTTP request to Anthropic API failed: {}", e)))?;

    let status = response.status_code;
    let body = response.as_str().map_err(|e| {
        GitAiError::Generic(format!("Failed to read Anthropic API response: {}", e))
    })?;

    if status < 200 || status >= 300 {
        return Err(GitAiError::Generic(format!(
            "Anthropic API returned HTTP {}: {}",
            status, body
        )));
    }

    parse_response(body)
}

/// Construct the rich system + user prompt for synopsis generation.
fn build_prompt(input: &SynopsisInput, config: &SynopsisConfig) -> String {
    let (min_words, max_words) = config.target_length.word_range();

    let mut prompt = String::new();

    prompt.push_str(&format!(
        "You are a technical writer specialising in AI-assisted software development. \
Your task is to write a detailed, engaging blog-article-style narrative synopsis of a \
single git commit. The synopsis should be readable by other developers and give them \
deep insight into what was built and why.\n\n\
**Length target**: {}-{} words.\n\n",
        min_words, max_words
    ));

    prompt.push_str(
        "**Required sections** (use Markdown headings):\n\
1. `## TL;DR` — One or two sentences summarising what was accomplished.\n\
2. `## Background and Motivation` — Why was this work needed? What problem does it solve?\n\
3. `## The Journey` — Describe the development process: approaches explored, \
   decisions made, dead ends encountered, pivots taken.  \
   If conversation context is provided, ground this section in the actual dialogue.\n\
4. `## The Solution` — What was actually implemented? Describe the architecture, \
   key algorithms, or design decisions at an appropriate level of detail.\n\
5. `## Key Files Changed` — A brief description of each significant file changed.\n\
6. `## Reflections` — What was learned? What trade-offs were made? What might be done differently?\n\n",
    );

    prompt.push_str(
        "Write with the voice of a thoughtful senior engineer reflecting on their work. \
Be specific — reference actual function names, file names, and design choices where \
relevant. Avoid generic filler phrases.\n\n",
    );

    prompt.push_str("---\n\n");

    // Commit metadata
    prompt.push_str(&format!("**Commit SHA**: `{}`\n", input.commit_sha));
    prompt.push_str(&format!("**Author**: {}\n", input.author));
    prompt.push_str("\n");

    // Commit message
    prompt.push_str("## Commit Message\n\n```\n");
    prompt.push_str(&input.commit_message);
    prompt.push_str("\n```\n\n");

    // Diff stat
    prompt.push_str("## Diff Statistics\n\n```\n");
    prompt.push_str(&input.diff.stat_summary);
    prompt.push_str("\n```\n\n");

    // Unified diff (may be large; truncate to ~200 kB to avoid context overruns)
    let diff_text = truncate_to_chars(&input.diff.unified_diff, 200_000);
    if !diff_text.is_empty() {
        prompt.push_str("## Unified Diff\n\n```diff\n");
        prompt.push_str(&diff_text);
        prompt.push_str("\n```\n\n");
    }

    // Conversation context (optional)
    if let Some(conv) = &input.conversation {
        // ~4 chars per token as a rough estimate
        let max_chars = config.max_conversation_tokens * 4;
        let rendered = render_conversation(conv, max_chars);
        if !rendered.is_empty() {
            prompt.push_str("## AI Conversation Context\n\n");
            prompt.push_str(&format!(
                "_Source: {} ({})_\n\n",
                conv.source_kind, conv.source_path
            ));
            prompt.push_str(&rendered);
            prompt.push('\n');
        }
    } else {
        prompt.push_str("_No conversation context was available for this commit._\n\n");
    }

    prompt.push_str("---\n\n");
    prompt.push_str(
        "Please write the synopsis now, following all the required sections above. \
Start directly with a suitable title on the first line (no preamble).\n",
    );

    prompt
}

/// Build the Anthropic Messages API request body as a JSON value.
fn build_request_body(model: &str, prompt: &str) -> serde_json::Value {
    serde_json::json!({
        "model": model,
        "max_tokens": 4096,
        "messages": [
            {
                "role": "user",
                "content": prompt
            }
        ]
    })
}

/// Parse the Anthropic Messages API JSON response and extract the text content.
fn parse_response(body: &str) -> Result<String, GitAiError> {
    let parsed: serde_json::Value = serde_json::from_str(body).map_err(GitAiError::JsonError)?;

    // Check for API-level error
    if let Some(error) = parsed.get("error") {
        let msg = error["message"].as_str().unwrap_or("Unknown API error");
        return Err(GitAiError::Generic(format!("Anthropic API error: {}", msg)));
    }

    // Navigate: content[0].text
    let text = parsed["content"]
        .as_array()
        .and_then(|arr| arr.first())
        .and_then(|block| block["text"].as_str())
        .ok_or_else(|| {
            GitAiError::Generic(format!("Unexpected Anthropic API response shape: {}", body))
        })?;

    Ok(text.trim().to_string())
}

/// Estimate input token count as a rough approximation (4 chars per token).
pub fn estimate_input_tokens(prompt: &str) -> usize {
    (prompt.len() + 3) / 4
}

fn truncate_to_chars(s: &str, max_chars: usize) -> String {
    if s.len() <= max_chars {
        s.to_string()
    } else {
        let mut truncated: String = s.chars().take(max_chars).collect();
        truncated.push_str("\n\n[... diff truncated for length ...]");
        truncated
    }
}

/// Build the synopsis prompt string without calling the API (used for token estimation).
pub fn build_synopsis_prompt(input: &SynopsisInput, config: &SynopsisConfig) -> String {
    build_prompt(input, config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_response_valid() {
        let body = serde_json::json!({
            "content": [{"type": "text", "text": "  Hello synopsis  "}],
            "model": "claude-opus-4-6",
            "stop_reason": "end_turn"
        })
        .to_string();

        let result = parse_response(&body).unwrap();
        assert_eq!(result, "Hello synopsis");
    }

    #[test]
    fn test_parse_response_api_error() {
        let body = serde_json::json!({
            "error": {"type": "authentication_error", "message": "invalid x-api-key"}
        })
        .to_string();

        let err = parse_response(&body).unwrap_err();
        assert!(matches!(err, GitAiError::Generic(_)));
        if let GitAiError::Generic(msg) = err {
            assert!(msg.contains("invalid x-api-key"));
        }
    }

    #[test]
    fn test_truncate_to_chars_short() {
        let s = "short";
        assert_eq!(truncate_to_chars(s, 100), "short");
    }

    #[test]
    fn test_truncate_to_chars_long() {
        let s = "a".repeat(200);
        let result = truncate_to_chars(&s, 100);
        assert!(result.len() > 100); // has truncation notice
        assert!(result.contains("truncated"));
    }

    #[test]
    fn test_estimate_input_tokens() {
        assert_eq!(estimate_input_tokens("1234"), 1);
        assert_eq!(estimate_input_tokens("12345678"), 2);
    }
}
