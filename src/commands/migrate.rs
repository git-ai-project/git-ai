use serde_json::Value;

use crate::commands::helpers::git_cmd;
use git_ai::core::authorship_log::{AUTHORSHIP_LOG_VERSION, AuthorshipLog};

// ---------------------------------------------------------------------------
// Migration framework
// ---------------------------------------------------------------------------

/// A single migration step that transforms a note from one schema version to another.
pub struct Migration {
    pub from_version: &'static str,
    pub to_version: &'static str,
    pub migrate_fn: fn(Value) -> Result<Value, String>,
}

/// Returns the ordered list of available migrations.
/// Each migration transforms from `from_version` to `to_version`.
/// Migrations are applied in chain order.
fn get_migrations() -> Vec<Migration> {
    vec![
        // Validation-only migration: 3.0.0 -> 3.0.0 (ensures well-formed)
        Migration {
            from_version: "authorship/3.0.0",
            to_version: "authorship/3.0.0",
            migrate_fn: migrate_3_0_0_validate,
        },
    ]
}

/// Validate that a 3.0.0 note is well-formed (no-op migration).
fn migrate_3_0_0_validate(value: Value) -> Result<Value, String> {
    // Ensure required fields exist
    let obj = value
        .as_object()
        .ok_or_else(|| "note metadata is not a JSON object".to_string())?;

    if !obj.contains_key("schema_version") {
        return Err("missing 'schema_version' field".to_string());
    }
    if !obj.contains_key("base_commit_sha") {
        return Err("missing 'base_commit_sha' field".to_string());
    }
    if !obj.contains_key("prompts") {
        return Err("missing 'prompts' field".to_string());
    }

    Ok(value)
}

/// Find migration chain from `from` version to `target` version.
/// Returns the sequence of migrations to apply, or None if no path exists.
pub fn find_migration_chain<'a>(
    migrations: &'a [Migration],
    from: &str,
    target: &str,
) -> Option<Vec<&'a Migration>> {
    if from == target {
        // Already at target; find the validation migration if any
        let validation = migrations
            .iter()
            .find(|m| m.from_version == from && m.to_version == target);
        return validation.map(|m| vec![m]);
    }

    // Build a chain from `from` to `target`
    let mut chain = Vec::new();
    let mut current = from;

    // Simple forward traversal (no cycles expected in version graph)
    for _ in 0..migrations.len() {
        if let Some(m) = migrations
            .iter()
            .find(|m| m.from_version == current && m.to_version != current)
        {
            chain.push(m);
            current = m.to_version;
            if current == target {
                return Some(chain);
            }
        } else {
            break;
        }
    }

    None
}

/// Apply a migration chain to a JSON metadata value.
pub fn apply_migration_chain(chain: &[&Migration], mut value: Value) -> Result<Value, String> {
    for migration in chain {
        value = (migration.migrate_fn)(value)?;
        // Update schema_version to reflect the migration target
        if let Some(obj) = value.as_object_mut() {
            obj.insert(
                "schema_version".to_string(),
                Value::String(migration.to_version.to_string()),
            );
        }
    }
    Ok(value)
}

// ---------------------------------------------------------------------------
// Schema version extraction
// ---------------------------------------------------------------------------

/// Extract the schema version from a raw note string.
/// Handles the text+JSON format: everything after `---` is JSON metadata.
pub fn extract_schema_version(note_content: &str) -> Option<String> {
    let lines: Vec<&str> = note_content.lines().collect();
    let divider = lines.iter().position(|&l| l == "---")?;
    let json_text: String = lines[divider + 1..].join("\n");
    let value: Value = serde_json::from_str(&json_text).ok()?;
    value
        .get("schema_version")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Extract the JSON metadata portion from a raw note string.
fn extract_metadata_json(note_content: &str) -> Option<(String, String)> {
    let lines: Vec<&str> = note_content.lines().collect();
    let divider = lines.iter().position(|&l| l == "---")?;
    let attestation_section: String = lines[..divider].join("\n");
    let json_text: String = lines[divider + 1..].join("\n");
    Some((attestation_section, json_text))
}

/// Reconstruct a note from its attestation section and metadata JSON.
fn reconstruct_note(attestation_section: &str, metadata_json: &str) -> String {
    let mut out = String::new();
    if !attestation_section.is_empty() {
        out.push_str(attestation_section);
        out.push('\n');
    }
    out.push_str("---\n");
    out.push_str(metadata_json);
    out
}

// ---------------------------------------------------------------------------
// Command handler
// ---------------------------------------------------------------------------

/// Handle the `git-ai migrate` command: upgrade note schemas in-place.
pub fn handle_migrate(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let mut dry_run = false;
    let mut from_version_filter: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--dry-run" => dry_run = true,
            "--from-version" => {
                i += 1;
                if i >= args.len() {
                    return Err("--from-version requires a value".into());
                }
                from_version_filter = Some(args[i].clone());
            }
            s if s.starts_with("--from-version=") => {
                let val = s.strip_prefix("--from-version=").unwrap();
                if val.is_empty() {
                    return Err("--from-version requires a value".into());
                }
                from_version_filter = Some(val.to_string());
            }
            "--help" | "-h" => {
                println!("usage: git-ai migrate [--dry-run] [--from-version <ver>]");
                println!();
                println!("Upgrade authorship note schemas in-place.");
                println!();
                println!("Options:");
                println!("  --dry-run              Report what would be migrated without writing");
                println!("  --from-version <ver>   Only migrate notes at this specific version");
                return Ok(());
            }
            other => {
                return Err(format!("unknown option '{}'", other).into());
            }
        }
        i += 1;
    }

    let migrations = get_migrations();
    let target_version = AUTHORSHIP_LOG_VERSION;

    // List all noted commits
    let noted_output = match git_cmd(&["notes", "--ref=ai", "list"]) {
        Ok(o) => o,
        Err(e) => {
            if e.contains("does not exist") || e.contains("not a valid ref") {
                println!("No authorship notes found.");
                return Ok(());
            }
            return Err(e.into());
        }
    };

    if noted_output.trim().is_empty() {
        println!("No authorship notes found.");
        return Ok(());
    }

    let commit_shas: Vec<String> = noted_output
        .lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 {
                Some(parts[1].to_string())
            } else {
                None
            }
        })
        .collect();

    let mut migrated = 0;
    let mut already_current = 0;
    let mut skipped = 0;
    let mut errors = 0;

    for sha in &commit_shas {
        let note_content = match git_cmd(&["notes", "--ref=ai", "show", sha]) {
            Ok(n) => n,
            Err(e) => {
                eprintln!("warning: could not read note for {}: {}", sha, e);
                errors += 1;
                continue;
            }
        };

        let version = match extract_schema_version(&note_content) {
            Some(v) => v,
            None => {
                eprintln!(
                    "warning: could not determine schema version for {}, skipping",
                    sha
                );
                skipped += 1;
                continue;
            }
        };

        // Apply from-version filter if specified
        if let Some(ref filter) = from_version_filter
            && &version != filter
        {
            skipped += 1;
            continue;
        }

        // Already at current version — run validation only
        if version == target_version {
            // Try to validate
            let (_attestation_section, json_text) = match extract_metadata_json(&note_content) {
                Some(parts) => parts,
                None => {
                    skipped += 1;
                    continue;
                }
            };

            let metadata_value: Value = match serde_json::from_str(&json_text) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("warning: malformed JSON in note for {}: {}", sha, e);
                    errors += 1;
                    continue;
                }
            };

            if let Some(chain) = find_migration_chain(&migrations, &version, target_version) {
                match apply_migration_chain(&chain, metadata_value) {
                    Ok(_) => {
                        already_current += 1;
                    }
                    Err(e) => {
                        eprintln!(
                            "warning: validation failed for {} ({}): {}",
                            sha, version, e
                        );
                        errors += 1;
                    }
                }
            } else {
                already_current += 1;
            }
            continue;
        }

        // Need migration
        let chain = match find_migration_chain(&migrations, &version, target_version) {
            Some(c) => c,
            None => {
                eprintln!(
                    "warning: no migration path from {} to {} for commit {}",
                    version, target_version, sha
                );
                skipped += 1;
                continue;
            }
        };

        let (attestation_section, json_text) = match extract_metadata_json(&note_content) {
            Some(parts) => parts,
            None => {
                eprintln!("warning: could not parse note structure for {}", sha);
                errors += 1;
                continue;
            }
        };

        let metadata_value: Value = match serde_json::from_str(&json_text) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("warning: malformed JSON in note for {}: {}", sha, e);
                errors += 1;
                continue;
            }
        };

        match apply_migration_chain(&chain, metadata_value) {
            Ok(migrated_value) => {
                if dry_run {
                    println!("would migrate: {} ({} -> {})", sha, version, target_version);
                    migrated += 1;
                } else {
                    let new_json =
                        serde_json::to_string_pretty(&migrated_value).map_err(|e| e.to_string())?;
                    let new_note = reconstruct_note(&attestation_section, &new_json);

                    match git_cmd(&["notes", "--ref=ai", "add", "-f", "-m", &new_note, sha]) {
                        Ok(_) => {
                            migrated += 1;
                        }
                        Err(e) => {
                            eprintln!("error: failed to write migrated note for {}: {}", sha, e);
                            errors += 1;
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!("error: migration failed for {} ({}): {}", sha, version, e);
                errors += 1;
            }
        }
    }

    // Summary
    if dry_run {
        if migrated > 0 {
            println!(
                "Would migrate {} notes to {} ({} already current)",
                migrated, target_version, already_current
            );
        } else {
            println!(
                "All {} notes already at {} (nothing to migrate)",
                already_current, target_version
            );
        }
    } else if migrated > 0 {
        println!(
            "Migrated {} notes to {} ({} already current)",
            migrated, target_version, already_current
        );
    } else {
        println!(
            "All {} notes already at {} (nothing to migrate)",
            already_current, target_version
        );
    }

    if errors > 0 {
        eprintln!("{} notes had errors (see warnings above)", errors);
    }
    if skipped > 0 && from_version_filter.is_some() {
        println!(
            "{} notes skipped (did not match --from-version filter)",
            skipped
        );
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Backward-compatible note reader
// ---------------------------------------------------------------------------

/// Attempt to parse an authorship note with backward compatibility.
/// If the note has an unrecognized schema version, it attempts best-effort parsing.
/// Returns None (with a warning) only if the note is completely unparseable.
#[allow(dead_code)]
pub fn parse_note_with_compat(note_content: &str) -> Option<AuthorshipLog> {
    // First try normal deserialization
    if let Ok(log) = AuthorshipLog::deserialize_from_string(note_content) {
        return Some(log);
    }

    // If normal parsing failed, try to at least extract the version and warn
    let version = extract_schema_version(note_content);
    match version {
        Some(v) if v != AUTHORSHIP_LOG_VERSION => {
            eprintln!(
                "[git-ai] warning: note has unrecognized schema version '{}', attempting best-effort parse",
                v
            );
            // Try parsing anyway — serde's default handling may fill in missing fields
            match AuthorshipLog::deserialize_from_string(note_content) {
                Ok(log) => Some(log),
                Err(e) => {
                    eprintln!(
                        "[git-ai] warning: could not parse note with schema '{}': {}",
                        v, e
                    );
                    None
                }
            }
        }
        Some(_) => {
            // Current version but still failed — truly broken note
            eprintln!("[git-ai] warning: failed to parse authorship note (current schema)");
            None
        }
        None => {
            eprintln!("[git-ai] warning: note has no recognizable schema version, skipping");
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_extract_schema_version() {
        let note = "src/main.rs\n  abcdef1234567890 1-5\n---\n{\"schema_version\":\"authorship/3.0.0\",\"base_commit_sha\":\"abc\",\"prompts\":{}}";
        assert_eq!(
            extract_schema_version(note),
            Some("authorship/3.0.0".to_string())
        );
    }

    #[test]
    fn test_extract_schema_version_unknown() {
        let note = "---\n{\"schema_version\":\"authorship/2.0.0\",\"base_commit_sha\":\"abc\",\"prompts\":{}}";
        assert_eq!(
            extract_schema_version(note),
            Some("authorship/2.0.0".to_string())
        );
    }

    #[test]
    fn test_extract_schema_version_missing() {
        let note = "---\n{\"base_commit_sha\":\"abc\"}";
        assert_eq!(extract_schema_version(note), None);
    }

    #[test]
    fn test_extract_schema_version_no_divider() {
        let note = "just some text without divider";
        assert_eq!(extract_schema_version(note), None);
    }

    #[test]
    fn test_migration_chain_same_version() {
        let migrations = get_migrations();
        let chain = find_migration_chain(&migrations, "authorship/3.0.0", "authorship/3.0.0");
        assert!(chain.is_some());
        assert_eq!(chain.unwrap().len(), 1);
    }

    #[test]
    fn test_migration_chain_no_path() {
        let migrations = get_migrations();
        let chain = find_migration_chain(&migrations, "authorship/1.0.0", "authorship/3.0.0");
        assert!(chain.is_none());
    }

    #[test]
    fn test_validate_migration_well_formed() {
        let metadata = json!({
            "schema_version": "authorship/3.0.0",
            "base_commit_sha": "abc123",
            "prompts": {},
            "sessions": {},
            "humans": {}
        });

        let result = migrate_3_0_0_validate(metadata.clone());
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), metadata);
    }

    #[test]
    fn test_validate_migration_missing_fields() {
        let metadata = json!({
            "schema_version": "authorship/3.0.0"
        });

        let result = migrate_3_0_0_validate(metadata);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("missing 'base_commit_sha'"));
    }

    #[test]
    fn test_validate_migration_not_object() {
        let metadata = json!("just a string");
        let result = migrate_3_0_0_validate(metadata);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not a JSON object"));
    }

    #[test]
    fn test_apply_migration_chain() {
        let migrations = get_migrations();
        let chain =
            find_migration_chain(&migrations, "authorship/3.0.0", "authorship/3.0.0").unwrap();

        let metadata = json!({
            "schema_version": "authorship/3.0.0",
            "base_commit_sha": "abc123",
            "prompts": {}
        });

        let result = apply_migration_chain(&chain, metadata.clone());
        assert!(result.is_ok());
        let migrated = result.unwrap();
        assert_eq!(
            migrated.get("schema_version").unwrap().as_str().unwrap(),
            "authorship/3.0.0"
        );
    }

    #[test]
    fn test_apply_migration_chain_idempotent() {
        let migrations = get_migrations();
        let chain =
            find_migration_chain(&migrations, "authorship/3.0.0", "authorship/3.0.0").unwrap();

        let metadata = json!({
            "schema_version": "authorship/3.0.0",
            "base_commit_sha": "def456",
            "prompts": {
                "abc123": {
                    "agent_id": {"tool": "cursor", "id": "sess1", "model": "claude"},
                    "total_additions": 5,
                    "total_deletions": 0,
                    "accepted_lines": 5,
                    "overriden_lines": 0
                }
            }
        });

        // Apply twice
        let first = apply_migration_chain(&chain, metadata.clone()).unwrap();
        let second = apply_migration_chain(&chain, first.clone()).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn test_reconstruct_note() {
        let attestation = "src/main.rs\n  abcdef1234567890 1-5";
        let metadata =
            "{\"schema_version\":\"authorship/3.0.0\",\"base_commit_sha\":\"abc\",\"prompts\":{}}";

        let note = reconstruct_note(attestation, metadata);
        assert!(note.starts_with("src/main.rs\n"));
        assert!(note.contains("---\n"));
        assert!(note.ends_with(metadata));
    }

    #[test]
    fn test_reconstruct_note_empty_attestations() {
        let attestation = "";
        let metadata = "{\"schema_version\":\"authorship/3.0.0\"}";

        let note = reconstruct_note(attestation, metadata);
        assert_eq!(note, "---\n{\"schema_version\":\"authorship/3.0.0\"}");
    }

    #[test]
    fn test_parse_note_with_compat_valid() {
        let note = "src/main.rs\n  abcdef1234567890 1,2,3-5\n---\n{\"schema_version\":\"authorship/3.0.0\",\"base_commit_sha\":\"abc123\",\"prompts\":{}}";
        let result = parse_note_with_compat(note);
        assert!(result.is_some());
        let log = result.unwrap();
        assert_eq!(log.metadata.schema_version, "authorship/3.0.0");
        assert_eq!(log.attestations.len(), 1);
    }

    #[test]
    fn test_parse_note_with_compat_no_divider() {
        let note = "garbage data without a divider";
        let result = parse_note_with_compat(note);
        assert!(result.is_none());
    }
}
