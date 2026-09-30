mod common;

use std::ffi::OsString;
use std::sync::Arc;

use common::FakeSource;
use mcpie::config::Config;
use mcpie::facade::cli::{Io, run};
use mcpie::model::{Registry, SourceOptions};
use serde_json::{Value, json};

async fn build(_: &Config) -> Result<Registry, String> {
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
    Ok(registry)
}

struct Run {
    code: u8,
    stdout: String,
    stderr: String,
}

impl Run {
    fn json(&self) -> Value {
        serde_json::from_str(&self.stdout)
            .unwrap_or_else(|e| panic!("stdout is not json ({e}): {}", self.stdout))
    }
}

fn mcpie_with_stdin(args: &[&str], stdin: &str) -> Run {
    let argv: Vec<OsString> = std::iter::once("mcpie")
        .chain(args.iter().copied())
        .map(OsString::from)
        .collect();
    let mut input = std::io::Cursor::new(stdin.as_bytes().to_vec());
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let code = {
        let mut io = Io {
            stdin: &mut input,
            stdout: &mut stdout,
            stderr: &mut stderr,
            tty: false,
        };
        runtime.block_on(run(argv, build, &mut io))
    };
    Run {
        code,
        stdout: String::from_utf8(stdout).unwrap(),
        stderr: String::from_utf8(stderr).unwrap(),
    }
}

fn mcpie(args: &[&str]) -> Run {
    mcpie_with_stdin(args, "")
}

#[test]
fn help_lists_static_commands_and_sources() {
    let run = mcpie(&["--help"]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    for expected in [
        "sources",
        "ops",
        "describe",
        "search",
        "config",
        "completions",
        "alpha",
        "beta",
    ] {
        assert!(
            run.stdout.contains(expected),
            "missing {expected} in {}",
            run.stdout
        );
    }
    let run = mcpie(&["alpha", "echo", "--help"]);
    assert_eq!(run.code, 0);
    for expected in [
        "--text <TEXT>",
        "--times <INT>",
        "--loud[=<BOOL>]",
        "--mode <VALUE>",
        "--tag",
        "--input <JSON|@FILE|->",
    ] {
        assert!(
            run.stdout.contains(expected),
            "missing {expected} in {}",
            run.stdout
        );
    }
    assert!(!run.stdout.contains("--all"), "echo is not paginated");
    assert!(
        mcpie(&["alpha", "list-things", "--help"])
            .stdout
            .contains("--all")
    );
}

#[test]
fn ops_and_describe_expose_read_operations_only() {
    let run = mcpie(&["ops", "alpha"]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    let rows = run.json();
    let names: Vec<&str> = rows
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["echo", "list_things"]);
    assert_eq!(rows[0]["inputs"], "text*, times, loud, mode, tags");
    let run = mcpie(&["ops"]);
    assert_eq!(run.json().as_array().unwrap().len(), 4);
    let run = mcpie(&["describe", "alpha", "list-things"]);
    assert_eq!(run.code, 0);
    let spec = run.json();
    assert_eq!(spec["paginated"], true);
    assert_eq!(spec["mcp_tool"], "alpha_list_things");
    assert!(spec["input_schema"]["properties"]["cursor"].is_object());
    assert_eq!(mcpie(&["ops", "nope"]).code, 2);
    assert_eq!(
        mcpie(&["alpha", "write-thing", "--name", "x"]).code,
        2,
        "write operations are not subcommands"
    );
}

#[test]
fn calls_with_typed_flags() {
    let run = mcpie(&[
        "alpha", "echo", "--text", "hi", "--times", "2", "--loud", "--mode", "markdown", "--tags",
        "a", "--tags", "b",
    ]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(
        run.json(),
        json!({ "text": "HIHI", "tags": ["a", "b"], "mode": "markdown" })
    );
    let run = mcpie(&["alpha", "echo", "--text", "hi", "--loud=false"]);
    assert_eq!(run.json()["text"], "hi");
    let run = mcpie(&["alpha", "echo", "--text", "hi", "--times", "two"]);
    assert_eq!(run.code, 2);
    assert!(run.stderr.contains("two"), "{}", run.stderr);
    let run = mcpie(&["alpha", "echo", "--text", "hi", "--mode", "loud"]);
    assert_eq!(run.code, 2);
    assert!(
        run.stderr.contains("markdown"),
        "possible values are listed: {}",
        run.stderr
    );
}

#[test]
fn merges_input_with_flags_winning() {
    let run = mcpie(&[
        "alpha",
        "echo",
        "--input",
        r#"{"text":"from-input","times":3}"#,
        "--text",
        "flag",
    ]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(run.json()["text"], "flagflagflag");
    let run = mcpie_with_stdin(&["alpha", "echo", "--input", "-"], r#"{"text":"stdin"}"#);
    assert_eq!(run.json()["text"], "stdin");
    let dir = std::env::temp_dir().join(format!("mcpie-cli-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("input.json");
    std::fs::write(&file, r#"{"text":"file"}"#).unwrap();
    let run = mcpie(&["alpha", "echo", "--input", &format!("@{}", file.display())]);
    assert_eq!(run.json()["text"], "file");
    let run = mcpie(&["alpha", "echo", "--input", "[1]"]);
    assert_eq!(run.code, 2);
    assert!(run.stderr.contains("JSON object"));
    let run = mcpie(&["alpha", "echo", "--input", "{nope"]);
    assert_eq!(run.code, 2);
    let run = mcpie(&["alpha", "echo", "--input", r#"{"text":"x","bogus":1}"#]);
    assert_eq!(
        run.code, 2,
        "unknown fields are invalid input: {}",
        run.stderr
    );
    assert!(run.stderr.contains("bogus"));
}

#[test]
fn missing_required_fields_are_usage_errors() {
    let run = mcpie(&["alpha", "echo"]);
    assert_eq!(run.code, 2);
    assert!(run.stderr.contains("--text"), "{}", run.stderr);
    let run = mcpie_with_stdin(&["alpha", "echo", "--input", "-"], "");
    assert_eq!(
        run.code, 2,
        "empty stdin is an empty object, so --text is still missing"
    );
}

#[test]
fn all_follows_every_page() {
    let run = mcpie(&["alpha", "list-things", "--limit", "10"]);
    assert_eq!(run.json()["items"].as_array().unwrap().len(), 10);
    assert!(run.json()["next_cursor"].is_string());
    let run = mcpie(&["alpha", "list-things", "--limit", "10", "--all"]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(run.json()["items"].as_array().unwrap().len(), 25);
    assert!(run.json()["next_cursor"].is_null());
}

#[test]
fn search_reports_failures_on_stderr() {
    let run = mcpie(&["search", "thing-1", "--limit", "3"]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(run.json()["items"].as_array().unwrap().len(), 3);
    assert!(run.stderr.contains("beta"), "{}", run.stderr);
    let run = mcpie(&["search", "thing", "--source", "beta"]);
    assert_eq!(run.code, 1, "every selected source failed");
    let run = mcpie(&["search", "thing", "--kind", "issue", "--format", "table"]);
    assert!(run.stdout.starts_with("SOURCE  KIND"), "{}", run.stdout);
    assert!(!run.stdout.contains("document"));
}

#[test]
fn sources_probe_each_source() {
    let run = mcpie(&["sources"]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    let rows = run.json();
    assert_eq!(rows[0]["id"], "alpha");
    assert_eq!(rows[0]["status"], "ok");
    assert_eq!(rows[0]["identity"], "alpha-user");
    let run = mcpie(&["sources", "--format", "table"]);
    assert!(
        run.stdout.starts_with("ID     TYPE  STATUS"),
        "{}",
        run.stdout
    );
}

#[test]
fn output_formats() {
    let run = mcpie(&["--format", "yaml", "alpha", "echo", "--text", "hi"]);
    assert!(run.stdout.contains("text: hi"), "{}", run.stdout);
    let run = mcpie(&["--format", "table", "alpha", "echo", "--text", "hi"]);
    assert_eq!(run.code, 2);
    assert!(run.stderr.contains("table"));
}

#[test]
fn config_commands_redact_and_locate() {
    let dir = std::env::temp_dir().join(format!("mcpie-cfg-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let path = dir.join("config.toml");
    let path_arg = path.display().to_string();
    let run = mcpie(&["--config", &path_arg, "config", "init"]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(path.is_file());
    assert_eq!(
        mcpie(&["--config", &path_arg, "config", "init"]).code,
        1,
        "refuses to overwrite"
    );
    let run = mcpie(&["--config", &path_arg, "config", "path"]);
    assert_eq!(run.stdout.trim(), path_arg);
    let run = mcpie(&[
        "--config",
        &path_arg,
        "--set",
        "sources.github.token=ghp_secret",
        "config",
        "show",
    ]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(!run.stdout.contains("ghp_secret"));
    assert_eq!(run.json()["sources"]["github"]["token"], "***");
    assert_eq!(run.json()["user_file"], path_arg);
}

#[test]
fn completions_include_runtime_sources() {
    let run = mcpie(&["completions", "zsh"]);
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert!(run.stdout.contains("alpha"));
    assert!(run.stdout.contains("list-things"));
}
