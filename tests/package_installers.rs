use std::path::Path;

fn packaging_path(path: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("packaging")
        .join(path)
}

#[test]
fn msi_is_per_user_and_updates_only_the_user_path() {
    let wix = std::fs::read_to_string(packaging_path("windows/git-ai.wxs")).unwrap();

    assert!(wix.contains("Scope=\"perUser\""));
    assert!(wix.contains("StandardDirectory Id=\"LocalAppDataFolder\""));
    assert!(wix.contains("System=\"no\""));
    assert!(!wix.contains("perMachine"));
    assert!(!wix.contains("ProgramFiles"));
    assert!(!wix.contains("System=\"yes\""));
}

#[test]
fn msi_accepts_hidden_api_properties_and_configures_the_installing_user() {
    let wix = std::fs::read_to_string(packaging_path("windows/git-ai.wxs")).unwrap();
    let readme = std::fs::read_to_string(packaging_path("README.md")).unwrap();

    assert!(wix.contains("Property Id=\"API_BASE\" Hidden=\"yes\""));
    assert!(wix.contains("Property Id=\"API_KEY\" Hidden=\"yes\""));
    assert!(wix.contains("FileRef=\"GitAiExe\""));
    assert!(wix.contains("Execute=\"deferred\""));
    assert!(wix.contains("Impersonate=\"yes\""));
    assert!(wix.contains("HideTarget=\"yes\""));
    assert!(wix.contains("setup-package --manager msi"));
    assert!(readme.contains("msiexec /i"));
    assert!(readme.contains("API_BASE="));
    assert!(readme.contains("API_KEY="));
}

#[test]
fn packaging_supports_only_msi_and_pkg() {
    assert!(packaging_path("windows").is_dir());
    assert!(packaging_path("macos").is_dir());
    assert!(!packaging_path("debian/build-deb.sh").exists());
    assert!(!packaging_path("homebrew/update-formula.sh").exists());
}

#[test]
fn installer_docs_cover_customer_setup_and_internal_release_policy() {
    let customer_readme = std::fs::read_to_string("README.md").unwrap();
    let release_runbook = std::fs::read_to_string("docs/installer-release-runbook.md").unwrap();

    assert!(customer_readme.contains("git-ai-windows-x64.msi"));
    assert!(customer_readme.contains("git-ai-macos-arm64.pkg"));
    assert!(customer_readme.contains("API_BASE="));
    assert!(customer_readme.contains("API_KEY="));
    assert!(release_runbook.contains("release-prod"));
    assert!(release_runbook.contains("release_production"));
    assert!(release_runbook.contains("PKG-SHA256SUMS"));
    assert!(release_runbook.contains("UTM"));
    assert!(!release_runbook.contains("Homebrew"));
    assert!(!release_runbook.contains("apt"));
}
