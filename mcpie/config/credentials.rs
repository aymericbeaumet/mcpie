//! Credential resolution: configured token, then `token_command`, then well-known environment
//! variables, then (GitHub only) the `gh` CLI. Resolved once per process by each source.

use std::fmt;
use std::time::Duration;

use serde::Serialize;
use tokio::process::Command;

use super::Secret;
use crate::model::SourceError;

const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

/// Where a credential came from, shown by `mcpie sources` and `mcpie config show`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "detail")]
pub enum Provenance {
    Config,
    TokenCommand(String),
    Env(String),
    GhCli,
}

impl fmt::Display for Provenance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Config => f.write_str("config"),
            Self::TokenCommand(command) => write!(f, "token_command ({command})"),
            Self::Env(name) => write!(f, "env:{name}"),
            Self::GhCli => f.write_str("gh auth token"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Credential {
    pub secret: Secret,
    pub provenance: Provenance,
}

/// What a source accepts, in resolution order.
#[derive(Debug, Clone, Default)]
pub struct CredentialSpec<'a> {
    pub token: Option<&'a Secret>,
    pub token_command: Option<&'a str>,
    /// Environment variables to try, in order.
    pub env: &'a [&'a str],
    /// Fall back to `gh auth token` when nothing else is set.
    pub gh_cli: bool,
}

/// Resolve the first available credential, or `None` when the source is not configured.
pub async fn resolve(spec: CredentialSpec<'_>) -> Result<Option<Credential>, SourceError> {
    if let Some(token) = spec.token.filter(|t| !t.is_empty()) {
        return Ok(Some(Credential {
            secret: token.clone(),
            provenance: Provenance::Config,
        }));
    }
    if let Some(command) = spec.token_command.map(str::trim).filter(|c| !c.is_empty()) {
        let output = run_shell(command).await?;
        if output.is_empty() {
            return Err(SourceError::NotConfigured(format!(
                "token_command {command:?} printed nothing"
            )));
        }
        return Ok(Some(Credential {
            secret: Secret::new(output),
            provenance: Provenance::TokenCommand(command.to_owned()),
        }));
    }
    for name in spec.env {
        if let Ok(value) = std::env::var(name)
            && !value.trim().is_empty()
        {
            return Ok(Some(Credential {
                secret: Secret::new(value.trim()),
                provenance: Provenance::Env((*name).to_owned()),
            }));
        }
    }
    if spec.gh_cli
        && let Some(token) = gh_auth_token().await
    {
        return Ok(Some(Credential {
            secret: Secret::new(token),
            provenance: Provenance::GhCli,
        }));
    }
    Ok(None)
}

async fn gh_auth_token() -> Option<String> {
    let mut command = Command::new("gh");
    command
        .args(["auth", "token"])
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    let output = tokio::time::timeout(COMMAND_TIMEOUT, command.output())
        .await
        .ok()?
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let token = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    (!token.is_empty()).then_some(token)
}

/// Run `command` through the platform shell and return its trimmed stdout. The command string is
/// logged by callers at debug level; its output never is.
pub async fn run_shell(command: &str) -> Result<String, SourceError> {
    let mut process = if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.args(["/C", command]);
        c
    } else {
        let mut c = Command::new("sh");
        c.args(["-c", command]);
        c
    };
    process
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    let output = tokio::time::timeout(COMMAND_TIMEOUT, process.output())
        .await
        .map_err(|_| {
            SourceError::NotConfigured(format!(
                "token_command {command:?} timed out after {COMMAND_TIMEOUT:?}"
            ))
        })?
        .map_err(|error| {
            SourceError::NotConfigured(format!(
                "token_command {command:?} could not start: {error}"
            ))
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(SourceError::NotConfigured(format!(
            "token_command {command:?} exited with {}: {}",
            output.status,
            stderr.trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn configured_token_wins() {
        let token = Secret::new("from-config");
        let credential = resolve(CredentialSpec {
            token: Some(&token),
            token_command: Some("echo nope"),
            ..Default::default()
        })
        .await
        .unwrap()
        .unwrap();
        assert_eq!(credential.secret.expose(), "from-config");
        assert_eq!(credential.provenance, Provenance::Config);
    }

    #[tokio::test]
    async fn empty_token_falls_through_to_command() {
        let token = Secret::new("");
        let credential = resolve(CredentialSpec {
            token: Some(&token),
            token_command: Some("echo   from-command  "),
            ..Default::default()
        })
        .await
        .unwrap()
        .unwrap();
        assert_eq!(credential.secret.expose(), "from-command");
        assert!(matches!(credential.provenance, Provenance::TokenCommand(_)));
    }

    #[tokio::test]
    async fn failing_command_is_a_configuration_error() {
        let error = resolve(CredentialSpec {
            token_command: Some("exit 3"),
            ..Default::default()
        })
        .await
        .unwrap_err();
        assert!(matches!(error, SourceError::NotConfigured(_)), "{error}");
        let error = resolve(CredentialSpec {
            token_command: Some("echo"),
            ..Default::default()
        })
        .await
        .unwrap_err();
        assert!(error.to_string().contains("printed nothing"));
    }

    #[tokio::test]
    async fn environment_is_tried_in_order() {
        // SAFETY: test-only, unique variable names, single-threaded access to them.
        unsafe {
            std::env::set_var("XMCPIE_CRED_SECOND", "second");
        }
        let credential = resolve(CredentialSpec {
            env: &["XMCPIE_CRED_FIRST", "XMCPIE_CRED_SECOND"],
            ..Default::default()
        })
        .await
        .unwrap()
        .unwrap();
        assert_eq!(credential.secret.expose(), "second");
        assert_eq!(
            credential.provenance,
            Provenance::Env("XMCPIE_CRED_SECOND".into())
        );
        assert!(resolve(CredentialSpec::default()).await.unwrap().is_none());
    }
}
