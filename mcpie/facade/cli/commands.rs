//! The static commands: sources, ops, describe, search, config, completions.

use std::io::Write;
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;

use super::output::Tabular;
use super::{CliError, KindArg};
use crate::config::Loaded;
use crate::model::names::kebab;
use crate::model::projection::project;
use crate::model::{CallContext, Item, Registry, SearchQuery, SearchResult, SourceError};

#[derive(Debug, Serialize)]
pub struct SourceRow {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub enabled: bool,
    pub status: String,
    pub identity: Option<String>,
    pub credential: Option<String>,
    pub token_kind: Option<String>,
    pub unavailable: Vec<String>,
    pub message: Option<String>,
}

impl Tabular for SourceRow {
    fn headers() -> Vec<&'static str> {
        vec!["id", "type", "status", "identity", "credential", "message"]
    }

    fn row(&self) -> Vec<String> {
        vec![
            self.id.clone(),
            self.kind.clone(),
            self.status.clone(),
            self.identity.clone().unwrap_or_default(),
            self.credential.clone().unwrap_or_default(),
            self.message.clone().unwrap_or_default(),
        ]
    }
}

/// Probe every source concurrently. Unconfigured or failing sources are rows, not errors.
pub async fn sources(registry: &Registry, ctx: &CallContext) -> Vec<SourceRow> {
    let timeout = Duration::from_secs(10).min(ctx.timeout);
    let probes = registry.sources().map(|source| async move {
        let id = source.id().to_owned();
        let kind = source.kind().to_owned();
        let enabled = registry.is_enabled(&id);
        if !enabled {
            return SourceRow {
                id,
                kind,
                enabled,
                status: "disabled".into(),
                identity: None,
                credential: None,
                token_kind: None,
                unavailable: vec![],
                message: None,
            };
        }
        match tokio::time::timeout(timeout, source.check(ctx)).await {
            Ok(Ok(status)) => SourceRow {
                id,
                kind,
                enabled,
                status: "ok".into(),
                identity: status.identity,
                credential: status.credential,
                token_kind: status.token_kind,
                unavailable: status.unavailable,
                message: (!status.warnings.is_empty()).then(|| status.warnings.join("; ")),
            },
            Ok(Err(error)) => SourceRow {
                id,
                kind,
                enabled,
                status: if matches!(error, SourceError::NotConfigured(_)) {
                    "not configured"
                } else {
                    "error"
                }
                .into(),
                identity: None,
                credential: None,
                token_kind: None,
                unavailable: vec![],
                message: Some(error.to_string()),
            },
            Err(_) => SourceRow {
                id,
                kind,
                enabled,
                status: "error".into(),
                identity: None,
                credential: None,
                token_kind: None,
                unavailable: vec![],
                message: Some(format!("check timed out after {timeout:?}")),
            },
        }
    });
    futures::future::join_all(probes).await
}

#[derive(Debug, Serialize)]
pub struct OpRow {
    pub source: String,
    pub name: String,
    pub title: String,
    pub description: String,
    pub paginated: bool,
    /// Input fields, required ones marked with `*`.
    pub inputs: String,
    pub cli: String,
}

impl Tabular for OpRow {
    fn headers() -> Vec<&'static str> {
        vec!["source", "operation", "inputs", "description"]
    }

    fn row(&self) -> Vec<String> {
        vec![
            self.source.clone(),
            kebab(&self.name),
            self.inputs.clone(),
            self.description.clone(),
        ]
    }
}

pub fn ops(registry: &Registry, source: Option<&str>) -> Result<Vec<OpRow>, CliError> {
    let ids: Vec<&str> = match source {
        Some(id) => {
            registry
                .source(id)
                .ok_or_else(|| CliError::Usage(format!("unknown source {id:?}")))?;
            vec![id]
        }
        None => registry.sources().map(|s| s.id()).collect(),
    };
    let mut rows = Vec::new();
    for id in ids {
        for spec in registry
            .operations(id)
            .into_iter()
            .flatten()
            .filter(|s| s.is_read())
        {
            let inputs = project(&spec.input_schema)
                .iter()
                .map(|f| {
                    if f.required {
                        format!("{}*", kebab(&f.name))
                    } else {
                        kebab(&f.name)
                    }
                })
                .collect::<Vec<_>>()
                .join(", ");
            rows.push(OpRow {
                source: id.to_owned(),
                name: spec.name.clone(),
                title: spec.title.clone(),
                description: spec.description.clone(),
                paginated: spec.paginated,
                inputs,
                cli: format!("{} {id} {}", crate::NAME, kebab(&spec.name)),
            });
        }
    }
    Ok(rows)
}

pub fn describe(registry: &Registry, source: &str, operation: &str) -> Result<Value, CliError> {
    registry
        .source(source)
        .ok_or_else(|| CliError::Usage(format!("unknown source {source:?}")))?;
    let spec = registry
        .spec(source, &crate::model::names::snake(operation))
        .ok_or_else(|| {
            CliError::Usage(format!(
                "unknown operation {operation:?} for source {source:?}"
            ))
        })?;
    let flags = project(&spec.input_schema)
        .iter()
        .map(|f| {
            if f.required {
                format!(" --{} <{}>", kebab(&f.name), kebab(&f.name).to_uppercase())
            } else {
                format!(" [--{}]", kebab(&f.name))
            }
        })
        .collect::<String>();
    let mut value = serde_json::to_value(spec).map_err(|e| CliError::Failure(e.to_string()))?;
    if let Some(object) = value.as_object_mut() {
        object.insert("source".into(), Value::String(source.to_owned()));
        object.insert(
            "cli".into(),
            Value::String(format!(
                "{} {source} {}{flags}",
                crate::NAME,
                kebab(&spec.name)
            )),
        );
        object.insert(
            "mcp_tool".into(),
            Value::String(crate::model::names::mcp_tool_name(source, &spec.name)),
        );
        object.insert(
            "rest".into(),
            Value::String(format!("POST /v1/{source}/{}", spec.name)),
        );
    }
    Ok(value)
}

impl Tabular for Item {
    fn headers() -> Vec<&'static str> {
        vec!["source", "kind", "updated", "title", "snippet", "url"]
    }

    fn row(&self) -> Vec<String> {
        vec![
            self.source.clone(),
            serde_json::to_value(self.kind)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default(),
            self.updated_at
                .map(|t| t.format("%Y-%m-%d").to_string())
                .unwrap_or_default(),
            self.title.clone().unwrap_or_default(),
            self.snippet.clone(),
            self.url.clone().unwrap_or_default(),
        ]
    }
}

pub struct SearchArgs {
    pub query: String,
    pub sources: Vec<String>,
    pub kinds: Vec<KindArg>,
    pub limit: Option<u32>,
    pub raw: bool,
}

/// Run a search; returns the result and whether every selected source failed.
pub async fn search(
    registry: &Registry,
    args: SearchArgs,
    ctx: &CallContext,
) -> (SearchResult, bool) {
    let query = SearchQuery {
        query: args.query,
        sources: (!args.sources.is_empty()).then_some(args.sources),
        kinds: (!args.kinds.is_empty()).then(|| args.kinds.into_iter().map(Into::into).collect()),
        limit: args.limit,
        include_raw: args.raw,
    };
    let result = registry.search(query, ctx).await;
    let all_failed = result.items.is_empty() && !result.errors.is_empty();
    (result, all_failed)
}

pub fn config_show(loaded: &Loaded) -> Result<Value, CliError> {
    let mut value =
        serde_json::to_value(&loaded.config).map_err(|e| CliError::Failure(e.to_string()))?;
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "user_file".into(),
            Value::String(loaded.user_path.display().to_string()),
        );
        object.insert(
            "project_file".into(),
            loaded
                .project_path
                .as_ref()
                .map(|p| Value::String(p.display().to_string()))
                .unwrap_or(Value::Null),
        );
    }
    Ok(value)
}

pub fn config_path(loaded: &Loaded) -> String {
    let mut out = loaded.user_path.display().to_string();
    if let Some(project) = &loaded.project_path {
        out.push('\n');
        out.push_str(&project.display().to_string());
    }
    out
}

pub fn config_init(path: &std::path::Path) -> Result<String, CliError> {
    crate::config::init(path).map_err(|e| CliError::Failure(e.to_string()))?;
    Ok(format!("wrote {}", path.display()))
}

pub fn completions(
    command: &mut clap::Command,
    shell: clap_complete::Shell,
    out: &mut dyn Write,
) -> Result<(), CliError> {
    let mut buffer = Vec::new();
    clap_complete::generate(shell, command, crate::NAME, &mut buffer);
    out.write_all(&buffer).map_err(CliError::from)
}

/// `mcpie auth <source>`: run the OAuth loopback flow and store the refresh token.
pub async fn auth(
    loaded: &Loaded,
    source: &str,
    client_id: Option<String>,
    client_secret: Option<String>,
    no_browser: bool,
    stderr: &mut dyn Write,
) -> Result<String, CliError> {
    use crate::sources::google::{self, oauth};
    let kind = loaded
        .config
        .source_type(source)
        .ok_or_else(|| CliError::Usage(format!("unknown source {source:?}")))?;
    let scope = google::scope_for(kind).ok_or_else(|| {
        CliError::Usage(format!(
            "source {source:?} is {kind}; only gdrive and gmail use `mcpie auth`"
        ))
    })?;
    let source_config = &loaded.config.sources[source];
    let extra: google::Extra = source_config.extra().map_err(CliError::Failure)?;
    let (client_id, client_secret) = match (client_id, client_secret, &extra.oauth) {
        (Some(id), Some(secret), _) => (id, crate::config::Secret::new(secret)),
        (None, None, Some(oauth)) => (oauth.client_id.clone(), oauth.client_secret.clone()),
        (Some(id), None, Some(oauth)) => (id, oauth.client_secret.clone()),
        (None, Some(secret), Some(oauth)) => {
            (oauth.client_id.clone(), crate::config::Secret::new(secret))
        }
        _ => {
            return Err(CliError::Usage(
                "an OAuth client is required: create a Desktop app client in Google Cloud (APIs & Services > Credentials), \
                 then pass --client-id and --client-secret or set sources.<id>.oauth"
                    .into(),
            ));
        }
    };
    let authorization = oauth::Authorization {
        client_id: client_id.clone(),
        client_secret: client_secret.clone(),
        scopes: vec![scope.to_owned()],
        auth_url: extra
            .auth_url
            .clone()
            .unwrap_or_else(|| google::AUTH_URL.to_owned()),
        token_url: extra
            .token_url
            .clone()
            .unwrap_or_else(|| google::TOKEN_URL.to_owned()),
    };
    let http = crate::sources::http::Http::new("", Duration::from_secs(30), 2)
        .map_err(|e| CliError::Failure(e.to_string()))?;
    let tokens = oauth::authorize(
        &http,
        &authorization,
        |url| {
            let _ = writeln!(stderr, "open this url to authorize {source}:\n\n  {url}\n");
            if !no_browser && open::that_detached(url).is_err() {
                let _ = writeln!(stderr, "(could not open a browser; paste the url manually)");
            }
            let _ = writeln!(stderr, "waiting for the browser to redirect back...");
        },
        Duration::from_secs(300),
    )
    .await?;
    let refresh_token = tokens
        .refresh_token
        .ok_or_else(|| CliError::Failure("google returned no refresh token; revoke mcpie's access in your google account and retry".into()))?;
    oauth::save(
        &loaded.user_path,
        source,
        &client_id,
        &client_secret,
        &refresh_token,
    )
    .map_err(CliError::Failure)?;
    Ok(format!(
        "authorized {source}; refresh token saved to {}",
        loaded.user_path.display()
    ))
}
