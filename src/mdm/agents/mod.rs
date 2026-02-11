mod claude_code;
mod codex;
mod cursor;
mod droid;
mod gemini;
mod jetbrains;
mod opencode;
mod vscode;

pub use claude_code::ClaudeCodeInstaller;
pub use codex::CodexInstaller;
pub use cursor::CursorInstaller;
pub use droid::DroidInstaller;
pub use gemini::GeminiInstaller;
pub use jetbrains::JetBrainsInstaller;
pub use opencode::OpenCodeInstaller;
pub use vscode::VSCodeInstaller;

use super::hook_installer::HookInstaller;

/// Get all available hook installers
pub fn get_all_installers() -> Vec<Box<dyn HookInstaller>> {
    vec![
        Box::new(ClaudeCodeInstaller),
        Box::new(CodexInstaller),
        Box::new(CursorInstaller),
        Box::new(VSCodeInstaller),
        Box::new(OpenCodeInstaller),
        Box::new(GeminiInstaller),
        Box::new(DroidInstaller),
        Box::new(JetBrainsInstaller),
    ]
}
