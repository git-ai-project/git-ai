use crate::api::client::{ApiClient, ApiContext};
use crate::api::types::CasMessagesObject;
use crate::authorship::internal_db::InternalDatabase;
use crate::authorship::prompt_utils::find_prompt;
use crate::git::find_repository;
use crate::utils::debug_log;

/// Handle the `show-prompt` command
///
/// Usage: `git-ai show-prompt <prompt_id> [--commit <rev>] [--offset <n>]`
///
/// Returns the prompt object from the authorship note where the given prompt ID is found.
/// By default returns from the most recent commit containing the prompt.
pub fn handle_show_prompt(args: &[String]) {
    let parsed = match parse_args(args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    };

    let repo = match find_repository(&Vec::<String>::new()) {
        Ok(repo) => repo,
        Err(e) => {
            eprintln!("Failed to find repository: {}", e);
            std::process::exit(1);
        }
    };

    match find_prompt(
        &repo,
        &parsed.prompt_id,
        parsed.commit.as_deref(),
        parsed.offset,
    ) {
        Ok((commit_sha, mut prompt_record)) => {
            // If messages are empty, resolve from the best available source.
            // Priority: CAS cache → CAS API (if messages_url) → local SQLite
            if prompt_record.messages.is_empty() {
                if let Some(url) = &prompt_record.messages_url
                    && let Some(hash) = url.rsplit('/').next().filter(|h| !h.is_empty())
                {
                    // 1. Check cas_cache (instant, local)
                    if let Ok(db_mutex) = InternalDatabase::global()
                        && let Ok(db_guard) = db_mutex.lock()
                        && let Ok(Some(cached_json)) = db_guard.get_cas_cache(hash)
                        && let Ok(cas_obj) = serde_json::from_str::<CasMessagesObject>(&cached_json)
                    {
                        prompt_record.messages = cas_obj.messages;
                        debug_log("show-prompt: resolved from cas_cache");
                    }

                    // 2. If cache miss, fetch from CAS API (network)
                    if prompt_record.messages.is_empty() {
                        let context = ApiContext::new(None);
                        if context.auth_token.is_some() {
                            debug_log(&format!(
                                "show-prompt: trying CAS API for hash {}",
                                &hash[..8.min(hash.len())]
                            ));
                            let client = ApiClient::new(context);
                            match client.read_ca_prompt_store(&[hash]) {
                                Ok(response) => {
                                    for result in &response.results {
                                        if result.status == "ok"
                                            && let Some(content) = &result.content
                                        {
                                            let json_str =
                                                serde_json::to_string(content).unwrap_or_default();
                                            if let Ok(cas_obj) =
                                                serde_json::from_value::<CasMessagesObject>(
                                                    content.clone(),
                                                )
                                            {
                                                prompt_record.messages = cas_obj.messages;
                                                debug_log(&format!(
                                                    "show-prompt: resolved {} messages from CAS API",
                                                    prompt_record.messages.len()
                                                ));
                                                // Cache for next time
                                                if let Ok(db_mutex) = InternalDatabase::global()
                                                    && let Ok(mut db_guard) = db_mutex.lock()
                                                {
                                                    let _ = db_guard.set_cas_cache(hash, &json_str);
                                                }
                                            }
                                        }
                                    }
                                }
                                Err(e) => {
                                    debug_log(&format!("show-prompt: CAS API error: {}", e));
                                }
                            }
                        } else {
                            debug_log("show-prompt: no auth token, skipping CAS API");
                        }
                    }
                }

                // 3. Last resort: local SQLite (for prompts without a CAS URL)
                if prompt_record.messages.is_empty()
                    && let Ok(db_mutex) = InternalDatabase::global()
                    && let Ok(db_guard) = db_mutex.lock()
                    && let Ok(Some(db_record)) = db_guard.get_prompt(&parsed.prompt_id)
                    && !db_record.messages.messages.is_empty()
                {
                    prompt_record.messages = db_record.messages.messages;
                    debug_log(&format!(
                        "show-prompt: resolved {} messages from local SQLite",
                        prompt_record.messages.len()
                    ));
                }
            }

            // Output the prompt as JSON, including the commit SHA for context
            let output = serde_json::json!({
                "commit": commit_sha,
                "prompt_id": parsed.prompt_id,
                "prompt": prompt_record,
            });
            println!(
                "{}",
                serde_json::to_string_pretty(&output).unwrap_or_else(|_| "{}".to_string())
            );
        }
        Err(e) => {
            eprintln!("Error: {}", e);
            std::process::exit(1);
        }
    }
}

#[derive(Debug)]
pub struct ParsedArgs {
    pub prompt_id: String,
    pub commit: Option<String>,
    pub offset: usize,
}

pub fn parse_args(args: &[String]) -> Result<ParsedArgs, String> {
    let mut prompt_id: Option<String> = None;
    let mut commit: Option<String> = None;
    let mut offset: Option<usize> = None;

    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];

        if arg == "--commit" {
            if i + 1 >= args.len() {
                return Err("--commit requires a value".to_string());
            }
            i += 1;
            commit = Some(args[i].clone());
        } else if arg == "--offset" {
            if i + 1 >= args.len() {
                return Err("--offset requires a value".to_string());
            }
            i += 1;
            offset = Some(
                args[i]
                    .parse::<usize>()
                    .map_err(|_| "--offset must be a non-negative integer")?,
            );
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

    let prompt_id = prompt_id.ok_or("show-prompt requires a prompt ID")?;

    // Validate mutual exclusivity of --commit and --offset
    if commit.is_some() && offset.is_some() {
        return Err("--commit and --offset are mutually exclusive".to_string());
    }

    Ok(ParsedArgs {
        prompt_id,
        commit,
        offset: offset.unwrap_or(0),
    })
}
