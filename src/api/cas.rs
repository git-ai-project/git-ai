use crate::api::client::ApiClient;
use crate::api::types::{
    ApiErrorResponse, CAPromptStoreReadResponse, CasUploadRequest, CasUploadResponse,
};
use crate::error::GitAiError;

/// CAS API endpoints
impl ApiClient {
    /// Upload CAS objects to the server
    ///
    /// # Arguments
    /// * `request` - The CAS upload request containing objects to upload
    ///
    /// # Returns
    /// * `Ok(CasUploadResponse)` - Success response
    /// * `Err(GitAiError)` - Error response
    pub fn upload_cas(&self, request: CasUploadRequest) -> Result<CasUploadResponse, GitAiError> {
        let response = self.context().post_json("/worker/cas/upload", &request)?;
        let status_code = response.status_code;

        let body = response
            .as_str()
            .map_err(|e| GitAiError::Generic(format!("Failed to read response body: {}", e)))?;

        match status_code {
            200 => {
                let cas_response: CasUploadResponse =
                    serde_json::from_str(body).map_err(GitAiError::JsonError)?;
                Ok(cas_response)
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

    /// Read CAS objects by hash from the server
    ///
    /// # Arguments
    /// * `hashes` - Slice of CAS hashes to fetch (max 100 per call)
    ///
    /// # Returns
    /// * `Ok(CAPromptStoreReadResponse)` - Response with results for each hash
    /// * `Err(GitAiError)` - On network or server errors
    pub fn read_ca_prompt_store(
        &self,
        hashes: &[&str],
    ) -> Result<CAPromptStoreReadResponse, GitAiError> {
        let query = hashes.join(",");
        let endpoint = format!("/worker/cas/?hashes={}", query);
        let response = self.context().get(&endpoint)?;
        let status_code = response.status_code;

        let body = response
            .as_str()
            .map_err(|e| GitAiError::Generic(format!("Failed to read response body: {}", e)))?;

        match status_code {
            200 => {
                let cas_response: CAPromptStoreReadResponse =
                    serde_json::from_str(body).map_err(GitAiError::JsonError)?;
                Ok(cas_response)
            }
            404 => {
                // All hashes not found — return empty response gracefully
                Ok(CAPromptStoreReadResponse {
                    results: Vec::new(),
                    success_count: 0,
                    failure_count: hashes.len(),
                })
            }
            _ => Err(GitAiError::Generic(format!(
                "CAS read failed with status {}: {}",
                status_code, body
            ))),
        }
    }
}
