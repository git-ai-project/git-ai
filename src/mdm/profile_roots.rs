use crate::config::Config;
use crate::mdm::utils::home_dir;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AgentProfile {
    Claude,
    Codex,
    Cursor,
    Droid,
    Gemini,
    ContinueCli,
    CopilotCli,
    Amp,
}

impl AgentProfile {
    pub fn config_key(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Cursor => "cursor",
            Self::Droid => "droid",
            Self::Gemini => "gemini",
            Self::ContinueCli => "continue_cli",
            Self::CopilotCli => "copilot_cli",
            Self::Amp => "amp",
        }
    }
}

pub fn agent_profile_roots(agent: AgentProfile, config: &Config) -> Vec<PathBuf> {
    let home = home_dir();
    let environment_value = |name: &str| {
        std::env::var(name)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(PathBuf::from)
    };
    let environment = match agent {
        AgentProfile::Claude => environment_value("CLAUDE_CONFIG_DIR"),
        AgentProfile::Codex => environment_value("CODEX_HOME"),
        AgentProfile::Cursor => environment_value("CURSOR_CONFIG_DIR"),
        AgentProfile::Droid | AgentProfile::ContinueCli | AgentProfile::CopilotCli => None,
        AgentProfile::Gemini => {
            environment_value("GEMINI_CLI_HOME").map(|path| path.join(".gemini"))
        }
        AgentProfile::Amp => std::env::var("GIT_AI_AMP_THREADS_PATH")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(PathBuf::from)
            .and_then(|path| path.parent().map(Path::to_path_buf)),
    };
    let default = official_default_root_for_home(agent, &home);
    let configured = config
        .agent_profile_roots()
        .get(agent.config_key())
        .map(Vec::as_slice)
        .unwrap_or_default();

    merge_profile_roots(configured, environment.as_deref(), &default, &home)
}

pub fn official_default_root(agent: AgentProfile) -> PathBuf {
    official_default_root_for_home(agent, &home_dir())
}

fn official_default_root_for_home(agent: AgentProfile, home: &Path) -> PathBuf {
    match agent {
        AgentProfile::Claude => home.join(".claude"),
        AgentProfile::Codex => home.join(".codex"),
        AgentProfile::Cursor => home.join(".cursor"),
        AgentProfile::Droid => home.join(".factory"),
        AgentProfile::Gemini => home.join(".gemini"),
        AgentProfile::ContinueCli => home.join(".continue"),
        AgentProfile::CopilotCli => home.join(".copilot"),
        AgentProfile::Amp => amp_data_root(home),
    }
}

#[cfg(not(target_os = "windows"))]
fn amp_data_root(home: &Path) -> PathBuf {
    std::env::var("XDG_DATA_HOME")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local/share"))
        .join("amp")
}

#[cfg(target_os = "windows")]
fn amp_data_root(home: &Path) -> PathBuf {
    std::env::var("LOCALAPPDATA")
        .or_else(|_| std::env::var("APPDATA"))
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join("AppData/Local"))
        .join("amp")
}

fn merge_profile_roots(
    configured: &[String],
    environment: Option<&Path>,
    default: &Path,
    home: &Path,
) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let mut seen = HashSet::new();

    let configured = configured.iter().filter_map(|value| {
        let value = value.trim();
        if value == "~" {
            Some(home.to_path_buf())
        } else if let Some(relative) = value.strip_prefix("~/") {
            Some(home.join(relative))
        } else {
            let path = PathBuf::from(value);
            path.is_absolute().then_some(path)
        }
    });

    for root in configured
        .chain(
            environment
                .filter(|path| path.is_absolute())
                .map(Path::to_path_buf),
        )
        .chain(std::iter::once(default.to_path_buf()))
    {
        let identity = fs::canonicalize(&root).unwrap_or_else(|_| root.clone());
        if seen.insert(identity) {
            roots.push(root);
        }
    }

    roots
}

#[cfg(test)]
mod tests {
    use super::{
        AgentProfile, agent_profile_roots, merge_profile_roots, official_default_root_for_home,
    };
    use crate::config::Config;
    use std::fs;

    #[test]
    fn configured_environment_and_default_roots_are_merged_without_discovery() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        let default = home.join(".codex");
        let configured = home.join(".codex-personal2");
        let environment = home.join(".codex-work");
        let unconfigured_sibling = home.join(".codex-personal");
        for path in [&default, &configured, &environment, &unconfigured_sibling] {
            fs::create_dir_all(path).unwrap();
        }

        let configured_values = vec![configured.to_string_lossy().into_owned()];
        let roots = merge_profile_roots(
            &configured_values,
            Some(environment.as_path()),
            &default,
            home,
        );

        assert_eq!(roots, vec![configured, environment, default]);
        assert!(!roots.contains(&unconfigured_sibling));
    }

    #[test]
    fn roots_expand_home_ignore_relative_paths_and_deduplicate() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        let default = home.join(".codex");
        let configured = vec![
            "~/.codex-personal2".to_string(),
            "relative/profile".to_string(),
            "~/.codex-personal2".to_string(),
        ];

        let roots = merge_profile_roots(&configured, Some(&default), &default, home);

        assert_eq!(roots, vec![home.join(".codex-personal2"), default]);
    }

    #[test]
    #[serial_test::serial]
    fn codex_roots_include_config_environment_and_official_default() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        let config_dir = home.join(".git-ai");
        fs::create_dir_all(&config_dir).unwrap();
        fs::write(
            config_dir.join("config.json"),
            r#"{
                "agent_profile_roots": {
                    "codex": ["~/.codex-personal2"]
                }
            }"#,
        )
        .unwrap();
        let environment_root = home.join(".codex-work");

        let previous_home = std::env::var_os("HOME");
        let previous_codex_home = std::env::var_os("CODEX_HOME");
        unsafe {
            std::env::set_var("HOME", home);
            std::env::set_var("CODEX_HOME", &environment_root);
        }
        let config = Config::fresh();
        let roots = agent_profile_roots(AgentProfile::Codex, &config);
        match previous_home {
            Some(value) => unsafe { std::env::set_var("HOME", value) },
            None => unsafe { std::env::remove_var("HOME") },
        }
        match previous_codex_home {
            Some(value) => unsafe { std::env::set_var("CODEX_HOME", value) },
            None => unsafe { std::env::remove_var("CODEX_HOME") },
        }

        assert_eq!(
            roots,
            vec![
                home.join(".codex-personal2"),
                environment_root,
                home.join(".codex")
            ]
        );
    }

    #[test]
    fn every_sweep_agent_has_a_stable_config_key() {
        assert_eq!(AgentProfile::Claude.config_key(), "claude");
        assert_eq!(AgentProfile::Codex.config_key(), "codex");
        assert_eq!(AgentProfile::Cursor.config_key(), "cursor");
        assert_eq!(AgentProfile::Droid.config_key(), "droid");
        assert_eq!(AgentProfile::Gemini.config_key(), "gemini");
        assert_eq!(AgentProfile::ContinueCli.config_key(), "continue_cli");
        assert_eq!(AgentProfile::CopilotCli.config_key(), "copilot_cli");
        assert_eq!(AgentProfile::Amp.config_key(), "amp");
    }

    #[test]
    fn every_sweep_agent_keeps_its_official_default() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        assert_eq!(
            official_default_root_for_home(AgentProfile::Claude, home),
            home.join(".claude")
        );
        assert_eq!(
            official_default_root_for_home(AgentProfile::Codex, home),
            home.join(".codex")
        );
        assert_eq!(
            official_default_root_for_home(AgentProfile::Cursor, home),
            home.join(".cursor")
        );
        assert_eq!(
            official_default_root_for_home(AgentProfile::Droid, home),
            home.join(".factory")
        );
        assert_eq!(
            official_default_root_for_home(AgentProfile::Gemini, home),
            home.join(".gemini")
        );
        assert_eq!(
            official_default_root_for_home(AgentProfile::ContinueCli, home),
            home.join(".continue")
        );
        assert_eq!(
            official_default_root_for_home(AgentProfile::CopilotCli, home),
            home.join(".copilot")
        );
        assert!(official_default_root_for_home(AgentProfile::Amp, home).ends_with("amp"));
    }
}
