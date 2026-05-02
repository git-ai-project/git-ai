use git_ai::perf::subprocess_instrumentation::{
    enable_instrumentation, SubprocessCategory, SubprocessContext,
};
use git_ai::git::repository::exec_git_with_context;

#[test]
fn test_instrumentation_basic() {
    // Note: Instrumentation must be enabled via env var for these tests to work
    // This test verifies the API compiles and works correctly when enabled
    enable_instrumentation();

    let ctx = SubprocessContext::new(SubprocessCategory::RepositoryQuery, "version")
        .critical_path(false)
        .label("test-version-check");

    let result = exec_git_with_context(&["--version".to_string()], ctx);
    assert!(result.is_ok());
}
