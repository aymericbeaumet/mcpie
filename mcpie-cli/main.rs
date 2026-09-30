use std::ffi::OsString;
use std::io::IsTerminal;
use std::process::ExitCode;

use mcpie::facade::cli::{Io, run};

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
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let stderr = std::io::stderr();
    let tty = stdout.is_terminal();
    let mut io = Io {
        stdin: &mut stdin.lock(),
        stdout: &mut stdout.lock(),
        stderr: &mut stderr.lock(),
        tty,
    };
    let build =
        |config: &mcpie::config::Config| mcpie::sources::build(config).map_err(|e| e.to_string());
    let code = runtime.block_on(run(args, &build, &mut io));
    ExitCode::from(code)
}
