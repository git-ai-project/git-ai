use crate::authorship::stats::CommitStats;
use crate::error::GitAiError;
use crate::git::repository::Repository;

/// Configuration for commit message formatting
#[derive(Debug, Clone, Default)]
pub struct CommitMessageConfig {
    /// Enable adding stats to commit messages
    pub enabled: bool,
    /// Format style: "text" or "markdown"
    pub format: String,
    /// Include progress bar in text format
    pub include_progress_bar: bool,
    /// Include detailed breakdown in markdown format
    pub include_details: bool,
    /// Maximum width for progress bar (text format)
    pub bar_width: usize,
    /// Commit message template with placeholders
    pub template: String,
}

impl CommitMessageConfig {
    /// Create config from feature flags and git config
    pub fn from_repo(repo: &Repository) -> Result<Self, GitAiError> {
        let config = crate::config::Config::get();
        let feature_flags = config.get_feature_flags();

        // Check if feature is enabled
        let enabled = feature_flags.commit_message_stats;

        if !enabled {
            return Ok(Self::default());
        }

        // Read git config for additional options
        let format = repo
            .config_get_str("ai.commit-message-stats.format")
            .ok()
            .flatten()
            .unwrap_or_else(|| "text".to_string());

        let include_progress_bar = repo
            .config_get_str("ai.commit-message-stats.include-progress-bar")
            .ok()
            .flatten()
            .map(|v| v == "true")
            .unwrap_or(true);

        let include_details = repo
            .config_get_str("ai.commit-message-stats.include-details")
            .ok()
            .flatten()
            .map(|v| v == "true")
            .unwrap_or(true);

        let bar_width = repo
            .config_get_str("ai.commit-message-stats.bar-width")
            .ok()
            .flatten()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(20);

        let template = repo
            .config_get_str("ai.commit-message-stats.template")
            .ok()
            .flatten()
            .unwrap_or_else(|| "{original_message}\n\n{stats}".to_string());

        Ok(Self {
            enabled,
            format,
            include_progress_bar,
            include_details,
            bar_width,
            template,
        })
    }

    /// Create config with defaults for testing
    #[cfg(test)]
    pub fn test_enabled() -> Self {
        Self {
            enabled: true,
            format: "text".to_string(),
            include_progress_bar: true,
            include_details: true,
            bar_width: 20,
            template: "{original_message}\n\n{stats}".to_string(),
        }
    }
}

/// Format commit message with AI statistics
///
/// # Arguments
/// * `original_message` - The original commit message
/// * `stats` - The commit statistics
/// * `config` - Configuration for formatting
///
/// # Returns
/// The formatted commit message with statistics appended
pub fn format_commit_message(
    original_message: &str,
    stats: &CommitStats,
    config: &CommitMessageConfig,
) -> Result<String, GitAiError> {
    if !config.enabled {
        return Ok(original_message.to_string());
    }

    // Generate stats content based on format
    let stats_content = match config.format.as_str() {
        "markdown" => generate_markdown_stats(stats, config),
        "text" => generate_text_stats(stats, config),
        _ => generate_text_stats(stats, config), // Default to text
    };

    // If stats content is empty (no additions, no AI code), return original message unchanged
    if stats_content.is_empty() {
        return Ok(original_message.to_string());
    }

    // Apply template
    let mut result = config.template.clone();
    result = result.replace("{original_message}", original_message);
    result = result.replace("{stats}", &stats_content);

    // Clean up any extra newlines
    result = result.trim().to_string();
    result.push('\n');

    Ok(result)
}

/// Generate text format statistics
fn generate_text_stats(stats: &CommitStats, config: &CommitMessageConfig) -> String {
    let mut output = String::new();

    // Handle deletion-only commits
    if stats.git_diff_added_lines == 0 && stats.git_diff_deleted_lines > 0 {
        output.push_str("(no additions)");
        return output;
    }

    // Handle commits with no additions
    if stats.git_diff_added_lines == 0 {
        return output;
    }

    // **IMPORTANT**: Only add stats if there is AI code
    // If no AI additions, return empty string (no stats needed)
    if stats.ai_additions == 0 {
        return output;
    }

    // Calculate percentages
    let total_additions = stats.human_additions + stats.ai_additions;

    if total_additions == 0 {
        return output;
    }

    let pure_human = stats.human_additions.saturating_sub(stats.mixed_additions);
    let pure_human_percentage =
        ((pure_human as f64 / total_additions as f64) * 100.0).round() as u32;
    let mixed_percentage =
        ((stats.mixed_additions as f64 / total_additions as f64) * 100.0).round() as u32;
    let ai_percentage =
        ((stats.ai_additions as f64 / total_additions as f64) * 100.0).round() as u32;

    // Build progress bar if enabled
    if config.include_progress_bar {
        let bar_width = config.bar_width;

        let pure_human_bars =
            ((pure_human as f64 / total_additions as f64) * bar_width as f64) as usize;
        let mixed_bars =
            ((stats.mixed_additions as f64 / total_additions as f64) * bar_width as f64) as usize;
        let ai_bars = bar_width.saturating_sub(pure_human_bars + mixed_bars);

        let mut progress_bar = String::new();
        progress_bar.push_str(&"█".repeat(pure_human_bars));
        progress_bar.push_str(&"▒".repeat(mixed_bars));
        progress_bar.push_str(&"░".repeat(ai_bars));

        output.push_str(&format!(
            "Stats: {} | {}% you, {}% mixed, {}% ai\n",
            progress_bar, pure_human_percentage, mixed_percentage, ai_percentage
        ));
    } else {
        // Simple percentage line without bar
        output.push_str(&format!(
            "Stats: {}% you, {}% mixed, {}% ai\n",
            pure_human_percentage, mixed_percentage, ai_percentage
        ));
    }

    // Add AI-specific stats if applicable
    if stats.ai_additions > 0 {
        if stats.time_waiting_for_ai > 0 {
            let minutes = stats.time_waiting_for_ai / 60;
            let seconds = stats.time_waiting_for_ai % 60;
            let time_str = if minutes > 0 {
                format!("{}m {}s", minutes, seconds)
            } else {
                format!("{}s", seconds)
            };
            output.push_str(&format!(
                "AI: {} accepted, {} generated, waited {}\n",
                stats.ai_accepted, stats.total_ai_additions, time_str
            ));
        } else {
            output.push_str(&format!(
                "AI: {} accepted, {} generated\n",
                stats.ai_accepted, stats.total_ai_additions
            ));
        }
    }

    output.trim().to_string()
}

/// Generate markdown format statistics
fn generate_markdown_stats(stats: &CommitStats, config: &CommitMessageConfig) -> String {
    let mut output = String::new();

    // Handle deletion-only commits
    if stats.git_diff_added_lines == 0 && stats.git_diff_deleted_lines > 0 {
        output.push_str("(no additions)");
        return output;
    }

    // Handle commits with no additions
    if stats.git_diff_added_lines == 0 {
        return output;
    }

    // **IMPORTANT**: Only add stats if there is AI code
    // If no AI additions, return empty string (no stats needed)
    if stats.ai_additions == 0 {
        return output;
    }

    let total_additions = stats.git_diff_added_lines;
    let pure_human = stats.human_additions;
    let mixed = stats.mixed_additions;
    let pure_ai = stats.ai_accepted;

    // Calculate percentages
    let pure_human_percentage = if total_additions > 0 {
        ((pure_human as f64 / total_additions as f64) * 100.0).round() as u32
    } else {
        0
    };

    let mixed_percentage = if total_additions > 0 {
        ((mixed as f64 / total_additions as f64) * 100.0).round() as u32
    } else {
        0
    };

    let ai_percentage = if total_additions > 0 {
        ((pure_ai as f64 / total_additions as f64) * 100.0).round() as u32
    } else {
        0
    };

    // Build progress bar
    let bar_width = config.bar_width;

    let pure_human_bars = if total_additions > 0 {
        let calculated =
            ((pure_human as f64 / total_additions as f64) * bar_width as f64).round() as usize;
        if pure_human > 0 && calculated == 0 {
            1
        } else {
            calculated
        }
    } else {
        0
    };

    let mixed_bars = if total_additions > 0 {
        let calculated =
            ((mixed as f64 / total_additions as f64) * bar_width as f64).round() as usize;
        if mixed > 0 && calculated == 0 {
            1
        } else {
            calculated
        }
    } else {
        0
    };

    let ai_bars = if total_additions > 0 {
        let calculated =
            ((pure_ai as f64 / total_additions as f64) * bar_width as f64).round() as usize;
        if pure_ai > 0 && calculated == 0 {
            1
        } else {
            calculated
        }
    } else {
        0
    };

    // Build the output
    output.push_str("```text\n");
    output.push_str(&format!(
        "🧠 you    {}  {}%\n",
        "█".repeat(pure_human_bars),
        pure_human_percentage
    ));

    if mixed_percentage > 0 {
        output.push_str(&format!(
            "🤝 mixed  {}{}  {}%\n",
            "░".repeat(pure_human_bars),
            "█".repeat(mixed_bars),
            mixed_percentage
        ));
    }

    output.push_str(&format!(
        "🤖 ai     {}{}  {}%\n",
        "░".repeat(bar_width.saturating_sub(ai_bars)),
        "█".repeat(ai_bars),
        ai_percentage
    ));

    output.push_str("```\n");

    // Add details if enabled
    if config.include_details && stats.ai_additions > 0 {
        output.push_str("\n<details>\n<summary>AI Stats</summary>\n\n");

        let lines_per_accepted = if stats.ai_accepted > 0 {
            stats.total_ai_additions as f64 / stats.ai_accepted as f64
        } else {
            0.0
        };

        output.push_str(&format!(
            "- {:.1} lines generated for every 1 accepted\n",
            lines_per_accepted
        ));

        if stats.time_waiting_for_ai > 0 {
            let minutes = stats.time_waiting_for_ai / 60;
            let seconds = stats.time_waiting_for_ai % 60;
            let time_str = if minutes > 0 {
                format!("{} minute{}", minutes, if minutes == 1 { "" } else { "s" })
            } else {
                format!("{} second{}", seconds, if seconds == 1 { "" } else { "s" })
            };
            output.push_str(&format!("- {} waiting for AI\n", time_str));
        }

        // Add model breakdown if available
        if !stats.tool_model_breakdown.is_empty() {
            output.push_str("\n**Model breakdown:**\n");
            for (model_name, model_stats) in &stats.tool_model_breakdown {
                output.push_str(&format!(
                    "- {}: {} accepted, {} generated\n",
                    model_name, model_stats.ai_accepted, model_stats.total_ai_additions
                ));
            }
        }

        output.push_str("\n</details>\n");
    }

    output.trim().to_string()
}

/// Extract just the stats portion for use in templates
pub fn get_stats_only(stats: &CommitStats, format: &str) -> String {
    let config = CommitMessageConfig {
        enabled: true,
        format: format.to_string(),
        include_progress_bar: true,
        include_details: true,
        bar_width: 20,
        template: "{stats}".to_string(),
    };

    match format {
        "markdown" => generate_markdown_stats(stats, &config),
        "text" => generate_text_stats(stats, &config),
        _ => generate_text_stats(stats, &config),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn test_format_commit_message_disabled() {
        let config = CommitMessageConfig::default(); // disabled by default
        let stats = CommitStats::default();
        let original = "Test commit message";

        let result = format_commit_message(original, &stats, &config).unwrap();
        assert_eq!(result, "Test commit message\n");
    }

    #[test]
    fn test_format_commit_message_text() {
        let config = CommitMessageConfig::test_enabled();
        let stats = CommitStats {
            human_additions: 50,
            mixed_additions: 20,
            ai_additions: 30,
            ai_accepted: 10,
            total_ai_additions: 40,
            total_ai_deletions: 0,
            time_waiting_for_ai: 65,
            git_diff_deleted_lines: 5,
            git_diff_added_lines: 80,
            tool_model_breakdown: BTreeMap::new(),
        };

        let original = "Add feature";
        let result = format_commit_message(original, &stats, &config).unwrap();

        // Should contain original message
        assert!(result.contains("Add feature"));
        // Should contain stats
        assert!(result.contains("Stats:"));
        assert!(result.contains("% you"));
        assert!(result.contains("% ai"));
    }

    #[test]
    fn test_format_commit_message_markdown() {
        let mut config = CommitMessageConfig::test_enabled();
        config.format = "markdown".to_string();

        let stats = CommitStats {
            human_additions: 50,
            mixed_additions: 20,
            ai_additions: 30,
            ai_accepted: 10,
            total_ai_additions: 40,
            total_ai_deletions: 0,
            time_waiting_for_ai: 65,
            git_diff_deleted_lines: 5,
            git_diff_added_lines: 80,
            tool_model_breakdown: BTreeMap::new(),
        };

        let original = "Add feature";
        let result = format_commit_message(original, &stats, &config).unwrap();

        // Should contain original message
        assert!(result.contains("Add feature"));
        // Should contain markdown code block
        assert!(result.contains("```text"));
        assert!(result.contains("🧠 you"));
        assert!(result.contains("🤖 ai"));
    }

    #[test]
    fn test_deletion_only_commit() {
        let config = CommitMessageConfig::test_enabled();
        let stats = CommitStats {
            human_additions: 0,
            mixed_additions: 0,
            ai_additions: 0,
            ai_accepted: 0,
            total_ai_additions: 0,
            total_ai_deletions: 10,
            time_waiting_for_ai: 0,
            git_diff_deleted_lines: 10,
            git_diff_added_lines: 0,
            tool_model_breakdown: BTreeMap::new(),
        };

        let original = "Remove unused code";
        let result = format_commit_message(original, &stats, &config).unwrap();

        // Should still contain original message
        assert!(result.contains("Remove unused code"));
        // Should indicate no additions
        assert!(result.contains("(no additions)"));
    }

    #[test]
    fn test_custom_template_with_ai() {
        let mut config = CommitMessageConfig::test_enabled();
        config.template = "📝 {original_message}\n\n📊 {stats}".to_string();

        // Test WITH AI code - should apply template
        let stats_with_ai = CommitStats {
            human_additions: 10,
            mixed_additions: 0,
            ai_additions: 5,  // Has AI code
            ai_accepted: 5,
            total_ai_additions: 5,
            total_ai_deletions: 0,
            time_waiting_for_ai: 0,
            git_diff_deleted_lines: 0,
            git_diff_added_lines: 15,
            tool_model_breakdown: BTreeMap::new(),
        };

        let original = "Test";
        let result = format_commit_message(original, &stats_with_ai, &config).unwrap();

        assert!(result.contains("📝 Test"));
        assert!(result.contains("📊"));
    }

    #[test]
    fn test_no_ai_code_returns_original() {
        // Test WITHOUT AI code - should return original message unchanged
        let config = CommitMessageConfig::test_enabled();
        let stats_no_ai = CommitStats {
            human_additions: 10,
            mixed_additions: 0,
            ai_additions: 0,  // No AI code
            ai_accepted: 0,
            total_ai_additions: 0,
            total_ai_deletions: 0,
            time_waiting_for_ai: 0,
            git_diff_deleted_lines: 0,
            git_diff_added_lines: 10,
            tool_model_breakdown: BTreeMap::new(),
        };

        let original = "Test commit";
        let result = format_commit_message(original, &stats_no_ai, &config).unwrap();

        // Should return original message unchanged (no stats added)
        assert_eq!(result, "Test commit\n");
    }

    #[test]
    fn test_markdown_no_ai_returns_original() {
        // Test markdown format WITHOUT AI code
        let mut config = CommitMessageConfig::test_enabled();
        config.format = "markdown".to_string();

        let stats_no_ai = CommitStats {
            human_additions: 10,
            mixed_additions: 0,
            ai_additions: 0,  // No AI code
            ai_accepted: 0,
            total_ai_additions: 0,
            total_ai_deletions: 0,
            time_waiting_for_ai: 0,
            git_diff_deleted_lines: 0,
            git_diff_added_lines: 10,
            tool_model_breakdown: BTreeMap::new(),
        };

        let original = "Test commit";
        let result = format_commit_message(original, &stats_no_ai, &config).unwrap();

        // Should return original message unchanged
        assert_eq!(result, "Test commit\n");
    }
}
