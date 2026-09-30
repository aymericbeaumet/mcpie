//! A deterministic in-memory source used by every facade test.

#![allow(dead_code)]

use async_trait::async_trait;
use chrono::{TimeZone, Utc};
use mcpie::model::{
    CallContext, Item, ItemKind, OperationRef, OperationSpec, Page, SearchProvider, SearchQuery,
    Source, SourceError, Status, cursor, typed,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Plain text.
    Plain,
    /// Markdown.
    Markdown,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EchoInput {
    /// Text to echo back.
    pub text: String,
    /// How many times to repeat the text.
    #[serde(default)]
    pub times: Option<u32>,
    /// Shout.
    #[serde(default)]
    pub loud: Option<bool>,
    /// Rendering mode.
    #[serde(default)]
    pub mode: Option<Mode>,
    /// Tags to attach.
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct EchoOutput {
    pub text: String,
    pub tags: Vec<String>,
    pub mode: Option<Mode>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListThingsInput {
    /// Page size (default 10).
    #[serde(default)]
    pub limit: Option<u32>,
    /// Cursor from a previous page.
    #[serde(default)]
    pub cursor: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WriteThingInput {
    pub name: String,
}

#[derive(Serialize, Deserialize)]
struct ThingsPage {
    offset: u32,
}

pub struct FakeSource {
    id: String,
    things: u32,
    fail_search: bool,
    operations: Vec<OperationSpec>,
}

impl FakeSource {
    pub fn new(id: &str) -> Self {
        Self::with_things(id, 25)
    }

    pub fn with_things(id: &str, things: u32) -> Self {
        Self {
            id: id.to_owned(),
            things,
            fail_search: false,
            operations: vec![
                OperationSpec::read::<EchoInput, EchoOutput>(
                    "echo",
                    "Echo",
                    "Echo text back with options.",
                ),
                OperationSpec::read::<ListThingsInput, Page<Value>>(
                    "list_things",
                    "List things",
                    "List numbered things, paginated.",
                ),
                OperationSpec::write::<WriteThingInput, Value>(
                    "write_thing",
                    "Write thing",
                    "Create a thing.",
                ),
            ],
        }
    }

    pub fn failing_search(mut self) -> Self {
        self.fail_search = true;
        self
    }

    async fn echo(&self, input: EchoInput) -> Result<EchoOutput, SourceError> {
        let mut text = input.text.repeat(input.times.unwrap_or(1).max(1) as usize);
        if input.loud.unwrap_or(false) {
            text = text.to_uppercase();
        }
        Ok(EchoOutput {
            text,
            tags: input.tags,
            mode: input.mode,
        })
    }

    async fn list_things(&self, input: ListThingsInput) -> Result<Page<Value>, SourceError> {
        let limit = input.limit.unwrap_or(10).clamp(1, 100);
        let offset = match &input.cursor {
            Some(cursor) => cursor::decode::<ThingsPage>(cursor, &self.id, "list_things")?.offset,
            None => 0,
        };
        let end = (offset + limit).min(self.things);
        let items = (offset..end)
            .map(|n| serde_json::json!({ "n": n, "name": format!("thing-{n}") }))
            .collect();
        let next_cursor = (end < self.things)
            .then(|| cursor::encode(&self.id, "list_things", &ThingsPage { offset: end }));
        Ok(Page::new(items, next_cursor))
    }
}

#[async_trait]
impl Source for FakeSource {
    fn id(&self) -> &str {
        &self.id
    }

    fn kind(&self) -> &'static str {
        "fake"
    }

    fn description(&self) -> &str {
        "In-memory source for tests."
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
            "echo" => typed(input, |i: EchoInput| self.echo(i)).await,
            "list_things" => typed(input, |i: ListThingsInput| self.list_things(i)).await,
            "write_thing" => {
                typed(input, |i: WriteThingInput| async move {
                    Ok(serde_json::json!({ "created": i.name }))
                })
                .await
            }
            _ => Err(SourceError::UnknownOperation(operation.to_owned())),
        }
    }

    async fn check(&self, _ctx: &CallContext) -> Result<Status, SourceError> {
        Ok(Status {
            identity: Some(format!("{}-user", self.id)),
            token_kind: Some("fake".into()),
            ..Status::default()
        })
    }

    fn search(&self) -> Option<&dyn SearchProvider> {
        Some(self)
    }
}

#[async_trait]
impl SearchProvider for FakeSource {
    async fn search(
        &self,
        query: &SearchQuery,
        _ctx: &CallContext,
    ) -> Result<Vec<Item>, SourceError> {
        if self.fail_search {
            return Err(SourceError::Auth("token expired".into()));
        }
        Ok((0..self.things)
            .filter(|n| query.query.is_empty() || format!("thing-{n}").contains(&query.query))
            .map(|n| Item {
                kind: if n % 2 == 0 {
                    ItemKind::Document
                } else {
                    ItemKind::Issue
                },
                source: self.id.clone(),
                id: n.to_string(),
                title: Some(format!("thing-{n}")),
                snippet: format!("Thing number {n} from {}", self.id),
                url: None,
                author: Some(format!("{}-user", self.id)),
                updated_at: Some(
                    Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap()
                        + chrono::Duration::hours(n as i64),
                ),
                fetch: Some(OperationRef {
                    source: self.id.clone(),
                    operation: "list_things".into(),
                    input: serde_json::json!({ "limit": 1 }),
                }),
                raw: Some(serde_json::json!({ "n": n })),
            })
            .collect())
    }
}
