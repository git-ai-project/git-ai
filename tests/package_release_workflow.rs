fn release_workflow() -> String {
    std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/.github/workflows/release.yml"
    ))
    .unwrap()
}

fn job<'a>(workflow: &'a str, name: &str) -> &'a str {
    let start = workflow.find(&format!("  {name}:\n")).unwrap();
    let remainder = &workflow[start..];
    let end = remainder
        .match_indices("\n  ")
        .find_map(|(offset, _)| (!remainder[offset + 3..].starts_with(' ')).then_some(offset))
        .unwrap_or(remainder.len());
    &remainder[..end]
}

#[test]
fn releases_publish_core_artifacts_before_notarized_pkgs() {
    let workflow = release_workflow();
    let create_release = job(&workflow, "create-release");

    assert!(!create_release.contains("package-pkg"));
    assert!(!create_release.contains("test-pkg"));
    assert!(workflow.contains("  publish-pkg:\n"));
    assert!(workflow.contains("PKG-SHA256SUMS"));
}

#[test]
fn release_workflow_only_builds_msi_and_pkg_installers() {
    let workflow = release_workflow();

    assert!(!workflow.contains("package-deb"));
    assert!(!workflow.contains("test-deb"));
    assert!(!workflow.contains("homebrew"));
    assert!(workflow.contains("**Beta:** The Windows MSI and macOS PKG installers are beta."));
}

#[test]
fn release_credentials_stay_in_the_release_environment() {
    let workflow = release_workflow();

    for job_name in [
        "build",
        "build-macos-intel",
        "package-msi",
        "package-pkg",
        "create-release",
        "publish-pkg",
    ] {
        assert!(job(&workflow, job_name).contains("environment: release"));
    }
}

#[test]
fn apple_signing_identities_are_derived_from_the_imported_certificates() {
    let workflow = release_workflow();

    assert!(workflow.contains("security find-identity -v -p codesigning build.keychain"));
    assert!(workflow.contains("security find-identity -v -p basic build.keychain"));
    assert!(!workflow.contains("secrets.APPLE_DEVELOPER_ID_APPLICATION_IDENTITY"));
    assert!(!workflow.contains("secrets.APPLE_DEVELOPER_ID_INSTALLER_IDENTITY"));
}
