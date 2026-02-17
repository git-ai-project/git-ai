//! Common attributes shared across all metric events.

use super::pos_encoded::{PosEncoded, PosField, sparse_get_string, sparse_set, string_to_json};
use super::types::SparseArray;

/// Attribute positions (shared across all events).
pub mod attr_pos {
    pub const GIT_AI_VERSION: usize = 0;
    pub const REPO_URL: usize = 1;
    pub const AUTHOR: usize = 2;
    pub const COMMIT_SHA: usize = 3;
    pub const BASE_COMMIT_SHA: usize = 4;
    pub const BRANCH: usize = 5;
    pub const TOOL: usize = 20;
    pub const MODEL: usize = 21;
    pub const PROMPT_ID: usize = 22;
    pub const EXTERNAL_PROMPT_ID: usize = 23;
}

/// Common attributes for all events.
///
/// | Position | Name | Type | Required |
/// |----------|------|------|----------|
/// | 0 | git_ai_version | String | Yes |
/// | 1 | repo_url | String | No (nullable) |
/// | 2 | author | String | No (nullable) |
/// | 3 | commit_sha | String | No (nullable) |
/// | 4 | base_commit_sha | String | No (nullable) |
/// | 5 | branch | String | No (nullable) |
/// | 20 | tool | String | No (nullable) |
/// | 21 | model | String | No (nullable) |
/// | 22 | prompt_id | String | No (nullable) |
/// | 23 | external_prompt_id | String | No (nullable) |
#[derive(Debug, Clone, Default)]
pub struct EventAttributes {
    pub git_ai_version: PosField<String>,
    pub repo_url: PosField<String>,
    pub author: PosField<String>,
    pub commit_sha: PosField<String>,
    pub base_commit_sha: PosField<String>,
    pub branch: PosField<String>,
    pub tool: PosField<String>,
    pub model: PosField<String>,
    pub prompt_id: PosField<String>,
    pub external_prompt_id: PosField<String>,
}

impl EventAttributes {
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create with required git_ai_version field set.
    pub fn with_version(version: impl Into<String>) -> Self {
        Self {
            git_ai_version: Some(Some(version.into())),
            ..Default::default()
        }
    }

    // Builder methods for git_ai_version
    #[allow(dead_code)]
    pub fn git_ai_version(mut self, value: impl Into<String>) -> Self {
        self.git_ai_version = Some(Some(value.into()));
        self
    }

    #[allow(dead_code)]
    pub fn git_ai_version_null(mut self) -> Self {
        self.git_ai_version = Some(None);
        self
    }

    // Builder methods for repo_url
    pub fn repo_url(mut self, value: impl Into<String>) -> Self {
        self.repo_url = Some(Some(value.into()));
        self
    }

    #[allow(dead_code)]
    pub fn repo_url_null(mut self) -> Self {
        self.repo_url = Some(None);
        self
    }

    // Builder methods for author
    pub fn author(mut self, value: impl Into<String>) -> Self {
        self.author = Some(Some(value.into()));
        self
    }

    #[allow(dead_code)]
    pub fn author_null(mut self) -> Self {
        self.author = Some(None);
        self
    }

    // Builder methods for commit_sha
    pub fn commit_sha(mut self, value: impl Into<String>) -> Self {
        self.commit_sha = Some(Some(value.into()));
        self
    }

    #[allow(dead_code)]
    pub fn commit_sha_null(mut self) -> Self {
        self.commit_sha = Some(None);
        self
    }

    // Builder methods for base_commit_sha
    pub fn base_commit_sha(mut self, value: impl Into<String>) -> Self {
        self.base_commit_sha = Some(Some(value.into()));
        self
    }

    #[allow(dead_code)]
    pub fn base_commit_sha_null(mut self) -> Self {
        self.base_commit_sha = Some(None);
        self
    }

    // Builder methods for branch
    pub fn branch(mut self, value: impl Into<String>) -> Self {
        self.branch = Some(Some(value.into()));
        self
    }

    #[allow(dead_code)]
    pub fn branch_null(mut self) -> Self {
        self.branch = Some(None);
        self
    }

    // Builder methods for tool
    pub fn tool(mut self, value: impl Into<String>) -> Self {
        self.tool = Some(Some(value.into()));
        self
    }

    #[allow(dead_code)]
    pub fn tool_null(mut self) -> Self {
        self.tool = Some(None);
        self
    }

    // Builder methods for model
    pub fn model(mut self, value: impl Into<String>) -> Self {
        self.model = Some(Some(value.into()));
        self
    }

    #[allow(dead_code)]
    pub fn model_null(mut self) -> Self {
        self.model = Some(None);
        self
    }

    // Builder methods for prompt_id
    pub fn prompt_id(mut self, value: impl Into<String>) -> Self {
        self.prompt_id = Some(Some(value.into()));
        self
    }

    #[allow(dead_code)]
    pub fn prompt_id_null(mut self) -> Self {
        self.prompt_id = Some(None);
        self
    }

    // Builder methods for external_prompt_id
    pub fn external_prompt_id(mut self, value: impl Into<String>) -> Self {
        self.external_prompt_id = Some(Some(value.into()));
        self
    }

    #[allow(dead_code)]
    pub fn external_prompt_id_null(mut self) -> Self {
        self.external_prompt_id = Some(None);
        self
    }
}

impl PosEncoded for EventAttributes {
    fn to_sparse(&self) -> SparseArray {
        let mut map = SparseArray::new();
        sparse_set(
            &mut map,
            attr_pos::GIT_AI_VERSION,
            string_to_json(&self.git_ai_version),
        );
        sparse_set(&mut map, attr_pos::REPO_URL, string_to_json(&self.repo_url));
        sparse_set(&mut map, attr_pos::AUTHOR, string_to_json(&self.author));
        sparse_set(
            &mut map,
            attr_pos::COMMIT_SHA,
            string_to_json(&self.commit_sha),
        );
        sparse_set(
            &mut map,
            attr_pos::BASE_COMMIT_SHA,
            string_to_json(&self.base_commit_sha),
        );
        sparse_set(&mut map, attr_pos::BRANCH, string_to_json(&self.branch));
        sparse_set(&mut map, attr_pos::TOOL, string_to_json(&self.tool));
        sparse_set(&mut map, attr_pos::MODEL, string_to_json(&self.model));
        sparse_set(
            &mut map,
            attr_pos::PROMPT_ID,
            string_to_json(&self.prompt_id),
        );
        sparse_set(
            &mut map,
            attr_pos::EXTERNAL_PROMPT_ID,
            string_to_json(&self.external_prompt_id),
        );
        map
    }

    fn from_sparse(arr: &SparseArray) -> Self {
        Self {
            git_ai_version: sparse_get_string(arr, attr_pos::GIT_AI_VERSION),
            repo_url: sparse_get_string(arr, attr_pos::REPO_URL),
            author: sparse_get_string(arr, attr_pos::AUTHOR),
            commit_sha: sparse_get_string(arr, attr_pos::COMMIT_SHA),
            base_commit_sha: sparse_get_string(arr, attr_pos::BASE_COMMIT_SHA),
            branch: sparse_get_string(arr, attr_pos::BRANCH),
            tool: sparse_get_string(arr, attr_pos::TOOL),
            model: sparse_get_string(arr, attr_pos::MODEL),
            prompt_id: sparse_get_string(arr, attr_pos::PROMPT_ID),
            external_prompt_id: sparse_get_string(arr, attr_pos::EXTERNAL_PROMPT_ID),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn test_event_attributes_builder() {
        let attrs = EventAttributes::with_version("1.0.0")
            .repo_url("https://github.com/user/repo")
            .author("user@example.com")
            .commit_sha("commit-123")
            .base_commit_sha("base-commit-123")
            .branch("main")
            .tool("claude-code")
            .model_null()
            .prompt_id("prompt-123");

        assert_eq!(attrs.git_ai_version, Some(Some("1.0.0".to_string())));
        assert_eq!(
            attrs.repo_url,
            Some(Some("https://github.com/user/repo".to_string()))
        );
        assert_eq!(attrs.author, Some(Some("user@example.com".to_string())));
        assert_eq!(attrs.commit_sha, Some(Some("commit-123".to_string())));
        assert_eq!(
            attrs.base_commit_sha,
            Some(Some("base-commit-123".to_string()))
        );
        assert_eq!(attrs.branch, Some(Some("main".to_string())));
        assert_eq!(attrs.tool, Some(Some("claude-code".to_string())));
        assert_eq!(attrs.model, Some(None)); // explicitly null
        assert_eq!(attrs.prompt_id, Some(Some("prompt-123".to_string())));
    }

    #[test]
    fn test_event_attributes_to_sparse() {
        let attrs = EventAttributes::with_version("1.0.0")
            .tool("test-tool")
            .model_null()
            .prompt_id("prompt-123");

        let sparse = attrs.to_sparse();

        assert_eq!(sparse.get("0"), Some(&Value::String("1.0.0".to_string())));
        assert_eq!(sparse.get("1"), None); // not set
        assert_eq!(sparse.get("2"), None); // not set
        assert_eq!(sparse.get("3"), None); // not set
        assert_eq!(sparse.get("4"), None); // not set
        assert_eq!(sparse.get("5"), None); // not set
        assert_eq!(
            sparse.get("20"),
            Some(&Value::String("test-tool".to_string()))
        );
        assert_eq!(sparse.get("21"), Some(&Value::Null)); // explicitly null
        assert_eq!(
            sparse.get("22"),
            Some(&Value::String("prompt-123".to_string()))
        );
    }

    #[test]
    fn test_event_attributes_from_sparse() {
        let mut sparse = SparseArray::new();
        sparse.insert("0".to_string(), Value::String("2.0.0".to_string()));
        sparse.insert("1".to_string(), Value::Null);
        sparse.insert("20".to_string(), Value::String("my-tool".to_string()));
        sparse.insert("22".to_string(), Value::String("prompt-123".to_string()));

        let attrs = EventAttributes::from_sparse(&sparse);

        assert_eq!(attrs.git_ai_version, Some(Some("2.0.0".to_string())));
        assert_eq!(attrs.repo_url, Some(None)); // null
        assert_eq!(attrs.author, None); // not set
        assert_eq!(attrs.tool, Some(Some("my-tool".to_string())));
        assert_eq!(attrs.model, None); // not set
        assert_eq!(attrs.prompt_id, Some(Some("prompt-123".to_string())));
    }

    #[test]
    fn test_event_attributes_all_fields() {
        let attrs = EventAttributes::with_version("1.2.3")
            .repo_url("https://github.com/user/repo")
            .author("dev@example.com")
            .commit_sha("abc123")
            .base_commit_sha("def456")
            .branch("feature-branch")
            .tool("cursor")
            .model("gpt-4")
            .prompt_id("prompt-456")
            .external_prompt_id("ext-789");

        assert_eq!(attrs.git_ai_version, Some(Some("1.2.3".to_string())));
        assert_eq!(
            attrs.repo_url,
            Some(Some("https://github.com/user/repo".to_string()))
        );
        assert_eq!(attrs.author, Some(Some("dev@example.com".to_string())));
        assert_eq!(attrs.commit_sha, Some(Some("abc123".to_string())));
        assert_eq!(attrs.base_commit_sha, Some(Some("def456".to_string())));
        assert_eq!(attrs.branch, Some(Some("feature-branch".to_string())));
        assert_eq!(attrs.tool, Some(Some("cursor".to_string())));
        assert_eq!(attrs.model, Some(Some("gpt-4".to_string())));
        assert_eq!(attrs.prompt_id, Some(Some("prompt-456".to_string())));
        assert_eq!(attrs.external_prompt_id, Some(Some("ext-789".to_string())));
    }

    #[test]
    fn test_event_attributes_all_nulls() {
        let attrs = EventAttributes::new()
            .git_ai_version_null()
            .repo_url_null()
            .author_null()
            .commit_sha_null()
            .base_commit_sha_null()
            .branch_null()
            .tool_null()
            .model_null()
            .prompt_id_null()
            .external_prompt_id_null();

        assert_eq!(attrs.git_ai_version, Some(None));
        assert_eq!(attrs.repo_url, Some(None));
        assert_eq!(attrs.author, Some(None));
        assert_eq!(attrs.commit_sha, Some(None));
        assert_eq!(attrs.base_commit_sha, Some(None));
        assert_eq!(attrs.branch, Some(None));
        assert_eq!(attrs.tool, Some(None));
        assert_eq!(attrs.model, Some(None));
        assert_eq!(attrs.prompt_id, Some(None));
        assert_eq!(attrs.external_prompt_id, Some(None));
    }

    #[test]
    fn test_event_attributes_to_sparse_all_fields() {
        let attrs = EventAttributes::with_version("1.0.0")
            .repo_url("https://github.com/test/repo")
            .author("author@test.com")
            .commit_sha("commit-sha")
            .base_commit_sha("base-sha")
            .branch("main")
            .tool("test-tool")
            .model("test-model")
            .prompt_id("prompt-id")
            .external_prompt_id("ext-id");

        let sparse = attrs.to_sparse();

        assert_eq!(sparse.get("0"), Some(&Value::String("1.0.0".to_string())));
        assert_eq!(
            sparse.get("1"),
            Some(&Value::String("https://github.com/test/repo".to_string()))
        );
        assert_eq!(
            sparse.get("2"),
            Some(&Value::String("author@test.com".to_string()))
        );
        assert_eq!(
            sparse.get("3"),
            Some(&Value::String("commit-sha".to_string()))
        );
        assert_eq!(
            sparse.get("4"),
            Some(&Value::String("base-sha".to_string()))
        );
        assert_eq!(sparse.get("5"), Some(&Value::String("main".to_string())));
        assert_eq!(
            sparse.get("20"),
            Some(&Value::String("test-tool".to_string()))
        );
        assert_eq!(
            sparse.get("21"),
            Some(&Value::String("test-model".to_string()))
        );
        assert_eq!(
            sparse.get("22"),
            Some(&Value::String("prompt-id".to_string()))
        );
        assert_eq!(sparse.get("23"), Some(&Value::String("ext-id".to_string())));
    }

    #[test]
    fn test_event_attributes_roundtrip() {
        let original = EventAttributes::with_version("2.5.0")
            .repo_url("https://gitlab.com/org/repo")
            .author_null()
            .commit_sha("sha123")
            .tool("copilot");

        let sparse = original.to_sparse();
        let restored = EventAttributes::from_sparse(&sparse);

        assert_eq!(restored.git_ai_version, Some(Some("2.5.0".to_string())));
        assert_eq!(
            restored.repo_url,
            Some(Some("https://gitlab.com/org/repo".to_string()))
        );
        assert_eq!(restored.author, Some(None)); // explicitly null
        assert_eq!(restored.commit_sha, Some(Some("sha123".to_string())));
        assert_eq!(restored.tool, Some(Some("copilot".to_string())));
        assert_eq!(restored.base_commit_sha, None); // not set
        assert_eq!(restored.model, None); // not set
    }

    #[test]
    fn test_event_attributes_partial_sparse() {
        let mut sparse = SparseArray::new();
        sparse.insert("0".to_string(), Value::String("3.0.0".to_string()));
        sparse.insert("20".to_string(), Value::String("windsurf".to_string()));

        let attrs = EventAttributes::from_sparse(&sparse);

        assert_eq!(attrs.git_ai_version, Some(Some("3.0.0".to_string())));
        assert_eq!(attrs.repo_url, None); // not set
        assert_eq!(attrs.author, None); // not set
        assert_eq!(attrs.tool, Some(Some("windsurf".to_string())));
        assert_eq!(attrs.branch, None); // not set
    }

    #[test]
    fn test_event_attributes_default() {
        let attrs = EventAttributes::default();

        assert_eq!(attrs.git_ai_version, None);
        assert_eq!(attrs.repo_url, None);
        assert_eq!(attrs.author, None);
        assert_eq!(attrs.commit_sha, None);
        assert_eq!(attrs.base_commit_sha, None);
        assert_eq!(attrs.branch, None);
        assert_eq!(attrs.tool, None);
        assert_eq!(attrs.model, None);
        assert_eq!(attrs.prompt_id, None);
        assert_eq!(attrs.external_prompt_id, None);
    }

    #[test]
    fn test_event_attributes_git_ai_version_builder() {
        let attrs = EventAttributes::new().git_ai_version("4.0.0");
        assert_eq!(attrs.git_ai_version, Some(Some("4.0.0".to_string())));
    }

    #[test]
    fn test_event_attributes_sparse_positions() {
        // Verify the position constants match expected values
        use super::attr_pos::*;

        assert_eq!(GIT_AI_VERSION, 0);
        assert_eq!(REPO_URL, 1);
        assert_eq!(AUTHOR, 2);
        assert_eq!(COMMIT_SHA, 3);
        assert_eq!(BASE_COMMIT_SHA, 4);
        assert_eq!(BRANCH, 5);
        assert_eq!(TOOL, 20);
        assert_eq!(MODEL, 21);
        assert_eq!(PROMPT_ID, 22);
        assert_eq!(EXTERNAL_PROMPT_ID, 23);
    }
}
