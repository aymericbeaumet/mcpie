//! Slack operation inputs. Doc comments become schema descriptions and CLI help.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// No input.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Empty {}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Asc,
    Desc,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SearchSort {
    Score,
    Timestamp,
}

/// List channels and conversations visible to the token.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListChannels {
    /// Comma-separated conversation types: public_channel, private_channel, mpim, im
    /// (default public_channel).
    #[serde(default)]
    pub types: Option<String>,
    /// Leave out archived channels (default true).
    #[serde(default)]
    pub exclude_archived: Option<bool>,
    /// Page size, at most 1000.
    #[serde(default)]
    pub limit: Option<u32>,
    /// Cursor from a previous page.
    #[serde(default)]
    pub cursor: Option<String>,
}

/// Read the messages of a channel, newest first.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetChannelHistory {
    /// Channel id (C…) or `#name`.
    pub channel: String,
    /// Only messages after this timestamp (`1700000000.000000`).
    #[serde(default)]
    pub oldest: Option<String>,
    /// Only messages before this timestamp.
    #[serde(default)]
    pub latest: Option<String>,
    /// Include messages exactly at `oldest` and `latest`.
    #[serde(default)]
    pub inclusive: Option<bool>,
    /// Page size, at most 1000.
    #[serde(default)]
    pub limit: Option<u32>,
    /// Cursor from a previous page.
    #[serde(default)]
    pub cursor: Option<String>,
}

/// Read a thread: the parent message and its replies.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetThreadReplies {
    /// Channel id (C…) or `#name`.
    pub channel: String,
    /// Timestamp of the parent message.
    pub ts: String,
    /// Page size, at most 1000.
    #[serde(default)]
    pub limit: Option<u32>,
    /// Cursor from a previous page.
    #[serde(default)]
    pub cursor: Option<String>,
}

/// Search messages (needs a user token).
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchMessages {
    /// Slack search syntax, e.g. `release in:#eng from:@jane after:2026-01-01`.
    pub query: String,
    /// Sort by score (default) or timestamp.
    #[serde(default)]
    pub sort: Option<SearchSort>,
    #[serde(default)]
    pub sort_dir: Option<Direction>,
    /// Page size, at most 100.
    #[serde(default)]
    pub limit: Option<u32>,
    /// Cursor from a previous page.
    #[serde(default)]
    pub cursor: Option<String>,
}

/// List workspace members.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListUsers {
    /// Page size, at most 1000.
    #[serde(default)]
    pub limit: Option<u32>,
    /// Cursor from a previous page.
    #[serde(default)]
    pub cursor: Option<String>,
}

/// Read one member's profile.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetUser {
    /// User id (U…) or `@handle`.
    pub user: String,
}

/// Get the permalink of a message.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetPermalink {
    /// Channel id (C…) or `#name`.
    pub channel: String,
    /// Timestamp of the message.
    pub message_ts: String,
}

/// Call any read-only Web API method.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Request {
    /// Method name, e.g. `conversations.info` or `team.info`.
    pub method: String,
    /// Parameters as an object of scalars.
    #[serde(default)]
    pub params: Option<Map<String, Value>>,
}
