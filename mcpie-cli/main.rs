use std::ffi::OsString;
use std::io::IsTerminal;
use std::process::ExitCode;

use mcpie::facade::cli::{Io, run};

async fn build_registry(config: &mcpie::config::Config) -> Result<mcpie::model::Registry, String> {
    mcpie::sources::build(config)
        .await
        .map_err(|e| e.to_string())
}

fn main() -> ExitCode {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("{}: cannot start runtime: {error}", mcpie::NAME);
            return ExitCode::FAILURE;
        }
    };
    let args: Vec<OsString> = std::env::args_os().collect();
    // The handles are deliberately not locked: `mcpie mcp` hands stdin and stdout to the MCP
    // transport, which takes the same locks internally.
    let mut stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let mut stderr = std::io::stderr();
    let tty = stdout.is_terminal();
    let mut io = Io {
        stdin: &mut stdin,
        stdout: &mut stdout,
        stderr: &mut stderr,
        tty,
    };
    let code = runtime.block_on(run(args, build_registry, &mut io));
    ExitCode::from(code)
}
