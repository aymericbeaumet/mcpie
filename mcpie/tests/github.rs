use std::sync::Arc;

use httpmock::prelude::*;
use mcpie::config::{Config, Secret, SourceConfig};
use mcpie::model::{CallContext, Registry, Selection, Source, SourceError, SourceOptions};
use mcpie::sources::github::Github;
use mcpie::sources::{Settings, build};
use serde_json::{Value, json};

fn github(server: &MockServer, token: &str) -> Github {
    let config = SourceConfig {
        token: Some(Secret::new(token)),
        base_url: Some(server.base_url()),
        ..SourceConfig::default()
    };
    Github::new(
        Settings::new("github", &config, &Config::default().http),
        &config,
    )
    .unwrap()
}

fn registry(server: &MockServer) -> Registry {
    let mut registry = Registry::new();
    registry
        .register(
            Arc::new(github(server, "test-token")),
            SourceOptions::default(),
        )
        .unwrap();
    registry
}

#[tokio::test]
async fn get_viewer_sends_github_headers() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(GET)
            .path("/user")
            .header("authorization", "Bearer test-token")
            .header("accept", "application/vnd.github+json")
            .header("x-github-api-version", "2022-11-28")
            .header_exists("user-agent");
        then.status(200)
            .header("x-oauth-scopes", "repo, read:org")
            .json_body(json!({ "login": "octocat", "id": 1 }));
    });
    let registry = registry(&server);
    let ctx = CallContext::default();
    let user = registry
        .call("github", "get_viewer", Value::Null, &ctx)
        .await
        .unwrap();
    assert_eq!(user["login"], "octocat");
    let status = registry
        .source("github")
        .unwrap()
        .check(&ctx)
        .await
        .unwrap();
    assert_eq!(status.identity.as_deref(), Some("octocat"));
    assert_eq!(status.scopes, ["repo", "read:org"]);
    assert_eq!(status.credential.as_deref(), Some("config"));
    assert_eq!(mock.calls(), 2);
}

#[tokio::test]
async fn list_repos_paginates_through_link_headers() {
    let server = MockServer::start();
    let page1 = server.mock(|when, then| {
        when.method(GET)
            .path("/user/repos")
            .query_param("per_page", "2")
            .query_param("page", "1");
        then.status(200)
            .header(
                "link",
                format!(
                    "<{}/user/repos?per_page=2&page=2>; rel=\"next\"",
                    server.base_url()
                ),
            )
            .json_body(json!([{ "name": "a" }, { "name": "b" }]));
    });
    let page2 = server.mock(|when, then| {
        when.method(GET)
            .path("/user/repos")
            .query_param("per_page", "2")
            .query_param("page", "2");
        then.status(200).json_body(json!([{ "name": "c" }]));
    });
    let registry = registry(&server);
    let ctx = CallContext::default();
    let first = registry
        .call("github", "list_repos", json!({ "limit": 2 }), &ctx)
        .await
        .unwrap();
    assert_eq!(first["items"].as_array().unwrap().len(), 2);
    let cursor = first["next_cursor"]
        .as_str()
        .expect("next cursor")
        .to_owned();
    let second = registry
        .call(
            "github",
            "list_repos",
            json!({ "limit": 2, "cursor": cursor }),
            &ctx,
        )
        .await
        .unwrap();
    assert_eq!(second["items"][0]["name"], "c");
    assert!(second["next_cursor"].is_null());
    page1.assert();
    page2.assert();
}

#[tokio::test]
async fn list_repos_resolves_owner_type() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/users/acme");
        then.status(200)
            .json_body(json!({ "login": "acme", "type": "Organization" }));
    });
    let org = server.mock(|when, then| {
        when.method(GET)
            .path("/orgs/acme/repos")
            .query_param("sort", "updated")
            .query_param("type", "private");
        then.status(200).json_body(json!([{ "name": "widgets" }]));
    });
    let registry = registry(&server);
    let page = registry
        .call(
            "github",
            "list_repos",
            json!({ "owner": "acme", "sort": "updated", "type": "private" }),
            &CallContext::default(),
        )
        .await
        .unwrap();
    assert_eq!(page["items"][0]["name"], "widgets");
    org.assert();
}

#[tokio::test]
async fn raw_request_forwards_query_and_rejects_foreign_paths() {
    let server = MockServer::start();
    let mock = server.mock(|when, then| {
        when.method(GET)
            .path("/repos/acme/widgets/issues")
            .query_param("state", "open")
            .query_param("per_page", "5");
        then.status(200).json_body(json!([{ "number": 7 }]));
    });
    let registry = registry(&server);
    let ctx = CallContext::default();
    let body = registry.call("github", "request", json!({ "path": "/repos/acme/widgets/issues", "query": { "state": "open", "per_page": 5 } }), &ctx).await.unwrap();
    assert_eq!(body[0]["number"], 7);
    mock.assert();
    let error = registry
        .call(
            "github",
            "request",
            json!({ "path": "https://evil.example/x" }),
            &ctx,
        )
        .await
        .unwrap_err();
    assert!(matches!(error, SourceError::InvalidInput(_)), "{error}");
}

#[tokio::test]
async fn maps_upstream_failures() {
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/user");
        then.status(401)
            .json_body(json!({ "message": "Bad credentials" }));
    });
    server.mock(|when, then| {
        when.method(GET).path("/users/ghost");
        then.status(404)
            .json_body(json!({ "message": "Not Found" }));
    });
    server.mock(|when, then| {
        when.method(GET).path("/rate");
        then.status(403)
            .header("x-ratelimit-remaining", "0")
            .header("x-ratelimit-reset", "1")
            .json_body(json!({ "message": "API rate limit exceeded" }));
    });
    let registry = registry(&server);
    let ctx = CallContext::default();
    let error = registry
        .call("github", "get_viewer", Value::Null, &ctx)
        .await
        .unwrap_err();
    assert!(matches!(error, SourceError::Auth(_)), "{error}");
    let error = registry
        .call("github", "list_repos", json!({ "owner": "ghost" }), &ctx)
        .await
        .unwrap_err();
    assert!(matches!(error, SourceError::NotFound(_)), "{error}");
    let error = registry
        .call("github", "request", json!({ "path": "/rate" }), &ctx)
        .await
        .unwrap_err();
    assert!(matches!(error, SourceError::RateLimited { .. }), "{error}");
}

#[tokio::test]
async fn missing_credentials_are_not_configured() {
    let server = MockServer::start();
    let config = SourceConfig {
        token_command: Some("exit 1".into()),
        base_url: Some(server.base_url()),
        ..SourceConfig::default()
    };
    let source = Github::new(
        Settings::new("github", &config, &Config::default().http),
        &config,
    )
    .unwrap();
    let error = source.check(&CallContext::default()).await.unwrap_err();
    assert!(matches!(error, SourceError::NotConfigured(_)), "{error}");
}

#[test]
fn builder_registers_github_instances() {
    let mut config = Config::default();
    config.sources.insert(
        "ghe".into(),
        SourceConfig {
            kind: Some("github".into()),
            base_url: Some("https://ghe.example/api/v3".into()),
            ..SourceConfig::default()
        },
    );
    config.sources.get_mut("github").unwrap().tools = vec!["get_*".into()];
    let registry = build(&config).unwrap();
    assert!(registry.source("github").is_some());
    assert!(registry.source("ghe").is_some());
    let exposed = registry.select(&Selection::default()).unwrap();
    let github = exposed.iter().find(|e| e.source.id() == "github").unwrap();
    assert_eq!(
        github
            .operations
            .iter()
            .map(|o| o.name.as_str())
            .collect::<Vec<_>>(),
        ["get_viewer"]
    );
    let ghe = exposed.iter().find(|e| e.source.id() == "ghe").unwrap();
    assert_eq!(ghe.operations.len(), 3);
    config
        .sources
        .get_mut("ghe")
        .unwrap()
        .extra
        .insert("bogus".into(), json!(1));
    assert!(build(&config).unwrap_err().to_string().contains("bogus"));
}
