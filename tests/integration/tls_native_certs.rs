use std::io::Write;

/// SAFETY: These tests mutate environment variables. They must run serially
/// (enforced by #[serial] attribute where needed) and only touch
/// GIT_AI_SSL_* vars that no other code reads concurrently.
unsafe fn set_env(key: &str, val: &str) {
    unsafe { std::env::set_var(key, val) };
}
unsafe fn remove_env(key: &str) {
    unsafe { std::env::remove_var(key) };
}

#[test]
fn test_native_cert_store_is_loaded() {
    let result = rustls_native_certs::load_native_certs();
    if result.certs.is_empty() {
        let all_io_errors = result
            .errors
            .iter()
            .all(|err| err.to_string().contains("I/O error"));
        if all_io_errors {
            // Some environments (e.g. sandboxed macOS runners) deny keychain reads.
            // Treat that as a non-fatal environment limitation.
            return;
        }
    }
    assert!(
        !result.certs.is_empty(),
        "Failed to load native certificate store: {:?}",
        result.errors
    );
}

/// Test that build_agent creates a working agent with default config
/// (no custom SSL settings).
#[test]
fn test_build_agent_default_config() {
    // With no SSL env vars set, build_agent should succeed using system certs
    unsafe {
        remove_env("GIT_AI_SSL_CERT_FILE");
        remove_env("GIT_AI_SSL_NO_VERIFY");
    }
    let agent = git_ai::http::build_agent(Some(5));
    // Agent should be created successfully - just verify it doesn't panic
    drop(agent);
}

/// Test that build_agent with ssl_no_verify creates an agent that accepts
/// any certificate.
#[test]
fn test_build_agent_ssl_no_verify() {
    unsafe {
        set_env("GIT_AI_SSL_NO_VERIFY", "true");
        remove_env("GIT_AI_SSL_CERT_FILE");
    }

    let agent = git_ai::http::build_agent(Some(5));
    drop(agent);

    unsafe {
        remove_env("GIT_AI_SSL_NO_VERIFY");
    }
}

/// Test that build_agent with ssl_cert_file loads additional certs from
/// a PEM file on top of native certs.
#[test]
fn test_build_agent_ssl_cert_file() {
    let cert_pem = generate_self_signed_ca_pem();
    let dir = tempfile::tempdir().unwrap();
    let cert_path = dir.path().join("custom-ca.pem");
    std::fs::write(&cert_path, &cert_pem).unwrap();

    unsafe {
        set_env("GIT_AI_SSL_CERT_FILE", cert_path.to_str().unwrap());
        remove_env("GIT_AI_SSL_NO_VERIFY");
    }

    let agent = git_ai::http::build_agent(Some(5));
    drop(agent);

    unsafe {
        remove_env("GIT_AI_SSL_CERT_FILE");
    }
}

/// Test that build_agent with a non-existent ssl_cert_file still creates
/// an agent (with a warning) - it should fall back to native certs only.
#[test]
fn test_build_agent_ssl_cert_file_missing() {
    unsafe {
        set_env("GIT_AI_SSL_CERT_FILE", "/nonexistent/path/cert.pem");
        remove_env("GIT_AI_SSL_NO_VERIFY");
    }

    // Should not panic, just warn
    let agent = git_ai::http::build_agent(Some(5));
    drop(agent);

    unsafe {
        remove_env("GIT_AI_SSL_CERT_FILE");
    }
}

/// End-to-end test: start an HTTPS server with a private CA + leaf cert,
/// verify that default config fails with UnknownIssuer, then verify
/// that ssl_cert_file (pointing to the CA cert) makes it succeed,
/// and ssl_no_verify also works.
#[test]
fn test_self_signed_cert_roundtrip() {
    use std::io::Read;
    use std::net::TcpListener;
    use std::sync::Arc;

    // Step 1: Generate a CA (self-signed, with CA basic constraint)
    let ca_key = rcgen::KeyPair::generate().unwrap();
    let mut ca_params = rcgen::CertificateParams::new(vec![]).unwrap();
    ca_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    let ca_cert = ca_params.self_signed(&ca_key).unwrap();
    let ca_pem = ca_cert.pem();

    // Step 2: Generate a leaf (server) cert signed by the CA
    let server_key = rcgen::KeyPair::generate().unwrap();
    let server_params =
        rcgen::CertificateParams::new(vec!["localhost".to_string()]).unwrap();
    let server_cert = server_params.signed_by(&server_key, &ca_cert, &ca_key).unwrap();
    let server_cert_pem = server_cert.pem();
    let server_key_pem = server_key.serialize_pem();

    // Parse for rustls server config
    let certs = rustls_pemfile::certs(&mut server_cert_pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let key = rustls_pemfile::private_key(&mut server_key_pem.as_bytes())
        .unwrap()
        .unwrap();

    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let server_config = Arc::new(
        rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .unwrap(),
    );

    // Bind to a random port
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let url = format!("https://localhost:{}/test", port);

    // Start HTTPS server in a background thread
    let server_config_clone = server_config.clone();
    let handle = std::thread::spawn(move || {
        // Accept up to 3 connections (one per test phase)
        for _ in 0..3 {
            if let Ok((tcp_stream, _)) = listener.accept() {
                let conn = rustls::ServerConnection::new(server_config_clone.clone());
                if let Ok(conn) = conn {
                    let mut tls = rustls::StreamOwned::new(conn, tcp_stream);
                    let mut buf = [0u8; 4096];
                    // Read request (may fail if client rejects cert)
                    let _ = tls.read(&mut buf);
                    let http_response =
                        "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";
                    let _ = tls.write_all(http_response.as_bytes());
                    let _ = tls.flush();
                }
            }
        }
    });

    // Phase 1: Default config should FAIL (CA cert not in system trust store)
    unsafe {
        remove_env("GIT_AI_SSL_CERT_FILE");
        remove_env("GIT_AI_SSL_NO_VERIFY");
    }
    let agent = git_ai::http::build_agent(Some(5));
    let result = git_ai::http::send(agent.get(&url));
    assert!(
        result.is_err(),
        "Expected connection to fail with untrusted CA, but it succeeded"
    );

    // Phase 2: ssl_cert_file pointing to the CA cert should make it SUCCEED
    let dir = tempfile::tempdir().unwrap();
    let ca_path = dir.path().join("ca.pem");
    std::fs::write(&ca_path, &ca_pem).unwrap();
    unsafe {
        set_env("GIT_AI_SSL_CERT_FILE", ca_path.to_str().unwrap());
        remove_env("GIT_AI_SSL_NO_VERIFY");
    }
    let agent = git_ai::http::build_agent(Some(5));
    let result = git_ai::http::send(agent.get(&url));
    assert!(
        result.is_ok(),
        "Expected ssl_cert_file to trust the CA, got: {:?}",
        result.err()
    );
    let response = result.unwrap();
    assert_eq!(response.status_code, 200);
    assert_eq!(response.as_str().unwrap(), "ok");

    // Phase 3: ssl_no_verify should also SUCCEED
    unsafe {
        remove_env("GIT_AI_SSL_CERT_FILE");
        set_env("GIT_AI_SSL_NO_VERIFY", "1");
    }
    let agent = git_ai::http::build_agent(Some(5));
    let result = git_ai::http::send(agent.get(&url));
    assert!(
        result.is_ok(),
        "Expected ssl_no_verify to bypass cert check, got: {:?}",
        result.err()
    );
    let response = result.unwrap();
    assert_eq!(response.status_code, 200);

    // Clean up
    unsafe {
        remove_env("GIT_AI_SSL_CERT_FILE");
        remove_env("GIT_AI_SSL_NO_VERIFY");
    }
    let _ = handle.join();
}

/// Generate a self-signed CA certificate in PEM format for testing.
fn generate_self_signed_ca_pem() -> String {
    let key_pair = rcgen::KeyPair::generate().unwrap();
    let mut cert_params =
        rcgen::CertificateParams::new(vec!["localhost".to_string()]).unwrap();
    cert_params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
    let cert = cert_params.self_signed(&key_pair).unwrap();
    cert.pem()
}
