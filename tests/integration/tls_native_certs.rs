#[test]
fn test_https_request_uses_system_certs() {
    let result = git_ai::http::Request::get("https://example.com", Some(10)).send();
    assert!(
        result.is_ok(),
        "HTTPS request to example.com failed — native TLS certs not working: {:?}",
        result.err()
    );
    let response = result.unwrap();
    assert_eq!(response.status_code, 200);
}
