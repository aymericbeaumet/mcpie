//! The runtime subcommand tree: one subcommand per source, one per read operation, with flags
//! generated from the shared input projection.

use std::io::Read;

use clap::builder::PossibleValuesParser;
use clap::{Arg, ArgAction, ArgMatches, Command};
use serde_json::{Map, Value};

use super::CliError;
use crate::model::names::kebab;
use crate::model::projection::{FieldKind, InputField, coerce, project};
use crate::model::{CallContext, OperationSpec, Registry, SourceError};

pub const INPUT_ARG: &str = "input";
pub const ALL_ARG: &str = "all";

/// Graft every source and read operation onto `command`.
pub fn build_command(mut command: Command, registry: &Registry) -> Command {
    for source in registry.sources() {
        let id = source.id();
        let mut sub = Command::new(id.to_owned())
            .about(source.description().to_owned())
            .subcommand_required(true)
            .arg_required_else_help(true)
            .subcommand_value_name("OPERATION");
        if let Some(operations) = registry.operations(id) {
            for spec in operations.into_iter().filter(|s| s.is_read()) {
                sub = sub.subcommand(operation_command(spec));
            }
        }
        command = command.subcommand(sub);
    }
    command
}

/// The subcommand for one operation.
pub fn operation_command(spec: &OperationSpec) -> Command {
    let mut command = Command::new(kebab(&spec.name)).about(spec.description.to_lowercase());
    for field in project(&spec.input_schema) {
        command = command.arg(arg_for(&field));
    }
    command = command.arg(
        Arg::new(INPUT_ARG)
            .long(INPUT_ARG)
            .value_name("JSON|@FILE|-")
            .help("input object as json, a @file, or - for stdin; flags override its fields")
            .action(ArgAction::Set),
    );
    if spec.paginated {
        command = command.arg(
            Arg::new(ALL_ARG)
                .long(ALL_ARG)
                .action(ArgAction::SetTrue)
                .help("follow next_cursor until exhausted and return every item"),
        );
    }
    command
}

fn arg_for(field: &InputField) -> Arg {
    let mut help = field.description.clone().unwrap_or_default();
    if let Some(default) = &field.default {
        if !help.is_empty() {
            help.push(' ');
        }
        help.push_str(&format!("[default: {default}]"));
    }
    let arg = Arg::new(field.name.clone())
        .long(kebab(&field.name))
        .help(help);
    typed_arg(arg, &field.kind)
}

fn typed_arg(arg: Arg, kind: &FieldKind) -> Arg {
    match kind {
        FieldKind::Str => arg.value_name("TEXT").action(ArgAction::Set),
        FieldKind::I64 => arg
            .value_name("INT")
            .value_parser(clap::value_parser!(i64))
            .action(ArgAction::Set),
        FieldKind::F64 => arg
            .value_name("NUMBER")
            .value_parser(clap::value_parser!(f64))
            .action(ArgAction::Set),
        FieldKind::Bool => arg
            .value_name("BOOL")
            .value_parser(clap::value_parser!(bool))
            .num_args(0..=1)
            .require_equals(true)
            .default_missing_value("true")
            .action(ArgAction::Set),
        FieldKind::Enum(variants) => arg
            .value_name("VALUE")
            .value_parser(PossibleValuesParser::new(
                variants.iter().map(|v| v.value.clone()).collect::<Vec<_>>(),
            ))
            .action(ArgAction::Set),
        FieldKind::List(inner) => {
            let inner_arg = typed_arg(arg, inner);
            inner_arg
                .num_args(1)
                .require_equals(false)
                .action(ArgAction::Append)
        }
        FieldKind::Json => arg.value_name("JSON").action(ArgAction::Set),
    }
}

/// A parsed invocation of `mcpie <source> <operation> ...`.
#[derive(Debug)]
pub struct Call {
    pub source: String,
    pub operation: String,
    pub input: Map<String, Value>,
    pub all: bool,
}

/// Turn the dynamic part of the matches into a call, or `None` when a static command matched.
pub fn parse_call(
    registry: &Registry,
    matches: &ArgMatches,
    stdin: &mut dyn Read,
) -> Result<Option<Call>, CliError> {
    let Some((source_id, source_matches)) = matches.subcommand() else {
        return Ok(None);
    };
    if registry.source(source_id).is_none() {
        return Ok(None);
    }
    let Some((operation_name, operation_matches)) = source_matches.subcommand() else {
        return Ok(None);
    };
    let spec = registry
        .operations(source_id)
        .into_iter()
        .flatten()
        .find(|s| kebab(&s.name) == operation_name)
        .ok_or_else(|| {
            CliError::Usage(format!(
                "unknown operation {operation_name} for source {source_id}"
            ))
        })?;
    let fields = project(&spec.input_schema);

    let mut input = match operation_matches.get_one::<String>(INPUT_ARG) {
        Some(raw) => read_input(raw, stdin)?,
        None => Map::new(),
    };
    for field in &fields {
        if let Some(value) = flag_value(operation_matches, field)? {
            input.insert(field.name.clone(), value);
        }
    }
    let missing: Vec<String> = fields
        .iter()
        .filter(|f| f.required && !input.contains_key(&f.name))
        .map(|f| format!("--{}", kebab(&f.name)))
        .collect();
    if !missing.is_empty() {
        return Err(CliError::Usage(format!(
            "the following required arguments were not provided: {}",
            missing.join(", ")
        )));
    }
    let all = spec.paginated && operation_matches.get_flag(ALL_ARG);
    Ok(Some(Call {
        source: source_id.to_owned(),
        operation: spec.name.clone(),
        input,
        all,
    }))
}

fn read_input(raw: &str, stdin: &mut dyn Read) -> Result<Map<String, Value>, CliError> {
    let text = if raw == "-" {
        let mut text = String::new();
        stdin
            .read_to_string(&mut text)
            .map_err(|e| CliError::Failure(format!("cannot read stdin: {e}")))?;
        text
    } else if let Some(path) = raw.strip_prefix('@') {
        std::fs::read_to_string(path)
            .map_err(|e| CliError::Failure(format!("cannot read {path}: {e}")))?
    } else {
        raw.to_owned()
    };
    if text.trim().is_empty() {
        return Ok(Map::new());
    }
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(map)) => Ok(map),
        Ok(_) => Err(CliError::Usage("--input must be a JSON object".into())),
        Err(error) => Err(CliError::Usage(format!(
            "--input is not valid JSON: {error}"
        ))),
    }
}

fn flag_value(matches: &ArgMatches, field: &InputField) -> Result<Option<Value>, CliError> {
    let name = field.name.as_str();
    if !matches.contains_id(name) {
        return Ok(None);
    }
    let usage = |message: String| CliError::Usage(format!("--{}: {message}", kebab(name)));
    let value = match &field.kind {
        FieldKind::Str | FieldKind::Enum(_) => matches
            .get_one::<String>(name)
            .map(|s| Value::String(s.clone())),
        FieldKind::I64 => matches.get_one::<i64>(name).map(|v| Value::from(*v)),
        FieldKind::F64 => matches.get_one::<f64>(name).map(|v| Value::from(*v)),
        FieldKind::Bool => matches.get_one::<bool>(name).map(|v| Value::Bool(*v)),
        FieldKind::Json => match matches.get_one::<String>(name) {
            Some(raw) => Some(coerce(&FieldKind::Json, raw).map_err(usage)?),
            None => None,
        },
        FieldKind::List(inner) => {
            let values: Option<Vec<Value>> = match inner.as_ref() {
                FieldKind::I64 => matches
                    .get_many::<i64>(name)
                    .map(|v| v.map(|x| Value::from(*x)).collect()),
                FieldKind::F64 => matches
                    .get_many::<f64>(name)
                    .map(|v| v.map(|x| Value::from(*x)).collect()),
                FieldKind::Bool => matches
                    .get_many::<bool>(name)
                    .map(|v| v.map(|x| Value::Bool(*x)).collect()),
                FieldKind::Json => match matches.get_many::<String>(name) {
                    Some(raws) => Some(
                        raws.map(|r| coerce(&FieldKind::Json, r))
                            .collect::<Result<Vec<_>, _>>()
                            .map_err(usage)?,
                    ),
                    None => None,
                },
                _ => matches
                    .get_many::<String>(name)
                    .map(|v| v.map(|s| Value::String(s.clone())).collect()),
            };
            values.map(Value::Array)
        }
    };
    Ok(value)
}

/// Run a call, following pages when `--all` was given.
pub async fn run_call(
    registry: &Registry,
    call: Call,
    ctx: &CallContext,
) -> Result<Value, SourceError> {
    if !call.all {
        return registry
            .call(
                &call.source,
                &call.operation,
                Value::Object(call.input),
                ctx,
            )
            .await;
    }
    let mut input = call.input;
    let mut items = Vec::new();
    loop {
        let page = registry
            .call(
                &call.source,
                &call.operation,
                Value::Object(input.clone()),
                ctx,
            )
            .await?;
        match page.get("items").and_then(Value::as_array) {
            Some(found) => items.extend(found.iter().cloned()),
            None => {
                return Err(SourceError::Internal(format!(
                    "{}.{} returned no items array",
                    call.source, call.operation
                )));
            }
        }
        match page.get("next_cursor").and_then(Value::as_str) {
            Some(cursor) => {
                input.insert("cursor".into(), Value::String(cursor.to_owned()));
            }
            None => break,
        }
    }
    Ok(serde_json::json!({ "items": items, "next_cursor": Value::Null }))
}
