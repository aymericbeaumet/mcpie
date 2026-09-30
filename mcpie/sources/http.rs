//! The HTTP client every source shares: one connection pool, a per-source concurrency limit,
//! the mcpie user agent, request timeouts, and transport-level error mapping. Upstream-specific
//! status and body semantics stay in each source's client.

use std::sync::{Arc, Once};
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderValue, USER_AGENT};
use serde_json::Value;
use tokio::sync::Semaphore;

use crate::model::SourceError;

/// A response with its body already read.
#[derive(Debug)]
pub struct Response {
    pub status: u16,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// The body as JSON, or `null` when empty, or a string when not JSON.
    pub fn json(&self) -> Value {
        if self.body.iter().all(u8::is_ascii_whitespace) {
            return Value::Null;
        }
        serde_json::from_slice(&self.body).unwrap_or_else(|_| Value::String(self.text()))
    }

    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

#[derive(Clone)]
pub struct Http {
    client: reqwest::Client,
    limiter: Arc<Semaphore>,
    base_url: String,
}

impl Http {
    /// `base_url` has no trailing slash; paths passed to [`Http::url`] start with one.
    pub fn new(
        base_url: &str,
        timeout: Duration,
        max_in_flight: usize,
    ) -> Result<Self, SourceError> {
        install_crypto_provider();
        let mut headers = HeaderMap::new();
        headers.insert(
            USER_AGENT,
            HeaderValue::from_str(&format!("{}/{}", crate::NAME, crate::VERSION))
                .expect("static user agent"),
        );
        let client = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(timeout)
            .use_rustls_tls()
            .build()
            .map_err(|e| SourceError::Internal(format!("cannot build http client: {e}")))?;
        Ok(Self {
            client,
            limiter: Arc::new(Semaphore::new(max_in_flight.max(1))),
            base_url: base_url.trim_end_matches('/').to_owned(),
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Join a path (starting with `/`) onto the base URL.
    pub fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    pub fn request(&self, method: reqwest::Method, url: &str) -> reqwest::RequestBuilder {
        self.client.request(method, url)
    }

    /// Send a request under the concurrency limit and read the whole body. Never logs headers.
    pub async fn send(&self, request: reqwest::RequestBuilder) -> Result<Response, SourceError> {
        let _permit = self
            .limiter
            .acquire()
            .await
            .map_err(|_| SourceError::Internal("http limiter closed".into()))?;
        let response = request.send().await.map_err(map_reqwest_error)?;
        let status = response.status().as_u16();
        let headers = response.headers().clone();
        let body = response.bytes().await.map_err(map_reqwest_error)?.to_vec();
        tracing::debug!(status, bytes = body.len(), "http response");
        Ok(Response {
            status,
            headers,
            body,
        })
    }
}

fn map_reqwest_error(error: reqwest::Error) -> SourceError {
    if error.is_timeout() {
        return SourceError::Timeout(Duration::ZERO);
    }
    // reqwest's Display includes the URL but never headers or bodies.
    SourceError::Transport(error.to_string())
}

pub(crate) fn install_crypto_provider() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// Seconds until an epoch timestamp, used for rate-limit reset headers.
pub fn seconds_until(epoch_seconds: u64) -> Duration {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    Duration::from_secs(epoch_seconds.saturating_sub(now))
}

/// Parse a `Retry-After` header given in seconds.
pub fn retry_after(response: &Response) -> Option<Duration> {
    response
        .header("retry-after")
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
}
