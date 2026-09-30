mod common;

use std::sync::Arc;

use common::FakeSource;
use mcpie::model::{
    CallContext, Registry, RegistryError, SearchQuery, Selection, SourceError, SourceOptions,
};
use serde_json::{Value, json};

fn registry() -> Registry {
    let mut registry = Registry::new();
    registry
        .register(Arc::new(FakeSource::new("alpha")), SourceOptions::default())
        .unwrap();
    registry
        .register(
            Arc::new(FakeSource::with_things("beta", 3)),
            SourceOptions::default(),
        )
        .unwrap();
    registry
}

#[test]
fn exposes_only_read_operations_by_default() {
    let registry = registry();
    let exposed = registry.select(&Selection::default()).unwrap();
    assert_eq!(exposed.len(), 2);
    let names: Vec<&str> = exposed[0]
        .operations
        .iter()
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(names, ["echo", "list_things"]);
    let all = registry
        .select(&Selection {
            include_write: true,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(all[0].operations.len(), 3);
}

#[test]
fn selection_filters_sources_and_globs() {
    let registry = registry();
    let selection = Selection {
        sources: Some(vec!["beta".into()]),
        tools: vec!["list_*".into()],
        ..Default::default()
    };
    let exposed = registry.select(&selection).unwrap();
    assert_eq!(exposed.len(), 1);
    assert_eq!(exposed[0].source.id(), "beta");
    assert_eq!(exposed[0].operations.len(), 1);
    let selection = Selection {
        exclude_tools: vec!["alpha.echo".into()],
        ..Default::default()
    };
    let exposed = registry.select(&selection).unwrap();
    assert_eq!(
        exposed[0]
            .operations
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>(),
        ["list_things"]
    );
    assert_eq!(
        exposed[1].operations.len(),
        2,
        "glob qualified by source only affects alpha"
    );
}

#[tokio::test]
async fn configured_allowlist_hides_operations_everywhere() {
    let mut registry = Registry::new();
    registry
        .register(
            Arc::new(FakeSource::new("alpha")),
            SourceOptions {
                enabled: true,
                tools: vec!["echo".into()],
            },
        )
        .unwrap();
    assert_eq!(registry.operations("alpha").unwrap().len(), 1);
    assert!(registry.spec("alpha", "list_things").is_none());
    let error = registry
        .call("alpha", "list_things", Value::Null, &CallContext::default())
        .await
        .unwrap_err();
    assert!(matches!(error, SourceError::UnknownOperation(_)), "{error}");
}

#[test]
fn rejects_invalid_registrations() {
    let mut registry = Registry::new();
    let error = registry
        .register(
            Arc::new(FakeSource::new("Bad_Id")),
            SourceOptions::default(),
        )
        .unwrap_err();
    assert!(
        matches!(error, RegistryError::InvalidSourceId(_)),
        "{error}"
    );
    registry
        .register(Arc::new(FakeSource::new("alpha")), SourceOptions::default())
        .unwrap();
    let error = registry
        .register(Arc::new(FakeSource::new("alpha")), SourceOptions::default())
        .unwrap_err();
    assert!(
        matches!(error, RegistryError::DuplicateSource(_)),
        "{error}"
    );
}

#[tokio::test]
async fn calls_typed_operations() {
    let registry = registry();
    let ctx = CallContext::default();
    let out = registry
        .call(
            "alpha",
            "echo",
            json!({ "text": "hi", "times": 2, "loud": true, "tags": ["a"], "mode": "markdown" }),
            &ctx,
        )
        .await
        .unwrap();
    assert_eq!(
        out,
        json!({ "text": "HIHI", "tags": ["a"], "mode": "markdown" })
    );
    let error = registry
        .call("alpha", "echo", json!({ "text": "hi", "bogus": 1 }), &ctx)
        .await
        .unwrap_err();
    assert!(matches!(error, SourceError::InvalidInput(_)), "{error}");
    assert!(error.to_string().contains("bogus"));
    let error = registry
        .call("alpha", "nope", Value::Null, &ctx)
        .await
        .unwrap_err();
    assert!(matches!(error, SourceError::UnknownOperation(_)));
    let error = registry
        .call("gamma", "echo", Value::Null, &ctx)
        .await
        .unwrap_err();
    assert!(matches!(error, SourceError::UnknownSource(_)));
}

#[tokio::test]
async fn null_input_means_empty_object() {
    let registry = registry();
    let out = registry
        .call("beta", "list_things", Value::Null, &CallContext::default())
        .await
        .unwrap();
    assert_eq!(out["items"].as_array().unwrap().len(), 3);
    assert_eq!(out["next_cursor"], Value::Null);
}

#[tokio::test]
async fn write_operations_are_refused_unless_allowed() {
    let registry = registry();
    let ctx = CallContext::default();
    let error = registry
        .call("alpha", "write_thing", json!({ "name": "x" }), &ctx)
        .await
        .unwrap_err();
    assert!(matches!(error, SourceError::Unsupported(_)), "{error}");
    let out = registry
        .call_with_policy("alpha", "write_thing", json!({ "name": "x" }), &ctx, true)
        .await
        .unwrap();
    assert_eq!(out, json!({ "created": "x" }));
}

#[tokio::test]
async fn disabled_sources_are_hidden_but_known() {
    let mut registry = Registry::new();
    registry
        .register(
            Arc::new(FakeSource::new("alpha")),
            SourceOptions {
                enabled: false,
                tools: vec![],
            },
        )
        .unwrap();
    assert!(registry.select(&Selection::default()).unwrap().is_empty());
    assert!(registry.source("alpha").is_some());
    let error = registry
        .call(
            "alpha",
            "echo",
            json!({ "text": "x" }),
            &CallContext::default(),
        )
        .await
        .unwrap_err();
    assert!(matches!(error, SourceError::NotConfigured(_)), "{error}");
}

#[tokio::test]
async fn paginates_with_opaque_cursors() {
    let registry = registry();
    let ctx = CallContext::default();
    let mut cursor: Option<String> = None;
    let mut seen = Vec::new();
    let mut pages = 0;
    loop {
        let input = json!({ "limit": 10, "cursor": cursor });
        let page = registry
            .call("alpha", "list_things", input, &ctx)
            .await
            .unwrap();
        pages += 1;
        seen.extend(
            page["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|i| i["n"].as_u64().unwrap()),
        );
        match page["next_cursor"].as_str() {
            Some(next) => cursor = Some(next.to_owned()),
            None => break,
        }
    }
    assert_eq!(pages, 3);
    assert_eq!(seen, (0..25).collect::<Vec<u64>>());
    // A cursor from another source is rejected instead of silently reused.
    let page = registry
        .call("alpha", "list_things", json!({ "limit": 10 }), &ctx)
        .await
        .unwrap();
    let foreign = page["next_cursor"].as_str().unwrap();
    let error = registry
        .call("beta", "list_things", json!({ "cursor": foreign }), &ctx)
        .await
        .unwrap_err();
    assert!(matches!(error, SourceError::InvalidInput(_)), "{error}");
}

#[tokio::test]
async fn search_merges_newest_first_and_reports_failures() {
    let mut registry = registry();
    registry
        .register(
            Arc::new(FakeSource::new("broken").failing_search()),
            SourceOptions::default(),
        )
        .unwrap();
    let ctx = CallContext::default();
    let result = registry
        .search(
            SearchQuery {
                query: "thing-1".into(),
                limit: Some(5),
                ..Default::default()
            },
            &ctx,
        )
        .await;
    assert_eq!(result.items.len(), 5);
    assert!(
        result
            .items
            .windows(2)
            .all(|w| w[0].updated_at >= w[1].updated_at),
        "newest first"
    );
    assert!(
        result.items.iter().all(|i| i.raw.is_none()),
        "raw is opt-in"
    );
    assert_eq!(result.errors.len(), 1);
    assert_eq!(result.errors[0].source, "broken");
    assert_eq!(result.errors[0].code, "upstream_auth");

    let result = registry
        .search(
            SearchQuery {
                query: String::new(),
                sources: Some(vec!["beta".into(), "nope".into()]),
                include_raw: true,
                ..Default::default()
            },
            &ctx,
        )
        .await;
    assert_eq!(result.items.len(), 3);
    assert!(
        result
            .items
            .iter()
            .all(|i| i.source == "beta" && i.raw.is_some())
    );
    assert_eq!(result.errors[0].source, "nope");
    assert_eq!(result.errors[0].code, "unknown_operation");
}
