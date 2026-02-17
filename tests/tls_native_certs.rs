#[test]
fn test_native_cert_store_is_loaded() {
    let result = rustls_native_certs::load_native_certs();
    assert!(
        !result.certs.is_empty(),
        "Native certificate store should contain at least one certificate"
    );
}
