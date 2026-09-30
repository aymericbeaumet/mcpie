//! Built-in source types and the registry builder that instantiates them from configuration.

pub mod github;
pub mod google;
pub mod http;
pub mod linear;
pub mod slack;

use std::sync::Arc;
use std::time::Duration;

use crate::config::{Config, HttpConfig, SourceConfig};
use crate::model::{Registry, RegistryError, SourceOptions};

/// Source types mcpie knows how to build.
pub const KNOWN_TYPES: &[&str] = &["github", "slack", "linear", "gdrive", "gmail", "mcp"];

#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    #[error("{0}")]
    Registry(#[from] RegistryError),
    #[error("source {id:?}: {message}")]
    Source { id: String, message: String },
}

impl BuildError {
    pub fn source(id: &str, message: impl std::fmt::Display) -> Self {
        Self::Source {
            id: id.to_owned(),
            message: message.to_string(),
        }
    }
}

/// Settings shared by every source type, derived from the instance and global configuration.
#[derive(Debug, Clone)]
pub struct Settings {
    pub id: String,
    pub timeout: Duration,
    pub max_in_flight: usize,
}

impl Settings {
    pub fn new(id: &str, source: &SourceConfig, http: &HttpConfig) -> Self {
        Self {
            id: id.to_owned(),
            timeout: Duration::from_secs(
                source
                    .timeout_seconds
                    .unwrap_or(http.timeout_seconds)
                    .max(1),
            ),
            max_in_flight: http.max_in_flight_per_source.max(1),
        }
    }
}

/// Build the registry for `config`: one instance per `[sources.<id>]` entry.
pub fn build(config: &Config) -> Result<Registry, BuildError> {
    let mut registry = Registry::new();
    for (id, source) in &config.sources {
        let kind = config
            .source_type(id)
            .ok_or_else(|| BuildError::source(id, "missing `type`"))?;
        let settings = Settings::new(id, source, &config.http);
        let options = SourceOptions {
            enabled: source.enabled,
            tools: source.tools.clone(),
        };
        match kind {
            "github" => {
                registry.register(Arc::new(github::Github::new(settings, source)?), options)?
            }
            "slack" => {
                registry.register(Arc::new(slack::Slack::new(settings, source)?), options)?
            }
            "linear" => {
                registry.register(Arc::new(linear::Linear::new(settings, source)?), options)?
            }
            "gdrive" => registry.register(
                Arc::new(google::drive::Drive::new(settings, source)?),
                options,
            )?,
            "gmail" => registry.register(
                Arc::new(google::gmail::Gmail::new(settings, source)?),
                options,
            )?,
            "mcp" => {
                // Source types that are not built yet are known but skipped.
            }
            other => {
                return Err(BuildError::source(
                    id,
                    format!("unknown source type {other:?}"),
                ));
            }
        }
    }
    Ok(registry)
}
