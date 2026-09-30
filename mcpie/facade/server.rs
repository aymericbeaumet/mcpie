//! The HTTP server: one router serving REST, the OpenAPI document, MCP over streamable HTTP and
//! (later) GraphQL, behind the guards a local, token-optional server needs.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use serde_json::Value;
use tokio::net::TcpListener;

use super::mcp::{McpServer, Mode};
use super::rest;
use crate::config::Secret;
use crate::model::{OperationSpec, Registry, RegistryError, Selection};

/// A source and the operations exposed in this server, owned so handlers need no lifetimes.
pub struct ExposedSource {
    pub id: String,
    pub kind: String,
    pub description: String,
    pub operations: Vec<OperationSpec>,
    pub searchable: bool,
}

/// Everything the HTTP handlers share.
pub struct AppState {
    pub registry: Arc<Registry>,
    pub timeout: Duration,
    pub token: Option<Secret>,
    /// Lower-case host names (no port) accepted in the `Host` header.
    pub allowed_hosts: Vec<String>,
    pub exposed: Vec<ExposedSource>,
    pub openapi: Value,
}

impl AppState {
    pub fn new(
        registry: Arc<Registry>,
        selection: &Selection,
        timeout: Duration,
        token: Option<Secret>,
        allowed_hosts: Vec<String>,
    ) -> Result<Arc<Self>, RegistryError> {
        let exposed = registry
            .select(selection)?
            .into_iter()
            .map(|entry| ExposedSource {
                id: entry.source.id().to_owned(),
                kind: entry.source.kind().to_owned(),
                description: entry.source.description().to_owned(),
                operations: entry.operations.into_iter().cloned().collect(),
                searchable: entry.source.search().is_some(),
            })
            .collect();
        let mut state = Self {
            registry,
            timeout,
            token,
            allowed_hosts,
            exposed,
            openapi: Value::Null,
        };
        state.openapi = rest::openapi::document(&state);
        Ok(Arc::new(state))
    }

    pub fn source(&self, id: &str) -> Option<&ExposedSource> {
        self.exposed.iter().find(|s| s.id == id)
    }

    pub fn spec(&self, source: &str, operation: &str) -> Option<&OperationSpec> {
        self.source(source)?
            .operations
            .iter()
            .find(|o| o.name == operation)
    }

    pub fn searchable(&self) -> bool {
        self.exposed.iter().any(|s| s.searchable)
    }
}

/// The request id middleware stores this in request extensions.
#[derive(Debug, Clone)]
pub struct RequestId(pub String);

/// Options for [`serve`].
pub struct ServeOptions {
    pub bind: String,
    pub token: Option<Secret>,
    pub insecure_no_auth: bool,
    pub selection: Selection,
    pub timeout: Duration,
    pub mcp_mode: Mode,
}

/// Assemble the application: REST routes, the MCP endpoint, and the guard layers.
pub fn app(state: Arc<AppState>, mcp: Option<Router>) -> Router {
    let mut router = rest::routes(state.clone());
    if let Some(mcp) = mcp {
        router = router.merge(mcp);
    }
    router
        .layer(middleware::from_fn_with_state(state.clone(), bearer_guard))
        .layer(middleware::from_fn_with_state(state.clone(), host_guard))
        .layer(middleware::from_fn(request_id))
        .layer(
            tower_http::trace::TraceLayer::new_for_http().on_response(
                tower_http::trace::DefaultOnResponse::new().level(tracing::Level::INFO),
            ),
        )
}

/// The MCP endpoint at `/mcp`, one handler per session.
pub fn mcp_router(state: &Arc<AppState>, selection: Selection, mode: Mode) -> Router {
    let registry = state.registry.clone();
    let timeout = state.timeout;
    let service = StreamableHttpService::new(
        move || {
            McpServer::new(registry.clone(), &selection, mode, timeout)
                .map_err(std::io::Error::other)
        },
        Arc::new(LocalSessionManager::default()),
        // The router-wide host guard already validates `Host`.
        StreamableHttpServerConfig::default()
            .disable_allowed_hosts()
            .with_json_response(true),
    );
    Router::new().nest_service("/mcp", service)
}

async fn request_id(mut request: Request, next: Next) -> Response {
    let id = request
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .filter(|v| {
            !v.is_empty()
                && v.len() <= 64
                && v.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        })
        .map(str::to_owned)
        .unwrap_or_else(|| crate::model::CallContext::default().request_id);
    request.extensions_mut().insert(RequestId(id.clone()));
    let mut response = next.run(request).await;
    if let Ok(value) = HeaderValue::from_str(&id) {
        response.headers_mut().insert("x-request-id", value);
    }
    response
}

/// Reject requests whose `Host` is not the loopback or the bound host: a DNS-rebinding page
/// cannot reach a local server through a name it controls.
async fn host_guard(State(state): State<Arc<AppState>>, request: Request, next: Next) -> Response {
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(strip_port)
        .unwrap_or_default();
    if !state
        .allowed_hosts
        .iter()
        .any(|allowed| allowed.eq_ignore_ascii_case(&host))
    {
        return rest::error_response(
            StatusCode::MISDIRECTED_REQUEST,
            "misdirected",
            format!("host {host:?} is not served here"),
            None,
            None,
            None,
        );
    }
    next.run(request).await
}

fn strip_port(host: &str) -> String {
    let host = host.trim();
    if let Some(rest) = host.strip_prefix('[') {
        return rest.split(']').next().unwrap_or_default().to_owned();
    }
    host.rsplit_once(':')
        .map(|(h, _)| h)
        .unwrap_or(host)
        .to_owned()
}

fn bearer_exempt(request: &Request) -> bool {
    let path = request.uri().path();
    matches!(path, "/healthz" | "/openapi.json" | "/docs")
        || (path == "/graphql" && request.method() == axum::http::Method::GET)
}

async fn bearer_guard(
    State(state): State<Arc<AppState>>,
    request: Request,
    next: Next,
) -> Response {
    let Some(token) = &state.token else {
        return next.run(request).await;
    };
    if bearer_exempt(&request) {
        return next.run(request).await;
    }
    let presented = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            v.strip_prefix("Bearer ")
                .or_else(|| v.strip_prefix("bearer "))
        })
        .map(str::trim);
    if presented.is_some_and(|p| constant_time_eq(p.as_bytes(), token.expose().as_bytes())) {
        return next.run(request).await;
    }
    let mut response = rest::error_response(
        StatusCode::UNAUTHORIZED,
        "unauthorized",
        "a bearer token is required".into(),
        None,
        None,
        None,
    );
    response
        .headers_mut()
        .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
    response
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Hosts accepted for a bind address: loopback names plus the bound IP.
pub fn allowed_hosts_for(addr: &SocketAddr) -> Vec<String> {
    let mut hosts = vec![
        "localhost".to_owned(),
        "127.0.0.1".to_owned(),
        "::1".to_owned(),
    ];
    let ip = addr.ip().to_string();
    if !hosts.contains(&ip) {
        hosts.push(ip);
    }
    hosts
}

/// Bind, serve until ctrl-c or SIGTERM, then drain for at most five seconds. `on_listen`
/// receives the bound address before the first request.
pub async fn serve(
    registry: Arc<Registry>,
    options: ServeOptions,
    on_listen: impl FnOnce(SocketAddr),
) -> Result<(), String> {
    let addr: SocketAddr = options.bind.parse().map_err(|_| {
        format!(
            "invalid bind address {:?}; use host:port such as 127.0.0.1:7878",
            options.bind
        )
    })?;
    if !addr.ip().is_loopback() && options.token.is_none() && !options.insecure_no_auth {
        return Err(format!(
            "refusing to listen on {addr} without a token: set server.token (or --set server.token=...) or pass --insecure-no-auth"
        ));
    }
    let state = AppState::new(
        registry,
        &options.selection,
        options.timeout,
        options.token,
        allowed_hosts_for(&addr),
    )
    .map_err(|e| e.to_string())?;
    let mcp = mcp_router(&state, options.selection.clone(), options.mcp_mode);
    let application = app(state, Some(mcp));
    let listener = TcpListener::bind(addr)
        .await
        .map_err(|e| format!("cannot bind {addr}: {e}"))?;
    let local = listener.local_addr().map_err(|e| e.to_string())?;
    on_listen(local);
    let notify = Arc::new(tokio::sync::Notify::new());
    let signal_notify = notify.clone();
    tokio::spawn(async move {
        wait_for_signal().await;
        tracing::info!("shutting down");
        signal_notify.notify_waiters();
    });
    let graceful = notify.clone();
    let server = axum::serve(listener, application.into_make_service())
        .with_graceful_shutdown(async move { graceful.notified().await });
    let drain = async move {
        notify.notified().await;
        tokio::time::sleep(Duration::from_secs(5)).await;
    };
    tokio::select! {
        result = server => result.map_err(|e| e.to_string()),
        _ = drain => {
            tracing::warn!("connections still open after 5s; exiting");
            Ok(())
        }
    }
}

async fn wait_for_signal() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("sigterm handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = terminate.recv() => {},
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// Respond with a body, used by tests to read responses.
pub async fn body_json(response: Response) -> Value {
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap_or_default();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

impl IntoResponse for RequestId {
    fn into_response(self) -> Response {
        Response::new(Body::from(self.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_ports_and_brackets() {
        assert_eq!(strip_port("localhost:7878"), "localhost");
        assert_eq!(strip_port("127.0.0.1"), "127.0.0.1");
        assert_eq!(strip_port("[::1]:7878"), "::1");
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        let hosts = allowed_hosts_for(&"0.0.0.0:80".parse().unwrap());
        assert!(hosts.contains(&"0.0.0.0".to_owned()) && hosts.contains(&"localhost".to_owned()));
    }
}
