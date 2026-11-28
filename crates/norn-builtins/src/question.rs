//! `norn.question@1`: a read-only flow step that evaluates an admitted question
//! with `RequiredSources` freshness enforcement (`QUESTION-STEP-1`, Core 03/07).
//!
//! A flow lowers "read this admitted question" to `perform` of this Function.
//! The cell evaluates it itself, read-only, never an outside executor: it
//! evaluates the named admitted question through `QuestionRuntime` under the
//! run starter's authority and markings disclosure (`PRIVACY-1`/`PRIVACY-3`),
//! verifies `STEP-PIN-1` pins, and enforces `#1905` `RequiredSources` freshness.
//!
//! Fresh, complete answers deliver `Answered` output (`status`, structured
//! `rows`/`value`, and cryptographic/revision pins). Stale or incomplete
//! required sources deliver the typed failure `{_tag: "StaleSource", code,
//! freshness, complete, sources}` so the flow's decide table can withhold
//! downstream proposals. Authorization or marking/disclosure failures deliver
//! `{_tag: "Denied"}`; any other deterministic refusal delivers
//! `{_tag: "Refused", code}`. The step performs zero business-state mutations,
//! and a person who decided an earlier step lends it none of their rights.

use std::collections::BTreeMap;

use norn_ir::{RuntimeValue, TraceContext};

/// The Function id.
pub const FUNCTION_ID: &str = "norn.question";
/// The Function version.
pub const VERSION: u32 = 1;
/// The input schema document.
pub const INPUT_SCHEMA: &str = include_str!("schemas/question.input.json");
/// The output schema document.
pub const OUTPUT_SCHEMA: &str = include_str!("schemas/question.output.json");
/// The typed error schema document (`{_tag: "StaleSource" | "Denied" | "Refused", ...}`).
pub const ERROR_SCHEMA: &str = include_str!("schemas/question.error.json");
/// The longest question id, in bytes.
pub const MAX_QUESTION_ID_BYTES: usize = 256;
/// The longest pinned artifact version string, in bytes.
pub const MAX_VERSION_BYTES: usize = 128;
/// The longest `asOf` RFC 3339 timestamp string, in bytes.
pub const MAX_AS_OF_BYTES: usize = 64;
/// The largest step input, in canonical JSON bytes.
pub const MAX_INPUT_BYTES: u64 = 64 * 1024;

/// Optional target-definition pin carried by a `norn.question@1` step (`STEP-PIN-1`).
///
/// When present, the cell verifies every specified field against the active
/// admitted question definition inside the same definitions hold as evaluation,
/// failing closed if the active catalog no longer matches.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct QuestionStepPin {
    /// Exact question definition/artifact id.
    pub artifact_id: Option<String>,
    /// Exact question definition/artifact version string.
    pub version: Option<String>,
    /// Exact effective question definition digest (64 lowercase hex characters).
    pub digest: Option<String>,
    /// Exact cell definitions version (`>= 1`).
    pub definitions_version: Option<u64>,
}

/// Optional freshness guard tightening `RequiredSources` (`QUERY-FRESH-1`) for
/// a `norn.question@1` step. A step may only tighten freshness bounds, never
/// weaken them to allow stale or incomplete required sources.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct QuestionFreshnessGuard {
    /// Maximum allowed heartbeat lag in milliseconds (`>= 0`).
    pub max_age_ms: Option<u64>,
    /// When `true`, even `Fresh` (nonzero heartbeat lag) is rejected as
    /// `StaleSource`, requiring `Exact`.
    pub require_exact: Option<bool>,
}

/// One read-only question step: the question id, optional parameter bindings,
/// optional `asOf` effective cutoff, optional freshness guard, optional trace
/// context, and optional target-definition pin.
#[derive(Debug, Clone, PartialEq)]
pub struct QuestionStep {
    /// The admitted question id (retained `"question"` definition or named
    /// `QuestionRuntime` question).
    pub question: String,
    /// Optional parameter bindings (filter values for retained questions).
    pub params: Option<BTreeMap<String, RuntimeValue>>,
    /// Optional RFC 3339 UTC timestamp (`asOf` effective cutoff).
    pub as_of: Option<String>,
    /// Optional monotonic freshness tightening guard (`RequiredSources`).
    pub freshness: Option<QuestionFreshnessGuard>,
    /// Optional provenance trace context.
    pub trace: Option<TraceContext>,
    /// Optional target-definition pin enforced before evaluation (`STEP-PIN-1`).
    pub pin: Option<QuestionStepPin>,
}

/// Why a perform input is not a valid question step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepRefusal {
    /// The input is not `{question, params?, asOf?, freshness?, trace?, pin?}`
    /// with bounded, well-formed fields.
    Shape,
    /// The input is larger than [`MAX_INPUT_BYTES`].
    TooLarge,
}

impl StepRefusal {
    /// Stable refusal code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Shape => "question_step.shape",
            Self::TooLarge => "question_step.too_large",
        }
    }
}

/// Parse a perform record's step input as a question step.
///
/// # Errors
/// [`StepRefusal`] naming the broken bound.
pub fn parse_step(input: &RuntimeValue) -> Result<QuestionStep, StepRefusal> {
    let RuntimeValue::Object(members) = input else {
        return Err(StepRefusal::Shape);
    };
    if !members.keys().all(|key| {
        matches!(
            key.as_str(),
            "question" | "params" | "asOf" | "freshness" | "trace" | "pin"
        )
    }) {
        return Err(StepRefusal::Shape);
    }
    let Some(RuntimeValue::String(question)) = members.get("question") else {
        return Err(StepRefusal::Shape);
    };
    if question.trim().is_empty()
        || question.len() > MAX_QUESTION_ID_BYTES
        || question.chars().any(char::is_control)
    {
        return Err(StepRefusal::Shape);
    }
    let params = match members.get("params") {
        None => None,
        Some(RuntimeValue::Object(map)) => Some(map.clone()),
        Some(
            RuntimeValue::Null
            | RuntimeValue::Bool(_)
            | RuntimeValue::Number(_)
            | RuntimeValue::String(_)
            | RuntimeValue::Array(_),
        ) => return Err(StepRefusal::Shape),
    };
    let as_of = match members.get("asOf") {
        None => None,
        Some(RuntimeValue::String(text)) => {
            if text.trim().is_empty()
                || text.len() > MAX_AS_OF_BYTES
                || text.chars().any(char::is_control)
            {
                return Err(StepRefusal::Shape);
            }
            Some(text.clone())
        }
        Some(
            RuntimeValue::Null
            | RuntimeValue::Bool(_)
            | RuntimeValue::Number(_)
            | RuntimeValue::Array(_)
            | RuntimeValue::Object(_),
        ) => return Err(StepRefusal::Shape),
    };
    let freshness = members.get("freshness").map(parse_freshness).transpose()?;
    let trace = match members.get("trace") {
        None => None,
        Some(value) => {
            let decoded: TraceContext =
                norn_ir::runtime::decode(value).map_err(|_decode| StepRefusal::Shape)?;
            decoded.validate().map_err(|_invalid| StepRefusal::Shape)?;
            Some(decoded)
        }
    };
    let pin = members.get("pin").map(parse_pin).transpose()?;
    let size = input
        .canonical_json()
        .map_err(|_canonical| StepRefusal::Shape)?
        .len();
    if !u64::try_from(size).is_ok_and(|size| size <= MAX_INPUT_BYTES) {
        return Err(StepRefusal::TooLarge);
    }
    Ok(QuestionStep {
        question: question.clone(),
        params,
        as_of,
        freshness,
        trace,
        pin,
    })
}

fn parse_freshness(value: &RuntimeValue) -> Result<QuestionFreshnessGuard, StepRefusal> {
    let RuntimeValue::Object(fields) = value else {
        return Err(StepRefusal::Shape);
    };
    if fields.is_empty() {
        return Err(StepRefusal::Shape);
    }
    let mut guard = QuestionFreshnessGuard::default();
    for (key, field_val) in fields {
        match key.as_str() {
            "maxAgeMs" => {
                let max_age: u64 =
                    norn_ir::runtime::decode(field_val).map_err(|_bad| StepRefusal::Shape)?;
                guard.max_age_ms = Some(max_age);
            }
            "requireExact" => {
                let RuntimeValue::Bool(flag) = field_val else {
                    return Err(StepRefusal::Shape);
                };
                guard.require_exact = Some(*flag);
            }
            _ => return Err(StepRefusal::Shape),
        }
    }
    Ok(guard)
}

fn parse_pin(value: &RuntimeValue) -> Result<QuestionStepPin, StepRefusal> {
    let RuntimeValue::Object(fields) = value else {
        return Err(StepRefusal::Shape);
    };
    if fields.is_empty() {
        return Err(StepRefusal::Shape);
    }
    let mut pin = QuestionStepPin::default();
    for (key, field_val) in fields {
        match key.as_str() {
            "artifactId" => {
                let RuntimeValue::String(text) = field_val else {
                    return Err(StepRefusal::Shape);
                };
                if text.trim().is_empty() || text.len() > MAX_QUESTION_ID_BYTES {
                    return Err(StepRefusal::Shape);
                }
                pin.artifact_id = Some(text.clone());
            }
            "version" => {
                let RuntimeValue::String(text) = field_val else {
                    return Err(StepRefusal::Shape);
                };
                if text.trim().is_empty() || text.len() > MAX_VERSION_BYTES {
                    return Err(StepRefusal::Shape);
                }
                pin.version = Some(text.clone());
            }
            "digest" => {
                let RuntimeValue::String(text) = field_val else {
                    return Err(StepRefusal::Shape);
                };
                if text.len() != 64
                    || !text
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                {
                    return Err(StepRefusal::Shape);
                }
                pin.digest = Some(text.clone());
            }
            "definitionsVersion" => {
                let version: u64 =
                    norn_ir::runtime::decode(field_val).map_err(|_bad| StepRefusal::Shape)?;
                if version == 0 {
                    return Err(StepRefusal::Shape);
                }
                pin.definitions_version = Some(version);
            }
            _ => return Err(StepRefusal::Shape),
        }
    }
    Ok(pin)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_step_accepts_minimal_and_full_options() -> Result<(), String> {
        let parse_json = |text: &str| {
            RuntimeValue::from_json_text(text).map_err(|error| format!("{text}: {error}"))
        };
        let minimal = parse_json(r#"{"question":"inventory.on_hand"}"#)?;
        let step = parse_step(&minimal).map_err(|error| format!("minimal refused: {error:?}"))?;
        assert_eq!(step.question, "inventory.on_hand");
        assert_eq!(step.params, None);
        assert_eq!(step.as_of, None);
        assert_eq!(step.freshness, None);
        assert_eq!(step.trace, None);
        assert_eq!(step.pin, None);

        let hex = "b".repeat(64);
        let session = format!("sha256:{}", "c".repeat(64));
        let full = parse_json(&format!(
            r#"{{"question":"inventory.on_hand","params":{{"warehouse":"vault-1"}},"asOf":"2026-11-28T00:00:00Z","freshness":{{"maxAgeMs":60000,"requireExact":true}},"trace":{{"traceId":"tr-q1","intentRef":"run:r1","sessionDigest":"{session}"}},"pin":{{"artifactId":"inventory.on_hand","version":"1","digest":"{hex}","definitionsVersion":3}}}}"#
        ))?;
        let step = parse_step(&full).map_err(|error| format!("full refused: {error:?}"))?;
        assert_eq!(step.question, "inventory.on_hand");
        assert_eq!(
            step.freshness,
            Some(QuestionFreshnessGuard {
                max_age_ms: Some(60_000),
                require_exact: Some(true),
            })
        );
        assert_eq!(
            step.pin,
            Some(QuestionStepPin {
                artifact_id: Some("inventory.on_hand".to_owned()),
                version: Some("1".to_owned()),
                digest: Some(hex),
                definitions_version: Some(3),
            })
        );
        assert_eq!(step.as_of.as_deref(), Some("2026-11-28T00:00:00Z"));
        Ok(())
    }

    #[test]
    fn parse_step_refuses_malformed_inputs_and_empty_pin_or_freshness() -> Result<(), String> {
        let parse_json = |text: &str| {
            RuntimeValue::from_json_text(text).map_err(|error| format!("{text}: {error}"))
        };
        for bad in [
            r#"{}"#,
            r#"{"question":""}"#,
            r#"{"question":"   "}"#,
            r#"{"question":"q","extra":1}"#,
            r#"{"question":"q","params":[]}"#,
            r#"{"question":"q","asOf":""}"#,
            r#"{"question":"q","asOf":123}"#,
            r#"{"question":"q","freshness":null}"#,
            r#"{"question":"q","freshness":{}}"#,
            r#"{"question":"q","freshness":{"maxAgeMs":-1}}"#,
            r#"{"question":"q","freshness":{"requireExact":"yes"}}"#,
            r#"{"question":"q","freshness":{"allowStale":true}}"#,
            r#"{"question":"q","pin":null}"#,
            r#"{"question":"q","pin":{}}"#,
            r#"{"question":"q","pin":{"artifactId":""}}"#,
            r#"{"question":"q","pin":{"version":"  "}}"#,
            r#"{"question":"q","pin":{"digest":"not-hex"}}"#,
            r#"{"question":"q","pin":{"definitionsVersion":0}}"#,
            r#"{"question":"q","pin":{"extra":"bad"}}"#,
            r#"{"question":"q","trace":null}"#,
            r#"{"question":"q","trace":{"traceId":""}}"#,
        ] {
            let value = parse_json(bad)?;
            assert_eq!(parse_step(&value), Err(StepRefusal::Shape), "{bad}");
        }
        Ok(())
    }
}
