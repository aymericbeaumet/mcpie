//! Linear issues as normalized items.

use async_trait::async_trait;
use serde_json::{Value, json};

use super::Linear;
use super::types::SearchIssues;
use crate::model::{
    CallContext, Item, ItemKind, OperationRef, SearchProvider, SearchQuery, SourceError, normalized,
};

/// An issue node with the fields `ISSUE_FIELDS` selects.
pub fn issue_item(source: &str, issue: &Value) -> Option<Item> {
    let identifier = issue.get("identifier")?.as_str()?;
    let state = issue
        .get("state")
        .and_then(|s| s.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let description = issue
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("");
    Some(Item {
        kind: ItemKind::Issue,
        source: source.to_owned(),
        id: identifier.to_owned(),
        title: issue
            .get("title")
            .and_then(Value::as_str)
            .map(str::to_owned),
        snippet: normalized::snippet(&format!("[{state}] {description}"), 200),
        url: issue.get("url").and_then(Value::as_str).map(str::to_owned),
        author: issue
            .get("assignee")
            .and_then(|a| a.get("name"))
            .or_else(|| issue.get("creator").and_then(|c| c.get("name")))
            .and_then(Value::as_str)
            .map(str::to_owned),
        updated_at: issue.get("updatedAt").and_then(normalized::parse_time),
        fetch: Some(OperationRef {
            source: source.to_owned(),
            operation: "get_issue".into(),
            input: json!({ "issue": identifier }),
        }),
        raw: Some(issue.clone()),
    })
}

#[async_trait]
impl SearchProvider for Linear {
    async fn search(
        &self,
        query: &SearchQuery,
        _ctx: &CallContext,
    ) -> Result<Vec<Item>, SourceError> {
        if query
            .kinds
            .as_ref()
            .is_some_and(|kinds| !kinds.contains(&ItemKind::Issue))
        {
            return Ok(Vec::new());
        }
        let page = self
            .search_issues(SearchIssues {
                query: query.query.clone(),
                limit: Some(query.effective_limit() as u32),
                cursor: None,
            })
            .await?;
        Ok(page
            .items
            .iter()
            .filter_map(|i| issue_item(&self.id, i))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_issue_nodes() {
        let issue = json!({ "identifier": "ENG-7", "title": "Crash", "description": "boom", "url": "https://linear.app/acme/issue/ENG-7",
            "updatedAt": "2026-01-02T03:04:05.000Z", "state": { "name": "Todo" }, "assignee": { "name": "Jane" } });
        let item = issue_item("linear", &issue).unwrap();
        assert_eq!(item.id, "ENG-7");
        assert_eq!(item.snippet, "[Todo] boom");
        assert_eq!(item.author.as_deref(), Some("Jane"));
        assert_eq!(item.fetch.as_ref().unwrap().input["issue"], "ENG-7");
    }
}
