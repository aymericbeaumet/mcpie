//! A string that never leaks through `Debug`, `Display` or `Serialize`.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A credential value. Read it only through [`Secret::expose`].
#[derive(Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(transparent)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The raw value. Keep the call sites few and obvious.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Empty strings come from templates (`token = ""`) and count as absent.
    pub fn is_empty(&self) -> bool {
        self.0.trim().is_empty()
    }
}

impl From<&str> for Secret {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for Secret {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(if self.is_empty() {
            "Secret(\"\")"
        } else {
            "Secret(***)"
        })
    }
}

impl Serialize for Secret {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(if self.is_empty() { "" } else { "***" })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_prints_the_value() {
        let secret = Secret::new("ghp_very_secret");
        assert_eq!(format!("{secret:?}"), "Secret(***)");
        assert_eq!(serde_json::to_string(&secret).unwrap(), "\"***\"");
        assert_eq!(secret.expose(), "ghp_very_secret");
        let parsed: Secret = serde_json::from_str("\"abc\"").unwrap();
        assert_eq!(parsed, Secret::new("abc"));
        assert!(Secret::new("  ").is_empty());
    }
}
