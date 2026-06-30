//! Security sandbox detection for daemon startup.

use crate::api::types::{DaemonLogEvent, DaemonLogFieldValue, DaemonLogKind, DaemonLogLevel};
use crate::daemon::DaemonConfig;
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};

const PROBE_FILE_PREFIX: &str = ".git-ai-daemon-sandbox-probe-";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SecuritySandboxHomeRestriction {
    pub(crate) sandbox: &'static str,
    pub(crate) env_signal: String,
    pub(crate) path: PathBuf,
    pub(crate) operation: &'static str,
    pub(crate) error: String,
}

impl SecuritySandboxHomeRestriction {
    pub(crate) fn user_message(&self) -> String {
        format!(
            "daemon startup refused: detected {} security sandbox and git-ai daemon home is not accessible ({} {} failed for {}: {}). Whitelist ~/.git-ai or set GIT_AI_DAEMON_HOME to a writable path outside the sandbox.",
            self.sandbox,
            self.operation,
            if self.operation == "create_dir_all" {
                "at"
            } else {
                "in"
            },
            self.path.display(),
            self.error
        )
    }
}

/// Detects the issue described in #909: a security sandbox can allow the
/// git-ai binary to run while blocking access to `~/.git-ai`, which makes
/// trace2 silently fail. Keep this check conservative by requiring both an
/// explicit security-sandbox signal and an actual daemon-home access failure.
///
/// Only use documented child-process sandbox markers or git-ai's explicit
/// opt-in marker here. Generic agent identity, cloud-agent identity, and OS
/// backend names are not enough to distinguish this issue from unrelated
/// permission failures.
pub(crate) fn detect_security_sandbox_home_restriction(
    config: &DaemonConfig,
) -> Option<SecuritySandboxHomeRestriction> {
    let (sandbox, env_signal) = detect_security_sandbox_env()?;
    probe_daemon_paths(config)
        .err()
        .map(|failure| SecuritySandboxHomeRestriction {
            sandbox,
            env_signal,
            path: failure.path,
            operation: failure.operation,
            error: failure.error,
        })
}

pub(crate) fn log_security_sandbox_home_restriction(
    context: &'static str,
    config: &DaemonConfig,
    restriction: &SecuritySandboxHomeRestriction,
) {
    let message = "daemon startup refused in security sandbox with restricted git-ai home";
    eprintln!("[git-ai] {}", restriction.user_message());
    tracing::error!(
        context,
        sandbox = restriction.sandbox,
        env_signal = %restriction.env_signal,
        path = %restriction.path.display(),
        operation = restriction.operation,
        error = %restriction.error,
        internal_dir = %config.internal_dir.display(),
        lock_path = %config.lock_path.display(),
        control_socket_path = %config.control_socket_path.display(),
        trace_socket_path = %config.trace_socket_path.display(),
        "{message}"
    );

    let event = daemon_log_event(context, config, restriction, message);
    let _ = crate::daemon::telemetry_worker::flush_daemon_log_event_now(event);
}

fn detect_security_sandbox_env() -> Option<(&'static str, String)> {
    if env_var_has_non_empty_value("CURSOR_SANDBOX") {
        return Some(("Cursor", "CURSOR_SANDBOX".to_string()));
    }
    if env_var_has_non_empty_value("CODEX_SANDBOX") {
        return Some(("Codex", "CODEX_SANDBOX".to_string()));
    }

    for key in ["GIT_AI_SECURITY_SANDBOX", "GIT_AI_AGENT_SECURITY_SANDBOX"] {
        if env_var_is_truthy(key) {
            return Some(("git-ai configured", key.to_string()));
        }
    }

    None
}

fn env_var_has_non_empty_value(key: &str) -> bool {
    std::env::var_os(key).is_some_and(|value| !value.to_string_lossy().trim().is_empty())
}

fn env_var_is_truthy(key: &str) -> bool {
    std::env::var(key)
        .ok()
        .is_some_and(|value| matches!(value.to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
}

struct PathProbeFailure {
    path: PathBuf,
    operation: &'static str,
    error: String,
}

fn probe_daemon_paths(config: &DaemonConfig) -> Result<(), PathProbeFailure> {
    probe_writable_dir(&config.internal_dir)?;

    if let Some(lock_parent) = config.lock_path.parent() {
        probe_writable_dir(lock_parent)?;
    }
    #[cfg(not(windows))]
    {
        if let Some(control_parent) = config.control_socket_path.parent() {
            probe_writable_dir(control_parent)?;
        }
        if let Some(trace_parent) = config.trace_socket_path.parent() {
            probe_writable_dir(trace_parent)?;
        }
    }

    Ok(())
}

fn probe_writable_dir(path: &Path) -> Result<(), PathProbeFailure> {
    fs::create_dir_all(path).map_err(|error| PathProbeFailure {
        path: path.to_path_buf(),
        operation: "create_dir_all",
        error: error.to_string(),
    })?;

    let probe_path = path.join(format!("{}{}", PROBE_FILE_PREFIX, std::process::id()));
    match OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&probe_path)
    {
        Ok(_) => {
            let _ = fs::remove_file(&probe_path);
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(PathProbeFailure {
            path: path.to_path_buf(),
            operation: "create_probe_file",
            error: error.to_string(),
        }),
    }
}

fn daemon_log_event(
    context: &'static str,
    config: &DaemonConfig,
    restriction: &SecuritySandboxHomeRestriction,
    message: &str,
) -> DaemonLogEvent {
    let mut fields = BTreeMap::new();
    fields.insert("context".to_string(), DaemonLogFieldValue::from(context));
    fields.insert(
        "sandbox".to_string(),
        DaemonLogFieldValue::from(restriction.sandbox),
    );
    fields.insert(
        "env_signal".to_string(),
        DaemonLogFieldValue::from(restriction.env_signal.clone()),
    );
    fields.insert(
        "operation".to_string(),
        DaemonLogFieldValue::from(restriction.operation),
    );
    fields.insert(
        "path".to_string(),
        DaemonLogFieldValue::from(restriction.path.display().to_string()),
    );
    fields.insert(
        "error".to_string(),
        DaemonLogFieldValue::from(restriction.error.clone()),
    );
    fields.insert(
        "internal_dir".to_string(),
        DaemonLogFieldValue::from(config.internal_dir.display().to_string()),
    );
    fields.insert(
        "lock_path".to_string(),
        DaemonLogFieldValue::from(config.lock_path.display().to_string()),
    );
    fields.insert(
        "control_socket_path".to_string(),
        DaemonLogFieldValue::from(config.control_socket_path.display().to_string()),
    );
    fields.insert(
        "trace_socket_path".to_string(),
        DaemonLogFieldValue::from(config.trace_socket_path.display().to_string()),
    );

    DaemonLogEvent {
        id: Some(crate::uuid::generate_v4()),
        kind: DaemonLogKind::Log,
        timestamp: chrono::Utc::now().to_rfc3339(),
        level: DaemonLogLevel::Error,
        target: Some("git_ai::daemon::sandbox".to_string()),
        message: message.to_string(),
        fields,
        repo_url: None,
        git_ai_version: Some(
            crate::authorship::authorship_log_serialization::GIT_AI_VERSION.to_string(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    const SANDBOX_ENV_KEYS: &[&str] = &[
        "CODEX_THREAD_ID",
        "CODEX_CI",
        "CODEX_SANDBOX",
        "CODEX_SANDBOX_NETWORK_DISABLED",
        "CODEX_MANAGED_BY_NPM",
        "CODEX_INTERNAL_ORIGINATOR_OVERRIDE",
        "CLAUDE_CODE_REMOTE",
        "CLAUDE_CONFIG_DIR",
        "CLAUDECODE",
        "ANTHROPIC_PRODUCT",
        "CURSOR_AGENT",
        "CURSOR_SANDBOX",
        "CURSOR_CONFIG_DIR",
        "CURSOR_TRACE_ID",
        "HOSTNAME",
        "GIT_AI_CLOUD_AGENT",
        "AGENT_OS",
        "SEATBELT_PROFILE",
        "SEATBELT_EXEC_PATH",
        "APP_SANDBOX_CONTAINER_ID",
        "GIT_AI_SECURITY_SANDBOX",
        "GIT_AI_AGENT_SECURITY_SANDBOX",
    ];

    struct ScopedEnvVar {
        key: &'static str,
        previous: Option<std::ffi::OsString>,
    }

    impl ScopedEnvVar {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = std::env::var_os(key);
            // SAFETY: these tests are serialized, so there are no concurrent
            // environment mutations while this guard is alive.
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, previous }
        }
    }

    impl Drop for ScopedEnvVar {
        fn drop(&mut self) {
            // SAFETY: these tests are serialized, so there are no concurrent
            // environment mutations while this guard is alive.
            unsafe {
                match &self.previous {
                    Some(value) => std::env::set_var(self.key, value),
                    None => std::env::remove_var(self.key),
                }
            }
        }
    }

    struct ClearedSandboxEnv {
        previous: Vec<(&'static str, Option<std::ffi::OsString>)>,
        cloud_agent_previous: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    }

    impl ClearedSandboxEnv {
        fn new() -> Self {
            let previous = SANDBOX_ENV_KEYS
                .iter()
                .map(|key| (*key, std::env::var_os(key)))
                .collect::<Vec<_>>();
            let cloud_agent_previous = std::env::vars_os()
                .filter(|(key, _)| {
                    key.to_string_lossy()
                        .to_ascii_uppercase()
                        .starts_with("CLOUD_AGENT_")
                })
                .collect::<Vec<_>>();

            // SAFETY: these tests are serialized, so there are no concurrent
            // environment mutations while this guard is alive.
            unsafe {
                for key in SANDBOX_ENV_KEYS {
                    std::env::remove_var(key);
                }
                for (key, _) in &cloud_agent_previous {
                    std::env::remove_var(key);
                }
            }

            Self {
                previous,
                cloud_agent_previous,
            }
        }
    }

    impl Drop for ClearedSandboxEnv {
        fn drop(&mut self) {
            // SAFETY: these tests are serialized, so there are no concurrent
            // environment mutations while this guard is alive.
            unsafe {
                for (key, value) in &self.previous {
                    match value {
                        Some(value) => std::env::set_var(key, value),
                        None => std::env::remove_var(key),
                    }
                }
                for (key, value) in &self.cloud_agent_previous {
                    std::env::set_var(key, value);
                }
            }
        }
    }

    #[test]
    #[serial]
    fn detects_security_sandbox_only_when_daemon_home_is_inaccessible() {
        let _sandbox_env = ClearedSandboxEnv::new();
        let _env = ScopedEnvVar::set("CURSOR_SANDBOX", "native");
        let temp = tempfile::tempdir().unwrap();
        let blocked_home = temp.path().join("not-a-directory");
        fs::write(&blocked_home, "file").unwrap();

        let config = DaemonConfig::from_home(&blocked_home);
        let restriction = detect_security_sandbox_home_restriction(&config).unwrap();

        assert_eq!(restriction.sandbox, "Cursor");
        assert_eq!(restriction.env_signal, "CURSOR_SANDBOX");
        assert_eq!(restriction.operation, "create_dir_all");
        assert!(
            restriction
                .user_message()
                .contains("daemon startup refused")
        );
    }

    #[test]
    #[serial]
    fn allows_security_sandbox_when_daemon_home_is_writable() {
        let _sandbox_env = ClearedSandboxEnv::new();
        let _env = ScopedEnvVar::set("CURSOR_SANDBOX", "native");
        let temp = tempfile::tempdir().unwrap();
        let config = DaemonConfig::from_home(temp.path());

        assert!(detect_security_sandbox_home_restriction(&config).is_none());
        assert!(config.internal_dir.exists());
    }

    #[test]
    #[serial]
    fn ignores_restricted_home_without_security_sandbox_signal() {
        let _sandbox_env = ClearedSandboxEnv::new();
        let temp = tempfile::tempdir().unwrap();
        let blocked_home = temp.path().join("not-a-directory");
        fs::write(&blocked_home, "file").unwrap();

        let config = DaemonConfig::from_home(&blocked_home);

        assert!(detect_security_sandbox_home_restriction(&config).is_none());
    }

    #[test]
    #[serial]
    fn ignores_cloud_agent_and_undocumented_backend_signals() {
        let _sandbox_env = ClearedSandboxEnv::new();
        let _codex_thread = ScopedEnvVar::set("CODEX_THREAD_ID", "test-thread");
        let _codex_network = ScopedEnvVar::set("CODEX_SANDBOX_NETWORK_DISABLED", "1");
        let _cloud_agent = ScopedEnvVar::set("CLOUD_AGENT_ID", "test-cloud-agent");
        let _git_ai_cloud_agent = ScopedEnvVar::set("GIT_AI_CLOUD_AGENT", "1");
        let _agent_os = ScopedEnvVar::set("AGENT_OS", "linux");
        let _seatbelt_profile = ScopedEnvVar::set("SEATBELT_PROFILE", "agent");
        let _seatbelt_exec = ScopedEnvVar::set("SEATBELT_EXEC_PATH", "/usr/bin/sandbox-exec");
        let _app_sandbox = ScopedEnvVar::set("APP_SANDBOX_CONTAINER_ID", "container");
        let temp = tempfile::tempdir().unwrap();
        let blocked_home = temp.path().join("not-a-directory");
        fs::write(&blocked_home, "file").unwrap();

        let config = DaemonConfig::from_home(&blocked_home);

        assert!(detect_security_sandbox_home_restriction(&config).is_none());
    }

    #[test]
    #[serial]
    fn detects_codex_security_sandbox_signal() {
        let _sandbox_env = ClearedSandboxEnv::new();
        let _env = ScopedEnvVar::set("CODEX_SANDBOX", "seatbelt");
        let temp = tempfile::tempdir().unwrap();
        let blocked_home = temp.path().join("not-a-directory");
        fs::write(&blocked_home, "file").unwrap();

        let config = DaemonConfig::from_home(&blocked_home);
        let restriction = detect_security_sandbox_home_restriction(&config).unwrap();

        assert_eq!(restriction.sandbox, "Codex");
        assert_eq!(restriction.env_signal, "CODEX_SANDBOX");
    }

    #[test]
    #[serial]
    fn ignores_empty_or_false_security_sandbox_signals() {
        let _sandbox_env = ClearedSandboxEnv::new();
        let _cursor = ScopedEnvVar::set("CURSOR_SANDBOX", " ");
        let _codex = ScopedEnvVar::set("CODEX_SANDBOX", " ");
        let _git_ai = ScopedEnvVar::set("GIT_AI_SECURITY_SANDBOX", "0");
        let temp = tempfile::tempdir().unwrap();
        let blocked_home = temp.path().join("not-a-directory");
        fs::write(&blocked_home, "file").unwrap();

        let config = DaemonConfig::from_home(&blocked_home);

        assert!(detect_security_sandbox_home_restriction(&config).is_none());
    }

    #[test]
    #[serial]
    fn detects_explicit_git_ai_security_sandbox_signal() {
        let _sandbox_env = ClearedSandboxEnv::new();
        let _env = ScopedEnvVar::set("GIT_AI_SECURITY_SANDBOX", "1");
        let temp = tempfile::tempdir().unwrap();
        let blocked_home = temp.path().join("not-a-directory");
        fs::write(&blocked_home, "file").unwrap();

        let config = DaemonConfig::from_home(&blocked_home);
        let restriction = detect_security_sandbox_home_restriction(&config).unwrap();

        assert_eq!(restriction.sandbox, "git-ai configured");
        assert_eq!(restriction.env_signal, "GIT_AI_SECURITY_SANDBOX");
    }
}
