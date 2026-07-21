use std::fs;

fn read(path: &str) -> String {
    fs::read_to_string(path).unwrap_or_else(|err| panic!("failed to read {path}: {err}"))
}

#[test]
fn macos_pkg_installs_a_user_session_launch_agent() {
    let build_pkg = read("packaging/macos/build-pkg.sh");
    let postinstall = read("packaging/macos/scripts/postinstall");

    assert!(build_pkg.contains("$SCRIPTS/git-ai-daemon-launcher"));
    assert!(build_pkg.contains("$SCRIPTS/com.git-ai.daemon.plist"));
    assert!(postinstall.contains("$USER_HOME/Library/LaunchAgents"));
    assert!(postinstall.contains("$GIT_AI_BIN_DIR/git-ai-daemon-launcher"));

    let plist = read("packaging/macos/launchagents/com.git-ai.daemon.plist");
    assert!(plist.contains("<string>com.git-ai.daemon</string>"));
    assert!(plist.contains("<key>RunAtLoad</key>"));
    assert!(plist.contains("$HOME/.git-ai/bin/git-ai-daemon-launcher"));

    let launcher = read("packaging/macos/launchagents/git-ai-daemon-launcher");
    assert!(launcher.contains("$HOME/.git-ai/bin/git-ai"));
    assert!(!launcher.contains("/opt/git-ai"));
}

#[test]
fn windows_msi_registers_login_startup_per_user() {
    let wix = read("packaging/windows/git-ai.wxs");

    assert!(wix.contains("File Id=\"GitAiLoginLauncher\""));
    assert!(wix.contains("Root=\"HKCU\""));
    assert!(wix.contains("Key=\"Software\\Microsoft\\Windows\\CurrentVersion\\Run\""));
    assert!(!wix.contains("Root=\"HKLM\""));
    assert!(wix.contains("Value=\"&quot;[INSTALLFOLDER]git-ai-login.cmd&quot;\""));

    let launcher = read("packaging/windows/git-ai-login.cmd");
    assert!(launcher.contains("%USERPROFILE%\\.git-ai\\bin\\git-ai.exe"));
    assert!(launcher.contains("\"%GIT_AI_USER_BINARY%\" bg start"));
    assert!(launcher.contains("\"%~dp0git-ai.exe\" bg start"));
}
