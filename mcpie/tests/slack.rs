use std::sync::Arc;

use httpmock::prelude::*;
use mcpie::config::{Config, Secret, SourceConfig};
use mcpie::model::{CallContext, Registry, Source, SourceError, SourceOptions};
use mcpie::sources::Settings;
use mcpie::sources::slack::Slack;
use serde_json::{Value, json};

fn slack(server: &MockServer, token: &str) -> Slack {
    let config = SourceConfig {
        token: Some(Secret::new(token)),
        base_url: Some(server.base_url()),
        ..SourceConfig::default()
    };
    Slack::new(
        Settings::new("slack", &config, &Config::default().http),
        &config,
    )
    .unwrap()
}

fn registry(server: &MockServer, token: &str) -> Registry {
    let mut registry = Registry::new();
    registry
        .register(Arc::new(slack(server, token)), SourceOptions::default())
        .unwrap();
    registry
}

fn mock_directory(server: &MockServer) {
    server.mock(|when, then| {
        when.method(POST).path("/conversations.list").form_urlencoded_tuple("types", "public_channel,private_channel");
        then.status(200).json_body(json!({ "ok": true, "channels": [{ "id": "C1", "name": "general" }, { "id": "C2", "name": "eng" }], "response_metadata": { "next_cursor": "" } }));
    });
    server.mock(|when, then| {
        when.method(POST).path("/users.list");
        then.status(200).json_body(json!({ "ok": true, "members": [{ "id": "U1", "name": "jane", "real_name": "Jane Doe", "profile": { "display_name": "jane.d" } }], "response_metadata": { "next_cursor": "" } }));
    });
}

#[tokio::test]
async fn check_reports_identity_and_token_kind() {
    let server = MockServer::start();
    let auth = server.mock(|when, then| {
        when.method(POST)
            .path("/auth.test")
            .header("authorization", "Bearer xoxb-1");
        then.status(200)
            .header("x-oauth-scopes", "channels:read,users:read")
            .json_body(json!({ "ok": true, "user": "bot", "team": "Acme", "user_id": "U9" }));
    });
    let registry = registry(&server, "xoxb-1");
    let status = registry
        .source("slack")
        .unwrap()
        .check(&CallContext::default())
        .await
        .unwrap();
    assert_eq!(status.identity.as_deref(), Some("bot @ Acme"));
    assert_eq!(status.token_kind.as_deref(), Some("bot"));
    assert_eq!(status.scopes, ["channels:read", "users:read"]);
    assert_eq!(status.unavailable, ["search_messages"]);
    auth.assert();
}

#[tokio::test]
async fn history_resolves_channel_names_and_paginates() {
    let server = MockServer::start();
    mock_directory(&server);
    let first = server.mock(|when, then| {
        when.method(POST).path("/conversations.history").form_urlencoded_tuple("channel", "C2").form_urlencoded_tuple("limit", "2").form_urlencoded_tuple("oldest", "1.0");
        then.status(200).json_body(json!({ "ok": true, "messages": [{ "ts": "3.0", "text": "b" }, { "ts": "2.0", "text": "a" }], "response_metadata": { "next_cursor": "dXNlcjpVMDYxTkZUVDI=" } }));
    });
    let second = server.mock(|when, then| {
        when.method(POST).path("/conversations.history").form_urlencoded_tuple("channel", "C2").form_urlencoded_tuple("cursor", "dXNlcjpVMDYxTkZUVDI=");
        then.status(200).json_body(json!({ "ok": true, "messages": [{ "ts": "1.5", "text": "z" }], "response_metadata": { "next_cursor": "" } }));
    });
    let registry = registry(&server, "xoxp-1");
    let ctx = CallContext::default();
    let page = registry
        .call(
            "slack",
            "get_channel_history",
            json!({ "channel": "#eng", "limit": 2, "oldest": "1.0" }),
            &ctx,
        )
        .await
        .unwrap();
    assert_eq!(page["items"][0]["text"], "b");
    let cursor = page["next_cursor"].as_str().unwrap().to_owned();
    assert!(
        !cursor.contains("dXNlcjp"),
        "slack cursors are wrapped, not exposed"
    );
    let next = registry
        .call(
            "slack",
            "get_channel_history",
            json!({ "channel": "C2", "cursor": cursor }),
            &ctx,
        )
        .await
        .unwrap();
    assert_eq!(next["items"][0]["text"], "z");
    assert!(next["next_cursor"].is_null());
    first.assert();
    second.assert();
    let error = registry
        .call(
            "slack",
            "get_channel_history",
            json!({ "channel": "#nope" }),
            &ctx,
        )
        .await
        .unwrap_err();
    assert!(matches!(error, SourceError::NotFound(_)), "{error}");
}

#[tokio::test]
async fn users_resolve_handles_and_search_pages_by_number() {
    let server = MockServer::start();
    mock_directory(&server);
    let info = server.mock(|when, then| {
        when.method(POST)
            .path("/users.info")
            .form_urlencoded_tuple("user", "U1");
        then.status(200)
            .json_body(json!({ "ok": true, "user": { "id": "U1", "name": "jane" } }));
    });
    let search = server.mock(|when, then| {
        when.method(POST).path("/search.messages").form_urlencoded_tuple("query", "release in:#eng").form_urlencoded_tuple("count", "1").form_urlencoded_tuple("page", "1").form_urlencoded_tuple("sort", "timestamp");
        then.status(200).json_body(json!({ "ok": true, "messages": { "matches": [{ "ts": "9.0", "text": "release!" }], "paging": { "page": 1, "pages": 3 } } }));
    });
    let registry = registry(&server, "xoxp-1");
    let ctx = CallContext::default();
    let user = registry
        .call("slack", "get_user", json!({ "user": "@Jane.D" }), &ctx)
        .await
        .unwrap();
    assert_eq!(user["name"], "jane");
    let page = registry
        .call(
            "slack",
            "search_messages",
            json!({ "query": "release in:#eng", "limit": 1, "sort": "timestamp" }),
            &ctx,
        )
        .await
        .unwrap();
    assert_eq!(page["items"][0]["text"], "release!");
    assert!(page["next_cursor"].is_string());
    info.assert();
    search.assert();
}

#[tokio::test]
async fn raw_requests_are_limited_to_read_methods_and_errors_map() {
    let server = MockServer::start();
    let team = server.mock(|when, then| {
        when.method(POST).path("/team.info");
        then.status(200)
            .json_body(json!({ "ok": true, "team": { "name": "Acme" } }));
    });
    server.mock(|when, then| {
        when.method(POST).path("/auth.test");
        then.status(200)
            .json_body(json!({ "ok": false, "error": "invalid_auth" }));
    });
    server.mock(|when, then| {
        when.method(POST).path("/conversations.replies");
        then.status(429).header("retry-after", "3").body("");
    });
    let registry = registry(&server, "xoxp-1");
    let ctx = CallContext::default();
    let body = registry
        .call("slack", "request", json!({ "method": "team.info" }), &ctx)
        .await
        .unwrap();
    assert_eq!(body["team"]["name"], "Acme");
    team.assert();
    let error = registry
        .call(
            "slack",
            "request",
            json!({ "method": "chat.postMessage", "params": { "text": "hi" } }),
            &ctx,
        )
        .await
        .unwrap_err();
    assert!(matches!(error, SourceError::Unsupported(_)), "{error}");
    let error = registry
        .call("slack", "get_auth", Value::Null, &ctx)
        .await
        .unwrap_err();
    assert!(matches!(error, SourceError::Auth(_)), "{error}");
    let error = registry
        .call(
            "slack",
            "get_thread_replies",
            json!({ "channel": "C1", "ts": "1.0" }),
            &ctx,
        )
        .await
        .unwrap_err();
    assert_eq!(
        error.retry_after(),
        Some(std::time::Duration::from_secs(3)),
        "{error}"
    );
}

#[test]
fn every_slack_operation_is_dispatched() {
    let server = MockServer::start();
    let source = slack(&server, "xoxp-1");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    for spec in source.operations() {
        let error = runtime
            .block_on(source.call(&spec.name, json!({}), &CallContext::default()))
            .unwrap_err();
        assert!(
            !matches!(error, SourceError::UnknownOperation(_)),
            "{} is in the spec table but not dispatched",
            spec.name
        );
    }
    assert_eq!(source.operations().len(), 9);
}
