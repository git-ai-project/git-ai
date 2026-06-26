//! Notes API endpoints for the HTTP notes backend.
//!
//! Authentication is handled automatically by `ApiContext`: the existing
//! `X-API-Key` / Bearer token headers are attached on every request.
//! The daemon flusher should skip uploads when neither `is_logged_in()` nor
//! `has_api_key()` is true (matching the CAS pattern).

use crate::api::client::ApiClient;
use crate::api::types::{
    ApiErrorResponse, NotesReadResponse, NotesUploadRequest, NotesUploadResponse,
};
use crate::error::GitAiError;

impl ApiClient {
    /// Upload a batch of authorship notes to the remote backend.
    ///
    /// # Arguments
    /// * `request` - The notes upload request containing entries to upload
    ///
    /// # Returns
    /// * `Ok(NotesUploadResponse)` - Success response with counts
    /// * `Err(GitAiError)` - On network or server errors
    pub fn upload_notes(
        &self,
        request: NotesUploadRequest,
    ) -> Result<NotesUploadResponse, GitAiError> {
        let response = self.context().post_json("/worker/notes/upload", &request)?;
        let status_code = response.status_code;

        let body = response
            .as_str()
            .map_err(|e| GitAiError::Generic(format!("Failed to read response body: {}", e)))?;

        match status_code {
            200 => serde_json::from_str(body).map_err(GitAiError::JsonError),
            400 => {
                let err: ApiErrorResponse =
                    serde_json::from_str(body).unwrap_or_else(|_| ApiErrorResponse {
                        error: "Invalid request body".to_string(),
                        details: Some(serde_json::Value::String(body.to_string())),
                    });
                Err(GitAiError::Generic(format!("Bad Request: {}", err.error)))
            }
            _ => Err(GitAiError::Generic(format!(
                "Notes upload failed with status {}: {}",
                status_code, body
            ))),
        }
    }

    /// Read authorship notes by commit SHAs. Max 100 per call.
    ///
    /// Returns an empty map for any SHAs not found (404 is treated as success).
    ///
    /// # Arguments
    /// * `commit_shas` - Slice of hex commit SHAs to fetch
    ///
    /// # Returns
    /// * `Ok(NotesReadResponse)` - Response mapping commit_sha → note content
    /// * `Err(GitAiError)` - On invalid input, network, or server errors
    pub fn read_notes(&self, commit_shas: &[&str]) -> Result<NotesReadResponse, GitAiError> {
        // Validate that all SHAs are hex strings before making the request
        for sha in commit_shas {
            if !sha.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(GitAiError::Generic(format!(
                    "Commit SHA contains non-hex characters: {}",
                    sha
                )));
            }
        }

        let query = commit_shas.join(",");
        let endpoint = format!("/worker/notes/?commits={}", query);
        let response = self.context().get(&endpoint)?;
        let status_code = response.status_code;

        let body = response
            .as_str()
            .map_err(|e| GitAiError::Generic(format!("Failed to read response body: {}", e)))?;

        match status_code {
            200 => serde_json::from_str(body).map_err(GitAiError::JsonError),
            404 => Ok(NotesReadResponse {
                notes: std::collections::HashMap::new(),
            }),
            _ => Err(GitAiError::Generic(format!(
                "Notes read failed with status {}: {}",
                status_code, body
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::client::{ApiClient, ApiContext};
    use crate::api::types::NoteEntry;

    #[test]
    fn test_read_notes_rejects_non_hex_sha() {
        let ctx = ApiContext::without_auth(Some("https://example.com".to_string()));
        let client = ApiClient::new(ctx);

        let result = client.read_notes(&["not-a-hex-sha"]);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("non-hex"),
            "error should mention non-hex: {}",
            err
        );
    }

    #[test]
    fn test_read_notes_accepts_valid_hex_sha() {
        // A valid hex SHA should pass validation (the actual HTTP call will fail
        // because there is no real server, but we are testing input validation only).
        let ctx = ApiContext::without_auth(Some("https://127.0.0.1:1".to_string()));
        let client = ApiClient::new(ctx);

        let valid_sha = "abc123def456abc123def456abc123def456abc1";
        // This will fail on the HTTP call, not on validation
        let result = client.read_notes(&[valid_sha]);
        // The error should be network-related, not a validation error
        if let Err(e) = result {
            let msg = e.to_string();
            assert!(
                !msg.contains("non-hex"),
                "should not fail hex validation for valid SHA, got: {}",
                msg
            );
        }
    }

    #[test]
    fn test_notes_upload_request_serialization() {
        let request = NotesUploadRequest {
            entries: vec![NoteEntry {
                commit_sha: "abc123".to_string(),
                content: "authorship data".to_string(),
            }],
        };

        let json = serde_json::to_string(&request).unwrap();
        assert!(json.contains("abc123"));
        assert!(json.contains("authorship data"));
        assert!(json.contains("entries"));
    }

    #[test]
    fn test_notes_read_response_deserialization() {
        let json = r#"{"notes": {"abc123": "content1", "def456": "content2"}}"#;
        let response: NotesReadResponse = serde_json::from_str(json).unwrap();
        assert_eq!(response.notes.get("abc123"), Some(&"content1".to_string()));
        assert_eq!(response.notes.get("def456"), Some(&"content2".to_string()));
    }

    #[test]
    fn test_notes_upload_response_deserialization() {
        let json = r#"{"success_count": 5, "failure_count": 1}"#;
        let response: NotesUploadResponse = serde_json::from_str(json).unwrap();
        assert_eq!(response.success_count, 5);
        assert_eq!(response.failure_count, 1);
    }
}
