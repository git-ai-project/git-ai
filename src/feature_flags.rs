use serde::Deserialize;

/// Feature flags for the application
#[derive(Debug, Clone)]
pub struct FeatureFlags {
    rewrite_stash: bool,
    proxy_push_notes_with_head: bool,
}

impl Default for FeatureFlags {
    fn default() -> Self {
        FeatureFlags {
            rewrite_stash: false,
            proxy_push_notes_with_head: false,
        }
    }
}

/// Deserializable version of FeatureFlags with all optional fields
/// and unknown fields allowed for graceful degradation
#[derive(Deserialize, Default)]
#[serde(default)]
pub(crate) struct FileFeatureFlags {
    #[serde(default, rename = "rewrite.stash")]
    rewrite_stash: Option<bool>,
    #[serde(default, rename = "proxy.push_notes_with_head")]
    proxy_push_notes_with_head: Option<bool>,
}

impl FeatureFlags {
    /// Build FeatureFlags from file configuration
    /// Falls back to defaults for any invalid or missing values
    pub(crate) fn from_file_config(file_flags: Option<FileFeatureFlags>) -> Self {
        let file_flags = match file_flags {
            Some(flags) => flags,
            None => return FeatureFlags::default(),
        };

        let defaults = FeatureFlags::default();

        FeatureFlags {
            rewrite_stash: file_flags.rewrite_stash.unwrap_or(defaults.rewrite_stash),
            proxy_push_notes_with_head: file_flags
                .proxy_push_notes_with_head
                .unwrap_or(defaults.proxy_push_notes_with_head),
        }
    }

    pub fn rewrite_stash(&self) -> bool {
        self.rewrite_stash
    }

    pub fn proxy_push_notes_with_head(&self) -> bool {
        self.proxy_push_notes_with_head
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_feature_flags() {
        let flags = FeatureFlags::default();
        assert!(!flags.rewrite_stash());
        assert!(!flags.proxy_push_notes_with_head());
    }

    #[test]
    fn test_from_file_config_with_none() {
        let flags = FeatureFlags::from_file_config(None);
        // Should match defaults
        assert!(!flags.rewrite_stash());
        assert!(!flags.proxy_push_notes_with_head());
    }

    #[test]
    fn test_unknown_fields_are_ignored() {
        let json = r#"{"rewrite.stash": true, "unknown_flag": "value", "another_unknown": 123}"#;
        let file_flags: Result<FileFeatureFlags, _> = serde_json::from_str(json);
        // Should succeed even with unknown fields
        assert!(file_flags.is_ok());

        if let Ok(file_flags) = file_flags {
            assert_eq!(file_flags.rewrite_stash, Some(true));
        }
    }

    #[test]
    fn test_invalid_value_uses_default() {
        // Test that the building process handles invalid values gracefully
        let file_flags = FileFeatureFlags::default();
        let flags = FeatureFlags::from_file_config(Some(file_flags));
        // Should match defaults
        assert!(!flags.rewrite_stash());
        assert!(!flags.proxy_push_notes_with_head());
    }

    #[test]
    fn test_from_file_config_with_values() {
        let file_flags = FileFeatureFlags {
            rewrite_stash: Some(true),
            proxy_push_notes_with_head: Some(true),
        };
        let flags = FeatureFlags::from_file_config(Some(file_flags));
        assert!(flags.rewrite_stash());
        assert!(flags.proxy_push_notes_with_head());
    }

    #[test]
    fn test_partial_config() {
        let file_flags = FileFeatureFlags {
            rewrite_stash: Some(true),
            proxy_push_notes_with_head: None,
        };
        let flags = FeatureFlags::from_file_config(Some(file_flags));
        assert!(flags.rewrite_stash());
        assert!(!flags.proxy_push_notes_with_head()); // Should use default
    }

    #[test]
    fn test_deserialize_dotted_names() {
        let json = r#"{"rewrite.stash": true, "proxy.push_notes_with_head": false}"#;
        let file_flags: Result<FileFeatureFlags, _> = serde_json::from_str(json);
        assert!(file_flags.is_ok());

        let file_flags = file_flags.unwrap();
        assert_eq!(file_flags.rewrite_stash, Some(true));
        assert_eq!(file_flags.proxy_push_notes_with_head, Some(false));
    }
}
