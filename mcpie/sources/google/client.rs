//! HTTP client for Google REST APIs with access-token resolution and pageToken pagination.

use std::time::{Duration, Instant};

use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::sync::{Mutex, OnceCell};

use super::{ENV_TOKENS, Extra, OAuthConfig, TOKEN_URL, oauth};
use crate::config::{Credential, CredentialSpec, Secret, SourceConfig, resolve};
use crate::model::{SourceError, cursor};
use crate::sources::http::{Http, Response, retry_after};

const REFRESH_MARGIN: Duration = Duration::from_secs(60);

#[derive(Debug, Serialize, Deserialize)]
pub struct PageState {
    pub page_token: String,
}

/// One `pageToken`-paginated GET.
pub struct PageCall<'a> {
    pub operation: &'a str,
    pub path: &'a str,
    pub query: Vec<(&'static str, String)>,
    pub limit: Option<u32>,
    pub default_limit: u32,
    pub max_limit: u32,
    pub cursor: Option<&'a str>,
    pub items_key: &'a str,
}

/// A resolved access token and where it came from.
pub struct Access {
    pub token: Secret,
    pub provenance: String,
}

struct Refreshed {
    token: Secret,
    expires_at: Instant,
}

pub struct GoogleClient {
    id: String,
    http: Http,
    token: Option<Secret>,
    token_command: Option<String>,
    oauth: Option<OAuthConfig>,
    token_url: String,
    fixed: OnceCell<Option<Credential>>,
    refreshed: Mutex<Option<Refreshed>>,
}

impl GoogleClient {
    pub fn new(id: &str, http: Http, config: &SourceConfig, extra: &Extra) -> Self {
        Self {
            id: id.to_owned(),
            http,
            token: config.token.clone(),
            token_command: config.token_command.clone(),
            oauth: extra.oauth.clone(),
            token_url: extra
                .token_url
                .clone()
                .unwrap_or_else(|| TOKEN_URL.to_owned()),
            fixed: OnceCell::new(),
            refreshed: Mutex::new(None),
        }
    }

    pub fn http(&self) -> &Http {
        &self.http
    }

    /// Resolve an access token: static settings, then the OAuth refresh token, then the
    /// environment. Refreshed tokens are cached until shortly before they expire.
    pub async fn access(&self) -> Result<Access, SourceError> {
        let fixed = self
            .fixed
            .get_or_try_init(|| async {
                resolve(CredentialSpec {
                    token: self.token.as_ref(),
                    token_command: self.token_command.as_deref(),
                    env: &[],
                    gh_cli: false,
                })
                .await
            })
            .await?;
        if let Some(credential) = fixed {
            return Ok(Access {
                token: credential.secret.clone(),
                provenance: credential.provenance.to_string(),
            });
        }
        if let Some(oauth) = &self.oauth
            && let Some(refresh_token) = oauth.refresh_token.as_ref().filter(|t| !t.is_empty())
        {
            let mut guard = self.refreshed.lock().await;
            if let Some(current) = guard.as_ref()
                && current.expires_at > Instant::now() + REFRESH_MARGIN
            {
                return Ok(Access {
                    token: current.token.clone(),
                    provenance: "oauth refresh token".into(),
                });
            }
            let (token, ttl) = oauth::refresh(
                &self.http,
                &self.token_url,
                &oauth.client_id,
                &oauth.client_secret,
                refresh_token,
            )
            .await?;
            *guard = Some(Refreshed {
                token: token.clone(),
                expires_at: Instant::now() + ttl,
            });
            return Ok(Access {
                token,
                provenance: "oauth refresh token".into(),
            });
        }
        for name in ENV_TOKENS {
            if let Ok(value) = std::env::var(name)
                && !value.trim().is_empty()
            {
                return Ok(Access {
                    token: Secret::new(value.trim()),
                    provenance: format!("env:{name}"),
                });
            }
        }
        Err(SourceError::NotConfigured(format!(
            "{}: no credentials; set sources.{}.token_command (e.g. `gcloud auth application-default print-access-token`) or run `mcpie auth {}`",
            self.id, self.id, self.id
        )))
    }

    /// GET a JSON resource.
    pub async fn get(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<(Value, Response), SourceError> {
        let response = self.get_raw(path, query).await?;
        Ok((response.json(), response))
    }

    /// GET anything (media downloads, exports); the caller reads the body.
    pub async fn get_raw(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<Response, SourceError> {
        let access = self.access().await?;
        let request = self
            .http
            .request(Method::GET, &self.http.url(path))
            .query(query)
            .bearer_auth(access.token.expose());
        let response = self.http.send(request).await?;
        if !response.is_success() {
            return Err(map_error(&response));
        }
        Ok(response)
    }

    /// One page of a list endpoint, with the page token wrapped in a source-bound cursor.
    pub async fn page(
        &self,
        call: PageCall<'_>,
    ) -> Result<(Vec<Value>, Option<String>), SourceError> {
        let mut query = call.query;
        if let Some(cursor) = call.cursor {
            let state: PageState = cursor::decode(cursor, &self.id, call.operation)?;
            query.push(("pageToken", state.page_token));
        }
        query.push((
            if call.path.starts_with("/users/") {
                "maxResults"
            } else {
                "pageSize"
            },
            call.limit
                .unwrap_or(call.default_limit)
                .clamp(1, call.max_limit)
                .to_string(),
        ));
        let (body, _) = self.get(call.path, &query).await?;
        let items = body
            .get(call.items_key)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let next = body
            .get("nextPageToken")
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty())
            .map(|t| {
                cursor::encode(
                    &self.id,
                    call.operation,
                    &PageState {
                        page_token: t.to_owned(),
                    },
                )
            });
        Ok((items, next))
    }

    /// Validate a user-supplied API path for the raw `request` operation.
    pub fn validate_path(path: &str) -> Result<&str, SourceError> {
        if !path.starts_with('/') || path.contains("://") || path.starts_with("//") {
            return Err(SourceError::InvalidInput(format!(
                "path must start with `/` and stay on the API host, got {path:?}"
            )));
        }
        Ok(path)
    }
}

/// Turn a JSON object of scalars into query parameters.
pub fn query_from_map(map: &Map<String, Value>) -> Result<Vec<(&str, String)>, SourceError> {
    map.iter()
        .map(|(key, value)| match value {
            Value::String(s) => Ok((key.as_str(), s.clone())),
            Value::Number(n) => Ok((key.as_str(), n.to_string())),
            Value::Bool(b) => Ok((key.as_str(), b.to_string())),
            other => Err(SourceError::InvalidInput(format!(
                "query.{key} must be a scalar, got {other}"
            ))),
        })
        .collect()
}

/// Map Google's `{"error": {"code", "message", "errors": [{"reason"}]}}` bodies.
pub fn map_error(response: &Response) -> SourceError {
    let body = response.json();
    let error = body.get("error");
    let message = error
        .and_then(|e| e.get("message"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| {
            let text = response.text();
            if text.is_empty() {
                format!("http {}", response.status)
            } else {
                text.chars().take(200).collect()
            }
        });
    let reasons: Vec<String> = error
        .and_then(|e| e.get("errors"))
        .and_then(Value::as_array)
        .map(|errors| {
            errors
                .iter()
                .filter_map(|e| e.get("reason").and_then(Value::as_str))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let rate_limited = reasons
        .iter()
        .any(|r| r.contains("ateLimit") || r.contains("uota") || r == "dailyLimitExceeded");
    match response.status {
        401 => SourceError::Auth(message),
        403 if rate_limited => SourceError::RateLimited {
            retry_after: retry_after(response).or(Some(Duration::from_secs(60))),
            message,
        },
        403 => SourceError::Auth(message),
        404 => SourceError::NotFound(message),
        400 => SourceError::InvalidInput(message),
        429 => SourceError::RateLimited {
            retry_after: retry_after(response).or(Some(Duration::from_secs(60))),
            message,
        },
        status => SourceError::Upstream { status, message },
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
    fn maps_google_errors() {
        assert!(matches!(
            map_error(&response(
                401,
                r#"{"error":{"message":"Invalid Credentials"}}"#
            )),
            SourceError::Auth(_)
        ));
        let limited = map_error(&response(
            403,
            r#"{"error":{"message":"Rate Limit Exceeded","errors":[{"reason":"userRateLimitExceeded"}]}}"#,
        ));
        assert!(
            matches!(limited, SourceError::RateLimited { .. }),
            "{limited}"
        );
        assert!(matches!(
            map_error(&response(
                403,
                r#"{"error":{"message":"Insufficient Permission","errors":[{"reason":"insufficientPermissions"}]}}"#
            )),
            SourceError::Auth(_)
        ));
        assert!(matches!(
            map_error(&response(404, r#"{"error":{"message":"File not found"}}"#)),
            SourceError::NotFound(_)
        ));
        assert!(matches!(
            map_error(&response(400, r#"{"error":{"message":"Invalid Value"}}"#)),
            SourceError::InvalidInput(_)
        ));
        assert!(matches!(
            map_error(&response(500, "boom")),
            SourceError::Upstream { status: 500, .. }
        ));
        assert!(GoogleClient::validate_path("/files").is_ok());
        assert!(GoogleClient::validate_path("https://evil/x").is_err());
    }
}
