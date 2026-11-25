use super::{
    AgentPreset, ParsedHookEvent, PostBashCall, PostFileEdit, PreBashCall, PreFileEdit,
    PresetContext, StreamFormat, StreamSource,
};
use crate::authorship::authorship_log_serialization::generate_session_id;
use crate::authorship::working_log::AgentId;
use crate::commands::checkpoint_agent::bash_tool::{self, Agent, ToolClass};
use crate::error::GitAiError;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub struct OpenCodePreset;

/// Kilo Code (CLI and the VS Code extension, which runs `kilo serve`) is an
/// OpenCode fork: same plugin hooks and SQLite schema, different names/paths.
pub struct KiloPreset;

/// Per-fork naming for agents in the OpenCode family.
pub(crate) struct OpenCodeFamily {
    /// Tool name recorded in attribution (`AgentId.tool`).
    pub tool: &'static str,
    /// Directory name under `$XDG_DATA_HOME` / `~/.local/share`.
    pub data_dir_name: &'static str,
    /// SQLite database filename inside the data dir.
    pub db_filename: &'static str,
    /// Env var that overrides the data dir (used by tests).
    pub storage_env_var: &'static str,
    /// Env var the agent reads its DB path from: absolute, or relative to the data dir.
    pub db_env_var: Option<&'static str>,
    /// Pre-rename channel DB stem the agent still uses (`<stem>-<channel>.db`).
    pub legacy_channel_db_stem: Option<&'static str>,
}

pub(crate) static OPENCODE_FAMILY: OpenCodeFamily = OpenCodeFamily {
    tool: "opencode",
    data_dir_name: "opencode",
    db_filename: "opencode.db",
    storage_env_var: "GIT_AI_OPENCODE_STORAGE_PATH",
    db_env_var: None,
    legacy_channel_db_stem: None,
};

pub(crate) static KILO_FAMILY: OpenCodeFamily = OpenCodeFamily {
    tool: "kilo",
    data_dir_name: "kilo",
    db_filename: "kilo.db",
    storage_env_var: "GIT_AI_KILO_STORAGE_PATH",
    db_env_var: Some("KILO_DB"),
    legacy_channel_db_stem: Some("opencode"),
};

#[derive(Debug, Deserialize)]
struct OpenCodeHookInput {
    hook_event_name: String,
    session_id: String,
    cwd: String,
    tool_input: Option<serde_json::Value>,
    #[serde(default)]
    tool_name: Option<String>,
    #[serde(default, alias = "toolUseId")]
    tool_use_id: Option<String>,
}

impl OpenCodePreset {
    pub(crate) fn extract_filepaths_from_tool_input(
        tool_input: Option<&serde_json::Value>,
        cwd: &str,
    ) -> Vec<PathBuf> {
        let mut raw_paths = Vec::new();

        if let Some(value) = tool_input {
            Self::collect_tool_paths(value, &mut raw_paths);
        }

        let mut normalized_paths = Vec::new();
        for raw in raw_paths {
            if let Some(path) = Self::normalize_hook_path(&raw, cwd) {
                let pb = PathBuf::from(&path);
                if !normalized_paths.contains(&pb) {
                    normalized_paths.push(pb);
                }
            }
        }

        normalized_paths
    }

    fn collect_tool_paths(value: &serde_json::Value, out: &mut Vec<String>) {
        match value {
            serde_json::Value::Object(map) => {
                for (key, val) in map {
                    let key_lower = key.to_ascii_lowercase();
                    let is_single_path_key = key_lower == "file_path"
                        || key_lower == "filepath"
                        || key_lower == "path"
                        || key_lower == "fspath";

                    let is_multi_path_key = key_lower == "files"
                        || key_lower == "filepaths"
                        || key_lower == "file_paths";

                    if is_single_path_key {
                        if let Some(path) = val.as_str() {
                            out.push(path.to_string());
                        }
                    } else if is_multi_path_key {
                        match val {
                            serde_json::Value::String(path) => out.push(path.to_string()),
                            serde_json::Value::Array(paths) => {
                                for path_value in paths {
                                    if let Some(path) = path_value.as_str() {
                                        out.push(path.to_string());
                                    }
                                }
                            }
                            _ => {}
                        }
                    }

                    Self::collect_tool_paths(val, out);
                }
            }
            serde_json::Value::Array(arr) => {
                for item in arr {
                    Self::collect_tool_paths(item, out);
                }
            }
            serde_json::Value::String(s) => {
                if s.starts_with("file://") {
                    out.push(s.to_string());
                }
                super::parse::collect_apply_patch_paths_from_text(s, out);
            }
            _ => {}
        }
    }

    fn normalize_hook_path(raw_path: &str, cwd: &str) -> Option<String> {
        let trimmed = raw_path.trim();
        if trimmed.is_empty() {
            return None;
        }

        let path_without_scheme = trimmed
            .strip_prefix("file://localhost")
            .or_else(|| trimmed.strip_prefix("file://"))
            .unwrap_or(trimmed);

        let path = Path::new(path_without_scheme);
        let joined = if path.is_absolute()
            || path_without_scheme.starts_with("\\\\")
            || path_without_scheme
                .as_bytes()
                .get(1)
                .map(|b| *b == b':')
                .unwrap_or(false)
        {
            PathBuf::from(path_without_scheme)
        } else {
            Path::new(cwd).join(path_without_scheme)
        };

        Some(joined.to_string_lossy().replace('\\', "/"))
    }

    fn resolve_stream_source(
        family: &OpenCodeFamily,
        session_id: &str,
    ) -> Option<(StreamSource, PathBuf)> {
        let opencode_path = if let Ok(test_path) = std::env::var(family.storage_env_var) {
            PathBuf::from(test_path)
        } else {
            Self::data_path(family).ok()?
        };

        // Try sqlite first
        let db_path = Self::db_override(family, &opencode_path)
            .or_else(|| Self::resolve_sqlite_db_path(family, &opencode_path, session_id));
        if let Some(db_path) = db_path {
            let parent_id = Self::lookup_parent_session(&db_path, session_id);
            return Some((
                StreamSource {
                    path: db_path,
                    format: StreamFormat::OpenCodeSqlite,
                    session_id: generate_session_id(session_id, family.tool),
                    external_session_id: session_id.to_string(),
                    external_parent_session_id: parent_id,
                },
                opencode_path,
            ));
        }

        None
    }

    fn lookup_parent_session(db_path: &Path, session_id: &str) -> Option<String> {
        let conn = crate::streams::agents::opencode::open_sqlite_readonly(db_path).ok()?;
        let table = if crate::streams::agents::opencode::is_v2_session(&conn, session_id) {
            "session_v2"
        } else {
            "session"
        };
        conn.query_row(
            &format!("SELECT parent_id FROM {table} WHERE id = ?"),
            [session_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .ok()
        .flatten()
    }

    #[cfg(test)]
    fn opencode_data_path() -> Result<PathBuf, GitAiError> {
        Self::data_path(&OPENCODE_FAMILY)
    }

    fn data_path(family: &OpenCodeFamily) -> Result<PathBuf, GitAiError> {
        // OpenCode V1 (xdg-basedir) and V2 use this layout on every OS,
        // including Windows. An empty XDG_DATA_HOME falls back to home.
        if let Ok(xdg_data) = std::env::var("XDG_DATA_HOME")
            && !xdg_data.is_empty()
        {
            return Ok(PathBuf::from(xdg_data).join(family.data_dir_name));
        }
        // OpenCode's Bun runtime uses the OS profile when the override is empty.
        let home_variable = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
        let home = std::env::var_os(home_variable)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(dirs::home_dir)
            .ok_or_else(|| GitAiError::Generic("Could not determine home directory".to_string()))?;
        Ok(home.join(".local").join("share").join(family.data_dir_name))
    }

    /// Picks the family DB that holds `session_id`. Besides `<name>.db`, the
    /// CLIs write per-channel DBs (`<name>-<channel>.db`, e.g. `kilo-rc.db`)
    /// unless started with `*_DISABLE_CHANNEL_DB` as the Kilo VS Code extension
    /// does. Falls back to the first candidate when no DB has the session yet.
    fn resolve_sqlite_db_path(
        family: &OpenCodeFamily,
        path: &Path,
        session_id: &str,
    ) -> Option<PathBuf> {
        if path.is_file() {
            return path
                .file_name()
                .and_then(|name| name.to_str())
                .filter(|name| Self::is_family_db(family, name))
                .map(|_| path.to_path_buf());
        }

        if !path.is_dir() {
            return None;
        }

        let mut candidates = Self::family_dbs_in(family, path);
        if path.file_name().and_then(|name| name.to_str()) == Some("storage") {
            candidates.extend(Self::family_dbs_in(family, path.parent()?));
        }
        candidates
            .iter()
            .find(|db| Self::db_has_session(db, session_id))
            .or(candidates.first())
            .cloned()
    }

    /// The DB named by the family's env var (e.g. `KILO_DB`), if it exists.
    fn db_override(family: &OpenCodeFamily, data_dir: &Path) -> Option<PathBuf> {
        let value = std::env::var_os(family.db_env_var?).filter(|value| !value.is_empty())?;
        // `join` keeps an absolute path as is; `:memory:` never names a file.
        let db_path = data_dir.join(value);
        db_path.is_file().then_some(db_path)
    }

    fn is_family_db(family: &OpenCodeFamily, name: &str) -> bool {
        let stem = family.db_filename.trim_end_matches(".db");
        name == family.db_filename
            || (name.ends_with(".db")
                && std::iter::once(stem)
                    .chain(family.legacy_channel_db_stem)
                    .any(|stem| name.starts_with(&format!("{stem}-"))))
    }

    /// `<name>.db` first, then channel DBs in name order.
    fn family_dbs_in(family: &OpenCodeFamily, dir: &Path) -> Vec<PathBuf> {
        let mut channel_dbs: Vec<PathBuf> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .filter(|db| {
                db.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        name != family.db_filename && Self::is_family_db(family, name)
                    })
            })
            .collect();
        channel_dbs.sort();
        let direct_db = dir.join(family.db_filename);
        direct_db
            .exists()
            .then_some(direct_db)
            .into_iter()
            .chain(channel_dbs)
            .collect()
    }

    fn db_has_session(db_path: &Path, session_id: &str) -> bool {
        let Ok(conn) = crate::streams::agents::opencode::open_sqlite_readonly(db_path) else {
            return false;
        };
        crate::streams::agents::opencode::is_v2_session(&conn, session_id)
            || conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM session WHERE id = ?1)",
                    [session_id],
                    |row| row.get(0),
                )
                .unwrap_or(false)
    }
}

impl AgentPreset for OpenCodePreset {
    fn parse(&self, hook_input: &str, trace_id: &str) -> Result<Vec<ParsedHookEvent>, GitAiError> {
        OpenCodePreset::parse_family(&OPENCODE_FAMILY, hook_input, trace_id)
    }
}

impl AgentPreset for KiloPreset {
    fn parse(&self, hook_input: &str, trace_id: &str) -> Result<Vec<ParsedHookEvent>, GitAiError> {
        OpenCodePreset::parse_family(&KILO_FAMILY, hook_input, trace_id)
    }
}

impl OpenCodePreset {
    pub(crate) fn parse_family(
        family: &OpenCodeFamily,
        hook_input: &str,
        trace_id: &str,
    ) -> Result<Vec<ParsedHookEvent>, GitAiError> {
        let hook_input: OpenCodeHookInput = serde_json::from_str(hook_input)
            .map_err(|e| GitAiError::PresetError(format!("Invalid JSON in hook_input: {}", e)))?;

        let is_bash = hook_input
            .tool_name
            .as_deref()
            .map(|name| bash_tool::classify_tool(Agent::OpenCode, name) == ToolClass::Bash)
            .unwrap_or(false);

        let is_pre = hook_input.hook_event_name == "PreToolUse";

        let OpenCodeHookInput {
            hook_event_name: _,
            session_id,
            cwd,
            tool_input,
            tool_name: _,
            tool_use_id,
        } = hook_input;

        let file_paths = Self::extract_filepaths_from_tool_input(tool_input.as_ref(), &cwd);
        let bash_command = tool_input
            .as_ref()
            .and_then(|value| {
                value
                    .get("command")
                    .or_else(|| value.get("cmd"))
                    .and_then(|v| v.as_str())
            })
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToString::to_string);
        let tool_use_id_str = tool_use_id.as_deref().unwrap_or("bash").to_string();

        // Build metadata
        let mut metadata = HashMap::new();
        metadata.insert("session_id".to_string(), session_id.clone());
        if let Ok(test_path) = std::env::var(family.storage_env_var) {
            metadata.insert("__test_storage_path".to_string(), test_path);
        }

        // Resolve transcript source
        let transcript_result = Self::resolve_stream_source(family, &session_id);

        let extracted_model = transcript_result.as_ref().and_then(|(ts, _)| {
            crate::streams::model_extraction::extract_model(
                &ts.path,
                crate::streams::sweep::StreamFormat::OpenCodeSqlite,
                Some(session_id.as_str()),
            )
            .ok()
            .flatten()
        });

        let context = PresetContext {
            agent_id: AgentId {
                tool: family.tool.to_string(),
                id: session_id.clone(),
                model: extracted_model.unwrap_or_else(|| "unknown".to_string()),
            },
            external_session_id: session_id,
            trace_id: trace_id.to_string(),
            cwd: PathBuf::from(&cwd),
            metadata,
        };

        let stream_source = transcript_result.map(|(source, _)| source);

        let event = match (is_pre, is_bash) {
            (true, true) => ParsedHookEvent::PreBashCall(PreBashCall {
                context,
                tool_use_id: tool_use_id_str,
                command: bash_command,
            }),
            (true, false) => ParsedHookEvent::PreFileEdit(PreFileEdit {
                context,
                file_paths,
                dirty_files: None,
                tool_use_id: Some(tool_use_id_str),
            }),
            (false, true) => ParsedHookEvent::PostBashCall(PostBashCall {
                context,
                tool_use_id: tool_use_id_str,
                command: bash_command,
                stream_source,
            }),
            (false, false) => ParsedHookEvent::PostFileEdit(PostFileEdit {
                context,
                file_paths,
                dirty_files: None,
                stream_source,
                tool_use_id: Some(tool_use_id_str),
            }),
        };

        Ok(vec![event])
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_opencode_empty_home_override_uses_os_profile() {
        const CHILD: &str = "GIT_AI_OPENCODE_EMPTY_HOME_PROBE";
        if std::env::var_os(CHILD).is_some() {
            let actual = super::OpenCodePreset::opencode_data_path().unwrap();
            assert!(
                actual.is_absolute(),
                "Empty home must not resolve relative to cwd"
            );
            assert_eq!(
                actual,
                dirs::home_dir().unwrap().join(".local/share/opencode")
            );
            return;
        }
        // Isolate the environment change from parallel tests. This only
        // resolves a path; it never reads or writes the user's OpenCode data.
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("test_opencode_empty_home_override_uses_os_profile")
            .arg("--nocapture")
            .env(CHILD, "1")
            .env_remove("XDG_DATA_HOME")
            .env(if cfg!(windows) { "USERPROFILE" } else { "HOME" }, "")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    use super::*;
    use crate::commands::checkpoint_agent::presets::*;
    use serde_json::json;

    fn make_opencode_input(event: &str, tool: &str) -> String {
        json!({
            "hook_event_name": event,
            "session_id": "oc-sess-123",
            "cwd": "/home/user/project",
            "tool_name": tool,
            "tool_use_id": "tu-1",
            "tool_input": {"file_path": "src/main.rs"}
        })
        .to_string()
    }

    #[test]
    fn test_opencode_pre_file_edit() {
        let input = make_opencode_input("PreToolUse", "edit");
        let events = OpenCodePreset.parse(&input, "t_test").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PreFileEdit(e) => {
                assert_eq!(e.context.agent_id.tool, "opencode");
                assert_eq!(e.context.external_session_id, "oc-sess-123");
                assert_eq!(e.context.cwd, PathBuf::from("/home/user/project"));
                assert!(!e.file_paths.is_empty());
                assert_eq!(
                    e.context.metadata.get("session_id").map(String::as_str),
                    Some("oc-sess-123")
                );
            }
            _ => panic!("Expected PreFileEdit"),
        }
    }

    #[test]
    fn test_opencode_post_file_edit() {
        let input = make_opencode_input("PostToolUse", "write");
        let events = OpenCodePreset.parse(&input, "t_test").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                assert_eq!(e.context.agent_id.tool, "opencode");
                // Transcript source depends on whether the storage path exists
            }
            _ => panic!("Expected PostFileEdit"),
        }
    }

    #[test]
    fn test_opencode_pre_bash_call() {
        let input = make_opencode_input("PreToolUse", "bash");
        let events = OpenCodePreset.parse(&input, "t_test").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PreBashCall(e) => {
                assert_eq!(e.context.agent_id.tool, "opencode");
                assert_eq!(e.tool_use_id, "tu-1");
            }
            _ => panic!("Expected PreBashCall"),
        }
    }

    #[test]
    fn test_opencode_post_bash_call() {
        let input = make_opencode_input("PostToolUse", "shell");
        let events = OpenCodePreset.parse(&input, "t_test").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PostBashCall(e) => {
                assert_eq!(e.context.agent_id.tool, "opencode");
                assert_eq!(e.tool_use_id, "tu-1");
            }
            _ => panic!("Expected PostBashCall"),
        }
    }

    #[test]
    fn test_opencode_extracts_file_paths_from_tool_input() {
        let input = json!({
            "hook_event_name": "PostToolUse",
            "session_id": "sess-1",
            "cwd": "/project",
            "tool_name": "edit",
            "tool_input": {
                "file_path": "src/main.rs",
                "fspath": "/project/src/lib.rs"
            }
        })
        .to_string();
        let events = OpenCodePreset.parse(&input, "t_test").unwrap();
        match &events[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                assert!(!e.file_paths.is_empty());
            }
            _ => panic!("Expected PostFileEdit"),
        }
    }

    #[test]
    fn test_opencode_normalize_hook_path_absolute() {
        assert_eq!(
            OpenCodePreset::normalize_hook_path("/home/user/file.rs", "/project"),
            Some("/home/user/file.rs".to_string())
        );
    }

    #[test]
    fn test_opencode_normalize_hook_path_relative() {
        assert_eq!(
            OpenCodePreset::normalize_hook_path("src/main.rs", "/project"),
            Some("/project/src/main.rs".to_string())
        );
    }

    #[test]
    fn test_opencode_normalize_hook_path_file_uri() {
        assert_eq!(
            OpenCodePreset::normalize_hook_path("file:///home/user/file.rs", "/project"),
            Some("/home/user/file.rs".to_string())
        );
    }

    #[test]
    fn test_opencode_normalize_hook_path_empty() {
        assert_eq!(OpenCodePreset::normalize_hook_path("", "/project"), None);
    }

    #[test]
    fn test_opencode_collect_apply_patch_paths() {
        let mut out = Vec::new();
        super::super::parse::collect_apply_patch_paths_from_text(
            "*** Update File: src/main.rs\n*** Add File: src/new.rs",
            &mut out,
        );
        assert_eq!(out, vec!["src/main.rs", "src/new.rs"]);
    }

    #[test]
    fn test_opencode_extracts_file_paths_from_patch_key() {
        let input = json!({
            "hook_event_name": "PostToolUse",
            "session_id": "sess-1",
            "cwd": "/project",
            "tool_name": "edit",
            "tool_input": {
                "patch": "*** Update File: /project/src/main.rs\n@@ old\n+new\n"
            }
        })
        .to_string();
        let events = OpenCodePreset.parse(&input, "t_test").unwrap();
        match &events[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                assert_eq!(e.file_paths, vec![PathBuf::from("/project/src/main.rs")]);
            }
            _ => panic!("Expected PostFileEdit"),
        }
    }

    #[test]
    fn test_opencode_default_tool_use_id() {
        let input = json!({
            "hook_event_name": "PreToolUse",
            "session_id": "sess-1",
            "cwd": "/project",
            "tool_name": "bash"
        })
        .to_string();
        let events = OpenCodePreset.parse(&input, "t_test").unwrap();
        match &events[0] {
            ParsedHookEvent::PreBashCall(e) => {
                assert_eq!(e.tool_use_id, "bash");
            }
            _ => panic!("Expected PreBashCall"),
        }
    }

    #[test]
    fn test_kilo_preset_records_kilo_tool() {
        let input = make_opencode_input("PostToolUse", "edit");
        let events = KiloPreset.parse(&input, "t_test").unwrap();
        assert_eq!(events.len(), 1);
        match &events[0] {
            ParsedHookEvent::PostFileEdit(e) => {
                assert_eq!(e.context.agent_id.tool, "kilo");
                assert_eq!(e.context.external_session_id, "oc-sess-123");
                assert!(!e.file_paths.is_empty());
            }
            _ => panic!("Expected PostFileEdit"),
        }
    }

    #[test]
    fn test_kilo_preset_bash_classification() {
        let input = make_opencode_input("PreToolUse", "bash");
        let events = KiloPreset.parse(&input, "t_test").unwrap();
        assert!(
            matches!(&events[0], ParsedHookEvent::PreBashCall(e) if e.context.agent_id.tool == "kilo")
        );
    }

    #[test]
    fn test_kilo_resolves_kilo_db_not_opencode_db() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("opencode.db"), b"").unwrap();
        assert_eq!(
            OpenCodePreset::resolve_sqlite_db_path(&KILO_FAMILY, dir.path(), "ses"),
            None
        );
        std::fs::write(dir.path().join("kilo.db"), b"").unwrap();
        assert_eq!(
            OpenCodePreset::resolve_sqlite_db_path(&KILO_FAMILY, dir.path(), "ses"),
            Some(dir.path().join("kilo.db"))
        );
    }

    #[test]
    fn test_resolve_preset_kilo() {
        assert!(resolve_preset("kilo").is_ok());
    }
}
