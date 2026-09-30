use assert_cmd::Command;
use httpmock::prelude::*;
use predicates::prelude::*;

fn mcpie(home: &std::path::Path) -> Command {
    let mut command = Command::cargo_bin("mcpie").expect("mcpie binary");
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
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
