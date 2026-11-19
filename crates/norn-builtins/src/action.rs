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

use norn_ir::RuntimeValue;

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

/// One action step: the action to call and its input.
#[derive(Debug, Clone, PartialEq)]
pub struct ActionStep {
    /// The admitted action id.
    pub action: String,
    /// The action's input object.
    pub input: BTreeMap<String, RuntimeValue>,
}

/// Why a perform input is not an action step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepRefusal {
    /// The input is not `{action, input}` with a bounded action id and an
    /// input object.
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
    if members.len() != 2 {
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
    })
}
