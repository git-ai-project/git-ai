use crate::config::skills_dir_path;
use crate::error::GitAiError;
use crate::mdm::utils::write_atomic;
use serde::{Deserialize, Serialize};
use std::fs;

/// Embedded command files - each command has a name, description, and its .md content
struct EmbeddedCommand {
    name: &'static str,
    description: &'static str,
    command_md: &'static str,
}

/// All embedded commands - add new commands here
const EMBEDDED_COMMANDS: &[EmbeddedCommand] = &[EmbeddedCommand {
    name: "prompt-analysis",
    description: "Analyze AI prompting patterns and acceptance rates",
    command_md: include_str!("../../skills/prompt-analysis/SKILL.md"),
}];

/// Marketplace JSON structure
#[derive(Serialize, Deserialize)]
struct Marketplace {
    name: String,
    owner: MarketplaceOwner,
    metadata: MarketplaceMetadata,
    plugins: Vec<MarketplacePlugin>,
}

#[derive(Serialize, Deserialize)]
struct MarketplaceOwner {
    name: String,
}

#[derive(Serialize, Deserialize)]
struct MarketplaceMetadata {
    description: String,
    version: String,
    #[serde(rename = "pluginRoot")]
    plugin_root: String,
}

#[derive(Serialize, Deserialize)]
struct MarketplacePlugin {
    name: String,
    source: String,
    description: String,
    version: String,
    category: String,
    keywords: Vec<String>,
}

/// Plugin JSON structure (for .claude-plugin/plugin.json inside each plugin)
#[derive(Serialize, Deserialize)]
struct PluginJson {
    name: String,
    description: String,
    author: PluginAuthor,
}

#[derive(Serialize, Deserialize)]
struct PluginAuthor {
    name: String,
}

/// The name of the single plugin that contains all git-ai commands
const PLUGIN_NAME: &str = "git-ai";
const PLUGIN_DESCRIPTION: &str = "Official Git AI commands for AI-assisted development analytics";

/// Generate the marketplace.json content
/// The marketplace contains a single "git-ai" plugin that holds all commands
fn generate_marketplace() -> Marketplace {
    Marketplace {
        name: "git-ai".to_string(),
        owner: MarketplaceOwner {
            name: "Git AI".to_string(),
        },
        metadata: MarketplaceMetadata {
            description: "Official Git AI skills for your Agents".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            plugin_root: ".".to_string(),
        },
        plugins: vec![MarketplacePlugin {
            name: PLUGIN_NAME.to_string(),
            source: format!("./{}", PLUGIN_NAME),
            description: PLUGIN_DESCRIPTION.to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            category: "ai-tools".to_string(),
            keywords: vec!["git-ai".to_string(), "prompts".to_string(), "analytics".to_string()],
        }],
    }
}

/// Generate plugin.json content for the git-ai plugin
fn generate_plugin_json() -> PluginJson {
    PluginJson {
        name: PLUGIN_NAME.to_string(),
        description: PLUGIN_DESCRIPTION.to_string(),
        author: PluginAuthor {
            name: "Git AI".to_string(),
        },
    }
}

/// Result of installing skills
pub struct SkillsInstallResult {
    /// Whether any changes were made
    pub changed: bool,
    /// Number of skills installed
    pub installed_count: usize,
}

/// Install all embedded commands to ~/.git-ai/skills/
/// This nukes the entire skills directory and recreates it fresh each time.
///
/// Creates the proper Claude Code plugin structure:
/// ~/.git-ai/skills/           (marketplace)
/// ├── .claude-plugin/
/// │   └── marketplace.json
/// └── git-ai/                 (single plugin containing all commands)
///     ├── .claude-plugin/
///     │   └── plugin.json
///     └── commands/
///         └── prompt-analysis.md
///         └── (future commands...)
pub fn install_skills(dry_run: bool, _verbose: bool) -> Result<SkillsInstallResult, GitAiError> {
    let skills_base = skills_dir_path()
        .ok_or_else(|| GitAiError::Generic("Could not determine skills directory path".to_string()))?;

    if dry_run {
        return Ok(SkillsInstallResult {
            changed: true,
            installed_count: EMBEDDED_COMMANDS.len(),
        });
    }

    // Nuke the skills directory if it exists
    if skills_base.exists() {
        fs::remove_dir_all(&skills_base)?;
    }

    // Create fresh skills directory (this is the marketplace root)
    fs::create_dir_all(&skills_base)?;

    // Write .claude-plugin/marketplace.json at marketplace root
    let marketplace_plugin_dir = skills_base.join(".claude-plugin");
    fs::create_dir_all(&marketplace_plugin_dir)?;

    let marketplace = generate_marketplace();
    let marketplace_content = serde_json::to_string_pretty(&marketplace)
        .map_err(|e| GitAiError::Generic(format!("Failed to serialize marketplace.json: {}", e)))?;
    let marketplace_path = marketplace_plugin_dir.join("marketplace.json");
    write_atomic(&marketplace_path, marketplace_content.as_bytes())?;

    // Create the single "git-ai" plugin directory
    let plugin_dir = skills_base.join(PLUGIN_NAME);
    fs::create_dir_all(&plugin_dir)?;

    // Create .claude-plugin/plugin.json inside the git-ai plugin
    let plugin_claude_dir = plugin_dir.join(".claude-plugin");
    fs::create_dir_all(&plugin_claude_dir)?;

    let plugin_json = generate_plugin_json();
    let plugin_json_content = serde_json::to_string_pretty(&plugin_json)
        .map_err(|e| GitAiError::Generic(format!("Failed to serialize plugin.json: {}", e)))?;
    let plugin_json_path = plugin_claude_dir.join("plugin.json");
    write_atomic(&plugin_json_path, plugin_json_content.as_bytes())?;

    // Create commands/ directory inside the git-ai plugin
    let commands_dir = plugin_dir.join("commands");
    fs::create_dir_all(&commands_dir)?;

    // Install all commands into the single git-ai plugin
    for cmd in EMBEDDED_COMMANDS {
        let command_md_path = commands_dir.join(format!("{}.md", cmd.name));
        write_atomic(&command_md_path, cmd.command_md.as_bytes())?;
    }

    Ok(SkillsInstallResult {
        changed: true,
        installed_count: EMBEDDED_COMMANDS.len(),
    })
}

/// Uninstall all skills by removing ~/.git-ai/skills/
pub fn uninstall_skills(dry_run: bool, _verbose: bool) -> Result<SkillsInstallResult, GitAiError> {
    let skills_base = skills_dir_path()
        .ok_or_else(|| GitAiError::Generic("Could not determine skills directory path".to_string()))?;

    if !skills_base.exists() {
        return Ok(SkillsInstallResult {
            changed: false,
            installed_count: 0,
        });
    }

    if dry_run {
        return Ok(SkillsInstallResult {
            changed: true,
            installed_count: EMBEDDED_COMMANDS.len(),
        });
    }

    // Nuke the entire skills directory
    fs::remove_dir_all(&skills_base)?;

    Ok(SkillsInstallResult {
        changed: true,
        installed_count: EMBEDDED_COMMANDS.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_embedded_commands_are_loaded() {
        // Verify that the embedded commands are not empty
        for cmd in EMBEDDED_COMMANDS {
            assert!(!cmd.name.is_empty(), "Command name should not be empty");
            assert!(
                !cmd.command_md.is_empty(),
                "Command {} .md should not be empty",
                cmd.name
            );
            assert!(
                cmd.command_md.contains("---"),
                "Command {} should have frontmatter",
                cmd.name
            );
        }
    }

    #[test]
    fn test_skills_dir_path_is_under_git_ai() {
        if let Some(path) = skills_dir_path() {
            assert!(path.ends_with("skills"));
            let parent = path.parent().unwrap();
            assert!(parent.ends_with(".git-ai"));
        }
    }
}
