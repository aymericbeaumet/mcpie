//! The one error type every source returns, and its mapping to each facade.

use std::time::Duration;

/// Failures a source or the registry can report. Facades map these with [`SourceError::code`],
/// [`SourceError::exit_code`] and [`SourceError::http_status`]; the mapping table lives in
/// `docs/architecture.md`.
#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("unknown source: {0}")]
    UnknownSource(String),
    #[error("unknown operation: {0}")]
    UnknownOperation(String),
    #[error("{0}")]
    NotConfigured(String),
    #[error("authentication failed: {0}")]
    Auth(String),
    #[error("rate limited: {message}")]
    RateLimited {
        retry_after: Option<Duration>,
        message: String,
    },
    #[error("not found: {0}")]
    NotFound(String),
    #[error("upstream error (status {status}): {message}")]
    Upstream { status: u16, message: String },
    #[error("timed out after {0:?}")]
    Timeout(Duration),
    #[error("transport error: {0}")]
    Transport(String),
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("internal error: {0}")]
    Internal(String),
}

impl SourceError {
    pub fn invalid_input(error: impl std::fmt::Display) -> Self {
        Self::InvalidInput(error.to_string())
    }

    pub fn internal(error: impl std::fmt::Display) -> Self {
        Self::Internal(error.to_string())
    }

    pub fn transport(error: impl std::fmt::Display) -> Self {
        Self::Transport(error.to_string())
    }

    /// Stable machine-readable code shared by every facade.
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidInput(_) => "invalid_input",
            Self::UnknownSource(_) | Self::UnknownOperation(_) => "unknown_operation",
            Self::NotConfigured(_) => "not_configured",
            Self::Auth(_) => "upstream_auth",
            Self::RateLimited { .. } => "rate_limited",
            Self::NotFound(_) => "not_found",
            Self::Upstream { .. } => "upstream",
            Self::Timeout(_) => "timeout",
            Self::Transport(_) => "transport",
            Self::Unsupported(_) => "unsupported",
            Self::Internal(_) => "internal",
        }
    }

    /// Process exit code for the CLI: usage problems exit 2, everything else 1.
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::InvalidInput(_) | Self::UnknownSource(_) | Self::UnknownOperation(_) => 2,
            _ => 1,
        }
    }

    /// HTTP status for REST. Upstream failures never forward their own status: 401 is reserved
    /// for mcpie's bearer token and 404 for mcpie's own routes plus genuine not-found answers.
    pub fn http_status(&self) -> u16 {
        match self {
            Self::InvalidInput(_) => 400,
            Self::UnknownSource(_) | Self::UnknownOperation(_) | Self::NotFound(_) => 404,
            Self::NotConfigured(_) => 503,
            Self::Auth(_) | Self::Upstream { .. } | Self::Transport(_) => 502,
            Self::RateLimited { .. } => 429,
            Self::Timeout(_) => 504,
            Self::Unsupported(_) => 501,
            Self::Internal(_) => 500,
        }
    }

    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::RateLimited { retry_after, .. } => *retry_after,
            _ => None,
        }
    }

    /// True for failures that are the caller's protocol mistake rather than a tool outcome; MCP
    /// reports these as JSON-RPC errors instead of in-band tool errors.
    pub fn is_protocol_error(&self) -> bool {
        matches!(
            self,
            Self::UnknownSource(_) | Self::UnknownOperation(_) | Self::Internal(_)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_codes_and_statuses() {
        let error = SourceError::RateLimited {
            retry_after: Some(Duration::from_secs(3)),
            message: "slow".into(),
        };
        assert_eq!(error.code(), "rate_limited");
        assert_eq!(error.http_status(), 429);
        assert_eq!(error.exit_code(), 1);
        assert_eq!(error.retry_after(), Some(Duration::from_secs(3)));
        assert_eq!(SourceError::InvalidInput("x".into()).exit_code(), 2);
        assert_eq!(
            SourceError::Upstream {
                status: 401,
                message: "no".into()
            }
            .http_status(),
            502
        );
        assert!(SourceError::UnknownOperation("x".into()).is_protocol_error());
        assert!(!SourceError::Auth("x".into()).is_protocol_error());
    }
}
