//! Slack search matches as normalized items.

use async_trait::async_trait;
use serde_json::{Value, json};

use super::Slack;
use super::types::SearchMessages;
use crate::model::{
    CallContext, Item, ItemKind, OperationRef, SearchProvider, SearchQuery, SourceError, normalized,
};

/// A match from `search.messages`.
pub fn message_item(source: &str, message: &Value) -> Option<Item> {
    let ts = message.get("ts")?.as_str()?;
    let channel = message.get("channel")?;
    let channel_id = channel.get("id")?.as_str()?;
    let channel_name = channel
        .get("name")
        .and_then(Value::as_str)
        .map(|n| format!("#{n}"));
    let text = message.get("text").and_then(Value::as_str).unwrap_or("");
    let author = message
        .get("username")
        .and_then(Value::as_str)
        .filter(|u| !u.is_empty())
        .or_else(|| message.get("user").and_then(Value::as_str))
        .map(str::to_owned);
    Some(Item {
        kind: ItemKind::Message,
        source: source.to_owned(),
        id: format!("{channel_id}:{ts}"),
        title: channel_name,
        snippet: normalized::snippet(text, 200),
        url: message
            .get("permalink")
            .and_then(Value::as_str)
            .map(str::to_owned),
        author,
        updated_at: normalized::parse_time(&Value::String(ts.to_owned())),
        fetch: Some(OperationRef {
            source: source.to_owned(),
            operation: "get_thread_replies".into(),
            input: json!({ "channel": channel_id, "ts": ts }),
        }),
        raw: Some(message.clone()),
    })
}

#[async_trait]
impl SearchProvider for Slack {
    async fn search(
        &self,
        query: &SearchQuery,
        _ctx: &CallContext,
    ) -> Result<Vec<Item>, SourceError> {
        if query
            .kinds
            .as_ref()
            .is_some_and(|kinds| !kinds.contains(&ItemKind::Message))
        {
            return Ok(Vec::new());
        }
        let page = self
            .search_messages(SearchMessages {
                query: query.query.clone(),
                limit: Some(query.effective_limit() as u32),
                ..Default::default()
            })
            .await?;
        let mut items: Vec<Item> = page
            .items
            .iter()
            .filter_map(|m| message_item(&self.id, m))
            .collect();
        for item in &mut items {
            if let Some(author) = item.author.clone()
                && author.starts_with('U')
                && author
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
                && let Some(name) = self.client.user_name(&author).await
            {
                item.author = Some(name);
            }
        }
        Ok(items)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_search_matches() {
        let message = json!({ "ts": "1700000000.000100", "text": "release  is out", "username": "jane", "permalink": "https://acme.slack.com/archives/C1/p1700000000000100",
            "channel": { "id": "C1", "name": "eng" } });
        let item = message_item("slack", &message).unwrap();
        assert_eq!(item.kind, ItemKind::Message);
        assert_eq!(item.id, "C1:1700000000.000100");
        assert_eq!(item.title.as_deref(), Some("#eng"));
        assert_eq!(item.snippet, "release is out");
        assert_eq!(item.author.as_deref(), Some("jane"));
        assert_eq!(item.updated_at.unwrap().timestamp(), 1700000000);
        assert_eq!(item.fetch.as_ref().unwrap().input["channel"], "C1");
    }
}
