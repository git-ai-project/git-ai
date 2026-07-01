use std::fs;

fn read(path: &str) -> String {
    fs::read_to_string(path).unwrap_or_else(|err| panic!("failed to read {path}: {err}"))
}

#[test]
fn macos_pkg_installs_user_launch_agent_for_daemon_login_startup() {
    let build_pkg = read("packaging/macos/build-pkg.sh");
    assert!(
        build_pkg.contains("\"$PAYLOAD/Library/LaunchAgents\""),
        "PKG payload should include the system LaunchAgents directory"
    );
    assert!(
        build_pkg.contains("install -m 0644 \"$ROOT/packaging/macos/launchagents/com.git-ai.daemon.plist\" \"$PAYLOAD/Library/LaunchAgents/com.git-ai.daemon.plist\""),
        "PKG build should install the LaunchAgent plist into the payload"
    );
    assert!(
        build_pkg.contains("xattr -cr \"$PAYLOAD\""),
        "PKG build should strip extended attributes that produce AppleDouble payload entries"
    );

    let plist = read("packaging/macos/launchagents/com.git-ai.daemon.plist");
    assert!(plist.contains("<string>com.git-ai.daemon</string>"));
    assert!(plist.contains("<string>/opt/git-ai/bin/git-ai</string>"));
    assert!(plist.contains("<string>bg</string>"));
    assert!(plist.contains("<string>run</string>"));
    assert!(plist.contains("<key>RunAtLoad</key>"));
    assert!(plist.contains("<true/>"));
    assert!(
        !plist.contains("<string>start</string>"),
        "launchd should own the long-running daemon process instead of invoking bg start"
    );
}

#[test]
fn windows_msi_registers_machine_run_key_for_daemon_login_startup() {
    let wxs = read("packaging/windows/git-ai.wxs");
    assert!(
        wxs.contains("Root=\"HKLM\""),
        "per-machine MSI should register login startup at machine scope"
    );
    assert!(
        wxs.contains("Key=\"Software\\Microsoft\\Windows\\CurrentVersion\\Run\""),
        "MSI should use the Windows Run key for user logon startup"
    );
    assert!(wxs.contains("Name=\"Git AI\""));
    assert!(
        wxs.contains("Value=\"&quot;[INSTALLFOLDER]git-ai.exe&quot; bg start\""),
        "Run key should start the per-user daemon with the installed absolute path"
    );
}
