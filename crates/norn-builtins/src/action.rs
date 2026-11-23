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
/// The largest step input, in canonical JSON bytes: the kernel's caller-input
/// bound.
pub const MAX_INPUT_BYTES: u64 = 256 * 1024;

/// One action step: the action to call, its input, and optional trace context.
#[derive(Debug, Clone, PartialEq)]
pub struct ActionStep {
    /// The admitted action id.
    pub action: String,
    /// The action's input object.
    pub input: BTreeMap<String, RuntimeValue>,
    /// Optional provenance trace context forwarded to the governed action call.
    pub trace: Option<TraceContext>,
}

/// Why a perform input is not an action step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepRefusal {
    /// The input is not `{action, input, trace?}` with a bounded action id,
    /// an input object, and an optional valid trace context.
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
    let expected_len = if members.contains_key("trace") { 3 } else { 2 };
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
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "fixture assertions")]
mod tests {
    use super::*;

    #[test]
    fn parse_step_accepts_optional_trace_and_refuses_malformed_trace() {
        let digest = format!("sha256:{}", "c".repeat(64));
        let plain =
            RuntimeValue::from_json_text(r#"{"action":"ops.melt.convert","input":{"id":"m1"}}"#)
                .unwrap();
        let step = parse_step(&plain).unwrap();
        assert_eq!(step.action, "ops.melt.convert");
        assert_eq!(step.trace, None);

        let traced = RuntimeValue::from_json_text(format!(
            r#"{{"action":"ops.melt.convert","input":{{"id":"m1"}},"trace":{{"traceId":"tr-1","intentRef":"run:r1","sessionDigest":"{digest}"}}}}"#
        ))
        .unwrap();
        let step = parse_step(&traced).unwrap();
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
            r#"{"action":"ops.melt.convert","input":{"id":"m1"},"trace":{"traceId":""}}"#,
            r#"{"action":"ops.melt.convert","input":{"id":"m1"},"trace":{"traceId":"t1","sessionDigest":"raw transcript"}}"#,
            r#"{"action":"ops.melt.convert","input":{"id":"m1"},"trace":{"traceId":"t1","extra":"bad"}}"#,
        ] {
            let value = RuntimeValue::from_json_text(bad).unwrap();
            assert_eq!(parse_step(&value), Err(StepRefusal::Shape), "{bad}");
        }
    }
}
