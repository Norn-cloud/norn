//! Exact JSON admission and deterministic redaction for received observations.
//!
//! DP-EXACT-1: parsing refuses numbers or duplicate keys changed by JSON decoding.
//! DP-EXACT-2: integers in the u64 range remain exact beyond f64 precision.
//! DP-REDACT-1: an invalid policy leaves the payload unchanged.
//! DP-REDACT-2: repeated redaction preserves the payload and sorted outermost
//! replacement paths, including wildcard overlaps and escaped member names.
//! Sampled correspondence evidence: `boundary_invariants_conform`.

use std::collections::BTreeSet;

use norn_ir::canonical::{CanonicalJsonError, HashProfile};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The contract identifier carried by every raw observation.
pub const RAW_OBSERVATION_CONTRACT: &str = "norn.raw_observation/v1";

/// The value that replaces a redacted JSON node.
pub const REDACTED: &str = "[redacted]";

/// Whether the source record was written or deleted.
///
/// The derived order (`Upsert < Delete`) breaks ties between observations of
/// the same `source_version`: a delete of a version supersedes its upsert.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ObservationKind {
    /// The record was created or changed; `payload` is its state as received.
    Upsert,
    /// The record was deleted; `payload` is the tombstone as received.
    Delete,
}

/// A versioned, deterministic redaction policy applied before load.
///
/// Each pointer is an RFC 6901 JSON pointer; a `*` token matches every array
/// element or object member at that level (so a literal `*` key cannot be
/// addressed). Matched nodes are replaced with [`REDACTED`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RedactionPolicy {
    /// The policy identity.
    pub id: String,
    /// The policy version; a changed pointer list is a new version.
    pub version: String,
    /// The JSON pointers to redact.
    pub pointers: Vec<String>,
}

/// Provenance of the redaction applied to one observation.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RedactionRecord {
    /// The policy identity.
    pub policy_id: String,
    /// The policy version.
    pub policy_version: String,
    /// The concrete JSON pointers that were replaced, sorted and unique.
    pub redacted: Vec<String>,
}

impl RedactionPolicy {
    /// Apply the policy to `payload` in place and return its record.
    ///
    /// The result is a pure function of `(policy, payload)`: applying the same
    /// policy to the same input always yields the same payload and record,
    /// and applying it twice changes nothing further.
    ///
    /// # Errors
    ///
    /// [`ObservationError::Empty`] for an empty id or version, and
    /// [`ObservationError::InvalidPointer`] for a pointer that does not start
    /// with `/` (the whole-document pointer `""` is refused: a fully redacted
    /// payload should not be ingested at all).
    pub fn apply(&self, payload: &mut Value) -> Result<RedactionRecord, ObservationError> {
        non_empty("redaction.policy_id", &self.id)?;
        non_empty("redaction.policy_version", &self.version)?;
        let pointers: BTreeSet<&str> = self.pointers.iter().map(String::as_str).collect();
        let validated = pointers
            .iter()
            .map(|pointer| {
                pointer
                    .strip_prefix('/')
                    .ok_or_else(|| ObservationError::InvalidPointer {
                        pointer: (*pointer).to_owned(),
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut redacted = BTreeSet::new();
        for rest in validated {
            let tokens: Vec<String> = rest.split('/').map(unescape_token).collect();
            redact_at(payload, &tokens, String::new(), &mut redacted);
        }
        // Overlapping pointers (`/*/secret` and `/a`) can replace a node and
        // then its ancestor. Only the outermost replaced nodes are recorded,
        // so re-applying the policy yields the same record.
        let outermost: Vec<String> = redacted
            .iter()
            .filter(|pointer| {
                !pointer.match_indices('/').any(|(index, _)| {
                    index > 0
                        && pointer
                            .get(..index)
                            .is_some_and(|prefix| redacted.contains(prefix))
                })
            })
            .cloned()
            .collect();
        Ok(RedactionRecord {
            policy_id: self.id.clone(),
            policy_version: self.version.clone(),
            redacted: outermost,
        })
    }
}

fn unescape_token(token: &str) -> String {
    token.replace("~1", "/").replace("~0", "~")
}

fn escape_token(token: &str) -> String {
    token.replace('~', "~0").replace('/', "~1")
}

fn redact_at(value: &mut Value, tokens: &[String], at: String, redacted: &mut BTreeSet<String>) {
    let Some((head, rest)) = tokens.split_first() else {
        *value = Value::String(REDACTED.to_owned());
        redacted.insert(at);
        return;
    };
    match value {
        Value::Object(members) => {
            for (key, member) in members.iter_mut() {
                if head == "*" || head == key {
                    let path = format!("{at}/{}", escape_token(key));
                    redact_at(member, rest, path, redacted);
                }
            }
        }
        Value::Array(items) => {
            for (index, item) in items.iter_mut().enumerate() {
                if head == "*" || *head == index.to_string() {
                    redact_at(item, rest, format!("{at}/{index}"), redacted);
                }
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

/// Parse received JSON bytes, refusing any document `serde_json::Value`
/// cannot hold exactly.
///
/// `Value` stores numbers as `u64`/`i64`/`f64`, so a decimal with more
/// precision than `f64` (`1.00000000000000001`) would silently change, and a
/// duplicate key would silently collapse. Either would make the stored
/// payload and its digest differ from the evidence received. This compares
/// the Norn1 canonical form of the bytes with that of the parsed value and
/// refuses the document unless they are identical. Every ingest path builds
/// its payload through this function.
///
/// # Errors
///
/// [`ObservationError::Json`] for invalid JSON,
/// [`ObservationError::Canonical`] for a document Norn1 refuses (duplicate
/// keys, excessive nesting), and [`ObservationError::LossyPayload`] when the
/// parsed value is not exact.
pub fn parse_exact_json(bytes: &[u8]) -> Result<Value, ObservationError> {
    let value: Value = serde_json::from_slice(bytes)?;
    let received = HashProfile::Norn1.canonicalize_json(bytes)?;
    let parsed = HashProfile::Norn1.canonicalize_json(serde_json::to_string(&value)?)?;
    if received != parsed {
        return Err(ObservationError::LossyPayload);
    }
    Ok(value)
}

fn non_empty(field: &'static str, value: &str) -> Result<(), ObservationError> {
    if value.trim().is_empty() {
        Err(ObservationError::Empty { field })
    } else {
        Ok(())
    }
}

/// Why an observation was refused.
#[derive(Debug, thiserror::Error)]
pub enum ObservationError {
    /// A required field was empty.
    #[error("raw observation field `{field}` is empty")]
    Empty {
        /// The field.
        field: &'static str,
    },
    /// The contract tag is not [`RAW_OBSERVATION_CONTRACT`].
    #[error("unsupported raw observation contract `{found}`")]
    ContractMismatch {
        /// The tag found.
        found: String,
    },
    /// The source name contains `/`, which would make the observation id
    /// ambiguous.
    #[error("source `{source_name}` must not contain `/`")]
    InvalidSource {
        /// The source name.
        source_name: String,
    },
    /// The recorded digest does not match the payload.
    #[error("payload digest mismatch: recorded {recorded}, computed {computed}")]
    DigestMismatch {
        /// The digest in the observation.
        recorded: String,
        /// The digest of its payload.
        computed: String,
    },
    /// A redaction pointer is not a non-empty RFC 6901 pointer.
    #[error("invalid redaction pointer `{pointer}`")]
    InvalidPointer {
        /// The pointer.
        pointer: String,
    },
    /// The payload is not Norn1-canonicalizable.
    #[error("payload is not canonical JSON: {0}")]
    Canonical(#[from] CanonicalJsonError),
    /// The received JSON cannot be held exactly (a number beyond `f64`
    /// precision or a duplicate key); see [`parse_exact_json`].
    #[error("the payload cannot be represented exactly (number precision or duplicate key)")]
    LossyPayload,
    /// JSON encoding failed.
    #[error("raw observation JSON: {0}")]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    #[allow(clippy::expect_used, reason = "conformance assertions")]
    fn boundary_invariants_conform() {
        for bytes in [
            br#"{"a":1.00000000000000001}"#.as_slice(),
            br#"{"a":1,"a":2}"#,
        ] {
            assert!(parse_exact_json(bytes).is_err());
        }
        assert_eq!(
            parse_exact_json(br#"{"a":42}"#).expect("exact"),
            json!({"a":42})
        );
        assert_eq!(
            parse_exact_json(br#"9007199254740993"#).expect("exact integer"),
            json!(9007199254740993_u64)
        );
        let original = json!({"a":{"secret":1},"a/b":{"secret":2},"c":{"secret":3}});
        for pointers in [vec!["/a", "x"], vec!["/a", ""]] {
            let policy = RedactionPolicy {
                id: "policy".into(),
                version: "1".into(),
                pointers: pointers.into_iter().map(str::to_owned).collect(),
            };
            let mut payload = original.clone();
            assert!(matches!(
                policy.apply(&mut payload),
                Err(ObservationError::InvalidPointer { .. })
            ));
            assert_eq!(payload, original);
        }
        let policy = RedactionPolicy {
            id: "policy".into(),
            version: "1".into(),
            pointers: vec!["/*/secret".into(), "/a".into(), "/a~1b".into()],
        };
        let mut payload = original;
        let record = policy.apply(&mut payload).expect("redact");
        assert_eq!(
            payload,
            json!({"a":REDACTED,"a/b":REDACTED,"c":{"secret":REDACTED}})
        );
        assert_eq!(record.redacted, vec!["/a", "/a~1b", "/c/secret"]);
        assert_eq!(policy.apply(&mut payload).expect("repeat"), record);
        assert_eq!(
            payload,
            json!({"a":REDACTED,"a/b":REDACTED,"c":{"secret":REDACTED}})
        );
    }
}
