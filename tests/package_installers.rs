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
fn packaging_supports_only_msi_and_pkg() {
    assert!(packaging_path("windows").is_dir());
    assert!(packaging_path("macos").is_dir());
    assert!(!packaging_path("debian/build-deb.sh").exists());
    assert!(!packaging_path("homebrew/update-formula.sh").exists());
}
