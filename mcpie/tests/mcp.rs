mod common;

use std::sync::Arc;
use std::time::Duration;

use common::FakeSource;
use mcpie::facade::mcp::{McpServer, Mode};
use mcpie::model::{Registry, Selection, SourceOptions};
use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use serde_json::{Value, json};

fn registry() -> Arc<Registry> {
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
    Arc::new(registry)
}

fn params(name: &str, arguments: Value) -> CallToolRequestParams {
    let mut params = CallToolRequestParams::new(name.to_owned());
    params.arguments = arguments.as_object().cloned();
    params
}

/// Run `server` behind an in-memory pipe and hand back a connected client.
async fn connect(server: McpServer) -> rmcp::service::RunningService<rmcp::RoleClient, ()> {
    let (client_side, server_side) = tokio::io::duplex(1 << 16);
    let (server_read, server_write) = tokio::io::split(server_side);
    let (client_read, client_write) = tokio::io::split(client_side);
    tokio::spawn(async move {
        let running = server
            .serve((server_read, server_write))
            .await
            .expect("server initializes");
        let _ = running.waiting().await;
    });
    ().serve((client_read, client_write))
        .await
        .expect("client initializes")
}

#[test]
fn tools_are_derived_from_specs() {
    let server = McpServer::new(
        registry(),
        &Selection::default(),
        Mode::Tools,
        Duration::from_secs(5),
    )
    .unwrap();
    let names: Vec<&str> = server.tools().iter().map(|t| t.name.as_ref()).collect();
    assert_eq!(
        names,
        [
            "alpha_echo",
            "alpha_list_things",
            "beta_echo",
            "beta_list_things",
            "search"
        ]
    );
    let echo = &server.tools()[0];
    assert_eq!(
        echo.annotations.as_ref().unwrap().read_only_hint,
        Some(true)
    );
    assert_eq!(echo.title.as_deref(), Some("Echo"));
    assert!(
        echo.input_schema
            .get("properties")
            .unwrap()
            .get("text")
            .is_some()
    );
    assert!(echo.output_schema.is_some(), "typed outputs carry a schema");
    let selection = Selection {
        sources: Some(vec!["beta".into()]),
        exclude_tools: vec!["echo".into()],
        ..Default::default()
    };
    let server =
        McpServer::new(registry(), &selection, Mode::Tools, Duration::from_secs(5)).unwrap();
    let names: Vec<&str> = server.tools().iter().map(|t| t.name.as_ref()).collect();
    assert_eq!(names, ["beta_list_things", "search"]);
    let server = McpServer::new(
        registry(),
        &Selection::default(),
        Mode::Meta,
        Duration::from_secs(5),
    )
    .unwrap();
    let names: Vec<&str> = server.tools().iter().map(|t| t.name.as_ref()).collect();
    assert_eq!(
        names,
        [
            "list_operations",
            "describe_operation",
            "call_operation",
            "search"
        ]
    );
}

#[tokio::test]
async fn tools_mode_round_trips_over_the_protocol() {
    let server = McpServer::new(
        registry(),
        &Selection::default(),
        Mode::Tools,
        Duration::from_secs(5),
    )
    .unwrap();
    let client = connect(server).await;
    let info = client.peer_info().expect("server info");
    assert_eq!(info.server_info.as_ref().unwrap().name, "mcpie");
    assert!(
        info.instructions
            .as_deref()
            .unwrap_or_default()
            .contains("next_cursor")
    );

    let tools = client.list_all_tools().await.unwrap();
    assert_eq!(tools.len(), 5);
    assert!(tools.iter().all(|t| t.name != "alpha_write_thing"));

    let result = client
        .peer()
        .call_tool(params("alpha_echo", json!({ "text": "hi", "times": 2 })))
        .await
        .unwrap();
    assert_ne!(result.is_error, Some(true), "{result:?}");
    assert_eq!(
        result.structured_content,
        Some(json!({ "text": "hihi", "tags": [], "mode": null }))
    );

    let page = client
        .peer()
        .call_tool(params("alpha_list_things", json!({ "limit": 10 })))
        .await
        .unwrap();
    let cursor = page.structured_content.unwrap()["next_cursor"]
        .as_str()
        .unwrap()
        .to_owned();
    let next = client
        .peer()
        .call_tool(params(
            "alpha_list_things",
            json!({ "limit": 10, "cursor": cursor }),
        ))
        .await
        .unwrap();
    assert_eq!(next.structured_content.unwrap()["items"][0]["n"], 10);

    let failed = client
        .peer()
        .call_tool(params("alpha_echo", json!({ "text": "hi", "bogus": 1 })))
        .await
        .unwrap();
    assert_eq!(failed.is_error, Some(true));
    let text = serde_json::to_string(&failed.content).unwrap();
    assert!(
        text.contains("invalid_input") && text.contains("bogus"),
        "{text}"
    );

    let search = client
        .peer()
        .call_tool(params("search", json!({ "query": "thing-1", "limit": 2 })))
        .await
        .unwrap();
    let structured = search.structured_content.unwrap();
    assert_eq!(structured["items"].as_array().unwrap().len(), 2);
    assert_eq!(structured["errors"][0]["source"], "beta");

    assert!(
        client
            .peer()
            .call_tool(params("alpha_write_thing", json!({ "name": "x" })))
            .await
            .is_err(),
        "unknown tools are protocol errors"
    );
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn meta_mode_round_trips_over_the_protocol() {
    let server = McpServer::new(
        registry(),
        &Selection::default(),
        Mode::Meta,
        Duration::from_secs(5),
    )
    .unwrap();
    let client = connect(server).await;
    let listed = client
        .peer()
        .call_tool(params("list_operations", json!({ "source": "alpha" })))
        .await
        .unwrap();
    let operations = listed.structured_content.unwrap()["operations"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(operations.len(), 2);
    assert_eq!(operations[0]["inputs"][0], "text*");

    let described = client
        .peer()
        .call_tool(params(
            "describe_operation",
            json!({ "source": "alpha", "operation": "list_things" }),
        ))
        .await
        .unwrap();
    assert!(
        described.structured_content.unwrap()["input_schema"]["properties"]["cursor"].is_object()
    );

    let called = client
        .peer()
        .call_tool(params(
            "call_operation",
            json!({ "source": "alpha", "operation": "echo", "input": { "text": "meta" } }),
        ))
        .await
        .unwrap();
    assert_eq!(called.structured_content.unwrap()["text"], "meta");

    let missing = client
        .peer()
        .call_tool(params(
            "call_operation",
            json!({ "source": "alpha", "operation": "write_thing" }),
        ))
        .await
        .unwrap();
    assert_eq!(
        missing.is_error,
        Some(true),
        "hidden operations are not callable"
    );
    client.cancel().await.unwrap();
}
