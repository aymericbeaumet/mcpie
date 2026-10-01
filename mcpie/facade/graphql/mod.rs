//! The GraphQL facade: a schema built at runtime from the registry.
//!
//! `query { github { listIssues(owner: "acme", repo: "widgets") } }`. Each exposed source is an
//! object with one field per read operation; arguments come from the shared projection and
//! results are a `JSON` scalar (typed outputs are a later refinement). `search` sits on the root.

use std::sync::Arc;

use async_graphql::dynamic::{
    Field, FieldFuture, FieldValue, InputValue, Object, ResolverContext, Scalar, Schema, TypeRef,
};
use async_graphql::http::GraphiQLSource;
use async_graphql::{Error as GraphQLError, ErrorExtensions, Value as GqlValue};
use async_graphql_axum::GraphQL;
use axum::Router;
use axum::response::{Html, IntoResponse};
use axum::routing::get;
use serde_json::{Map, Value};

use super::server::AppState;
use crate::model::names::{camel, pascal};
use crate::model::projection::{FieldKind, InputField, project};
use crate::model::{CallContext, SearchQuery, SourceError};

const JSON: &str = "JSON";

/// Build the schema for the sources and operations `state` exposes.
pub fn schema(state: Arc<AppState>) -> Result<Schema, String> {
    let mut builder = Schema::build("Query", None, None).data(state.clone());
    builder = builder.register(
        Scalar::new(JSON)
            .description("Any JSON value: objects, arrays, strings, numbers, booleans or null."),
    );
    let mut query = Object::new("Query")
        .description("Read-only operations of the configured sources, grouped by source.");
    for source in &state.exposed {
        let type_name = format!("{}Source", pascal(&source.id));
        let mut object = Object::new(type_name.clone()).description(source.description.clone());
        for spec in &source.operations {
            let source_id = source.id.clone();
            let operation = spec.name.clone();
            let fields = project(&spec.input_schema);
            let arg_names: Vec<String> = fields.iter().map(|f| f.name.clone()).collect();
            let mut field = Field::new(camel(&spec.name), TypeRef::named_nn(JSON), move |ctx| {
                let source_id = source_id.clone();
                let operation = operation.clone();
                let arg_names = arg_names.clone();
                FieldFuture::new(async move { call(ctx, &source_id, &operation, &arg_names).await })
            })
            .description(
                format!(
                    "{} {}",
                    spec.description,
                    if spec.paginated {
                        "Paginated: pass `cursor` from `next_cursor`."
                    } else {
                        ""
                    }
                )
                .trim()
                .to_owned(),
            );
            for input in &fields {
                field = field.argument(argument(input));
            }
            object = object.field(field);
        }
        let source_id = source.id.clone();
        query = query.field(
            Field::new(
                camel(&source.id),
                TypeRef::named_nn(type_name.clone()),
                move |_ctx| {
                    let source_id = source_id.clone();
                    FieldFuture::new(async move { Ok(Some(FieldValue::owned_any(source_id))) })
                },
            )
            .description(source.description.clone()),
        );
        builder = builder.register(object);
    }
    query = query.field(
        Field::new("sources", TypeRef::named_nn(JSON), |ctx| {
            FieldFuture::new(async move {
                let state = ctx.data::<Arc<AppState>>()?;
                let value: Vec<Value> = state
                    .exposed
                    .iter()
                    .map(|s| serde_json::json!({ "id": s.id, "type": s.kind, "description": s.description, "searchable": s.searchable, "operations": s.operations.iter().map(|o| o.name.clone()).collect::<Vec<_>>() }))
                    .collect();
                Ok(Some(FieldValue::value(GqlValue::from_json(Value::Array(value))?)))
            })
        })
        .description("The exposed sources and their operation names."),
    );
    if state.searchable() {
        query = query.field(
            Field::new("search", TypeRef::named_nn(JSON), |ctx| {
                FieldFuture::new(async move { search(ctx).await })
            })
            .description("Search every source at once; returns {items, errors} newest first.")
            .argument(InputValue::new("query", TypeRef::named_nn(TypeRef::STRING)))
            .argument(
                InputValue::new("sources", TypeRef::named_nn_list(TypeRef::STRING))
                    .description("Restrict to these source ids."),
            )
            .argument(
                InputValue::new("kinds", TypeRef::named_nn_list(TypeRef::STRING))
                    .description("message, issue, pull_request, document or person."),
            )
            .argument(InputValue::new("limit", TypeRef::named(TypeRef::INT)))
            .argument(InputValue::new(
                "includeRaw",
                TypeRef::named(TypeRef::BOOLEAN),
            )),
        );
    }
    builder.register(query).finish().map_err(|e| e.to_string())
}

fn argument(field: &InputField) -> InputValue {
    let base = match &field.kind {
        FieldKind::Str | FieldKind::Enum(_) => TypeRef::named(TypeRef::STRING),
        FieldKind::I64 => TypeRef::named(TypeRef::INT),
        FieldKind::F64 => TypeRef::named(TypeRef::FLOAT),
        FieldKind::Bool => TypeRef::named(TypeRef::BOOLEAN),
        FieldKind::List(inner) => {
            TypeRef::List(Box::new(TypeRef::NonNull(Box::new(match inner.as_ref() {
                FieldKind::I64 => TypeRef::named(TypeRef::INT),
                FieldKind::F64 => TypeRef::named(TypeRef::FLOAT),
                FieldKind::Bool => TypeRef::named(TypeRef::BOOLEAN),
                FieldKind::Json => TypeRef::named(JSON),
                _ => TypeRef::named(TypeRef::STRING),
            }))))
        }
        FieldKind::Json => TypeRef::named(JSON),
    };
    let type_ref = if field.required {
        TypeRef::NonNull(Box::new(base))
    } else {
        base
    };
    let mut description = field.description.clone().unwrap_or_default();
    if let FieldKind::Enum(variants) = &field.kind {
        let values: Vec<&str> = variants.iter().map(|v| v.value.as_str()).collect();
        if !description.is_empty() {
            description.push(' ');
        }
        description.push_str(&format!("One of: {}.", values.join(", ")));
    }
    let mut input = InputValue::new(camel(&field.name), type_ref);
    if !description.is_empty() {
        input = input.description(description);
    }
    input
}

async fn call(
    ctx: ResolverContext<'_>,
    source: &str,
    operation: &str,
    arg_names: &[String],
) -> Result<Option<FieldValue<'static>>, GraphQLError> {
    let state = ctx.data::<Arc<AppState>>()?;
    let mut input = Map::new();
    for name in arg_names {
        if let Some(accessor) = ctx.args.get(&camel(name)) {
            let value = accessor
                .as_value()
                .clone()
                .into_json()
                .map_err(|e| GraphQLError::new(e.to_string()))?;
            if !value.is_null() {
                input.insert(name.clone(), value);
            }
        }
    }
    let call_ctx = CallContext::new(state.timeout);
    match state
        .registry
        .call(source, operation, Value::Object(input), &call_ctx)
        .await
    {
        Ok(value) => Ok(Some(FieldValue::value(GqlValue::from_json(value)?))),
        Err(error) => Err(source_error(error, source)),
    }
}

async fn search(ctx: ResolverContext<'_>) -> Result<Option<FieldValue<'static>>, GraphQLError> {
    let state = ctx.data::<Arc<AppState>>()?;
    let strings = |name: &str| -> Option<Vec<String>> {
        ctx.args.get(name).and_then(|v| v.list().ok()).map(|list| {
            list.iter()
                .filter_map(|v| v.string().ok().map(str::to_owned))
                .collect()
        })
    };
    let kinds = match strings("kinds") {
        Some(kinds) => Some(
            kinds
                .iter()
                .map(|k| {
                    serde_json::from_value(Value::String(k.clone()))
                        .map_err(|_| GraphQLError::new(format!("unknown kind {k:?}")))
                })
                .collect::<Result<Vec<_>, _>>()?,
        ),
        None => None,
    };
    let query = SearchQuery {
        query: ctx.args.try_get("query")?.string()?.to_owned(),
        sources: strings("sources"),
        kinds,
        limit: ctx
            .args
            .get("limit")
            .and_then(|v| v.i64().ok())
            .map(|n| n.max(0) as u32),
        include_raw: ctx
            .args
            .get("includeRaw")
            .and_then(|v| v.boolean().ok())
            .unwrap_or(false),
    };
    let result = state
        .registry
        .search(query, &CallContext::new(state.timeout))
        .await;
    let value = serde_json::to_value(result).map_err(|e| GraphQLError::new(e.to_string()))?;
    Ok(Some(FieldValue::value(GqlValue::from_json(value)?)))
}

fn source_error(error: SourceError, source: &str) -> GraphQLError {
    let code = error.code();
    let retry_after = error.retry_after();
    let source = source.to_owned();
    GraphQLError::new(error.to_string()).extend_with(|_, extensions| {
        extensions.set("code", code);
        extensions.set("source", source.as_str());
        if let Some(retry_after) = retry_after {
            extensions.set("retry_after", retry_after.as_secs());
        }
    })
}

/// `GET /graphql` serves GraphiQL, `POST /graphql` executes queries.
pub fn routes(schema: Schema) -> Router {
    Router::new().route("/graphql", get(graphiql).post_service(GraphQL::new(schema)))
}

async fn graphiql() -> impl IntoResponse {
    Html(
        GraphiQLSource::build()
            .endpoint("/graphql")
            .title("mcpie")
            .finish(),
    )
}
