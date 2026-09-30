//! The contract a source implements.

use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;

use super::{SourceError, normalized::SearchProvider, spec::OperationSpec};

/// Per-call parameters every facade provides.
#[derive(Debug, Clone)]
pub struct CallContext {
    pub timeout: Duration,
    pub request_id: String,
}

impl CallContext {
    pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

    pub fn new(timeout: Duration) -> Self {
        Self {
            timeout,
            request_id: next_request_id(),
        }
    }

    pub fn with_request_id(mut self, request_id: impl Into<String>) -> Self {
        self.request_id = request_id.into();
        self
    }
}

impl Default for CallContext {
    fn default() -> Self {
        Self::new(Self::DEFAULT_TIMEOUT)
    }
}

fn next_request_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or_default();
    format!("{:x}-{:x}", nanos, COUNTER.fetch_add(1, Ordering::Relaxed))
}

/// What a credential can do, as reported by [`Source::check`].
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct Status {
    /// Who the credential authenticates as (login, email, workspace).
    pub identity: Option<String>,
    /// Credential flavour when the source distinguishes them (`xoxp`, `xoxb`, `pat`).
    pub token_kind: Option<String>,
    pub scopes: Vec<String>,
    /// Where the credential came from (`config`, `env:GITHUB_TOKEN`, `gh auth token`).
    pub credential: Option<String>,
    pub warnings: Vec<String>,
    /// Operations this credential cannot use.
    pub unavailable: Vec<String>,
}

/// A connected system. The source is the single stateful object: it owns its HTTP client,
/// credentials and caches, and answers operations by name.
#[async_trait]
pub trait Source: Send + Sync + 'static {
    /// Instance id (`github`, `my-notion`); see [`super::names::is_valid_source_id`].
    fn id(&self) -> &str;
    /// Source type (`github`, `slack`, `mcp`).
    fn kind(&self) -> &'static str;
    fn description(&self) -> &str;
    fn operations(&self) -> &[OperationSpec];
    async fn call(
        &self,
        operation: &str,
        input: Value,
        ctx: &CallContext,
    ) -> Result<Value, SourceError>;
    /// Probe the credential without side effects.
    async fn check(&self, ctx: &CallContext) -> Result<Status, SourceError>;
    /// The normalized search entry point, when the source has one.
    fn search(&self) -> Option<&dyn SearchProvider> {
        None
    }
}

/// Deserialize `input` into `I`, run the handler, serialize its output. This is the one place
/// every facade's payload crosses into typed Rust, so a `null` or missing input counts as `{}`.
pub async fn typed<I, O, F, Fut>(input: Value, handler: F) -> Result<Value, SourceError>
where
    I: DeserializeOwned,
    O: Serialize,
    F: FnOnce(I) -> Fut,
    Fut: Future<Output = Result<O, SourceError>>,
{
    let input = if input.is_null() {
        Value::Object(Default::default())
    } else {
        input
    };
    let input: I = serde_json::from_value(input).map_err(SourceError::invalid_input)?;
    let output = handler(input).await?;
    serde_json::to_value(output).map_err(SourceError::internal)
}
