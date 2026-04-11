mod amp;
mod claude_code;
mod codex;
mod cursor;
mod droid;
mod firebender;
mod gemini;
mod github_copilot;
mod jetbrains;
mod opencode;
mod vscode;
mod windsurf;
#[cfg(target_os = "macos")]
mod xcode;

pub use amp::AmpInstaller;
pub use claude_code::ClaudeCodeInstaller;
pub use codex::CodexInstaller;
pub use cursor::CursorInstaller;
pub use droid::DroidInstaller;
pub use firebender::FirebenderInstaller;
pub use gemini::GeminiInstaller;
pub use github_copilot::GitHubCopilotInstaller;
pub use jetbrains::JetBrainsInstaller;
pub use opencode::OpenCodeInstaller;
pub use vscode::VSCodeInstaller;
pub use windsurf::WindsurfInstaller;
#[cfg(target_os = "macos")]
pub use xcode::XcodeInstaller;

use super::hook_installer::HookInstaller;

/// Get all available hook installers
pub fn get_all_installers() -> Vec<Box<dyn HookInstaller>> {
    vec![
        Box::new(ClaudeCodeInstaller),
        Box::new(CodexInstaller),
        Box::new(CursorInstaller),
        Box::new(VSCodeInstaller),
        Box::new(GitHubCopilotInstaller),
        Box::new(AmpInstaller),
        Box::new(OpenCodeInstaller),
        Box::new(GeminiInstaller),
        Box::new(DroidInstaller),
        Box::new(FirebenderInstaller),
        Box::new(JetBrainsInstaller),
        Box::new(WindsurfInstaller),
        #[cfg(target_os = "macos")]
        Box::new(XcodeInstaller),
    ]
}
