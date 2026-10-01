mod common;

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::FakeSource;
use mcpie::facade::server::{AppState, app, body_json};
use mcpie::model::{Registry, Selection, SourceOptions};
use serde_json::{Value, json};
use tower::ServiceExt;

fn state() -> Arc<AppState> {
    let mut registry = Registry::new();
    registry
        .register(Arc::new(FakeSource::new("alpha")), SourceOptions::default())
        .unwrap();
    registry
        .register(
            Arc::new(FakeSource::with_things("beta", 3).failing_search()),
            SourceOptions::default(),
        )
        .unwrap();
    AppState::new(
        Arc::new(registry),
        &Selection::default(),
        Duration::from_secs(5),
        None,
        vec!["localhost".into()],
    )
    .unwrap()
}

async fn graphql(query: &str) -> Value {
    let body = json!({ "query": query }).to_string();
    let request = Request::builder()
        .method("POST")
        .uri("/graphql")
        .header(header::HOST, "localhost")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .unwrap();
    let response = app(state(), None).unwrap().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    body_json(response).await
}

#[tokio::test]
async fn operations_are_fields_with_typed_arguments() {
    let data = graphql(r#"{ alpha { echo(text: "hi", times: 2, loud: true, tags: ["a", "b"], mode: "markdown") } }"#).await;
    assert_eq!(data["errors"], Value::Null, "{data}");
    assert_eq!(
        data["data"]["alpha"]["echo"],
        json!({ "text": "HIHI", "tags": ["a", "b"], "mode": "markdown" })
    );
    let page = graphql(r#"{ beta { listThings(limit: 2) } }"#).await;
    assert_eq!(
        page["data"]["beta"]["listThings"]["items"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let cursor = page["data"]["beta"]["listThings"]["next_cursor"]
        .as_str()
        .unwrap()
        .to_owned();
    let next = graphql(&format!(
        r#"{{ beta {{ listThings(limit: 2, cursor: "{cursor}") }} }}"#
    ))
    .await;
    assert_eq!(next["data"]["beta"]["listThings"]["items"][0]["n"], 2);
}

#[tokio::test]
async fn errors_carry_codes_and_validation_is_typed() {
    let data = graphql(r#"{ alpha { echo(text: "hi", mode: "loud") } }"#).await;
    assert_eq!(
        data["errors"][0]["extensions"]["code"], "invalid_input",
        "{data}"
    );
    assert_eq!(data["errors"][0]["extensions"]["source"], "alpha");
    let data = graphql(r#"{ alpha { echo(text: "hi", times: "two") } }"#).await;
    assert!(
        data["errors"][0]["message"]
            .as_str()
            .unwrap()
            .contains("Int"),
        "graphql rejects the wrong scalar: {data}"
    );
    let data = graphql(r#"{ alpha { echo } }"#).await;
    assert!(
        data["errors"][0]["message"]
            .as_str()
            .unwrap()
            .contains("text"),
        "required arguments are non-null: {data}"
    );
    let data = graphql(r#"{ alpha { writeThing(name: "x") } }"#).await;
    assert!(
        data["errors"][0]["message"]
            .as_str()
            .unwrap()
            .contains("writeThing"),
        "write operations are not fields: {data}"
    );
}

#[tokio::test]
async fn search_and_introspection() {
    let data =
        graphql(r#"{ search(query: "thing-1", limit: 2, kinds: ["issue", "document"]) }"#).await;
    assert_eq!(
        data["data"]["search"]["items"].as_array().unwrap().len(),
        2,
        "{data}"
    );
    assert_eq!(data["data"]["search"]["errors"][0]["source"], "beta");
    let data = graphql(r#"{ sources }"#).await;
    assert_eq!(data["data"]["sources"][0]["id"], "alpha");
    let data = graphql(r#"{ __type(name: "AlphaSource") { fields { name args { name type { kind ofType { name } } } } } }"#).await;
    let fields = data["data"]["__type"]["fields"].as_array().unwrap();
    let echo = fields.iter().find(|f| f["name"] == "echo").unwrap();
    let text = echo["args"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["name"] == "text")
        .unwrap();
    assert_eq!(text["type"]["kind"], "NON_NULL");
    assert_eq!(text["type"]["ofType"]["name"], "String");
    let request = Request::builder()
        .uri("/graphql")
        .header(header::HOST, "localhost")
        .body(Body::empty())
        .unwrap();
    let response = app(state(), None).unwrap().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("text/html")
    );
}
