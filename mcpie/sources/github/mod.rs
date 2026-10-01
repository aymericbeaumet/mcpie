//! GitHub over the REST API v3.

mod client;
mod normalize;
pub mod types;

use async_trait::async_trait;
use base64::Engine;
use serde::Deserialize;
use serde_json::Value;

use self::client::{Client, query_from_map, token_kind};
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
    /// Owner used when an operation's `owner` is omitted.
    pub default_owner: Option<String>,
    /// Repository used when an operation's `repo` is omitted.
    pub default_repo: Option<String>,
}

pub struct Github {
    id: String,
    client: Client,
    extra: Extra,
    operations: Vec<OperationSpec>,
}

type Query = Vec<(&'static str, String)>;

impl Github {
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

    fn repo_path(
        &self,
        owner: Option<String>,
        repo: Option<String>,
    ) -> Result<String, SourceError> {
        let owner = owner
            .or_else(|| self.extra.default_owner.clone())
            .ok_or_else(|| {
                SourceError::InvalidInput(format!(
                    "owner is required (or set sources.{}.default_owner)",
                    self.id
                ))
            })?;
        let repo = repo
            .or_else(|| self.extra.default_repo.clone())
            .ok_or_else(|| {
                SourceError::InvalidInput(format!(
                    "repo is required (or set sources.{}.default_repo)",
                    self.id
                ))
            })?;
        for (name, value) in [("owner", &owner), ("repo", &repo)] {
            if value.is_empty() || value.contains('/') || value.contains("..") {
                return Err(SourceError::InvalidInput(format!(
                    "invalid {name} {value:?}"
                )));
            }
        }
        Ok(format!("/repos/{owner}/{repo}"))
    }

    async fn get_viewer(&self, _: Empty) -> Result<Value, SourceError> {
        Ok(self.client.get("/user", &[]).await?.0)
    }

    async fn list_repos(&self, input: ListRepos) -> Result<Page<Value>, SourceError> {
        let owner = input.owner.or_else(|| self.extra.default_owner.clone());
        let path = match &owner {
            Some(owner) => match self.owner_kind(owner).await? {
                OwnerKind::Organization => format!("/orgs/{owner}/repos"),
                OwnerKind::User => format!("/users/{owner}/repos"),
            },
            None => "/user/repos".to_owned(),
        };
        let mut query = Query::new();
        push(&mut query, "type", input.kind);
        push(&mut query, "sort", input.sort.map(enum_name));
        push(&mut query, "direction", input.direction.map(enum_name));
        self.page("list_repos", &path, query, input.limit, input.cursor)
            .await
    }

    async fn get_repo(&self, input: GetRepo) -> Result<Value, SourceError> {
        let path = self.repo_path(input.owner, input.repo)?;
        Ok(self.client.get(&path, &[]).await?.0)
    }

    async fn list_issues(&self, input: ListIssues) -> Result<Page<Value>, SourceError> {
        let path = format!("{}/issues", self.repo_path(input.owner, input.repo)?);
        let mut query = Query::new();
        push(&mut query, "state", input.state.map(enum_name));
        push(&mut query, "labels", input.labels);
        push(&mut query, "assignee", input.assignee);
        push(&mut query, "creator", input.creator);
        push(&mut query, "since", input.since);
        push(&mut query, "sort", input.sort.map(enum_name));
        push(&mut query, "direction", input.direction.map(enum_name));
        self.page("list_issues", &path, query, input.limit, input.cursor)
            .await
    }

    async fn get_issue(&self, input: GetIssue) -> Result<Value, SourceError> {
        let path = format!(
            "{}/issues/{}",
            self.repo_path(input.owner, input.repo)?,
            input.number
        );
        Ok(self.client.get(&path, &[]).await?.0)
    }

    async fn list_issue_comments(
        &self,
        input: ListIssueComments,
    ) -> Result<Page<Value>, SourceError> {
        let path = format!(
            "{}/issues/{}/comments",
            self.repo_path(input.owner, input.repo)?,
            input.number
        );
        let mut query = Query::new();
        push(&mut query, "since", input.since);
        self.page(
            "list_issue_comments",
            &path,
            query,
            input.limit,
            input.cursor,
        )
        .await
    }

    async fn list_pull_requests(
        &self,
        input: ListPullRequests,
    ) -> Result<Page<Value>, SourceError> {
        let path = format!("{}/pulls", self.repo_path(input.owner, input.repo)?);
        let mut query = Query::new();
        push(&mut query, "state", input.state.map(enum_name));
        push(&mut query, "head", input.head);
        push(&mut query, "base", input.base);
        push(&mut query, "sort", input.sort.map(enum_name));
        push(&mut query, "direction", input.direction.map(enum_name));
        self.page(
            "list_pull_requests",
            &path,
            query,
            input.limit,
            input.cursor,
        )
        .await
    }

    async fn get_pull_request(&self, input: GetIssue) -> Result<Value, SourceError> {
        let path = format!(
            "{}/pulls/{}",
            self.repo_path(input.owner, input.repo)?,
            input.number
        );
        Ok(self.client.get(&path, &[]).await?.0)
    }

    async fn list_commits(&self, input: ListCommits) -> Result<Page<Value>, SourceError> {
        let path = format!("{}/commits", self.repo_path(input.owner, input.repo)?);
        let mut query = Query::new();
        push(&mut query, "sha", input.sha);
        push(&mut query, "path", input.path);
        push(&mut query, "author", input.author);
        push(&mut query, "since", input.since);
        push(&mut query, "until", input.until);
        self.page("list_commits", &path, query, input.limit, input.cursor)
            .await
    }

    async fn get_file_content(&self, input: GetFileContent) -> Result<Value, SourceError> {
        let file = input.path.trim_matches('/');
        if file.is_empty() || file.split('/').any(|segment| segment == "..") {
            return Err(SourceError::InvalidInput(format!(
                "invalid path {:?}",
                input.path
            )));
        }
        let path = format!(
            "{}/contents/{file}",
            self.repo_path(input.owner, input.repo)?
        );
        let mut query = Query::new();
        push(&mut query, "ref", input.reference);
        let (mut body, _) = self.client.get(&path, &query).await?;
        if let Some(object) = body.as_object_mut()
            && object.get("encoding").and_then(Value::as_str) == Some("base64")
            && let Some(encoded) = object.get("content").and_then(Value::as_str)
        {
            let raw: String = encoded.chars().filter(|c| !c.is_whitespace()).collect();
            if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(raw)
                && let Ok(text) = String::from_utf8(bytes)
            {
                object.insert("content".into(), Value::String(text));
                object.insert("encoding".into(), Value::String("utf-8".into()));
            }
        }
        Ok(body)
    }

    async fn search_issues(&self, input: SearchIssues) -> Result<Page<Value>, SourceError> {
        let mut query = vec![("q", input.query)];
        push(&mut query, "sort", input.sort);
        push(&mut query, "order", input.direction.map(enum_name));
        self.page(
            "search_issues",
            "/search/issues",
            query,
            input.limit,
            input.cursor,
        )
        .await
    }

    async fn search_code(&self, input: SearchCode) -> Result<Page<Value>, SourceError> {
        let query = vec![("q", input.query)];
        self.page(
            "search_code",
            "/search/code",
            query,
            input.limit,
            input.cursor,
        )
        .await
    }

    async fn search_repos(&self, input: SearchRepos) -> Result<Page<Value>, SourceError> {
        let mut query = vec![("q", input.query)];
        push(&mut query, "sort", input.sort);
        push(&mut query, "order", input.direction.map(enum_name));
        self.page(
            "search_repos",
            "/search/repositories",
            query,
            input.limit,
            input.cursor,
        )
        .await
    }

    async fn request(&self, input: Request) -> Result<Value, SourceError> {
        let path = Client::validate_path(&input.path)?;
        let query = input
            .query
            .as_ref()
            .map(query_from_map)
            .transpose()?
            .unwrap_or_default();
        Ok(self.client.get(path, &query).await?.0)
    }

    async fn page(
        &self,
        operation: &str,
        path: &str,
        query: Query,
        limit: Option<u32>,
        cursor: Option<String>,
    ) -> Result<Page<Value>, SourceError> {
        let (items, next_cursor) = self
            .client
            .get_page(operation, path, query, limit, cursor.as_deref())
            .await?;
        Ok(Page::new(items, next_cursor))
    }

    async fn owner_kind(&self, owner: &str) -> Result<OwnerKind, SourceError> {
        let (user, _) = self.client.get(&format!("/users/{owner}"), &[]).await?;
        Ok(
            if user.get("type").and_then(Value::as_str) == Some("Organization") {
                OwnerKind::Organization
            } else {
                OwnerKind::User
            },
        )
    }
}

enum OwnerKind {
    User,
    Organization,
}

fn push(query: &mut Query, name: &'static str, value: Option<String>) {
    if let Some(value) = value {
        query.push((name, value));
    }
}

fn enum_name<T: serde::Serialize>(value: T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn operations() -> Vec<OperationSpec> {
    vec![
        OperationSpec::read::<Empty, Value>(
            "get_viewer",
            "Get viewer",
            "Return the authenticated user.",
        ),
        OperationSpec::read::<ListRepos, Page<Value>>(
            "list_repos",
            "List repositories",
            "List repositories of the authenticated user, a user or an organization.",
        ),
        OperationSpec::read::<GetRepo, Value>(
            "get_repo",
            "Get repository",
            "Return one repository with its metadata.",
        ),
        OperationSpec::read::<ListIssues, Page<Value>>(
            "list_issues",
            "List issues",
            "List issues of a repository; pull requests are included and carry a pull_request key.",
        ),
        OperationSpec::read::<GetIssue, Value>(
            "get_issue",
            "Get issue",
            "Return one issue by number, with its body.",
        ),
        OperationSpec::read::<ListIssueComments, Page<Value>>(
            "list_issue_comments",
            "List issue comments",
            "List the comments of an issue or pull request.",
        ),
        OperationSpec::read::<ListPullRequests, Page<Value>>(
            "list_pull_requests",
            "List pull requests",
            "List pull requests of a repository.",
        ),
        OperationSpec::read::<GetIssue, Value>(
            "get_pull_request",
            "Get pull request",
            "Return one pull request by number, with its body and merge state.",
        ),
        OperationSpec::read::<ListCommits, Page<Value>>(
            "list_commits",
            "List commits",
            "List commits of a repository, optionally filtered by ref, path, author or time.",
        ),
        OperationSpec::read::<GetFileContent, Value>(
            "get_file_content",
            "Get file content",
            "Read a file as text (base64 decoded) or list a directory.",
        ),
        OperationSpec::read::<SearchIssues, Page<Value>>(
            "search_issues",
            "Search issues",
            "Search issues and pull requests across GitHub with the search syntax.",
        ),
        OperationSpec::read::<SearchCode, Page<Value>>(
            "search_code",
            "Search code",
            "Search file contents across GitHub with the code search syntax.",
        ),
        OperationSpec::read::<SearchRepos, Page<Value>>(
            "search_repos",
            "Search repositories",
            "Search repositories across GitHub with the search syntax.",
        ),
        OperationSpec::read::<Request, Value>(
            "request",
            "Raw request",
            "GET any GitHub REST API path, like `gh api`.",
        ),
    ]
}

#[async_trait]
impl Source for Github {
    fn id(&self) -> &str {
        &self.id
    }

    fn kind(&self) -> &'static str {
        "github"
    }

    fn description(&self) -> &str {
        "GitHub repositories, issues, pull requests, commits, files and search"
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
            "list_repos" => typed(input, |i: ListRepos| self.list_repos(i)).await,
            "get_repo" => typed(input, |i: GetRepo| self.get_repo(i)).await,
            "list_issues" => typed(input, |i: ListIssues| self.list_issues(i)).await,
            "get_issue" => typed(input, |i: GetIssue| self.get_issue(i)).await,
            "list_issue_comments" => {
                typed(input, |i: ListIssueComments| self.list_issue_comments(i)).await
            }
            "list_pull_requests" => {
                typed(input, |i: ListPullRequests| self.list_pull_requests(i)).await
            }
            "get_pull_request" => typed(input, |i: GetIssue| self.get_pull_request(i)).await,
            "list_commits" => typed(input, |i: ListCommits| self.list_commits(i)).await,
            "get_file_content" => typed(input, |i: GetFileContent| self.get_file_content(i)).await,
            "search_issues" => typed(input, |i: SearchIssues| self.search_issues(i)).await,
            "search_code" => typed(input, |i: SearchCode| self.search_code(i)).await,
            "search_repos" => typed(input, |i: SearchRepos| self.search_repos(i)).await,
            "request" => typed(input, |i: Request| self.request(i)).await,
            _ => Err(SourceError::UnknownOperation(operation.to_owned())),
        }
    }

    async fn check(&self, _ctx: &CallContext) -> Result<Status, SourceError> {
        let credential = self.client.credential().await?;
        let (user, response) = self.client.get("/user", &[]).await?;
        let scopes: Vec<String> = response
            .header("x-oauth-scopes")
            .map(|s| {
                s.split(',')
                    .map(|x| x.trim().to_owned())
                    .filter(|x| !x.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        Ok(Status {
            identity: user.get("login").and_then(Value::as_str).map(str::to_owned),
            token_kind: Some(token_kind(credential.secret.expose()).to_owned()),
            scopes,
            credential: Some(credential.provenance.to_string()),
            warnings: Vec::new(),
            unavailable: Vec::new(),
        })
    }
}
