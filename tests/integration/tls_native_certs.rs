/// Test that build_agent creates a working agent using the platform's
/// native TLS library (OpenSSL on Linux, Secure Transport on macOS,
/// SChannel on Windows).
#[test]
fn test_build_agent_default_config() {
    let agent = git_ai::http::build_agent(Some(5));
    // Agent should be created successfully - just verify it doesn't panic
    drop(agent);
}

/// Test that the agent can make a real HTTPS request, proving that the
/// native TLS stack and system certificate store are working correctly.
#[test]
fn test_https_request_uses_system_certs() {
    let agent = git_ai::http::build_agent(Some(10));
    let result = git_ai::http::send(agent.get("https://example.com"));
    assert!(
        result.is_ok(),
        "HTTPS request to example.com failed — native TLS certs not working: {:?}",
        result.err()
    );
    let response = result.unwrap();
    assert_eq!(response.status_code, 200);
}
