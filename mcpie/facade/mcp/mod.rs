//! The MCP facade: the registry as an MCP server.
//!
//! `tools` mode exposes one tool per read operation (`<source>_<operation>`) plus `search`;
//! `meta` mode exposes a fixed four-tool surface for clients with many sources or small
//! context budgets. Both are views over the same registry, decided once at startup: the tool
//! list never changes during a session.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use rmcp::model::CallToolResponse;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ContentBlock, Implementation, JsonObject,
    ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
    ToolAnnotations,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler, ServiceExt};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::model::names::{kebab, mcp_tool_name};
use crate::model::projection::project;
use crate::model::{
    CallContext, OperationSpec, Registry, RegistryError, SearchQuery, SearchResult, Selection,
    SourceError, schema_of,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Tools,
    Meta,
}

const SEARCH: &str = "search";
const LIST_OPERATIONS: &str = "list_operations";
const DESCRIBE_OPERATION: &str = "describe_operation";
const CALL_OPERATION: &str = "call_operation";

const INSTRUCTIONS: &str = "mcpie exposes read-only operations of configured sources (GitHub, Slack, Linear, Google Drive, Gmail, custom MCP servers). \
Tool names are <source>_<operation>. List operations accept `limit` and `cursor` and return {items, next_cursor}: pass next_cursor back to continue. \
`search` queries every source at once and returns short items; each item's `fetch` names the operation and input that return the full object. \
`<source>_request` performs a raw GET against that source's API when no dedicated operation fits.";

/// A description of an operation in `meta` mode listings.
#[derive(Debug, serde::Serialize, JsonSchema)]
pub struct OperationSummary {
    pub source: String,
    pub operation: String,
    pub title: String,
    pub description: String,
    pub paginated: bool,
    /// Input fields, required ones suffixed with `*`.
    pub inputs: Vec<String>,
}

/// List the operations available to `call_operation`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListOperations {
    /// Only this source.
    #[serde(default)]
    pub source: Option<String>,
}

/// Get the full input and output schema of one operation.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DescribeOperation {
    pub source: String,
    pub operation: String,
}

/// Call one operation with a JSON input object.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CallOperation {
    pub source: String,
    pub operation: String,
    /// The operation input; see describe_operation for its schema.
    #[serde(default)]
    pub input: Option<Map<String, Value>>,
}

/// Operations exposed in this session, in stable order.
#[derive(Clone)]
struct ExposedOp {
    source: String,
    spec: OperationSpec,
}

pub struct McpServer {
    registry: Arc<Registry>,
    mode: Mode,
    timeout: Duration,
    tools: Vec<Tool>,
    exposed: Vec<ExposedOp>,
    /// `tools` mode: tool name to (source, operation).
    index: BTreeMap<String, (String, String)>,
    searchable: bool,
}

impl McpServer {
    pub fn new(
        registry: Arc<Registry>,
        selection: &Selection,
        mode: Mode,
        timeout: Duration,
    ) -> Result<Self, RegistryError> {
        let mut exposed = Vec::new();
        let mut searchable = false;
        for entry in registry.select(selection)? {
            searchable |= entry.source.search().is_some();
            for spec in entry.operations {
                exposed.push(ExposedOp {
                    source: entry.source.id().to_owned(),
                    spec: spec.clone(),
                });
            }
        }
        let mut index = BTreeMap::new();
        let tools = match mode {
            Mode::Tools => {
                let mut tools: Vec<Tool> = exposed
                    .iter()
                    .map(|op| {
                        let name = mcp_tool_name(&op.source, &op.spec.name);
                        index.insert(name.clone(), (op.source.clone(), op.spec.name.clone()));
                        tool(
                            name,
                            &format!("{} [{}]", op.spec.description, op.source),
                            &op.spec.title,
                            &op.spec.input_schema,
                            &op.spec.output_schema,
                        )
                    })
                    .collect();
                if searchable {
                    tools.push(search_tool());
                }
                tools
            }
            Mode::Meta => {
                let mut tools = vec![
                    tool(
                        LIST_OPERATIONS.into(),
                        "List the read-only operations available through call_operation, optionally for one source.",
                        "List operations",
                        &schema_of::<ListOperations>(),
                        &Value::Bool(true),
                    ),
                    tool(
                        DESCRIBE_OPERATION.into(),
                        "Return the full input and output JSON schema of one operation.",
                        "Describe operation",
                        &schema_of::<DescribeOperation>(),
                        &Value::Bool(true),
                    ),
                    tool(
                        CALL_OPERATION.into(),
                        "Call one operation with a JSON input object; use describe_operation first for its schema.",
                        "Call operation",
                        &schema_of::<CallOperation>(),
                        &Value::Bool(true),
                    ),
                ];
                if searchable {
                    tools.push(search_tool());
                }
                tools
            }
        };
        Ok(Self {
            registry,
            mode,
            timeout,
            tools,
            exposed,
            index,
            searchable,
        })
    }

    pub fn tools(&self) -> &[Tool] {
        &self.tools
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    fn knows(&self, name: &str) -> bool {
        match self.mode {
            Mode::Tools => self.index.contains_key(name) || (self.searchable && name == SEARCH),
            Mode::Meta => {
                matches!(name, LIST_OPERATIONS | DESCRIBE_OPERATION | CALL_OPERATION)
                    || (self.searchable && name == SEARCH)
            }
        }
    }

    fn find(&self, source: &str, operation: &str) -> Option<&ExposedOp> {
        self.exposed
            .iter()
            .find(|op| op.source == source && op.spec.name == operation)
    }

    async fn handle(
        &self,
        name: &str,
        arguments: Map<String, Value>,
        ctx: &CallContext,
    ) -> Result<Value, SourceError> {
        let arguments = Value::Object(arguments);
        if name == SEARCH && self.searchable {
            let query: SearchQuery =
                serde_json::from_value(arguments).map_err(SourceError::invalid_input)?;
            let result = self.registry.search(query, ctx).await;
            return serde_json::to_value(result).map_err(SourceError::internal);
        }
        match self.mode {
            Mode::Tools => {
                let (source, operation) = self
                    .index
                    .get(name)
                    .ok_or_else(|| SourceError::UnknownOperation(name.to_owned()))?;
                self.registry.call(source, operation, arguments, ctx).await
            }
            Mode::Meta => match name {
                LIST_OPERATIONS => {
                    let input: ListOperations =
                        serde_json::from_value(arguments).map_err(SourceError::invalid_input)?;
                    let operations: Vec<OperationSummary> = self
                        .exposed
                        .iter()
                        .filter(|op| input.source.as_ref().is_none_or(|s| *s == op.source))
                        .map(|op| OperationSummary {
                            source: op.source.clone(),
                            operation: op.spec.name.clone(),
                            title: op.spec.title.clone(),
                            description: op.spec.description.clone(),
                            paginated: op.spec.paginated,
                            inputs: project(&op.spec.input_schema)
                                .iter()
                                .map(|f| {
                                    if f.required {
                                        format!("{}*", f.name)
                                    } else {
                                        f.name.clone()
                                    }
                                })
                                .collect(),
                        })
                        .collect();
                    Ok(serde_json::json!({ "operations": operations }))
                }
                DESCRIBE_OPERATION => {
                    let input: DescribeOperation =
                        serde_json::from_value(arguments).map_err(SourceError::invalid_input)?;
                    let op = self.find(&input.source, &input.operation).ok_or_else(|| {
                        SourceError::UnknownOperation(format!(
                            "{}.{}",
                            input.source, input.operation
                        ))
                    })?;
                    let mut value =
                        serde_json::to_value(&op.spec).map_err(SourceError::internal)?;
                    if let Some(object) = value.as_object_mut() {
                        object.insert("source".into(), Value::String(op.source.clone()));
                        object.insert(
                            "cli".into(),
                            Value::String(format!(
                                "{} {} {}",
                                crate::NAME,
                                op.source,
                                kebab(&op.spec.name)
                            )),
                        );
                    }
                    Ok(value)
                }
                CALL_OPERATION => {
                    let input: CallOperation =
                        serde_json::from_value(arguments).map_err(SourceError::invalid_input)?;
                    self.find(&input.source, &input.operation).ok_or_else(|| {
                        SourceError::UnknownOperation(format!(
                            "{}.{}",
                            input.source, input.operation
                        ))
                    })?;
                    self.registry
                        .call(
                            &input.source,
                            &input.operation,
                            Value::Object(input.input.unwrap_or_default()),
                            ctx,
                        )
                        .await
                }
                other => Err(SourceError::UnknownOperation(other.to_owned())),
            },
        }
    }
}

fn tool(
    name: String,
    description: &str,
    title: &str,
    input_schema: &Value,
    output_schema: &Value,
) -> Tool {
    let input: JsonObject = input_schema.as_object().cloned().unwrap_or_default();
    let mut tool = Tool::new(name, description.to_owned(), Arc::new(input))
        .with_title(title.to_owned())
        .with_annotations(
            ToolAnnotations::new()
                .read_only(true)
                .destructive(false)
                .idempotent(true)
                .open_world(true),
        );
    if output_schema.get("type").and_then(Value::as_str) == Some("object")
        && let Some(output) = output_schema.as_object()
    {
        tool = tool.with_raw_output_schema(Arc::new(output.clone()));
    }
    tool
}

fn search_tool() -> Tool {
    tool(
        SEARCH.into(),
        "Search every configured source at once. Returns short items newest first; each item's `fetch` names the operation that returns the full object.",
        "Search all sources",
        &schema_of::<SearchQuery>(),
        &schema_of::<SearchResult>(),
    )
}

fn success(value: Value) -> CallToolResult {
    let text = serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string());
    let mut result = CallToolResult::success(vec![ContentBlock::text(text)]);
    if value.is_object() {
        result.structured_content = Some(value);
    }
    result
}

impl ServerHandler for McpServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(crate::NAME, crate::VERSION))
            .with_instructions(INSTRUCTIONS)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(self.tools.clone()))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        self.tools.iter().find(|t| t.name == name).cloned()
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let name = request.name.to_string();
        if !self.knows(&name) {
            return Err(ErrorData::invalid_params(
                format!("unknown tool {name}"),
                None,
            ));
        }
        let arguments = request.arguments.unwrap_or_default();
        let ctx = CallContext::new(self.timeout);
        tracing::info!(tool = %name, request_id = %ctx.request_id, "mcp call");
        match self.handle(&name, arguments, &ctx).await {
            Ok(value) => Ok(success(value).into()),
            Err(SourceError::Internal(message)) => Err(ErrorData::internal_error(message, None)),
            Err(error) => Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                "{}: {error}",
                error.code()
            ))])
            .into()),
        }
    }
}

/// Serve over stdin/stdout until the client disconnects. Nothing but protocol frames may be
/// written to stdout while this runs.
pub async fn serve_stdio(server: McpServer) -> Result<(), String> {
    let running = match server.serve(rmcp::transport::stdio()).await {
        Ok(running) => running,
        Err(
            rmcp::service::ServerInitializeError::ConnectionClosed(_)
            | rmcp::service::ServerInitializeError::ExpectedInitializeRequest(None),
        ) => {
            tracing::info!("client disconnected before initializing");
            return Ok(());
        }
        Err(error) => return Err(format!("mcp initialization failed: {error}")),
    };
    running
        .waiting()
        .await
        .map_err(|e| format!("mcp server task failed: {e}"))?;
    Ok(())
}

/// Ids of enabled sources whose credentials are missing, found by probing each source with a
/// short timeout. Used to keep unusable tools out of a session unless the user asks for them.
pub async fn unconfigured_sources(registry: &Registry, ctx: &CallContext) -> Vec<String> {
    let timeout = Duration::from_secs(5).min(ctx.timeout);
    let probes = registry
        .sources()
        .filter(|s| registry.is_enabled(s.id()))
        .map(|source| async move {
            match tokio::time::timeout(timeout, source.check(ctx)).await {
                Ok(Err(SourceError::NotConfigured(_))) => Some(source.id().to_owned()),
                _ => None,
            }
        });
    futures::future::join_all(probes)
        .await
        .into_iter()
        .flatten()
        .collect()
}
