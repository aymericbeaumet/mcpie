mod common;

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::FakeSource;
use mcpie::config::Secret;
use mcpie::facade::server::{AppState, app, body_json};
use mcpie::model::{Registry, Selection, SourceOptions};
use serde_json::{Value, json};
use tower::ServiceExt;

fn state(token: Option<&str>) -> Arc<AppState> {
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
        token.map(Secret::new),
        vec!["localhost".into(), "127.0.0.1".into()],
    )
    .unwrap()
}

async fn send(
    state: Arc<AppState>,
    request: Request<Body>,
) -> (StatusCode, axum::http::HeaderMap, Value) {
    let response = app(state, None).oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    (status, headers, body_json(response).await)
}

fn get(path: &str) -> Request<Body> {
    Request::builder()
        .uri(path)
        .header(header::HOST, "localhost:7878")
        .body(Body::empty())
        .unwrap()
}

fn post(path: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(path)
        .header(header::HOST, "127.0.0.1")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_owned()))
        .unwrap()
}

#[tokio::test]
async fn lists_and_describes_sources() {
    let (status, headers, body) = send(state(None), get("/healthz")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], true);
    assert!(headers.get("x-request-id").is_some());
    let (status, _, body) = send(state(None), get("/v1/sources")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body[0]["id"], "alpha");
    assert_eq!(body[0]["operations"], json!(["echo", "list_things"]));
    let (status, _, body) = send(state(None), get("/v1/sources/beta")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["operations"][0]["input_schema"].is_object());
    let (status, _, body) = send(state(None), get("/v1/sources/gamma")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "unknown_operation");
}

#[tokio::test]
async fn calls_operations_with_bodies_and_query_parameters() {
    let (status, headers, body) = send(
        state(None),
        post("/v1/sources/alpha/echo", r#"{"text":"hi","times":2}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        headers.get(header::CONTENT_TYPE).unwrap(),
        "application/json"
    );
    assert_eq!(body["text"], "hihi");
    let (status, _, body) = send(
        state(None),
        get("/v1/sources/alpha/echo?text=hi&times=3&loud=true&tags=a&tags=b,c&mode=markdown"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body,
        json!({ "text": "HIHIHI", "tags": ["a", "b", "c"], "mode": "markdown" })
    );
    let (status, _, body) = send(state(None), post("/v1/sources/alpha/echo", "")).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "missing required field: {body}"
    );
    assert_eq!(body["error"]["code"], "invalid_input");
    assert_eq!(body["error"]["source"], "alpha");
    assert!(body["error"]["request_id"].is_string());
    let (status, _, body) =
        send(state(None), get("/v1/sources/alpha/echo?text=hi&times=two")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("integer")
    );
    let (status, _, _) = send(state(None), post("/v1/sources/alpha/echo", "[1,2]")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _, body) = send(
        state(None),
        post("/v1/sources/alpha/write_thing", r#"{"name":"x"}"#),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "write operations are not exposed: {body}"
    );
    let (status, _, body) = send(state(None), get("/v1/sources/alpha/list-things?limit=2")).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "operations use snake_case in paths: {body}"
    );
    let (status, _, body) = send(state(None), get("/v1/sources/alpha/list_things?limit=2")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["items"].as_array().unwrap().len(), 2);
    assert!(body["next_cursor"].is_string());
}

#[tokio::test]
async fn searches_over_get_and_post() {
    let (status, _, body) = send(
        state(None),
        get("/v1/search?query=thing-1&limit=2&kinds=issue,document"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["items"].as_array().unwrap().len(), 2);
    assert_eq!(body["errors"][0]["source"], "beta");
    let (status, _, body) = send(
        state(None),
        post(
            "/v1/search",
            r#"{"query":"thing","sources":["alpha"],"limit":1,"include_raw":true}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["items"][0]["raw"].is_object());
    let (status, _, body) = send(state(None), get("/v1/search?limit=2")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"]["message"].as_str().unwrap().contains("query"));
}

#[tokio::test]
async fn openapi_and_docs_describe_the_registry() {
    let (status, _, body) = send(state(Some("secret")), get("/openapi.json")).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "openapi is exempt from the bearer token"
    );
    assert_eq!(body["openapi"], "3.1.0");
    let echo = &body["paths"]["/v1/sources/alpha/echo"];
    assert_eq!(
        echo["post"]["requestBody"]["content"]["application/json"]["schema"]["properties"]["text"]
            ["type"],
        "string"
    );
    assert!(
        echo["get"]["parameters"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["name"] == "times" && p["schema"]["type"] == "integer")
    );
    assert!(body["paths"]["/v1/search"]["post"].is_object());
    assert!(body["components"]["schemas"]["SearchResult"].is_object());
    assert_eq!(body["security"][0]["bearerAuth"], json!([]));
    let response = app(state(None), None).oneshot(get("/docs")).await.unwrap();
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

#[tokio::test]
async fn bearer_and_host_guards() {
    let (status, headers, body) = send(state(Some("secret")), get("/v1/sources")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], "unauthorized");
    assert_eq!(headers.get(header::WWW_AUTHENTICATE).unwrap(), "Bearer");
    let request = Request::builder()
        .uri("/v1/sources")
        .header(header::HOST, "localhost")
        .header(header::AUTHORIZATION, "Bearer secret")
        .body(Body::empty())
        .unwrap();
    let (status, _, _) = send(state(Some("secret")), request).await;
    assert_eq!(status, StatusCode::OK);
    let request = Request::builder()
        .uri("/v1/sources")
        .header(header::HOST, "localhost")
        .header(header::AUTHORIZATION, "Bearer wrong")
        .body(Body::empty())
        .unwrap();
    let (status, _, _) = send(state(Some("secret")), request).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, _) = send(state(Some("secret")), get("/healthz")).await;
    assert_eq!(status, StatusCode::OK, "healthz is exempt");
    let request = Request::builder()
        .uri("/healthz")
        .header(header::HOST, "evil.example")
        .body(Body::empty())
        .unwrap();
    let (status, _, body) = send(state(None), request).await;
    assert_eq!(status, StatusCode::MISDIRECTED_REQUEST);
    assert_eq!(body["error"]["code"], "misdirected");
    let request = Request::builder()
        .uri("/healthz")
        .header(header::HOST, "localhost")
        .header("x-request-id", "abc-123")
        .body(Body::empty())
        .unwrap();
    let (_, headers, _) = send(state(None), request).await;
    assert_eq!(headers.get("x-request-id").unwrap(), "abc-123");
}
