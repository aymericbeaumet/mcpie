//! Slack over the Web API.

mod client;
mod normalize;
pub mod types;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;

use self::client::{Client, PageCall, PageState, params_from_map, token_kind};
use self::types::*;
use super::http::Http;
use super::{BuildError, Settings};
use crate::config::SourceConfig;
use crate::model::{
    CallContext, OperationSpec, Page, SearchProvider, Source, SourceError, Status, cursor, typed,
};

/// Type-specific settings under `[sources.<id>]`.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Extra {}

pub struct Slack {
    id: String,
    client: Client,
    operations: Vec<OperationSpec>,
}

type Params = Vec<(&'static str, String)>;

impl Slack {
    pub fn new(settings: Settings, config: &SourceConfig) -> Result<Self, BuildError> {
        let _extra: Extra = config
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
            operations: operations(),
        })
    }

    async fn get_auth(&self, _: Empty) -> Result<Value, SourceError> {
        Ok(self.client.call("auth.test", &[]).await?.0)
    }

    async fn list_channels(&self, input: ListChannels) -> Result<Page<Value>, SourceError> {
        let mut params = Params::new();
        push(&mut params, "types", input.types);
        push(
            &mut params,
            "exclude_archived",
            input.exclude_archived.map(|b| b.to_string()),
        );
        let (items, next) = self
            .client
            .call_page(PageCall {
                operation: "list_channels",
                method: "conversations.list",
                params,
                limit: input.limit,
                max_limit: 1000,
                cursor: input.cursor.as_deref(),
                items_key: "channels",
            })
            .await?;
        Ok(Page::new(items, next))
    }

    async fn get_channel_history(
        &self,
        input: GetChannelHistory,
    ) -> Result<Page<Value>, SourceError> {
        let channel = self.client.resolve_channel(&input.channel).await?;
        let mut params = vec![("channel", channel)];
        push(&mut params, "oldest", input.oldest);
        push(&mut params, "latest", input.latest);
        push(
            &mut params,
            "inclusive",
            input.inclusive.map(|b| b.to_string()),
        );
        let (items, next) = self
            .client
            .call_page(PageCall {
                operation: "get_channel_history",
                method: "conversations.history",
                params,
                limit: input.limit,
                max_limit: 1000,
                cursor: input.cursor.as_deref(),
                items_key: "messages",
            })
            .await?;
        Ok(Page::new(items, next))
    }

    async fn get_thread_replies(
        &self,
        input: GetThreadReplies,
    ) -> Result<Page<Value>, SourceError> {
        let channel = self.client.resolve_channel(&input.channel).await?;
        let params = vec![("channel", channel), ("ts", input.ts)];
        let (items, next) = self
            .client
            .call_page(PageCall {
                operation: "get_thread_replies",
                method: "conversations.replies",
                params,
                limit: input.limit,
                max_limit: 1000,
                cursor: input.cursor.as_deref(),
                items_key: "messages",
            })
            .await?;
        Ok(Page::new(items, next))
    }

    async fn search_messages(&self, input: SearchMessages) -> Result<Page<Value>, SourceError> {
        let page = match &input.cursor {
            Some(cursor) => cursor::decode::<PageState>(cursor, &self.id, "search_messages")?.page,
            None => 1,
        };
        let mut params = vec![
            ("query", input.query),
            ("count", input.limit.unwrap_or(20).clamp(1, 100).to_string()),
            ("page", page.to_string()),
        ];
        push(&mut params, "sort", input.sort.map(enum_name));
        push(&mut params, "sort_dir", input.sort_dir.map(enum_name));
        let (body, _) = self.client.call("search.messages", &params).await?;
        let messages = body.get("messages").cloned().unwrap_or(Value::Null);
        let items = messages
            .get("matches")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let pages = messages
            .get("paging")
            .and_then(|p| p.get("pages"))
            .and_then(Value::as_u64)
            .unwrap_or(1) as u32;
        let next = (page < pages)
            .then(|| cursor::encode(&self.id, "search_messages", &PageState { page: page + 1 }));
        Ok(Page::new(items, next))
    }

    async fn list_users(&self, input: ListUsers) -> Result<Page<Value>, SourceError> {
        let (items, next) = self
            .client
            .call_page(PageCall {
                operation: "list_users",
                method: "users.list",
                params: Params::new(),
                limit: input.limit,
                max_limit: 1000,
                cursor: input.cursor.as_deref(),
                items_key: "members",
            })
            .await?;
        Ok(Page::new(items, next))
    }

    async fn get_user(&self, input: GetUser) -> Result<Value, SourceError> {
        let user = self.client.resolve_user(&input.user).await?;
        let (body, _) = self.client.call("users.info", &[("user", user)]).await?;
        Ok(body.get("user").cloned().unwrap_or(body))
    }

    async fn get_permalink(&self, input: GetPermalink) -> Result<Value, SourceError> {
        let channel = self.client.resolve_channel(&input.channel).await?;
        let (body, _) = self
            .client
            .call(
                "chat.getPermalink",
                &[("channel", channel), ("message_ts", input.message_ts)],
            )
            .await?;
        Ok(body)
    }

    async fn request(&self, input: Request) -> Result<Value, SourceError> {
        let method = Client::validate_method(&input.method)?;
        let params = input
            .params
            .as_ref()
            .map(params_from_map)
            .transpose()?
            .unwrap_or_default();
        Ok(self.client.call(method, &params).await?.0)
    }
}

fn push(params: &mut Params, name: &'static str, value: Option<String>) {
    if let Some(value) = value {
        params.push((name, value));
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
            "get_auth",
            "Get auth",
            "Return the workspace and identity behind the token.",
        ),
        OperationSpec::read::<ListChannels, Page<Value>>(
            "list_channels",
            "List channels",
            "List channels and conversations visible to the token.",
        ),
        OperationSpec::read::<GetChannelHistory, Page<Value>>(
            "get_channel_history",
            "Get channel history",
            "Read the messages of a channel, newest first.",
        ),
        OperationSpec::read::<GetThreadReplies, Page<Value>>(
            "get_thread_replies",
            "Get thread replies",
            "Read a thread: the parent message and its replies.",
        ),
        OperationSpec::read::<SearchMessages, Page<Value>>(
            "search_messages",
            "Search messages",
            "Search messages with Slack's search syntax (user tokens only).",
        ),
        OperationSpec::read::<ListUsers, Page<Value>>(
            "list_users",
            "List users",
            "List workspace members.",
        ),
        OperationSpec::read::<GetUser, Value>(
            "get_user",
            "Get user",
            "Read one member's profile by id or @handle.",
        ),
        OperationSpec::read::<GetPermalink, Value>(
            "get_permalink",
            "Get permalink",
            "Get the permalink of a message.",
        ),
        OperationSpec::read::<Request, Value>(
            "request",
            "Raw request",
            "Call any read-only Web API method with parameters.",
        ),
    ]
}

#[async_trait]
impl Source for Slack {
    fn id(&self) -> &str {
        &self.id
    }

    fn kind(&self) -> &'static str {
        "slack"
    }

    fn description(&self) -> &str {
        "Slack channels, threads, messages, members and search"
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
            "get_auth" => typed(input, |i: Empty| self.get_auth(i)).await,
            "list_channels" => typed(input, |i: ListChannels| self.list_channels(i)).await,
            "get_channel_history" => {
                typed(input, |i: GetChannelHistory| self.get_channel_history(i)).await
            }
            "get_thread_replies" => {
                typed(input, |i: GetThreadReplies| self.get_thread_replies(i)).await
            }
            "search_messages" => typed(input, |i: SearchMessages| self.search_messages(i)).await,
            "list_users" => typed(input, |i: ListUsers| self.list_users(i)).await,
            "get_user" => typed(input, |i: GetUser| self.get_user(i)).await,
            "get_permalink" => typed(input, |i: GetPermalink| self.get_permalink(i)).await,
            "request" => typed(input, |i: Request| self.request(i)).await,
            _ => Err(SourceError::UnknownOperation(operation.to_owned())),
        }
    }

    async fn check(&self, _ctx: &CallContext) -> Result<Status, SourceError> {
        let credential = self.client.credential().await?;
        let (auth, response) = self.client.call("auth.test", &[]).await?;
        let kind = token_kind(credential.secret.expose());
        let scopes: Vec<String> = response
            .header("x-oauth-scopes")
            .map(|s| {
                s.split(',')
                    .map(|x| x.trim().to_owned())
                    .filter(|x| !x.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        let identity = match (
            auth.get("user").and_then(Value::as_str),
            auth.get("team").and_then(Value::as_str),
        ) {
            (Some(user), Some(team)) => Some(format!("{user} @ {team}")),
            (Some(user), None) => Some(user.to_owned()),
            _ => None,
        };
        let mut warnings = Vec::new();
        let mut unavailable = Vec::new();
        if kind == "bot" {
            warnings.push(
                "bot tokens cannot search; use a user token (xoxp-) for search_messages".into(),
            );
            unavailable.push("search_messages".into());
        }
        Ok(Status {
            identity,
            token_kind: Some(kind.to_owned()),
            scopes,
            credential: Some(credential.provenance.to_string()),
            warnings,
            unavailable,
        })
    }
}
