use std::sync::Arc;

use httpmock::prelude::*;
use mcpie::config::{Config, Secret, SourceConfig};
use mcpie::model::{CallContext, Registry, Source, SourceError, SourceOptions};
use mcpie::sources::Settings;
use mcpie::sources::linear::Linear;
use serde_json::{Value, json};

fn linear(server: &MockServer, token: &str, default_team: Option<&str>) -> Linear {
    let mut config = SourceConfig {
        token: Some(Secret::new(token)),
        base_url: Some(server.base_url()),
        ..SourceConfig::default()
    };
    if let Some(team) = default_team {
        config.extra.insert("default_team".into(), json!(team));
    }
    Linear::new(
        Settings::new("linear", &config, &Config::default().http),
        &config,
    )
    .unwrap()
}

fn build_registry(server: &MockServer, token: &str, default_team: Option<&str>) -> Registry {
    let mut registry = Registry::new();
    registry
        .register(
            Arc::new(linear(server, token, default_team)),
            SourceOptions::default(),
        )
        .unwrap();
    registry
}

#[tokio::test]
async fn api_keys_are_sent_raw_and_oauth_tokens_as_bearer() {
    let server = MockServer::start();
    let raw = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .header("authorization", "lin_api_key")
            .body_includes("viewer");
        then.status(200).json_body(
            json!({ "data": { "viewer": { "name": "Jane", "organization": { "name": "Acme" } } } }),
        );
    });
    let registry = build_registry(&server, "lin_api_key", None);
    let ctx = CallContext::default();
    let status = registry
        .source("linear")
        .unwrap()
        .check(&ctx)
        .await
        .unwrap();
    assert_eq!(status.identity.as_deref(), Some("Jane @ Acme"));
    assert_eq!(status.token_kind.as_deref(), Some("api key"));
    raw.assert();
    let bearer = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .header("authorization", "Bearer lin_oauth_tok");
        then.status(200)
            .json_body(json!({ "data": { "viewer": { "name": "Bot" } } }));
    });
    let oauth = build_registry(&server, "lin_oauth_tok", None);
    let viewer = oauth
        .call("linear", "get_viewer", Value::Null, &ctx)
        .await
        .unwrap();
    assert_eq!(viewer["name"], "Bot");
    bearer.assert();
}

#[tokio::test]
async fn issues_are_filtered_and_paginated_with_relay_cursors() {
    let server = MockServer::start();
    let first = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("issues(first: $first")
            .body_excludes("\"after\"")
            .json_body_includes(r#"{"variables":{"first":2,"filter":{"team":{"key":{"eq":"ENG"}},"state":{"name":{"eqIgnoreCase":"todo"},"type":{"nin":["completed","canceled"]}}}}}"#);
        then.status(200).json_body(json!({ "data": { "issues": { "nodes": [{ "identifier": "ENG-1" }, { "identifier": "ENG-2" }], "pageInfo": { "hasNextPage": true, "endCursor": "abc" } } } }));
    });
    let second = server.mock(|when, then| {
        when.method(POST).path("/graphql").json_body_includes(r#"{"variables":{"after":"abc"}}"#);
        then.status(200).json_body(json!({ "data": { "issues": { "nodes": [{ "identifier": "ENG-3" }], "pageInfo": { "hasNextPage": false, "endCursor": "def" } } } }));
    });
    let registry = build_registry(&server, "lin_api_key", Some("ENG"));
    let ctx = CallContext::default();
    let page = registry
        .call(
            "linear",
            "list_issues",
            json!({ "state": "todo", "limit": 2 }),
            &ctx,
        )
        .await
        .unwrap();
    assert_eq!(page["items"][1]["identifier"], "ENG-2");
    let cursor = page["next_cursor"].as_str().unwrap().to_owned();
    let next = registry
        .call(
            "linear",
            "list_issues",
            json!({ "state": "todo", "limit": 2, "cursor": cursor }),
            &ctx,
        )
        .await
        .unwrap();
    assert_eq!(next["items"][0]["identifier"], "ENG-3");
    assert!(next["next_cursor"].is_null());
    first.assert();
    second.assert();
}

#[tokio::test]
async fn get_issue_comments_and_search() {
    let server = MockServer::start();
    let issue = server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("issue(id: $id)")
            .json_body_includes(r#"{"variables":{"id":"ENG-7"}}"#)
            .body_excludes("comments(");
        then.status(200)
            .json_body(json!({ "data": { "issue": { "identifier": "ENG-7", "title": "Crash" } } }));
    });
    let comments = server.mock(|when, then| {
        when.method(POST).path("/graphql").body_includes("comments(first: $first");
        then.status(200).json_body(json!({ "data": { "issue": { "comments": { "nodes": [{ "body": "on it" }], "pageInfo": { "hasNextPage": false } } } } }));
    });
    let search = server.mock(|when, then| {
        when.method(POST).path("/graphql").body_includes("searchIssues(term: $term").json_body_includes(r#"{"variables":{"term":"crash"}}"#);
        then.status(200).json_body(json!({ "data": { "searchIssues": { "nodes": [{ "identifier": "ENG-7" }], "pageInfo": { "hasNextPage": false } } } }));
    });
    let registry = build_registry(&server, "lin_api_key", None);
    let ctx = CallContext::default();
    assert_eq!(
        registry
            .call("linear", "get_issue", json!({ "issue": "ENG-7" }), &ctx)
            .await
            .unwrap()["title"],
        "Crash"
    );
    assert_eq!(
        registry
            .call("linear", "list_comments", json!({ "issue": "ENG-7" }), &ctx)
            .await
            .unwrap()["items"][0]["body"],
        "on it"
    );
    assert_eq!(
        registry
            .call("linear", "search_issues", json!({ "query": "crash" }), &ctx)
            .await
            .unwrap()["items"][0]["identifier"],
        "ENG-7"
    );
    issue.assert();
    comments.assert();
    search.assert();
}

#[tokio::test]
async fn raw_queries_refuse_mutations_and_errors_map() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(POST)
            .path("/graphql")
            .body_includes("teams { nodes { key } }");
        then.status(200)
            .json_body(json!({ "data": { "teams": { "nodes": [{ "key": "ENG" }] } } }));
    });
    server.mock(|when, then| {
        when.method(POST).path("/graphql").body_includes("viewer");
        then.status(400).json_body(json!({ "errors": [{ "message": "Authentication required, not authenticated", "extensions": { "type": "authentication error" } }] }));
    });
    server.mock(|when, then| {
        when.method(POST).path("/graphql").body_includes("issue(id: $id)");
        then.status(200).json_body(json!({ "errors": [{ "message": "Entity not found: Issue - Could not find referenced Issue." }] }));
    });
    let registry = build_registry(&server, "lin_api_key", None);
    let ctx = CallContext::default();
    let data = registry
        .call(
            "linear",
            "request",
            json!({ "query": "{ teams { nodes { key } } }" }),
            &ctx,
        )
        .await
        .unwrap();
    assert_eq!(data["teams"]["nodes"][0]["key"], "ENG");
    let error = registry
        .call(
            "linear",
            "request",
            json!({ "query": "mutation { issueDelete(id: \"x\") { success } }" }),
            &ctx,
        )
        .await
        .unwrap_err();
    assert!(matches!(error, SourceError::Unsupported(_)), "{error}");
    let error = registry
        .call("linear", "get_viewer", Value::Null, &ctx)
        .await
        .unwrap_err();
    assert!(matches!(error, SourceError::Auth(_)), "{error}");
    let error = registry
        .call("linear", "get_issue", json!({ "issue": "ENG-999" }), &ctx)
        .await
        .unwrap_err();
    assert!(matches!(error, SourceError::NotFound(_)), "{error}");
}

#[test]
fn every_linear_operation_is_dispatched() {
    let server = MockServer::start();
    let source = linear(&server, "lin_api_key", None);
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
