//! Opaque pagination cursors.
//!
//! A cursor is base64url (no padding) of `{"v":1,"s":source,"o":operation,"p":state}`. It is
//! bound to the source and operation that produced it, and it never carries a URL, so a crafted
//! cursor cannot redirect a request or its credentials.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use super::SourceError;

const VERSION: u8 = 1;

#[derive(Serialize, Deserialize)]
struct Envelope<'a, P> {
    v: u8,
    s: &'a str,
    o: &'a str,
    p: P,
}

/// Encode a source-private page state into an opaque cursor.
pub fn encode<P: Serialize>(source: &str, operation: &str, state: &P) -> String {
    let envelope = Envelope {
        v: VERSION,
        s: source,
        o: operation,
        p: state,
    };
    let json = serde_json::to_vec(&envelope).expect("cursor state serializes");
    URL_SAFE_NO_PAD.encode(json)
}

/// Decode a cursor produced by [`encode`] for the same source and operation.
pub fn decode<P: DeserializeOwned>(
    cursor: &str,
    source: &str,
    operation: &str,
) -> Result<P, SourceError> {
    let invalid = |reason: &str| SourceError::InvalidInput(format!("invalid cursor: {reason}"));
    let bytes = URL_SAFE_NO_PAD
        .decode(cursor.trim())
        .map_err(|_| invalid("not base64url"))?;
    let envelope: Envelope<'_, P> =
        serde_json::from_slice(&bytes).map_err(|_| invalid("malformed"))?;
    if envelope.v != VERSION {
        return Err(invalid("unsupported version"));
    }
    if envelope.s != source || envelope.o != operation {
        return Err(invalid(&format!(
            "issued by {}.{}, not {source}.{operation}",
            envelope.s, envelope.o
        )));
    }
    Ok(envelope.p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct State {
        page: u32,
    }

    #[test]
    fn round_trips_and_binds_to_operation() {
        let cursor = encode("github", "list_issues", &State { page: 3 });
        assert!(!cursor.contains('='));
        let state: State = decode(&cursor, "github", "list_issues").unwrap();
        assert_eq!(state, State { page: 3 });
        let error = decode::<State>(&cursor, "github", "list_repos").unwrap_err();
        assert!(matches!(error, SourceError::InvalidInput(_)), "{error}");
        assert!(error.to_string().contains("github.list_issues"));
        assert!(decode::<State>("not a cursor", "github", "list_issues").is_err());
    }
}
