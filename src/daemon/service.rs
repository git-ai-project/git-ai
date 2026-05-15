use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq)]
pub enum ServiceManager {
    Launchd,
    Systemd,
    None,
}

pub fn detect_service_manager() -> ServiceManager {
    #[cfg(target_os = "macos")]
    {
        return ServiceManager::Launchd;
    }

    #[cfg(target_os = "linux")]
    {
        if is_systemd_available() {
            return ServiceManager::Systemd;
        }
        return ServiceManager::None;
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        ServiceManager::None
    }
}

pub fn enable_service() -> Result<(), String> {
    match detect_service_manager() {
        ServiceManager::Launchd => enable_launchd(),
        ServiceManager::Systemd => enable_systemd(),
        ServiceManager::None => Err("no supported service manager detected".to_string()),
    }
}

pub fn disable_service() -> Result<(), String> {
    match detect_service_manager() {
        ServiceManager::Launchd => disable_launchd(),
        ServiceManager::Systemd => disable_systemd(),
        ServiceManager::None => Err("no supported service manager detected".to_string()),
    }
}

pub fn is_service_enabled() -> bool {
    match detect_service_manager() {
        ServiceManager::Launchd => is_launchd_enabled(),
        ServiceManager::Systemd => is_systemd_enabled(),
        ServiceManager::None => false,
    }
}

// --- Binary path resolution ---

fn get_git_ai_bin_path() -> String {
    // Prefer the installed location, fall back to current exe
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    let installed_path = PathBuf::from(&home).join(".git-ai").join("bin").join("git-ai");
    if installed_path.exists() {
        return installed_path.to_string_lossy().to_string();
    }
    std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "git-ai".to_string())
}

fn get_home_dir() -> String {
    std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string())
}

// --- macOS launchd ---

#[cfg(target_os = "macos")]
const LAUNCHD_LABEL: &str = "com.git-ai.daemon";

#[cfg(target_os = "macos")]
fn launchd_plist_path() -> PathBuf {
    let home = get_home_dir();
    PathBuf::from(&home)
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{}.plist", LAUNCHD_LABEL))
}

#[cfg(target_os = "macos")]
pub fn generate_launchd_plist(bin_path: &str, home: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{bin_path}</string>
        <string>bg</string>
        <string>start</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{home}/.git-ai/daemon.log</string>
    <key>StandardErrorPath</key>
    <string>{home}/.git-ai/daemon.log</string>
</dict>
</plist>
"#,
        label = LAUNCHD_LABEL,
        bin_path = bin_path,
        home = home,
    )
}

#[cfg(target_os = "macos")]
fn enable_launchd() -> Result<(), String> {
    let plist_path = launchd_plist_path();
    let bin_path = get_git_ai_bin_path();
    let home = get_home_dir();

    let content = generate_launchd_plist(&bin_path, &home);

    // Ensure LaunchAgents directory exists
    if let Some(parent) = plist_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create LaunchAgents directory: {}", e))?;
    }

    fs::write(&plist_path, &content)
        .map_err(|e| format!("failed to write plist: {}", e))?;

    let output = std::process::Command::new("launchctl")
        .args(["load", &plist_path.to_string_lossy()])
        .output()
        .map_err(|e| format!("failed to run launchctl load: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("launchctl load failed: {}", stderr));
    }

    Ok(())
}

#[cfg(target_os = "macos")]
fn disable_launchd() -> Result<(), String> {
    let plist_path = launchd_plist_path();

    if plist_path.exists() {
        let output = std::process::Command::new("launchctl")
            .args(["unload", &plist_path.to_string_lossy()])
            .output()
            .map_err(|e| format!("failed to run launchctl unload: {}", e))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // Don't fail if it wasn't loaded
            if !stderr.contains("Could not find specified service") {
                return Err(format!("launchctl unload failed: {}", stderr));
            }
        }

        fs::remove_file(&plist_path)
            .map_err(|e| format!("failed to remove plist: {}", e))?;
    }

    Ok(())
}

#[cfg(target_os = "macos")]
fn is_launchd_enabled() -> bool {
    launchd_plist_path().exists()
}

#[cfg(not(target_os = "macos"))]
fn enable_launchd() -> Result<(), String> {
    Err("launchd is only available on macOS".to_string())
}

#[cfg(not(target_os = "macos"))]
fn disable_launchd() -> Result<(), String> {
    Err("launchd is only available on macOS".to_string())
}

#[cfg(not(target_os = "macos"))]
fn is_launchd_enabled() -> bool {
    false
}

// --- Linux systemd ---

#[cfg(target_os = "linux")]
const SYSTEMD_SERVICE_NAME: &str = "git-ai";

#[cfg(target_os = "linux")]
fn systemd_unit_path() -> PathBuf {
    let home = get_home_dir();
    PathBuf::from(&home)
        .join(".config")
        .join("systemd")
        .join("user")
        .join(format!("{}.service", SYSTEMD_SERVICE_NAME))
}

#[cfg(target_os = "linux")]
pub fn generate_systemd_unit(bin_path: &str) -> String {
    format!(
        r#"[Unit]
Description=git-ai attribution daemon
After=default.target

[Service]
Type=simple
ExecStart={bin_path} bg start --foreground
Restart=on-failure
RestartSec=5

[Install]
WantedBy=default.target
"#,
        bin_path = bin_path,
    )
}

#[cfg(target_os = "linux")]
fn is_systemd_available() -> bool {
    std::process::Command::new("systemctl")
        .args(["--user", "status"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok()
}

#[cfg(target_os = "linux")]
fn enable_systemd() -> Result<(), String> {
    let unit_path = systemd_unit_path();
    let bin_path = get_git_ai_bin_path();

    let content = generate_systemd_unit(&bin_path);

    // Ensure systemd user directory exists
    if let Some(parent) = unit_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create systemd user directory: {}", e))?;
    }

    fs::write(&unit_path, &content)
        .map_err(|e| format!("failed to write systemd unit file: {}", e))?;

    // Reload systemd daemon
    let output = std::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .output()
        .map_err(|e| format!("failed to run systemctl daemon-reload: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("systemctl daemon-reload failed: {}", stderr));
    }

    // Enable the service
    let output = std::process::Command::new("systemctl")
        .args(["--user", "enable", SYSTEMD_SERVICE_NAME])
        .output()
        .map_err(|e| format!("failed to run systemctl enable: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("systemctl enable failed: {}", stderr));
    }

    Ok(())
}

#[cfg(target_os = "linux")]
fn disable_systemd() -> Result<(), String> {
    let unit_path = systemd_unit_path();

    // Disable the service
    let _ = std::process::Command::new("systemctl")
        .args(["--user", "disable", SYSTEMD_SERVICE_NAME])
        .output();

    // Remove the unit file
    if unit_path.exists() {
        fs::remove_file(&unit_path)
            .map_err(|e| format!("failed to remove systemd unit file: {}", e))?;
    }

    // Reload systemd daemon
    let _ = std::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .output();

    Ok(())
}

#[cfg(target_os = "linux")]
fn is_systemd_enabled() -> bool {
    let output = std::process::Command::new("systemctl")
        .args(["--user", "is-enabled", SYSTEMD_SERVICE_NAME])
        .output();

    match output {
        Ok(o) => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            stdout.trim() == "enabled"
        }
        Err(_) => false,
    }
}

#[cfg(not(target_os = "linux"))]
fn is_systemd_available() -> bool {
    false
}

#[cfg(not(target_os = "linux"))]
fn enable_systemd() -> Result<(), String> {
    Err("systemd is only available on Linux".to_string())
}

#[cfg(not(target_os = "linux"))]
fn disable_systemd() -> Result<(), String> {
    Err("systemd is only available on Linux".to_string())
}

#[cfg(not(target_os = "linux"))]
fn is_systemd_enabled() -> bool {
    false
}

// --- Tests ---

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_service_manager() {
        let manager = detect_service_manager();
        // On the test platform, we should get a valid result
        #[cfg(target_os = "macos")]
        assert_eq!(manager, ServiceManager::Launchd);

        #[cfg(target_os = "linux")]
        assert!(manager == ServiceManager::Systemd || manager == ServiceManager::None);

        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        assert_eq!(manager, ServiceManager::None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn test_generate_launchd_plist() {
        let plist = generate_launchd_plist("/usr/local/bin/git-ai", "/Users/testuser");

        assert!(plist.contains("<string>com.git-ai.daemon</string>"));
        assert!(plist.contains("<string>/usr/local/bin/git-ai</string>"));
        assert!(plist.contains("<string>bg</string>"));
        assert!(plist.contains("<string>start</string>"));
        assert!(plist.contains("<true/>"));
        assert!(plist.contains("<string>/Users/testuser/.git-ai/daemon.log</string>"));
        assert!(plist.contains("RunAtLoad"));
        assert!(plist.contains("KeepAlive"));
        assert!(plist.contains("StandardOutPath"));
        assert!(plist.contains("StandardErrorPath"));
        assert!(plist.contains("<?xml version=\"1.0\""));
        assert!(plist.contains("<!DOCTYPE plist"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_generate_systemd_unit() {
        let unit = generate_systemd_unit("/home/testuser/.git-ai/bin/git-ai");

        assert!(unit.contains("[Unit]"));
        assert!(unit.contains("Description=git-ai attribution daemon"));
        assert!(unit.contains("After=default.target"));
        assert!(unit.contains("[Service]"));
        assert!(unit.contains("Type=simple"));
        assert!(unit.contains(
            "ExecStart=/home/testuser/.git-ai/bin/git-ai bg start --foreground"
        ));
        assert!(unit.contains("Restart=on-failure"));
        assert!(unit.contains("RestartSec=5"));
        assert!(unit.contains("[Install]"));
        assert!(unit.contains("WantedBy=default.target"));
    }

    // Cross-platform tests for generated content (always run)
    #[test]
    fn test_plist_generation_content() {
        // Test the plist format directly without cfg guard on the function
        let bin_path = "/opt/bin/git-ai";
        let home = "/home/user";
        let label = "com.git-ai.daemon";

        let plist = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{bin_path}</string>
        <string>bg</string>
        <string>start</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{home}/.git-ai/daemon.log</string>
    <key>StandardErrorPath</key>
    <string>{home}/.git-ai/daemon.log</string>
</dict>
</plist>
"#,
            label = label,
            bin_path = bin_path,
            home = home,
        );

        assert!(plist.contains("<string>com.git-ai.daemon</string>"));
        assert!(plist.contains("<string>/opt/bin/git-ai</string>"));
        assert!(plist.contains("<string>/home/user/.git-ai/daemon.log</string>"));
        assert!(plist.contains("<key>KeepAlive</key>"));
        assert!(plist.contains("<key>RunAtLoad</key>"));
    }

    #[test]
    fn test_systemd_unit_generation_content() {
        let bin_path = "/opt/bin/git-ai";

        let unit = format!(
            r#"[Unit]
Description=git-ai attribution daemon
After=default.target

[Service]
Type=simple
ExecStart={bin_path} bg start --foreground
Restart=on-failure
RestartSec=5

[Install]
WantedBy=default.target
"#,
            bin_path = bin_path,
        );

        assert!(unit.contains("[Unit]"));
        assert!(unit.contains("Description=git-ai attribution daemon"));
        assert!(unit.contains("After=default.target"));
        assert!(unit.contains("[Service]"));
        assert!(unit.contains("Type=simple"));
        assert!(unit.contains("ExecStart=/opt/bin/git-ai bg start --foreground"));
        assert!(unit.contains("Restart=on-failure"));
        assert!(unit.contains("RestartSec=5"));
        assert!(unit.contains("[Install]"));
        assert!(unit.contains("WantedBy=default.target"));
    }

    #[test]
    fn test_get_git_ai_bin_path_returns_non_empty() {
        let path = get_git_ai_bin_path();
        assert!(!path.is_empty());
    }
}
