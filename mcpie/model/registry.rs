//! The registry: every source, the read-only policy, filters, dispatch and search fan-out.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use globset::{Glob, GlobSet, GlobSetBuilder};
use serde_json::Value;

use super::SourceError;
use super::names::{is_valid_operation_name, is_valid_source_id};
use super::normalized::{SearchQuery, SearchResult, SourceFailure};
use super::projection::lint;
use super::source::{CallContext, Source};
use super::spec::OperationSpec;

/// Per-instance settings from configuration.
#[derive(Debug, Clone)]
pub struct SourceOptions {
    pub enabled: bool,
    /// Glob allow-list over operation names (`list_*`); empty or `*` means everything.
    pub tools: Vec<String>,
}

impl Default for SourceOptions {
    fn default() -> Self {
        Self {
            enabled: true,
            tools: Vec::new(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("invalid source id {0:?}: use lowercase letters, digits and dashes")]
    InvalidSourceId(String),
    #[error("source {0:?} is registered twice")]
    DuplicateSource(String),
    #[error("{source_id}: invalid operation name {operation:?}")]
    InvalidOperationName {
        source_id: String,
        operation: String,
    },
    #[error("{source_id}: operation {operation:?} is registered twice")]
    DuplicateOperation {
        source_id: String,
        operation: String,
    },
    #[error("{source_id}: {problem}")]
    Lint { source_id: String, problem: String },
    #[error("invalid glob {glob:?}: {message}")]
    InvalidGlob { glob: String, message: String },
}

struct Entry {
    source: Arc<dyn Source>,
    enabled: bool,
    tools: Option<GlobSet>,
}

/// Which operations a facade wants to expose, on top of the configuration.
#[derive(Debug, Clone, Default)]
pub struct Selection {
    /// Only these source ids; `None` means every enabled source.
    pub sources: Option<Vec<String>>,
    /// Globs matched against `operation` and `source.operation`; empty means everything.
    pub tools: Vec<String>,
    pub exclude_tools: Vec<String>,
    /// Expose write operations too. Always false in v1 facades.
    pub include_write: bool,
}

/// A source with the operations a [`Selection`] exposes.
pub struct Exposed<'a> {
    pub source: &'a dyn Source,
    pub operations: Vec<&'a OperationSpec>,
}

#[derive(Default)]
pub struct Registry {
    entries: BTreeMap<String, Entry>,
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_map()
            .entries(self.entries.iter().map(|(id, e)| {
                (
                    id,
                    (e.source.kind(), e.enabled, e.source.operations().len()),
                )
            }))
            .finish()
    }
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a source, validating its id, operation names and schemas.
    pub fn register(
        &mut self,
        source: Arc<dyn Source>,
        options: SourceOptions,
    ) -> Result<(), RegistryError> {
        let id = source.id().to_owned();
        if !is_valid_source_id(&id) {
            return Err(RegistryError::InvalidSourceId(id));
        }
        if self.entries.contains_key(&id) {
            return Err(RegistryError::DuplicateSource(id));
        }
        let mut seen = std::collections::BTreeSet::new();
        for spec in source.operations() {
            if !is_valid_operation_name(&spec.name) {
                return Err(RegistryError::InvalidOperationName {
                    source_id: id,
                    operation: spec.name.clone(),
                });
            }
            if !seen.insert(spec.name.as_str()) {
                return Err(RegistryError::DuplicateOperation {
                    source_id: id,
                    operation: spec.name.clone(),
                });
            }
            if let Some(problem) = lint(spec).into_iter().next() {
                return Err(RegistryError::Lint {
                    source_id: id,
                    problem,
                });
            }
        }
        let tools = if options.tools.is_empty() || options.tools.iter().any(|g| g == "*") {
            None
        } else {
            Some(globs(&options.tools)?)
        };
        self.entries.insert(
            id,
            Entry {
                source,
                enabled: options.enabled,
                tools,
            },
        );
        Ok(())
    }

    /// Every registered source, enabled or not, in id order.
    pub fn sources(&self) -> impl Iterator<Item = &dyn Source> {
        self.entries.values().map(|e| e.source.as_ref())
    }

    pub fn source(&self, id: &str) -> Option<&dyn Source> {
        self.entries.get(id).map(|e| e.source.as_ref())
    }

    pub fn is_enabled(&self, id: &str) -> bool {
        self.entries.get(id).is_some_and(|e| e.enabled)
    }

    /// Operations of a source after its configured allow-list, every kind included.
    pub fn operations(&self, id: &str) -> Option<Vec<&OperationSpec>> {
        let entry = self.entries.get(id)?;
        Some(
            entry
                .source
                .operations()
                .iter()
                .filter(|spec| entry.allows(&spec.name))
                .collect(),
        )
    }

    pub fn spec(&self, source: &str, operation: &str) -> Option<&OperationSpec> {
        self.operations(source)?
            .into_iter()
            .find(|spec| spec.name == operation)
    }

    /// Enabled sources and the operations a facade should expose for `selection`.
    pub fn select(&self, selection: &Selection) -> Result<Vec<Exposed<'_>>, RegistryError> {
        let include = if selection.tools.is_empty() {
            None
        } else {
            Some(globs(&selection.tools)?)
        };
        let exclude = if selection.exclude_tools.is_empty() {
            None
        } else {
            Some(globs(&selection.exclude_tools)?)
        };
        let mut exposed = Vec::new();
        for (id, entry) in &self.entries {
            if !entry.enabled {
                continue;
            }
            if let Some(only) = &selection.sources
                && !only.iter().any(|s| s == id)
            {
                continue;
            }
            let operations: Vec<&OperationSpec> = entry
                .source
                .operations()
                .iter()
                .filter(|spec| selection.include_write || spec.is_read())
                .filter(|spec| entry.allows(&spec.name))
                .filter(|spec| {
                    let qualified = format!("{id}.{}", spec.name);
                    include
                        .as_ref()
                        .is_none_or(|g| g.is_match(&spec.name) || g.is_match(&qualified))
                        && !exclude
                            .as_ref()
                            .is_some_and(|g| g.is_match(&spec.name) || g.is_match(&qualified))
                })
                .collect();
            exposed.push(Exposed {
                source: entry.source.as_ref(),
                operations,
            });
        }
        Ok(exposed)
    }

    /// Dispatch one call. Disabled sources answer `NotConfigured`; write operations are refused
    /// unless `allow_write` is set (never, in v1 facades).
    pub async fn call(
        &self,
        source: &str,
        operation: &str,
        input: Value,
        ctx: &CallContext,
    ) -> Result<Value, SourceError> {
        self.call_with_policy(source, operation, input, ctx, false)
            .await
    }

    pub async fn call_with_policy(
        &self,
        source: &str,
        operation: &str,
        input: Value,
        ctx: &CallContext,
        allow_write: bool,
    ) -> Result<Value, SourceError> {
        let entry = self
            .entries
            .get(source)
            .ok_or_else(|| SourceError::UnknownSource(source.to_owned()))?;
        let spec = entry
            .source
            .operations()
            .iter()
            .find(|spec| spec.name == operation)
            .filter(|spec| entry.allows(&spec.name))
            .ok_or_else(|| SourceError::UnknownOperation(format!("{source}.{operation}")))?;
        if !entry.enabled {
            return Err(SourceError::NotConfigured(format!(
                "source {source:?} is disabled"
            )));
        }
        if !spec.is_read() && !allow_write {
            return Err(SourceError::Unsupported(format!(
                "{source}.{operation} writes upstream state; write operations are disabled"
            )));
        }
        match tokio::time::timeout(ctx.timeout, entry.source.call(operation, input, ctx)).await {
            Ok(result) => result,
            Err(_) => Err(SourceError::Timeout(ctx.timeout)),
        }
    }

    /// Fan `query` out to every enabled source that supports search, merge newest first, and
    /// report per-source failures alongside the items instead of failing the whole search.
    pub async fn search(&self, query: SearchQuery, ctx: &CallContext) -> SearchResult {
        let per_source_timeout = Duration::from_secs(10).min(ctx.timeout);
        let targets: Vec<(&str, &dyn Source)> = self
            .entries
            .iter()
            .filter(|(_, e)| e.enabled)
            .filter(|(id, _)| {
                query
                    .sources
                    .as_ref()
                    .is_none_or(|only| only.iter().any(|s| s == *id))
            })
            .filter_map(|(id, e)| e.source.search().map(|_| (id.as_str(), e.source.as_ref())))
            .collect();
        let mut errors: Vec<SourceFailure> = query
            .sources
            .iter()
            .flatten()
            .filter(|wanted| !targets.iter().any(|(id, _)| id == wanted))
            .map(|wanted| SourceFailure {
                source: wanted.clone(),
                code: if self.entries.contains_key(wanted) {
                    "unsupported"
                } else {
                    "unknown_operation"
                }
                .into(),
                message: if self.entries.contains_key(wanted) {
                    format!("source {wanted:?} is disabled or does not support search")
                } else {
                    format!("unknown source {wanted:?}")
                },
            })
            .collect();
        let futures = targets.iter().map(|(id, source)| {
            let query = &query;
            async move {
                let provider = source.search().expect("filtered to searchable sources");
                let outcome =
                    match tokio::time::timeout(per_source_timeout, provider.search(query, ctx))
                        .await
                    {
                        Ok(result) => result,
                        Err(_) => Err(SourceError::Timeout(per_source_timeout)),
                    };
                (*id, outcome)
            }
        });
        let mut items = Vec::new();
        for (id, outcome) in futures::future::join_all(futures).await {
            match outcome {
                Ok(found) => items.extend(found),
                Err(error) => errors.push(SourceFailure {
                    source: id.to_owned(),
                    code: error.code().into(),
                    message: error.to_string(),
                }),
            }
        }
        if let Some(kinds) = &query.kinds {
            items.retain(|item| kinds.contains(&item.kind));
        }
        items.sort_by_key(|item| std::cmp::Reverse(item.updated_at));
        items.truncate(query.effective_limit());
        if !query.include_raw {
            for item in &mut items {
                item.raw = None;
            }
        }
        SearchResult { items, errors }
    }
}

impl Entry {
    fn allows(&self, operation: &str) -> bool {
        self.tools.as_ref().is_none_or(|g| g.is_match(operation))
    }
}

fn globs(patterns: &[String]) -> Result<GlobSet, RegistryError> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        let glob = Glob::new(pattern).map_err(|e| RegistryError::InvalidGlob {
            glob: pattern.clone(),
            message: e.to_string(),
        })?;
        builder.add(glob);
    }
    builder.build().map_err(|e| RegistryError::InvalidGlob {
        glob: patterns.join(","),
        message: e.to_string(),
    })
}
