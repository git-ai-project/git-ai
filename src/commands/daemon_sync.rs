use crate::daemon::{ControlRequest, DaemonConfig, send_control_request};
use crate::git::find_repository;
use crate::git::repository::Repository;

fn daemon_sync_available_for_git_ai_command() -> bool {
    #[cfg(any(test, feature = "test-support"))]
    {
        let Some(_path) = std::env::var("GIT_AI_DAEMON_CONTROL_SOCKET")
            .ok()
            .filter(|path| !path.trim().is_empty())
        else {
            return false;
        };

        #[cfg(windows)]
        {
            true
        }
        #[cfg(not(windows))]
        {
            std::path::Path::new(&_path).exists()
        }
    }
    #[cfg(not(any(test, feature = "test-support")))]
    {
        true
    }
}

pub(crate) fn sync_daemon_family_for_repo_or_exit(repo: &Repository, command_name: &str) {
    if !daemon_sync_available_for_git_ai_command() {
        return;
    }

    let workdir = repo.workdir().unwrap_or_else(|e| {
        eprintln!("Failed to resolve repository worktree for {command_name}: {e}");
        std::process::exit(1);
    });
    let config = DaemonConfig::from_env_or_default_paths().unwrap_or_else(|e| {
        eprintln!("Failed to resolve daemon paths for {command_name}: {e}");
        std::process::exit(1);
    });
    let request = ControlRequest::SyncFamily {
        repo_working_dir: workdir.to_string_lossy().to_string(),
    };
    match send_control_request(&config.control_socket_path, &request) {
        Ok(response) if response.ok => {}
        Ok(response) => {
            eprintln!(
                "Failed to sync git-ai background service before {command_name}: {}",
                response
                    .error
                    .unwrap_or_else(|| "unknown daemon error".to_string())
            );
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("Failed to sync git-ai background service before {command_name}: {e}");
            std::process::exit(1);
        }
    }
}

pub(crate) fn sync_daemon_family_for_current_repo_or_exit(command_name: &str) {
    let repo = find_repository(&Vec::<String>::new()).unwrap_or_else(|e| {
        eprintln!("Failed to find repository for {command_name}: {e}");
        std::process::exit(1);
    });
    sync_daemon_family_for_repo_or_exit(&repo, command_name);
}

pub(crate) fn sync_daemon_family_for_current_repo_if_present(command_name: &str) {
    if let Ok(repo) = find_repository(&Vec::<String>::new()) {
        sync_daemon_family_for_repo_or_exit(&repo, command_name);
    }
}
