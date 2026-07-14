use crate::commands::install_hooks;
use crate::error::GitAiError;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PackageManager {
    Msi,
    Pkg,
}

impl PackageManager {
    fn parse(value: &str) -> Result<Self, GitAiError> {
        match value {
            "msi" => Ok(Self::Msi),
            "pkg" => Ok(Self::Pkg),
            _ => Err(GitAiError::Generic(format!(
                "unsupported package manager: {value}"
            ))),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Msi => "msi",
            Self::Pkg => "pkg",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PackageSetupOptions {
    manager: PackageManager,
    dry_run: bool,
    target_user: Option<String>,
    api_base: Option<String>,
    api_key: Option<String>,
}

pub fn run(args: &[String]) -> Result<HashMap<String, String>, GitAiError> {
    let options = parse_options(args)?;

    if should_skip_user_setup(&options) {
        print_manual_setup_message(options.manager);
        return Ok(HashMap::from([(
            "package_setup".to_string(),
            "skipped_system_context".to_string(),
        )]));
    }

    let mut install_args = vec!["--dry-run=false".to_string()];
    if options.dry_run {
        install_args[0] = "--dry-run=true".to_string();
    }
    install_hooks::run_with_package_config(&install_args, options.api_base, options.api_key)
}

fn parse_options(args: &[String]) -> Result<PackageSetupOptions, GitAiError> {
    let mut manager = None;
    let mut dry_run = false;
    let mut target_user = None;
    let mut api_base = None;
    let mut api_key = None;
    let mut iter = args.iter();

    while let Some(arg) = iter.next() {
        if let Some(value) = arg.strip_prefix("--manager=") {
            manager = Some(PackageManager::parse(value)?);
        } else if arg == "--manager" {
            let value = iter
                .next()
                .ok_or_else(|| GitAiError::Generic("missing value for --manager".to_string()))?;
            manager = Some(PackageManager::parse(value)?);
        } else if let Some(value) = arg.strip_prefix("--target-user=") {
            target_user = non_empty_value(value);
        } else if arg == "--target-user" {
            let value = iter.next().ok_or_else(|| {
                GitAiError::Generic("missing value for --target-user".to_string())
            })?;
            target_user = non_empty_value(value);
        } else if let Some(value) = arg.strip_prefix("--api-base=") {
            api_base = non_empty_value(value);
        } else if arg == "--api-base" {
            let value = iter
                .next()
                .ok_or_else(|| GitAiError::Generic("missing value for --api-base".to_string()))?;
            api_base = non_empty_value(value);
        } else if let Some(value) = arg.strip_prefix("--api-key=") {
            api_key = non_empty_value(value);
        } else if arg == "--api-key" {
            let value = iter
                .next()
                .ok_or_else(|| GitAiError::Generic("missing value for --api-key".to_string()))?;
            api_key = non_empty_value(value);
        } else if arg == "--dry-run" || arg == "--dry-run=true" {
            dry_run = true;
        } else if arg == "--dry-run=false" {
            dry_run = false;
        } else {
            return Err(GitAiError::Generic(format!(
                "unknown setup-package option: {arg}"
            )));
        }
    }

    let manager =
        manager.ok_or_else(|| GitAiError::Generic("missing required --manager".to_string()))?;
    Ok(PackageSetupOptions {
        manager,
        dry_run,
        target_user,
        api_base,
        api_key,
    })
}

fn non_empty_value(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn should_skip_user_setup(options: &PackageSetupOptions) -> bool {
    should_skip_user_setup_for_context(options.dry_run, crate::utils::is_running_as_superuser())
}

fn should_skip_user_setup_for_context(dry_run: bool, is_superuser: bool) -> bool {
    !dry_run && is_superuser
}

fn print_manual_setup_message(manager: PackageManager) {
    eprintln!(
        "Git AI was installed by {manager}. Run `git-ai install-hooks` as each developer user to enable trace2 integration and editor/agent setup.",
        manager = manager.as_str()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| arg.to_string()).collect()
    }

    #[test]
    fn parse_options_accepts_manager_equals() {
        let options = parse_options(&strings(&["--manager=msi"])).unwrap();
        assert_eq!(options.manager, PackageManager::Msi);
        assert!(!options.dry_run);
        assert_eq!(options.target_user, None);
    }

    #[test]
    fn parse_options_accepts_manager_value_and_dry_run() {
        let options = parse_options(&strings(&[
            "--manager",
            "pkg",
            "--target-user",
            "alice",
            "--dry-run",
        ]))
        .unwrap();
        assert_eq!(options.manager, PackageManager::Pkg);
        assert!(options.dry_run);
        assert_eq!(options.target_user.as_deref(), Some("alice"));
    }

    #[test]
    fn parse_options_accepts_msi_api_configuration() {
        let options = parse_options(&strings(&[
            "--manager=msi",
            "--api-base=https://enterprise.example",
            "--api-key",
            "sk-enterprise-key",
        ]))
        .unwrap();

        assert_eq!(
            options.api_base.as_deref(),
            Some("https://enterprise.example")
        );
        assert_eq!(options.api_key.as_deref(), Some("sk-enterprise-key"));
    }

    #[test]
    fn parse_options_rejects_missing_manager() {
        let err = parse_options(&strings(&["--dry-run"])).unwrap_err();
        assert!(err.to_string().contains("missing required --manager"));
    }

    #[test]
    fn parse_options_rejects_unknown_manager() {
        let err = parse_options(&strings(&["--manager", "rpm"])).unwrap_err();
        assert!(err.to_string().contains("unsupported package manager: rpm"));
    }

    #[test]
    fn parse_options_rejects_removed_package_managers() {
        for manager in ["apt", "brew"] {
            let err = parse_options(&strings(&["--manager", manager])).unwrap_err();
            assert!(
                err.to_string()
                    .contains(&format!("unsupported package manager: {manager}"))
            );
        }
    }

    #[test]
    fn elevated_processes_always_skip_per_user_setup() {
        assert!(should_skip_user_setup_for_context(false, true));
        assert!(!should_skip_user_setup_for_context(true, true));
        assert!(!should_skip_user_setup_for_context(false, false));
    }

    #[test]
    fn parse_options_rejects_unknown_option() {
        let err = parse_options(&strings(&["--manager", "msi", "--shim"])).unwrap_err();
        assert!(err.to_string().contains("unknown setup-package option"));
    }
}
