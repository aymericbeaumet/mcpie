use assert_cmd::Command;
use httpmock::prelude::*;
use predicates::prelude::*;

fn mcpie(home: &std::path::Path) -> Command {
    let mut command = Command::cargo_bin("mcpie").expect("mcpie binary");
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .envs(platform_env())
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .current_dir(home);
    command
}

#[test]
fn calls_github_through_environment_configuration() {
    let home = tempfile::tempdir().unwrap();
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET)
            .path("/user")
            .header("authorization", "Bearer env-token");
        then.status(200)
            .json_body(serde_json::json!({ "login": "octocat" }));
    });
    mcpie(home.path())
        .env("MCPIE_SOURCES__GITHUB__BASE_URL", server.base_url())
        .env("MCPIE_SOURCES__GITHUB__TOKEN", "env-token")
        .args(["github", "get-viewer"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"login\":\"octocat\""));
}

#[test]
fn missing_token_fails_clearly() {
    let home = tempfile::tempdir().unwrap();
    mcpie(home.path())
        .args([
            "--set",
            "sources.github.token_command=exit 1",
            "github",
            "get-viewer",
        ])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("token_command"));
}

#[test]
fn invalid_input_json_is_a_usage_error() {
    let home = tempfile::tempdir().unwrap();
    mcpie(home.path())
        .args(["github", "list-repos", "--input", "{not json"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("not valid JSON"));
}

#[test]
fn empty_stdin_input_is_fine() {
    let home = tempfile::tempdir().unwrap();
    let server = MockServer::start();
    server.mock(|when, then| {
        when.method(GET).path("/user");
        then.status(200)
            .json_body(serde_json::json!({ "login": "octocat" }));
    });
    mcpie(home.path())
        .env("MCPIE_SOURCES__GITHUB__BASE_URL", server.base_url())
        .env("MCPIE_SOURCES__GITHUB__TOKEN", "t")
        .args(["github", "get-viewer", "--input", "-"])
        .write_stdin("")
        .assert()
        .success();
}

#[test]
fn project_file_with_secrets_is_rejected() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join(".mcpie.toml"),
        "[sources.github]\ntoken = \"leaked\"\n",
    )
    .unwrap();
    mcpie(home.path()).args(["ops"]).assert().code(1).stderr(
        predicate::str::contains("sources.github.token")
            .and(predicate::str::contains("leaked").not()),
    );
}

#[test]
fn broken_pipe_is_success() {
    use std::io::Read;
    use std::process::{Command as StdCommand, Stdio};
    let home = tempfile::tempdir().unwrap();
    let mut child = StdCommand::new(assert_cmd::cargo::cargo_bin("mcpie"))
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .envs(platform_env())
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join("config"))
        .current_dir(home.path())
        .args(["completions", "bash"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let mut first = [0u8; 16];
    let _ = stdout.read(&mut first);
    drop(stdout);
    let status = child.wait().unwrap();
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert!(status.success(), "status {status}, stderr: {stderr}");
    assert!(stderr.is_empty(), "stderr: {stderr}");
}

#[test]
fn mcp_keeps_stdout_for_the_protocol() {
    use std::io::Read;
    use std::process::{Command as StdCommand, Stdio};
    let home = tempfile::tempdir().unwrap();
    let mut child = StdCommand::new(assert_cmd::cargo::cargo_bin("mcpie"))
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .envs(platform_env())
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join("config"))
        .current_dir(home.path())
        .args([
            "-v",
            "mcp",
            "--sources",
            "github",
            "--set",
            "sources.github.token=t",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdin.take());
    let status = child.wait().unwrap();
    let mut stdout = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut stdout)
        .unwrap();
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert!(status.success(), "status {status}, stderr: {stderr}");
    assert!(
        stdout.is_empty(),
        "stdout must stay empty for the protocol: {stdout}"
    );
    assert!(
        stderr.contains("serving mcp"),
        "logs go to stderr: {stderr}"
    );
}

#[test]
fn serve_answers_rest_and_mcp_over_http() {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpStream;
    use std::process::{Command as StdCommand, Stdio};
    let home = tempfile::tempdir().unwrap();
    let mut child = StdCommand::new(assert_cmd::cargo::cargo_bin("mcpie"))
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .envs(platform_env())
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join("config"))
        .current_dir(home.path())
        .args([
            "serve",
            "--bind",
            "127.0.0.1:0",
            "--sources",
            "github",
            "--set",
            "sources.github.token=t",
            "--set",
            "server.token=s3cret",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stderr = child.stderr.take().unwrap();
    let mut lines = BufReader::new(stderr).lines();
    let address = loop {
        let line = lines.next().expect("serve prints its address").unwrap();
        if let Some(rest) = line.split("listening on http://").nth(1) {
            break rest.split_whitespace().next().unwrap().to_owned();
        }
    };
    let request = |raw: &str| -> String {
        let mut stream = TcpStream::connect(&address).unwrap();
        stream.write_all(raw.as_bytes()).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    };
    let health = request("GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    assert!(health.starts_with("HTTP/1.1 200"), "{health}");
    let denied =
        request("GET /v1/sources HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    assert!(denied.starts_with("HTTP/1.1 401"), "{denied}");
    let sources = request(
        "GET /v1/sources HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer s3cret\r\nConnection: close\r\n\r\n",
    );
    assert!(
        sources.starts_with("HTTP/1.1 200") && sources.contains("\"list_issues\""),
        "{sources}"
    );
    let body = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"probe","version":"0"}}}"#;
    let mcp = request(&format!(
        "POST /mcp HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer s3cret\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    ));
    assert!(
        mcp.starts_with("HTTP/1.1 200") && mcp.contains("\"mcpie\""),
        "{mcp}"
    );
    let evil = request("GET /healthz HTTP/1.1\r\nHost: evil.example\r\nConnection: close\r\n\r\n");
    assert!(evil.starts_with("HTTP/1.1 421"), "{evil}");
    child.kill().unwrap();
    child.wait().unwrap();
}

/// Variables the OS needs even in a cleared environment: Winsock cannot open sockets without
/// `SystemRoot`, and Windows temp-file APIs read `TEMP`/`TMP`.
fn platform_env() -> Vec<(String, std::ffi::OsString)> {
    ["SystemRoot", "SYSTEMROOT", "windir", "TEMP", "TMP"]
        .iter()
        .filter(|_| cfg!(windows))
        .filter_map(|name| std::env::var_os(name).map(|value| ((*name).to_owned(), value)))
        .collect()
}
