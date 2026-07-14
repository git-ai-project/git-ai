use crate::api::client::ApiClient;
use crate::enterprise_config::{
    EnterpriseConfigFetchResponse, EnterpriseConfigFetchResult, FETCH_ENDPOINT_PATH,
    validate_enterprise_config,
};

impl ApiClient {
    pub fn fetch_enterprise_config(&self) -> Result<EnterpriseConfigFetchResult, String> {
        let response = self
            .context()
            .get(FETCH_ENDPOINT_PATH)
            .map_err(|e| e.to_string())?;
        let status_code = response.status_code;
        let body = response
            .as_str()
            .map_err(|e| format!("failed to read enterprise config response: {e}"))?;

        if status_code != 200 {
            return Err(format!(
                "enterprise config fetch failed with status {status_code}: {body}"
            ));
        }

        let parsed: EnterpriseConfigFetchResponse = serde_json::from_str(body)
            .map_err(|e| format!("invalid enterprise config response: {e}"))?;
        if !parsed.enabled {
            return Ok(EnterpriseConfigFetchResult::Disabled);
        }

        let config = parsed
            .config
            .ok_or_else(|| "enabled enterprise config response missing config".to_string())
            .and_then(validate_enterprise_config)?;
        Ok(EnterpriseConfigFetchResult::Enabled(Box::new(config)))
    }
}
