//! Any MCP server as a source: mcpie connects as a client, discovers the server's tools once
//! at startup, and exposes them like every other operation.
//!
//! Tools annotated `readOnlyHint: true` are read operations. Everything else is treated as a
//! write and stays hidden unless named in `trusted_read_only`, because an unannotated tool
//! could mutate state.

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use async_trait::async_trait;
use globset::{Glob, GlobSet, GlobSetBuilder};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, ContentBlock, Tool};
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::{StreamableHttpClientTransport, TokioChildProcess};
use serde::Deserialize;
use serde_json::{Map, Value};

use super::{BuildError, Settings};
use crate::config::{CredentialSpec, SourceConfig, resolve};
use crate::model::projection::lint;
use crate::model::{CallContext, OperationKind, OperationSpec, Source, SourceError, Status};

/// Type-specific settings under `[sources.<id>]` for `type = "mcp"`.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Extra {
    /// Command and arguments of a stdio server, e.g. `["npx", "-y", "@notionhq/notion-mcp-server"]`.
    pub command: Vec<String>,
    /// Extra environment variables for the command.
    pub env: BTreeMap<String, String>,
    /// Working directory for the command.
    pub cwd: Option<String>,
    /// URL of a streamable HTTP server, e.g. `https://mcp.example.com/mcp`.
    pub url: Option<String>,
    /// Extra HTTP headers; `token`/`token_command` become `Authorization: Bearer`.
    pub headers: BTreeMap<String, String>,
    /// Tool name globs to treat as read-only even without a `readOnlyHint`.
    pub trusted_read_only: Vec<String>,
    /// Seconds to wait for the server to start and list its tools (default 15).
    pub connect_timeout_seconds: Option<u64>,
}

enum State {
    Connected {
        service: RunningService<RoleClient, ()>,
        transport: String,
    },
    Unavailable(String),
}

pub struct McpSource {
    id: String,
    description: String,
    operations: Vec<OperationSpec>,
    /// Operation name to the server's tool name.
    remote_names: HashMap<String, String>,
    state: State,
}

impl McpSource {
    /// Connect and discover tools. Configuration mistakes fail the build; a server that cannot
    /// be reached yields a source with no operations whose `check` explains why.
    pub async fn connect(settings: Settings, config: &SourceConfig) -> Result<Self, BuildError> {
        let extra: Extra = config
            .extra()
            .map_err(|e| BuildError::source(&settings.id, e))?;
        let transport = match (extra.command.is_empty(), &extra.url) {
            (false, None) => format!("stdio: {}", extra.command.join(" ")),
            (true, Some(url)) => format!("http: {url}"),
            (false, Some(_)) => {
                return Err(BuildError::source(
                    &settings.id,
                    "set either command or url, not both",
                ));
            }
            (true, None) => {
                return Err(BuildError::source(
                    &settings.id,
                    "set command (stdio server) or url (streamable http server)",
                ));
            }
        };
        let trusted =
            globs(&extra.trusted_read_only).map_err(|e| BuildError::source(&settings.id, e))?;
        let timeout = Duration::from_secs(extra.connect_timeout_seconds.unwrap_or(15).max(1));
        let outcome = tokio::time::timeout(timeout, async {
            let service = start(&extra, config).await?;
            let tools = service
                .peer()
                .list_all_tools()
                .await
                .map_err(|e| format!("cannot list tools: {e}"))?;
            Ok::<_, String>((service, tools))
        })
        .await;
        match outcome {
            Ok(Ok((service, tools))) => Ok(Self::from_service(
                &settings.id,
                service,
                tools,
                &trusted,
                transport,
            )),
            Ok(Err(error)) => {
                tracing::warn!(source = %settings.id, %error, "mcp server unavailable");
                Ok(Self::unavailable(
                    &settings.id,
                    format!("{transport}: {error}"),
                ))
            }
            Err(_) => {
                tracing::warn!(source = %settings.id, ?timeout, "mcp server did not answer in time");
                Ok(Self::unavailable(
                    &settings.id,
                    format!("{transport}: no answer within {timeout:?}"),
                ))
            }
        }
    }

    /// Wrap an already connected client (used by tests and by `connect`).
    pub fn from_service(
        id: &str,
        service: RunningService<RoleClient, ()>,
        tools: Vec<Tool>,
        trusted: &GlobSet,
        transport: String,
    ) -> Self {
        let server = service
            .peer_info()
            .and_then(|info| {
                info.server_info
                    .as_ref()
                    .map(|s| format!("{} {}", s.name, s.version))
            })
            .unwrap_or_else(|| "unknown server".into());
        let (operations, remote_names) = map_tools(id, tools, trusted);
        Self {
            id: id.to_owned(),
            description: format!("MCP server {server} ({transport})"),
            operations,
            remote_names,
            state: State::Connected { service, transport },
        }
    }

    fn unavailable(id: &str, reason: String) -> Self {
        Self {
            id: id.to_owned(),
            description: format!("MCP server (unavailable: {reason})"),
            operations: Vec::new(),
            remote_names: HashMap::new(),
            state: State::Unavailable(reason),
        }
    }
}

async fn start(
    extra: &Extra,
    config: &SourceConfig,
) -> Result<RunningService<RoleClient, ()>, String> {
    if !extra.command.is_empty() {
        let mut command = tokio::process::Command::new(&extra.command[0]);
        command.args(&extra.command[1..]).envs(&extra.env);
        if let Some(cwd) = &extra.cwd {
            command.current_dir(cwd);
        }
        let transport = TokioChildProcess::builder(command)
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| format!("cannot start {}: {e}", extra.command[0]))?
            .0;
        return ().serve(transport).await.map_err(|e| format!("mcp handshake failed: {e}"));
    }
    let url = extra.url.clone().unwrap_or_default();
    crate::sources::http::install_crypto_provider();
    let client = reqwest::Client::builder()
        .use_rustls_tls()
        .build()
        .map_err(|e| e.to_string())?;
    let mut settings = StreamableHttpClientTransportConfig::with_uri(url);
    let credential = resolve(CredentialSpec {
        token: config.token.as_ref(),
        token_command: config.token_command.as_deref(),
        env: &[],
        gh_cli: false,
    })
    .await
    .map_err(|e| e.to_string())?;
    if let Some(credential) = credential {
        settings = settings.auth_header(format!("Bearer {}", credential.secret.expose()));
    }
    if !extra.headers.is_empty() {
        let mut headers = HashMap::new();
        for (name, value) in &extra.headers {
            let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
                .map_err(|e| format!("header {name}: {e}"))?;
            let value = reqwest::header::HeaderValue::from_str(value)
                .map_err(|e| format!("header {name}: {e}"))?;
            headers.insert(name, value);
        }
        settings = settings.custom_headers(headers);
    }
    let transport = StreamableHttpClientTransport::with_client(client, settings);
    ().serve(transport)
        .await
        .map_err(|e| format!("mcp handshake failed: {e}"))
}

fn globs(patterns: &[String]) -> Result<GlobSet, String> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        builder.add(Glob::new(pattern).map_err(|e| format!("trusted_read_only {pattern:?}: {e}"))?);
    }
    builder.build().map_err(|e| e.to_string())
}

/// Turn remote tools into operation specs. Tools that cannot be exposed consistently are
/// skipped with a warning rather than failing the whole source.
pub fn map_tools(
    source_id: &str,
    tools: Vec<Tool>,
    trusted: &GlobSet,
) -> (Vec<OperationSpec>, HashMap<String, String>) {
    let mut operations = Vec::new();
    let mut remote_names = HashMap::new();
    for tool in tools {
        let remote = tool.name.to_string();
        let mut name = normalize_name(&remote);
        let mut suffix = 2;
        while remote_names.contains_key(&name) {
            name = format!("{}_{suffix}", normalize_name(&remote));
            suffix += 1;
        }
        let read_only = tool.annotations.as_ref().and_then(|a| a.read_only_hint) == Some(true)
            || trusted.is_match(&remote)
            || trusted.is_match(&name);
        let kind = if read_only {
            OperationKind::Read
        } else {
            OperationKind::Write
        };
        let mut input_schema = Value::Object(tool.input_schema.as_ref().clone());
        if let Some(object) = input_schema.as_object_mut()
            && !object.contains_key("type")
        {
            object.insert("type".into(), Value::String("object".into()));
        }
        let output_schema = tool
            .output_schema
            .as_ref()
            .map(|s| Value::Object(s.as_ref().clone()))
            .unwrap_or(Value::Bool(true));
        let title = tool
            .title
            .clone()
            .unwrap_or_else(|| remote.replace(['_', '-'], " "));
        let description = tool.description.as_deref().unwrap_or("").to_owned();
        let spec = OperationSpec::raw(
            &name,
            &title,
            &description,
            kind,
            input_schema,
            output_schema,
        );
        let problems = lint(&spec);
        if !problems.is_empty() {
            tracing::warn!(source = %source_id, tool = %remote, problems = ?problems, "skipping tool that cannot be exposed");
            continue;
        }
        remote_names.insert(name, remote);
        operations.push(spec);
    }
    (operations, remote_names)
}

/// Lower-case, `[a-z0-9_]` only, starting with a letter.
pub fn normalize_name(tool: &str) -> String {
    let mut out = String::with_capacity(tool.len());
    let mut last_underscore = false;
    for c in tool.chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            out.push(c);
            last_underscore = false;
        } else if !last_underscore && !out.is_empty() {
            out.push('_');
            last_underscore = true;
        }
    }
    let trimmed = out.trim_end_matches('_').to_owned();
    if trimmed
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_lowercase())
    {
        trimmed
    } else {
        format!("t_{trimmed}")
    }
}

fn content_to_value(content: Vec<ContentBlock>) -> Value {
    let mut texts = Vec::new();
    let mut others = Vec::new();
    for block in content {
        match block {
            ContentBlock::Text(text) => texts.push(text.text),
            other => others.push(serde_json::to_value(other).unwrap_or(Value::Null)),
        }
    }
    if others.is_empty() {
        let joined = texts.join("\n");
        return serde_json::from_str(&joined).unwrap_or(Value::String(joined));
    }
    let mut object = Map::new();
    if !texts.is_empty() {
        object.insert("text".into(), Value::String(texts.join("\n")));
    }
    object.insert("content".into(), Value::Array(others));
    Value::Object(object)
}

#[async_trait]
impl Source for McpSource {
    fn id(&self) -> &str {
        &self.id
    }

    fn kind(&self) -> &'static str {
        "mcp"
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn operations(&self) -> &[OperationSpec] {
        &self.operations
    }

    async fn call(
        &self,
        operation: &str,
        input: Value,
        ctx: &CallContext,
    ) -> Result<Value, SourceError> {
        let State::Connected { service, .. } = &self.state else {
            return Err(SourceError::NotConfigured(format!(
                "{}: mcp server unavailable",
                self.id
            )));
        };
        let remote = self
            .remote_names
            .get(operation)
            .ok_or_else(|| SourceError::UnknownOperation(operation.to_owned()))?;
        let mut params = CallToolRequestParams::new(remote.clone());
        params.arguments = match input {
            Value::Object(map) => Some(map),
            Value::Null => None,
            other => {
                return Err(SourceError::InvalidInput(format!(
                    "input must be an object, got {other}"
                )));
            }
        };
        let result = tokio::time::timeout(ctx.timeout, service.peer().call_tool(params))
            .await
            .map_err(|_| SourceError::Timeout(ctx.timeout))?
            .map_err(|e| match e {
                rmcp::ServiceError::McpError(data) => {
                    SourceError::InvalidInput(data.message.to_string())
                }
                other => SourceError::Transport(other.to_string()),
            })?;
        if result.is_error == Some(true) {
            let message = content_to_value(result.content);
            let text = message
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| message.to_string());
            return Err(SourceError::Upstream {
                status: 200,
                message: text,
            });
        }
        Ok(match result.structured_content {
            Some(structured) => structured,
            None => content_to_value(result.content),
        })
    }

    async fn check(&self, _ctx: &CallContext) -> Result<Status, SourceError> {
        match &self.state {
            State::Connected { service, transport } => {
                let info = service.peer_info();
                let identity = info
                    .as_ref()
                    .and_then(|i| i.server_info.as_ref())
                    .map(|s| format!("{} {}", s.name, s.version));
                let hidden: Vec<String> = self
                    .operations
                    .iter()
                    .filter(|o| !o.is_read())
                    .map(|o| o.name.clone())
                    .collect();
                let warnings = if hidden.is_empty() {
                    Vec::new()
                } else {
                    vec![format!(
                        "{} tool(s) without readOnlyHint are hidden; list them in trusted_read_only to expose them",
                        hidden.len()
                    )]
                };
                Ok(Status {
                    identity,
                    token_kind: None,
                    scopes: Vec::new(),
                    credential: Some(transport.clone()),
                    warnings,
                    unavailable: hidden,
                })
            }
            State::Unavailable(reason) => Err(SourceError::Transport(reason.clone())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_tool_names() {
        assert_eq!(normalize_name("search_pages"), "search_pages");
        assert_eq!(normalize_name("Search-Pages"), "search_pages");
        assert_eq!(normalize_name("API.v2::Call!"), "api_v2_call");
        assert_eq!(normalize_name("2fast"), "t_2fast");
    }

    #[test]
    fn folds_content_blocks() {
        let value = content_to_value(vec![ContentBlock::text("{\"a\":1}")]);
        assert_eq!(value, serde_json::json!({ "a": 1 }));
        let value = content_to_value(vec![
            ContentBlock::text("plain"),
            ContentBlock::text("text"),
        ]);
        assert_eq!(value, Value::String("plain\ntext".into()));
    }
}
