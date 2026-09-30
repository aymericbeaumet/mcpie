use std::process::ExitCode;

use clap::Parser;

/// query slack, github and other sources through one cli, mcp server, rest api or graphql endpoint
#[derive(Debug, Parser)]
#[command(name = mcpie::NAME, version, author, about)]
struct Cli {}

fn main() -> ExitCode {
    let _cli = Cli::parse();
    ExitCode::SUCCESS
}
