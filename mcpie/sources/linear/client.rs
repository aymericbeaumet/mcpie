//! Linear GraphQL client: API-key or OAuth auth, error mapping and Relay cursor pagination.

use std::time::Duration;

use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tokio::sync::OnceCell;

use crate::config::{Credential, CredentialSpec, Secret, resolve};
use crate::model::{SourceError, cursor};
use crate::sources::http::{Http, retry_after};

pub const DEFAULT_BASE_URL: &str = "https://api.linear.app";
pub const MAX_PAGE_SIZE: u32 = 250;
const ENV_TOKENS: &[&str] = &["LINEAR_API_KEY", "LINEAR_TOKEN"];

#[derive(Debug, Serialize, Deserialize)]
pub struct CursorState {
    pub after: String,
}

pub struct Client {
    id: String,
    http: Http,
    token: Option<Secret>,
    token_command: Option<String>,
    credential: OnceCell<Credential>,
}

/// One connection page: nodes plus the wrapped cursor of the next page.
pub struct Connection {
    pub nodes: Vec<Value>,
    pub next_cursor: Option<String>,
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
                let spec = CredentialSpec {
                    token: self.token.as_ref(),
                    token_command: self.token_command.as_deref(),
                    env: ENV_TOKENS,
                    gh_cli: false,
                };
                resolve(spec).await?.ok_or_else(|| {
                    SourceError::NotConfigured(format!(
                        "{}: no API key; set sources.{}.token or LINEAR_API_KEY",
                        self.id, self.id
                    ))
                })
            })
            .await
    }

    /// Execute a GraphQL document and return its `data`.
    pub async fn query(&self, document: &str, variables: Value) -> Result<Value, SourceError> {
        let credential = self.credential().await?;
        let token = credential.secret.expose();
        let header = if token.starts_with("lin_api_") {
            token.to_owned()
        } else {
            format!("Bearer {token}")
        };
        let request = self
            .http
            .request(Method::POST, &self.http.url("/graphql"))
            .header("authorization", header)
            .json(&json!({ "query": document, "variables": variables }));
        let response = self.http.send(request).await?;
        let body = response.json();
        let errors = body
            .get("errors")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if response.status == 429 {
            return Err(SourceError::RateLimited {
                retry_after: retry_after(&response).or(Some(Duration::from_secs(60))),
                message: "linear rate limit".into(),
            });
        }
        if !errors.is_empty() {
            return Err(map_graphql_errors(response.status, &errors));
        }
        if !response.is_success() {
            return Err(match response.status {
                401 | 403 => SourceError::Auth(format!("http {}", response.status)),
                status => SourceError::Upstream {
                    status,
                    message: response.text().chars().take(200).collect(),
                },
            });
        }
        Ok(body.get("data").cloned().unwrap_or(Value::Null))
    }

    /// Fetch one page of a Relay connection at `path` inside `data`.
    pub async fn connection(
        &self,
        operation: &str,
        document: &str,
        mut variables: Map<String, Value>,
        path: &[&str],
        limit: Option<u32>,
        cursor: Option<&str>,
    ) -> Result<Connection, SourceError> {
        variables.insert(
            "first".into(),
            Value::from(limit.unwrap_or(50).clamp(1, MAX_PAGE_SIZE)),
        );
        if let Some(cursor) = cursor {
            let state: CursorState = cursor::decode(cursor, &self.id, operation)?;
            variables.insert("after".into(), Value::String(state.after));
        }
        let data = self.query(document, Value::Object(variables)).await?;
        let connection = path
            .iter()
            .try_fold(&data, |value, key| value.get(key))
            .ok_or_else(|| SourceError::Upstream {
                status: 200,
                message: format!("missing {} in response", path.join(".")),
            })?;
        let nodes = connection
            .get("nodes")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let page_info = connection.get("pageInfo");
        let has_next = page_info
            .and_then(|p| p.get("hasNextPage"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let end_cursor = page_info
            .and_then(|p| p.get("endCursor"))
            .and_then(Value::as_str);
        let next_cursor = match (has_next, end_cursor) {
            (true, Some(after)) => Some(cursor::encode(
                &self.id,
                operation,
                &CursorState {
                    after: after.to_owned(),
                },
            )),
            _ => None,
        };
        Ok(Connection { nodes, next_cursor })
    }

    /// Refuse documents that could mutate.
    pub fn validate_query(document: &str) -> Result<&str, SourceError> {
        let lowered = document.to_lowercase();
        let has_mutation = lowered
            .split(|c: char| !c.is_alphanumeric() && c != '_')
            .any(|word| word == "mutation");
        if has_mutation {
            return Err(SourceError::Unsupported(
                "mutations are refused; mcpie only runs read-only queries".into(),
            ));
        }
        Ok(document)
    }
}

fn map_graphql_errors(status: u16, errors: &[Value]) -> SourceError {
    let message = errors
        .iter()
        .filter_map(|e| e.get("message").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("; ");
    let codes: Vec<&str> = errors
        .iter()
        .filter_map(|e| {
            e.get("extensions")
                .and_then(|x| x.get("code"))
                .and_then(Value::as_str)
        })
        .collect();
    let types: Vec<&str> = errors
        .iter()
        .filter_map(|e| {
            e.get("extensions")
                .and_then(|x| x.get("type"))
                .and_then(Value::as_str)
        })
        .collect();
    let has = |needle: &str| {
        codes
            .iter()
            .chain(types.iter())
            .any(|c| c.eq_ignore_ascii_case(needle))
    };
    if status == 401
        || status == 403
        || has("AUTHENTICATION_ERROR")
        || has("authentication error")
        || has("FORBIDDEN")
    {
        SourceError::Auth(message)
    } else if has("RATELIMITED") || has("ratelimited") {
        SourceError::RateLimited {
            retry_after: Some(Duration::from_secs(60)),
            message,
        }
    } else if has("GRAPHQL_VALIDATION_FAILED")
        || has("BAD_USER_INPUT")
        || has("invalid input")
        || has("Entity not found") && false
    {
        SourceError::InvalidInput(message)
    } else if message.contains("Entity not found") || has("ENTITY_NOT_FOUND") {
        SourceError::NotFound(message)
    } else if status == 400 {
        SourceError::InvalidInput(message)
    } else {
        SourceError::Upstream { status, message }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_mutations_but_not_words_inside_names() {
        assert!(Client::validate_query("query { viewer { id } }").is_ok());
        assert!(Client::validate_query("{ issues(first: 1) { nodes { id } } }").is_ok());
        assert!(Client::validate_query("mutation { issueCreate(input: {}) { success } }").is_err());
        assert!(Client::validate_query("query { x }\nMUTATION { y }").is_err());
        assert!(Client::validate_query("query { mutationsCount }").is_ok());
    }

    #[test]
    fn maps_graphql_errors() {
        let auth = map_graphql_errors(
            400,
            &[
                json!({ "message": "Authentication required", "extensions": { "type": "authentication error" } }),
            ],
        );
        assert!(matches!(auth, SourceError::Auth(_)), "{auth}");
        let missing = map_graphql_errors(200, &[json!({ "message": "Entity not found: Issue" })]);
        assert!(matches!(missing, SourceError::NotFound(_)), "{missing}");
        let invalid = map_graphql_errors(
            400,
            &[
                json!({ "message": "Cannot query field", "extensions": { "code": "GRAPHQL_VALIDATION_FAILED" } }),
            ],
        );
        assert!(matches!(invalid, SourceError::InvalidInput(_)), "{invalid}");
        let limited = map_graphql_errors(
            400,
            &[json!({ "message": "Rate limit", "extensions": { "code": "RATELIMITED" } })],
        );
        assert!(
            matches!(limited, SourceError::RateLimited { .. }),
            "{limited}"
        );
    }
}
