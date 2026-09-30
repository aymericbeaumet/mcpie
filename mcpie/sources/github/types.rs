//! GitHub operation inputs. Doc comments become schema descriptions and CLI help.

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
pub enum RepoSort {
    Created,
    Updated,
    Pushed,
    FullName,
}

/// List repositories of the authenticated user, a user or an organization.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListRepos {
    /// User or organization login. Defaults to the authenticated user.
    #[serde(default)]
    pub owner: Option<String>,
    /// Which repositories: all, owner, member, public, private, forks or sources.
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    #[serde(default)]
    pub sort: Option<RepoSort>,
    #[serde(default)]
    pub direction: Option<Direction>,
    /// Page size, at most 100.
    #[serde(default)]
    pub limit: Option<u32>,
    /// Cursor from a previous page.
    #[serde(default)]
    pub cursor: Option<String>,
}

/// A raw GET against the GitHub REST API, like `gh api`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Request {
    /// API path starting with `/`, e.g. `/repos/acme/widgets/issues`.
    pub path: String,
    /// Query parameters as an object of scalars.
    #[serde(default)]
    pub query: Option<Map<String, Value>>,
}
