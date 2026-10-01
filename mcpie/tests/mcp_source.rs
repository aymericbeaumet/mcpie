mod common;

use std::sync::Arc;
use std::time::Duration;

use common::FakeSource;
use globset::{Glob, GlobSetBuilder};
use mcpie::facade::mcp::{McpServer, Mode};
use mcpie::model::{CallContext, Registry, Selection, Source, SourceError, SourceOptions};
use mcpie::sources::mcp::McpSource;
use rmcp::ServiceExt;
use serde_json::json;

/// A FakeSource behind mcpie's own MCP server, reached over an in-memory pipe.
async fn remote(include_write: bool) -> McpSource {
    let mut registry = Registry::new();
    registry
        .register(Arc::new(FakeSource::new("alpha")), SourceOptions::default())
        .unwrap();
    let selection = Selection {
        include_write,
        ..Default::default()
    };
    let server = McpServer::new(
        Arc::new(registry),
        &selection,
        Mode::Tools,
        Duration::from_secs(5),
    )
    .unwrap();
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
    let service = ().serve((client_read, client_write)).await.expect("client initializes");
    let tools = service.peer().list_all_tools().await.unwrap();
    let mut trusted = GlobSetBuilder::new();
    trusted.add(Glob::new("alpha_write_*").unwrap());
    McpSource::from_service(
        "remote",
        service,
        tools,
        &trusted.build().unwrap(),
        "test pipe".into(),
    )
}

#[tokio::test]
async fn remote_tools_become_operations_and_calls_are_proxied() {
    let source = remote(false).await;
    assert_eq!(source.kind(), "mcp");
    let names: Vec<&str> = source
        .operations()
        .iter()
        .map(|o| o.name.as_str())
        .collect();
    assert_eq!(names, ["alpha_echo", "alpha_list_things", "search"]);
    assert!(
        source.operations().iter().all(|o| o.is_read()),
        "mcpie annotates its tools read-only"
    );
    assert!(source.operations()[0].input_schema["properties"]["text"].is_object());
    assert!(
        source.operations()[1].paginated,
        "cursor inputs are recognised through the proxy"
    );

    let ctx = CallContext::default();
    let out = source
        .call("alpha_echo", json!({ "text": "hi", "times": 2 }), &ctx)
        .await
        .unwrap();
    assert_eq!(out["text"], "hihi");
    let error = source
        .call("alpha_echo", json!({ "text": "hi", "bogus": 1 }), &ctx)
        .await
        .unwrap_err();
    assert!(
        matches!(error, SourceError::Upstream { .. }),
        "in-band tool errors surface as upstream failures: {error}"
    );
    assert!(error.to_string().contains("bogus"));
    let error = source.call("nope", json!({}), &ctx).await.unwrap_err();
    assert!(matches!(error, SourceError::UnknownOperation(_)));

    let status = source.check(&ctx).await.unwrap();
    assert_eq!(status.identity.as_deref(), Some("mcpie 0.1.0"));
    assert_eq!(status.credential.as_deref(), Some("test pipe"));
}

#[tokio::test]
async fn unannotated_tools_are_hidden_unless_trusted() {
    // With include_write the server exposes alpha_write_thing without a read-only annotation.
    let source = remote(true).await;
    let write = source
        .operations()
        .iter()
        .find(|o| o.name == "alpha_write_thing")
        .expect("mapped");
    assert!(
        write.is_read(),
        "trusted_read_only globs promote unannotated tools"
    );
    let mut registry = Registry::new();
    registry
        .register(Arc::new(source), SourceOptions::default())
        .unwrap();
    let exposed = registry.select(&Selection::default()).unwrap();
    assert!(
        exposed[0]
            .operations
            .iter()
            .any(|o| o.name == "alpha_write_thing")
    );
}
