//! Daemon diagnostics upload API.

use crate::api::client::ApiClient;
use crate::api::metrics::metrics_upload_allowed;
use crate::api::types::{ApiErrorResponse, DaemonLogsUploadRequest, DaemonLogsUploadResponse};
use crate::error::GitAiError;

/// Returns whether daemon log uploads are allowed for the current API context.
///
/// This intentionally matches metrics delivery: the hosted API requires either
/// OAuth login or an API key, while custom API URLs are assumed to be deliberate.
pub fn daemon_logs_upload_allowed(api_base_url: &str, client: &ApiClient) -> bool {
    metrics_upload_allowed(api_base_url, client)
}

impl ApiClient {
    /// Upload a batch of daemon diagnostics to the server.
    pub fn upload_daemon_logs(
        &self,
        request: &DaemonLogsUploadRequest,
    ) -> Result<DaemonLogsUploadResponse, GitAiError> {
        let response = self.context().post_json("/worker/logs/upload", request)?;
        let status_code = response.status_code;

        let body = response
            .as_str()
            .map_err(|e| GitAiError::Generic(format!("Failed to read response body: {}", e)))?;

        match status_code {
            200 => {
                let logs_response: DaemonLogsUploadResponse =
                    serde_json::from_str(body).map_err(GitAiError::JsonError)?;
                Ok(logs_response)
            }
            400 => {
                let error_response: ApiErrorResponse =
                    serde_json::from_str(body).unwrap_or_else(|_| ApiErrorResponse {
                        error: "Invalid request body".to_string(),
                        details: Some(serde_json::Value::String(body.to_string())),
                    });
                Err(GitAiError::Generic(format!(
                    "Bad Request: {}",
                    error_response.error
                )))
            }
            401 => Err(GitAiError::Generic("Unauthorized".to_string())),
            500 => {
                let error_response: ApiErrorResponse =
                    serde_json::from_str(body).unwrap_or_else(|_| ApiErrorResponse {
                        error: "Internal server error".to_string(),
                        details: None,
                    });
                Err(GitAiError::Generic(format!(
                    "Internal Server Error: {}",
                    error_response.error
                )))
            }
            _ => Err(GitAiError::Generic(format!(
                "Unexpected status code {}: {}",
                status_code, body
            ))),
        }
    }
}
