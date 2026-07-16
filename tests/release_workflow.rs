use std::fs;

fn read_workflow() -> String {
    fs::read_to_string(".github/workflows/release.yml")
        .expect("release workflow should be readable")
}

#[test]
fn production_releases_use_one_separate_approval_gate() {
    let workflow = read_workflow();

    assert!(workflow.contains("  release-prod-approval:\n    name: Approve production release"));
    assert!(workflow.contains("    environment: release-prod"));
    assert!(workflow.contains("    environment: release\n"));
    assert!(
        !workflow.contains("  authorize-admin:\n"),
        "pre-releases must not require repository-admin approval"
    );
    assert!(workflow.contains("    needs: release-prod-approval\n"));
}
