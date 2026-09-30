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

#[derive(Debug, Clone, Copy, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Open,
    Closed,
    All,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum IssueSort {
    Created,
    Updated,
    Comments,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PullSort {
    Created,
    Updated,
    Popularity,
    LongRunning,
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

/// Identify a repository.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetRepo {
    /// Repository owner. Defaults to the configured default_owner.
    #[serde(default)]
    pub owner: Option<String>,
    /// Repository name. Defaults to the configured default_repo.
    #[serde(default)]
    pub repo: Option<String>,
}

/// List issues of a repository. GitHub returns pull requests here too; they carry a
/// `pull_request` key.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListIssues {
    /// Repository owner. Defaults to the configured default_owner.
    #[serde(default)]
    pub owner: Option<String>,
    /// Repository name. Defaults to the configured default_repo.
    #[serde(default)]
    pub repo: Option<String>,
    /// Filter by state (default open).
    #[serde(default)]
    pub state: Option<State>,
    /// Comma-separated label names; issues must carry all of them.
    #[serde(default)]
    pub labels: Option<String>,
    /// Assignee login, `none` for unassigned or `*` for any assignee.
    #[serde(default)]
    pub assignee: Option<String>,
    /// Creator login.
    #[serde(default)]
    pub creator: Option<String>,
    /// Only issues updated at or after this ISO 8601 timestamp.
    #[serde(default)]
    pub since: Option<String>,
    #[serde(default)]
    pub sort: Option<IssueSort>,
    #[serde(default)]
    pub direction: Option<Direction>,
    /// Page size, at most 100.
    #[serde(default)]
    pub limit: Option<u32>,
    /// Cursor from a previous page.
    #[serde(default)]
    pub cursor: Option<String>,
}

/// Identify an issue or pull request by number.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetIssue {
    /// Repository owner. Defaults to the configured default_owner.
    #[serde(default)]
    pub owner: Option<String>,
    /// Repository name. Defaults to the configured default_repo.
    #[serde(default)]
    pub repo: Option<String>,
    /// Issue or pull request number.
    pub number: u64,
}

/// List the comments of an issue or pull request.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListIssueComments {
    /// Repository owner. Defaults to the configured default_owner.
    #[serde(default)]
    pub owner: Option<String>,
    /// Repository name. Defaults to the configured default_repo.
    #[serde(default)]
    pub repo: Option<String>,
    /// Issue or pull request number.
    pub number: u64,
    /// Only comments updated at or after this ISO 8601 timestamp.
    #[serde(default)]
    pub since: Option<String>,
    /// Page size, at most 100.
    #[serde(default)]
    pub limit: Option<u32>,
    /// Cursor from a previous page.
    #[serde(default)]
    pub cursor: Option<String>,
}

/// List pull requests of a repository.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListPullRequests {
    /// Repository owner. Defaults to the configured default_owner.
    #[serde(default)]
    pub owner: Option<String>,
    /// Repository name. Defaults to the configured default_repo.
    #[serde(default)]
    pub repo: Option<String>,
    /// Filter by state (default open).
    #[serde(default)]
    pub state: Option<State>,
    /// Filter by head, as `user:branch`.
    #[serde(default)]
    pub head: Option<String>,
    /// Filter by base branch name.
    #[serde(default)]
    pub base: Option<String>,
    #[serde(default)]
    pub sort: Option<PullSort>,
    #[serde(default)]
    pub direction: Option<Direction>,
    /// Page size, at most 100.
    #[serde(default)]
    pub limit: Option<u32>,
    /// Cursor from a previous page.
    #[serde(default)]
    pub cursor: Option<String>,
}

/// List commits of a repository.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListCommits {
    /// Repository owner. Defaults to the configured default_owner.
    #[serde(default)]
    pub owner: Option<String>,
    /// Repository name. Defaults to the configured default_repo.
    #[serde(default)]
    pub repo: Option<String>,
    /// Branch, tag or commit SHA to start from (default branch when omitted).
    #[serde(default)]
    pub sha: Option<String>,
    /// Only commits touching this path.
    #[serde(default)]
    pub path: Option<String>,
    /// Only commits by this author (login or email).
    #[serde(default)]
    pub author: Option<String>,
    /// Only commits after this ISO 8601 timestamp.
    #[serde(default)]
    pub since: Option<String>,
    /// Only commits before this ISO 8601 timestamp.
    #[serde(default)]
    pub until: Option<String>,
    /// Page size, at most 100.
    #[serde(default)]
    pub limit: Option<u32>,
    /// Cursor from a previous page.
    #[serde(default)]
    pub cursor: Option<String>,
}

/// Read a file (decoded text) or list a directory.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetFileContent {
    /// Repository owner. Defaults to the configured default_owner.
    #[serde(default)]
    pub owner: Option<String>,
    /// Repository name. Defaults to the configured default_repo.
    #[serde(default)]
    pub repo: Option<String>,
    /// Path inside the repository.
    pub path: String,
    /// Branch, tag or commit (default branch when omitted).
    #[serde(default, rename = "ref")]
    pub reference: Option<String>,
}

/// Search issues and pull requests.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchIssues {
    /// GitHub search syntax, e.g. `repo:acme/widgets is:open label:bug`.
    pub query: String,
    /// comments, reactions, interactions, created or updated (default best match).
    #[serde(default)]
    pub sort: Option<String>,
    #[serde(default)]
    pub direction: Option<Direction>,
    /// Page size, at most 100.
    #[serde(default)]
    pub limit: Option<u32>,
    /// Cursor from a previous page.
    #[serde(default)]
    pub cursor: Option<String>,
}

/// Search file contents.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchCode {
    /// GitHub code search syntax, e.g. `repo:acme/widgets path:src extension:rs Registry`.
    pub query: String,
    /// Page size, at most 100.
    #[serde(default)]
    pub limit: Option<u32>,
    /// Cursor from a previous page.
    #[serde(default)]
    pub cursor: Option<String>,
}

/// Search repositories.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchRepos {
    /// GitHub search syntax, e.g. `language:rust topic:mcp stars:>100`.
    pub query: String,
    /// stars, forks, help-wanted-issues or updated (default best match).
    #[serde(default)]
    pub sort: Option<String>,
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
