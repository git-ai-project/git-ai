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
        }
    };
}

// Define all feature flags in one place
// Format: struct_field: file_and_env_name, debug = <bool>, release = <bool>
define_feature_flags!(
    rewrite_stash: rewrite_stash, debug = true, release = false,
    inter_commit_move: checkpoint_inter_commit_move, debug = false, release = false,
    auth_keyring: auth_keyring, debug = false, release = false,
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
        let env_flags: DeserializableFeatureFlags =
            envy::prefixed("GIT_AI_").from_env().unwrap_or_default();
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
        let env_flags: DeserializableFeatureFlags =
            envy::prefixed("GIT_AI_").from_env().unwrap_or_default();
        result = Self::merge_with(result, env_flags);

        result
    }
}
