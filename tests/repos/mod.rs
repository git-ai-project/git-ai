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
                        let mut command = Command::new(binary_path);
                        command.current_dir(&arbitrary_dir);
                        command.args(&full_args);
                        command.env("GIT_AI", "git");

                        // Add config patch if present
                        if let Some(patch) = &self.inner.config_patch {
                            if let Ok(patch_json) = serde_json::to_string(patch) {
                                command.env("GIT_AI_TEST_CONFIG_PATCH", patch_json);
                            }
                        }

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
                            let mut command = Command::new(binary_path);
                            command.current_dir(&arbitrary_dir);
                            command.args(&full_args);
                            command.env("GIT_AI", "git");

                            if let Some(patch) = &self.inner.config_patch {
                                if let Ok(patch_json) = serde_json::to_string(patch) {
                                    command.env("GIT_AI_TEST_CONFIG_PATCH", patch_json);
                                }
                            }

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
