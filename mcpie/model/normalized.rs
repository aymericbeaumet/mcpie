//! The small cross-source data model behind `search`.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{SourceError, source::CallContext};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    Message,
    Issue,
    PullRequest,
    Document,
    Person,
}

/// A native operation call that returns the full object behind an [`Item`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct OperationRef {
    pub source: String,
    pub operation: String,
    pub input: Value,
}

/// A search hit. Short by design: `fetch` says how to read the full object.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Item {
    pub kind: ItemKind,
    pub source: String,
    /// The source's native identifier, never rewritten.
    pub id: String,
    pub title: Option<String>,
    pub snippet: String,
    pub url: Option<String>,
    pub author: Option<String>,
    pub updated_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetch: Option<OperationRef>,
    /// The upstream object, present only when the query asked for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<Value>,
}

/// Cross-source search parameters.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchQuery {
    /// Free-text query passed to every selected source.
    pub query: String,
    /// Restrict to these source ids. Defaults to every enabled source that supports search.
    #[serde(default)]
    pub sources: Option<Vec<String>>,
    /// Restrict to these item kinds.
    #[serde(default)]
    pub kinds: Option<Vec<ItemKind>>,
    /// Maximum number of items across all sources (default 20, at most 100).
    #[serde(default)]
    pub limit: Option<u32>,
    /// Include each item's upstream object in `raw`.
    #[serde(default)]
    pub include_raw: bool,
}

impl SearchQuery {
    pub const DEFAULT_LIMIT: u32 = 20;
    pub const MAX_LIMIT: u32 = 100;

    pub fn effective_limit(&self) -> usize {
        self.limit
            .unwrap_or(Self::DEFAULT_LIMIT)
            .clamp(1, Self::MAX_LIMIT) as usize
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SourceFailure {
    pub source: String,
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct SearchResult {
    pub items: Vec<Item>,
    /// Sources that failed; the items above come from the ones that succeeded.
    pub errors: Vec<SourceFailure>,
}

/// Implemented by sources that can answer a [`SearchQuery`].
#[async_trait]
pub trait SearchProvider: Send + Sync {
    async fn search(
        &self,
        query: &SearchQuery,
        ctx: &CallContext,
    ) -> Result<Vec<Item>, SourceError>;
}
