//! Google APIs: one OAuth-aware client shared by the Drive and Gmail sources.
//!
//! Credentials, in order: a static `token`, a `token_command` printing an access token (for
//! example `gcloud auth application-default print-access-token`), an `oauth` block whose
//! refresh token mcpie exchanges itself (obtained with `mcpie auth <source>`), then the
//! `GOOGLE_OAUTH_ACCESS_TOKEN` environment variable.

mod client;
pub mod drive;
pub mod gmail;
pub mod oauth;

pub use client::{GoogleClient, PageCall};
use serde::Deserialize;

use crate::config::Secret;

pub const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
pub const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
pub const DRIVE_SCOPE: &str = "https://www.googleapis.com/auth/drive.readonly";
pub const GMAIL_SCOPE: &str = "https://www.googleapis.com/auth/gmail.readonly";
const ENV_TOKENS: &[&str] = &["GOOGLE_OAUTH_ACCESS_TOKEN"];

/// `[sources.<id>.oauth]`: an OAuth client the user created in Google Cloud.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OAuthConfig {
    pub client_id: String,
    pub client_secret: Secret,
    /// Written by `mcpie auth <source>`.
    #[serde(default)]
    pub refresh_token: Option<Secret>,
}

/// Type-specific settings shared by `gdrive` and `gmail`.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Extra {
    pub oauth: Option<OAuthConfig>,
    /// OAuth endpoints override, for tests.
    pub token_url: Option<String>,
    pub auth_url: Option<String>,
}

/// The read-only scope a source type needs.
pub fn scope_for(kind: &str) -> Option<&'static str> {
    match kind {
        "gdrive" => Some(DRIVE_SCOPE),
        "gmail" => Some(GMAIL_SCOPE),
        _ => None,
    }
}
