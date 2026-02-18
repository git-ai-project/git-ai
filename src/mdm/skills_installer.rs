use crate::config::skills_dir_path;
use crate::error::GitAiError;
use crate::mdm::utils::write_atomic;
use std::fs;
use std::path::PathBuf;

/// Embedded skill - each skill has a name and its SKILL.md content
struct EmbeddedSkill {
    name: &'static str,
    skill_md: &'static str,
}

/// All embedded skills - add new skills here
const EMBEDDED_SKILLS: &[EmbeddedSkill] = &[
    EmbeddedSkill {
        name: "prompt-analysis",
        skill_md: include_str!("../../skills/prompt-analysis/SKILL.md"),
    },
    EmbeddedSkill {
        name: "git-ai-search",
        skill_md: include_str!("../../skills/git-ai-search/SKILL.md"),
    },
];

/// Result of installing skills
pub struct SkillsInstallResult {
    /// Whether any changes were made
    pub changed: bool,
    /// Number of skills installed
    #[allow(dead_code)]
    pub installed_count: usize,
}

/// Get the ~/.agents/skills directory path
fn agents_skills_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".agents").join("skills"))
}

/// Get the ~/.claude/skills directory path
fn claude_skills_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude").join("skills"))
}

/// Create a symlink from link_path to target, removing any existing file/symlink first
fn create_skills_symlink(target: &PathBuf, link_path: &PathBuf) -> Result<(), GitAiError> {
    // Create parent directory if needed
    if let Some(parent) = link_path.parent() {
        fs::create_dir_all(parent)?;
    }

    // Remove existing file/symlink if present
    if link_path.exists() || link_path.symlink_metadata().is_ok() {
        if link_path.is_dir()
            && !link_path
                .symlink_metadata()
                .map(|m| m.file_type().is_symlink())
                .unwrap_or(false)
        {
            // It's a real directory, not a symlink - remove it
            fs::remove_dir_all(link_path)?;
        } else {
            // It's a file or symlink
            fs::remove_file(link_path)?;
        }
    }

    // Create the symlink
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link_path)?;

    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(target, link_path)?;

    Ok(())
}

/// Remove a symlink if it exists
fn remove_skills_symlink(link_path: &PathBuf) -> Result<(), GitAiError> {
    if link_path.symlink_metadata().is_ok()
        && link_path
            .symlink_metadata()
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
    {
        fs::remove_file(link_path)?;
    }
    Ok(())
}

/// Install all embedded skills to ~/.git-ai/skills/
/// This nukes the entire skills directory and recreates it fresh each time.
///
/// Creates the standard skills structure:
/// ~/.git-ai/skills/
/// └── prompt-analysis/
///     └── SKILL.md
///
/// Then symlinks each skill to:
/// - ~/.agents/skills/{skill-name}
/// - ~/.claude/skills/{skill-name}
pub fn install_skills(dry_run: bool, _verbose: bool) -> Result<SkillsInstallResult, GitAiError> {
    let skills_base = skills_dir_path().ok_or_else(|| {
        GitAiError::Generic("Could not determine skills directory path".to_string())
    })?;

    if dry_run {
        return Ok(SkillsInstallResult {
            changed: true,
            installed_count: EMBEDDED_SKILLS.len(),
        });
    }

    // Nuke the skills directory if it exists
    if skills_base.exists() {
        fs::remove_dir_all(&skills_base)?;
    }

    // Create fresh skills directory
    fs::create_dir_all(&skills_base)?;

    // Install each skill
    for skill in EMBEDDED_SKILLS {
        // Create skill directory: ~/.git-ai/skills/{skill-name}/
        let skill_dir = skills_base.join(skill.name);
        fs::create_dir_all(&skill_dir)?;

        // Write SKILL.md
        let skill_md_path = skill_dir.join("SKILL.md");
        write_atomic(&skill_md_path, skill.skill_md.as_bytes())?;

        // Create symlinks for this skill
        // ~/.agents/skills/{skill-name} -> ~/.git-ai/skills/{skill-name}
        if let Some(agents_dir) = agents_skills_dir() {
            let agents_link = agents_dir.join(skill.name);
            if let Err(e) = create_skills_symlink(&skill_dir, &agents_link) {
                eprintln!(
                    "Warning: Failed to create symlink at {:?}: {}",
                    agents_link, e
                );
            }
        }

        // ~/.claude/skills/{skill-name} -> ~/.git-ai/skills/{skill-name}
        if let Some(claude_dir) = claude_skills_dir() {
            let claude_link = claude_dir.join(skill.name);
            if let Err(e) = create_skills_symlink(&skill_dir, &claude_link) {
                eprintln!(
                    "Warning: Failed to create symlink at {:?}: {}",
                    claude_link, e
                );
            }
        }
    }

    Ok(SkillsInstallResult {
        changed: true,
        installed_count: EMBEDDED_SKILLS.len(),
    })
}

/// Uninstall all skills by removing ~/.git-ai/skills/ and symlinks
pub fn uninstall_skills(dry_run: bool, _verbose: bool) -> Result<SkillsInstallResult, GitAiError> {
    let skills_base = skills_dir_path().ok_or_else(|| {
        GitAiError::Generic("Could not determine skills directory path".to_string())
    })?;

    if !skills_base.exists() {
        return Ok(SkillsInstallResult {
            changed: false,
            installed_count: 0,
        });
    }

    if dry_run {
        return Ok(SkillsInstallResult {
            changed: true,
            installed_count: EMBEDDED_SKILLS.len(),
        });
    }

    // Remove symlinks for each skill first
    for skill in EMBEDDED_SKILLS {
        // ~/.agents/skills/{skill-name}
        if let Some(agents_dir) = agents_skills_dir() {
            let agents_link = agents_dir.join(skill.name);
            if let Err(e) = remove_skills_symlink(&agents_link) {
                eprintln!(
                    "Warning: Failed to remove symlink at {:?}: {}",
                    agents_link, e
                );
            }
        }

        // ~/.claude/skills/{skill-name}
        if let Some(claude_dir) = claude_skills_dir() {
            let claude_link = claude_dir.join(skill.name);
            if let Err(e) = remove_skills_symlink(&claude_link) {
                eprintln!(
                    "Warning: Failed to remove symlink at {:?}: {}",
                    claude_link, e
                );
            }
        }
    }

    // Nuke the entire skills directory
    fs::remove_dir_all(&skills_base)?;

    Ok(SkillsInstallResult {
        changed: true,
        installed_count: EMBEDDED_SKILLS.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_embedded_skills_are_loaded() {
        // Verify that the embedded skills are not empty
        for skill in EMBEDDED_SKILLS {
            assert!(!skill.name.is_empty(), "Skill name should not be empty");
            assert!(
                !skill.skill_md.is_empty(),
                "Skill {} SKILL.md should not be empty",
                skill.name
            );
            assert!(
                skill.skill_md.contains("---"),
                "Skill {} should have frontmatter",
                skill.name
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
