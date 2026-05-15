use std::process;

pub fn handle_bg(args: &[String]) {
    match args.first().map(String::as_str) {
        Some("run") => {
            if let Err(e) = git_ai::daemon::run::run_daemon(true) {
                eprintln!("git-ai bg run: {}", e);
                process::exit(1);
            }
        }
        Some("start") => {
            let foreground = args.iter().any(|a| a == "--foreground");
            if foreground {
                if let Err(e) = git_ai::daemon::run::run_daemon(true) {
                    eprintln!("git-ai bg start: {}", e);
                    process::exit(1);
                }
            } else if let Err(e) = git_ai::daemon::run::run_daemon(false) {
                eprintln!("git-ai bg start: {}", e);
                process::exit(1);
            }
        }
        Some("stop") => {
            if let Err(e) = git_ai::daemon::run::stop_daemon() {
                eprintln!("git-ai bg stop: {}", e);
                process::exit(1);
            }
        }
        Some("restart") => {
            if let Err(e) = git_ai::daemon::run::restart_daemon() {
                eprintln!("git-ai bg restart: {}", e);
                process::exit(1);
            }
        }
        Some("status") => {
            git_ai::daemon::run::print_status();
        }
        Some("health") => {
            let status = git_ai::daemon::health::check_health();
            println!("{}", status);
            match status {
                git_ai::daemon::health::HealthStatus::Healthy => {}
                git_ai::daemon::health::HealthStatus::Degraded(_) => process::exit(1),
                git_ai::daemon::health::HealthStatus::Dead(_) => process::exit(2),
            }
        }
        Some("enable") => {
            handle_enable();
        }
        Some("disable") => {
            handle_disable();
        }
        _ => {
            eprintln!("usage: git-ai bg <run|start|stop|restart|status|health|enable|disable>");
            eprintln!();
            eprintln!("commands:");
            eprintln!("  run               run daemon in foreground (internal)");
            eprintln!("  start             start daemon in background");
            eprintln!(
                "  start --foreground  start daemon without daemonizing (for service managers)"
            );
            eprintln!("  stop              stop the running daemon");
            eprintln!(
                "  restart           stop and restart the daemon (preserves cumulative stats)"
            );
            eprintln!("  status            show daemon status");
            eprintln!(
                "  health            check daemon health (exit 0=healthy, 1=degraded, 2=dead)"
            );
            eprintln!("  enable            enable auto-start via system service manager");
            eprintln!("  disable           disable auto-start via system service manager");
            process::exit(1);
        }
    }
}

fn handle_enable() {
    use git_ai::daemon::service::{ServiceManager, detect_service_manager, enable_service};

    let manager = detect_service_manager();
    match manager {
        ServiceManager::Launchd => {
            eprintln!("[git-ai] enabling auto-start via launchd...");
        }
        ServiceManager::Systemd => {
            eprintln!("[git-ai] enabling auto-start via systemd...");
        }
        ServiceManager::None => {
            eprintln!("[git-ai] error: no supported service manager detected");
            eprintln!("[git-ai] on macOS, launchd is used; on Linux, systemd is required");
            process::exit(1);
        }
    }

    match enable_service() {
        Ok(()) => match manager {
            ServiceManager::Launchd => {
                eprintln!("[git-ai] launchd service enabled");
                eprintln!("[git-ai] the daemon will auto-start on login");
                eprintln!("[git-ai] to disable: git-ai bg disable");
            }
            ServiceManager::Systemd => {
                eprintln!("[git-ai] systemd user service enabled");
                eprintln!("[git-ai] the daemon will auto-start on login");
                eprintln!("[git-ai] to start now: systemctl --user start git-ai");
                eprintln!("[git-ai] to disable: git-ai bg disable");
            }
            ServiceManager::None => unreachable!(),
        },
        Err(e) => {
            eprintln!("[git-ai] error: {}", e);
            process::exit(1);
        }
    }
}

fn handle_disable() {
    use git_ai::daemon::service::{
        ServiceManager, detect_service_manager, disable_service, is_service_enabled,
    };

    let manager = detect_service_manager();

    if !is_service_enabled() {
        eprintln!("[git-ai] auto-start is not currently enabled");
        return;
    }

    match manager {
        ServiceManager::Launchd => {
            eprintln!("[git-ai] disabling launchd service...");
        }
        ServiceManager::Systemd => {
            eprintln!("[git-ai] disabling systemd user service...");
        }
        ServiceManager::None => {
            eprintln!("[git-ai] error: no supported service manager detected");
            process::exit(1);
        }
    }

    match disable_service() {
        Ok(()) => {
            eprintln!("[git-ai] auto-start disabled");
            eprintln!("[git-ai] the daemon will no longer start automatically on login");
        }
        Err(e) => {
            eprintln!("[git-ai] error: {}", e);
            process::exit(1);
        }
    }
}
