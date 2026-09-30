//! Linear over its GraphQL API.

mod client;
mod normalize;
pub mod types;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use self::client::Client;
use self::types::*;
use super::http::Http;
use super::{BuildError, Settings};
use crate::config::SourceConfig;
use crate::model::{
    CallContext, OperationSpec, Page, SearchProvider, Source, SourceError, Status, typed,
};

/// Type-specific settings under `[sources.<id>]`.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Extra {
    /// Team key used when an operation's `team` is omitted.
    pub default_team: Option<String>,
}

const ISSUE_FIELDS: &str = "id identifier title description priority priorityLabel url createdAt updatedAt completedAt canceledAt \
state { name type } assignee { name email displayName } creator { name } team { key name } project { name } cycle { number } \
labels { nodes { name } } parent { identifier }";
const PAGE_INFO: &str = "pageInfo { hasNextPage endCursor }";

pub struct Linear {
    id: String,
    client: Client,
    extra: Extra,
    operations: Vec<OperationSpec>,
}

impl Linear {
    pub fn new(settings: Settings, config: &SourceConfig) -> Result<Self, BuildError> {
        let extra: Extra = config
            .extra()
            .map_err(|e| BuildError::source(&settings.id, e))?;
        let base_url = config
            .base_url
            .as_deref()
            .unwrap_or(client::DEFAULT_BASE_URL);
        let http = Http::new(base_url, settings.timeout, settings.max_in_flight)
            .map_err(|e| BuildError::source(&settings.id, e))?;
        let client = Client::new(
            &settings.id,
            http,
            config.token.clone(),
            config.token_command.clone(),
        );
        Ok(Self {
            id: settings.id,
            client,
            extra,
            operations: operations(),
        })
    }

    fn team(&self, team: Option<String>) -> Option<String> {
        team.or_else(|| self.extra.default_team.clone())
    }

    async fn get_viewer(&self, _: Empty) -> Result<Value, SourceError> {
        let data = self
            .client
            .query(
                "query { viewer { id name email displayName admin organization { name urlKey } } }",
                json!({}),
            )
            .await?;
        Ok(data.get("viewer").cloned().unwrap_or(Value::Null))
    }

    async fn list_teams(&self, input: ListTeams) -> Result<Page<Value>, SourceError> {
        let document = format!(
            "query($first: Int!, $after: String) {{ teams(first: $first, after: $after) {{ nodes {{ id key name description private }} {PAGE_INFO} }} }}"
        );
        self.page(
            "list_teams",
            &document,
            Map::new(),
            &["teams"],
            input.limit,
            input.cursor,
        )
        .await
    }

    async fn list_projects(&self, input: ListProjects) -> Result<Page<Value>, SourceError> {
        let mut variables = Map::new();
        let mut filter = Map::new();
        if let Some(team) = self.team(input.team) {
            filter.insert(
                "accessibleTeams".into(),
                json!({ "some": { "key": { "eq": team } } }),
            );
        }
        variables.insert("filter".into(), Value::Object(filter));
        let document = format!(
            "query($first: Int!, $after: String, $filter: ProjectFilter) {{ projects(first: $first, after: $after, filter: $filter, orderBy: updatedAt) {{ \
             nodes {{ id name description state progress startDate targetDate url updatedAt lead {{ name }} teams {{ nodes {{ key }} }} }} {PAGE_INFO} }} }}"
        );
        self.page(
            "list_projects",
            &document,
            variables,
            &["projects"],
            input.limit,
            input.cursor,
        )
        .await
    }

    async fn list_cycles(&self, input: ListCycles) -> Result<Page<Value>, SourceError> {
        let mut variables = Map::new();
        let mut filter = Map::new();
        if let Some(team) = self.team(input.team) {
            filter.insert("team".into(), json!({ "key": { "eq": team } }));
        }
        variables.insert("filter".into(), Value::Object(filter));
        let document = format!(
            "query($first: Int!, $after: String, $filter: CycleFilter) {{ cycles(first: $first, after: $after, filter: $filter) {{ \
             nodes {{ id number name startsAt endsAt completedAt progress team {{ key }} }} {PAGE_INFO} }} }}"
        );
        self.page(
            "list_cycles",
            &document,
            variables,
            &["cycles"],
            input.limit,
            input.cursor,
        )
        .await
    }

    async fn list_issues(&self, input: ListIssues) -> Result<Page<Value>, SourceError> {
        let mut filter = Map::new();
        if let Some(team) = self.team(input.team) {
            filter.insert("team".into(), json!({ "key": { "eq": team } }));
        }
        let mut state = Map::new();
        if let Some(name) = input.state {
            state.insert("name".into(), json!({ "eqIgnoreCase": name }));
        }
        if !input.include_closed.unwrap_or(false) {
            state.insert("type".into(), json!({ "nin": ["completed", "canceled"] }));
        }
        if !state.is_empty() {
            filter.insert("state".into(), Value::Object(state));
        }
        if let Some(assignee) = input.assignee {
            filter.insert("assignee".into(), json!({ "or": [{ "email": { "eq": assignee } }, { "displayName": { "eqIgnoreCase": assignee } }, { "name": { "eqIgnoreCase": assignee } }] }));
        }
        if let Some(project) = input.project {
            filter.insert(
                "project".into(),
                json!({ "name": { "eqIgnoreCase": project } }),
            );
        }
        if let Some(since) = input.updated_since {
            filter.insert("updatedAt".into(), json!({ "gte": since }));
        }
        let mut variables = Map::new();
        variables.insert("filter".into(), Value::Object(filter));
        let document = format!(
            "query($first: Int!, $after: String, $filter: IssueFilter) {{ issues(first: $first, after: $after, filter: $filter, orderBy: updatedAt) {{ \
             nodes {{ {ISSUE_FIELDS} }} {PAGE_INFO} }} }}"
        );
        self.page(
            "list_issues",
            &document,
            variables,
            &["issues"],
            input.limit,
            input.cursor,
        )
        .await
    }

    async fn get_issue(&self, input: GetIssue) -> Result<Value, SourceError> {
        let document = format!(
            "query($id: String!) {{ issue(id: $id) {{ {ISSUE_FIELDS} children {{ nodes {{ identifier title }} }} }} }}"
        );
        let data = self
            .client
            .query(&document, json!({ "id": input.issue }))
            .await?;
        Ok(data.get("issue").cloned().unwrap_or(Value::Null))
    }

    async fn list_comments(&self, input: ListComments) -> Result<Page<Value>, SourceError> {
        let document = format!(
            "query($id: String!, $first: Int!, $after: String) {{ issue(id: $id) {{ comments(first: $first, after: $after) {{ \
             nodes {{ id body createdAt updatedAt url user {{ name displayName }} parent {{ id }} }} {PAGE_INFO} }} }} }}"
        );
        let mut variables = Map::new();
        variables.insert("id".into(), Value::String(input.issue));
        self.page(
            "list_comments",
            &document,
            variables,
            &["issue", "comments"],
            input.limit,
            input.cursor,
        )
        .await
    }

    async fn search_issues(&self, input: SearchIssues) -> Result<Page<Value>, SourceError> {
        let document = format!(
            "query($term: String!, $first: Int!, $after: String) {{ searchIssues(term: $term, first: $first, after: $after) {{ \
             nodes {{ {ISSUE_FIELDS} }} {PAGE_INFO} }} }}"
        );
        let mut variables = Map::new();
        variables.insert("term".into(), Value::String(input.query));
        self.page(
            "search_issues",
            &document,
            variables,
            &["searchIssues"],
            input.limit,
            input.cursor,
        )
        .await
    }

    async fn request(&self, input: Request) -> Result<Value, SourceError> {
        let document = Client::validate_query(&input.query)?;
        self.client
            .query(document, Value::Object(input.variables.unwrap_or_default()))
            .await
    }

    async fn page(
        &self,
        operation: &str,
        document: &str,
        variables: Map<String, Value>,
        path: &[&str],
        limit: Option<u32>,
        cursor: Option<String>,
    ) -> Result<Page<Value>, SourceError> {
        let connection = self
            .client
            .connection(
                operation,
                document,
                variables,
                path,
                limit,
                cursor.as_deref(),
            )
            .await?;
        Ok(Page::new(connection.nodes, connection.next_cursor))
    }
}

fn operations() -> Vec<OperationSpec> {
    vec![
        OperationSpec::read::<Empty, Value>(
            "get_viewer",
            "Get viewer",
            "Return the authenticated user and organization.",
        ),
        OperationSpec::read::<ListTeams, Page<Value>>(
            "list_teams",
            "List teams",
            "List teams with their keys.",
        ),
        OperationSpec::read::<ListProjects, Page<Value>>(
            "list_projects",
            "List projects",
            "List projects, most recently updated first, optionally for one team.",
        ),
        OperationSpec::read::<ListCycles, Page<Value>>(
            "list_cycles",
            "List cycles",
            "List cycles of a team.",
        ),
        OperationSpec::read::<ListIssues, Page<Value>>(
            "list_issues",
            "List issues",
            "List open issues, most recently updated first, with team, state, assignee, project and time filters.",
        ),
        OperationSpec::read::<GetIssue, Value>(
            "get_issue",
            "Get issue",
            "Return one issue by identifier (ENG-123) with its description and children.",
        ),
        OperationSpec::read::<ListComments, Page<Value>>(
            "list_comments",
            "List comments",
            "List the comments of an issue.",
        ),
        OperationSpec::read::<SearchIssues, Page<Value>>(
            "search_issues",
            "Search issues",
            "Full-text search over issues.",
        ),
        OperationSpec::read::<Request, Value>(
            "request",
            "Raw request",
            "Run any read-only GraphQL query against the Linear API.",
        ),
    ]
}

#[async_trait]
impl Source for Linear {
    fn id(&self) -> &str {
        &self.id
    }

    fn kind(&self) -> &'static str {
        "linear"
    }

    fn description(&self) -> &str {
        "Linear teams, projects, cycles, issues, comments and search"
    }

    fn operations(&self) -> &[OperationSpec] {
        &self.operations
    }

    fn search(&self) -> Option<&dyn SearchProvider> {
        Some(self)
    }

    async fn call(
        &self,
        operation: &str,
        input: Value,
        _ctx: &CallContext,
    ) -> Result<Value, SourceError> {
        match operation {
            "get_viewer" => typed(input, |i: Empty| self.get_viewer(i)).await,
            "list_teams" => typed(input, |i: ListTeams| self.list_teams(i)).await,
            "list_projects" => typed(input, |i: ListProjects| self.list_projects(i)).await,
            "list_cycles" => typed(input, |i: ListCycles| self.list_cycles(i)).await,
            "list_issues" => typed(input, |i: ListIssues| self.list_issues(i)).await,
            "get_issue" => typed(input, |i: GetIssue| self.get_issue(i)).await,
            "list_comments" => typed(input, |i: ListComments| self.list_comments(i)).await,
            "search_issues" => typed(input, |i: SearchIssues| self.search_issues(i)).await,
            "request" => typed(input, |i: Request| self.request(i)).await,
            _ => Err(SourceError::UnknownOperation(operation.to_owned())),
        }
    }

    async fn check(&self, _ctx: &CallContext) -> Result<Status, SourceError> {
        let credential = self.client.credential().await?;
        let viewer = self.get_viewer(Empty {}).await?;
        let token = credential.secret.expose();
        let kind = if token.starts_with("lin_api_") {
            "api key"
        } else if token.starts_with("lin_oauth_") {
            "oauth"
        } else {
            "unknown"
        };
        let identity = match (
            viewer.get("name").and_then(Value::as_str),
            viewer
                .get("organization")
                .and_then(|o| o.get("name"))
                .and_then(Value::as_str),
        ) {
            (Some(name), Some(org)) => Some(format!("{name} @ {org}")),
            (Some(name), None) => Some(name.to_owned()),
            _ => None,
        };
        Ok(Status {
            identity,
            token_kind: Some(kind.to_owned()),
            scopes: Vec::new(),
            credential: Some(credential.provenance.to_string()),
            warnings: Vec::new(),
            unavailable: Vec::new(),
        })
    }
}
