use crate::auth::{AuthState, collect_auth_status, format_unix_timestamp};
use crate::config;
use crate::git::find_repository_in_path;
use std::env;
use std::fmt::Write as _;
use std::process::Command;

#[cfg(target_os = "linux")]
use std::fs;

pub fn handle_debug(args: &[String]) {
    if args
        .iter()
        .any(|arg| arg == "--help" || arg == "-h" || arg == "help")
    {
        print_debug_help();
        std::process::exit(0);
    }

    if !args.is_empty() {
        eprintln!("Error: unknown debug argument(s): {}", args.join(" "));
        print_debug_help();
        std::process::exit(1);
    }

    let report = build_debug_report();
    println!("{}", report);
}

fn print_debug_help() {
    eprintln!("git-ai debug - Print diagnostic information for troubleshooting");
    eprintln!();
    eprintln!("Usage:");
    eprintln!("  git-ai debug");
    eprintln!("  git-ai debug --help");
}

fn build_debug_report() -> String {
    let config = config::Config::get();
    let git_cmd = config.git_cmd().to_string();
    let git_version = run_command_capture(&git_cmd, &["--version"]);
    let git_config = collect_git_config_dump(&git_cmd);
    let git_ai_config = collect_git_ai_config_dump();
    let platform_info = collect_platform_info();
    let hardware_info = collect_hardware_info();
    let repository_info = collect_repository_info();
    let auth_info = collect_auth_status();
    let env_overrides = collect_git_ai_env_overrides();

    let mut out = String::new();
    let _ = writeln!(out, "git-ai debug report");
    let _ = writeln!(out, "Generated (UTC): {}", chrono::Utc::now().to_rfc3339());
    let _ = writeln!(out);

    let _ = writeln!(out, "== Versions ==");
    let _ = writeln!(
        out,
        "Git AI version: {}",
        if cfg!(debug_assertions) {
            format!("{} (debug)", env!("CARGO_PKG_VERSION"))
        } else {
            env!("CARGO_PKG_VERSION").to_string()
        }
    );
    let _ = writeln!(
        out,
        "Git AI binary: {}",
        env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|e| format!("<unavailable: {}>", e))
    );
    let _ = writeln!(out, "Git binary path: {}", git_cmd);
    match git_version {
        Ok(version) => {
            let _ = writeln!(out, "Git version: {}", version);
        }
        Err(err) => {
            let _ = writeln!(out, "Git version: <error: {}>", err);
        }
    }
    let _ = writeln!(out);

    let _ = writeln!(out, "== Platform ==");
    let _ = writeln!(out, "OS family: {}", env::consts::FAMILY);
    let _ = writeln!(out, "OS: {}", env::consts::OS);
    let _ = writeln!(out, "Arch: {}", env::consts::ARCH);
    if let Some(kernel) = platform_info.kernel {
        let _ = writeln!(out, "Kernel: {}", kernel);
    } else {
        let _ = writeln!(out, "Kernel: <unavailable>");
    }
    if let Some(hostname) = platform_info.hostname {
        let _ = writeln!(out, "Hostname: {}", hostname);
    } else {
        let _ = writeln!(out, "Hostname: <unavailable>");
    }
    let _ = writeln!(
        out,
        "Shell: {}",
        env::var("SHELL")
            .or_else(|_| env::var("ComSpec"))
            .unwrap_or_else(|_| "<unavailable>".to_string())
    );
    let _ = writeln!(
        out,
        "Current dir: {}",
        env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|e| format!("<unavailable: {}>", e))
    );
    let _ = writeln!(out);

    let _ = writeln!(out, "== Hardware ==");
    match hardware_info.cpu_model {
        Some(cpu) => {
            let _ = writeln!(out, "CPU: {}", cpu);
        }
        None => {
            let _ = writeln!(out, "CPU: <unavailable>");
        }
    }
    match hardware_info.physical_cores {
        Some(cores) => {
            let _ = writeln!(out, "Physical cores: {}", cores);
        }
        None => {
            let _ = writeln!(out, "Physical cores: <unavailable>");
        }
    }
    match hardware_info.logical_cores {
        Some(cores) => {
            let _ = writeln!(out, "Logical cores: {}", cores);
        }
        None => {
            let _ = writeln!(out, "Logical cores: <unavailable>");
        }
    }
    match hardware_info.total_memory_bytes {
        Some(bytes) => {
            let _ = writeln!(out, "Memory: {}", format_bytes(bytes));
        }
        None => {
            let _ = writeln!(out, "Memory: <unavailable>");
        }
    }
    let _ = writeln!(out);

    let _ = writeln!(out, "== Repository ==");
    let _ = writeln!(out, "In repository: {}", repository_info.in_repository);
    if let Some(err) = repository_info.error {
        let _ = writeln!(out, "Repository detection: {}", err);
    } else {
        if let Some(workdir) = repository_info.workdir {
            let _ = writeln!(out, "Workdir: {}", workdir);
        }
        if let Some(git_dir) = repository_info.git_dir {
            let _ = writeln!(out, "Git dir: {}", git_dir);
        }
        if let Some(common_dir) = repository_info.common_dir {
            let _ = writeln!(out, "Git common dir: {}", common_dir);
        }
        if let Some(branch) = repository_info.branch {
            let _ = writeln!(out, "Branch: {}", branch);
        }
        if let Some(head) = repository_info.head {
            let _ = writeln!(out, "HEAD: {}", head);
        }
        if let Some(hooks_path) = repository_info.hooks_path {
            let _ = writeln!(out, "core.hooksPath: {}", hooks_path);
        }
        if !repository_info.remotes.is_empty() {
            let _ = writeln!(out, "Remotes:");
            for (name, url) in repository_info.remotes {
                let _ = writeln!(out, "  {} = {}", name, url);
            }
        }
    }
    let _ = writeln!(out);

    let _ = writeln!(out, "== Git Config ==");
    let _ = writeln!(out, "Command: {}", git_config.command);
    match git_config.output {
        Ok(config_output) => {
            append_indented_block(&mut out, &config_output);
        }
        Err(err) => {
            let _ = writeln!(out, "  <error: {}>", err);
        }
    }
    let _ = writeln!(out);

    let _ = writeln!(out, "== Git AI Config ==");
    match git_ai_config {
        Ok(config_output) => {
            append_indented_block(&mut out, &config_output);
        }
        Err(err) => {
            let _ = writeln!(out, "  <error: {}>", err);
        }
    }
    let _ = writeln!(out);

    let _ = writeln!(out, "== Git AI Login ==");
    let _ = writeln!(out, "Credential backend: {}", auth_info.backend);
    match &auth_info.state {
        AuthState::LoggedOut => {
            let _ = writeln!(out, "Status: logged out");
        }
        AuthState::LoggedIn => {
            let _ = writeln!(out, "Status: logged in");
        }
        AuthState::RefreshExpired => {
            let _ = writeln!(out, "Status: credentials expired (refresh token expired)");
        }
        AuthState::Error(err) => {
            let _ = writeln!(out, "Status: <error: {}>", err);
        }
    }
    if let Some(expires_at) = auth_info.access_token_expires_at {
        let _ = writeln!(
            out,
            "Access token expires at: {}",
            format_unix_timestamp(expires_at)
        );
    }
    if let Some(expires_at) = auth_info.refresh_token_expires_at {
        let _ = writeln!(
            out,
            "Refresh token expires at: {}",
            format_unix_timestamp(expires_at)
        );
    }
    if let Some(user_id) = auth_info.user_id {
        let _ = writeln!(out, "User ID: {}", user_id);
    } else if matches!(
        &auth_info.state,
        AuthState::LoggedIn | AuthState::RefreshExpired
    ) {
        let _ = writeln!(out, "User ID: <unavailable>");
    }
    if let Some(email) = auth_info.email {
        let _ = writeln!(out, "Email: {}", email);
    } else if matches!(
        &auth_info.state,
        AuthState::LoggedIn | AuthState::RefreshExpired
    ) {
        let _ = writeln!(out, "Email: <unavailable>");
    }
    if let Some(name) = auth_info.name {
        let _ = writeln!(out, "Name: {}", name);
    } else if matches!(
        &auth_info.state,
        AuthState::LoggedIn | AuthState::RefreshExpired
    ) {
        let _ = writeln!(out, "Name: <unavailable>");
    }
    if let Some(personal_org_id) = auth_info.personal_org_id {
        let _ = writeln!(out, "Personal org ID: {}", personal_org_id);
    }
    if !auth_info.orgs.is_empty() {
        let _ = writeln!(out, "Organizations:");
        for org in auth_info.orgs {
            let org_id = org.org_id.unwrap_or_else(|| "<unknown-id>".to_string());
            let org_slug = org.org_slug.unwrap_or_else(|| "<unknown-slug>".to_string());
            let org_name = org.org_name.unwrap_or_else(|| "<unknown-name>".to_string());
            let role = org.role.unwrap_or_else(|| "<unknown-role>".to_string());
            let _ = writeln!(
                out,
                "  - {} ({}) [{}] role={}",
                org_slug, org_name, org_id, role
            );
        }
    }
    let _ = writeln!(out);

    let _ = writeln!(out, "== Git AI Environment ==");
    if env_overrides.is_empty() {
        let _ = writeln!(out, "No GIT_AI_* environment variables are set.");
    } else {
        let _ = writeln!(out, "GIT_AI_* variables set:");
        for entry in env_overrides {
            let _ = writeln!(out, "  {}", entry);
        }
    }

    out
}

fn append_indented_block(out: &mut String, content: &str) {
    if content.trim().is_empty() {
        let _ = writeln!(out, "  <empty>");
        return;
    }
    for line in content.lines() {
        let _ = writeln!(out, "  {}", line);
    }
}

fn run_command_capture(program: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| format!("failed to execute '{}': {}", program, e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let code = output
            .status
            .code()
            .map(|c| c.to_string())
            .unwrap_or_else(|| "signal".to_string());
        if stderr.is_empty() {
            return Err(format!("exit code {}", code));
        }
        return Err(format!("exit code {}: {}", code, stderr));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[derive(Default)]
struct PlatformInfo {
    kernel: Option<String>,
    hostname: Option<String>,
}

fn collect_platform_info() -> PlatformInfo {
    PlatformInfo {
        kernel: collect_kernel_details(),
        hostname: collect_hostname(),
    }
}

fn collect_kernel_details() -> Option<String> {
    #[cfg(unix)]
    {
        run_command_capture("uname", &["-srm"]).ok()
    }
    #[cfg(windows)]
    {
        run_command_capture("cmd", &["/C", "ver"]).ok()
    }
}

fn collect_hostname() -> Option<String> {
    if let Ok(hostname) = env::var("HOSTNAME")
        && !hostname.trim().is_empty()
    {
        return Some(hostname);
    }

    if let Ok(hostname) = env::var("COMPUTERNAME")
        && !hostname.trim().is_empty()
    {
        return Some(hostname);
    }

    run_command_capture("hostname", &[]).ok()
}

#[derive(Default)]
struct HardwareInfo {
    cpu_model: Option<String>,
    physical_cores: Option<usize>,
    logical_cores: Option<usize>,
    total_memory_bytes: Option<u64>,
}

fn collect_hardware_info() -> HardwareInfo {
    let mut info = HardwareInfo {
        logical_cores: std::thread::available_parallelism()
            .ok()
            .map(std::num::NonZeroUsize::get),
        ..HardwareInfo::default()
    };

    #[cfg(target_os = "macos")]
    {
        info.cpu_model = run_command_capture("sysctl", &["-n", "machdep.cpu.brand_string"]).ok();
        info.physical_cores = run_command_capture("sysctl", &["-n", "hw.physicalcpu"])
            .ok()
            .and_then(|s| s.parse::<usize>().ok());
        info.logical_cores = run_command_capture("sysctl", &["-n", "hw.logicalcpu"])
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .or(info.logical_cores);
        info.total_memory_bytes = run_command_capture("sysctl", &["-n", "hw.memsize"])
            .ok()
            .and_then(|s| s.parse::<u64>().ok());
    }

    #[cfg(target_os = "linux")]
    {
        info.cpu_model = read_linux_cpu_model();
        info.total_memory_bytes = read_linux_total_memory();
    }

    #[cfg(windows)]
    {
        info.cpu_model = run_command_capture(
            "powershell",
            &[
                "-NoProfile",
                "-Command",
                "(Get-CimInstance Win32_Processor | Select-Object -First 1 -ExpandProperty Name)",
            ],
        )
        .ok()
        .filter(|s| !s.trim().is_empty());

        info.physical_cores = run_command_capture(
            "powershell",
            &[
                "-NoProfile",
                "-Command",
                "(Get-CimInstance Win32_Processor | Select-Object -First 1 -ExpandProperty NumberOfCores)",
            ],
        )
        .ok()
        .and_then(|s| s.trim().parse::<usize>().ok());

        info.total_memory_bytes = run_command_capture(
            "powershell",
            &[
                "-NoProfile",
                "-Command",
                "(Get-CimInstance Win32_ComputerSystem | Select-Object -ExpandProperty TotalPhysicalMemory)",
            ],
        )
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok());
    }

    info
}

#[cfg(target_os = "linux")]
fn read_linux_cpu_model() -> Option<String> {
    let cpuinfo = fs::read_to_string("/proc/cpuinfo").ok()?;
    for line in cpuinfo.lines() {
        if let Some((_, value)) = line.split_once(':')
            && line.starts_with("model name")
        {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

#[cfg(target_os = "linux")]
fn read_linux_total_memory() -> Option<u64> {
    let meminfo = fs::read_to_string("/proc/meminfo").ok()?;
    for line in meminfo.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            let kb = rest.split_whitespace().next()?.parse::<u64>().ok()?;
            return Some(kb.saturating_mul(1024));
        }
    }
    None
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{:.2} {} ({} bytes)", value, UNITS[unit], bytes)
}

struct RepositoryInfo {
    in_repository: bool,
    error: Option<String>,
    workdir: Option<String>,
    git_dir: Option<String>,
    common_dir: Option<String>,
    branch: Option<String>,
    head: Option<String>,
    hooks_path: Option<String>,
    remotes: Vec<(String, String)>,
}

fn collect_repository_info() -> RepositoryInfo {
    let cwd = env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| ".".to_string());

    let repo = match find_repository_in_path(&cwd) {
        Ok(repo) => repo,
        Err(e) => {
            return RepositoryInfo {
                in_repository: false,
                error: Some(e.to_string()),
                workdir: None,
                git_dir: None,
                common_dir: None,
                branch: None,
                head: None,
                hooks_path: None,
                remotes: Vec::new(),
            };
        }
    };

    let head = repo.head().ok();

    RepositoryInfo {
        in_repository: true,
        error: None,
        workdir: repo.workdir().ok().map(|p| p.display().to_string()),
        git_dir: Some(repo.path().display().to_string()),
        common_dir: Some(repo.common_dir().display().to_string()),
        branch: head.as_ref().and_then(|h| h.shorthand().ok()),
        head: head.as_ref().and_then(|h| h.target().ok()),
        hooks_path: repo.config_get_str("core.hooksPath").ok().flatten(),
        remotes: repo.remotes_with_urls().unwrap_or_default(),
    }
}

struct GitConfigDump {
    command: String,
    output: Result<String, String>,
}

fn collect_git_config_dump(git_cmd: &str) -> GitConfigDump {
    let attempts: &[&[&str]] = &[
        &["config", "--list", "--show-origin", "--show-scope"],
        &["config", "--list", "--show-origin"],
        &["config", "--list"],
    ];

    let mut last_error = String::new();
    for args in attempts {
        match run_command_capture(git_cmd, args) {
            Ok(output) => {
                let redacted = output
                    .lines()
                    .map(redact_git_config_line)
                    .collect::<Vec<_>>()
                    .join("\n");
                return GitConfigDump {
                    command: format!("{} {}", git_cmd, args.join(" ")),
                    output: Ok(redacted),
                };
            }
            Err(err) => {
                last_error = err;
            }
        }
    }

    GitConfigDump {
        command: format!("{} config --list --show-origin --show-scope", git_cmd),
        output: Err(last_error),
    }
}

fn redact_git_config_line(line: &str) -> String {
    if !line.contains('\t') {
        if let Some((key, value)) = line.split_once('=')
            && should_redact_key_value(key, value)
        {
            return format!("{}=[REDACTED]", key);
        }
        return line.to_string();
    }

    let mut parts = line.splitn(3, '\t');
    let first = match parts.next() {
        Some(v) => v,
        None => return line.to_string(),
    };
    let second = match parts.next() {
        Some(v) => v,
        None => return line.to_string(),
    };

    match parts.next() {
        // 3-field format: scope \t origin \t key=value
        // (from `git config --list --show-origin --show-scope`)
        Some(key_value) => {
            let (key, value) = match key_value.split_once('=') {
                Some((key, value)) => (key, value),
                None => return line.to_string(),
            };
            if should_redact_key_value(key, value) {
                format!("{}\t{}\t{}=[REDACTED]", first, second, key)
            } else {
                line.to_string()
            }
        }
        // 2-field format: origin \t key=value
        // (from `git config --list --show-origin` without --show-scope)
        None => {
            let (key, value) = match second.split_once('=') {
                Some((key, value)) => (key, value),
                None => return line.to_string(),
            };
            if should_redact_key_value(key, value) {
                format!("{}\t{}=[REDACTED]", first, key)
            } else {
                line.to_string()
            }
        }
    }
}

fn should_redact_key_value(key: &str, value: &str) -> bool {
    let key_lower = key.to_lowercase();
    let value_lower = value.to_lowercase();

    let sensitive_key_markers = [
        "password",
        "passwd",
        "token",
        "secret",
        "oauth",
        "authorization",
        "apikey",
        "api_key",
        "extraheader",
    ];

    if sensitive_key_markers
        .iter()
        .any(|marker| key_lower.contains(marker))
    {
        return true;
    }

    if key_lower.starts_with("url.") {
        return true;
    }

    sensitive_key_markers
        .iter()
        .any(|marker| value_lower.contains(marker))
}

fn collect_git_ai_config_dump() -> Result<String, String> {
    let runtime = config::Config::get();
    let mut out = String::new();
    let config_path = config::config_file_path_public()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "<unavailable>".to_string());
    let git_ai_dir = config::git_ai_dir_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "<unavailable>".to_string());

    let _ = writeln!(out, "config_file_path: {}", config_path);
    let _ = writeln!(out, "git_ai_dir: {}", git_ai_dir);
    let _ = writeln!(out, "runtime_config:");
    let serialized = runtime.to_printable_json_pretty()?;
    append_indented_block(&mut out, &serialized);
    Ok(out)
}

fn collect_git_ai_env_overrides() -> Vec<String> {
    let mut entries: Vec<(String, String)> = env::vars()
        .filter(|(k, _)| k.starts_with("GIT_AI_"))
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    entries
        .into_iter()
        .map(|(key, value)| format!("{}={}", key, redact_env_value(&key, &value)))
        .collect()
}

fn redact_env_value(key: &str, value: &str) -> String {
    let key_lower = key.to_lowercase();
    let sensitive_markers = ["token", "secret", "password", "key"];
    if sensitive_markers
        .iter()
        .any(|marker| key_lower.contains(marker))
    {
        return "[REDACTED]".to_string();
    }

    let trimmed = value.trim();
    if trimmed.is_empty() {
        return "<empty>".to_string();
    }

    if trimmed.len() > 200 {
        let truncated: String = trimmed.chars().take(200).collect();
        return format!("{}...[truncated]", truncated);
    }

    trimmed.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_redact_git_config_line_redacts_sensitive_key() {
        let line =
            "global\tfile:/Users/me/.gitconfig\thttp.https://example.com/.extraheader=AUTH token";
        let redacted = redact_git_config_line(line);
        assert_eq!(
            redacted,
            "global\tfile:/Users/me/.gitconfig\thttp.https://example.com/.extraheader=[REDACTED]"
        );
    }

    #[test]
    fn test_redact_git_config_line_keeps_non_sensitive_key() {
        let line = "global\tfile:/Users/me/.gitconfig\tcore.editor=vim";
        let redacted = redact_git_config_line(line);
        assert_eq!(redacted, line);
    }

    #[test]
    fn test_redact_git_config_line_two_field_format_redacts_sensitive() {
        // `git config --list --show-origin` (without --show-scope) produces 2-tab fields
        let line =
            "file:/Users/me/.gitconfig\thttp.https://example.com/.extraheader=BEARER secret123";
        let redacted = redact_git_config_line(line);
        assert_eq!(
            redacted,
            "file:/Users/me/.gitconfig\thttp.https://example.com/.extraheader=[REDACTED]"
        );
    }

    #[test]
    fn test_redact_git_config_line_two_field_format_keeps_non_sensitive() {
        let line = "file:/Users/me/.gitconfig\tcore.editor=vim";
        let redacted = redact_git_config_line(line);
        assert_eq!(redacted, line);
    }

    #[test]
    fn test_format_bytes() {
        assert_eq!(format_bytes(1024), "1.00 KB (1024 bytes)");
    }
}
