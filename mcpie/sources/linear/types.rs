//! Linear operation inputs. Doc comments become schema descriptions and CLI help.

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Map, Value};

/// No input.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Empty {}

/// List teams.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListTeams {
    /// Page size, at most 250.
    #[serde(default)]
    pub limit: Option<u32>,
    /// Cursor from a previous page.
    #[serde(default)]
    pub cursor: Option<String>,
}

/// List projects, optionally for one team.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListProjects {
    /// Team key (e.g. ENG). Defaults to the configured default_team; omit to list every team's.
    #[serde(default)]
    pub team: Option<String>,
    /// Page size, at most 250.
    #[serde(default)]
    pub limit: Option<u32>,
    /// Cursor from a previous page.
    #[serde(default)]
    pub cursor: Option<String>,
}

/// List cycles of a team.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListCycles {
    /// Team key (e.g. ENG). Defaults to the configured default_team.
    #[serde(default)]
    pub team: Option<String>,
    /// Page size, at most 250.
    #[serde(default)]
    pub limit: Option<u32>,
    /// Cursor from a previous page.
    #[serde(default)]
    pub cursor: Option<String>,
}

/// List issues, most recently updated first.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListIssues {
    /// Team key (e.g. ENG). Defaults to the configured default_team; omit for every team.
    #[serde(default)]
    pub team: Option<String>,
    /// Workflow state name, e.g. Todo, In Progress, Done (case-insensitive).
    #[serde(default)]
    pub state: Option<String>,
    /// Assignee email or display name.
    #[serde(default)]
    pub assignee: Option<String>,
    /// Project name (case-insensitive).
    #[serde(default)]
    pub project: Option<String>,
    /// Only issues updated at or after this ISO 8601 timestamp.
    #[serde(default)]
    pub updated_since: Option<String>,
    /// Include completed and canceled issues (default false).
    #[serde(default)]
    pub include_closed: Option<bool>,
    /// Page size, at most 250.
    #[serde(default)]
    pub limit: Option<u32>,
    /// Cursor from a previous page.
    #[serde(default)]
    pub cursor: Option<String>,
}

/// Read one issue.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetIssue {
    /// Issue identifier (ENG-123) or id.
    pub issue: String,
}

/// List the comments of an issue, oldest first.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListComments {
    /// Issue identifier (ENG-123) or id.
    pub issue: String,
    /// Page size, at most 250.
    #[serde(default)]
    pub limit: Option<u32>,
    /// Cursor from a previous page.
    #[serde(default)]
    pub cursor: Option<String>,
}

/// Full-text search over issues.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchIssues {
    /// Search terms.
    pub query: String,
    /// Page size, at most 250.
    #[serde(default)]
    pub limit: Option<u32>,
    /// Cursor from a previous page.
    #[serde(default)]
    pub cursor: Option<String>,
}

/// A raw GraphQL query (mutations are refused).
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Request {
    /// A GraphQL query document.
    pub query: String,
    /// Query variables.
    #[serde(default)]
    pub variables: Option<Map<String, Value>>,
}
