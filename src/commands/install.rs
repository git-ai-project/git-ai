use std::env;
use std::fs;
use std::path::PathBuf;
use std::process;

use crate::commands::helpers::git_cmd;

pub fn handle_install() {
    // --- Step 1: Kill v1 daemon if running ---
    kill_v1_daemon_if_running();

    // --- Step 2: Install local post-commit hook (for fallback / non-daemon use) ---
    let git_dir_str = match git_cmd(&["rev-parse", "--git-dir"]) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("git-ai install: not in a git repository: {}", e);
            process::exit(1);
        }
    };

    let hooks_dir = PathBuf::from(&git_dir_str).join("hooks");
    fs::create_dir_all(&hooks_dir).unwrap_or_else(|e| {
        eprintln!("git-ai install: failed to create hooks dir: {}", e);
        process::exit(1);
    });

    let hook_path = hooks_dir.join("post-commit");
    let hook_content = "#!/bin/sh\ngit-ai post-commit\n";
    fs::write(&hook_path, hook_content).unwrap_or_else(|e| {
        eprintln!("git-ai install: failed to write hook: {}", e);
        process::exit(1);
    });

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&hook_path, fs::Permissions::from_mode(0o755)).unwrap_or_else(|e| {
            eprintln!("git-ai install: failed to chmod hook: {}", e);
            process::exit(1);
        });
    }

    println!("git-ai: installed post-commit hook");

    // --- Step 3: Configure global trace2 to point to the v2 daemon socket ---
    configure_trace2_global();
}

/// Stop the v1 daemon if it is running.
/// Reads the PID file from ~/.git-ai/internal/daemon/daemon.pid.json,
/// sends SIGTERM, and waits up to 5s for exit.
fn kill_v1_daemon_if_running() {
    let home = match env::var("HOME") {
        Ok(h) => h,
        Err(_) => return,
    };

    let pid_path = PathBuf::from(&home)
        .join(".git-ai")
        .join("internal")
        .join("daemon")
        .join("daemon.pid.json");

    if !pid_path.exists() {
        return;
    }

    let content = match fs::read_to_string(&pid_path) {
        Ok(c) => c,
        Err(_) => return,
    };

    // Minimal JSON parsing for {"pid": N, ...}
    let pid: u32 = match extract_pid_from_json(&content) {
        Some(p) => p,
        None => return,
    };

    // Check if the process is alive
    #[cfg(unix)]
    {
        let alive = unsafe { libc::kill(pid as i32, 0) } == 0;
        if !alive {
            let _ = fs::remove_file(&pid_path);
            return;
        }

        eprintln!("[git-ai] stopping v1 daemon (pid {})...", pid);
        unsafe {
            libc::kill(pid as i32, libc::SIGTERM);
        }

        // Wait up to 5s for exit
        for _ in 0..50 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            let still_alive = unsafe { libc::kill(pid as i32, 0) } == 0;
            if !still_alive {
                eprintln!("[git-ai] v1 daemon stopped");
                let _ = fs::remove_file(&pid_path);
                return;
            }
        }

        eprintln!(
            "[git-ai] warning: v1 daemon (pid {}) did not exit within 5s",
            pid
        );
    }

    #[cfg(not(unix))]
    {
        let _ = pid;
    }
}

/// Extract "pid" value from a minimal JSON object like {"pid":1234,...}
fn extract_pid_from_json(json: &str) -> Option<u32> {
    let pattern = "\"pid\":";
    let idx = json.find(pattern)?;
    let after = json[idx + pattern.len()..].trim_start();
    let end = after
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(after.len());
    if end == 0 {
        return None;
    }
    after[..end].parse().ok()
}

/// Configure git's global trace2 event target to point to the v2 daemon socket.
/// This is what makes git send events to the daemon without any proxy/wrapper.
fn configure_trace2_global() {
    let socket_path = resolve_trace2_socket_path();
    let target = format!("af_unix:stream:{}", socket_path.display());

    // Set trace2.eventTarget
    match git_cmd(&["config", "--global", "trace2.eventTarget", &target]) {
        Ok(_) => {}
        Err(e) => {
            eprintln!("git-ai install: failed to set trace2.eventTarget: {}", e);
            return;
        }
    }

    // Set trace2.eventNesting (need enough depth to see command details)
    match git_cmd(&["config", "--global", "trace2.eventNesting", "10"]) {
        Ok(_) => {}
        Err(e) => {
            eprintln!("git-ai install: failed to set trace2.eventNesting: {}", e);
            return;
        }
    }

    println!(
        "git-ai: configured trace2 event target -> {}",
        socket_path.display()
    );
}

/// Resolve the trace2 socket path.
/// Uses the same logic as DaemonPaths: ~/.git-ai/internal/daemon/trace2.sock
/// unless the path is too long (>= 100 chars), in which case it hashes to /tmp.
fn resolve_trace2_socket_path() -> PathBuf {
    let home = env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    let base_dir = PathBuf::from(&home)
        .join(".git-ai")
        .join("internal")
        .join("daemon");
    let candidate = base_dir.join("trace2.sock");

    if candidate.to_string_lossy().len() >= 100 {
        // Hash the base dir to create a short /tmp path (matching DaemonPaths logic)
        use sha2::Digest;
        let mut hasher = sha2::Sha256::new();
        hasher.update(base_dir.to_string_lossy().as_bytes());
        let hash = hasher.finalize();
        let short_hash: String = hash[..8].iter().map(|b| format!("{:02x}", b)).collect();
        PathBuf::from(format!("/tmp/git-ai-d-{}", short_hash)).join("trace2.sock")
    } else {
        candidate
    }
}
