use std::sync::Arc;

use httpmock::prelude::*;
use mcpie::config::{Config, Secret, SourceConfig};
use mcpie::model::{CallContext, ItemKind, Registry, SearchQuery, SourceOptions};
use mcpie::sources::Settings;
use mcpie::sources::github::Github;
use mcpie::sources::slack::Slack;
use serde_json::json;

fn source_config(server: &MockServer, token: &str) -> SourceConfig {
    SourceConfig {
        token: Some(Secret::new(token)),
        base_url: Some(server.base_url()),
        ..SourceConfig::default()
    }
}

#[tokio::test]
async fn search_merges_github_and_slack_newest_first() {
    let github = MockServer::start();
    github.mock(|when, then| {
        when.method(GET).path("/search/issues").query_param("q", "release");
        then.status(200).json_body(json!({ "total_count": 1, "items": [{ "number": 7, "title": "Release crash", "body": "boom", "state": "open",
            "html_url": "https://github.com/acme/widgets/issues/7", "repository_url": "https://api.github.com/repos/acme/widgets",
            "user": { "login": "jane" }, "updated_at": "2026-01-02T00:00:00Z" }] }));
    });
    github.mock(|when, then| {
        when.method(GET).path("/search/code");
        then.status(403)
            .json_body(json!({ "message": "code search requires more" }));
    });
    let slack = MockServer::start();
    slack.mock(|when, then| {
        when.method(POST).path("/search.messages").form_urlencoded_tuple("query", "release");
        then.status(200).json_body(json!({ "ok": true, "messages": { "matches": [{ "ts": "1767398400.000100", "text": "release is out", "user": "U1",
            "permalink": "https://acme.slack.com/archives/C1/p1", "channel": { "id": "C1", "name": "eng" } }], "paging": { "page": 1, "pages": 1 } } }));
    });
    slack.mock(|when, then| {
        when.method(POST).path("/conversations.list");
        then.status(200).json_body(
            json!({ "ok": true, "channels": [], "response_metadata": { "next_cursor": "" } }),
        );
    });
    slack.mock(|when, then| {
        when.method(POST).path("/users.list");
        then.status(200).json_body(json!({ "ok": true, "members": [{ "id": "U1", "name": "jane", "profile": { "display_name": "Jane D" } }], "response_metadata": { "next_cursor": "" } }));
    });
    let mut registry = Registry::new();
    let gh = source_config(&github, "t");
    registry
        .register(
            Arc::new(
                Github::new(Settings::new("github", &gh, &Config::default().http), &gh).unwrap(),
            ),
            SourceOptions::default(),
        )
        .unwrap();
    let sl = source_config(&slack, "xoxp-1");
    registry
        .register(
            Arc::new(
                Slack::new(Settings::new("slack", &sl, &Config::default().http), &sl).unwrap(),
            ),
            SourceOptions::default(),
        )
        .unwrap();

    let result = registry
        .search(
            SearchQuery {
                query: "release".into(),
                ..Default::default()
            },
            &CallContext::default(),
        )
        .await;
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_eq!(result.items.len(), 2);
    assert_eq!(
        result.items[0].source, "slack",
        "2026-01-03 message sorts before the 2026-01-02 issue"
    );
    assert_eq!(
        result.items[0].author.as_deref(),
        Some("Jane D"),
        "user ids resolve to names"
    );
    assert_eq!(result.items[1].id, "acme/widgets#7");
    assert!(
        result
            .items
            .iter()
            .all(|i| i.raw.is_none() && i.fetch.is_some())
    );

    let issues_only = registry
        .search(
            SearchQuery {
                query: "release".into(),
                kinds: Some(vec![ItemKind::Issue]),
                ..Default::default()
            },
            &CallContext::default(),
        )
        .await;
    assert_eq!(issues_only.items.len(), 1);
    assert_eq!(issues_only.items[0].kind, ItemKind::Issue);
}
