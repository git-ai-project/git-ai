pub mod test_file;
pub mod test_repo;

#[macro_export]
macro_rules! subdir_test_variants {
    (
        fn $test_name:ident() $body:block
    ) => {
        paste::paste! {
            // Variant 1: Run from subdirectory (original behavior)
            #[test]
            fn [<test_ $test_name _from_subdir>]() $body

            // Variant 2: Run with -C flag from arbitrary directory
            #[test]
            fn [<test_ $test_name _with_c_flag>]() {
                // Wrapper struct that intercepts git calls to use -C flag
                struct TestRepoWithCFlag {
                    inner: $crate::repos::test_repo::TestRepo,
                }

                #[allow(dead_code)]
                impl TestRepoWithCFlag {
                    fn new() -> Self {
                        Self { inner: $crate::repos::test_repo::TestRepo::new() }
                    }

                    fn git_from_working_dir(
                        &self,
                        _working_dir: &std::path::Path,
                        args: &[&str],
                    ) -> Result<String, String> {
                        // Prepend -C <repo_root> to args and run from arbitrary directory
                        let arbitrary_dir = std::env::temp_dir();

                        let mut full_args = vec!["-C", self.inner.path().to_str().unwrap()];
                        full_args.extend(args);

                        use std::process::Command;
                        use $crate::repos::test_repo::get_binary_path;

                        let binary_path = get_binary_path();
                        let mode = std::env::var("GIT_AI_TEST_GIT_MODE")
                            .unwrap_or_else(|_| "wrapper".to_string())
                            .to_lowercase();
                        let uses_wrapper = mode != "hooks";
                        let uses_hooks = mode == "hooks"
                            || mode == "both"
                            || mode == "wrapper+hooks"
                            || mode == "hooks+wrapper";

                        let mut command = if uses_wrapper {
                            Command::new(binary_path)
                        } else {
                            Command::new("git")
                        };
                        command.current_dir(&arbitrary_dir);
                        command.args(&full_args);
                        if uses_wrapper {
                            command.env("GIT_AI", "git");
                        }
                        if uses_hooks {
                            command.env("HOME", self.inner.test_home_path());
                            command.env(
                                "GIT_CONFIG_GLOBAL",
                                self.inner.test_home_path().join(".gitconfig"),
                            );
                        }

                        // Add config patch if present
                        if let Some(patch) = &self.inner.config_patch {
                            if let Ok(patch_json) = serde_json::to_string(patch) {
                                command.env("GIT_AI_TEST_CONFIG_PATCH", patch_json);
                            }
                        }

                        // Add test database path for isolation
                        command.env("GIT_AI_TEST_DB_PATH", self.inner.test_db_path().to_str().unwrap());
                        command.env("GITAI_TEST_DB_PATH", self.inner.test_db_path().to_str().unwrap());

                        let output = command.output().expect(&format!(
                            "Failed to execute git command with -C flag: {:?}", args
                        ));

                        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

                        if output.status.success() {
                            Ok(if stdout.is_empty() { stderr } else { stdout })
                        } else {
                            Err(stderr)
                        }
                    }

                    fn git_with_env(
                        &self,
                        args: &[&str],
                        envs: &[(&str, &str)],
                        working_dir: Option<&std::path::Path>,
                    ) -> Result<String, String> {
                        if working_dir.is_some() {
                            // If working_dir is specified, prepend -C and run from arbitrary dir
                            let arbitrary_dir = std::env::temp_dir();

                            let mut full_args = vec!["-C", self.inner.path().to_str().unwrap()];
                            full_args.extend(args);

                            use std::process::Command;
                            use $crate::repos::test_repo::get_binary_path;

                            let binary_path = get_binary_path();
                            let mode = std::env::var("GIT_AI_TEST_GIT_MODE")
                                .unwrap_or_else(|_| "wrapper".to_string())
                                .to_lowercase();
                            let uses_wrapper = mode != "hooks";
                            let uses_hooks = mode == "hooks"
                                || mode == "both"
                                || mode == "wrapper+hooks"
                                || mode == "hooks+wrapper";

                            let mut command = if uses_wrapper {
                                Command::new(binary_path)
                            } else {
                                Command::new("git")
                            };
                            command.current_dir(&arbitrary_dir);
                            command.args(&full_args);
                            if uses_wrapper {
                                command.env("GIT_AI", "git");
                            }
                            if uses_hooks {
                                command.env("HOME", self.inner.test_home_path());
                                command.env(
                                    "GIT_CONFIG_GLOBAL",
                                    self.inner.test_home_path().join(".gitconfig"),
                                );
                            }

                            if let Some(patch) = &self.inner.config_patch {
                                if let Ok(patch_json) = serde_json::to_string(patch) {
                                    command.env("GIT_AI_TEST_CONFIG_PATCH", patch_json);
                                }
                            }

                            // Add test database path for isolation
                            command.env("GIT_AI_TEST_DB_PATH", self.inner.test_db_path().to_str().unwrap());
                            command.env("GITAI_TEST_DB_PATH", self.inner.test_db_path().to_str().unwrap());

                            // Apply custom env vars
                            for (key, value) in envs {
                                command.env(key, value);
                            }

                            let output = command.output().expect(&format!(
                                "Failed to execute git command with -C flag and env: {:?}", args
                            ));

                            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                            let stderr = String::from_utf8_lossy(&output.stderr).to_string();

                            if output.status.success() {
                                Ok(if stdout.is_empty() { stderr } else { stdout })
                            } else {
                                Err(stderr)
                            }
                        } else {
                            // No working_dir, use normal behavior
                            self.inner.git_with_env(args, envs, None)
                        }
                    }
                }

                // Forward all other methods via Deref
                impl std::ops::Deref for TestRepoWithCFlag {
                    type Target = $crate::repos::test_repo::TestRepo;
                    fn deref(&self) -> &Self::Target {
                        &self.inner
                    }
                }

                // Type alias to shadow TestRepo
                type TestRepo = TestRepoWithCFlag;
                $body
            }
        }
    };
}

#[macro_export]
macro_rules! worktree_test_wrappers {
    (
        fn $test_name:ident() $body:block
    ) => {
        paste::paste! {
            #[test]
            fn [<test_ $test_name _in_worktree_wrapper_mode>]() {
                struct WorktreeTestRepo {
                    inner: $crate::repos::test_repo::TestRepo,
                }

                #[allow(dead_code)]
                impl WorktreeTestRepo {
                    fn new() -> Self {
                        Self {
                            inner: $crate::repos::test_repo::TestRepo::new_worktree_with_mode(
                                $crate::repos::test_repo::GitTestMode::Wrapper,
                            ),
                        }
                    }

                    fn new_with_remote() -> (Self, Self) {
                        let (local, upstream) =
                            $crate::repos::test_repo::TestRepo::new_with_remote_with_mode(
                                $crate::repos::test_repo::GitTestMode::Wrapper,
                            );
                        (
                            Self { inner: local },
                            Self { inner: upstream },
                        )
                    }

                    fn git_mode() -> $crate::repos::test_repo::GitTestMode {
                        $crate::repos::test_repo::GitTestMode::Wrapper
                    }
                }

                impl std::ops::Deref for WorktreeTestRepo {
                    type Target = $crate::repos::test_repo::TestRepo;
                    fn deref(&self) -> &Self::Target {
                        &self.inner
                    }
                }

                type TestRepo = WorktreeTestRepo;
                $body
            }

            #[test]
            fn [<test_ $test_name _in_worktree_hooks_mode>]() {
                struct WorktreeTestRepo {
                    inner: $crate::repos::test_repo::TestRepo,
                }

                #[allow(dead_code)]
                impl WorktreeTestRepo {
                    fn new() -> Self {
                        Self {
                            inner: $crate::repos::test_repo::TestRepo::new_worktree_with_mode(
                                $crate::repos::test_repo::GitTestMode::Hooks,
                            ),
                        }
                    }

                    fn new_with_remote() -> (Self, Self) {
                        let (local, upstream) =
                            $crate::repos::test_repo::TestRepo::new_with_remote_with_mode(
                                $crate::repos::test_repo::GitTestMode::Hooks,
                            );
                        (
                            Self { inner: local },
                            Self { inner: upstream },
                        )
                    }

                    fn git_mode() -> $crate::repos::test_repo::GitTestMode {
                        $crate::repos::test_repo::GitTestMode::Hooks
                    }
                }

                impl std::ops::Deref for WorktreeTestRepo {
                    type Target = $crate::repos::test_repo::TestRepo;
                    fn deref(&self) -> &Self::Target {
                        &self.inner
                    }
                }

                type TestRepo = WorktreeTestRepo;
                $body
            }

            #[test]
            fn [<test_ $test_name _in_worktree_both_mode>]() {
                struct WorktreeTestRepo {
                    inner: $crate::repos::test_repo::TestRepo,
                }

                #[allow(dead_code)]
                impl WorktreeTestRepo {
                    fn new() -> Self {
                        Self {
                            inner: $crate::repos::test_repo::TestRepo::new_worktree_with_mode(
                                $crate::repos::test_repo::GitTestMode::Both,
                            ),
                        }
                    }

                    fn new_with_remote() -> (Self, Self) {
                        let (local, upstream) =
                            $crate::repos::test_repo::TestRepo::new_with_remote_with_mode(
                                $crate::repos::test_repo::GitTestMode::Both,
                            );
                        (
                            Self { inner: local },
                            Self { inner: upstream },
                        )
                    }

                    fn git_mode() -> $crate::repos::test_repo::GitTestMode {
                        $crate::repos::test_repo::GitTestMode::Both
                    }
                }

                impl std::ops::Deref for WorktreeTestRepo {
                    type Target = $crate::repos::test_repo::TestRepo;
                    fn deref(&self) -> &Self::Target {
                        &self.inner
                    }
                }

                type TestRepo = WorktreeTestRepo;
                $body
            }
        }
    };
}
