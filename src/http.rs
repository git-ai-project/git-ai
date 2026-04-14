use crate::config;
use rustls::ClientConfig;
use rustls_pki_types::CertificateDer;
use std::fs::File;
use std::io::BufReader;
use std::sync::Arc;
use std::time::Duration;

/// Build a ureq Agent with TLS configuration based on current config.
///
/// If `ssl_cert_file` is set, loads additional CA certificates from that file
/// on top of the native system certificates. If `ssl_no_verify` is set,
/// disables all certificate verification (dangerous, but needed for some
/// self-hosted setups).
///
/// Otherwise, uses ureq's default native-certs behavior.
pub fn build_agent(timeout_secs: Option<u64>) -> ureq::Agent {
    let cfg = config::Config::fresh();
    let has_custom_ssl = cfg.ssl_cert_file().is_some() || cfg.ssl_no_verify();

    let mut builder = ureq::AgentBuilder::new();

    if let Some(secs) = timeout_secs {
        builder = builder.timeout(Duration::from_secs(secs));
    }

    if has_custom_ssl {
        let tls_config = build_custom_tls_config(&cfg);
        builder = builder.tls_config(Arc::new(tls_config));
    }

    builder.build()
}

fn build_custom_tls_config(cfg: &config::Config) -> ClientConfig {
    let provider = Arc::new(rustls::crypto::ring::default_provider());

    if cfg.ssl_no_verify() {
        return build_no_verify_config(provider);
    }

    // Load native certs + custom CA cert
    let mut root_store = rustls::RootCertStore::empty();

    // Load system certificates
    let native_certs = rustls_native_certs::load_native_certs();
    for cert in native_certs.certs {
        let _ = root_store.add(cert);
    }

    // Load additional CA certs from ssl_cert_file
    if let Some(cert_path) = cfg.ssl_cert_file() {
        match load_pem_certs(cert_path) {
            Ok(certs) => {
                for cert in certs {
                    let _ = root_store.add(cert);
                }
            }
            Err(e) => {
                eprintln!(
                    "Warning: Failed to load certificates from '{}': {}",
                    cert_path, e
                );
            }
        }
    }

    ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("TLS protocol versions")
        .with_root_certificates(root_store)
        .with_no_client_auth()
}

fn build_no_verify_config(provider: Arc<rustls::crypto::CryptoProvider>) -> ClientConfig {
    ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("TLS protocol versions")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(NoVerifier))
        .with_no_client_auth()
}

fn load_pem_certs(path: &str) -> Result<Vec<CertificateDer<'static>>, String> {
    let file = File::open(path).map_err(|e| format!("Cannot open '{}': {}", path, e))?;
    let mut reader = BufReader::new(file);

    rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("Failed to parse PEM certificates from '{}': {}", path, e))
}

/// A certificate verifier that accepts any certificate.
/// Only used when ssl_no_verify is enabled.
#[derive(Debug)]
struct NoVerifier;

impl rustls::client::danger::ServerCertVerifier for NoVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![
            rustls::SignatureScheme::RSA_PKCS1_SHA256,
            rustls::SignatureScheme::RSA_PKCS1_SHA384,
            rustls::SignatureScheme::RSA_PKCS1_SHA512,
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
            rustls::SignatureScheme::ECDSA_NISTP521_SHA512,
            rustls::SignatureScheme::RSA_PSS_SHA256,
            rustls::SignatureScheme::RSA_PSS_SHA384,
            rustls::SignatureScheme::RSA_PSS_SHA512,
            rustls::SignatureScheme::ED25519,
            rustls::SignatureScheme::ED448,
        ]
    }
}

/// HTTP response wrapper that normalizes ureq's error handling.
/// ureq treats non-2xx responses as errors; this wrapper treats them as normal
/// responses (matching minreq's previous behavior and what callers expect).
pub struct Response {
    pub status_code: u16,
    body: Vec<u8>,
}

impl Response {
    pub fn as_str(&self) -> Result<&str, std::str::Utf8Error> {
        std::str::from_utf8(&self.body)
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.body
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.body
    }
}

fn read_ureq_response(response: ureq::Response) -> Result<Response, String> {
    let status_code = response.status();
    let mut body = Vec::new();
    response
        .into_reader()
        .read_to_end(&mut body)
        .map_err(|e| format!("Failed to read response body: {}", e))?;
    Ok(Response { status_code, body })
}

/// Execute a ureq request, normalizing errors so that HTTP error status codes
/// are returned as Ok(Response) rather than Err.
pub fn send(request: ureq::Request) -> Result<Response, String> {
    match request.call() {
        Ok(response) => read_ureq_response(response),
        Err(ureq::Error::Status(_code, response)) => read_ureq_response(response),
        Err(ureq::Error::Transport(err)) => Err(err.to_string()),
    }
}

/// Execute a ureq request with a string body.
pub fn send_with_body(request: ureq::Request, body: &str) -> Result<Response, String> {
    match request.send_string(body) {
        Ok(response) => read_ureq_response(response),
        Err(ureq::Error::Status(_code, response)) => read_ureq_response(response),
        Err(ureq::Error::Transport(err)) => Err(err.to_string()),
    }
}

use std::io::Read;
