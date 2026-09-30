//! GitHub over the REST API v3.

mod client;
pub mod types;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;

use self::client::{Client, query_from_map, token_kind};
use self::types::{Empty, ListRepos, Request};
use super::http::Http;
use super::{BuildError, Settings};
use crate::config::SourceConfig;
use crate::model::{CallContext, OperationSpec, Page, Source, SourceError, Status, typed};

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
        let mut query = Vec::new();
        if let Some(kind) = input.kind {
            query.push(("type", kind));
        }
        if let Some(sort) = input.sort {
            query.push(("sort", enum_name(sort)));
        }
        if let Some(direction) = input.direction {
            query.push(("direction", enum_name(direction)));
        }
        let (items, next_cursor) = self
            .client
            .get_page(
                "list_repos",
                &path,
                query,
                input.limit,
                input.cursor.as_deref(),
            )
            .await?;
        Ok(Page::new(items, next_cursor))
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

    async fn call(
        &self,
        operation: &str,
        input: Value,
        _ctx: &CallContext,
    ) -> Result<Value, SourceError> {
        match operation {
            "get_viewer" => typed(input, |i: Empty| self.get_viewer(i)).await,
            "list_repos" => typed(input, |i: ListRepos| self.list_repos(i)).await,
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
