//! Google Drive over the Drive API v3 (read-only).

use async_trait::async_trait;
use base64::Engine;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::{Extra, GoogleClient, PageCall};
use crate::config::SourceConfig;
use crate::model::{
    CallContext, Item, ItemKind, OperationRef, OperationSpec, Page, SearchProvider, SearchQuery,
    Source, SourceError, Status, normalized, typed,
};
use crate::sources::http::Http;
use crate::sources::{BuildError, Settings};

pub const DEFAULT_BASE_URL: &str = "https://www.googleapis.com/drive/v3";
const FILE_FIELDS: &str = "id,name,mimeType,modifiedTime,createdTime,size,owners(displayName,emailAddress),lastModifyingUser(displayName),webViewLink,parents,shared,driveId,trashed";
const MAX_MEDIA_BYTES: usize = 4 * 1024 * 1024;

/// No input.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Empty {}

/// List files, most recently modified first.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListFiles {
    /// Drive query, e.g. `name contains 'plan' and mimeType = 'application/vnd.google-apps.document'`.
    #[serde(default)]
    pub q: Option<String>,
    /// Only direct children of this folder id.
    #[serde(default)]
    pub folder: Option<String>,
    /// Sort keys, e.g. `modifiedTime desc` (default) or `name`.
    #[serde(default)]
    pub order_by: Option<String>,
    /// Include files from shared drives (default true).
    #[serde(default)]
    pub include_shared_drives: Option<bool>,
    /// Page size, at most 1000.
    #[serde(default)]
    pub limit: Option<u32>,
    /// Cursor from a previous page.
    #[serde(default)]
    pub cursor: Option<String>,
}

/// Read a file's metadata.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetFile {
    /// File id.
    pub file: String,
}

/// Full-text search over file names and contents.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchFiles {
    /// Words to search for.
    pub query: String,
    /// Page size, at most 1000.
    #[serde(default)]
    pub limit: Option<u32>,
    /// Cursor from a previous page.
    #[serde(default)]
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExportFormat {
    /// `text/plain` (default).
    #[default]
    Text,
    Markdown,
    Html,
    /// `text/csv`, for spreadsheets.
    Csv,
}

/// Export a Google Docs, Sheets or Slides file as text.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExportFile {
    /// File id.
    pub file: String,
    /// Output format: text (default), markdown, html or csv.
    #[serde(default)]
    pub output: Option<ExportFormat>,
}

/// Download a regular (non Google Docs) file's content.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetFileContent {
    /// File id.
    pub file: String,
}

/// List shared drives.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListSharedDrives {
    /// Page size, at most 100.
    #[serde(default)]
    pub limit: Option<u32>,
    /// Cursor from a previous page.
    #[serde(default)]
    pub cursor: Option<String>,
}

/// A raw GET against the Drive API.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Request {
    /// API path starting with `/`, e.g. `/files/ID/comments`.
    pub path: String,
    /// Query parameters as an object of scalars.
    #[serde(default)]
    pub query: Option<Map<String, Value>>,
}

pub struct Drive {
    id: String,
    client: GoogleClient,
    operations: Vec<OperationSpec>,
}

type Query = Vec<(&'static str, String)>;

impl Drive {
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

    async fn list_files(&self, input: ListFiles) -> Result<Page<Value>, SourceError> {
        let mut clauses = Vec::new();
        if let Some(q) = input.q.filter(|q| !q.trim().is_empty()) {
            clauses.push(format!("({q})"));
        }
        if let Some(folder) = input.folder {
            clauses.push(format!("'{}' in parents", escape(&folder)));
        }
        clauses.push("trashed = false".into());
        let shared = input.include_shared_drives.unwrap_or(true);
        let query: Query = vec![
            ("q", clauses.join(" and ")),
            (
                "orderBy",
                input.order_by.unwrap_or_else(|| "modifiedTime desc".into()),
            ),
            ("fields", format!("nextPageToken,files({FILE_FIELDS})")),
            ("supportsAllDrives", "true".into()),
            ("includeItemsFromAllDrives", shared.to_string()),
        ];
        let (items, next) = self
            .client
            .page(PageCall {
                operation: "list_files",
                path: "/files",
                query,
                limit: input.limit,
                default_limit: 50,
                max_limit: 1000,
                cursor: input.cursor.as_deref(),
                items_key: "files",
            })
            .await?;
        Ok(Page::new(items, next))
    }

    async fn get_file(&self, input: GetFile) -> Result<Value, SourceError> {
        let path = format!("/files/{}", validate_id(&input.file)?);
        Ok(self
            .client
            .get(
                &path,
                &[("fields", "*".into()), ("supportsAllDrives", "true".into())],
            )
            .await?
            .0)
    }

    async fn search_files(&self, input: SearchFiles) -> Result<Page<Value>, SourceError> {
        let query: Query = vec![
            (
                "q",
                format!(
                    "fullText contains '{}' and trashed = false",
                    escape(&input.query)
                ),
            ),
            ("fields", format!("nextPageToken,files({FILE_FIELDS})")),
            ("supportsAllDrives", "true".into()),
            ("includeItemsFromAllDrives", "true".into()),
        ];
        let (items, next) = self
            .client
            .page(PageCall {
                operation: "search_files",
                path: "/files",
                query,
                limit: input.limit,
                default_limit: 20,
                max_limit: 1000,
                cursor: input.cursor.as_deref(),
                items_key: "files",
            })
            .await?;
        Ok(Page::new(items, next))
    }

    async fn export_file(&self, input: ExportFile) -> Result<Value, SourceError> {
        let mime = match input.output.unwrap_or_default() {
            ExportFormat::Text => "text/plain",
            ExportFormat::Markdown => "text/markdown",
            ExportFormat::Html => "text/html",
            ExportFormat::Csv => "text/csv",
        };
        let path = format!("/files/{}/export", validate_id(&input.file)?);
        let response = self
            .client
            .get_raw(&path, &[("mimeType", mime.into())])
            .await?;
        Ok(serde_json::json!({ "file": input.file, "mime_type": mime, "content": response.text() }))
    }

    async fn get_file_content(&self, input: GetFileContent) -> Result<Value, SourceError> {
        let path = format!("/files/{}", validate_id(&input.file)?);
        let response = self
            .client
            .get_raw(
                &path,
                &[
                    ("alt", "media".into()),
                    ("supportsAllDrives", "true".into()),
                ],
            )
            .await?;
        if response.body.len() > MAX_MEDIA_BYTES {
            return Err(SourceError::Unsupported(format!(
                "file is larger than {MAX_MEDIA_BYTES} bytes; download it with the Drive UI or API directly"
            )));
        }
        let content_type = response
            .header("content-type")
            .unwrap_or("application/octet-stream")
            .to_owned();
        Ok(match String::from_utf8(response.body.clone()) {
            Ok(text) => {
                serde_json::json!({ "file": input.file, "mime_type": content_type, "encoding": "utf-8", "size": response.body.len(), "content": text })
            }
            Err(_) => {
                serde_json::json!({ "file": input.file, "mime_type": content_type, "encoding": "base64", "size": response.body.len(), "content": base64::engine::general_purpose::STANDARD.encode(&response.body) })
            }
        })
    }

    async fn list_shared_drives(
        &self,
        input: ListSharedDrives,
    ) -> Result<Page<Value>, SourceError> {
        let (items, next) = self
            .client
            .page(PageCall {
                operation: "list_shared_drives",
                path: "/drives",
                query: Vec::new(),
                limit: input.limit,
                default_limit: 50,
                max_limit: 100,
                cursor: input.cursor.as_deref(),
                items_key: "drives",
            })
            .await?;
        Ok(Page::new(items, next))
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

/// Escape a value for a Drive query string literal.
fn escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('\'', "\\'")
}

fn validate_id(id: &str) -> Result<&str, SourceError> {
    if id.is_empty() || id.contains('/') || id.contains('?') {
        return Err(SourceError::InvalidInput(format!("invalid file id {id:?}")));
    }
    Ok(id)
}

fn operations() -> Vec<OperationSpec> {
    vec![
        OperationSpec::read::<ListFiles, Page<Value>>(
            "list_files",
            "List files",
            "List files, most recently modified first, with an optional Drive query or folder.",
        ),
        OperationSpec::read::<GetFile, Value>("get_file", "Get file", "Return a file's metadata."),
        OperationSpec::read::<SearchFiles, Page<Value>>(
            "search_files",
            "Search files",
            "Full-text search over file names and contents.",
        ),
        OperationSpec::read::<ExportFile, Value>(
            "export_file",
            "Export file",
            "Export a Google Docs, Sheets or Slides file as text, markdown, html or csv.",
        ),
        OperationSpec::read::<GetFileContent, Value>(
            "get_file_content",
            "Get file content",
            "Download a regular file's content (text, or base64 for binaries, up to 4 MB).",
        ),
        OperationSpec::read::<ListSharedDrives, Page<Value>>(
            "list_shared_drives",
            "List shared drives",
            "List the shared drives the user can see.",
        ),
        OperationSpec::read::<Request, Value>(
            "request",
            "Raw request",
            "GET any Drive API v3 path.",
        ),
    ]
}

#[async_trait]
impl Source for Drive {
    fn id(&self) -> &str {
        &self.id
    }

    fn kind(&self) -> &'static str {
        "gdrive"
    }

    fn description(&self) -> &str {
        "Google Drive files, folders, exports and search"
    }

    fn operations(&self) -> &[OperationSpec] {
        &self.operations
    }

    fn search(&self) -> Option<&dyn SearchProvider> {
        Some(self)
    }

    async fn call(
        &self,
        operation: &str,
        input: Value,
        _ctx: &CallContext,
    ) -> Result<Value, SourceError> {
        match operation {
            "list_files" => typed(input, |i: ListFiles| self.list_files(i)).await,
            "get_file" => typed(input, |i: GetFile| self.get_file(i)).await,
            "search_files" => typed(input, |i: SearchFiles| self.search_files(i)).await,
            "export_file" => typed(input, |i: ExportFile| self.export_file(i)).await,
            "get_file_content" => typed(input, |i: GetFileContent| self.get_file_content(i)).await,
            "list_shared_drives" => {
                typed(input, |i: ListSharedDrives| self.list_shared_drives(i)).await
            }
            "request" => typed(input, |i: Request| self.request(i)).await,
            _ => Err(SourceError::UnknownOperation(operation.to_owned())),
        }
    }

    async fn check(&self, _ctx: &CallContext) -> Result<Status, SourceError> {
        let access = self.client.access().await?;
        let (about, _) = self
            .client
            .get(
                "/about",
                &[("fields", "user(displayName,emailAddress)".into())],
            )
            .await?;
        let user = about.get("user");
        let identity = user
            .and_then(|u| u.get("emailAddress"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        Ok(Status {
            identity,
            token_kind: Some("oauth".into()),
            scopes: vec![super::DRIVE_SCOPE.into()],
            credential: Some(access.provenance),
            warnings: Vec::new(),
            unavailable: Vec::new(),
        })
    }
}

/// A Drive file as a normalized document.
pub fn file_item(source: &str, file: &Value) -> Option<Item> {
    let id = file.get("id")?.as_str()?;
    let mime = file.get("mimeType").and_then(Value::as_str).unwrap_or("");
    let owner = file
        .get("owners")
        .and_then(Value::as_array)
        .and_then(|o| o.first())
        .and_then(|o| o.get("displayName"))
        .and_then(Value::as_str);
    let fetch = if mime == "application/vnd.google-apps.folder" {
        OperationRef {
            source: source.to_owned(),
            operation: "list_files".into(),
            input: json!({ "folder": id }),
        }
    } else if mime.starts_with("application/vnd.google-apps.") {
        OperationRef {
            source: source.to_owned(),
            operation: "export_file".into(),
            input: json!({ "file": id }),
        }
    } else {
        OperationRef {
            source: source.to_owned(),
            operation: "get_file_content".into(),
            input: json!({ "file": id }),
        }
    };
    Some(Item {
        kind: ItemKind::Document,
        source: source.to_owned(),
        id: id.to_owned(),
        title: file.get("name").and_then(Value::as_str).map(str::to_owned),
        snippet: normalized::snippet(
            &format!(
                "{mime}{}",
                owner.map(|o| format!(", owned by {o}")).unwrap_or_default()
            ),
            200,
        ),
        url: file
            .get("webViewLink")
            .and_then(Value::as_str)
            .map(str::to_owned),
        author: owner.map(str::to_owned),
        updated_at: file.get("modifiedTime").and_then(normalized::parse_time),
        fetch: Some(fetch),
        raw: Some(file.clone()),
    })
}

#[async_trait]
impl SearchProvider for Drive {
    async fn search(
        &self,
        query: &SearchQuery,
        _ctx: &CallContext,
    ) -> Result<Vec<Item>, SourceError> {
        if query
            .kinds
            .as_ref()
            .is_some_and(|kinds| !kinds.contains(&ItemKind::Document))
        {
            return Ok(Vec::new());
        }
        let page = self
            .search_files(SearchFiles {
                query: query.query.clone(),
                limit: Some(query.effective_limit() as u32),
                cursor: None,
            })
            .await?;
        Ok(page
            .items
            .iter()
            .filter_map(|f| file_item(&self.id, f))
            .collect())
    }
}
