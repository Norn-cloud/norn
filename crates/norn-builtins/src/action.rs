//! `norn.action@1`: a flow step that calls a governed action (ACTION-STEP-1).
//!
//! A flow lowers "call this action" to `perform` of this Function. The cell
//! executes it itself, never an outside executor: it calls the named admitted
//! action with the step's input through the ordinary governed action path,
//! under the run starter's authority, and delivers the action's target
//! (`{objectType, objectId, version}`) as the step's output. A denial is the
//! typed failure `{_tag: "Denied"}`; any other refusal is
//! `{_tag: "Refused", code}`. The run holds no authority of its own, and a
//! person who decided an earlier step lends it none.
//!
//! The step's input is the perform record's input object; [`parse_step`]
//! checks its shape and bounds.

use std::collections::BTreeMap;

use norn_ir::{RuntimeValue, TraceContext};

/// The Function id.
pub const FUNCTION_ID: &str = "norn.action";
/// The Function version.
pub const VERSION: u32 = 1;
/// The input schema document.
pub const INPUT_SCHEMA: &str = include_str!("schemas/action.input.json");
/// The output schema document.
pub const OUTPUT_SCHEMA: &str = include_str!("schemas/action.output.json");
/// The typed error schema document (`{_tag: "Denied" | "Refused", code?}`).
pub const ERROR_SCHEMA: &str = include_str!("schemas/action.error.json");
/// The longest action id, in bytes.
pub const MAX_ACTION_ID_BYTES: usize = 256;
/// The longest pinned artifact version string, in bytes.
pub const MAX_VERSION_BYTES: usize = 128;
/// The largest step input, in canonical JSON bytes: the kernel's caller-input
/// bound.
pub const MAX_INPUT_BYTES: u64 = 256 * 1024;

/// Optional target-definition pin carried by a `norn.action@1` step (`STEP-PIN-1`).
///
/// When present, the cell verifies every specified field against the active
/// admitted action definition inside the same definitions hold as execution,
/// failing closed if the active catalog no longer matches.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ActionStepPin {
    /// Exact action artifact id (`artifact.artifact_id`).
    pub artifact_id: Option<String>,
    /// Exact action artifact version (`artifact.version`).
    pub version: Option<String>,
    /// Exact effective action definition digest (64 lowercase hex characters).
    pub digest: Option<String>,
    /// Exact cell definitions version (`>= 1`).
    pub definitions_version: Option<u64>,
}

/// One action step: the action to call, its input, optional trace context, and
/// optional target-definition pin.
#[derive(Debug, Clone, PartialEq)]
pub struct ActionStep {
    /// The admitted action id.
    pub action: String,
    /// The action's input object.
    pub input: BTreeMap<String, RuntimeValue>,
    /// Optional provenance trace context forwarded to the governed action call.
    pub trace: Option<TraceContext>,
    /// Optional target-definition pin enforced before dispatch (`STEP-PIN-1`).
    pub pin: Option<ActionStepPin>,
}

/// Why a perform input is not an action step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepRefusal {
    /// The input is not `{action, input, trace?, pin?}` with a bounded action
    /// id, an input object, an optional valid trace context, and an optional
    /// valid target-definition pin.
    Shape,
    /// The input is larger than [`MAX_INPUT_BYTES`].
    TooLarge,
}

impl StepRefusal {
    /// Stable refusal code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Shape => "action_step.shape",
            Self::TooLarge => "action_step.too_large",
        }
    }
}

/// Parse a perform record's step input as an action step.
///
/// # Errors
/// [`StepRefusal`] naming the broken bound.
pub fn parse_step(input: &RuntimeValue) -> Result<ActionStep, StepRefusal> {
    let RuntimeValue::Object(members) = input else {
        return Err(StepRefusal::Shape);
    };
    let expected_len =
        2 + usize::from(members.contains_key("trace")) + usize::from(members.contains_key("pin"));
    if members.len() != expected_len {
        return Err(StepRefusal::Shape);
    }
    let (Some(RuntimeValue::String(action)), Some(RuntimeValue::Object(step_input))) =
        (members.get("action"), members.get("input"))
    else {
        return Err(StepRefusal::Shape);
    };
    if action.trim().is_empty() || action.len() > MAX_ACTION_ID_BYTES {
        return Err(StepRefusal::Shape);
    }
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
    Ok(ActionStep {
        action: action.clone(),
        input: step_input.clone(),
        trace,
        pin,
    })
}

fn parse_pin(value: &RuntimeValue) -> Result<ActionStepPin, StepRefusal> {
    let RuntimeValue::Object(fields) = value else {
        return Err(StepRefusal::Shape);
    };
    if fields.is_empty() {
        return Err(StepRefusal::Shape);
    }
    let mut pin = ActionStepPin::default();
    for (key, field_val) in fields {
        match key.as_str() {
            "artifactId" => {
                let RuntimeValue::String(text) = field_val else {
                    return Err(StepRefusal::Shape);
                };
                if text.trim().is_empty() || text.len() > MAX_ACTION_ID_BYTES {
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
    fn parse_step_accepts_optional_trace_and_refuses_malformed_trace() -> Result<(), String> {
        let parse_json = |text: &str| {
            RuntimeValue::from_json_text(text).map_err(|error| format!("{text}: {error}"))
        };
        let digest = format!("sha256:{}", "c".repeat(64));
        let plain = parse_json(r#"{"action":"ops.melt.convert","input":{"id":"m1"}}"#)?;
        let step = parse_step(&plain).map_err(|error| format!("plain step refused: {error:?}"))?;
        assert_eq!(step.action, "ops.melt.convert");
        assert_eq!(step.trace, None);

        let traced = parse_json(&format!(
            r#"{{"action":"ops.melt.convert","input":{{"id":"m1"}},"trace":{{"traceId":"tr-1","intentRef":"run:r1","sessionDigest":"{digest}"}}}}"#
        ))?;
        let step =
            parse_step(&traced).map_err(|error| format!("traced step refused: {error:?}"))?;
        assert_eq!(
            step.trace,
            Some(TraceContext {
                trace_id: "tr-1".to_owned(),
                intent_ref: Some("run:r1".to_owned()),
                session_digest: Some(norn_ir::function::Digest(digest)),
            })
        );

        for bad in [
            r#"{"action":"ops.melt.convert","input":{"id":"m1"},"trace":null}"#,
            r#"{"action":"ops.melt.convert","input":{"id":"m1"},"trace":["tr-1"]}"#,
            r#"{"action":"ops.melt.convert","input":{"id":"m1"},"trace":{"traceId":""}}"#,
            r#"{"action":"ops.melt.convert","input":{"id":"m1"},"trace":{"traceId":"t1","sessionDigest":"raw transcript"}}"#,
            r#"{"action":"ops.melt.convert","input":{"id":"m1"},"trace":{"traceId":"t1","extra":"bad"}}"#,
        ] {
            let value = parse_json(bad)?;
            assert_eq!(parse_step(&value), Err(StepRefusal::Shape), "{bad}");
        }
        Ok(())
    }

    #[test]
    fn parse_step_accepts_optional_pin_and_refuses_malformed_pin() -> Result<(), String> {
        let parse_json = |text: &str| {
            RuntimeValue::from_json_text(text).map_err(|error| format!("{text}: {error}"))
        };
        let hex = "a".repeat(64);
        let pinned = parse_json(&format!(
            r#"{{"action":"ops.melt.convert","input":{{"id":"m1"}},"pin":{{"artifactId":"action.melt","version":"1.0.0","digest":"{hex}","definitionsVersion":2}}}}"#
        ))?;
        let step =
            parse_step(&pinned).map_err(|error| format!("pinned step refused: {error:?}"))?;
        assert_eq!(
            step.pin,
            Some(ActionStepPin {
                artifact_id: Some("action.melt".to_owned()),
                version: Some("1.0.0".to_owned()),
                digest: Some(hex),
                definitions_version: Some(2),
            })
        );
        for bad in [
            r#"{"action":"ops.melt.convert","input":{"id":"m1"},"pin":null}"#,
            r#"{"action":"ops.melt.convert","input":{"id":"m1"},"pin":{}}"#,
            r#"{"action":"ops.melt.convert","input":{"id":"m1"},"pin":{"artifactId":""}}"#,
            r#"{"action":"ops.melt.convert","input":{"id":"m1"},"pin":{"version":"   "}}"#,
            r#"{"action":"ops.melt.convert","input":{"id":"m1"},"pin":{"digest":"not-hex"}}"#,
            r#"{"action":"ops.melt.convert","input":{"id":"m1"},"pin":{"definitionsVersion":0}}"#,
            r#"{"action":"ops.melt.convert","input":{"id":"m1"},"pin":{"definitionsVersion":"1"}}"#,
            r#"{"action":"ops.melt.convert","input":{"id":"m1"},"pin":{"extra":"bad"}}"#,
        ] {
            let value = parse_json(bad)?;
            assert_eq!(parse_step(&value), Err(StepRefusal::Shape), "{bad}");
        }
        Ok(())
    }
}
