//! Gmail over the Gmail API v1 (read-only).

use async_trait::async_trait;
use base64::Engine;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::{Extra, GoogleClient, PageCall};
use crate::config::SourceConfig;
use crate::model::{CallContext, OperationSpec, Page, Source, SourceError, Status, typed};
use crate::sources::http::Http;
use crate::sources::{BuildError, Settings};

pub const DEFAULT_BASE_URL: &str = "https://gmail.googleapis.com/gmail/v1";
const METADATA_HEADERS: &[&str] = &["From", "To", "Cc", "Subject", "Date", "Message-Id"];

/// No input.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Empty {}

/// List message ids, newest first.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListMessages {
    /// Gmail search syntax, e.g. `from:jane newer_than:7d has:attachment`.
    #[serde(default)]
    pub q: Option<String>,
    /// Only messages carrying all of these label ids (e.g. INBOX, UNREAD).
    #[serde(default)]
    pub labels: Vec<String>,
    /// Include spam and trash (default false).
    #[serde(default)]
    pub include_spam_trash: Option<bool>,
    /// Page size, at most 500.
    #[serde(default)]
    pub limit: Option<u32>,
    /// Cursor from a previous page.
    #[serde(default)]
    pub cursor: Option<String>,
}

/// Search messages and return their headers and snippets.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchMessages {
    /// Gmail search syntax, e.g. `subject:invoice newer_than:30d`.
    pub query: String,
    /// Page size, at most 100.
    #[serde(default)]
    pub limit: Option<u32>,
    /// Cursor from a previous page.
    #[serde(default)]
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MessageFormat {
    /// Headers, decoded text body and the raw payload (default).
    #[default]
    Full,
    /// Headers only.
    Metadata,
    /// Ids and labels only.
    Minimal,
}

/// Read one message.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetMessage {
    /// Message id.
    pub message: String,
    /// How much to return: full (default), metadata or minimal.
    #[serde(default)]
    pub detail: Option<MessageFormat>,
}

/// List thread ids, newest first.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListThreads {
    /// Gmail search syntax.
    #[serde(default)]
    pub q: Option<String>,
    /// Only threads carrying all of these label ids.
    #[serde(default)]
    pub labels: Vec<String>,
    /// Page size, at most 500.
    #[serde(default)]
    pub limit: Option<u32>,
    /// Cursor from a previous page.
    #[serde(default)]
    pub cursor: Option<String>,
}

/// Read a whole thread with decoded messages.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetThread {
    /// Thread id.
    pub thread: String,
}

/// A raw GET against the Gmail API.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Request {
    /// API path starting with `/`, e.g. `/users/me/settings/filters`.
    pub path: String,
    /// Query parameters as an object of scalars.
    #[serde(default)]
    pub query: Option<Map<String, Value>>,
}

pub struct Gmail {
    id: String,
    client: GoogleClient,
    operations: Vec<OperationSpec>,
}

type Query = Vec<(&'static str, String)>;

impl Gmail {
    pub fn new(settings: Settings, config: &SourceConfig) -> Result<Self, BuildError> {
        let extra: Extra = config
            .extra()
            .map_err(|e| BuildError::source(&settings.id, e))?;
        let base_url = config.base_url.as_deref().unwrap_or(DEFAULT_BASE_URL);
        let http = Http::new(base_url, settings.timeout, settings.max_in_flight)
            .map_err(|e| BuildError::source(&settings.id, e))?;
        let client = GoogleClient::new(&settings.id, http, config, &extra);
        Ok(Self {
            id: settings.id,
            client,
            operations: operations(),
        })
    }

    async fn get_profile(&self, _: Empty) -> Result<Value, SourceError> {
        Ok(self.client.get("/users/me/profile", &[]).await?.0)
    }

    async fn list_labels(&self, _: Empty) -> Result<Value, SourceError> {
        let (body, _) = self.client.get("/users/me/labels", &[]).await?;
        Ok(body.get("labels").cloned().unwrap_or_else(|| json!([])))
    }

    async fn list_messages(&self, input: ListMessages) -> Result<Page<Value>, SourceError> {
        let mut query = Query::new();
        if let Some(q) = input.q {
            query.push(("q", q));
        }
        for label in input.labels {
            query.push(("labelIds", label));
        }
        if let Some(include) = input.include_spam_trash {
            query.push(("includeSpamTrash", include.to_string()));
        }
        let (items, next) = self
            .client
            .page(PageCall {
                operation: "list_messages",
                path: "/users/me/messages",
                query,
                limit: input.limit,
                default_limit: 25,
                max_limit: 500,
                cursor: input.cursor.as_deref(),
                items_key: "messages",
            })
            .await?;
        Ok(Page::new(items, next))
    }

    async fn search_messages(&self, input: SearchMessages) -> Result<Page<Value>, SourceError> {
        let query: Query = vec![("q", input.query)];
        let (ids, next) = self
            .client
            .page(PageCall {
                operation: "search_messages",
                path: "/users/me/messages",
                query,
                limit: input.limit,
                default_limit: 20,
                max_limit: 100,
                cursor: input.cursor.as_deref(),
                items_key: "messages",
            })
            .await?;
        let fetches = ids
            .iter()
            .filter_map(|m| m.get("id").and_then(Value::as_str))
            .map(|id| self.fetch_message(id, MessageFormat::Metadata));
        let items = futures::future::try_join_all(fetches).await?;
        Ok(Page::new(items, next))
    }

    async fn get_message(&self, input: GetMessage) -> Result<Value, SourceError> {
        self.fetch_message(
            validate_id(&input.message)?,
            input.detail.unwrap_or_default(),
        )
        .await
    }

    async fn fetch_message(&self, id: &str, format: MessageFormat) -> Result<Value, SourceError> {
        let mut query: Query = vec![("format", enum_name(format))];
        if matches!(format, MessageFormat::Metadata) {
            for header in METADATA_HEADERS {
                query.push(("metadataHeaders", (*header).to_owned()));
            }
        }
        let (message, _) = self
            .client
            .get(&format!("/users/me/messages/{id}"), &query)
            .await?;
        Ok(decode_message(message))
    }

    async fn list_threads(&self, input: ListThreads) -> Result<Page<Value>, SourceError> {
        let mut query = Query::new();
        if let Some(q) = input.q {
            query.push(("q", q));
        }
        for label in input.labels {
            query.push(("labelIds", label));
        }
        let (items, next) = self
            .client
            .page(PageCall {
                operation: "list_threads",
                path: "/users/me/threads",
                query,
                limit: input.limit,
                default_limit: 25,
                max_limit: 500,
                cursor: input.cursor.as_deref(),
                items_key: "threads",
            })
            .await?;
        Ok(Page::new(items, next))
    }

    async fn get_thread(&self, input: GetThread) -> Result<Value, SourceError> {
        let path = format!("/users/me/threads/{}", validate_id(&input.thread)?);
        let (mut thread, _) = self.client.get(&path, &[("format", "full".into())]).await?;
        if let Some(messages) = thread.get_mut("messages").and_then(Value::as_array_mut) {
            for message in messages.iter_mut() {
                *message = decode_message(message.take());
            }
        }
        Ok(thread)
    }

    async fn request(&self, input: Request) -> Result<Value, SourceError> {
        let path = GoogleClient::validate_path(&input.path)?;
        let query = input
            .query
            .as_ref()
            .map(super::client::query_from_map)
            .transpose()?
            .unwrap_or_default();
        Ok(self.client.get(path, &query).await?.0)
    }
}

fn validate_id(id: &str) -> Result<&str, SourceError> {
    if id.is_empty() || id.contains('/') || id.contains('?') {
        return Err(SourceError::InvalidInput(format!("invalid id {id:?}")));
    }
    Ok(id)
}

fn enum_name<T: serde::Serialize>(value: T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// Lift the useful parts of a Gmail message to the top: `headers`, `subject`, `from`, `to`,
/// `date`, and the decoded `text` (and `html`) bodies. The upstream `payload` stays as is.
pub fn decode_message(mut message: Value) -> Value {
    let Some(payload) = message.get("payload").cloned() else {
        return message;
    };
    let mut headers = Map::new();
    for header in payload
        .get("headers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let (Some(name), Some(value)) = (
            header.get("name").and_then(Value::as_str),
            header.get("value").and_then(Value::as_str),
        ) {
            headers.insert(name.to_lowercase(), Value::String(value.to_owned()));
        }
    }
    let mut text = String::new();
    let mut html = String::new();
    collect_bodies(&payload, &mut text, &mut html);
    if let Some(object) = message.as_object_mut() {
        for key in ["subject", "from", "to", "cc", "date"] {
            if let Some(value) = headers.get(key) {
                object.insert(key.to_owned(), value.clone());
            }
        }
        object.insert("headers".into(), Value::Object(headers));
        if !text.is_empty() {
            object.insert("text".into(), Value::String(text));
        }
        if !html.is_empty() {
            object.insert("html".into(), Value::String(html));
        }
    }
    message
}

fn collect_bodies(part: &Value, text: &mut String, html: &mut String) {
    let mime = part.get("mimeType").and_then(Value::as_str).unwrap_or("");
    let data = part
        .get("body")
        .and_then(|b| b.get("data"))
        .and_then(Value::as_str);
    match (mime, data.and_then(decode_base64url)) {
        ("text/plain", Some(decoded)) => text.push_str(&decoded),
        ("text/html", Some(decoded)) => html.push_str(&decoded),
        _ => {}
    }
    for child in part
        .get("parts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        collect_bodies(child, text, html);
    }
}

fn decode_base64url(data: &str) -> Option<String> {
    let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let trimmed = data.trim_end_matches('=');
    let bytes = engine.decode(trimmed).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

fn operations() -> Vec<OperationSpec> {
    vec![
        OperationSpec::read::<Empty, Value>(
            "get_profile",
            "Get profile",
            "Return the mailbox address and message counts.",
        ),
        OperationSpec::read::<Empty, Value>(
            "list_labels",
            "List labels",
            "List labels with their ids.",
        ),
        OperationSpec::read::<ListMessages, Page<Value>>(
            "list_messages",
            "List messages",
            "List message ids matching a Gmail query, newest first.",
        ),
        OperationSpec::read::<SearchMessages, Page<Value>>(
            "search_messages",
            "Search messages",
            "Search messages and return their headers and snippets.",
        ),
        OperationSpec::read::<GetMessage, Value>(
            "get_message",
            "Get message",
            "Read one message with decoded headers and text body.",
        ),
        OperationSpec::read::<ListThreads, Page<Value>>(
            "list_threads",
            "List threads",
            "List thread ids matching a Gmail query, newest first.",
        ),
        OperationSpec::read::<GetThread, Value>(
            "get_thread",
            "Get thread",
            "Read a whole thread with decoded messages.",
        ),
        OperationSpec::read::<Request, Value>(
            "request",
            "Raw request",
            "GET any Gmail API v1 path.",
        ),
    ]
}

#[async_trait]
impl Source for Gmail {
    fn id(&self) -> &str {
        &self.id
    }

    fn kind(&self) -> &'static str {
        "gmail"
    }

    fn description(&self) -> &str {
        "Gmail messages, threads, labels and search"
    }

    fn operations(&self) -> &[OperationSpec] {
        &self.operations
    }

    async fn call(
        &self,
        operation: &str,
        input: Value,
        _ctx: &CallContext,
    ) -> Result<Value, SourceError> {
        match operation {
            "get_profile" => typed(input, |i: Empty| self.get_profile(i)).await,
            "list_labels" => typed(input, |i: Empty| self.list_labels(i)).await,
            "list_messages" => typed(input, |i: ListMessages| self.list_messages(i)).await,
            "search_messages" => typed(input, |i: SearchMessages| self.search_messages(i)).await,
            "get_message" => typed(input, |i: GetMessage| self.get_message(i)).await,
            "list_threads" => typed(input, |i: ListThreads| self.list_threads(i)).await,
            "get_thread" => typed(input, |i: GetThread| self.get_thread(i)).await,
            "request" => typed(input, |i: Request| self.request(i)).await,
            _ => Err(SourceError::UnknownOperation(operation.to_owned())),
        }
    }

    async fn check(&self, _ctx: &CallContext) -> Result<Status, SourceError> {
        let access = self.client.access().await?;
        let (profile, _) = self.client.get("/users/me/profile", &[]).await?;
        Ok(Status {
            identity: profile
                .get("emailAddress")
                .and_then(Value::as_str)
                .map(str::to_owned),
            token_kind: Some("oauth".into()),
            scopes: vec![super::GMAIL_SCOPE.into()],
            credential: Some(access.provenance),
            warnings: Vec::new(),
            unavailable: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_headers_and_nested_bodies() {
        let message = json!({
            "id": "m1",
            "payload": {
                "mimeType": "multipart/alternative",
                "headers": [{ "name": "Subject", "value": "Hi" }, { "name": "From", "value": "Jane <jane@example.com>" }],
                "parts": [
                    { "mimeType": "text/plain", "body": { "data": "aGVsbG8gd29ybGQ" } },
                    { "mimeType": "text/html", "body": { "data": "PGI-aGk8L2I-" } }
                ]
            }
        });
        let decoded = decode_message(message);
        assert_eq!(decoded["subject"], "Hi");
        assert_eq!(decoded["from"], "Jane <jane@example.com>");
        assert_eq!(decoded["headers"]["subject"], "Hi");
        assert_eq!(decoded["text"], "hello world");
        assert_eq!(decoded["html"], "<b>hi</b>");
        assert!(decoded["payload"].is_object(), "upstream payload is kept");
    }
}
