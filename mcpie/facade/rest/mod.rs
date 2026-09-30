//! The REST facade: `/v1/sources`, `/v1/sources/{source}/{operation}`, `/v1/search`, plus the
//! OpenAPI document and a docs page. Errors use the shared code table.

pub mod openapi;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Extension, Path, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use serde::Serialize;
use serde_json::{Map, Value, json};

use super::server::{AppState, RequestId};
use crate::model::projection::{FieldKind, coerce, coerce_list, project};
use crate::model::{CallContext, ItemKind, SearchQuery, SourceError};

pub fn routes(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/v1/sources", get(list_sources))
        .route("/v1/sources/{source}", get(get_source))
        .route(
            "/v1/sources/{source}/{operation}",
            get(call_get).post(call_post),
        )
        .route("/v1/search", get(search_get).post(search_post))
        .route("/openapi.json", get(openapi_json))
        .route("/docs", get(docs))
        .with_state(state)
}

#[derive(Serialize)]
struct SourceSummary<'a> {
    id: &'a str,
    #[serde(rename = "type")]
    kind: &'a str,
    description: &'a str,
    searchable: bool,
    operations: Vec<&'a str>,
}

async fn healthz() -> Response {
    json_response(
        StatusCode::OK,
        &json!({ "ok": true, "name": crate::NAME, "version": crate::VERSION }),
    )
}

async fn list_sources(State(state): State<Arc<AppState>>) -> Response {
    let sources: Vec<SourceSummary<'_>> = state
        .exposed
        .iter()
        .map(|s| SourceSummary {
            id: &s.id,
            kind: &s.kind,
            description: &s.description,
            searchable: s.searchable,
            operations: s.operations.iter().map(|o| o.name.as_str()).collect(),
        })
        .collect();
    json_response(StatusCode::OK, &sources)
}

async fn get_source(
    State(state): State<Arc<AppState>>,
    Path(source): Path<String>,
    Extension(request_id): Extension<RequestId>,
) -> Response {
    match state.source(&source) {
        Some(exposed) => json_response(
            StatusCode::OK,
            &json!({ "id": exposed.id, "type": exposed.kind, "description": exposed.description, "searchable": exposed.searchable, "operations": exposed.operations }),
        ),
        None => ApiError::from_source(SourceError::UnknownSource(source), None, request_id.0)
            .into_response(),
    }
}

async fn call_post(
    State(state): State<Arc<AppState>>,
    Path((source, operation)): Path<(String, String)>,
    Extension(request_id): Extension<RequestId>,
    body: Bytes,
) -> Response {
    let input = if body.iter().all(u8::is_ascii_whitespace) {
        Value::Object(Map::new())
    } else {
        match serde_json::from_slice::<Value>(&body) {
            Ok(Value::Object(map)) => Value::Object(map),
            Ok(_) => {
                return ApiError::invalid(
                    "the request body must be a JSON object",
                    &source,
                    request_id.0,
                )
                .into_response();
            }
            Err(error) => {
                return ApiError::invalid(
                    &format!("invalid JSON body: {error}"),
                    &source,
                    request_id.0,
                )
                .into_response();
            }
        }
    };
    call(&state, &source, &operation, input, request_id.0).await
}

async fn call_get(
    State(state): State<Arc<AppState>>,
    Path((source, operation)): Path<(String, String)>,
    Extension(request_id): Extension<RequestId>,
    Query(pairs): Query<Vec<(String, String)>>,
) -> Response {
    let Some(spec) = state.spec(&source, &operation) else {
        return ApiError::from_source(
            SourceError::UnknownOperation(format!("{source}.{operation}")),
            Some(&source),
            request_id.0,
        )
        .into_response();
    };
    match input_from_query(&spec.input_schema, &pairs) {
        Ok(input) => {
            call(
                &state,
                &source,
                &operation,
                Value::Object(input),
                request_id.0,
            )
            .await
        }
        Err(message) => ApiError::invalid(&message, &source, request_id.0).into_response(),
    }
}

async fn call(
    state: &AppState,
    source: &str,
    operation: &str,
    input: Value,
    request_id: String,
) -> Response {
    if state.spec(source, operation).is_none() {
        return ApiError::from_source(
            SourceError::UnknownOperation(format!("{source}.{operation}")),
            Some(source),
            request_id,
        )
        .into_response();
    }
    let ctx = CallContext::new(state.timeout).with_request_id(request_id.clone());
    match state.registry.call(source, operation, input, &ctx).await {
        Ok(value) => json_response(StatusCode::OK, &value),
        Err(error) => ApiError::from_source(error, Some(source), request_id).into_response(),
    }
}

/// Build an input object from query parameters using the projection: scalars are coerced,
/// repeated keys become lists, nested objects are refused (use POST).
pub fn input_from_query(
    input_schema: &Value,
    pairs: &[(String, String)],
) -> Result<Map<String, Value>, String> {
    let fields = project(input_schema);
    let mut grouped: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (key, value) in pairs {
        grouped
            .entry(key.as_str())
            .or_default()
            .push(value.as_str());
    }
    let mut input = Map::new();
    for (key, values) in grouped {
        let Some(field) = fields.iter().find(|f| f.name == key) else {
            return Err(format!("unknown parameter {key:?}"));
        };
        let value = match &field.kind {
            FieldKind::Json => {
                return Err(format!(
                    "parameter {key:?} is an object; send it in a POST body"
                ));
            }
            FieldKind::List(inner) => {
                let raws: Vec<String> = values
                    .iter()
                    .flat_map(|v| v.split(','))
                    .map(str::to_owned)
                    .collect();
                coerce_list(inner, &raws).map_err(|e| format!("{key}: {e}"))?
            }
            kind => coerce(kind, values.last().copied().unwrap_or_default())
                .map_err(|e| format!("{key}: {e}"))?,
        };
        input.insert(key.to_owned(), value);
    }
    Ok(input)
}

fn search_query_from_pairs(pairs: &[(String, String)]) -> Result<SearchQuery, String> {
    let mut query = SearchQuery::default();
    let mut has_query = false;
    for (key, value) in pairs {
        match key.as_str() {
            "query" | "q" => {
                query.query = value.clone();
                has_query = true;
            }
            "sources" | "source" => query.sources.get_or_insert_with(Vec::new).extend(
                value
                    .split(',')
                    .map(|s| s.trim().to_owned())
                    .filter(|s| !s.is_empty()),
            ),
            "kinds" | "kind" => {
                for kind in value.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                    let parsed: ItemKind = serde_json::from_value(Value::String(kind.to_owned()))
                        .map_err(|_| format!("unknown kind {kind:?}"))?;
                    query.kinds.get_or_insert_with(Vec::new).push(parsed);
                }
            }
            "limit" => {
                query.limit = Some(
                    value
                        .parse()
                        .map_err(|_| format!("limit: expected an integer, got {value:?}"))?,
                )
            }
            "raw" | "include_raw" => {
                query.include_raw = matches!(value.as_str(), "true" | "1" | "yes")
            }
            other => return Err(format!("unknown parameter {other:?}")),
        }
    }
    if !has_query {
        return Err("the query parameter is required".into());
    }
    Ok(query)
}

async fn search_get(
    State(state): State<Arc<AppState>>,
    Extension(request_id): Extension<RequestId>,
    Query(pairs): Query<Vec<(String, String)>>,
) -> Response {
    match search_query_from_pairs(&pairs) {
        Ok(query) => search(&state, query, request_id.0).await,
        Err(message) => ApiError::invalid(&message, "search", request_id.0).into_response(),
    }
}

async fn search_post(
    State(state): State<Arc<AppState>>,
    Extension(request_id): Extension<RequestId>,
    body: Bytes,
) -> Response {
    match serde_json::from_slice::<SearchQuery>(&body) {
        Ok(query) => search(&state, query, request_id.0).await,
        Err(error) => ApiError::invalid(
            &format!("invalid search body: {error}"),
            "search",
            request_id.0,
        )
        .into_response(),
    }
}

async fn search(state: &AppState, query: SearchQuery, request_id: String) -> Response {
    let ctx = CallContext::new(state.timeout).with_request_id(request_id);
    let result = state.registry.search(query, &ctx).await;
    json_response(StatusCode::OK, &result)
}

async fn openapi_json(State(state): State<Arc<AppState>>) -> Response {
    json_response(StatusCode::OK, &state.openapi)
}

async fn docs() -> Html<&'static str> {
    Html(openapi::DOCS_HTML)
}

fn json_response<T: Serialize>(status: StatusCode, value: &T) -> Response {
    match serde_json::to_vec(value) {
        Ok(bytes) => {
            let mut response = (status, bytes).into_response();
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
            response
        }
        Err(error) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            error.to_string(),
            None,
            None,
            None,
        ),
    }
}

/// The error body every endpoint returns.
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
    pub source: Option<String>,
    pub retry_after: Option<Duration>,
    pub request_id: Option<String>,
}

impl ApiError {
    pub fn from_source(error: SourceError, source: Option<&str>, request_id: String) -> Self {
        Self {
            status: StatusCode::from_u16(error.http_status())
                .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            code: error.code(),
            message: error.to_string(),
            source: source.map(str::to_owned),
            retry_after: error.retry_after(),
            request_id: Some(request_id),
        }
    }

    pub fn invalid(message: &str, source: &str, request_id: String) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "invalid_input",
            message: message.to_owned(),
            source: Some(source.to_owned()),
            retry_after: None,
            request_id: Some(request_id),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        error_response(
            self.status,
            self.code,
            self.message,
            self.source,
            self.retry_after,
            self.request_id,
        )
    }
}

pub fn error_response(
    status: StatusCode,
    code: &str,
    message: String,
    source: Option<String>,
    retry_after: Option<Duration>,
    request_id: Option<String>,
) -> Response {
    let mut error = json!({ "code": code, "message": message });
    if let Some(source) = source {
        error["source"] = Value::String(source);
    }
    if let Some(retry_after) = retry_after {
        error["retry_after"] = Value::from(retry_after.as_secs());
    }
    if let Some(request_id) = request_id {
        error["request_id"] = Value::String(request_id);
    }
    let mut response = json_response(status, &json!({ "error": error }));
    if let Some(retry_after) = retry_after
        && let Ok(value) = HeaderValue::from_str(&retry_after.as_secs().to_string())
    {
        response.headers_mut().insert(header::RETRY_AFTER, value);
    }
    response
}
