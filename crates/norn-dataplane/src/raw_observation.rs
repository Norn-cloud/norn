//! The raw-observation contract (`norn.raw_observation/v1`, T12-DP-1).
//!
//! Ruling 2026-09-25 §8: raw is an **append-only log of observations**, never
//! a mirror of the source. Sources mutate and delete; observations do not.
//! Current state is a transform over the log; deletes
//! arrive as observations ([`ObservationKind::Delete`]). The only permitted
//! pre-load transform is compliance-mandated redaction, as a versioned
//! deterministic function recorded in the observation ([`RedactionRecord`]).
//!
//! # Fields
//!
//! | field | meaning |
//! |---|---|
//! | `contract` | always [`RAW_OBSERVATION_CONTRACT`] |
//! | `source` | the source system (`erpnext`, `shopify`) |
//! | `source_id` | the record's identity in the source (`Sales Order/SO-0001`) |
//! | `event_id` | the source event's identity; with `source` and `source_id`, the dedup key |
//! | `kind` | `upsert` or `delete` |
//! | `source_version` | the source-asserted version, ordered lexicographically (for Frappe the `modified` timestamp) |
//! | `payload` | the record as received, after redaction |
//! | `payload_digest` | Norn1 hash of `payload` |
//! | `extracted_at` | host receipt time, milliseconds since the Unix epoch |
//! | `cursor` | the source cursor at extraction, if the path has one (sweeps, dlt) |
//! | `pipeline_version` | the extract pipeline and its version; it is the provenance `mapping_version` |
//! | `redaction` | which redaction policy ran and which JSON pointers it replaced |
//!
//! Payloads must be decoded with [`parse_exact_json`], which refuses JSON
//! that `serde_json::Value` would change (excess number precision, duplicate
//! keys), so the stored payload and its digest are the evidence received.
//!
//! Public observation types serialize but deliberately do not implement
//! `Deserialize`. Received JSON must enter through the checked byte decoders:
//!
//! ```compile_fail
//! use norn_dataplane::ObservationDraft;
//! let _: ObservationDraft = serde_json::from_str("{}").unwrap();
//! ```
//!
//! ```compile_fail
//! use norn_dataplane::RawObservation;
//! let _: RawObservation = serde_json::from_str("{}").unwrap();
//! ```
//!
//! ```compile_fail
//! use norn_dataplane::ObservationSubmission;
//! let _: ObservationSubmission = serde_json::from_str("{}").unwrap();
//! ```
//!
//! Observations are history, not authorization: ingesting one records what a
//! source said; it never authorizes an action.
//!
//!
//! DP-BOUNDARY-1: published dependency metadata equals the executable policy
//! (`norn-skeleton-guard::machine_boundaries_match_dependency_policy`).
//! DP-EXACT-1: parsing refuses numbers or duplicate keys changed by JSON decoding.
//! DP-EXACT-2: integers in the u64 range remain exact beyond f64 precision.
//! DP-REDACT-1: an invalid policy leaves the payload unchanged.
//! DP-REDACT-2: repeated redaction preserves the payload and sorted outermost
//! replacement paths, including wildcard overlaps and escaped member names.
//! DP-REDACT-3: malformed RFC 6901 escapes refuse before any mutation.
//! Sampled correspondence evidence: `boundary_invariants_conform`.
//! DP-OBS-1: sealing hashes the redacted payload; checked decoding verifies it.
//! DP-OBS-2: received draft/observation bytes cannot round numeric evidence or
//! collapse duplicate keys. Public types have no unchecked Deserialize path.
//! Sampled correspondence evidence: `observation_sealing_conforms`.

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
    /// with `/` or has an escape other than `~0` or `~1` (the whole-document pointer `""` is refused: a fully redacted
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
                    .filter(|rest| {
                        rest.split('~')
                            .skip(1)
                            .all(|escaped| escaped.starts_with('0') || escaped.starts_with('1'))
                    })
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

/// An observation before sealing: everything except the digest and the
/// redaction record, which [`ObservationDraft::seal`] computes.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ObservationDraft {
    /// The source system.
    pub source: String,
    /// The record's identity in the source.
    pub source_id: String,
    /// The source event's identity (with `source` and `source_id`, the dedup key).
    pub event_id: String,
    /// Upsert or delete.
    pub kind: ObservationKind,
    /// The source-asserted version, ordered lexicographically.
    pub source_version: String,
    /// The record as received.
    pub payload: Value,
    /// Host receipt time, milliseconds since the Unix epoch.
    pub extracted_at: u64,
    /// The source cursor at extraction, if any.
    #[serde(default)]
    pub cursor: Option<Value>,
    /// The extract pipeline and its version.
    pub pipeline_version: String,
}

// Private decoding adapter; the public type deliberately has no Deserialize.
#[derive(Deserialize)]
#[serde(remote = "ObservationDraft", deny_unknown_fields)]
struct ObservationDraftWire {
    source: String,
    source_id: String,
    event_id: String,
    kind: ObservationKind,
    source_version: String,
    payload: Value,
    extracted_at: u64,
    #[serde(default)]
    cursor: Option<Value>,
    pipeline_version: String,
}

impl ObservationDraft {
    /// Decode a draft from received bytes without rounding numbers or
    /// collapsing duplicate keys. Public observation types deliberately do
    /// not implement `Deserialize`; use this boundary for received JSON.
    ///
    /// # Errors
    ///
    /// A lossy, malformed, or structurally invalid document. Identity fields
    /// are checked when the draft is sealed.
    pub fn from_json_slice(bytes: &[u8]) -> Result<Self, ObservationError> {
        Ok(ObservationDraftWire::deserialize(parse_exact_json(bytes)?)?)
    }

    /// Apply the redaction policy (if any), hash the payload, and produce the
    /// immutable observation.
    ///
    /// # Errors
    ///
    /// An empty identity field, an invalid redaction pointer, or a payload
    /// that is not Norn1-canonicalizable.
    pub fn seal(
        self,
        policy: Option<&RedactionPolicy>,
    ) -> Result<RawObservation, ObservationError> {
        let Self {
            source,
            source_id,
            event_id,
            kind,
            source_version,
            mut payload,
            extracted_at,
            cursor,
            pipeline_version,
        } = self;
        let redaction = policy
            .map(|policy| policy.apply(&mut payload))
            .transpose()?;
        let payload_digest = payload_digest(&payload)?;
        let observation = RawObservation {
            contract: RAW_OBSERVATION_CONTRACT.to_owned(),
            source,
            source_id,
            event_id,
            kind,
            source_version,
            payload,
            payload_digest,
            extracted_at,
            cursor,
            pipeline_version,
            redaction,
        };
        observation.validate()?;
        Ok(observation)
    }
}

/// One received observation (`norn.raw_observation/v1`). Immutable once
/// sealed; see the module docs for the field contract.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RawObservation {
    /// Always [`RAW_OBSERVATION_CONTRACT`].
    pub contract: String,
    /// The source system.
    pub source: String,
    /// The record's identity in the source.
    pub source_id: String,
    /// The source event's identity (with `source` and `source_id`, the dedup key).
    pub event_id: String,
    /// Upsert or delete.
    pub kind: ObservationKind,
    /// The source-asserted version, ordered lexicographically.
    pub source_version: String,
    /// The record as received, after redaction.
    pub payload: Value,
    /// Norn1 hash of `payload`.
    pub payload_digest: String,
    /// Host receipt time, milliseconds since the Unix epoch.
    pub extracted_at: u64,
    /// The source cursor at extraction, if any.
    #[serde(default)]
    pub cursor: Option<Value>,
    /// The extract pipeline and its version.
    pub pipeline_version: String,
    /// The redaction applied before load, if any.
    #[serde(default)]
    pub redaction: Option<RedactionRecord>,
}

// Private decoding adapter; the public type deliberately has no Deserialize.
#[derive(Deserialize)]
#[serde(remote = "RawObservation", deny_unknown_fields)]
struct RawObservationWire {
    contract: String,
    source: String,
    source_id: String,
    event_id: String,
    kind: ObservationKind,
    source_version: String,
    payload: Value,
    payload_digest: String,
    extracted_at: u64,
    #[serde(default)]
    cursor: Option<Value>,
    pipeline_version: String,
    #[serde(default)]
    redaction: Option<RedactionRecord>,
}

impl RawObservation {
    /// Check the contract tag, the identity fields, and that the digest
    /// matches the payload. Call it on every observation decoded from
    /// untrusted bytes.
    ///
    /// # Errors
    ///
    /// The first violated invariant.
    pub fn validate(&self) -> Result<(), ObservationError> {
        if self.contract != RAW_OBSERVATION_CONTRACT {
            return Err(ObservationError::ContractMismatch {
                found: self.contract.clone(),
            });
        }
        non_empty("source", &self.source)?;
        non_empty("source_id", &self.source_id)?;
        non_empty("event_id", &self.event_id)?;
        non_empty("source_version", &self.source_version)?;
        non_empty("pipeline_version", &self.pipeline_version)?;
        if self.source.contains('/') {
            return Err(ObservationError::InvalidSource {
                source_name: self.source.clone(),
            });
        }
        let computed = payload_digest(&self.payload)?;
        if computed != self.payload_digest {
            return Err(ObservationError::DigestMismatch {
                recorded: self.payload_digest.clone(),
                computed,
            });
        }
        Ok(())
    }

    /// Decode and validate a stored observation from untrusted bytes, through
    /// [`parse_exact_json`] so the payload is exactly what was stored.
    ///
    /// # Errors
    ///
    /// A lossy or malformed document, or a failed [`RawObservation::validate`].
    pub fn from_json_slice(bytes: &[u8]) -> Result<Self, ObservationError> {
        let observation = RawObservationWire::deserialize(parse_exact_json(bytes)?)?;
        observation.validate()?;
        Ok(observation)
    }

    /// The observation's identity: `{source}/{source_id}/{event_id}`. This is
    /// the id the cell's observe inbox dedups on (norn-api
    /// `POST /v1/observations/{source}` with `eventId`).
    #[must_use]
    pub fn observation_id(&self) -> String {
        format!("{}/{}/{}", self.source, self.source_id, self.event_id)
    }

    /// The submission for the cell's observe inbox.
    ///
    /// # Errors
    ///
    /// [`ObservationError::Json`] if the observation cannot be encoded.
    pub fn submission(&self) -> Result<ObservationSubmission, ObservationError> {
        Ok(ObservationSubmission {
            source: self.source.clone(),
            source_id: self.source_id.clone(),
            event_id: self.event_id.clone(),
            mapping_version: self.pipeline_version.clone(),
            payload: serde_json::to_value(self)?,
        })
    }
}

/// What a host posts to `POST /v1/observations/{source}` (norn-api).
///
/// It serializes to the request body fields this crate owns (`sourceId`,
/// `eventId`, `mappingVersion`, `payload`); `source` is the path segment and
/// is not serialized. The host adds the request `scope` and `actor`, which
/// are deployment configuration, not observation data. The payload is the
/// whole sealed [`RawObservation`].
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ObservationSubmission {
    /// The `{source}` path segment.
    #[serde(skip)]
    pub source: String,
    /// The record's identity in the source.
    pub source_id: String,
    /// The source event id; the inbox dedups on `{source}/{sourceId}/{eventId}`.
    pub event_id: String,
    /// The pipeline version, recorded as the provenance mapping version.
    pub mapping_version: String,
    /// The sealed raw observation.
    pub payload: Value,
}

/// The JSON Schema of [`RawObservation`], generated from the Rust type.
#[must_use]
pub fn raw_observation_schema() -> schemars::Schema {
    schemars::schema_for!(RawObservation)
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

fn payload_digest(payload: &Value) -> Result<String, ObservationError> {
    // `to_string` is only the transport into the canonicalizer: Norn1 re-parses
    // and canonicalizes, so serde_json's map order cannot reach the hash.
    let text = serde_json::to_string(payload)?;
    Ok(HashProfile::Norn1.hash_json(text)?)
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
            parse_exact_json(b"9007199254740993").expect("exact integer"),
            json!(9_007_199_254_740_993_u64)
        );
        let original = json!({"a":{"secret":1},"a/b":{"secret":2},"c":{"secret":3}});
        for pointers in [
            vec!["/a", "x"],
            vec!["/a", ""],
            vec!["/a", "/token~2"],
            vec!["/a", "/token~"],
        ] {
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

    #[test]
    #[allow(clippy::expect_used, reason = "conformance assertions")]
    fn observation_sealing_conforms() {
        let bytes = br#"{"source":"fixture","source_id":"record","event_id":"event",
            "kind":"upsert","source_version":"1","payload":{"secret":1,"amount":42},
            "extracted_at":1,"pipeline_version":"fixture/v1"}"#;
        let policy = RedactionPolicy {
            id: "policy".into(),
            version: "1".into(),
            pointers: vec!["/secret".into()],
        };
        let observation = ObservationDraft::from_json_slice(bytes)
            .expect("draft")
            .seal(Some(&policy))
            .expect("seal");
        assert_eq!(observation.payload, json!({"secret":REDACTED,"amount":42}));
        assert_eq!(
            observation.payload_digest,
            payload_digest(&observation.payload).expect("digest")
        );
        let stored = serde_json::to_vec(&observation).expect("encode");
        assert_eq!(
            RawObservation::from_json_slice(&stored).expect("replay"),
            observation
        );
        let mut tampered = observation.clone();
        tampered.payload = json!({"secret":REDACTED,"amount":43});
        assert!(matches!(
            RawObservation::from_json_slice(&serde_json::to_vec(&tampered).expect("encode")),
            Err(ObservationError::DigestMismatch { .. })
        ));
        let lossy = String::from_utf8(bytes.to_vec())
            .expect("utf8")
            .replace("42", "1.00000000000000001");
        assert!(ObservationDraft::from_json_slice(lossy.as_bytes()).is_err());
        let duplicate = String::from_utf8(bytes.to_vec())
            .expect("utf8")
            .replace("\"amount\":42", "\"amount\":42,\"amount\":43");
        assert!(ObservationDraft::from_json_slice(duplicate.as_bytes()).is_err());
        let submission = observation.submission().expect("submission");
        assert_eq!(submission.source, "fixture");
        assert_eq!(
            RawObservation::from_json_slice(
                &serde_json::to_vec(&submission.payload).expect("payload")
            )
            .expect("checked submission"),
            observation
        );
    }
}
