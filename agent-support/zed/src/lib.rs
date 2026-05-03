/// git-ai Zed extension
///
/// # File-save hook approach
///
/// The Zed WASM extension API (as of v0.5/v0.7) does **not** expose file-save
/// callbacks, `on_save`, or buffer-change events.  The full Extension trait
/// only provides hooks for language servers, debug adapters, slash commands,
/// and similar IDE-integration surfaces (see
/// https://docs.rs/zed_extension_api/0.7.0/zed_extension_api/trait.Extension.html).
///
/// Therefore, the known_human checkpoint is fired via Zed's built-in
/// `format_on_save` external-command formatter.  The `ZedInstaller` (in
/// `src/mdm/agents/zed.rs`) writes a per-language settings snippet to
/// `~/.config/zed/settings.json` that wires up the git-ai-zed-hook script as
/// the formatter, and also installs the wrapper script itself.
///
/// The wrapper script (`git-ai-zed-hook.sh`):
///   1. Reads the full file content from stdin (Zed passes it as stdin to the
///      formatter).
///   2. Emits that content unchanged to stdout (so Zed sees no formatting
///      change).
///   3. Fires `git-ai checkpoint known_human --hook-input stdin` in the
///      background with a JSON payload that matches the cross-IDE spec.
///
/// This extension crate is compiled to WASM and placed in the Zed extensions
/// directory so Zed will load it; the struct below satisfies the trait
/// requirement with no-op implementations.
use zed_extension_api::{self as zed, Extension, Result};

struct GitAiExtension;

impl Extension for GitAiExtension {
    fn new() -> Self {
        GitAiExtension
    }
}

zed::register_extension!(GitAiExtension);
