//! Kilo Code installer.
//!
//! Kilo Code (the `kilo` CLI, and the VS Code and JetBrains plugins, which run
//! `kilo serve` as their backend) is a fork of OpenCode. It loads global plugins
//! from `$XDG_CONFIG_HOME/kilo/plugins/` and calls the OpenCode V1 plugin API, so it
//! shares OpenCode's plugin and installer; only names and detection differ.

use super::opencode::OpenCodePluginAgent;
use crate::mdm::jetbrains::{DetectedIde, find_jetbrains_installations};
use crate::mdm::utils::{binary_exists, home_dir};
use std::fs;
use std::path::Path;

/// VS Code-family editors whose extension dirs may contain Kilo Code.
const EDITOR_EXTENSION_DIRS: &[&str] = &[
    ".vscode",
    ".vscode-insiders",
    ".cursor",
    ".windsurf",
    ".vscode-oss",
];

/// Marketplace id of the Kilo Code extension (`publisher.name`).
const KILO_EXTENSION_PREFIX: &str = "kilocode.kilo-code-";

/// Directory of Kilo Code for JetBrains (`ai.kilocode.jetbrains`) in an IDE's plugins dir.
const KILO_JETBRAINS_PLUGIN_DIR: &str = "kilo.jetbrains";

pub struct KiloInstaller;

impl KiloInstaller {
    fn has_vscode_extension() -> bool {
        let home = home_dir();
        EDITOR_EXTENSION_DIRS.iter().any(|editor| {
            let Ok(entries) = fs::read_dir(home.join(editor).join("extensions")) else {
                return false;
            };
            entries.flatten().any(|entry| {
                entry.file_name().to_str().is_some_and(|name| {
                    name.to_ascii_lowercase().starts_with(KILO_EXTENSION_PREFIX)
                })
            })
        })
    }

    /// The JetBrains plugin runs its own `kilo` binary, so before its first
    /// start the plugin dir is the only trace of Kilo.
    fn has_jetbrains_plugin(ides: &[DetectedIde]) -> bool {
        ides.iter()
            .any(|ide| ide.plugins_dir.join(KILO_JETBRAINS_PLUGIN_DIR).exists())
    }
}

impl OpenCodePluginAgent for KiloInstaller {
    const NAME: &'static str = "Kilo Code";
    const ID: &'static str = "kilo";
    const PROCESS_NAMES: &'static [&'static str] = &["kilo"];

    fn tool_installed() -> bool {
        binary_exists("kilo")
            || Self::config_dir().exists()
            || home_dir()
                .join(".local")
                .join("share")
                .join("kilo")
                .exists()
            || Path::new(".kilo").exists()
            || Path::new(".kilocode").exists()
            || Self::has_vscode_extension()
            || Self::has_jetbrains_plugin(&find_jetbrains_installations())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mdm::jetbrains::ide_types::JETBRAINS_IDES;
    use std::path::PathBuf;

    #[test]
    fn test_kilo_plugin_uses_kilo_preset() {
        let content = KiloInstaller::generate_plugin_content(Path::new("/usr/local/bin/git-ai"));
        assert!(content.contains(r#"const GIT_AI_BIN = "/usr/local/bin/git-ai""#));
        assert!(content.contains(r#"const PRESET = "kilo""#));
        assert!(!content.contains("__GIT_AI_PRESET__"));
        assert!(!content.contains("__GIT_AI_BINARY_PATH__"));
        // Kilo calls the V1 `server()` entrypoint.
        assert!(content.contains("async server(ctx?: V1PluginContext)"));
        assert!(content.contains("\"tool.execute.before\""));
    }

    #[test]
    fn test_kilo_detected_from_jetbrains_plugin_alone() {
        let plugins_dir = tempfile::TempDir::new().unwrap();
        let ides = [DetectedIde {
            ide: &JETBRAINS_IDES[0],
            install_path: PathBuf::from("/opt/idea"),
            binary_path: PathBuf::from("/opt/idea/bin/idea.sh"),
            build_number: Some("263.7307.200".to_string()),
            major_build: Some(263),
            plugins_dir: plugins_dir.path().to_path_buf(),
        }];
        assert!(!KiloInstaller::has_jetbrains_plugin(&ides));
        fs::create_dir_all(plugins_dir.path().join("kilo.jetbrains")).unwrap();
        assert!(KiloInstaller::has_jetbrains_plugin(&ides));
    }

    #[test]
    fn test_kilo_plugin_path() {
        let path = KiloInstaller::plugin_path();
        assert!(path.ends_with(".config/kilo/plugins/git-ai.ts"));
    }
}
