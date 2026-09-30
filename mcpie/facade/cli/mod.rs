//! The command-line facade.
//!
//! The static commands are a clap derive; the per-source subcommands are built at runtime from
//! the registry (see [`dynamic`]). [`run`] does everything except own the process: the binary
//! hands it argv and its I/O handles and turns the returned code into an exit status.

mod commands;
pub mod dynamic;
mod output;

use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::time::Duration;

use clap::{ArgAction, CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum};
pub use output::Format;
use output::{Tabular, render_table, render_value};

use crate::config::{self, Config, Loader};
use crate::model::{CallContext, ItemKind, Registry, SourceError};

const ABOUT: &str = "query slack, github and other sources through one cli, mcp server, rest api or graphql endpoint";
const LONG_ABOUT: &str = "query slack, github and other sources through one cli, mcp server, rest api or graphql endpoint.

every configured source is a subcommand and every read operation a sub-subcommand:

    mcpie github list-issues --owner acme --repo widgets --limit 5
    mcpie slack search-messages --query release
    mcpie search \"incident 42\"

results go to stdout as json; diagnostics go to stderr.";

#[derive(Debug, Parser)]
#[command(
    name = crate::NAME,
    version,
    author,
    about = ABOUT,
    long_about = LONG_ABOUT,
    disable_help_subcommand = true,
    arg_required_else_help = true,
    subcommand_value_name = "COMMAND|SOURCE"
)]
pub struct Cli {
    /// configuration file [default: ~/.config/mcpie/config.toml]
    #[arg(long, global = true, env = "MCPIE_CONFIG", value_name = "PATH")]
    pub config: Option<PathBuf>,
    /// output format; table is available for sources, ops and search
    #[arg(long, global = true, value_enum, value_name = "FORMAT")]
    pub format: Option<Format>,
    /// override a configuration value, e.g. sources.github.token=ghp_x (repeatable)
    #[arg(long = "set", global = true, value_name = "KEY=VALUE", action = ArgAction::Append)]
    pub sets: Vec<String>,
    /// per-call timeout in seconds [default: http.timeout_seconds]
    #[arg(long, global = true, value_name = "SECONDS")]
    pub timeout: Option<u64>,
    /// log more to stderr (-v info, -vv debug, -vvv trace)
    #[arg(short, long, global = true, action = ArgAction::Count)]
    pub verbose: u8,
    #[command(subcommand)]
    pub command: Option<StaticCommand>,
}

#[derive(Debug, Subcommand)]
pub enum StaticCommand {
    /// list sources with their credential and status
    Sources,
    /// list operations, optionally for one source
    Ops { source: Option<String> },
    /// print the full input and output schema of an operation
    Describe { source: String, operation: String },
    /// search across every source that supports it
    Search {
        query: String,
        /// restrict to a source (repeatable)
        #[arg(long = "source", value_name = "ID")]
        sources: Vec<String>,
        /// restrict to a kind (repeatable)
        #[arg(long = "kind", value_enum, value_name = "KIND")]
        kinds: Vec<KindArg>,
        /// maximum number of items [default: 20]
        #[arg(long, value_name = "N")]
        limit: Option<u32>,
        /// include each item's upstream object
        #[arg(long)]
        raw: bool,
    },
    /// authorize a source through the browser and store its refresh token
    Auth {
        /// source id (gdrive or gmail)
        source: String,
        /// oauth client id (defaults to the source's configured oauth.client_id)
        #[arg(long, value_name = "ID")]
        client_id: Option<String>,
        /// oauth client secret (defaults to the source's configured oauth.client_secret)
        #[arg(long, value_name = "SECRET")]
        client_secret: Option<String>,
        /// print the consent url instead of opening a browser
        #[arg(long)]
        no_browser: bool,
    },
    /// serve the registry as an mcp server over stdin/stdout
    Mcp {
        /// tools: one tool per operation; meta: list_operations, describe_operation, call_operation
        #[arg(long, value_enum, default_value_t = ModeArg::Tools)]
        mode: ModeArg,
        /// only these sources (comma-separated); default: every source with credentials
        #[arg(long = "sources", value_delimiter = ',', value_name = "ID,ID")]
        sources: Vec<String>,
        /// only operations matching these globs, e.g. list_*,github.get_issue
        #[arg(long = "tools", value_delimiter = ',', value_name = "GLOB,GLOB")]
        tools: Vec<String>,
        /// hide operations matching these globs
        #[arg(
            long = "exclude-tools",
            value_delimiter = ',',
            value_name = "GLOB,GLOB"
        )]
        exclude_tools: Vec<String>,
        /// expose every enabled source, even ones whose credentials are missing
        #[arg(long)]
        all: bool,
    },
    /// serve rest, openapi, mcp (and graphql) over http
    Serve {
        /// listen address [default: server.bind, 127.0.0.1:7878]
        #[arg(long, value_name = "HOST:PORT")]
        bind: Option<String>,
        /// allow a non-loopback bind without server.token
        #[arg(long)]
        insecure_no_auth: bool,
        /// mcp tool surface at /mcp: tools or meta
        #[arg(long, value_enum, default_value_t = ModeArg::Tools)]
        mode: ModeArg,
        /// only these sources (comma-separated); default: every source with credentials
        #[arg(long = "sources", value_delimiter = ',', value_name = "ID,ID")]
        sources: Vec<String>,
        /// only operations matching these globs
        #[arg(long = "tools", value_delimiter = ',', value_name = "GLOB,GLOB")]
        tools: Vec<String>,
        /// hide operations matching these globs
        #[arg(
            long = "exclude-tools",
            value_delimiter = ',',
            value_name = "GLOB,GLOB"
        )]
        exclude_tools: Vec<String>,
        /// expose every enabled source, even ones whose credentials are missing
        #[arg(long)]
        all: bool,
    },
    /// show, locate or create the configuration file
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// print shell completions
    Completions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
}

#[derive(Debug, Subcommand)]
pub enum ConfigAction {
    /// print the effective configuration with secrets redacted
    Show,
    /// print the configuration file paths
    Path,
    /// write a commented template to the user configuration path
    Init,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ModeArg {
    Tools,
    Meta,
}

impl From<ModeArg> for crate::facade::mcp::Mode {
    fn from(mode: ModeArg) -> Self {
        match mode {
            ModeArg::Tools => Self::Tools,
            ModeArg::Meta => Self::Meta,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum KindArg {
    Message,
    Issue,
    PullRequest,
    Document,
    Person,
}

impl From<KindArg> for ItemKind {
    fn from(kind: KindArg) -> Self {
        match kind {
            KindArg::Message => ItemKind::Message,
            KindArg::Issue => ItemKind::Issue,
            KindArg::PullRequest => ItemKind::PullRequest,
            KindArg::Document => ItemKind::Document,
            KindArg::Person => ItemKind::Person,
        }
    }
}

/// The process I/O handles `run` writes to.
pub struct Io<'a> {
    pub stdin: &'a mut dyn Read,
    pub stdout: &'a mut dyn Write,
    pub stderr: &'a mut dyn Write,
    /// Whether stdout is a terminal (pretty JSON, tables by default).
    pub tty: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum CliError {
    /// Wrong invocation: exit 2.
    #[error("{0}")]
    Usage(String),
    /// Anything else that stops the command: exit 1.
    #[error("{0}")]
    Failure(String),
    #[error("{0}")]
    Source(#[from] SourceError),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    /// Already reported (clap rendered it); just exit with this code.
    #[error("exit {0}")]
    Exit(u8),
}

impl CliError {
    fn exit_code(&self) -> u8 {
        match self {
            Self::Usage(_) => 2,
            Self::Failure(_) => 1,
            Self::Source(error) => error.exit_code(),
            Self::Io(_) => 1,
            Self::Exit(code) => *code,
        }
    }
}

/// Run the CLI end to end and return the process exit code. `build` turns the loaded
/// configuration into the registry (the binary passes `mcpie::sources::build`).
pub async fn run<B>(args: Vec<OsString>, build: B, io: &mut Io<'_>) -> u8
where
    B: AsyncFn(&Config) -> Result<Registry, String>,
{
    match run_inner(args, build, io).await {
        Ok(()) => 0,
        Err(CliError::Io(error)) if error.kind() == std::io::ErrorKind::BrokenPipe => 0,
        Err(CliError::Exit(code)) => code,
        Err(error) => {
            let _ = writeln!(io.stderr, "{}: {error}", crate::NAME);
            error.exit_code()
        }
    }
}

async fn run_inner<B>(args: Vec<OsString>, build: B, io: &mut Io<'_>) -> Result<(), CliError>
where
    B: AsyncFn(&Config) -> Result<Registry, String>,
{
    let (config_path, sets) = prescan(&args);
    let loaded = config::load(&Loader {
        config_path,
        cwd: None,
        sets,
        read_env: true,
    })
    .map_err(|e| CliError::Failure(e.to_string()))?;
    let registry = std::sync::Arc::new(build(&loaded.config).await.map_err(CliError::Failure)?);
    let mut command = dynamic::build_command(Cli::command(), &registry);
    let matches = match command.clone().try_get_matches_from(&args) {
        Ok(matches) => matches,
        Err(error) => {
            let rendered = error.render();
            let target: &mut dyn Write = if error.use_stderr() {
                io.stderr
            } else {
                io.stdout
            };
            write!(target, "{rendered}")?;
            let code = error.exit_code();
            return if code == 0 {
                Ok(())
            } else {
                Err(CliError::Exit(u8::try_from(code).unwrap_or(1)))
            };
        }
    };
    let cli = Cli::from_arg_matches(&matches).map_err(|e| CliError::Failure(e.to_string()))?;
    init_tracing(cli.verbose);
    if cli.config.is_some()
        && !loaded.user_path.is_file()
        && !matches!(
            cli.command,
            Some(StaticCommand::Config {
                action: ConfigAction::Init
            })
        )
    {
        writeln!(
            io.stderr,
            "{}: warning: configuration file not found: {}",
            crate::NAME,
            loaded.user_path.display()
        )?;
    }
    let ctx = CallContext::new(Duration::from_secs(
        cli.timeout.unwrap_or(loaded.config.http.timeout_seconds),
    ));
    let tty = io.tty;
    let value_format = cli.format.unwrap_or(Format::Json);
    let row_format = cli
        .format
        .unwrap_or(if tty { Format::Table } else { Format::Json });

    match cli.command {
        Some(StaticCommand::Sources) => {
            let rows = commands::sources(&registry, &ctx).await;
            emit_rows(io, &rows, row_format)
        }
        Some(StaticCommand::Ops { source }) => {
            let rows = commands::ops(&registry, source.as_deref())?;
            emit_rows(io, &rows, row_format)
        }
        Some(StaticCommand::Describe { source, operation }) => {
            let value = commands::describe(&registry, &source, &operation)?;
            emit_value(io, &value, value_format)
        }
        Some(StaticCommand::Search {
            query,
            sources,
            kinds,
            limit,
            raw,
        }) => {
            let (result, all_failed) = commands::search(
                &registry,
                commands::SearchArgs {
                    query,
                    sources,
                    kinds,
                    limit,
                    raw,
                },
                &ctx,
            )
            .await;
            for failure in &result.errors {
                writeln!(
                    io.stderr,
                    "{}: {}: {}",
                    crate::NAME,
                    failure.source,
                    failure.message
                )?;
            }
            if row_format == Format::Table {
                emit_rows(io, &result.items, row_format)?;
            } else {
                let value =
                    serde_json::to_value(&result).map_err(|e| CliError::Failure(e.to_string()))?;
                emit_value(io, &value, row_format)?;
            }
            if all_failed {
                Err(CliError::Failure("every selected source failed".into()))
            } else {
                Ok(())
            }
        }
        Some(StaticCommand::Auth {
            source,
            client_id,
            client_secret,
            no_browser,
        }) => {
            let message = commands::auth(
                &loaded,
                &source,
                client_id,
                client_secret,
                no_browser,
                io.stderr,
            )
            .await?;
            write_line(io, &message)
        }
        Some(StaticCommand::Serve {
            bind,
            insecure_no_auth,
            mode,
            sources,
            tools,
            exclude_tools,
            all,
        }) => {
            let selection =
                selection_for(&registry, sources, tools, exclude_tools, all, &ctx).await;
            let options = crate::facade::server::ServeOptions {
                bind: bind.unwrap_or_else(|| loaded.config.server.bind.clone()),
                token: loaded.config.server.token.clone().filter(|t| !t.is_empty()),
                insecure_no_auth,
                selection,
                timeout: ctx.timeout,
                mcp_mode: mode.into(),
            };
            let stderr = &mut *io.stderr;
            crate::facade::server::serve(registry.clone(), options, |addr| {
                let _ = writeln!(stderr, "{}: listening on http://{addr} (rest /v1, docs /docs, openapi /openapi.json, mcp /mcp)", crate::NAME);
            })
            .await
            .map_err(CliError::Failure)
        }
        Some(StaticCommand::Mcp {
            mode,
            sources,
            tools,
            exclude_tools,
            all,
        }) => {
            let selection =
                selection_for(&registry, sources, tools, exclude_tools, all, &ctx).await;
            let server = crate::facade::mcp::McpServer::new(
                registry.clone(),
                &selection,
                mode.into(),
                ctx.timeout,
            )
            .map_err(|e| CliError::Failure(e.to_string()))?;
            tracing::info!(tools = server.tools().len(), mode = ?server.mode(), "serving mcp on stdio");
            crate::facade::mcp::serve_stdio(server)
                .await
                .map_err(CliError::Failure)
        }
        Some(StaticCommand::Config { action }) => match action {
            ConfigAction::Show => emit_value(io, &commands::config_show(&loaded)?, value_format),
            ConfigAction::Path => write_line(io, &commands::config_path(&loaded)),
            ConfigAction::Init => write_line(io, &commands::config_init(&loaded.user_path)?),
        },
        Some(StaticCommand::Completions { shell }) => {
            commands::completions(&mut command, shell, io.stdout)
        }
        None => {
            let call = dynamic::parse_call(&registry, &matches, io.stdin)?.ok_or_else(|| {
                CliError::Usage("expected a command or a source; see --help".into())
            })?;
            let value = dynamic::run_call(&registry, call, &ctx).await?;
            emit_value(io, &value, value_format)
        }
    }
}

fn emit_value(io: &mut Io<'_>, value: &serde_json::Value, format: Format) -> Result<(), CliError> {
    let text = render_value(value, format, io.tty)?;
    write_line(io, text.trim_end())
}

fn emit_rows<T: Tabular + serde::Serialize>(
    io: &mut Io<'_>,
    rows: &[T],
    format: Format,
) -> Result<(), CliError> {
    match format {
        Format::Table => {
            io.stdout.write_all(render_table(rows).as_bytes())?;
            Ok(())
        }
        other => {
            let value = serde_json::to_value(rows).map_err(|e| CliError::Failure(e.to_string()))?;
            emit_value(io, &value, other)
        }
    }
}

fn write_line(io: &mut Io<'_>, text: &str) -> Result<(), CliError> {
    io.stdout.write_all(text.as_bytes())?;
    io.stdout.write_all(b"\n")?;
    io.stdout.flush()?;
    Ok(())
}

/// Extract `--config` and `--set` before the full command exists (the runtime subcommands need
/// the configuration that these flags select).
fn prescan(args: &[OsString]) -> (Option<PathBuf>, Vec<String>) {
    let mut config = None;
    let mut sets = Vec::new();
    let mut iter = args
        .iter()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned());
    while let Some(arg) = iter.next() {
        if arg == "--" {
            break;
        }
        if arg == "--config" {
            config = iter.next().map(PathBuf::from);
        } else if let Some(path) = arg.strip_prefix("--config=") {
            config = Some(PathBuf::from(path));
        } else if arg == "--set" {
            sets.extend(iter.next());
        } else if let Some(set) = arg.strip_prefix("--set=") {
            sets.push(set.to_owned());
        }
    }
    (config, sets)
}

fn init_tracing(verbose: u8) {
    use tracing_subscriber::EnvFilter;
    let level = match verbose {
        0 => "warn",
        1 => "info",
        2 => "debug",
        _ => "trace",
    };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(level));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .try_init();
}

/// The selection for `mcp` and `serve`: explicit sources win; otherwise every enabled source
/// whose credentials resolve, or every enabled source with `--all`.
async fn selection_for(
    registry: &Registry,
    sources: Vec<String>,
    tools: Vec<String>,
    exclude_tools: Vec<String>,
    all: bool,
    ctx: &CallContext,
) -> crate::model::Selection {
    let sources = if !sources.is_empty() {
        Some(sources)
    } else if all {
        None
    } else {
        let hidden = crate::facade::mcp::unconfigured_sources(registry, ctx).await;
        for id in &hidden {
            tracing::info!(source = %id, "hidden: no credentials (use --all or --sources to expose)");
        }
        Some(
            registry
                .sources()
                .map(|s| s.id().to_owned())
                .filter(|id| !hidden.contains(id))
                .collect(),
        )
    };
    crate::model::Selection {
        sources,
        tools,
        exclude_tools,
        include_write: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prescan_reads_config_and_set_flags() {
        let args: Vec<OsString> = [
            "mcpie",
            "--set",
            "a=1",
            "github",
            "--config=/tmp/c.toml",
            "list-issues",
            "--set=b=2",
            "--",
            "--set",
            "c=3",
        ]
        .iter()
        .map(OsString::from)
        .collect();
        let (config, sets) = prescan(&args);
        assert_eq!(config, Some(PathBuf::from("/tmp/c.toml")));
        assert_eq!(sets, ["a=1", "b=2"]);
    }

    #[test]
    fn static_command_is_a_valid_clap_definition() {
        Cli::command().debug_assert();
    }
}
