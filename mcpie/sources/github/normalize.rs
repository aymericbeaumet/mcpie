//! GitHub search results as normalized items.

use async_trait::async_trait;
use serde_json::{Value, json};

use super::Github;
use super::types::{SearchCode, SearchIssues};
use crate::model::{
    CallContext, Item, ItemKind, OperationRef, SearchProvider, SearchQuery, SourceError, normalized,
};

/// Owner and repository from an API `repository_url`.
fn repo_from_url(url: &str) -> Option<(&str, &str)> {
    let mut parts = url.trim_end_matches('/').rsplit('/');
    let repo = parts.next()?;
    let owner = parts.next()?;
    Some((owner, repo))
}

/// An issue or pull request from `/search/issues`.
pub fn issue_item(source: &str, issue: &Value) -> Option<Item> {
    let number = issue.get("number")?.as_u64()?;
    let (owner, repo) = repo_from_url(issue.get("repository_url")?.as_str()?)?;
    let is_pull = issue.get("pull_request").is_some_and(|p| !p.is_null());
    let (kind, operation) = if is_pull {
        (ItemKind::PullRequest, "get_pull_request")
    } else {
        (ItemKind::Issue, "get_issue")
    };
    let state = issue.get("state").and_then(Value::as_str).unwrap_or("");
    let body = issue.get("body").and_then(Value::as_str).unwrap_or("");
    Some(Item {
        kind,
        source: source.to_owned(),
        id: format!("{owner}/{repo}#{number}"),
        title: issue
            .get("title")
            .and_then(Value::as_str)
            .map(str::to_owned),
        snippet: normalized::snippet(&format!("[{state}] {body}"), 200),
        url: issue
            .get("html_url")
            .and_then(Value::as_str)
            .map(str::to_owned),
        author: issue
            .get("user")
            .and_then(|u| u.get("login"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        updated_at: issue.get("updated_at").and_then(normalized::parse_time),
        fetch: Some(OperationRef {
            source: source.to_owned(),
            operation: operation.into(),
            input: json!({ "owner": owner, "repo": repo, "number": number }),
        }),
        raw: Some(issue.clone()),
    })
}

/// A file hit from `/search/code`.
pub fn code_item(source: &str, hit: &Value) -> Option<Item> {
    let path = hit.get("path")?.as_str()?;
    let full_name = hit.get("repository")?.get("full_name")?.as_str()?;
    let (owner, repo) = full_name.split_once('/')?;
    Some(Item {
        kind: ItemKind::Document,
        source: source.to_owned(),
        id: format!("{full_name}:{path}"),
        title: Some(path.to_owned()),
        snippet: format!("{full_name}: {path}"),
        url: hit
            .get("html_url")
            .and_then(Value::as_str)
            .map(str::to_owned),
        author: None,
        updated_at: None,
        fetch: Some(OperationRef {
            source: source.to_owned(),
            operation: "get_file_content".into(),
            input: json!({ "owner": owner, "repo": repo, "path": path }),
        }),
        raw: Some(hit.clone()),
    })
}

#[async_trait]
impl SearchProvider for Github {
    async fn search(
        &self,
        query: &SearchQuery,
        _ctx: &CallContext,
    ) -> Result<Vec<Item>, SourceError> {
        let wants = |kind: ItemKind| {
            query
                .kinds
                .as_ref()
                .is_none_or(|kinds| kinds.contains(&kind))
        };
        let limit = query.effective_limit() as u32;
        let issues = async {
            if !(wants(ItemKind::Issue) || wants(ItemKind::PullRequest)) {
                return Ok(Vec::new());
            }
            let page = self
                .search_issues(SearchIssues {
                    query: query.query.clone(),
                    limit: Some(limit),
                    ..Default::default()
                })
                .await?;
            Ok::<_, SourceError>(
                page.items
                    .iter()
                    .filter_map(|i| issue_item(&self.id, i))
                    .collect(),
            )
        };
        let code = async {
            if !wants(ItemKind::Document) {
                return Ok(Vec::new());
            }
            let page = self
                .search_code(SearchCode {
                    query: query.query.clone(),
                    limit: Some(limit.min(20)),
                    cursor: None,
                })
                .await?;
            Ok::<_, SourceError>(
                page.items
                    .iter()
                    .filter_map(|h| code_item(&self.id, h))
                    .collect(),
            )
        };
        let (issues, code) = futures::join!(issues, code);
        match (issues, code) {
            (Ok(mut a), Ok(b)) => {
                a.extend(b);
                Ok(a)
            }
            (Ok(a), Err(error)) => {
                tracing::debug!(source = %self.id, %error, "code search failed; returning issues only");
                Ok(a)
            }
            (Err(error), Ok(b)) => {
                tracing::debug!(source = %self.id, %error, "issue search failed; returning code only");
                Ok(b)
            }
            (Err(error), Err(_)) => Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_issues_pulls_and_code() {
        let issue = json!({ "number": 7, "title": "Crash", "body": "It   breaks\nbadly", "state": "open", "html_url": "https://github.com/acme/widgets/issues/7",
            "repository_url": "https://api.github.com/repos/acme/widgets", "user": { "login": "jane" }, "updated_at": "2026-01-02T03:04:05Z" });
        let item = issue_item("github", &issue).unwrap();
        assert_eq!(item.kind, ItemKind::Issue);
        assert_eq!(item.id, "acme/widgets#7");
        assert_eq!(item.snippet, "[open] It breaks badly");
        assert_eq!(item.author.as_deref(), Some("jane"));
        assert_eq!(item.fetch.as_ref().unwrap().operation, "get_issue");
        assert_eq!(item.fetch.as_ref().unwrap().input["repo"], "widgets");
        let mut pull = issue.clone();
        pull["pull_request"] = json!({ "url": "x" });
        assert_eq!(
            issue_item("github", &pull).unwrap().kind,
            ItemKind::PullRequest
        );
        let hit = json!({ "path": "src/lib.rs", "repository": { "full_name": "acme/widgets" }, "html_url": "https://github.com/acme/widgets/blob/main/src/lib.rs" });
        let item = code_item("github", &hit).unwrap();
        assert_eq!(item.kind, ItemKind::Document);
        assert_eq!(item.fetch.as_ref().unwrap().input["path"], "src/lib.rs");
    }
}
