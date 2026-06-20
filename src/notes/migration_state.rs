//! Per-repo migration state for the HTTP notes backend rollout.
//!
//! The migration state is a *control-plane* concern, distinct from the note
//! data plane (which is keyed by `commit_sha`). It is keyed by the **normalized
//! remote URL** the daemon would push `refs/notes/ai` to, served by the backend,
//! and polled by the daemon.
//!
//! The default is [`MigrationState::GitNotesOnly`], which is a **no-op**: nothing
//! in this module changes behavior until later PRs consume the state. Resolution
//! fails safe to `GitNotesOnly` whenever the HTTP backend is not configured, the
//! client is unauthenticated, or the poll fails.
//!
//! See `docs/http-notes-backend-migration-spec.md`.

use crate::api::client::{ApiClient, ApiContext};
use crate::config::Config;
use crate::error::GitAiError;
use serde::{Deserialize, Serialize};

/// Per-repo rollout state, ordered from the legacy git-notes-only behavior
/// through full HTTP cutover. Variants are declared in advancing order so later
/// PRs can gate behavior with comparisons (e.g. `state >= DualWriteShadow`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MigrationState {
    /// Today's behavior: notes live in `refs/notes/ai` only.
    #[default]
    GitNotesOnly,
    /// Write git-notes **and** the HTTP queue; reads still trust git-notes.
    DualWriteShadow,
    /// Write both; reads prefer HTTP with a git fallback.
    DualWriteHttpRead,
    /// Cutover: client stops pushing `refs/notes/ai` (still writes it locally);
    /// HTTP is primary.
    HttpPrimaryNoPush,
    /// git-notes fully retired; HTTP only.
    HttpOnly,
}

impl MigrationState {
    /// Stable wire/string form (matches the serde representation).
    pub fn as_str(&self) -> &'static str {
        match self {
            MigrationState::GitNotesOnly => "git_notes_only",
            MigrationState::DualWriteShadow => "dual_write_shadow",
            MigrationState::DualWriteHttpRead => "dual_write_http_read",
            MigrationState::HttpPrimaryNoPush => "http_primary_no_push",
            MigrationState::HttpOnly => "http_only",
        }
    }

    /// Notes are enqueued to the HTTP backend at this state or later.
    pub fn writes_http(&self) -> bool {
        *self >= MigrationState::DualWriteShadow
    }

    /// Reads prefer the HTTP backend (with a git fallback) at this state or later.
    pub fn reads_http_first(&self) -> bool {
        *self >= MigrationState::DualWriteHttpRead
    }

    /// The client suppresses its own push to `refs/notes/ai` at this state or
    /// later (it still writes the note locally for lossless rollback).
    pub fn suppresses_client_push(&self) -> bool {
        *self >= MigrationState::HttpPrimaryNoPush
    }

    /// The client still writes notes to local git-notes. Dropped only at
    /// [`MigrationState::HttpOnly`].
    pub fn writes_git_notes(&self) -> bool {
        *self < MigrationState::HttpOnly
    }
}

/// Response body for `GET /worker/migration-state`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationStateResponse {
    pub state: MigrationState,
    /// Optional minimum client version the backend advertises (observed, not
    /// enforced on the default path). Only used for the optional, late
    /// `HttpOnly` retirement lever.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_client_version: Option<String>,
}

/// Normalize a git remote URL into a stable control-plane key.
///
/// Canonicalizes SCP-style SSH (`git@host:org/repo.git`) and URL forms
/// (`https://host/org/repo.git`, `ssh://git@host/org/repo`) to a single
/// `host[:port]/path` shape: scheme and userinfo are dropped, the host is
/// lowercased, and a trailing `.git`/`/` is stripped. Path case is preserved
/// (paths can be case-sensitive).
pub fn normalize_remote_url(remote_url: &str) -> String {
    let s = remote_url.trim();

    // Split off the scheme, if any. SCP-style remotes have no scheme.
    let (had_scheme, rest) = match s.find("://") {
        Some(idx) => (true, &s[idx + 3..]),
        None => (false, s),
    };

    // Drop userinfo (`user@`).
    let rest = match rest.find('@') {
        Some(idx) => &rest[idx + 1..],
        None => rest,
    };

    // For SCP-style (no scheme) the first ':' separates host from path; turn it
    // into '/'. For real URLs leave ':' alone so a host:port survives.
    let unified: String = if had_scheme {
        rest.to_string()
    } else {
        rest.replacen(':', "/", 1)
    };

    let unified = unified.trim_end_matches('/');
    let unified = unified.strip_suffix(".git").unwrap_or(unified);

    match unified.split_once('/') {
        Some((host, path)) => format!("{}/{}", host.to_lowercase(), path),
        None => unified.to_lowercase(),
    }
}

/// Poll the backend for the migration state of an already-normalized remote.
///
/// A `404` (no state registered for this repo) resolves to the default
/// [`MigrationState::GitNotesOnly`]. A `426` surfaces as
/// [`GitAiError::UpgradeRequired`].
pub fn fetch_migration_state(
    client: &ApiClient,
    remote_normalized: &str,
) -> Result<MigrationState, GitAiError> {
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("remote", remote_normalized)
        .finish();
    let endpoint = format!("/worker/migration-state?{}", query);
    let response = client.context().get(&endpoint)?;
    let status = response.status_code;
    let body = response
        .as_str()
        .map_err(|e| GitAiError::Generic(format!("Failed to read response body: {}", e)))?;

    match status {
        200 => {
            let parsed: MigrationStateResponse =
                serde_json::from_str(body).map_err(GitAiError::JsonError)?;
            Ok(parsed.state)
        }
        404 => Ok(MigrationState::GitNotesOnly),
        426 => Err(GitAiError::UpgradeRequired(format!(
            "migration-state poll rejected: client version too old (HTTP 426): {}",
            body
        ))),
        _ => Err(GitAiError::Generic(format!(
            "migration-state poll failed with status {}: {}",
            status, body
        ))),
    }
}

/// Best-effort resolution of the per-repo migration state for a raw remote URL.
///
/// Fails safe to [`MigrationState::GitNotesOnly`] (the no-op default) whenever
/// the HTTP backend is not enabled, no backend URL is configured, the client is
/// unauthenticated, or the poll errors — so nothing changes behavior until the
/// backend explicitly advances a repo's state.
pub fn resolve_state_for_remote(remote_url: &str) -> MigrationState {
    let cfg = Config::fresh();
    if !cfg.notes_backend_enabled() {
        return MigrationState::GitNotesOnly;
    }
    let Some(backend_url) = cfg.notes_backend_url().map(str::to_string) else {
        return MigrationState::GitNotesOnly;
    };

    let client = ApiClient::new(ApiContext::new(Some(backend_url)));
    if !client.is_logged_in() && !client.has_api_key() {
        return MigrationState::GitNotesOnly;
    }

    let normalized = normalize_remote_url(remote_url);
    match fetch_migration_state(&client, &normalized) {
        Ok(state) => state,
        Err(e) => {
            tracing::debug!(%e, "migration-state poll failed; defaulting to GitNotesOnly");
            MigrationState::GitNotesOnly
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_state_ordering() {
        assert!(MigrationState::GitNotesOnly < MigrationState::DualWriteShadow);
        assert!(MigrationState::DualWriteShadow < MigrationState::DualWriteHttpRead);
        assert!(MigrationState::DualWriteHttpRead < MigrationState::HttpPrimaryNoPush);
        assert!(MigrationState::HttpPrimaryNoPush < MigrationState::HttpOnly);
    }

    #[test]
    fn test_default_is_git_notes_only() {
        assert_eq!(MigrationState::default(), MigrationState::GitNotesOnly);
    }

    #[test]
    fn test_state_behavior_flags() {
        assert!(!MigrationState::GitNotesOnly.writes_http());
        assert!(MigrationState::DualWriteShadow.writes_http());

        assert!(!MigrationState::DualWriteShadow.reads_http_first());
        assert!(MigrationState::DualWriteHttpRead.reads_http_first());

        assert!(!MigrationState::DualWriteHttpRead.suppresses_client_push());
        assert!(MigrationState::HttpPrimaryNoPush.suppresses_client_push());

        assert!(MigrationState::HttpPrimaryNoPush.writes_git_notes());
        assert!(!MigrationState::HttpOnly.writes_git_notes());
    }

    #[test]
    fn test_serde_snake_case_roundtrip() {
        let json = serde_json::to_string(&MigrationState::DualWriteHttpRead).unwrap();
        assert_eq!(json, "\"dual_write_http_read\"");
        let parsed: MigrationState = serde_json::from_str("\"http_primary_no_push\"").unwrap();
        assert_eq!(parsed, MigrationState::HttpPrimaryNoPush);
    }

    #[test]
    fn test_normalize_ssh_and_https_match() {
        let ssh = normalize_remote_url("git@github.com:Org/Repo.git");
        let https = normalize_remote_url("https://github.com/Org/Repo.git");
        assert_eq!(ssh, "github.com/Org/Repo");
        assert_eq!(ssh, https);
    }

    #[test]
    fn test_normalize_strips_dot_git_and_trailing_slash_and_lowercases_host() {
        assert_eq!(
            normalize_remote_url("https://GitHub.com/org/repo/"),
            "github.com/org/repo"
        );
        assert_eq!(
            normalize_remote_url("ssh://git@GitHub.com/org/repo.git"),
            "github.com/org/repo"
        );
    }

    #[test]
    fn test_normalize_preserves_port() {
        assert_eq!(
            normalize_remote_url("https://github.com:8080/org/repo.git"),
            "github.com:8080/org/repo"
        );
    }

    #[test]
    fn test_fetch_migration_state_200() {
        let mut server = mockito::Server::new();
        let _m = server
            .mock("GET", mockito::Matcher::Any)
            .with_status(200)
            .with_body(r#"{"state":"dual_write_http_read"}"#)
            .create();

        let client = ApiClient::new(ApiContext::without_auth(Some(server.url())));
        let state = fetch_migration_state(&client, "github.com/org/repo").unwrap();
        assert_eq!(state, MigrationState::DualWriteHttpRead);
    }

    #[test]
    fn test_fetch_migration_state_404_is_git_notes_only() {
        let mut server = mockito::Server::new();
        let _m = server
            .mock("GET", mockito::Matcher::Any)
            .with_status(404)
            .with_body("not registered")
            .create();

        let client = ApiClient::new(ApiContext::without_auth(Some(server.url())));
        let state = fetch_migration_state(&client, "github.com/org/repo").unwrap();
        assert_eq!(state, MigrationState::GitNotesOnly);
    }

    #[test]
    fn test_fetch_migration_state_426_is_upgrade_required() {
        let mut server = mockito::Server::new();
        let _m = server
            .mock("GET", mockito::Matcher::Any)
            .with_status(426)
            .with_body("client too old")
            .create();

        let client = ApiClient::new(ApiContext::without_auth(Some(server.url())));
        let err = fetch_migration_state(&client, "github.com/org/repo").unwrap_err();
        assert!(matches!(err, GitAiError::UpgradeRequired(_)));
    }
}
