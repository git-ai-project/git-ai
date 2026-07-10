use std::io::{Read, Write};
use std::sync::Arc;
use std::time::Duration;

const MAX_HTTP_RESPONSE_BODY_BYTES: usize = 32 * 1_024 * 1_024;
const MAX_HTTP_REQUEST_BODY_BYTES: usize = 64 * 1_024 * 1_024;

/// Build a ureq Agent that uses the platform's native TLS library.
///
/// Uses OpenSSL on Linux, Secure Transport on macOS, and SChannel on
/// Windows — the same TLS implementations that curl uses. This ensures
/// certificates trusted by the OS (including custom CA certs added to
/// the system trust store) are handled identically to curl and browsers.
pub fn build_agent(timeout_secs: Option<u64>) -> ureq::Agent {
    let tls = native_tls::TlsConnector::new().expect("failed to create TLS connector");
    let mut builder = ureq::AgentBuilder::new().tls_connector(Arc::new(tls));

    if let Some(secs) = timeout_secs {
        builder = builder.timeout(Duration::from_secs(secs));
    }

    builder.build()
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
    if response
        .header("Content-Length")
        .and_then(|value| value.parse::<usize>().ok())
        .is_some_and(|length| length > MAX_HTTP_RESPONSE_BODY_BYTES)
    {
        return Err(format!(
            "HTTP response body exceeded the {MAX_HTTP_RESPONSE_BODY_BYTES} byte limit"
        ));
    }
    let body = read_body_with_limit(response.into_reader(), MAX_HTTP_RESPONSE_BODY_BYTES)?;
    Ok(Response { status_code, body })
}

fn read_body_with_limit(reader: impl Read, limit: usize) -> Result<Vec<u8>, String> {
    let read_limit = u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1);
    let mut limited = reader.take(read_limit);
    let mut body = Vec::new();
    limited
        .read_to_end(&mut body)
        .map_err(|e| format!("Failed to read response body: {e}"))?;
    if body.len() > limit {
        return Err(format!(
            "HTTP response body exceeded the {limit} byte limit"
        ));
    }
    Ok(body)
}

struct BoundedJsonWriter {
    bytes: Vec<u8>,
    limit: usize,
}

impl Write for BoundedJsonWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let remaining = self.limit.saturating_sub(self.bytes.len());
        if buf.len() > remaining {
            return Err(std::io::Error::other(format!(
                "HTTP JSON request body exceeded the {} byte limit",
                self.limit
            )));
        }
        self.bytes.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub(crate) fn serialize_json_body<T: serde::Serialize + ?Sized>(
    body: &T,
) -> Result<String, serde_json::Error> {
    serialize_json_with_limit(body, MAX_HTTP_REQUEST_BODY_BYTES)
}

fn serialize_json_with_limit<T: serde::Serialize + ?Sized>(
    body: &T,
    limit: usize,
) -> Result<String, serde_json::Error> {
    let mut writer = BoundedJsonWriter {
        bytes: Vec::new(),
        limit,
    };
    serde_json::to_writer(&mut writer, body)?;
    String::from_utf8(writer.bytes).map_err(|error| {
        serde_json::Error::io(std::io::Error::new(std::io::ErrorKind::InvalidData, error))
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn response_body_accepts_exact_byte_limit() {
        let body = read_body_with_limit(Cursor::new(b"12345678"), 8).unwrap();

        assert_eq!(body, b"12345678");
    }

    #[test]
    fn response_body_rejects_one_byte_over_limit() {
        let error = read_body_with_limit(Cursor::new(b"123456789"), 8)
            .expect_err("HTTP bodies must be rejected after the first excess byte");

        assert!(error.contains("8 byte limit"));
    }

    #[test]
    fn bounded_json_serialization_accepts_exact_limit() {
        let value = "123456";
        let serialized = serialize_json_with_limit(&value, 8).unwrap();

        assert_eq!(serialized, "\"123456\"");
    }

    #[test]
    fn bounded_json_serialization_rejects_first_excess_write() {
        let error = serialize_json_with_limit(&"1234567", 8)
            .expect_err("JSON serialization must stop at the configured byte limit");

        assert!(error.to_string().contains("8 byte limit"));
    }
}
