use regex::Regex;
use std::fs;
use std::path::{Path, PathBuf};

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

#[test]
fn internal_background_subcommands_must_use_spawn_helper() {
    let src_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    collect_rs_files(&src_root, &mut files);

    let disallowed_patterns = [
        Regex::new(r#"Command::new\([^\)]*\)(?s:.*?)\.arg\("flush-cas"\)"#).unwrap(),
        Regex::new(r#"Command::new\([^\)]*\)(?s:.*?)\.arg\("flush-logs"\)"#).unwrap(),
        Regex::new(r#"Command::new\([^\)]*\)(?s:.*?)\.arg\("flush-metrics-db"\)"#).unwrap(),
        Regex::new(
            r#"Command::new\([^\)]*\)(?s:.*?)\.arg\("upgrade"\)(?s:.*?)\.arg\("--background"\)"#,
        )
        .unwrap(),
    ];

    for file in files {
        // Utility layer is allowed to own the centralized spawn implementation.
        if file.ends_with("src/utils.rs") {
            continue;
        }

        let Ok(content) = fs::read_to_string(&file) else {
            continue;
        };
        for pattern in &disallowed_patterns {
            assert!(
                !pattern.is_match(&content),
                "direct internal background spawn found in {}: must use spawn_internal_git_ai_subcommand()",
                file.display()
            );
        }
    }
}

#[test]
fn critical_background_spawners_call_spawn_helper() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let files = [
        root.join("src/commands/flush_cas.rs"),
        root.join("src/observability/mod.rs"),
        root.join("src/commands/flush_metrics_db.rs"),
        root.join("src/commands/upgrade.rs"),
    ];

    for file in files {
        let content = fs::read_to_string(&file).unwrap();
        assert!(
            content.contains("spawn_internal_git_ai_subcommand("),
            "{} must call spawn_internal_git_ai_subcommand()",
            file.display()
        );
    }
}

#[test]
fn internal_spawn_helper_calls_must_provide_guard_env() {
    let src_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    collect_rs_files(&src_root, &mut files);

    let disallowed = Regex::new(
        r#"spawn_internal_git_ai_subcommand\(\s*"[^"]+"\s*,\s*&\[[^\]]*\]\s*,\s*None\s*,"#,
    )
    .unwrap();

    for file in files {
        let Ok(content) = fs::read_to_string(&file) else {
            continue;
        };
        assert!(
            !disallowed.is_match(&content),
            "guardless spawn_internal_git_ai_subcommand call found in {}",
            file.display()
        );
    }
}

#[test]
fn direct_git_command_spawns_are_centralized() {
    let src_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    collect_rs_files(&src_root, &mut files);

    let allowed_suffixes = [
        "src/git/repository.rs",
        "src/commands/git_handlers.rs",
        "src/git/test_utils/mod.rs",
    ];
    let pattern = Regex::new(r#"Command::new\(config::Config::get\(\)\.git_cmd\(\)\)"#).unwrap();

    for file in files {
        let file_str = file.to_string_lossy();
        if allowed_suffixes
            .iter()
            .any(|suffix| file_str.ends_with(suffix))
        {
            continue;
        }

        let Ok(content) = fs::read_to_string(&file) else {
            continue;
        };
        assert!(
            !pattern.is_match(&content),
            "direct git command spawn found in {}: route through centralized repository exec helpers",
            file.display()
        );
    }
}
