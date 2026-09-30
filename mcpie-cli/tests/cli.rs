use assert_cmd::Command;
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
fn version_prints_the_crate_version() {
    let home = tempfile::tempdir().unwrap();
    mcpie(home.path())
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn help_mentions_the_program_name_and_builtin_sources() {
    let home = tempfile::tempdir().unwrap();
    mcpie(home.path())
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("mcpie"));
    mcpie(home.path())
        .assert()
        .code(2)
        .stderr(predicate::str::contains("Usage"));
}

#[test]
fn config_init_then_show() {
    let home = tempfile::tempdir().unwrap();
    let expected = home.path().join("config").join("mcpie").join("config.toml");
    mcpie(home.path())
        .args(["config", "path"])
        .assert()
        .success()
        .stdout(predicate::str::contains(expected.display().to_string()));
    mcpie(home.path())
        .args(["config", "init"])
        .assert()
        .success();
    assert!(expected.is_file());
    mcpie(home.path())
        .args(["--set", "sources.github.token=secret", "config", "show"])
        .assert()
        .success()
        .stdout(predicate::str::contains("***").and(predicate::str::contains("secret").not()));
}

#[test]
fn completions_and_ops_work_without_configuration() {
    let home = tempfile::tempdir().unwrap();
    mcpie(home.path())
        .args(["completions", "bash"])
        .assert()
        .success()
        .stdout(predicate::str::contains("mcpie"));
    mcpie(home.path()).args(["ops"]).assert().success();
}

#[test]
fn stray_mcpie_environment_variables_are_reported() {
    let home = tempfile::tempdir().unwrap();
    mcpie(home.path())
        .env("MCPIE_TYPO", "1")
        .args(["ops"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains("typo"));
}
