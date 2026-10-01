//! OAuth 2.0 for installed applications: refresh-token exchange and the loopback flow.
//!
//! mcpie ships no client id. The user creates an OAuth client of type "Desktop app" in Google
//! Cloud, then `mcpie auth <source>` opens the consent page, receives the code on
//! `http://127.0.0.1:<random port>/`, exchanges it and stores the refresh token in the user
//! configuration file.

use std::hash::{BuildHasher, Hasher};
use std::path::Path;
use std::time::Duration;

use reqwest::Method;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::config::Secret;
use crate::model::SourceError;
use crate::sources::http::Http;

/// What the flow needs to know about the client and the provider.
pub struct Authorization {
    pub client_id: String,
    pub client_secret: Secret,
    pub scopes: Vec<String>,
    pub auth_url: String,
    pub token_url: String,
}

/// The provider's answer to a code exchange.
#[derive(Debug)]
pub struct Tokens {
    pub access_token: Secret,
    pub refresh_token: Option<Secret>,
    pub expires_in: Duration,
    pub scope: Option<String>,
}

/// Exchange a refresh token for an access token and its lifetime.
pub async fn refresh(
    http: &Http,
    token_url: &str,
    client_id: &str,
    client_secret: &Secret,
    refresh_token: &Secret,
) -> Result<(Secret, Duration), SourceError> {
    let form = [
        ("client_id", client_id),
        ("client_secret", client_secret.expose()),
        ("refresh_token", refresh_token.expose()),
        ("grant_type", "refresh_token"),
    ];
    let response = http
        .send(http.request(Method::POST, token_url).form(&form))
        .await?;
    let body = response.json();
    if !response.is_success() {
        let error = body
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let description = body
            .get("error_description")
            .and_then(Value::as_str)
            .unwrap_or("");
        return Err(SourceError::Auth(format!(
            "token refresh failed: {error} {description}; run `mcpie auth` again"
        )));
    }
    let token = body
        .get("access_token")
        .and_then(Value::as_str)
        .ok_or_else(|| SourceError::Auth("token refresh returned no access_token".into()))?;
    let ttl = body
        .get("expires_in")
        .and_then(Value::as_u64)
        .unwrap_or(3600);
    Ok((Secret::new(token), Duration::from_secs(ttl)))
}

/// The consent URL for `redirect_uri` and `state`.
pub fn auth_url(auth: &Authorization, redirect_uri: &str, state: &str) -> String {
    let mut serializer = form_urlencoded::Serializer::new(String::new());
    serializer
        .append_pair("client_id", &auth.client_id)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("response_type", "code")
        .append_pair("scope", &auth.scopes.join(" "))
        .append_pair("access_type", "offline")
        .append_pair("prompt", "consent")
        .append_pair("state", state);
    format!("{}?{}", auth.auth_url, serializer.finish())
}

/// Extract the authorization code from the redirect request line, checking `state`.
pub fn parse_redirect(request_line: &str, expected_state: &str) -> Result<String, String> {
    let target = request_line
        .split_whitespace()
        .nth(1)
        .ok_or("malformed request")?;
    let query = target.split_once('?').map(|(_, q)| q).unwrap_or("");
    let mut code = None;
    let mut state = None;
    let mut error = None;
    for (key, value) in form_urlencoded::parse(query.as_bytes()) {
        match key.as_ref() {
            "code" => code = Some(value.into_owned()),
            "state" => state = Some(value.into_owned()),
            "error" => error = Some(value.into_owned()),
            _ => {}
        }
    }
    if let Some(error) = error {
        return Err(format!("authorization refused: {error}"));
    }
    if state.as_deref() != Some(expected_state) {
        return Err("state mismatch; the redirect did not come from this login attempt".into());
    }
    code.ok_or_else(|| "redirect carried no code".into())
}

fn random_state() -> String {
    let random = std::collections::hash_map::RandomState::new();
    let mut hasher = random.build_hasher();
    hasher.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    );
    hasher.write_u32(std::process::id());
    let a = hasher.finish();
    hasher.write_u64(a);
    format!("{a:016x}{:016x}", hasher.finish())
}

/// Run the loopback flow: bind a local port, hand the consent URL to `on_url`, wait for the
/// redirect, and exchange the code. Nothing is written to stdout here.
pub async fn authorize(
    http: &Http,
    auth: &Authorization,
    on_url: impl FnOnce(&str),
    timeout: Duration,
) -> Result<Tokens, SourceError> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(SourceError::transport)?;
    let port = listener
        .local_addr()
        .map_err(SourceError::transport)?
        .port();
    let redirect_uri = format!("http://127.0.0.1:{port}/");
    let state = random_state();
    on_url(&auth_url(auth, &redirect_uri, &state));

    let (mut stream, _) = tokio::time::timeout(timeout, listener.accept())
        .await
        .map_err(|_| SourceError::Timeout(timeout))?
        .map_err(SourceError::transport)?;
    let mut buffer = vec![0u8; 8192];
    let mut read = 0;
    loop {
        let n = stream
            .read(&mut buffer[read..])
            .await
            .map_err(SourceError::transport)?;
        if n == 0 {
            break;
        }
        read += n;
        if buffer[..read].windows(4).any(|w| w == b"\r\n\r\n") || read == buffer.len() {
            break;
        }
    }
    let request = String::from_utf8_lossy(&buffer[..read]);
    let first_line = request.lines().next().unwrap_or_default();
    let outcome = parse_redirect(first_line, &state);
    let (status, page) = match &outcome {
        Ok(_) => (
            "200 OK",
            "<!doctype html><title>mcpie</title><p>Authorized. You can close this tab and return to the terminal.</p>",
        ),
        Err(_) => (
            "400 Bad Request",
            "<!doctype html><title>mcpie</title><p>Authorization failed; see the terminal.</p>",
        ),
    };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}",
        page.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
    let code = outcome.map_err(SourceError::Auth)?;

    let form = [
        ("code", code.as_str()),
        ("client_id", auth.client_id.as_str()),
        ("client_secret", auth.client_secret.expose()),
        ("redirect_uri", redirect_uri.as_str()),
        ("grant_type", "authorization_code"),
    ];
    let response = http
        .send(http.request(Method::POST, &auth.token_url).form(&form))
        .await?;
    let body = response.json();
    if !response.is_success() {
        let error = body
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let description = body
            .get("error_description")
            .and_then(Value::as_str)
            .unwrap_or("");
        return Err(SourceError::Auth(format!(
            "code exchange failed: {error} {description}"
        )));
    }
    Ok(Tokens {
        access_token: Secret::new(
            body.get("access_token")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        ),
        refresh_token: body
            .get("refresh_token")
            .and_then(Value::as_str)
            .map(Secret::new),
        expires_in: Duration::from_secs(
            body.get("expires_in")
                .and_then(Value::as_u64)
                .unwrap_or(3600),
        ),
        scope: body.get("scope").and_then(Value::as_str).map(str::to_owned),
    })
}

/// Store the OAuth client and refresh token under `[sources.<id>.oauth]`, keeping every other
/// line and comment of the file intact. New files are created private (0600).
pub fn save(
    config_path: &Path,
    source_id: &str,
    client_id: &str,
    client_secret: &Secret,
    refresh_token: &Secret,
) -> Result<(), String> {
    let existing = match std::fs::read_to_string(config_path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(format!("{}: {error}", config_path.display())),
    };
    let mut document: toml_edit::DocumentMut = existing
        .parse()
        .map_err(|e| format!("{}: {e}", config_path.display()))?;
    let sources = table_entry(document.as_table_mut(), "sources", true)?;
    let source = table_entry(sources, source_id, true)?;
    let oauth = table_entry(source, "oauth", false)?;
    oauth.insert("client_id", toml_edit::value(client_id));
    oauth.insert("client_secret", toml_edit::value(client_secret.expose()));
    oauth.insert("refresh_token", toml_edit::value(refresh_token.expose()));
    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    #[cfg(unix)]
    let is_new = !config_path.exists();
    std::fs::write(config_path, document.to_string())
        .map_err(|e| format!("{}: {e}", config_path.display()))?;
    #[cfg(unix)]
    if is_new {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(config_path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// Get or create a standard table under `parent`, upgrading an inline table when the file
/// used the `oauth = { ... }` form.
fn table_entry<'a>(
    parent: &'a mut toml_edit::Table,
    key: &str,
    implicit: bool,
) -> Result<&'a mut toml_edit::Table, String> {
    let item = parent.entry(key).or_insert_with(|| {
        let mut table = toml_edit::Table::new();
        table.set_implicit(implicit);
        toml_edit::Item::Table(table)
    });
    if let toml_edit::Item::Value(toml_edit::Value::InlineTable(inline)) = item {
        *item = toml_edit::Item::Table(inline.clone().into_table());
    }
    item.as_table_mut()
        .ok_or_else(|| format!("{key} is not a table"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auth() -> Authorization {
        Authorization {
            client_id: "id".into(),
            client_secret: Secret::new("secret"),
            scopes: vec!["https://www.googleapis.com/auth/drive.readonly".into()],
            auth_url: "https://accounts.example/auth".into(),
            token_url: "https://accounts.example/token".into(),
        }
    }

    #[test]
    fn builds_consent_urls_and_parses_redirects() {
        let url = auth_url(&auth(), "http://127.0.0.1:1234/", "st4te");
        assert!(url.starts_with("https://accounts.example/auth?client_id=id&redirect_uri=http%3A%2F%2F127.0.0.1%3A1234%2F&response_type=code"));
        assert!(
            url.contains("access_type=offline")
                && url.contains("prompt=consent")
                && url.contains("state=st4te")
        );
        assert_eq!(
            parse_redirect("GET /?state=st4te&code=4%2Fabc HTTP/1.1", "st4te").unwrap(),
            "4/abc"
        );
        assert!(
            parse_redirect("GET /?state=other&code=x HTTP/1.1", "st4te")
                .unwrap_err()
                .contains("state")
        );
        assert!(
            parse_redirect("GET /?state=st4te&error=access_denied HTTP/1.1", "st4te")
                .unwrap_err()
                .contains("access_denied")
        );
        assert!(parse_redirect("GET / HTTP/1.1", "st4te").is_err());
        assert_ne!(random_state(), random_state());
    }

    #[test]
    fn save_keeps_comments_and_adds_the_oauth_table() {
        let dir = std::env::temp_dir().join(format!("mcpie-oauth-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("config.toml");
        save(
            &path,
            "gdrive",
            "id",
            &Secret::new("secret"),
            &Secret::new("refresh-1"),
        )
        .unwrap();
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.contains("[sources.gdrive.oauth]"), "{written}");
        std::fs::write(
            &path,
            format!(
                "# keep me\n{written}\n[sources.github]\n# also me\ndefault_owner = \"acme\"\n"
            ),
        )
        .unwrap();
        save(
            &path,
            "gdrive",
            "id",
            &Secret::new("secret"),
            &Secret::new("refresh-2"),
        )
        .unwrap();
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(
            written.contains("# keep me") && written.contains("# also me"),
            "{written}"
        );
        assert!(
            written.contains("refresh_token = \"refresh-2\"") && !written.contains("refresh-1"),
            "{written}"
        );
        assert!(written.contains("default_owner = \"acme\""));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::remove_file(&path);
            save(&path, "gmail", "id", &Secret::new("s"), &Secret::new("r")).unwrap();
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
}
