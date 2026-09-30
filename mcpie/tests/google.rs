use std::sync::Arc;
use std::time::Duration;

use httpmock::prelude::*;
use mcpie::config::{Config, Secret, SourceConfig};
use mcpie::model::{CallContext, Registry, Source, SourceError, SourceOptions};
use mcpie::sources::Settings;
use mcpie::sources::google::drive::Drive;
use mcpie::sources::google::gmail::Gmail;
use mcpie::sources::google::oauth::{Authorization, authorize};
use mcpie::sources::http::Http;
use serde_json::{Value, json};

fn config(server: &MockServer, token: Option<&str>, oauth: bool) -> SourceConfig {
    let mut config = SourceConfig {
        token: token.map(Secret::new),
        base_url: Some(server.base_url()),
        ..SourceConfig::default()
    };
    if oauth {
        config.extra.insert(
            "oauth".into(),
            json!({ "client_id": "cid", "client_secret": "csecret", "refresh_token": "rtoken" }),
        );
        config.extra.insert(
            "token_url".into(),
            json!(format!("{}/token", server.base_url())),
        );
    }
    config
}

fn drive(server: &MockServer, token: Option<&str>, oauth: bool) -> Drive {
    let config = config(server, token, oauth);
    Drive::new(
        Settings::new("gdrive", &config, &Config::default().http),
        &config,
    )
    .unwrap()
}

fn gmail(server: &MockServer, token: &str) -> Gmail {
    let config = config(server, Some(token), false);
    Gmail::new(
        Settings::new("gmail", &config, &Config::default().http),
        &config,
    )
    .unwrap()
}

fn with(source: impl Source) -> Registry {
    let mut registry = Registry::new();
    registry
        .register(Arc::new(source), SourceOptions::default())
        .unwrap();
    registry
}

#[tokio::test]
async fn refresh_tokens_are_exchanged_once_and_cached() {
    let server = MockServer::start();
    let token = server.mock(|when, then| {
        when.method(POST)
            .path("/token")
            .form_urlencoded_tuple("grant_type", "refresh_token")
            .form_urlencoded_tuple("refresh_token", "rtoken")
            .form_urlencoded_tuple("client_id", "cid");
        then.status(200).json_body(
            json!({ "access_token": "at-1", "expires_in": 3600, "token_type": "Bearer" }),
        );
    });
    let about = server.mock(|when, then| {
        when.method(GET)
            .path("/about")
            .header("authorization", "Bearer at-1");
        then.status(200)
            .json_body(json!({ "user": { "emailAddress": "jane@example.com" } }));
    });
    let registry = with(drive(&server, None, true));
    let ctx = CallContext::default();
    let status = registry
        .source("gdrive")
        .unwrap()
        .check(&ctx)
        .await
        .unwrap();
    assert_eq!(status.identity.as_deref(), Some("jane@example.com"));
    assert_eq!(status.credential.as_deref(), Some("oauth refresh token"));
    registry
        .source("gdrive")
        .unwrap()
        .check(&ctx)
        .await
        .unwrap();
    assert_eq!(
        token.calls(),
        1,
        "the access token is cached until it expires"
    );
    assert_eq!(about.calls(), 2);
}

#[tokio::test]
async fn drive_lists_exports_and_downloads() {
    let server = MockServer::start();
    let list = server.mock(|when, then| {
        when.method(GET).path("/files").query_param("q", "(name contains 'plan') and 'folder1' in parents and trashed = false").query_param("pageSize", "2").query_param("orderBy", "modifiedTime desc");
        then.status(200).json_body(json!({ "files": [{ "id": "f1", "name": "Plan" }, { "id": "f2", "name": "Plan B" }], "nextPageToken": "tok2" }));
    });
    let second = server.mock(|when, then| {
        when.method(GET)
            .path("/files")
            .query_param("pageToken", "tok2");
        then.status(200)
            .json_body(json!({ "files": [{ "id": "f3", "name": "Plan C" }] }));
    });
    let export = server.mock(|when, then| {
        when.method(GET)
            .path("/files/f1/export")
            .query_param("mimeType", "text/markdown");
        then.status(200)
            .header("content-type", "text/markdown")
            .body("# Plan\n\nhello");
    });
    let media = server.mock(|when, then| {
        when.method(GET)
            .path("/files/f2")
            .query_param("alt", "media");
        then.status(200)
            .header("content-type", "text/plain")
            .body("plain text");
    });
    let registry = with(drive(&server, Some("static-token"), false));
    let ctx = CallContext::default();
    let page = registry
        .call(
            "gdrive",
            "list_files",
            json!({ "q": "name contains 'plan'", "folder": "folder1", "limit": 2 }),
            &ctx,
        )
        .await
        .unwrap();
    assert_eq!(page["items"][1]["name"], "Plan B");
    let cursor = page["next_cursor"].as_str().unwrap().to_owned();
    let next = registry
        .call("gdrive", "list_files", json!({ "cursor": cursor }), &ctx)
        .await
        .unwrap();
    assert_eq!(next["items"][0]["id"], "f3");
    assert!(next["next_cursor"].is_null());
    let exported = registry
        .call(
            "gdrive",
            "export_file",
            json!({ "file": "f1", "output": "markdown" }),
            &ctx,
        )
        .await
        .unwrap();
    assert_eq!(exported["content"], "# Plan\n\nhello");
    let content = registry
        .call("gdrive", "get_file_content", json!({ "file": "f2" }), &ctx)
        .await
        .unwrap();
    assert_eq!(content["content"], "plain text");
    assert_eq!(content["encoding"], "utf-8");
    list.assert();
    second.assert();
    export.assert();
    media.assert();
    let error = registry
        .call("gdrive", "get_file", json!({ "file": "a/b" }), &ctx)
        .await
        .unwrap_err();
    assert!(matches!(error, SourceError::InvalidInput(_)), "{error}");
}

#[tokio::test]
async fn gmail_searches_and_decodes_messages() {
    let server = MockServer::start();
    let list = server.mock(|when, then| {
        when.method(GET).path("/users/me/messages").query_param("q", "from:jane").query_param("maxResults", "2");
        then.status(200).json_body(json!({ "messages": [{ "id": "m1", "threadId": "t1" }, { "id": "m2", "threadId": "t1" }], "nextPageToken": "p2" }));
    });
    server.mock(|when, then| {
        when.method(GET).path("/users/me/messages/m1").query_param("format", "metadata").query_param("metadataHeaders", "Subject");
        then.status(200).json_body(json!({ "id": "m1", "snippet": "hello", "payload": { "headers": [{ "name": "Subject", "value": "One" }] } }));
    });
    server.mock(|when, then| {
        when.method(GET).path("/users/me/messages/m2").query_param("format", "metadata");
        then.status(200).json_body(json!({ "id": "m2", "snippet": "world", "payload": { "headers": [{ "name": "Subject", "value": "Two" }] } }));
    });
    let full = server.mock(|when, then| {
        when.method(GET).path("/users/me/messages/m1").query_param("format", "full");
        then.status(200).json_body(json!({ "id": "m1", "payload": { "mimeType": "text/plain", "headers": [{ "name": "From", "value": "jane@example.com" }], "body": { "data": "aGVsbG8" } } }));
    });
    let registry = with(gmail(&server, "static-token"));
    let ctx = CallContext::default();
    let page = registry
        .call(
            "gmail",
            "search_messages",
            json!({ "query": "from:jane", "limit": 2 }),
            &ctx,
        )
        .await
        .unwrap();
    assert_eq!(page["items"][0]["subject"], "One");
    assert_eq!(page["items"][1]["snippet"], "world");
    assert!(page["next_cursor"].is_string());
    let message = registry
        .call("gmail", "get_message", json!({ "message": "m1" }), &ctx)
        .await
        .unwrap();
    assert_eq!(message["text"], "hello");
    assert_eq!(message["from"], "jane@example.com");
    list.assert();
    full.assert();
    let error = registry
        .call("gmail", "get_message", json!({ "message": "../x" }), &ctx)
        .await
        .unwrap_err();
    assert!(matches!(error, SourceError::InvalidInput(_)), "{error}");
}

#[tokio::test]
async fn unconfigured_google_sources_say_how_to_authorize() {
    let server = MockServer::start();
    let registry = with(drive(&server, None, false));
    let error = registry
        .source("gdrive")
        .unwrap()
        .check(&CallContext::default())
        .await
        .unwrap_err();
    assert!(matches!(error, SourceError::NotConfigured(_)), "{error}");
    assert!(error.to_string().contains("mcpie auth gdrive"), "{error}");
}

#[tokio::test]
async fn loopback_flow_exchanges_the_code() {
    let server = MockServer::start();
    let exchange = server.mock(|when, then| {
        when.method(POST).path("/token").form_urlencoded_tuple("grant_type", "authorization_code").form_urlencoded_tuple("code", "4/abc").form_urlencoded_tuple("client_id", "cid");
        then.status(200).json_body(json!({ "access_token": "at", "refresh_token": "rt", "expires_in": 3599, "scope": "drive.readonly" }));
    });
    let http = Http::new("", Duration::from_secs(5), 2).unwrap();
    let auth = Authorization {
        client_id: "cid".into(),
        client_secret: Secret::new("csecret"),
        scopes: vec!["drive.readonly".into()],
        auth_url: "https://accounts.example/auth".into(),
        token_url: format!("{}/token", server.base_url()),
    };
    let tokens = authorize(
        &http,
        &auth,
        |url| {
            // Play the browser: follow the redirect_uri from the consent url with the same state.
            let query = url.split_once('?').unwrap().1;
            let mut redirect = None;
            let mut state = None;
            for (k, v) in form_urlencoded::parse(query.as_bytes()) {
                match k.as_ref() {
                    "redirect_uri" => redirect = Some(v.into_owned()),
                    "state" => state = Some(v.into_owned()),
                    _ => {}
                }
            }
            let target = format!(
                "{}?state={}&code=4%2Fabc",
                redirect.unwrap(),
                state.unwrap()
            );
            std::thread::spawn(move || {
                use std::io::{Read, Write};
                let address = target
                    .trim_start_matches("http://")
                    .split('/')
                    .next()
                    .unwrap()
                    .to_owned();
                let mut stream = std::net::TcpStream::connect(address).unwrap();
                let path = target
                    .splitn(4, '/')
                    .nth(3)
                    .map(|p| format!("/{p}"))
                    .unwrap();
                write!(
                    stream,
                    "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
                )
                .unwrap();
                let mut response = String::new();
                stream.read_to_string(&mut response).unwrap();
                assert!(response.starts_with("HTTP/1.1 200"), "{response}");
            });
        },
        Duration::from_secs(10),
    )
    .await
    .unwrap();
    assert_eq!(tokens.refresh_token.unwrap().expose(), "rt");
    assert_eq!(tokens.access_token.expose(), "at");
    exchange.assert();
}

#[test]
fn every_google_operation_is_dispatched() {
    let server = MockServer::start();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let drive = drive(&server, Some("t"), false);
    for spec in drive.operations() {
        let error = runtime
            .block_on(drive.call(&spec.name, json!({}), &CallContext::default()))
            .unwrap_err();
        assert!(
            !matches!(error, SourceError::UnknownOperation(_)),
            "{} is not dispatched",
            spec.name
        );
    }
    let gmail = gmail(&server, "t");
    for spec in gmail.operations() {
        let error = runtime
            .block_on(gmail.call(&spec.name, json!({}), &CallContext::default()))
            .unwrap_err();
        assert!(
            !matches!(error, SourceError::UnknownOperation(_)),
            "{} is not dispatched",
            spec.name
        );
    }
    assert_eq!(drive.operations().len(), 7);
    assert_eq!(gmail.operations().len(), 8);
    let _: Value = json!(null);
}
