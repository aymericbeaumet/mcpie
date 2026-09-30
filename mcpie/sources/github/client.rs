//! GitHub REST client: credentials, headers, error mapping and page-number pagination.

use std::time::Duration;

use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::sync::OnceCell;

use crate::config::{Credential, CredentialSpec, Secret, resolve};
use crate::model::{SourceError, cursor};
use crate::sources::http::{Http, Response, retry_after, seconds_until};

pub const DEFAULT_BASE_URL: &str = "https://api.github.com";
pub const MAX_PAGE_SIZE: u32 = 100;
const ENV_TOKENS: &[&str] = &["GITHUB_TOKEN", "GH_TOKEN"];

pub struct Client {
    id: String,
    http: Http,
    token: Option<Secret>,
    token_command: Option<String>,
    credential: OnceCell<Credential>,
}

/// A page-number cursor state.
#[derive(Debug, Serialize, Deserialize)]
pub struct PageState {
    pub page: u32,
}

impl Client {
    pub fn new(id: &str, http: Http, token: Option<Secret>, token_command: Option<String>) -> Self {
        Self {
            id: id.to_owned(),
            http,
            token,
            token_command,
            credential: OnceCell::new(),
        }
    }

    pub async fn credential(&self) -> Result<&Credential, SourceError> {
        self.credential
            .get_or_try_init(|| async {
                let spec = CredentialSpec { token: self.token.as_ref(), token_command: self.token_command.as_deref(), env: ENV_TOKENS, gh_cli: true };
                resolve(spec).await?.ok_or_else(|| {
                    SourceError::NotConfigured(format!(
                        "{}: no token; set sources.{}.token, GITHUB_TOKEN, or log in with `gh auth login`",
                        self.id, self.id
                    ))
                })
            })
            .await
    }

    /// GET `path` with query parameters; returns the parsed body and the response for headers.
    pub async fn get(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<(Value, Response), SourceError> {
        let credential = self.credential().await?;
        let request = self
            .http
            .request(Method::GET, &self.http.url(path))
            .query(query)
            .bearer_auth(credential.secret.expose())
            .header("accept", "application/vnd.github+json")
            .header("x-github-api-version", "2022-11-28");
        let response = self.http.send(request).await?;
        if !response.is_success() {
            return Err(map_error(&response));
        }
        Ok((response.json(), response))
    }

    /// A paginated GET: `limit` and an opaque cursor in, items and the next cursor out.
    pub async fn get_page(
        &self,
        operation: &str,
        path: &str,
        mut query: Vec<(&str, String)>,
        limit: Option<u32>,
        cursor: Option<&str>,
    ) -> Result<(Vec<Value>, Option<String>), SourceError> {
        let page = match cursor {
            Some(cursor) => cursor::decode::<PageState>(cursor, &self.id, operation)?.page,
            None => 1,
        };
        query.push((
            "per_page",
            limit.unwrap_or(30).clamp(1, MAX_PAGE_SIZE).to_string(),
        ));
        query.push(("page", page.to_string()));
        let (body, response) = self.get(path, &query).await?;
        let items = match body {
            Value::Array(items) => items,
            Value::Object(mut object) => object
                .remove("items")
                .and_then(|v| v.as_array().cloned())
                .ok_or_else(|| SourceError::Upstream {
                    status: response.status,
                    message: "expected a list".into(),
                })?,
            other => {
                return Err(SourceError::Upstream {
                    status: response.status,
                    message: format!("expected a list, got {other}"),
                });
            }
        };
        let next = next_page(&response)
            .map(|page| cursor::encode(&self.id, operation, &PageState { page }));
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

/// The `page` number of the `rel="next"` link, if any. Only the number is kept: the URL itself
/// is never trusted or stored.
pub fn next_page(response: &Response) -> Option<u32> {
    let link = response.header("link")?;
    link.split(',').find_map(|part| {
        let (url, rel) = part.split_once(';')?;
        if !rel.contains("rel=\"next\"") {
            return None;
        }
        let url = url.trim().trim_start_matches('<').trim_end_matches('>');
        let query = url.split_once('?')?.1;
        query.split('&').find_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            (key == "page").then(|| value.parse::<u32>().ok()).flatten()
        })
    })
}

pub fn map_error(response: &Response) -> SourceError {
    let message = response
        .json()
        .get("message")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| {
            let text = response.text();
            if text.is_empty() {
                format!("http {}", response.status)
            } else {
                text
            }
        });
    match response.status {
        401 => SourceError::Auth(message),
        403 | 429 => {
            let remaining = response
                .header("x-ratelimit-remaining")
                .and_then(|v| v.parse::<u64>().ok());
            if let Some(after) = retry_after(response) {
                return SourceError::RateLimited {
                    retry_after: Some(after),
                    message,
                };
            }
            if response.status == 429 || remaining == Some(0) {
                let reset = response
                    .header("x-ratelimit-reset")
                    .and_then(|v| v.parse::<u64>().ok())
                    .map(seconds_until);
                return SourceError::RateLimited {
                    retry_after: reset.or(Some(Duration::from_secs(60))),
                    message,
                };
            }
            SourceError::Auth(message)
        }
        404 => SourceError::NotFound(message),
        422 => SourceError::InvalidInput(message),
        status => SourceError::Upstream { status, message },
    }
}

/// Classify a token by its prefix, for `mcpie sources`.
pub fn token_kind(token: &str) -> &'static str {
    if token.starts_with("github_pat_") {
        "fine-grained pat"
    } else if token.starts_with("ghp_") {
        "classic pat"
    } else if token.starts_with("gho_") {
        "oauth"
    } else if token.starts_with("ghs_") {
        "app installation"
    } else if token.starts_with("ghu_") {
        "app user"
    } else {
        "unknown"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(status: u16, headers: &[(&str, &str)], body: &str) -> Response {
        let mut map = reqwest::header::HeaderMap::new();
        for (name, value) in headers {
            map.insert(
                reqwest::header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                value.parse().unwrap(),
            );
        }
        Response {
            status,
            headers: map,
            body: body.as_bytes().to_vec(),
        }
    }

    #[test]
    fn parses_next_page_from_link_header() {
        let r = response(
            200,
            &[(
                "link",
                "<https://api.github.com/user/repos?per_page=2&page=3>; rel=\"next\", <https://api.github.com/user/repos?per_page=2&page=9>; rel=\"last\"",
            )],
            "[]",
        );
        assert_eq!(next_page(&r), Some(3));
        let r = response(
            200,
            &[(
                "link",
                "<https://api.github.com/user/repos?page=1>; rel=\"prev\"",
            )],
            "[]",
        );
        assert_eq!(next_page(&r), None);
        assert_eq!(next_page(&response(200, &[], "[]")), None);
    }

    #[test]
    fn maps_github_failures() {
        assert!(
            matches!(map_error(&response(401, &[], r#"{"message":"Bad credentials"}"#)), SourceError::Auth(m) if m == "Bad credentials")
        );
        assert!(matches!(
            map_error(&response(404, &[], "{}")),
            SourceError::NotFound(_)
        ));
        assert!(matches!(
            map_error(&response(422, &[], r#"{"message":"Validation Failed"}"#)),
            SourceError::InvalidInput(_)
        ));
        let limited = map_error(&response(
            403,
            &[("x-ratelimit-remaining", "0"), ("x-ratelimit-reset", "0")],
            r#"{"message":"API rate limit exceeded"}"#,
        ));
        assert!(
            matches!(
                limited,
                SourceError::RateLimited {
                    retry_after: Some(_),
                    ..
                }
            ),
            "{limited}"
        );
        let secondary = map_error(&response(403, &[("retry-after", "7")], "{}"));
        assert_eq!(secondary.retry_after(), Some(Duration::from_secs(7)));
        assert!(matches!(
            map_error(&response(
                403,
                &[("x-ratelimit-remaining", "10")],
                r#"{"message":"Resource not accessible"}"#
            )),
            SourceError::Auth(_)
        ));
        assert!(matches!(
            map_error(&response(502, &[], "bad gateway")),
            SourceError::Upstream { status: 502, .. }
        ));
    }

    #[test]
    fn validates_raw_paths() {
        assert!(Client::validate_path("/repos/a/b").is_ok());
        assert!(Client::validate_path("repos/a/b").is_err());
        assert!(Client::validate_path("https://evil/x").is_err());
        assert!(Client::validate_path("//evil/x").is_err());
        assert_eq!(token_kind("ghp_x"), "classic pat");
        assert_eq!(token_kind("github_pat_x"), "fine-grained pat");
    }
}
