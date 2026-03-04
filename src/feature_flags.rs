use serde::{Deserialize, Serialize};

macro_rules! define_feature_flags {
    (
        $(
            $field:ident: $file_name:ident, debug = $debug_default:expr, release = $release_default:expr
        ),* $(,)?
    ) => {
        /// Feature flags for the application
        #[derive(Debug, Clone, Serialize)]
        pub struct FeatureFlags {
            $(pub $field: bool,)*
        }

        impl Default for FeatureFlags {
            fn default() -> Self {
                #[cfg(debug_assertions)]
                {
                    return FeatureFlags {
                        $($field: $debug_default,)*
                    };
                }
                #[cfg(not(debug_assertions))]
                FeatureFlags {
                    $($field: $release_default,)*
                }
            }
        }

        /// Deserializable version of FeatureFlags with all optional fields
        /// Works for both file config and environment variables
        #[derive(Deserialize, Default)]
        #[serde(default)]
        pub(crate) struct DeserializableFeatureFlags {
            $(
                #[serde(default)]
                $file_name: Option<bool>,
            )*
        }

        impl FeatureFlags {
            /// Merge flags with a base, applying any Some values as overrides
            fn merge_with(base: Self, overrides: DeserializableFeatureFlags) -> Self {
                FeatureFlags {
                    $($field: overrides.$file_name.unwrap_or(base.$field),)*
                }
            }

            /// Read environment overrides for known feature-flag keys only.
            ///
            /// We intentionally avoid deserializing all `GIT_AI_*` env vars because
            /// non-feature env keys (for example `GIT_AI=git`) would otherwise cause
            /// parse failures and silently drop valid feature-flag overrides.
            fn env_overrides() -> DeserializableFeatureFlags {
                let mut env_flags = DeserializableFeatureFlags::default();
                $(
                    {
                        let key = format!(
                            "GIT_AI_{}",
                            stringify!($file_name).to_ascii_uppercase()
                        );
                        env_flags.$file_name = parse_env_bool_var(&key);
                    }
                )*
                env_flags
            }
        }
    };
}

// Define all feature flags in one place
// Format: struct_field: file_and_env_name, debug = <bool>, release = <bool>
define_feature_flags!(
    rewrite_stash: rewrite_stash, debug = true, release = false,
    inter_commit_move: checkpoint_inter_commit_move, debug = false, release = false,
    auth_keyring: auth_keyring, debug = false, release = false,
    async_rewrite_hooks: async_rewrite_hooks, debug = false, release = false,
);

impl FeatureFlags {
    /// Build FeatureFlags from deserializable config
    #[allow(dead_code)]
    fn from_deserializable(flags: DeserializableFeatureFlags) -> Self {
        Self::merge_with(FeatureFlags::default(), flags)
    }

    /// Build FeatureFlags from file configuration
    /// Falls back to defaults for any invalid or missing values
    #[allow(dead_code)]
    pub(crate) fn from_file_config(file_flags: Option<DeserializableFeatureFlags>) -> Self {
        match file_flags {
            Some(flags) => Self::from_deserializable(flags),
            None => FeatureFlags::default(),
        }
    }

    /// Build FeatureFlags from environment variables
    /// Reads from GIT_AI_* prefixed environment variables
    /// Example: GIT_AI_REWRITE_STASH=true, GIT_AI_CHECKPOINT_INTER_COMMIT_MOVE=false
    /// Falls back to defaults for any invalid or missing values
    #[allow(dead_code)]
    pub fn from_env() -> Self {
        let env_flags: DeserializableFeatureFlags = Self::env_overrides();
        Self::from_deserializable(env_flags)
    }

    /// Build FeatureFlags from both file and environment variables
    /// Precedence: Environment > File > Default
    /// - Starts with defaults
    /// - Applies file config overrides if present
    /// - Applies environment variable overrides if present (highest priority)
    pub(crate) fn from_env_and_file(file_flags: Option<DeserializableFeatureFlags>) -> Self {
        // Start with defaults
        let mut result = FeatureFlags::default();

        // Apply file config overrides
        if let Some(file) = file_flags {
            result = Self::merge_with(result, file);
        }

        // Apply env var overrides (highest priority)
        let env_flags: DeserializableFeatureFlags = Self::env_overrides();
        result = Self::merge_with(result, env_flags);

        result
    }
}

fn parse_env_bool_var(key: &str) -> Option<bool> {
    let raw = std::env::var(key).ok()?;
    let normalized = raw.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_feature_flags() {
        let flags = FeatureFlags::default();
        // Test that defaults are set correctly based on debug/release mode
        #[cfg(debug_assertions)]
        {
            assert!(flags.rewrite_stash);
            assert!(!flags.inter_commit_move);
            assert!(!flags.auth_keyring);
            assert!(!flags.async_rewrite_hooks);
        }
        #[cfg(not(debug_assertions))]
        {
            assert!(!flags.rewrite_stash);
            assert!(!flags.inter_commit_move);
            assert!(!flags.auth_keyring);
            assert!(!flags.async_rewrite_hooks);
        }
    }

    #[test]
    fn test_from_file_config_none() {
        let flags = FeatureFlags::from_file_config(None);
        // Should return defaults
        let defaults = FeatureFlags::default();
        assert_eq!(flags.rewrite_stash, defaults.rewrite_stash);
        assert_eq!(flags.inter_commit_move, defaults.inter_commit_move);
        assert_eq!(flags.auth_keyring, defaults.auth_keyring);
    }

    #[test]
    fn test_from_file_config_some() {
        let mut deserializable = DeserializableFeatureFlags::default();
        deserializable.rewrite_stash = Some(false);
        deserializable.checkpoint_inter_commit_move = Some(true);
        deserializable.auth_keyring = Some(true);
        deserializable.async_rewrite_hooks = Some(true);

        let flags = FeatureFlags::from_file_config(Some(deserializable));
        assert!(!flags.rewrite_stash);
        assert!(flags.inter_commit_move);
        assert!(flags.auth_keyring);
        assert!(flags.async_rewrite_hooks);
    }

    #[test]
    fn test_from_file_config_partial() {
        let mut deserializable = DeserializableFeatureFlags::default();
        deserializable.rewrite_stash = Some(true);
        // Other fields remain None, should use defaults

        let flags = FeatureFlags::from_file_config(Some(deserializable));
        assert!(flags.rewrite_stash);

        let defaults = FeatureFlags::default();
        assert_eq!(flags.inter_commit_move, defaults.inter_commit_move);
        assert_eq!(flags.auth_keyring, defaults.auth_keyring);
        assert_eq!(flags.async_rewrite_hooks, defaults.async_rewrite_hooks);
    }

    #[test]
    fn test_from_deserializable() {
        let mut deserializable = DeserializableFeatureFlags::default();
        deserializable.rewrite_stash = Some(false);
        deserializable.checkpoint_inter_commit_move = Some(false);
        deserializable.auth_keyring = Some(true);
        deserializable.async_rewrite_hooks = Some(true);

        let flags = FeatureFlags::from_deserializable(deserializable);
        assert!(!flags.rewrite_stash);
        assert!(!flags.inter_commit_move);
        assert!(flags.auth_keyring);
        assert!(flags.async_rewrite_hooks);
    }

    #[test]
    #[serial_test::serial]
    fn test_from_env_and_file_defaults_only() {
        // No file flags, env should be empty
        unsafe {
            std::env::remove_var("GIT_AI_REWRITE_STASH");
            std::env::remove_var("GIT_AI_CHECKPOINT_INTER_COMMIT_MOVE");
            std::env::remove_var("GIT_AI_AUTH_KEYRING");
            std::env::remove_var("GIT_AI_ASYNC_REWRITE_HOOKS");
        }

        let flags = FeatureFlags::from_env_and_file(None);
        let defaults = FeatureFlags::default();
        assert_eq!(flags.rewrite_stash, defaults.rewrite_stash);
        assert_eq!(flags.inter_commit_move, defaults.inter_commit_move);
        assert_eq!(flags.auth_keyring, defaults.auth_keyring);
        assert_eq!(flags.async_rewrite_hooks, defaults.async_rewrite_hooks);
    }

    #[test]
    #[serial_test::serial]
    fn test_from_env_and_file_file_overrides() {
        unsafe {
            std::env::remove_var("GIT_AI_REWRITE_STASH");
            std::env::remove_var("GIT_AI_CHECKPOINT_INTER_COMMIT_MOVE");
            std::env::remove_var("GIT_AI_AUTH_KEYRING");
            std::env::remove_var("GIT_AI_ASYNC_REWRITE_HOOKS");
        }

        let mut file_flags = DeserializableFeatureFlags::default();
        file_flags.rewrite_stash = Some(true);
        file_flags.auth_keyring = Some(true);
        file_flags.async_rewrite_hooks = Some(true);

        let flags = FeatureFlags::from_env_and_file(Some(file_flags));
        assert!(flags.rewrite_stash);
        assert!(flags.auth_keyring);
        assert!(flags.async_rewrite_hooks);
    }

    #[test]
    #[serial_test::serial]
    fn test_from_env_ignores_non_feature_git_ai_env_vars() {
        unsafe {
            std::env::set_var("GIT_AI", "git");
            std::env::set_var("GIT_AI_ASYNC_REWRITE_HOOKS", "true");
        }

        let flags = FeatureFlags::from_env_and_file(None);
        assert!(flags.async_rewrite_hooks);

        unsafe {
            std::env::remove_var("GIT_AI");
            std::env::remove_var("GIT_AI_ASYNC_REWRITE_HOOKS");
        }
    }

    #[test]
    fn test_serialization() {
        let flags = FeatureFlags {
            rewrite_stash: true,
            inter_commit_move: false,
            auth_keyring: true,
            async_rewrite_hooks: false,
        };

        let serialized = serde_json::to_string(&flags).unwrap();
        assert!(serialized.contains("rewrite_stash"));
        assert!(serialized.contains("inter_commit_move"));
        assert!(serialized.contains("auth_keyring"));
        assert!(serialized.contains("async_rewrite_hooks"));
    }

    #[test]
    fn test_clone_trait() {
        let flags = FeatureFlags {
            rewrite_stash: true,
            inter_commit_move: false,
            auth_keyring: true,
            async_rewrite_hooks: false,
        };
        let cloned = flags.clone();
        assert_eq!(cloned.rewrite_stash, flags.rewrite_stash);
        assert_eq!(cloned.inter_commit_move, flags.inter_commit_move);
        assert_eq!(cloned.auth_keyring, flags.auth_keyring);
        assert_eq!(cloned.async_rewrite_hooks, flags.async_rewrite_hooks);
    }

    #[test]
    fn test_debug_trait() {
        let flags = FeatureFlags::default();
        let debug_str = format!("{:?}", flags);
        assert!(debug_str.contains("FeatureFlags"));
    }
}
