use super::CredentialStore;
use super::identity::{TokenOrg, extract_identity_from_access_token};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthState {
    LoggedOut,
    LoggedIn,
    RefreshExpired,
    Error(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthStatus {
    pub backend: String,
    pub state: AuthState,
    pub access_token_expires_at: Option<i64>,
    pub refresh_token_expires_at: Option<i64>,
    pub user_id: Option<String>,
    pub email: Option<String>,
    pub name: Option<String>,
    pub personal_org_id: Option<String>,
    pub orgs: Vec<TokenOrg>,
}

pub fn collect_auth_status() -> AuthStatus {
    let store = CredentialStore::new();
    let backend = store.backend_name().to_string();

    match store.load() {
        Ok(None) => AuthStatus {
            backend,
            state: AuthState::LoggedOut,
            access_token_expires_at: None,
            refresh_token_expires_at: None,
            user_id: None,
            email: None,
            name: None,
            personal_org_id: None,
            orgs: Vec::new(),
        },
        Ok(Some(creds)) => {
            let identity = extract_identity_from_access_token(&creds.access_token);
            let state = if creds.is_refresh_token_expired() {
                AuthState::RefreshExpired
            } else {
                AuthState::LoggedIn
            };

            AuthStatus {
                backend,
                state,
                access_token_expires_at: Some(creds.access_token_expires_at),
                refresh_token_expires_at: Some(creds.refresh_token_expires_at),
                user_id: identity.user_id,
                email: identity.email,
                name: identity.name,
                personal_org_id: identity.personal_org_id,
                orgs: identity.orgs,
            }
        }
        Err(err) => AuthStatus {
            backend,
            state: AuthState::Error(err),
            access_token_expires_at: None,
            refresh_token_expires_at: None,
            user_id: None,
            email: None,
            name: None,
            personal_org_id: None,
            orgs: Vec::new(),
        },
    }
}

pub fn format_unix_timestamp(ts: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp(ts, 0)
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_else(|| format!("{} (invalid timestamp)", ts))
}
