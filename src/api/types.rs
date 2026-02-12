use crate::authorship::authorship_log::{LineRange, PromptRecord};
use crate::commands::diff::FileDiffJson;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// File record for API - converts LineRange annotations to API format
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApiFileRecord {
    /// Maps prompt_hash to line numbers/ranges
    /// Example: { "prompt_abc123": [[1, 5], 10] } means lines 1-5 and line 10 attributed to prompt_abc123
    pub annotations: HashMap<String, Vec<serde_json::Value>>,
    /// Git diff output
    pub diff: String,
    /// Original file content before changes
    #[serde(rename = "base_content")]
    pub base_content: String,
}

impl From<&FileDiffJson> for ApiFileRecord {
    fn from(file_diff: &FileDiffJson) -> Self {
        let annotations: HashMap<String, Vec<serde_json::Value>> = file_diff
            .annotations
            .iter()
            .map(|(key, ranges)| {
                let json_ranges: Vec<serde_json::Value> = ranges
                    .iter()
                    .map(|range| match range {
                        LineRange::Single(line) => serde_json::Value::Number((*line as u64).into()),
                        LineRange::Range(start, end) => serde_json::Value::Array(vec![
                            serde_json::Value::Number((*start as u64).into()),
                            serde_json::Value::Number((*end as u64).into()),
                        ]),
                    })
                    .collect();
                (key.clone(), json_ranges)
            })
            .collect();

        Self {
            annotations,
            diff: file_diff.diff.clone(),
            base_content: file_diff.base_content.clone(),
        }
    }
}

/// Bundle data containing prompts and optional files
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BundleData {
    /// REQUIRED: At least one prompt
    pub prompts: HashMap<String, PromptRecord>,
    /// OPTIONAL: File diffs and annotations
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub files: HashMap<String, ApiFileRecord>,
}

/// Request body for creating a bundle
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CreateBundleRequest {
    /// Bundle title (min 1 character)
    pub title: String,
    /// Bundle data containing prompts and optional files
    pub data: BundleData,
    // TODO PR Metadata if linked to PR
}

/// Success response from bundle creation
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CreateBundleResponse {
    pub success: bool,
    pub id: String,
    pub url: String,
}

/// Error response from API
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ApiErrorResponse {
    pub error: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

/// Single CAS object for upload
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CasObject {
    pub content: serde_json::Value,
    pub hash: String,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub metadata: HashMap<String, String>,
}

/// Request body for CAS upload
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CasUploadRequest {
    pub objects: Vec<CasObject>,
}

/// Result for a single CAS object upload
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CasUploadResult {
    pub hash: String,
    pub status: String, // "ok" or "error"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Response from CAS upload
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CasUploadResponse {
    pub results: Vec<CasUploadResult>,
    pub success_count: usize,
    pub failure_count: usize,
}

/// Wrapper for messages stored in CAS
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CasMessagesObject {
    pub messages: Vec<crate::authorship::transcript::Message>,
}

/// Single result from CA prompt store batch read
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CAPromptStoreReadResult {
    pub hash: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Response from CA prompt store batch read
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CAPromptStoreReadResponse {
    pub results: Vec<CAPromptStoreReadResult>,
    pub success_count: usize,
    pub failure_count: usize,
}
