use assert_cmd::Command;
use predicates::prelude::*;

fn mcpie() -> Command {
    Command::cargo_bin("mcpie").expect("mcpie binary")
}

#[test]
fn version_prints_the_crate_version() {
    mcpie()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn help_mentions_the_program_name() {
    mcpie()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("mcpie"));
}
