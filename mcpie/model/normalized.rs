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

/// Collapse whitespace and cut to `max_chars`, marking the cut with an ellipsis.
pub fn snippet(text: &str, max_chars: usize) -> String {
    let collapsed: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= max_chars {
        return collapsed;
    }
    let mut cut: String = collapsed
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect();
    cut.push('…');
    cut
}

/// Parse the timestamp formats upstreams use: RFC 3339, unix seconds (possibly fractional, as
/// Slack sends them), or unix milliseconds (as Gmail sends them).
pub fn parse_time(value: &Value) -> Option<DateTime<Utc>> {
    match value {
        Value::String(text) => {
            let text = text.trim();
            if let Ok(parsed) = DateTime::parse_from_rfc3339(text) {
                return Some(parsed.with_timezone(&Utc));
            }
            if let Ok(number) = text.parse::<f64>() {
                return from_unix(number);
            }
            chrono::DateTime::parse_from_rfc2822(text)
                .ok()
                .map(|t| t.with_timezone(&Utc))
        }
        Value::Number(number) => number.as_f64().and_then(from_unix),
        _ => None,
    }
}

fn from_unix(number: f64) -> Option<DateTime<Utc>> {
    // Anything past year 2286 in seconds is milliseconds.
    let seconds = if number > 1e11 {
        number / 1000.0
    } else {
        number
    };
    let whole = seconds.trunc() as i64;
    let nanos = ((seconds - seconds.trunc()) * 1e9) as u32;
    DateTime::from_timestamp(whole, nanos)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snippets_and_times() {
        assert_eq!(snippet("  a\n\nb   c ", 10), "a b c");
        assert_eq!(snippet("abcdefghij", 5), "abcd…");
        assert_eq!(
            parse_time(&Value::String("2026-01-02T03:04:05Z".into()))
                .unwrap()
                .to_rfc3339(),
            "2026-01-02T03:04:05+00:00"
        );
        assert_eq!(
            parse_time(&Value::String("1700000000.123456".into()))
                .unwrap()
                .timestamp(),
            1700000000
        );
        assert_eq!(
            parse_time(&Value::String("1700000000123".into()))
                .unwrap()
                .timestamp(),
            1700000000
        );
        assert_eq!(
            parse_time(&Value::from(1700000000)).unwrap().timestamp(),
            1700000000
        );
        assert!(parse_time(&Value::String("Tue, 1 Jul 2003 10:52:37 +0200".into())).is_some());
        assert!(parse_time(&Value::String("nope".into())).is_none());
    }
}
