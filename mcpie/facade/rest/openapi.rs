//! The OpenAPI 3.1 document, assembled from the registry. Schema objects in 3.1 are JSON Schema
//! 2020-12, so the inlined schemars output embeds unchanged.

use serde_json::{Map, Value, json};

use super::super::server::AppState;
use crate::model::projection::{FieldKind, project};
use crate::model::{Item, SearchQuery, SearchResult, schema_of};

pub const DOCS_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>mcpie API</title>
</head>
<body>
  <script id="api-reference" data-url="/openapi.json"></script>
  <script src="https://cdn.jsdelivr.net/npm/@scalar/api-reference"></script>
</body>
</html>
"#;

pub fn document(state: &AppState) -> Value {
    let mut paths = Map::new();
    paths.insert("/healthz".into(), json!({ "get": { "operationId": "healthz", "summary": "Liveness", "tags": ["mcpie"], "responses": { "200": { "description": "OK", "content": { "application/json": { "schema": { "type": "object" } } } } } } }));
    paths.insert("/v1/sources".into(), json!({ "get": { "operationId": "list_sources", "summary": "List exposed sources and their operations", "tags": ["mcpie"],
        "responses": { "200": { "description": "Sources", "content": { "application/json": { "schema": { "type": "array", "items": { "$ref": "#/components/schemas/SourceSummary" } } } } } } } }));
    paths.insert("/v1/sources/{source}".into(), json!({ "get": { "operationId": "get_source", "summary": "Describe a source with full operation schemas", "tags": ["mcpie"],
        "parameters": [{ "name": "source", "in": "path", "required": true, "schema": { "type": "string" } }],
        "responses": { "200": { "description": "Source", "content": { "application/json": { "schema": { "type": "object" } } } }, "404": error_ref() } } }));
    for source in &state.exposed {
        for spec in &source.operations {
            let mut item = Map::new();
            let responses = json!({
                "200": { "description": "Result", "content": { "application/json": { "schema": spec.output_schema } } },
                "400": error_ref(), "404": error_ref(), "429": error_ref(), "502": error_ref(), "503": error_ref()
            });
            item.insert("post".into(), json!({
                "operationId": format!("{}_{}", source.id, spec.name),
                "summary": spec.title,
                "description": spec.description,
                "tags": [source.id],
                "requestBody": { "required": false, "content": { "application/json": { "schema": spec.input_schema } } },
                "responses": responses
            }));
            let fields = project(&spec.input_schema);
            if fields.iter().all(|f| !matches!(f.kind, FieldKind::Json)) {
                let parameters: Vec<Value> = fields
                    .iter()
                    .map(|f| {
                        let schema = match &f.kind {
                            FieldKind::List(inner) => json!({ "type": "array", "items": scalar_schema(inner) }),
                            kind => scalar_schema(kind),
                        };
                        let mut parameter = json!({ "name": f.name, "in": "query", "required": f.required, "schema": schema });
                        if let Some(description) = &f.description {
                            parameter["description"] = Value::String(description.clone());
                        }
                        if matches!(f.kind, FieldKind::List(_)) {
                            parameter["style"] = Value::String("form".into());
                            parameter["explode"] = Value::Bool(true);
                        }
                        parameter
                    })
                    .collect();
                item.insert(
                    "get".into(),
                    json!({
                        "operationId": format!("{}_{}_get", source.id, spec.name),
                        "summary": format!("{} (query parameters)", spec.title),
                        "description": spec.description,
                        "tags": [source.id],
                        "parameters": parameters,
                        "responses": responses
                    }),
                );
            }
            paths.insert(
                format!("/v1/sources/{}/{}", source.id, spec.name),
                Value::Object(item),
            );
        }
    }
    if state.searchable() {
        let search_responses = json!({ "200": { "description": "Merged items newest first, with per-source failures", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/SearchResult" } } } }, "400": error_ref() });
        paths.insert("/v1/search".into(), json!({
            "get": { "operationId": "search_get", "summary": "Search every source", "tags": ["mcpie"],
                "parameters": [
                    { "name": "query", "in": "query", "required": true, "schema": { "type": "string" } },
                    { "name": "sources", "in": "query", "schema": { "type": "string" }, "description": "Comma-separated source ids" },
                    { "name": "kinds", "in": "query", "schema": { "type": "string" }, "description": "Comma-separated: message, issue, pull_request, document, person" },
                    { "name": "limit", "in": "query", "schema": { "type": "integer", "maximum": 100 } },
                    { "name": "raw", "in": "query", "schema": { "type": "boolean" } }
                ],
                "responses": search_responses },
            "post": { "operationId": "search_post", "summary": "Search every source", "tags": ["mcpie"],
                "requestBody": { "required": true, "content": { "application/json": { "schema": { "$ref": "#/components/schemas/SearchQuery" } } } },
                "responses": search_responses }
        }));
    }
    paths.insert("/mcp".into(), json!({ "post": { "operationId": "mcp", "summary": "MCP streamable HTTP endpoint (JSON-RPC)", "tags": ["mcpie"], "responses": { "200": { "description": "JSON-RPC response or SSE stream" } } } }));

    let mut schemas = Map::new();
    schemas.insert("Error".into(), json!({ "type": "object", "required": ["error"], "properties": { "error": { "type": "object", "required": ["code", "message"], "properties": {
        "code": { "type": "string", "enum": ["invalid_input", "unknown_operation", "not_configured", "upstream_auth", "rate_limited", "not_found", "upstream", "timeout", "transport", "unsupported", "internal", "unauthorized", "misdirected"] },
        "message": { "type": "string" }, "source": { "type": "string" }, "retry_after": { "type": "integer" }, "request_id": { "type": "string" } } } } }));
    schemas.insert("SourceSummary".into(), json!({ "type": "object", "properties": { "id": { "type": "string" }, "type": { "type": "string" }, "description": { "type": "string" }, "searchable": { "type": "boolean" }, "operations": { "type": "array", "items": { "type": "string" } } } }));
    schemas.insert("SearchQuery".into(), schema_of::<SearchQuery>());
    schemas.insert("SearchResult".into(), schema_of::<SearchResult>());
    schemas.insert("Item".into(), schema_of::<Item>());

    let mut components = json!({ "schemas": schemas });
    let mut document = json!({
        "openapi": "3.1.0",
        "info": { "title": "mcpie", "version": crate::VERSION, "description": "Read-only operations of the configured sources. Every list operation accepts limit and cursor and returns {items, next_cursor}." },
        "servers": [{ "url": "/" }],
        "tags": std::iter::once(json!({ "name": "mcpie" })).chain(state.exposed.iter().map(|s| json!({ "name": s.id, "description": s.description }))).collect::<Vec<_>>(),
        "paths": paths
    });
    if state.token.is_some() {
        components["securitySchemes"] =
            json!({ "bearerAuth": { "type": "http", "scheme": "bearer" } });
        document["security"] = json!([{ "bearerAuth": [] }]);
    }
    document["components"] = components;
    document
}

fn scalar_schema(kind: &FieldKind) -> Value {
    match kind {
        FieldKind::Str => json!({ "type": "string" }),
        FieldKind::I64 => json!({ "type": "integer" }),
        FieldKind::F64 => json!({ "type": "number" }),
        FieldKind::Bool => json!({ "type": "boolean" }),
        FieldKind::Enum(variants) => {
            json!({ "type": "string", "enum": variants.iter().map(|v| v.value.clone()).collect::<Vec<_>>() })
        }
        FieldKind::List(inner) => json!({ "type": "array", "items": scalar_schema(inner) }),
        FieldKind::Json => json!({ "type": "object" }),
    }
}

fn error_ref() -> Value {
    json!({ "description": "Error", "content": { "application/json": { "schema": { "$ref": "#/components/schemas/Error" } } } })
}
