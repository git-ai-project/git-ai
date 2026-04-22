//! Applies the effective prompt-storage policy to an `AuthorshipLog` right
//! before it is written to git notes.
//!
//! This was originally inline in `post_commit.rs`. It is extracted here so
//! `rebase_authorship.rs`'s rewrite paths (amend / rebase / cherry-pick /
//! squash-merge) can enforce the same policy on every `notes_add`. See
//! `fix/prompt-storage-in-rewrite-paths` for why that matters.

use std::collections::HashMap;

use crate::api::{ApiClient, ApiContext};
use crate::authorship::authorship_log::PromptRecord;
use crate::authorship::authorship_log_serialization::AuthorshipLog;
use crate::authorship::secrets::{redact_secrets_from_prompts, strip_prompt_messages};
use crate::config::{Config, PromptStorageMode};
use crate::error::GitAiError;
use crate::git::repository::Repository;

/// Mutate `authorship_log.metadata.prompts` so that the note written to disk
/// reflects the effective `prompt_storage` mode for `repo`:
///
/// - `Local`  — strip all `messages` (prompts stay in SQLite only).
/// - `Notes`  — keep `messages`, but redact secrets first.
/// - `Default` — attempt a CAS upload (when logged in / API key / custom URL)
///   and clear `messages` from the note regardless of upload outcome.
///
/// Custom attributes from config are also injected into every prompt record.
pub fn apply_prompt_storage_policy(
    repo: &Repository,
    authorship_log: &mut AuthorshipLog,
) -> Result<(), GitAiError> {
    let config = Config::fresh();
    let effective_storage = config.effective_prompt_storage(&Some(repo.clone()));
    let using_custom_api = config.api_base_url() != crate::config::DEFAULT_API_BASE_URL;
    let custom_attrs = config.custom_attributes().clone();

    if !custom_attrs.is_empty() {
        for pr in authorship_log.metadata.prompts.values_mut() {
            pr.custom_attributes = Some(custom_attrs.clone());
        }
    }

    match effective_storage {
        PromptStorageMode::Local => {
            strip_prompt_messages(&mut authorship_log.metadata.prompts);
        }
        PromptStorageMode::Notes => {
            let count = redact_secrets_from_prompts(&mut authorship_log.metadata.prompts);
            if count > 0 {
                tracing::debug!("Redacted {} secrets from prompts", count);
            }
        }
        PromptStorageMode::Default => {
            let context = ApiContext::new(None);
            let client = ApiClient::new(context);
            let should_enqueue_cas =
                client.is_logged_in() || client.has_api_key() || using_custom_api;

            if should_enqueue_cas {
                let redaction_count =
                    redact_secrets_from_prompts(&mut authorship_log.metadata.prompts);
                if redaction_count > 0 {
                    tracing::debug!(
                        "Redacted {} secrets from prompts before CAS upload",
                        redaction_count
                    );
                }

                if let Err(e) =
                    enqueue_prompt_messages_to_cas(repo, &mut authorship_log.metadata.prompts)
                {
                    tracing::debug!("[Warning] Failed to enqueue prompt messages to CAS: {}", e);
                    strip_prompt_messages(&mut authorship_log.metadata.prompts);
                }
            } else {
                strip_prompt_messages(&mut authorship_log.metadata.prompts);
            }
        }
    }

    Ok(())
}

/// Apply the effective policy to an already-serialized note body. Used by the
/// fast-path remap callers in `rebase_authorship.rs` that byte-copy a source
/// note: we must parse, re-apply policy, and re-serialize so that the current
/// policy is enforced on every rewrite (preventing propagation of stale
/// messages from older/leaky upstream notes).
pub fn apply_prompt_storage_policy_to_note(
    repo: &Repository,
    note_content: &str,
) -> Result<String, GitAiError> {
    let mut log = match AuthorshipLog::deserialize_from_string(note_content) {
        Ok(log) => log,
        // If we can't parse it, pass it through unchanged. The downstream reader
        // will produce the same error, but we don't want to silently lose data.
        Err(_) => return Ok(note_content.to_string()),
    };
    apply_prompt_storage_policy(repo, &mut log)?;
    log.serialize_to_string()
        .map_err(|_| GitAiError::Generic("Failed to serialize authorship log".to_string()))
}

fn enqueue_prompt_messages_to_cas(
    repo: &Repository,
    prompts: &mut std::collections::BTreeMap<String, PromptRecord>,
) -> Result<(), GitAiError> {
    use crate::authorship::internal_db::InternalDatabase;

    let db = InternalDatabase::global()?;
    let mut db_lock = db
        .lock()
        .map_err(|e| GitAiError::Generic(format!("Failed to lock database: {}", e)))?;

    let mut metadata = HashMap::new();
    metadata.insert("api_version".to_string(), "v1".to_string());
    metadata.insert("kind".to_string(), "prompt".to_string());

    let repo_url = repo
        .get_default_remote()
        .ok()
        .flatten()
        .and_then(|remote_name| {
            repo.remotes_with_urls().ok().and_then(|remotes| {
                remotes
                    .into_iter()
                    .find(|(name, _)| name == &remote_name)
                    .map(|(_, url)| url)
            })
        });

    if let Some(url) = repo_url
        && let Ok(normalized) = crate::repo_url::normalize_repo_url(&url)
    {
        metadata.insert("repo_url".to_string(), normalized);
    }

    let api_base_url = Config::fresh().api_base_url().to_string();

    for (_key, prompt) in prompts.iter_mut() {
        if !prompt.messages.is_empty() {
            let messages_obj = crate::api::types::CasMessagesObject {
                messages: prompt.messages.clone(),
            };
            let messages_json = serde_json::to_value(&messages_obj)
                .map_err(|e| GitAiError::Generic(format!("Failed to serialize messages: {}", e)))?;

            let hash = db_lock.enqueue_cas_object(&messages_json, Some(&metadata))?;

            let metadata_json = serde_json::to_string(&metadata).ok();
            let canonical = serde_json_canonicalizer::to_string(&messages_json)
                .unwrap_or_else(|_| messages_json.to_string());
            let cas_payload = crate::daemon::control_api::CasSyncPayload {
                hash: hash.clone(),
                data: canonical,
                metadata: metadata_json,
            };

            if crate::daemon::daemon_process_active() {
                let _ =
                    crate::daemon::telemetry_worker::submit_daemon_internal_cas(vec![cas_payload]);
            } else if crate::daemon::telemetry_handle::daemon_telemetry_available() {
                crate::daemon::telemetry_handle::submit_cas(vec![cas_payload]);
            }

            prompt.messages_url = Some(format!("{}/cas/{}", api_base_url, hash));
            prompt.messages.clear();
        }
    }

    Ok(())
}
