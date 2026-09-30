//! Built-in source types and the registry builder that instantiates them from configuration.

use crate::config::{BUILTIN_TYPES, Config};
use crate::model::{Registry, RegistryError};

/// Source types mcpie knows how to build.
pub const KNOWN_TYPES: &[&str] = &["github", "slack", "linear", "gdrive", "gmail", "mcp"];

#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    #[error("{0}")]
    Registry(#[from] RegistryError),
    #[error("source {id:?}: {message}")]
    Source { id: String, message: String },
}

/// Build the registry for `config`: one instance per `[sources.<id>]` entry.
pub fn build(config: &Config) -> Result<Registry, BuildError> {
    let registry = Registry::new();
    for id in config.sources.keys() {
        let kind = config.source_type(id).ok_or_else(|| BuildError::Source {
            id: id.clone(),
            message: "missing `type`".into(),
        })?;
        if !KNOWN_TYPES.contains(&kind) {
            return Err(BuildError::Source {
                id: id.clone(),
                message: format!("unknown source type {kind:?}"),
            });
        }
        debug_assert!(BUILTIN_TYPES.contains(&kind) || kind == "mcp");
    }
    Ok(registry)
}
