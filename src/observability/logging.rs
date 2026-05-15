use std::fmt;
use std::sync::OnceLock;

/// Log levels in order of severity (most severe first).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel {
    Error = 0,
    Warn = 1,
    Info = 2,
    Debug = 3,
    Trace = 4,
}

impl LogLevel {
    fn as_str(self) -> &'static str {
        match self {
            LogLevel::Error => "ERROR",
            LogLevel::Warn => "WARN",
            LogLevel::Info => "INFO",
            LogLevel::Debug => "DEBUG",
            LogLevel::Trace => "TRACE",
        }
    }

    fn as_str_lower(self) -> &'static str {
        match self {
            LogLevel::Error => "error",
            LogLevel::Warn => "warn",
            LogLevel::Info => "info",
            LogLevel::Debug => "debug",
            LogLevel::Trace => "trace",
        }
    }

    fn from_str(s: &str) -> Option<LogLevel> {
        match s.to_ascii_lowercase().as_str() {
            "error" => Some(LogLevel::Error),
            "warn" | "warning" => Some(LogLevel::Warn),
            "info" => Some(LogLevel::Info),
            "debug" => Some(LogLevel::Debug),
            "trace" => Some(LogLevel::Trace),
            _ => None,
        }
    }
}

impl fmt::Display for LogLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Output format for log messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    /// Human-readable: `[git-ai][LEVEL] message`
    Human,
    /// JSON: `{"ts":"...","level":"info","msg":"...","subsystem":"daemon"}`
    Json,
}

/// Parsed and cached log configuration.
struct LogConfig {
    /// Default level for all subsystems (when no per-subsystem override matches).
    default_level: LogLevel,
    /// Per-subsystem level overrides.
    subsystem_levels: Vec<(String, LogLevel)>,
    /// Output format.
    format: LogFormat,
}

/// Global cached log configuration.
static LOG_CONFIG: OnceLock<LogConfig> = OnceLock::new();

/// Parse the `GIT_AI_LOG` environment variable.
///
/// Supported formats:
/// - `debug` — set all subsystems to debug level
/// - `daemon=debug,mdm=info` — per-subsystem levels
/// - `info,daemon=debug` — global default + per-subsystem overrides
pub fn parse_log_spec(spec: &str) -> (LogLevel, Vec<(String, LogLevel)>) {
    let mut default_level = LogLevel::Warn; // Default if nothing specified
    let mut subsystem_levels = Vec::new();

    if spec.is_empty() {
        return (default_level, subsystem_levels);
    }

    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((subsystem, level_str)) = part.split_once('=') {
            let subsystem = subsystem.trim();
            let level_str = level_str.trim();
            if let Some(level) = LogLevel::from_str(level_str) {
                subsystem_levels.push((subsystem.to_string(), level));
            }
        } else {
            // No '=' means it's a global default level
            if let Some(level) = LogLevel::from_str(part) {
                default_level = level;
            }
        }
    }

    (default_level, subsystem_levels)
}

fn get_config() -> &'static LogConfig {
    LOG_CONFIG.get_or_init(|| {
        let format = match std::env::var("GIT_AI_LOG_FORMAT").as_deref() {
            Ok("json") => LogFormat::Json,
            _ => LogFormat::Human,
        };

        let (default_level, subsystem_levels) = match std::env::var("GIT_AI_LOG") {
            Ok(spec) => parse_log_spec(&spec),
            Err(_) => {
                // If GIT_AI_LOG is not set, default to Warn level
                // In debug builds, default to Debug for backward compat with debug_log
                if cfg!(debug_assertions) {
                    (LogLevel::Debug, Vec::new())
                } else {
                    (LogLevel::Warn, Vec::new())
                }
            }
        };

        LogConfig {
            default_level,
            subsystem_levels,
            format,
        }
    })
}

/// Check whether a log message at the given level and subsystem should be emitted.
#[inline]
pub fn is_enabled(level: LogLevel, subsystem: &str) -> bool {
    let config = get_config();

    // Check for subsystem-specific override first
    for (sub, sub_level) in &config.subsystem_levels {
        if sub == subsystem {
            return level <= *sub_level;
        }
    }

    // Fall back to the default level
    level <= config.default_level
}

/// Emit a log message. Call `is_enabled` first to avoid formatting cost.
pub fn emit(level: LogLevel, subsystem: &str, msg: &str) {
    let config = get_config();

    match config.format {
        LogFormat::Human => {
            eprintln!("[git-ai][{}] {}", level.as_str(), msg);
        }
        LogFormat::Json => {
            // Manual JSON construction to avoid serde overhead for simple cases.
            // Escape msg for JSON safety.
            let escaped_msg = escape_json_string(msg);
            let escaped_sub = escape_json_string(subsystem);
            eprintln!(
                r#"{{"ts":"{}","level":"{}","msg":"{}","subsystem":"{}"}}"#,
                timestamp_now(),
                level.as_str_lower(),
                escaped_msg,
                escaped_sub,
            );
        }
    }
}

/// Get a timestamp string (seconds since epoch with millisecond precision).
fn timestamp_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(d) => {
            let secs = d.as_secs();
            let millis = d.subsec_millis();
            format!("{}.{:03}", secs, millis)
        }
        Err(_) => "0.000".to_string(),
    }
}

/// Escape a string for safe JSON embedding (no serde needed).
fn escape_json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c < '\x20' => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

/// Log at ERROR level.
#[macro_export]
macro_rules! log_error {
    ($subsystem:expr, $($arg:tt)*) => {
        if $crate::observability::logging::is_enabled(
            $crate::observability::logging::LogLevel::Error,
            $subsystem,
        ) {
            $crate::observability::logging::emit(
                $crate::observability::logging::LogLevel::Error,
                $subsystem,
                &format!($($arg)*),
            );
        }
    };
}

/// Log at WARN level.
#[macro_export]
macro_rules! log_warn {
    ($subsystem:expr, $($arg:tt)*) => {
        if $crate::observability::logging::is_enabled(
            $crate::observability::logging::LogLevel::Warn,
            $subsystem,
        ) {
            $crate::observability::logging::emit(
                $crate::observability::logging::LogLevel::Warn,
                $subsystem,
                &format!($($arg)*),
            );
        }
    };
}

/// Log at INFO level.
#[macro_export]
macro_rules! log_info {
    ($subsystem:expr, $($arg:tt)*) => {
        if $crate::observability::logging::is_enabled(
            $crate::observability::logging::LogLevel::Info,
            $subsystem,
        ) {
            $crate::observability::logging::emit(
                $crate::observability::logging::LogLevel::Info,
                $subsystem,
                &format!($($arg)*),
            );
        }
    };
}

/// Log at DEBUG level.
#[macro_export]
macro_rules! log_debug {
    ($subsystem:expr, $($arg:tt)*) => {
        if $crate::observability::logging::is_enabled(
            $crate::observability::logging::LogLevel::Debug,
            $subsystem,
        ) {
            $crate::observability::logging::emit(
                $crate::observability::logging::LogLevel::Debug,
                $subsystem,
                &format!($($arg)*),
            );
        }
    };
}

/// Log at TRACE level.
#[macro_export]
macro_rules! log_trace {
    ($subsystem:expr, $($arg:tt)*) => {
        if $crate::observability::logging::is_enabled(
            $crate::observability::logging::LogLevel::Trace,
            $subsystem,
        ) {
            $crate::observability::logging::emit(
                $crate::observability::logging::LogLevel::Trace,
                $subsystem,
                &format!($($arg)*),
            );
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_log_level_ordering() {
        assert!(LogLevel::Error < LogLevel::Warn);
        assert!(LogLevel::Warn < LogLevel::Info);
        assert!(LogLevel::Info < LogLevel::Debug);
        assert!(LogLevel::Debug < LogLevel::Trace);
    }

    #[test]
    fn test_log_level_from_str() {
        assert_eq!(LogLevel::from_str("error"), Some(LogLevel::Error));
        assert_eq!(LogLevel::from_str("ERROR"), Some(LogLevel::Error));
        assert_eq!(LogLevel::from_str("warn"), Some(LogLevel::Warn));
        assert_eq!(LogLevel::from_str("warning"), Some(LogLevel::Warn));
        assert_eq!(LogLevel::from_str("info"), Some(LogLevel::Info));
        assert_eq!(LogLevel::from_str("INFO"), Some(LogLevel::Info));
        assert_eq!(LogLevel::from_str("debug"), Some(LogLevel::Debug));
        assert_eq!(LogLevel::from_str("trace"), Some(LogLevel::Trace));
        assert_eq!(LogLevel::from_str("garbage"), None);
        assert_eq!(LogLevel::from_str(""), None);
    }

    #[test]
    fn test_parse_log_spec_global_level() {
        let (default, subs) = parse_log_spec("debug");
        assert_eq!(default, LogLevel::Debug);
        assert!(subs.is_empty());
    }

    #[test]
    fn test_parse_log_spec_per_subsystem() {
        let (default, subs) = parse_log_spec("daemon=debug,mdm=info");
        // When no explicit global level is set, default remains Warn
        assert_eq!(default, LogLevel::Warn);
        assert_eq!(subs.len(), 2);
        assert_eq!(subs[0], ("daemon".to_string(), LogLevel::Debug));
        assert_eq!(subs[1], ("mdm".to_string(), LogLevel::Info));
    }

    #[test]
    fn test_parse_log_spec_mixed() {
        let (default, subs) = parse_log_spec("info,daemon=debug,mdm=trace");
        assert_eq!(default, LogLevel::Info);
        assert_eq!(subs.len(), 2);
        assert_eq!(subs[0], ("daemon".to_string(), LogLevel::Debug));
        assert_eq!(subs[1], ("mdm".to_string(), LogLevel::Trace));
    }

    #[test]
    fn test_parse_log_spec_empty() {
        let (default, subs) = parse_log_spec("");
        assert_eq!(default, LogLevel::Warn);
        assert!(subs.is_empty());
    }

    #[test]
    fn test_parse_log_spec_invalid_parts_ignored() {
        let (default, subs) = parse_log_spec("info,daemon=garbage,mdm=debug");
        assert_eq!(default, LogLevel::Info);
        // "daemon=garbage" should be ignored since "garbage" is invalid
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0], ("mdm".to_string(), LogLevel::Debug));
    }

    #[test]
    fn test_escape_json_string() {
        assert_eq!(escape_json_string("hello"), "hello");
        assert_eq!(escape_json_string("say \"hi\""), "say \\\"hi\\\"");
        assert_eq!(escape_json_string("line\nnew"), "line\\nnew");
        assert_eq!(escape_json_string("tab\there"), "tab\\there");
        assert_eq!(escape_json_string("back\\slash"), "back\\\\slash");
    }

    #[test]
    fn test_log_level_display() {
        assert_eq!(format!("{}", LogLevel::Error), "ERROR");
        assert_eq!(format!("{}", LogLevel::Warn), "WARN");
        assert_eq!(format!("{}", LogLevel::Info), "INFO");
        assert_eq!(format!("{}", LogLevel::Debug), "DEBUG");
        assert_eq!(format!("{}", LogLevel::Trace), "TRACE");
    }

    /// Test the subsystem filtering logic directly without relying on global state.
    #[test]
    fn test_subsystem_filtering_logic() {
        // Simulate: default=info, daemon=debug
        let default_level = LogLevel::Info;
        let subsystem_levels = vec![("daemon".to_string(), LogLevel::Debug)];

        let check = |level: LogLevel, subsystem: &str| -> bool {
            for (sub, sub_level) in &subsystem_levels {
                if sub == subsystem {
                    return level <= *sub_level;
                }
            }
            level <= default_level
        };

        // "daemon" subsystem should allow up to Debug
        assert!(check(LogLevel::Error, "daemon"));
        assert!(check(LogLevel::Warn, "daemon"));
        assert!(check(LogLevel::Info, "daemon"));
        assert!(check(LogLevel::Debug, "daemon"));
        assert!(!check(LogLevel::Trace, "daemon"));

        // "other" subsystem should use default (Info)
        assert!(check(LogLevel::Error, "other"));
        assert!(check(LogLevel::Warn, "other"));
        assert!(check(LogLevel::Info, "other"));
        assert!(!check(LogLevel::Debug, "other"));
        assert!(!check(LogLevel::Trace, "other"));
    }
}
