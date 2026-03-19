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
