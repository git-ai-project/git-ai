use git_ai::auth::state::{AuthState, collect_auth_status, format_unix_timestamp};
use std::env;
use std::fmt::Write as _;
use std::process::Command;

pub fn handle_debug(args: &[String]) {
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        eprintln!("git-ai debug - Print diagnostic information for troubleshooting");
        eprintln!();
        eprintln!("Usage: git-ai debug");
        return;
    }

    println!("{}", build_debug_report());
}

fn build_debug_report() -> String {
    let mut out = String::new();
    let _ = writeln!(out, "git-ai debug report");
    let _ = writeln!(out, "Generated (UTC): {}", chrono::Utc::now().to_rfc3339());
    let _ = writeln!(out);

    // Versions
    let _ = writeln!(out, "== Versions ==");
    let _ = writeln!(
        out,
        "Git AI version: {}{}",
        env!("CARGO_PKG_VERSION"),
        if cfg!(debug_assertions) {
            " (debug)"
        } else {
            ""
        }
    );
    let _ = writeln!(
        out,
        "Git AI binary: {}",
        env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|e| format!("<unavailable: {}>", e))
    );
    let git_version = run_cmd("git", &["--version"]);
    let _ = writeln!(
        out,
        "Git version: {}",
        git_version.unwrap_or_else(|e| format!("<error: {}>", e))
    );
    let _ = writeln!(out);

    // Platform
    let _ = writeln!(out, "== Platform ==");
    let _ = writeln!(out, "OS: {} ({})", env::consts::OS, env::consts::FAMILY);
    let _ = writeln!(out, "Arch: {}", env::consts::ARCH);
    if let Ok(kernel) = run_cmd("uname", &["-r"]) {
        let _ = writeln!(out, "Kernel: {}", kernel);
    }
    let _ = writeln!(
        out,
        "Shell: {}",
        env::var("SHELL").unwrap_or_else(|_| "<unknown>".to_string())
    );
    let _ = writeln!(out);

    // Auth
    let _ = writeln!(out, "== Auth ==");
    let auth = collect_auth_status();
    let _ = writeln!(out, "Backend: {}", auth.backend);
    let state_str = match &auth.state {
        AuthState::LoggedOut => "logged out",
        AuthState::LoggedIn => "logged in",
        AuthState::RefreshExpired => "expired",
        AuthState::Error(e) => e.as_str(),
    };
    let _ = writeln!(out, "State: {}", state_str);
    if let Some(email) = &auth.email {
        let _ = writeln!(out, "Email: {}", email);
    }
    if let Some(ts) = auth.access_token_expires_at {
        let _ = writeln!(out, "Access token expires: {}", format_unix_timestamp(ts));
    }
    if let Some(ts) = auth.refresh_token_expires_at {
        let _ = writeln!(out, "Refresh token expires: {}", format_unix_timestamp(ts));
    }
    let _ = writeln!(out);

    // Repository
    let _ = writeln!(out, "== Repository ==");
    let cwd = env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "<unknown>".to_string());
    let _ = writeln!(out, "CWD: {}", cwd);
    if let Ok(toplevel) = run_cmd("git", &["rev-parse", "--show-toplevel"]) {
        let _ = writeln!(out, "Repo root: {}", toplevel);
    }
    if let Ok(git_dir) = run_cmd("git", &["rev-parse", "--git-dir"]) {
        let _ = writeln!(out, "Git dir: {}", git_dir);
    }
    if let Ok(remotes) = run_cmd("git", &["remote", "-v"]) {
        let _ = writeln!(out, "Remotes:\n{}", remotes);
    }
    let _ = writeln!(out);

    // Hooks
    let _ = writeln!(out, "== Hooks ==");
    if let Ok(hooks_path) = run_cmd("git", &["config", "--get", "core.hooksPath"]) {
        let _ = writeln!(out, "core.hooksPath: {}", hooks_path);
    } else {
        let _ = writeln!(out, "core.hooksPath: <not set>");
    }
    let _ = writeln!(out);

    // Notes
    let _ = writeln!(out, "== Notes ==");
    if let Ok(notes_list) = run_cmd("git", &["notes", "--ref=ai", "list"]) {
        let count = notes_list.lines().count();
        let _ = writeln!(out, "Authorship notes count: {}", count);
    } else {
        let _ = writeln!(out, "Authorship notes: <none or unavailable>");
    }
    let _ = writeln!(out);

    // Environment overrides
    let _ = writeln!(out, "== Environment Overrides ==");
    let mut found_any = false;
    for (key, value) in env::vars() {
        if key.starts_with("GIT_AI") {
            let _ = writeln!(out, "  {}={}", key, value);
            found_any = true;
        }
    }
    if !found_any {
        let _ = writeln!(out, "  (none)");
    }

    out
}

fn run_cmd(program: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| e.to_string())?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}
