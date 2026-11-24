//! Kilo Code installer.
//!
//! Kilo Code (the `kilo` CLI and the Kilo Code VS Code extension, which runs
//! `kilo serve` as its backend) is a fork of OpenCode. It loads global plugins
//! from `~/.config/kilo/plugins/` and calls the OpenCode V1 plugin API, so it
//! shares OpenCode's plugin and installer; only names and detection differ.

use super::opencode::OpenCodePluginAgent;
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
}

impl OpenCodePluginAgent for KiloInstaller {
    const NAME: &'static str = "Kilo Code";
    const ID: &'static str = "kilo";
    const PROCESS_NAMES: &'static [&'static str] = &["kilo"];

    fn tool_installed() -> bool {
        binary_exists("kilo")
            || home_dir().join(".config").join("kilo").exists()
            || home_dir()
                .join(".local")
                .join("share")
                .join("kilo")
                .exists()
            || Path::new(".kilo").exists()
            || Path::new(".kilocode").exists()
            || Self::has_vscode_extension()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn test_kilo_plugin_path() {
        let path = KiloInstaller::plugin_path();
        assert!(path.ends_with(".config/kilo/plugins/git-ai.ts"));
    }
}
