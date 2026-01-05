use crate::api::{ApiClient, ApiContext, ApiFileRecord};
use crate::api::{BundleData, CreateBundleRequest};
use crate::authorship::prompt_utils::find_prompt_with_db_fallback;
use crate::authorship::secrets::redact_secrets_from_prompts;
use crate::commands::diff::{get_diff_json_filtered, DiffOptions};
use crate::git::find_repository;
use std::collections::{BTreeMap, HashMap};

/// Handle the `share` command
///
/// Usage: git-ai share [<prompt_id>] [--title <title>]
///
/// If prompt_id is provided, uses CLI mode. Otherwise, launches TUI.
pub fn handle_share(args: &[String]) {
    match parse_args(args) {
        Ok(parsed) => {
            // Has prompt_id - use CLI mode
            handle_share_cli(parsed);
        }
        Err(e) if e.contains("requires a prompt ID") => {
            // No prompt_id - launch TUI
            if let Err(tui_err) = crate::commands::share_tui::run_tui() {
                eprintln!("TUI error: {}", tui_err);
                std::process::exit(1);
            }
        }
        Err(e) => {
            // Other parsing error
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    }
}

/// CLI mode for share command (when prompt_id is provided)
fn handle_share_cli(parsed: ParsedArgs) {
    // Sync recent prompts before lookup to ensure fresh data
    let _ = crate::commands::sync_prompts::sync_recent_prompts_silent(20);

    // Try to find repository (optional - prompt might be in DB)
    let repo = find_repository(&Vec::<String>::new()).ok();

    // Find the prompt (DB first, then repository)
    let (_commit_sha, prompt_record) = match find_prompt_with_db_fallback(&parsed.prompt_id, repo.as_ref()) {
        Ok((sha, prompt)) => (sha, prompt),
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    };

    // Generate a title if not provided
    let title = parsed.title.unwrap_or_else(|| {
        // Try to get snippet from database
        use crate::authorship::internal_db::InternalDatabase;

        if let Ok(db) = InternalDatabase::global() {
            if let Ok(db_guard) = db.lock() {
                if let Ok(Some(db_record)) = db_guard.get_prompt(&parsed.prompt_id) {
                    return db_record.first_message_snippet(60);
                }
            }
        }

        // Fallback if not in database
        format!("Prompt {}", parsed.prompt_id)
    });

    // Create bundle using helper (single prompt only in CLI mode, no diffs)
    match create_bundle(parsed.prompt_id, prompt_record, title, false, false) {
        Ok(response) => {
            println!("{}", response.url);
        }
        Err(e) => {
            eprintln!("Failed to create bundle: {}", e);
            std::process::exit(1);
        }
    }
}

#[derive(Debug)]
pub struct ParsedArgs {
    pub prompt_id: String,
    pub title: Option<String>,
}

pub fn parse_args(args: &[String]) -> Result<ParsedArgs, String> {
    let mut prompt_id: Option<String> = None;
    let mut title: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];

        if arg == "--title" {
            if i + 1 >= args.len() {
                return Err("--title requires a value".to_string());
            }
            i += 1;
            title = Some(args[i].clone());
        } else if arg.starts_with('-') {
            return Err(format!("Unknown option: {}", arg));
        } else {
            if prompt_id.is_some() {
                return Err("Only one prompt ID can be specified".to_string());
            }
            prompt_id = Some(arg.clone());
        }

        i += 1;
    }

    let prompt_id = prompt_id.ok_or("share requires a prompt ID")?;

    Ok(ParsedArgs { prompt_id, title })
}

/// Create a bundle from a prompt, optionally including all prompts in the commit
/// and optionally including code diffs
pub fn create_bundle(
    prompt_id: String,
    prompt_record: crate::authorship::authorship_log::PromptRecord,
    title: String,
    include_all_in_commit: bool,
    include_diffs: bool,
) -> Result<crate::api::CreateBundleResponse, crate::error::GitAiError> {
    use crate::authorship::internal_db::InternalDatabase;

    let mut prompts = HashMap::new();
    prompts.insert(prompt_id.clone(), prompt_record.clone());

    // Get commit_sha from the database
    let db = InternalDatabase::global()?;
    let db_guard = db.lock().map_err(|e| {
        crate::error::GitAiError::Generic(format!("Failed to lock database: {}", e))
    })?;

    let db_record = db_guard.get_prompt(&prompt_id)?;
    let commit_sha = db_record.as_ref().and_then(|r| r.commit_sha.clone());

    // If include_all_in_commit, fetch all prompts with same commit_sha
    if include_all_in_commit {
        if let Some(ref sha) = commit_sha {
            // Get all prompts for this commit
            let commit_prompts = db_guard.get_prompts_by_commit(sha)?;

            for p in commit_prompts {
                prompts.insert(p.id.clone(), p.to_prompt_record());
            }
        }
    }

    // Drop the db guard before we do other work
    drop(db_guard);

    // Collect prompt IDs for diff filtering
    let prompt_ids: Vec<String> = prompts.keys().cloned().collect();

    // Redact secrets from all prompts before uploading
    let mut prompts_btree: BTreeMap<String, _> = prompts.into_iter().collect();
    redact_secrets_from_prompts(&mut prompts_btree);
    let prompts: HashMap<String, _> = prompts_btree.into_iter().collect();

    // Get diff files if requested
    let files: HashMap<String, ApiFileRecord> = if include_diffs {
        if let Some(ref sha) = commit_sha {
            // Try to get the repository
            if let Ok(repo) = find_repository(&Vec::<String>::new()) {
                // Configure diff options based on whether we're sharing all prompts or just one
                let diff_options = DiffOptions {
                    prompt_ids: Some(prompt_ids),
                    // If sharing all in commit, don't filter to attributed files (include full diff)
                    // If sharing single prompt, filter to only files touched by this prompt
                    filter_to_attributed_files: !include_all_in_commit,
                };

                match get_diff_json_filtered(&repo, sha, diff_options) {
                    Ok(diff_json) => {
                        // Convert FileDiffJson to ApiFileRecord
                        diff_json
                            .files
                            .iter()
                            .map(|(path, file_diff)| (path.clone(), ApiFileRecord::from(file_diff)))
                            .collect()
                    }
                    Err(_) => HashMap::new(), // Diff failed, proceed without files
                }
            } else {
                HashMap::new() // No repo, proceed without files
            }
        } else {
            HashMap::new() // No commit SHA, proceed without files
        }
    } else {
        HashMap::new()
    };

    // Create bundle with prompts and optional files
    let bundle_request = CreateBundleRequest {
        title,
        data: BundleData { prompts, files },
    };

    let context = ApiContext::new(None);
    let client = ApiClient::new(context);
    client.create_bundle(bundle_request)
}

