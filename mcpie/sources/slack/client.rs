//! Slack Web API client: form-encoded POSTs, body-level error mapping, cursor and page
//! pagination, and a small directory cache for `#channel` and `@user` resolution.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::sync::{OnceCell, RwLock};

use crate::config::{Credential, CredentialSpec, Secret, resolve};
use crate::model::{SourceError, cursor};
use crate::sources::http::{Http, Response, retry_after};

pub const DEFAULT_BASE_URL: &str = "https://slack.com/api";
const ENV_TOKENS: &[&str] = &["SLACK_TOKEN", "SLACK_USER_TOKEN", "SLACK_BOT_TOKEN"];
const DIRECTORY_TTL: Duration = Duration::from_secs(300);
const DIRECTORY_PAGES: usize = 20;

/// Web API methods the raw `request` operation may call. Everything else mutates.
pub const READ_METHODS: &[&str] = &[
    "auth.test",
    "bookmarks.list",
    "bots.info",
    "chat.getPermalink",
    "conversations.history",
    "conversations.info",
    "conversations.list",
    "conversations.members",
    "conversations.replies",
    "dnd.info",
    "dnd.teamInfo",
    "emoji.list",
    "files.info",
    "files.list",
    "pins.list",
    "reactions.get",
    "reactions.list",
    "reminders.info",
    "reminders.list",
    "search.all",
    "search.files",
    "search.messages",
    "stars.list",
    "team.info",
    "usergroups.list",
    "usergroups.users.list",
    "users.getPresence",
    "users.identity",
    "users.info",
    "users.list",
    "users.lookupByEmail",
    "users.profile.get",
];

#[derive(Debug, Serialize, Deserialize)]
pub struct CursorState {
    pub cursor: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PageState {
    pub page: u32,
}

#[derive(Default)]
struct Directory {
    fetched: Option<Instant>,
    /// name -> id
    channels: HashMap<String, String>,
    /// handle -> id
    users: HashMap<String, String>,
    /// id -> display name
    user_names: HashMap<String, String>,
}

pub struct Client {
    id: String,
    http: Http,
    token: Option<Secret>,
    token_command: Option<String>,
    credential: OnceCell<Credential>,
    directory: RwLock<Directory>,
}

impl Client {
    pub fn new(id: &str, http: Http, token: Option<Secret>, token_command: Option<String>) -> Self {
        Self {
            id: id.to_owned(),
            http,
            token,
            token_command,
            credential: OnceCell::new(),
            directory: RwLock::new(Directory::default()),
        }
    }

    pub async fn credential(&self) -> Result<&Credential, SourceError> {
        self.credential
            .get_or_try_init(|| async {
                let spec = CredentialSpec {
                    token: self.token.as_ref(),
                    token_command: self.token_command.as_deref(),
                    env: ENV_TOKENS,
                    gh_cli: false,
                };
                resolve(spec).await?.ok_or_else(|| {
                    SourceError::NotConfigured(format!(
                        "{}: no token; set sources.{}.token or SLACK_TOKEN",
                        self.id, self.id
                    ))
                })
            })
            .await
    }

    /// Call a Web API method with form-encoded parameters and return its `ok` body.
    pub async fn call(
        &self,
        method: &str,
        params: &[(&str, String)],
    ) -> Result<(Value, Response), SourceError> {
        let credential = self.credential().await?;
        let request = self
            .http
            .request(Method::POST, &self.http.url(&format!("/{method}")))
            .bearer_auth(credential.secret.expose())
            .form(params);
        let response = self.http.send(request).await?;
        let body = response.json();
        if let Some(error) = failure(&response, &body) {
            return Err(error);
        }
        Ok((body, response))
    }

    /// A cursor-paginated call: wraps Slack's cursor in a source-bound one.
    pub async fn call_page(
        &self,
        operation: &str,
        method: &str,
        mut params: Vec<(&str, String)>,
        limit: Option<u32>,
        max_limit: u32,
        cursor: Option<&str>,
        items_key: &str,
    ) -> Result<(Vec<Value>, Option<String>), SourceError> {
        if let Some(cursor) = cursor {
            let state: CursorState = cursor::decode(cursor, &self.id, operation)?;
            params.push(("cursor", state.cursor));
        }
        params.push((
            "limit",
            limit.unwrap_or(100).clamp(1, max_limit).to_string(),
        ));
        let (body, _) = self.call(method, &params).await?;
        let items = body
            .get(items_key)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let next = next_cursor(&body)
            .map(|c| cursor::encode(&self.id, operation, &CursorState { cursor: c }));
        Ok((items, next))
    }

    /// Resolve `#name` to a channel id; ids pass through.
    pub async fn resolve_channel(&self, channel: &str) -> Result<String, SourceError> {
        let Some(name) = channel.strip_prefix('#') else {
            return Ok(channel.to_owned());
        };
        self.refresh_directory().await?;
        self.directory
            .read()
            .await
            .channels
            .get(name)
            .cloned()
            .ok_or_else(|| {
                SourceError::NotFound(format!(
                    "channel #{name} (not visible to this token or does not exist)"
                ))
            })
    }

    /// Resolve `@handle` to a user id; ids pass through.
    pub async fn resolve_user(&self, user: &str) -> Result<String, SourceError> {
        let Some(handle) = user.strip_prefix('@') else {
            return Ok(user.to_owned());
        };
        self.refresh_directory().await?;
        self.directory
            .read()
            .await
            .users
            .get(&handle.to_lowercase())
            .cloned()
            .ok_or_else(|| SourceError::NotFound(format!("user @{handle}")))
    }

    /// Display name for a user id, from the directory cache.
    pub async fn user_name(&self, id: &str) -> Option<String> {
        if self.refresh_directory().await.is_err() {
            return None;
        }
        self.directory.read().await.user_names.get(id).cloned()
    }

    async fn refresh_directory(&self) -> Result<(), SourceError> {
        if self
            .directory
            .read()
            .await
            .fetched
            .is_some_and(|at| at.elapsed() < DIRECTORY_TTL)
        {
            return Ok(());
        }
        let mut directory = Directory {
            fetched: Some(Instant::now()),
            ..Directory::default()
        };
        let mut cursor: Option<String> = None;
        for _ in 0..DIRECTORY_PAGES {
            let mut params = vec![
                ("types", "public_channel,private_channel".to_owned()),
                ("limit", "1000".to_owned()),
                ("exclude_archived", "true".to_owned()),
            ];
            if let Some(c) = cursor.take() {
                params.push(("cursor", c));
            }
            let (body, _) = self.call("conversations.list", &params).await?;
            for channel in body
                .get("channels")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if let (Some(id), Some(name)) = (
                    channel.get("id").and_then(Value::as_str),
                    channel.get("name").and_then(Value::as_str),
                ) {
                    directory.channels.insert(name.to_owned(), id.to_owned());
                }
            }
            cursor = next_cursor(&body);
            if cursor.is_none() {
                break;
            }
        }
        for _ in 0..DIRECTORY_PAGES {
            let mut params = vec![("limit", "1000".to_owned())];
            if let Some(c) = cursor.take() {
                params.push(("cursor", c));
            }
            let (body, _) = self.call("users.list", &params).await?;
            for user in body
                .get("members")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let Some(id) = user.get("id").and_then(Value::as_str) else {
                    continue;
                };
                let profile = user.get("profile");
                let display = profile
                    .and_then(|p| p.get("display_name"))
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty());
                let real = user
                    .get("real_name")
                    .or_else(|| profile.and_then(|p| p.get("real_name")))
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty());
                let handle = user.get("name").and_then(Value::as_str);
                for alias in [display, handle, real].into_iter().flatten() {
                    directory
                        .users
                        .entry(alias.to_lowercase())
                        .or_insert_with(|| id.to_owned());
                }
                if let Some(name) = display.or(real).or(handle) {
                    directory.user_names.insert(id.to_owned(), name.to_owned());
                }
            }
            cursor = next_cursor(&body);
            if cursor.is_none() {
                break;
            }
        }
        *self.directory.write().await = directory;
        Ok(())
    }

    /// Reject anything but the known read-only methods.
    pub fn validate_method(method: &str) -> Result<&str, SourceError> {
        READ_METHODS
            .contains(&method)
            .then_some(method)
            .ok_or_else(|| {
                SourceError::Unsupported(format!(
                    "{method} is not a read-only method; mcpie only calls: {}",
                    READ_METHODS.join(", ")
                ))
            })
    }
}

/// Slack's `response_metadata.next_cursor`, empty when exhausted.
pub fn next_cursor(body: &Value) -> Option<String> {
    body.get("response_metadata")
        .and_then(|m| m.get("next_cursor"))
        .and_then(Value::as_str)
        .filter(|c| !c.is_empty())
        .map(str::to_owned)
}

/// Turn a JSON object of scalars into form parameters.
pub fn params_from_map(map: &Map<String, Value>) -> Result<Vec<(&str, String)>, SourceError> {
    map.iter()
        .map(|(key, value)| match value {
            Value::String(s) => Ok((key.as_str(), s.clone())),
            Value::Number(n) => Ok((key.as_str(), n.to_string())),
            Value::Bool(b) => Ok((key.as_str(), b.to_string())),
            other => Err(SourceError::InvalidInput(format!(
                "params.{key} must be a scalar, got {other}"
            ))),
        })
        .collect()
}

/// Map HTTP status and Slack's `ok: false` bodies to `SourceError`.
pub fn failure(response: &Response, body: &Value) -> Option<SourceError> {
    if response.status == 429 {
        return Some(SourceError::RateLimited {
            retry_after: retry_after(response).or(Some(Duration::from_secs(30))),
            message: "slack rate limit".into(),
        });
    }
    if !response.is_success() {
        return Some(SourceError::Upstream {
            status: response.status,
            message: response.text().chars().take(200).collect(),
        });
    }
    if body.get("ok").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let error = body
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or("unknown_error")
        .to_owned();
    let detail = match body.get("needed").and_then(Value::as_str) {
        Some(needed) => format!("{error} (needed scope: {needed})"),
        None => error.clone(),
    };
    Some(match error.as_str() {
        "ratelimited" => SourceError::RateLimited {
            retry_after: retry_after(response).or(Some(Duration::from_secs(30))),
            message: detail,
        },
        "invalid_auth"
        | "not_authed"
        | "token_revoked"
        | "token_expired"
        | "account_inactive"
        | "missing_scope"
        | "not_allowed_token_type"
        | "no_permission"
        | "ekm_access_denied"
        | "not_in_channel"
        | "user_is_restricted" => SourceError::Auth(detail),
        "channel_not_found" | "user_not_found" | "thread_not_found" | "message_not_found"
        | "file_not_found" | "users_not_found" => SourceError::NotFound(detail),
        "invalid_arguments" | "invalid_cursor" | "invalid_ts_latest" | "invalid_ts_oldest"
        | "invalid_limit" | "invalid_arg_name" | "invalid_array_arg" | "invalid_charset"
        | "invalid_form_data" | "invalid_post_type" | "missing_post_type" | "no_query" => {
            SourceError::InvalidInput(detail)
        }
        "method_not_supported_for_channel_type" | "unknown_method" | "method_deprecated" => {
            SourceError::Unsupported(detail)
        }
        _ => SourceError::Upstream {
            status: response.status,
            message: detail,
        },
    })
}

/// Token flavour by prefix, for `mcpie sources`.
pub fn token_kind(token: &str) -> &'static str {
    if token.starts_with("xoxp-") {
        "user"
    } else if token.starts_with("xoxb-") {
        "bot"
    } else if token.starts_with("xoxc-") {
        "session"
    } else if token.starts_with("xoxe") {
        "refresh"
    } else {
        "unknown"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(status: u16, body: &str) -> Response {
        Response {
            status,
            headers: reqwest::header::HeaderMap::new(),
            body: body.as_bytes().to_vec(),
        }
    }

    #[test]
    fn maps_slack_bodies() {
        let ok = response(200, r#"{"ok":true}"#);
        assert!(failure(&ok, &ok.json()).is_none());
        for (body, check) in [
            (r#"{"ok":false,"error":"invalid_auth"}"#, "upstream_auth"),
            (
                r#"{"ok":false,"error":"missing_scope","needed":"search:read"}"#,
                "upstream_auth",
            ),
            (r#"{"ok":false,"error":"channel_not_found"}"#, "not_found"),
            (r#"{"ok":false,"error":"invalid_cursor"}"#, "invalid_input"),
            (r#"{"ok":false,"error":"ratelimited"}"#, "rate_limited"),
            (r#"{"ok":false,"error":"something_else"}"#, "upstream"),
        ] {
            let r = response(200, body);
            let error = failure(&r, &r.json()).unwrap();
            assert_eq!(error.code(), check, "{body}: {error}");
        }
        let r = response(429, "");
        assert!(matches!(
            failure(&r, &r.json()).unwrap(),
            SourceError::RateLimited { .. }
        ));
        let r = response(503, "down");
        assert!(matches!(
            failure(&r, &r.json()).unwrap(),
            SourceError::Upstream { status: 503, .. }
        ));
    }

    #[test]
    fn cursors_and_methods() {
        assert_eq!(
            next_cursor(&serde_json::json!({ "response_metadata": { "next_cursor": "abc" } })),
            Some("abc".into())
        );
        assert_eq!(
            next_cursor(&serde_json::json!({ "response_metadata": { "next_cursor": "" } })),
            None
        );
        assert!(Client::validate_method("conversations.info").is_ok());
        assert!(Client::validate_method("chat.postMessage").is_err());
        assert_eq!(token_kind("xoxb-1"), "bot");
        assert_eq!(token_kind("xoxp-1"), "user");
    }
}
