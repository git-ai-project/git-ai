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
            pub(crate) fn merge_with(base: Self, overrides: DeserializableFeatureFlags) -> Self {
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
    async_mode: async_mode, debug = true, release = true,
    git_hooks_enabled: git_hooks_enabled, debug = false, release = false,
    git_hooks_externally_managed: git_hooks_externally_managed, debug = false, release = false,
);

/// Returns true when running under a non-daemon test mode
/// (i.e. GIT_AI_TEST_GIT_MODE is set to "wrapper", "hooks", "both", or any value
/// that does NOT imply daemon usage).  In this case async_mode should be off by
/// default so that pure-wrapper tests do not accidentally try to reach a daemon.
///
/// File config and the GIT_AI_ASYNC_MODE env var can still override this baseline.
fn is_non_daemon_test_mode() -> bool {
    if let Ok(mode) = std::env::var("GIT_AI_TEST_GIT_MODE") {
        // Daemon modes: daemon, trace-daemon, pure-daemon, wrapper-daemon
        // Everything else (wrapper, hooks, both, unknown) → non-daemon
        !matches!(
            mode.to_lowercase().as_str(),
            "daemon" | "trace-daemon" | "pure-daemon" | "wrapper-daemon"
        )
    } else {
        false // env var not set → production binary, no override
    }
}

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
    /// Precedence: Environment > File > Test-mode baseline > Default
    /// - Starts with defaults
    /// - In non-daemon test mode (GIT_AI_TEST_GIT_MODE=wrapper|hooks|both),
    ///   async_mode is forced off so wrapper tests don't accidentally reach a daemon
    /// - Applies file config overrides if present
    /// - Applies environment variable overrides if present (highest priority)
    pub(crate) fn from_env_and_file(file_flags: Option<DeserializableFeatureFlags>) -> Self {
        // Start with defaults
        let mut result = FeatureFlags::default();

        // In non-daemon test modes disable async_mode at the baseline level so
        // that plain wrapper tests don't try to delegate to a daemon that isn't
        // running.  File config and GIT_AI_ASYNC_MODE env var can still override.
        if is_non_daemon_test_mode() {
            result.async_mode = false;
        }

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
            assert!(flags.async_mode); // async_mode defaults to true in all builds
            assert!(!flags.git_hooks_enabled);
            assert!(!flags.git_hooks_externally_managed);
        }
        #[cfg(not(debug_assertions))]
        {
            assert!(!flags.rewrite_stash);
            assert!(!flags.inter_commit_move);
            assert!(!flags.auth_keyring);
            assert!(flags.async_mode); // async_mode defaults to true in all builds
            assert!(!flags.git_hooks_enabled);
            assert!(!flags.git_hooks_externally_managed);
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
        let deserializable = DeserializableFeatureFlags {
            rewrite_stash: Some(false),
            checkpoint_inter_commit_move: Some(true),
            auth_keyring: Some(true),
            ..Default::default()
        };

        let flags = FeatureFlags::from_file_config(Some(deserializable));
        assert!(!flags.rewrite_stash);
        assert!(flags.inter_commit_move);
        assert!(flags.auth_keyring);
    }

    #[test]
    fn test_from_file_config_partial() {
        let deserializable = DeserializableFeatureFlags {
            rewrite_stash: Some(true),
            ..Default::default()
        };
        // Other fields remain None, should use defaults

        let flags = FeatureFlags::from_file_config(Some(deserializable));
        assert!(flags.rewrite_stash);

        let defaults = FeatureFlags::default();
        assert_eq!(flags.inter_commit_move, defaults.inter_commit_move);
        assert_eq!(flags.auth_keyring, defaults.auth_keyring);
    }

    #[test]
    fn test_from_deserializable() {
        let deserializable = DeserializableFeatureFlags {
            rewrite_stash: Some(false),
            checkpoint_inter_commit_move: Some(false),
            auth_keyring: Some(true),
            ..Default::default()
        };

        let flags = FeatureFlags::from_deserializable(deserializable);
        assert!(!flags.rewrite_stash);
        assert!(!flags.inter_commit_move);
        assert!(flags.auth_keyring);
    }

    #[test]
    #[serial_test::serial]
    fn test_from_env_and_file_defaults_only() {
        // No file flags, env should be empty.
        // Remove GIT_AI_TEST_GIT_MODE so we get the raw compile-time defaults.
        unsafe {
            std::env::remove_var("GIT_AI_REWRITE_STASH");
            std::env::remove_var("GIT_AI_CHECKPOINT_INTER_COMMIT_MOVE");
            std::env::remove_var("GIT_AI_AUTH_KEYRING");
            std::env::remove_var("GIT_AI_ASYNC_MODE");
            std::env::remove_var("GIT_AI_TEST_GIT_MODE");
        }

        let flags = FeatureFlags::from_env_and_file(None);
        let defaults = FeatureFlags::default();
        assert_eq!(flags.rewrite_stash, defaults.rewrite_stash);
        assert_eq!(flags.inter_commit_move, defaults.inter_commit_move);
        assert_eq!(flags.auth_keyring, defaults.auth_keyring);
        // async_mode defaults to true when no test mode is active
        assert!(flags.async_mode);
    }

    #[test]
    #[serial_test::serial]
    fn test_from_env_and_file_non_daemon_test_mode_disables_async_mode() {
        // When GIT_AI_TEST_GIT_MODE is a non-daemon mode (wrapper, hooks, both),
        // async_mode should be forced off at the baseline level.
        unsafe {
            std::env::remove_var("GIT_AI_ASYNC_MODE");
            std::env::set_var("GIT_AI_TEST_GIT_MODE", "wrapper");
        }
        let flags = FeatureFlags::from_env_and_file(None);
        assert!(
            !flags.async_mode,
            "async_mode should be false in wrapper test mode"
        );

        // Explicit file override can re-enable it
        let file_flags = DeserializableFeatureFlags {
            async_mode: Some(true),
            ..Default::default()
        };
        let flags_with_file = FeatureFlags::from_env_and_file(Some(file_flags));
        assert!(
            flags_with_file.async_mode,
            "file config should be able to override the test-mode baseline"
        );

        // Explicit env var override can also re-enable it
        unsafe {
            std::env::set_var("GIT_AI_ASYNC_MODE", "true");
        }
        let flags_with_env = FeatureFlags::from_env_and_file(None);
        assert!(
            flags_with_env.async_mode,
            "GIT_AI_ASYNC_MODE env var should override the test-mode baseline"
        );

        unsafe {
            std::env::remove_var("GIT_AI_ASYNC_MODE");
            std::env::remove_var("GIT_AI_TEST_GIT_MODE");
        }
    }

    #[test]
    #[serial_test::serial]
    fn test_from_env_and_file_daemon_test_mode_keeps_async_mode_default() {
        // Daemon modes (daemon, wrapper-daemon) should NOT override async_mode.
        unsafe {
            std::env::remove_var("GIT_AI_ASYNC_MODE");
        }
        for daemon_mode in &["daemon", "trace-daemon", "pure-daemon", "wrapper-daemon"] {
            unsafe {
                std::env::set_var("GIT_AI_TEST_GIT_MODE", daemon_mode);
            }
            let flags = FeatureFlags::from_env_and_file(None);
            assert!(
                flags.async_mode,
                "async_mode should remain true in daemon test mode '{}'",
                daemon_mode
            );
        }
        unsafe {
            std::env::remove_var("GIT_AI_TEST_GIT_MODE");
        }
    }

    #[test]
    #[serial_test::serial]
    fn test_from_env_and_file_file_overrides() {
        unsafe {
            std::env::remove_var("GIT_AI_REWRITE_STASH");
            std::env::remove_var("GIT_AI_CHECKPOINT_INTER_COMMIT_MOVE");
            std::env::remove_var("GIT_AI_AUTH_KEYRING");
            std::env::remove_var("GIT_AI_ASYNC_MODE");
            std::env::remove_var("GIT_AI_TEST_GIT_MODE");
        }

        let file_flags = DeserializableFeatureFlags {
            rewrite_stash: Some(true),
            auth_keyring: Some(true),
            async_mode: Some(true),
            ..Default::default()
        };

        let flags = FeatureFlags::from_env_and_file(Some(file_flags));
        assert!(flags.rewrite_stash);
        assert!(flags.auth_keyring);
        assert!(flags.async_mode);
    }

    #[test]
    fn test_serialization() {
        let flags = FeatureFlags {
            rewrite_stash: true,
            inter_commit_move: false,
            auth_keyring: true,
            async_mode: true,
            git_hooks_enabled: false,
            git_hooks_externally_managed: false,
        };

        let serialized = serde_json::to_string(&flags).unwrap();
        assert!(serialized.contains("rewrite_stash"));
        assert!(serialized.contains("inter_commit_move"));
        assert!(serialized.contains("auth_keyring"));
        assert!(serialized.contains("async_mode"));
        assert!(serialized.contains("git_hooks_enabled"));
        assert!(serialized.contains("git_hooks_externally_managed"));
    }

    #[test]
    fn test_clone_trait() {
        let flags = FeatureFlags {
            rewrite_stash: true,
            inter_commit_move: false,
            auth_keyring: true,
            async_mode: true,
            git_hooks_enabled: true,
            git_hooks_externally_managed: false,
        };
        let cloned = flags.clone();
        assert_eq!(cloned.rewrite_stash, flags.rewrite_stash);
        assert_eq!(cloned.inter_commit_move, flags.inter_commit_move);
        assert_eq!(cloned.auth_keyring, flags.auth_keyring);
        assert_eq!(cloned.async_mode, flags.async_mode);
        assert_eq!(cloned.git_hooks_enabled, flags.git_hooks_enabled);
        assert_eq!(
            cloned.git_hooks_externally_managed,
            flags.git_hooks_externally_managed
        );
    }

    #[test]
    fn test_debug_trait() {
        let flags = FeatureFlags::default();
        let debug_str = format!("{:?}", flags);
        assert!(debug_str.contains("FeatureFlags"));
    }
}
