use crate::error::GitAiError;
use std::path::Path;

pub(super) fn configure(binary_path: &Path, dry_run: bool) -> Result<(), GitAiError> {
    if dry_run {
        return Ok(());
    }

    let install_dir = binary_path.parent().ok_or_else(|| {
        GitAiError::Generic("could not determine git-ai install directory".to_string())
    })?;

    #[cfg(windows)]
    configure_windows(install_dir);

    #[cfg(not(windows))]
    configure_unix(install_dir)?;

    Ok(())
}

#[cfg(not(windows))]
fn detect_unix_shells(
    home: &Path,
    login_shell: Option<&std::ffi::OsStr>,
) -> Vec<(&'static str, std::path::PathBuf)> {
    let mut shells = Vec::new();
    let bashrc = home.join(".bashrc");
    let bash_profile = home.join(".bash_profile");
    let zshrc = home.join(".zshrc");
    let fish = home.join(".config").join("fish").join("config.fish");

    if bashrc.is_file() {
        shells.push(("bash", bashrc));
    } else if bash_profile.is_file() {
        shells.push(("bash", bash_profile));
    }
    if zshrc.is_file() {
        shells.push(("zsh", zshrc));
    }
    if fish.is_file() {
        shells.push(("fish", fish));
    }

    if shells.is_empty() {
        let login_shell = login_shell
            .and_then(|shell| Path::new(shell).file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let (name, path) = match login_shell.as_str() {
            "fish" => ("fish", ".config/fish/config.fish"),
            "zsh" => ("zsh", ".zshrc"),
            _ => ("bash", ".bashrc"),
        };
        shells.push((name, home.join(path)));
    }

    shells
}

#[cfg(not(windows))]
fn configure_unix(install_dir: &Path) -> Result<(), GitAiError> {
    use chrono::Local;
    use std::fs::{self, OpenOptions};
    use std::io::Write;

    let home = crate::mdm::utils::home_dir();
    let install_dir = install_dir.to_string_lossy();
    let mut configured = Vec::new();
    let mut already_configured = Vec::new();
    let mut created_paths = Vec::new();
    let login_shell = std::env::var_os("SHELL");

    for (shell_name, config_file) in detect_unix_shells(&home, login_shell.as_deref()) {
        let path_command = if shell_name == "fish" {
            let config_dir = config_file.parent().ok_or_else(|| {
                GitAiError::Generic(format!(
                    "could not determine parent directory for {}",
                    config_file.display()
                ))
            })?;
            if !config_dir.is_dir() {
                fs::create_dir_all(config_dir)?;
                created_paths.push(config_dir.to_path_buf());
            }
            format!("fish_add_path -g \"{install_dir}\"")
        } else {
            format!("export PATH=\"{install_dir}:$PATH\"")
        };

        if !config_file.is_file() {
            created_paths.push(config_file.clone());
        }

        let existing = fs::read(&config_file).unwrap_or_default();
        let install_dir_bytes = install_dir.as_bytes();
        let contains_install_dir = !install_dir_bytes.is_empty()
            && existing
                .windows(install_dir_bytes.len())
                .any(|window| window == install_dir_bytes);

        if contains_install_dir {
            already_configured.push((shell_name, config_file));
            continue;
        }

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&config_file)?;
        writeln!(file)?;
        writeln!(
            file,
            "# Added by git-ai installer on {}",
            Local::now().format("%a %b %e %T %Z %Y")
        )?;
        writeln!(file, "{path_command}")?;
        configured.push((shell_name, config_file));
    }

    if !configured.is_empty() {
        println!("\nUpdated shell configurations:");
        for (_, config_file) in &configured {
            println!("\x1b[0;32m  ✓ {}\x1b[0m", config_file.display());
        }

        println!("\nTo apply changes immediately:");
        for (shell_name, config_file) in &configured {
            println!("  - For {shell_name}: source {}", config_file.display());
        }
    }

    if !already_configured.is_empty() {
        println!("\nAlready configured (no changes needed):");
        for (_, config_file) in &already_configured {
            println!("  ✓ {}", config_file.display());
        }
    }

    if configured.is_empty() && already_configured.is_empty() {
        println!("\nCould not detect any shell config files.");
        println!("Please add the following line to your shell config and restart:");
        println!("  export PATH=\"{install_dir}:$PATH\"");
    }

    repair_created_path_ownership(&created_paths);

    println!("\n\x1b[0;33mClose and reopen your terminal and IDE sessions to use git-ai.\x1b[0m");
    Ok(())
}

#[cfg(not(windows))]
fn repair_created_path_ownership(created_paths: &[std::path::PathBuf]) {
    use std::process::{Command, Stdio};

    if !crate::utils::is_running_as_superuser() {
        return;
    }
    let Some(install_user) =
        std::env::var_os("GIT_AI_INSTALL_USER").filter(|user| !user.is_empty())
    else {
        return;
    };

    for path in created_paths {
        let _ = Command::new("chown")
            .arg(&install_user)
            .arg(path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

#[cfg(all(test, not(windows)))]
mod unix_tests {
    use super::*;
    use std::ffi::OsStr;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn unix_shell_detection_prefers_bashrc_and_includes_every_existing_shell() {
        let temp = tempdir().unwrap();
        let home = temp.path();
        fs::write(home.join(".bashrc"), "").unwrap();
        fs::write(home.join(".bash_profile"), "").unwrap();
        fs::write(home.join(".zshrc"), "").unwrap();
        fs::create_dir_all(home.join(".config/fish")).unwrap();
        fs::write(home.join(".config/fish/config.fish"), "").unwrap();

        assert_eq!(
            detect_unix_shells(home, Some(OsStr::new("/bin/bash"))),
            vec![
                ("bash", home.join(".bashrc")),
                ("zsh", home.join(".zshrc")),
                ("fish", home.join(".config/fish/config.fish")),
            ]
        );
    }

    #[test]
    fn unix_shell_detection_falls_back_to_login_shell_or_bash() {
        for (login_shell, expected_name, expected_path) in [
            ("/usr/local/bin/fish", "fish", ".config/fish/config.fish"),
            ("/bin/zsh", "zsh", ".zshrc"),
            ("/bin/bash", "bash", ".bashrc"),
            ("/bin/unknown", "bash", ".bashrc"),
            ("", "bash", ".bashrc"),
        ] {
            let temp = tempdir().unwrap();
            assert_eq!(
                detect_unix_shells(temp.path(), Some(OsStr::new(login_shell))),
                vec![(expected_name, temp.path().join(expected_path))]
            );
        }
    }
}

#[cfg(windows)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UserPathStatus {
    Updated,
    AlreadyPresent,
    Error,
    Skipped,
}

#[cfg(windows)]
fn normalize_windows_path(path: &str) -> String {
    let trimmed = path.trim();
    let absolute = std::path::absolute(trimmed)
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| trimmed.to_string());
    absolute.trim_end_matches('\\').to_lowercase()
}

#[cfg(windows)]
fn path_contains_windows_entry(path: &str, path_to_add: &str) -> bool {
    let normalized_add = normalize_windows_path(path_to_add);
    path.split(';')
        .filter(|entry| !entry.trim().is_empty())
        .any(|entry| normalize_windows_path(entry) == normalized_add)
}

#[cfg(windows)]
fn append_windows_path(path: &str, path_to_add: &str) -> String {
    if path.is_empty() {
        path_to_add.to_string()
    } else {
        format!("{path};{path_to_add}")
    }
}

#[cfg(windows)]
fn ensure_windows_user_path(install_dir: &Path) -> UserPathStatus {
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE};

    let path_to_add = install_dir.to_string_lossy();
    let user_status = (|| -> std::io::Result<UserPathStatus> {
        let environment = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags("Environment", KEY_READ | KEY_WRITE)?;
        let user_path = environment
            .get_value::<String, _>("Path")
            .unwrap_or_default();
        if path_contains_windows_entry(&user_path, &path_to_add) {
            return Ok(UserPathStatus::AlreadyPresent);
        }

        let new_user_path = append_windows_path(&user_path, &path_to_add);
        environment.set_value("Path", &new_user_path)?;
        broadcast_windows_environment_change();
        Ok(UserPathStatus::Updated)
    })()
    .unwrap_or(UserPathStatus::Error);

    let process_path = std::env::var("PATH").unwrap_or_default();
    if !path_contains_windows_entry(&process_path, &path_to_add) {
        let updated = append_windows_path(&process_path, &path_to_add);
        // SAFETY: Windows synchronizes process-environment access, unlike Unix.
        unsafe { std::env::set_var("PATH", updated) };
    }

    user_status
}

#[cfg(windows)]
fn broadcast_windows_environment_change() {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SETTINGCHANGE,
    };

    let environment: Vec<u16> = std::ffi::OsStr::new("Environment")
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut result = 0;
    // SAFETY: the string remains live and NUL-terminated for the synchronous call.
    unsafe {
        SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            0,
            environment.as_ptr() as isize,
            SMTO_ABORTIFHUNG,
            5_000,
            &mut result,
        );
    }
}

#[cfg(windows)]
fn configure_git_bash() -> Result<(), GitAiError> {
    use chrono::Local;
    use std::fs::{self, OpenOptions};
    use std::io::Write;

    let git_bash_installed = [
        std::env::var_os("ProgramFiles")
            .map(std::path::PathBuf::from)
            .map(|path| path.join(r"Git\bin\bash.exe")),
        std::env::var_os("ProgramFiles(x86)")
            .map(std::path::PathBuf::from)
            .map(|path| path.join(r"Git\bin\bash.exe")),
        std::env::var_os("LOCALAPPDATA")
            .map(std::path::PathBuf::from)
            .map(|path| path.join(r"Programs\Git\bin\bash.exe")),
    ]
    .into_iter()
    .flatten()
    .any(|path| path.exists());

    if !git_bash_installed {
        return Ok(());
    }

    let home = crate::mdm::utils::home_dir();
    let bashrc = home.join(".bashrc");
    let bash_profile = home.join(".bash_profile");
    let target = if bashrc.exists() {
        bashrc
    } else if bash_profile.exists() {
        bash_profile
    } else {
        bashrc
    };

    let existing = fs::read_to_string(&target).unwrap_or_default();
    if existing.contains(".git-ai/bin") {
        println!(
            "\x1b[0;32mGit Bash already configured ({})\x1b[0m",
            target.display()
        );
        return Ok(());
    }

    let mut file = OpenOptions::new().create(true).append(true).open(&target)?;
    write!(
        file,
        "\n# Added by git-ai installer on {}\nexport PATH=\"$HOME/.git-ai/bin:$PATH\"\n",
        Local::now().format("%Y-%m-%d %H:%M:%S")
    )?;
    println!(
        "\x1b[0;32mSuccessfully configured Git Bash ({})\x1b[0m",
        target.display()
    );
    Ok(())
}

#[cfg(windows)]
fn configure_windows(install_dir: &Path) {
    let path_status = if std::env::var("GIT_AI_SKIP_PATH_UPDATE").as_deref() == Ok("1") {
        eprintln!("Skipping PATH updates because GIT_AI_SKIP_PATH_UPDATE=1");
        UserPathStatus::Skipped
    } else {
        ensure_windows_user_path(install_dir)
    };

    match path_status {
        UserPathStatus::Updated => {
            println!("\x1b[0;32mSuccessfully added git-ai to the user PATH.\x1b[0m");
        }
        UserPathStatus::AlreadyPresent => {
            println!("\x1b[0;32mgit-ai already present in the user PATH.\x1b[0m");
        }
        UserPathStatus::Error => eprintln!("Failed to update the user PATH."),
        UserPathStatus::Skipped => {}
    }

    if let Err(error) = configure_git_bash() {
        eprintln!("Warning: Failed to configure Git Bash: {error}");
    }

    println!("\x1b[0;33mClose and reopen your terminal and IDE sessions to use git-ai.\x1b[0m");
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    #[test]
    fn windows_path_matching_normalizes_case_whitespace_and_trailing_slashes() {
        let path = r" C:\Windows ;c:\Users\Alice\.git-ai\bin\;";
        assert!(path_contains_windows_entry(
            path,
            r"C:\Users\Alice\.git-ai\bin"
        ));
    }

    #[test]
    fn windows_path_matching_does_not_accept_parent_or_child_paths() {
        let path = r"C:\Users\Alice\.git-ai;C:\Users\Alice\.git-ai\bin-tools";
        assert!(!path_contains_windows_entry(
            path,
            r"C:\Users\Alice\.git-ai\bin"
        ));
    }

    #[test]
    fn windows_path_append_preserves_the_existing_value_exactly() {
        assert_eq!(
            append_windows_path(r"C:\Windows;", r"C:\Users\Alice\.git-ai\bin"),
            r"C:\Windows;;C:\Users\Alice\.git-ai\bin"
        );
        assert_eq!(
            append_windows_path("", r"C:\Users\Alice\.git-ai\bin"),
            r"C:\Users\Alice\.git-ai\bin"
        );
    }
}
